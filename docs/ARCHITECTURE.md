# Architecture

This document records the design of blink: the pipeline, the crate layout, the
rules that govern each crate's `unsafe` usage, and the decisions behind them. It
exists so that implementers, human or agent, follow one design instead of
re-litigating it per pull request.

blink is pre-alpha. The pipeline described here is implemented; what is still
open is marked below.

Items that cannot be settled without a GPU and a Windows host are marked **needs
hardware verification**; a few others are marked **to confirm before
implementation** where the open question is a license or tooling check rather
than a hardware one. Do not treat those as decided: test them, record the result,
and update this document in the same change.

## Pipeline overview

```mermaid
flowchart LR
  Hotkey[blink-app hotkey] --> Ring
  DXGI["blink-capture DXGI Desktop Duplication (D3D11 texture)"] --> NVENC["blink-encode NVENC H.264"]
  NVENC --> Ring["blink-ring encoded packet ring buffer (last N seconds, GOP-aligned)"]
  WASAPI["blink-audio WASAPI loopback"] --> Mix["mix on the QPC grid"]
  Mic["blink-audio WASAPI microphone"] --> Mix
  Mix --> AAC["AAC encode (Media Foundation MFT)"] --> Ring
  Ring -->|on hotkey| Mux["blink-mux MP4 writer"] --> File[clip.mp4]
  Tray["Tray icon (main process)"] -->|Settings item| Settings["blink.exe --settings (egui window)"]
  Settings -->|"writes config.toml, exits"| Config[("config.toml")]
  Config -->|reload off hot path| Hotkey
```

Two properties define the design:

1. Frames never leave the GPU. The duplicated D3D11 texture is handed to NVENC
   directly, and NVENC returns encoded packets.
2. Only encoded packets are buffered. The ring holds compressed H.264 and AAC,
   not raw frames, so a multi-minute buffer costs megabytes rather than
   gigabytes.

Saving a clip does not touch capture or the encoder. It cuts the ring at an IDR
keyframe, drops the packets before it, and remuxes what remains into MP4.

## Crate layout and unsafe policy

| Crate | Role | `unsafe` policy |
| --- | --- | --- |
| `crates/blink-app` | Binary. Hotkey registration, configuration, wiring, `tracing` setup. | `#![forbid(unsafe_code)]` |
| `crates/blink-ring` | Platform-agnostic encoded-packet ring buffer, GOP-aligned eviction. | `#![forbid(unsafe_code)]`, tests run under Miri |
| `crates/blink-mux` | Platform-agnostic MP4 muxing, including the `pasp` box for stretch. | `#![forbid(unsafe_code)]` |
| `crates/blink-config` | Shared config types and load/save, used by both processes. | `#![forbid(unsafe_code)]` |
| `crates/blink-ui` | egui/eframe settings window for the `--settings` process. | `#![forbid(unsafe_code)]` |
| `crates/blink-capture` | DXGI Desktop Duplication and D3D11 device management. | `unsafe` allowed; every block needs `// SAFETY:` |
| `crates/blink-audio` | WASAPI loopback and microphone capture, their mix, and AAC encode through a Media Foundation MFT. | `unsafe` allowed; every block needs `// SAFETY:` |
| `crates/blink-encode` | NVENC session management, resource registration, encode calls. | `unsafe` allowed; every block needs `// SAFETY:` |
| `crates/blink-sys` | Win32 plumbing the other crates share: the QPC clock, thread priority and background mode, the single-instance mutex, the tray thread's message pump, and asynchronous playback of in-memory WAV sounds. Also the portable helpers they share: `Backoff`, `ErrorChain`, and the replacing and no-replace file renames. | `unsafe` allowed; every block needs `// SAFETY:` |

The four FFI crates are the only place `unsafe` is permitted. They are also
where bugs are most expensive, so they carry the strictest review bar.

Rules that apply everywhere:

- `unsafe` operations are documented with a `// SAFETY:` comment stating why the
  operation is sound. Clippy's `undocumented_unsafe_blocks` and
  `missing_safety_doc` are denied.
- One unsafe operation per block (`multiple_unsafe_ops_per_block` is denied).
  Split the block and explain each operation rather than batching them.
- `unsafe_op_in_unsafe_fn` is denied, so an `unsafe fn` body still needs explicit
  `unsafe` blocks.
- `blink-ring` and `blink-mux` deny indexing and slicing crate-wide. Both process
  untrusted or partially trusted buffers, and an out-of-bounds index would panic
  the recorder mid-session.

## Cross-platform buildability

Windows is the only supported runtime, but it should not be the only place the
workspace compiles.

- The FFI crates declare Windows dependencies under
  `[target.'cfg(windows)'.dependencies]` and gate their modules with
  `#[cfg(windows)]`. On other hosts they export only their portable items
  (config, errors, pure logic). The entry points of `blink-capture` and
  `blink-audio` also have stubs that return an `Unsupported` error;
  `blink-encode` and `blink-sys` have none.
- `cargo check`, `cargo test`, and `cargo clippy` therefore work for the whole
  workspace on macOS and Linux.
- Windows code is typechecked from other hosts with
  `cargo xwin clippy --target x86_64-pc-windows-msvc`. That never links or runs,
  and can fail on C build scripts that need libclang, so it is best effort
  locally. `windows-latest` CI is authoritative.

The stubs are a compile-time convenience, not a fallback. A stubbed build must
report that capture is unavailable rather than silently doing nothing.

## Architecture decisions

### Clock

One timeline, based on the Windows Query Performance Counter. Video frames and
WASAPI packets both carry presentation timestamps in that clock, and the ring
stores the PTS with every packet. `blink-sys` owns the conversion from QPC
counts to 100 ns `Timestamp` ticks (`QpcClock`) and the `qpc_now` reading that
capture, audio, and save requests share. Nothing downstream computes time from a
different source, and no component re-derives timestamps from arrival order.

### Variable frame rate

DXGI Desktop Duplication only returns a frame when the screen content changes,
so the duplicated frame rate is inherently variable. blink writes a
variable-frame-rate MP4 rather than repeating frames to hold a constant rate:
each encoded frame keeps its capture timestamp, the ring stores it, and the
muxer writes per-sample durations from consecutive timestamps. Nothing is
encoded while the screen is static except the periodic IDR refresh that keeps
a clip start available. Repeating frames would cost encoder work per held
frame, which the no-extra-work goal rules out.

Capture timestamps sit on a fixed grid: the QPC timeline is divided into slots
of one target frame interval, and a frame is stamped with the slot start
nearest the moment it was acquired (see [Paced capture](#paced-capture)).
During motion frames are therefore exactly one interval apart, and the only
variable part is the gaps, in whole intervals, while the screen is static or
the desktop updates slower than the target rate. A frame accumulated during
motion is taken at its slot start, so its stamp trails the desktop update by
up to one interval and video lags audio by that much. A frame taken mid-slot
is stamped within half an interval either way, so video leads audio by at
most half an interval (8.3 ms at 60 fps). Both stay well inside the
perceptibility thresholds of ITU-R BT.1359 (about 45 ms of audio lead and
125 ms of lag).

**Needs verification**: confirm, with hardware and the players people actually
use (editors and share targets included), that the timestamps are handled
correctly.

### Audio gaps

WASAPI loopback delivers nothing while the system is silent. blink fills those
gaps with digital silence derived from the QPC timeline rather than waiting for
real samples, so audio and video stay in sync across silence and across a
stream that stalls.

Fill covers gaps up to 60 seconds (silent AAC frames cost a few bytes each). A
longer gap, usually a system sleep, ends the audio segment instead: the encoder
is drained and the next packet starts a new segment at its real timestamp. A
clip spanning that point has an audio timestamp jump, which the muxer must
handle (with an edit list or by starting the clip's audio after the jump), but
a clip cut from the last few minutes never spans it in practice.

### Microphone

With **Record microphone** on (the default), the audio thread also captures the
default recording device and mixes it into the same AAC track. The setting
only applies while system audio is on; the settings window disables it
otherwise, so a config that had audio off never starts recording the
microphone on upgrade.

- **Device role.** The `eConsole` default, not `eCommunications`. Windows
  treats a stream on the communications device as a call and can duck other
  audio (the game) while it runs; the console default is also what the user
  picked as "the microphone" in Sound settings.
- **Format.** Shared mode with `AUTOCONVERTPCM | SRC_DEFAULT_QUALITY`, the same
  as loopback, so WASAPI converts a mono or 44.1 kHz microphone to the 48 kHz
  stereo the encoder takes.
- **Sync.** The microphone runs its own `Timeline` (the gap fill, slew, and
  segment logic above), created on the desktop timeline's grid, so both emit
  1024-frame chunks with identical timestamps. A mixer pairs chunks by
  timestamp and sums them with unity gain through a soft clipper (transparent
  up to -1 dBFS, a `tanh` knee above). Each source's clock drift is slewed
  against QPC independently, so the two never drift apart. When the desktop
  timeline starts a new segment, the microphone timeline is rebuilt on its
  grid.
- **Latency.** Nothing waits on the microphone. Desktop chunks sit in a fixed
  queue (no allocation after start) until their microphone chunk arrives, the
  microphone timeline has passed them, or 200 ms after they end; then they are
  encoded alone. The microphone fills its own gaps 150 ms behind real time,
  later than loopback's fill, so a late microphone packet is not mistaken for
  silence. A delivering microphone adds about one device period; a stalled
  one holds audio back by up to these waits, about the 200 ms a save settles
  for, so such a clip's audio can end a fraction of a second before its
  video.
- **Failure.** No device, access denied by the privacy settings, a removed or
  switched device: the thread logs a warning once per cause, keeps recording
  desktop audio, retries with backoff (up to 10 s), and follows a default
  device change. The tray tooltip shows when the microphone is missing,
  blocked, or unavailable.

**Needs hardware verification**: that a privacy denial surfaces as
`E_ACCESSDENIED`; the conversion of a mono 44.1 kHz microphone; Bluetooth
headsets, which switch to their lower-quality hands-free profile while the
microphone is open; and that the microphone's delivery latency stays inside
the 200 ms wait.

### GOP structure

An IDR at least once per second of PTS, with SPS/PPS repeated on every IDR.
Capture is variable-rate (unchanged frames are skipped), so the interval is
measured in timestamps, not frames: NVENC runs with an infinite GOP and the
encoder forces an IDR once a second of PTS has passed since the last one, on
the first frame after a resize, and whenever the ring asks for one
(`PushOutcome::DroppedAwaitingKeyframe`). On a static screen nothing is
encoded, so the capture loop asks the encoder whether an IDR is due and
re-encodes its last captured texture when it is. That buys two things the rest
of the system depends on:

- The ring evicts whole GOPs, so no packet in the buffer references a frame that
  has already been dropped.
- A clip starts at the nearest IDR at or before `now - N`. The consequence is
  that clips can run up to one GOP longer than the requested duration; this is
  expected, not a bug.

### Codecs

H.264 first, for compatibility with players, editors, and share targets. HEVC is
a later addition, not a v1 requirement. Audio is AAC encoded through the Media
Foundation MFT that ships with Windows, so there is no additional licensing
burden and no bundled encoder binary.

### NVENC loading

`nvEncodeAPI64.dll` is loaded from the installed NVIDIA driver at runtime with
`LoadLibrary`. Nothing NVIDIA-authored is redistributed, which keeps the
licensing story clean and means blink runs against whichever driver version the
user has.

Only permissively licensed headers may be used, and SDK sample code must never
be vendored. **To confirm before implementation** (a license check, not a
hardware one): whether the `nv-codec-headers`-style headers are usable under
their own license, as opposed to the full SDK EULA.

### Why this capture path

Desktop Duplication plus NVENC is NVIDIA's recommended capture path on Windows.
The alternatives were considered and rejected:

- **NvFBC** is deprecated and restricted on GeForce cards.
- **Game hooks** carry anti-cheat risk for little measurable gain, and would put
  blink inside the process it is trying to leave undisturbed.
- **Windows.Graphics.Capture** has no performance advantage over Desktop
  Duplication for this workload.

### HDR

Out of scope for v1. Supporting it later means capturing with
`IDXGIOutput5::DuplicateOutput1` using an FP16 format and then tone-mapping to
SDR or encoding HDR end to end. The pipeline's single-clock and packet-ring
designs do not prevent this, but the muxer and player compatibility story does
need work.

## Performance rules

The primary goal is minimal in-game FPS loss. These rules outrank feature work,
code elegance, and convenience. Every one of them is a review criterion.

1. **GPU-only, one copy.** Each admitted frame costs exactly one GPU
   `CopyResource` from the duplicated surface into a small pool of textures
   that blink owns (`blink-capture`, `TEXTURE_POOL_SIZE` = 3). NVENC registers
   those pool textures (`NvEncRegisterResource`) and is fed BGRA, so NVENC
   performs the color conversion. No CPU readback, no scaling, and no
   color-conversion shader of our own.

   The copy is deliberate. The duplicated surface is a single OS-owned texture
   that DXGI reuses for every frame, so it cannot stay mapped as encoder input
   across an asynchronous encode or the next `AcquireNextFrame`. Its bind flags
   are not guaranteed, and it changes identity with every duplication session,
   which would churn NVENC registrations on each mode change. Holding the
   frame is not the problem: Microsoft recommends releasing it just before the
   next acquire.

   Cost: a copy reads and writes the frame once, so at 2560x1440 BGRA and
   60 fps that is about 14.7 MB x 2 x 60 = 1.8 GB/s of video memory
   bandwidth, a fraction of a percent of a current discrete GPU's. **Needs
   hardware verification**: measure the copy's GPU time (timestamp queries or
   the GPU 3D-engine delta in a capture-on versus capture-off benchmark run)
   at 1440p and 4K, and record it with the other benchmark results.
2. **No CUDA-core NVENC features.** Use a fast preset (P1 to P3), no lookahead,
   single pass, and no adaptive-quality modes that run on the CUDA cores. Use CBR
   or VBR at a sensible bitrate.
3. **Capture at the clip FPS, not the refresh rate.** Acquisition is paced to
   the clip frame rate: the capture thread sleeps until the next slot and
   acquires once, and DXGI folds the updates in between into that one frame.
   On a 240 Hz monitor recording at 60 fps, that is one acquire, copy, and
   encode per four refreshes, and the other three are never acquired.
4. **Skip unchanged frames.** When duplication reports no new content (a
   timeout, or an update that only moved the pointer), nothing is copied or
   encoded; the previous frame stands until the next change. On a static
   screen the only encoder work is the once-a-second IDR refresh.
5. **CPU discipline.** A fixed set of threads, no per-frame allocation
   (preallocated buffers and a preallocated ring), below-normal priority on
   blink's threads, and saving and muxing on a low-priority thread that the
   capture and encode threads never wait on.
6. **Native resolution by default.** No scaling pass. Downscaling is opt-in.
7. **Measure.** Every change that touches the hot path includes before-and-after
   numbers produced with [BENCHMARKING.md](BENCHMARKING.md).

A change that violates any rule needs the rule changed here first, with numbers,
in its own pull request.

## Resolution changes

This is the least obvious part of the design, and CS2 in fullscreen at a
stretched 4:3 resolution exercises all of it: the game launch, the alt-tab, and
the stretch.

### Capture recovery

A game launch or alt-tab changes the display mode, which ends the duplication
session with `DXGI_ERROR_ACCESS_LOST`. Recreate the session with a short retry
backoff, because several of these can arrive in a row as the mode settles. The
recording gap lasts a fraction of a second.

Recovery has two tiers. The duplicator recreates a lost session itself, on the
same D3D11 device, so the NVENC session and its texture registrations survive
(a size change goes through encoder reconfiguration below). Only errors that
recreating the session cannot fix, such as device removal, end the capture
session: the capture thread then rebuilds the device, duplicator, and encoder.
Its backoff resets only after a session has encoded for 10 seconds, so a fault
that recurs every few seconds escalates the delay instead of rebuilding at the
shortest one.

A process can hold one duplication per output, and `DuplicateOutput` returns
`E_INVALIDARG` while any reference to the previous session is alive. The
duplicator drops its session before recreating it, on every path.

Only frames with new desktop content (`LastPresentTime != 0`) are copied. A
pointer-only update's surface is not guaranteed to hold the desktop, and on
a new session it is black, so copying one encodes a black frame. A new
session therefore waits for its first content update. A new session with
nothing to show (startup, or after a resize) that gets no content update
within a second is re-duplicated, backing off to every 8 s, rather than
copying a pointer-only frame.

### Paced capture

The capture thread acquires at most one frame per slot of the clip frame rate
(`FramePacer` in `blink-capture`). Slots tile the QPC timeline from zero, one
per `1 / fps` seconds. After emitting a frame the thread sleeps until the
next slot starts, without calling `AcquireNextFrame`. DXGI accumulates
desktop updates while the application holds no frame
(`DXGI_OUTDUPL_FRAME_INFO::AccumulatedFrames`), so the acquire at the slot
returns at once with the latest desktop image, including a change made just
before the desktop went quiet. No update is dropped, so nothing ever needs
re-delivering, and a 144 Hz desktop costs one acquire per slot instead of 144
a second.

- **Nothing new at the slot.** The acquire blocks until the desktop changes
  (or the caller's 100 ms poll ends, for the static-screen IDR refresh) and
  takes that update straight away, stamped with the nearest slot. This adds
  no latency and stays on the grid. Holding the update for the next slot
  would add up to an interval of latency for the same regularity.
- **Timestamps.** A frame is stamped with the slot start nearest the moment
  it was taken, on the same QPC timeline as audio, and each frame takes a
  later slot than the one before, so timestamps strictly increase and frames
  during motion are exactly `1 / fps` apart. A take in the second half of a
  slot claims the next one (never further), so the stamp error stays within
  half an interval and a frame at motion onset is not crowded against the
  next. The encoder's one-IDR-per-second rule counts PTS, so it lands on
  every `fps`-th frame during motion.
- **Keyframe refreshes.** On a timeout the engine may refresh an IDR stamped
  with the current time, so after a timeout, and when a session is created
  (the engine may have refreshed during the outage), the slot containing the
  current time is reserved: the next frame takes a later slot and its PTS
  stays after the refresh.
- **Pointer-only updates.** These are never copied. They also hold the next
  acquire until the following slot, so a moving mouse costs at most one
  acquire per slot. A content change made meanwhile accumulates into the same
  frame, and `LastPresentTime` is set whenever any content accumulated, so the
  mouse cannot hide or delay content beyond the slot.
- **Overruns.** If an encode or a preempted thread makes the acquire start
  nearer a later slot than its own, the frame takes the nearest slot. The
  slots that passed are skipped (counted as `late_slots`), never caught up
  with a burst.
- **Resync fallback.** A content update that comes without a desktop image
  (no resource) is looked for again at the next slot without waiting. If that
  slot brings no content, the desktop went quiet with the change undelivered,
  and the session is recreated, because the first content update of a new
  session carries the whole desktop. This is the only resync left, and it is
  not expected in normal operation.

The sleep is `std::thread::sleep`. On the pinned toolchain (Rust 1.100) it
uses a `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` waitable timer on Windows 10
1803 and later, and falls back to `Sleep` (timer-tick resolution) only on
older systems, where pacing would be coarse. The duplicator logs its counters
every 60 s: `copied`, `coalesced` (updates folded into copied frames),
`unchanged_slots`, `pointer_only`, `late_slots`, `no_image`, `resyncs`, and
the new-session diagnostics.

The decisions (pacing, staleness, the empty-session check, counters) live in
`Pacing` in `decision.rs`, which the duplicator calls around each
`AcquireNextFrame`. The tests drive the same calls through a simulated
desktop and engine (content, pointer-only and imageless updates, keyframe
refreshes, pointer-only first frames, overruns), so the simulation cannot
drift from the Windows control flow. Session loss and recreation retries are
Windows-only and not simulated.

A desktop that updates at a whole fraction of the frame rate (30 Hz into
60 fps), with its updates phase-locked within jitter of a slot boundary or
midpoint, alternates between the slots either side of it (gaps of one and three
intervals instead of two). Any fixed-grid sampler has this. It lasts only
while the phases stay aligned.

**Needs hardware verification**: that an acquire at the slot returns the
accumulated update immediately with the latest image, and that `late_slots`
stays near zero under a CPU-bound game, given that the capture thread runs at
below-normal priority.

### Encoder reconfiguration

Create the NVENC session once, with its maximum width and height set to the
native resolution, then call `NvEncReconfigureEncoder` with a forced IDR on each
size change. The encoder is never torn down and recreated, so a mode change does
not cost a session setup.

**Needs hardware verification**: that a session created at the native size
accepts smaller reconfigured sizes without renegotiating the `NV_ENC_CAPS` and
buffer-format settings.

### Ring segments

Every resolution change starts a new segment, and a clip only spans the current
segment, because an MP4 whose track changes resolution mid-stream plays badly in
most players. The consequence is that right after an alt-tab a clip can be
shorter than the requested N seconds. Report this to the user rather than
trimming or merging segments.

### Stretched look at zero cost

An optional `stretch_to` config value (for example `1920x1080`) is the display
size every frame is shown at. The encoder derives the sample aspect ratio for
the current encode size (`stretch_w * height : stretch_h * width`, so 1280x960
stretched to 1920x1080 is 4:3) and recomputes it on every resize, so the native
desktop after an alt-tab goes back to square pixels. The ratio lands in the
H.264 VUI (NVENC derives it from the display aspect ratio it is given) and the
MP4 `pasp` box, so players display 4:3 frames stretched, exactly as they appear
in game. There is no GPU scaling pass and no extra frame work; the pixels are
untouched and only the display aspect metadata changes. It is off by default.

**Needs hardware verification**: whether Desktop Duplication reports the game's
mode resolution (1280x960, say) rather than a pre-scaled native image under the
different GPU scaling settings (full panel, aspect ratio, no scaling). If the
compositor pre-scales, `stretch_to` has nothing to stretch and the design needs
revisiting.

## Saving a clip

The ring keeps the clip length plus 3 seconds. A save waits 200 ms after the
hotkey press so the encoder and audio packets of that moment land, then copies
while capture keeps pushing; without the slack, those newer packets would evict
the keyframe the clip starts at. Saving runs on two threads, so copying never
waits on the disk. The cutter sizes and page-touches a reusable clip buffer
before taking the ring lock, copies in 1 MB chunks at below-normal priority,
and hands the lock back with a fair unlock between chunks, so a push waits at
most one chunk. The writer writes the MP4 in Windows background mode (lowest
CPU and I/O priority), never holding the lock, to a hidden `.partial` file
that is renamed when complete; leftovers from a crash are deleted at startup.
A second press while a clip is being written is cut right away into a second
buffer, which is freed once both are back; more presses are ignored.

### Save sound

When the writer reports a result, the tray thread plays a short chime: a
rising two-note one (250 ms) when the clip was written, a lower falling pair
(300 ms) when the save failed. Both start after 40 ms of silence, because a
Bluetooth (A2DP) output that has gone idle can drop the first moments of a
stream while it wakes. The `save_sound` setting, on by default, turns it
off; the tray thread reads it from the current config each time, so a change
applies to the next save.

The sounds are synthesized in code (`blink-app`'s `chime` module, integer
sine and envelope arithmetic) rather than shipped as audio files, so there is
no asset to license and nothing in the binary but the generator. Each is
rendered once at startup into a 16-bit mono 24 kHz WAV image (about 14 KB and
16 KB) that lives until the process exits. `blink_sys::play_wav` hands that
image to winmm's `PlaySoundW` with `SND_MEMORY | SND_ASYNC`, so winmm plays
it on its own thread and keeps reading the image during playback. The
wrapper therefore takes a `'static` slice, and before the call it checks
that the RIFF length and every chunk size stay inside that slice. A save
costs no allocation and no work on the capture, encode, or save threads; a
second result while a sound plays cuts it off. An `IAudioClient` render
stream would add a thread and device handling for no benefit at this length.

The first `PlaySoundW` call in a process sets winmm up (its playback thread
and the output device), which can take tens to hundreds of milliseconds. So
that this lands at startup rather than on the first save, the tray thread
plays 10 ms of silence when the engine starts with the sound on, and when
the setting is turned on.

blink does not pass `SND_SYSTEM`, so the sound plays under blink's own entry
in the Windows volume mixer rather than following the system sounds slider,
and users can set its level independently of other notifications.

The chime plays through the default output device, so WASAPI loopback
records it into the ring, and the next clip covering that moment has it in
its audio. That is accepted, as it is in other clip recorders; turning the
sound off is the remedy.

**Needs hardware verification**: that `PlaySoundW` with `SND_ASYNC` returns
without waiting for playback, that the warm-up removes any stall on the
first save's sound, that the 40 ms lead-in is enough for Bluetooth outputs,
and that the chime's volume sits comfortably under typical game audio.

## Settings UI

The main `blink` process contains only the capture engine and a system tray icon
(the `tray-icon` crate, driven by an idle Win32 message loop). The binary links
eframe/egui, but the engine process never initializes it. The tray's "Settings"
item spawns `blink.exe --settings`, a separate short-lived process that shows a
small fixed-size egui window on eframe's OpenGL (glow) renderer. It repaints
only on input, never continuously. Closing the window exits the process, so the
settings UI and its GL context cost nothing while gaming.

Rationale: an in-process GPU-backed UI keeps a GPU context resident for the life
of the process; a separate process that exits removes that cost entirely, so the
toolkit choice does not affect in-game FPS. egui is MIT/Apache-2.0, and runs
unchanged on macOS and Linux, which blink may support later. glow rather than wgpu keeps the binary and dependency tree small;
blink already requires an NVIDIA GPU, whose drivers always provide OpenGL. Raw
Win32 would be marginally lighter, but needs extensive `unsafe` code and is
Windows-only.

Settings exposed: output folder, clip length (seconds), hotkey, FPS,
quality/bitrate, audio on/off, microphone on/off, the optional `stretch_to`,
and the save sound on/off.

IPC: there is none beyond the file and the process exit. The settings process
writes `%APPDATA%\blink\config.toml` atomically (write a temp file, then rename)
and exits. The engine spawned it, so it waits for that child to exit and then
reloads the config off the hot path. Changes to FPS, video or audio bitrate,
audio on/off, microphone on/off (while audio is on), or `stretch_to` restart
the pipeline cleanly (capture, encoder,
audio, and a fresh ring). The save hotkey is suspended while the window is open and registered again
when it closes, so a new hotkey applies live; a new clip length
swaps in a resized ring live (the buffered clip is lost), and a new output
folder or save sound setting applies to the next save. An invalid file is
logged and ignored; the engine keeps its previous config. Hand edits to the
file are picked up the next time the settings window closes.

`blink-config` holds the shared config types and load/save. `blink-ui` is a
library with one entry point, `blink_ui::run(config_path)`, which `blink-app`
calls when started with `--settings` and which returns whether the user saved
(a Save with no changes does not rewrite the file) or cancelled. The window
opens the file without validating it, so out-of-range values show inline
errors; it validates the draft every frame and disables Save while any error
remains. A config file that does not parse is never overwritten: the settings
window does not open and a native error dialog says why. eframe is built with
only the glow renderer, egui's bundled fonts, and the system clipboard (no
wgpu, accessibility, or Wayland). The crate forbids `unsafe`. The engine never calls into
`blink-ui`, so no UI toolkit initializes in the engine process.

The hotkey recorder picks the key the way the engine's registration will
match it, so the key that records is the key that fires. `global-hotkey`
registers a virtual-key code on Windows and a keysym on X11, both of which
follow the keyboard layout, so the recorder prefers the key's layout meaning
(AZERTY's A key records A) and falls back to its position for keys with no
Latin meaning (a Cyrillic layout records the US position). macOS registers by
position, so there the recorder prefers the position. Numpad digits record as
digits; they fire only if the engine also registers the numpad codes.
Ctrl+C, Ctrl+X, and Ctrl+V (Cmd on macOS) stay copy and paste for the text
fields and cannot be recorded. egui's modifier state has no Windows-key flag
outside macOS, so the recorder tracks the Super key presses egui reports
instead; combinations the shell reserves (Win+L, Win+D) never reach the window.

Every dependency's license and copyright notice, including the fonts egui
embeds, is in `THIRD-PARTY-NOTICES.txt`, generated by cargo-about (`just
notices`) and checked for staleness in CI. Release artifacts ship it next to
`blink.exe`. The size-heavy UI crates are built at `opt-level = "s"` in the
release profile; the engine's crates keep the default speed level.

## Decisions settled during implementation

- NVENC bindings are hand-written declarations in `crates/blink-encode/src/ffi`,
  covering only the API blink calls. Nothing is generated at build time, so no
  libclang is needed; `ffi/README.md` records the header version they follow.
- The MP4 muxer is a minimal hand-written writer in `blink-mux`, which keeps
  the `pasp` box and variable-frame-rate sample durations under our control.
