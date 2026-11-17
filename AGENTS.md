# AGENTS.md

blink is a Windows game-clip recorder: DXGI Desktop Duplication feeds NVENC
directly on the GPU, encoded packets sit in a ring buffer, and a hotkey cuts the
last N seconds into an MP4 without re-encoding. The primary goal is minimal
in-game FPS loss. Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) before
changing capture, encode, ring, or mux code.

## Layout

| Crate | Role | `unsafe` |
| --- | --- | --- |
| `crates/blink-app` | Binary: hotkey, config, wiring, `tracing` setup | forbidden |
| `crates/blink-ring` | Encoded-packet ring buffer, tested under Miri | forbidden |
| `crates/blink-mux` | MP4 muxing | forbidden |
| `crates/blink-config` (planned) | Shared config types + load/save | forbidden |
| `crates/blink-ui` (planned) | Slint settings window (`--settings` process) | forbidden |
| `crates/blink-capture` | DXGI Desktop Duplication (Windows FFI) | allowed, documented |
| `crates/blink-audio` | WASAPI loopback + AAC MFT (Windows FFI) | allowed, documented |
| `crates/blink-encode` | NVENC (Windows FFI) | allowed, documented |

## Commands

- `just check`: everything CI runs. Run it before you finish; it must pass.
- `just fmt`, `just lint` (host + Windows clippy via `cargo xwin`), `just test`,
  `just miri`, `just deny`, `just doc`, `just machete`.

The toolchain is pinned in `rust-toolchain.toml`. Do not change it, the lint
table, or `clippy.toml` to make code pass.

## Hard rules

- No `unwrap`, `expect`, `panic!`, `todo!`, or `unimplemented!` outside tests.
  Libraries return `thiserror` errors; `anyhow` is only for `blink-app`.
- Every `unsafe` block has a `// SAFETY:` comment explaining why it is sound,
  and contains exactly one unsafe operation. `unsafe fn` bodies still need
  explicit `unsafe` blocks. `unsafe` is only allowed in the three FFI crates.
- Suppress a lint only with `#[expect(lint, reason = "...")]`, never `#[allow]`.
- All output goes through `tracing`. No `println!`, `eprintln!`, or `dbg!`.
- No `as` casts; use `From`/`TryFrom`. No panicking indexing in `blink-ring` or
  `blink-mux`; use `get` and iterators.
- Windows code stays behind `#[cfg(windows)]`, with dependencies under
  `[target.'cfg(windows)'.dependencies]`, so the workspace builds on macOS and
  Linux.
- Every public item has a doc comment.
- No UI toolkit code in the engine process; UI lives in the separate settings
 process (`blink.exe --settings`).
- Performance (details in `docs/ARCHITECTURE.md`): the GPU path is zero-copy
  with no CPU readback; no NVENC features that run on CUDA cores; no per-frame
  allocation; capture and encode threads never block on I/O or wait on muxing.
