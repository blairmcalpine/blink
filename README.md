# blink

A bare-bones Windows screen clipper. Press a hotkey and blink saves the last N
seconds of your screen and system audio as an MP4 in a folder you choose.

blink exists for one reason: clipping should not cost frames. Capture runs
through DXGI Desktop Duplication, H.264 encoding runs on the GPU through NVIDIA
NVENC without a copy back to system memory, and only encoded packets are kept in
memory. Saving a clip cuts the ring buffer at a keyframe and remuxes it, so the
clip is never re-encoded. blink lives in the system tray, and a minimal settings
window opens only on demand in a separate process.

## Status

Pre-alpha. The repository currently contains the project foundation (workspace
layout, tooling, guardrails, design docs) and no working capture, encode, or mux
code. Nothing is usable yet.

## Requirements

- Windows 10 or Windows 11 (x86_64)
- An NVIDIA GPU with NVENC support
- A recent NVIDIA driver, new enough to ship `nvEncodeAPI64.dll` for your GPU

blink does not ship or redistribute NVIDIA binaries; the encoder library is
loaded from the installed driver at runtime.

## Building

The workspace builds and lints on macOS and Linux as well as Windows. Windows
capture, audio, encode, and system code is gated behind `cfg(windows)` and
compiles to a stub elsewhere, so `cargo check`, `cargo test`, and `cargo clippy` work on any
host.

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
