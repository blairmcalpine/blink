# blink

A bare-bones Windows screen clipper. Press a hotkey and blink saves the last N
seconds of your screen, system audio, and microphone as an MP4 in a folder you
choose.

blink exists for one reason: clipping should not cost frames. Capture runs
through DXGI Desktop Duplication, H.264 encoding runs on the GPU through NVIDIA
NVENC without a copy back to system memory, and only encoded packets are kept in
memory. Saving a clip cuts the ring buffer at a keyframe and remuxes it, so the
clip is never re-encoded. blink lives in the system tray, and a minimal settings
window opens only on demand in a separate process.

A short chime confirms each saved clip, and a lower tone means the save
failed. Turn it off with **Play a sound when a clip is saved** in Settings.

## Status

Pre-alpha. The whole pipeline is implemented: desktop capture, NVENC encode,
system-audio capture mixed with the default microphone (a settings toggle, on
by default) into one AAC track, the packet ring, MP4 muxing, the tray and
hotkey, and the settings window. It has not had broad hardware testing yet, and
the items marked **needs hardware verification** in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) are still open. Expect rough
edges.

## Requirements

- Windows 10 or Windows 11 (x86_64)
- An NVIDIA GPU with NVENC support
- A recent NVIDIA driver, new enough to ship `nvEncodeAPI64.dll` for your GPU

blink does not ship or redistribute NVIDIA binaries; the encoder library is
loaded from the installed driver at runtime.

## Install

Download `blink-<version>-x86_64.msi` from the
[latest release](https://github.com/blairmcalpine/blink/releases/latest) and
run it. It installs blink for your user only, into
`%LOCALAPPDATA%\Programs\blink`, without asking for administrator rights. Its
one options screen adds a Start menu shortcut and starts blink when you sign
in to Windows; both are on by default. The last screen can launch blink right
away. Each release also has a `.sha256` file with the installer's checksum.

To change whether blink starts when you sign in later, use **Start blink when
I sign in** in blink's settings. Turning blink off under **Settings > Apps >
Startup** in Windows also works, and blink's settings show it as off.

For a quiet install, pass `0` to skip either option:

```powershell
msiexec /i blink-<version>-x86_64.msi /qn STARTMENUSHORTCUT=0 AUTOSTART=0
```

A quiet install never launches blink.

The installer is not code-signed yet, so Microsoft Defender SmartScreen may
warn that it is from an unknown publisher. Select **More info**, then
**Run anyway**.

Installing a newer release upgrades blink in place and keeps your choices: the
Start menu shortcut if you had it, and starting at sign-in as blink's
settings last left it. To remove blink, uninstall it from **Settings > Apps >
Installed apps**; that also stops it starting at sign-in. Uninstalling keeps
your settings (`%APPDATA%\blink`), logs (`%LOCALAPPDATA%\blink\logs`), and saved
clips.

## Building

The workspace builds and lints on macOS and Linux as well as Windows. Windows
capture, audio, encode, and system code is gated behind `cfg(windows)`, and
those crates export only their portable parts elsewhere, so `cargo check`,
`cargo test`, and `cargo clippy` work on any host.

```sh
just check
```

`just check` runs formatting, linting (host and Windows target), tests, Miri,
dependency and unused-dependency checks, and documentation builds. CI requires
it to pass.

See [CONTRIBUTING.md](CONTRIBUTING.md) for prerequisites, the full recipe list,
and the toolchain bump procedure.

## Known limitations

- Some older DX9-era exclusive-fullscreen games may capture black. blink never
  injects into game processes, so it does not trip anti-cheat.
- Brief recording gaps on display mode changes, such as a game launch or an
  alt-tab.
- Protected (DRM) content that DXGI cannot capture.
- On dual-GPU laptops where the integrated GPU drives the display, frames need a
  cross-adapter copy, which costs extra overhead.
- Consumer NVENC session limits apply.
- A machine without an NVIDIA GPU gets a clear error rather than a silent
  fallback to CPU encoding.
- The save chime plays through your speakers, so system-audio capture records
  it, and the next clip that covers that moment includes it.

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): design decisions, crate layout,
  unsafe policy, and the performance rules every change must respect.
- [docs/BENCHMARKING.md](docs/BENCHMARKING.md): how to measure in-game impact.

## Third-party

[THIRD-PARTY-NOTICES.txt](THIRD-PARTY-NOTICES.txt) lists the licenses and
copyright notices of every dependency compiled into `blink.exe`, and release
artifacts ship it next to the binary. It includes the fonts the settings
window's toolkit, [egui](https://github.com/emilk/egui), embeds: Ubuntu-Light
(Ubuntu Font Licence 1.0), Hack (MIT and the Bitstream Vera licence), and
egui-icons (MIT).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
