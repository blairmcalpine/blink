# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.3](https://github.com/blairmcalpine/blink/compare/v0.0.2...v0.0.3) - 2026-11-22

### Added

- *(app)* play a sound when a clip is saved ([#8](https://github.com/blairmcalpine/blink/pull/8))

## [0.0.2](https://github.com/blairmcalpine/blink/compare/v0.0.1...v0.0.2) - 2026-11-22

### Fixed

- *(capture)* stop the duplication rebuild loop and black frames ([#6](https://github.com/blairmcalpine/blink/pull/6))

## [0.0.1]

### Added

- `blink-capture`: DXGI Desktop Duplication into a pool of GPU textures, with
  device-loss and mode-change recovery.
- `blink-encode`: NVENC H.264 encoding straight from the captured D3D11
  textures through hand-written bindings, with periodic and on-demand IDRs.
- `blink-audio`: WASAPI loopback capture of system audio, a QPC-anchored
  timeline that fills silence and corrects drift, and AAC-LC encoding through
  the Media Foundation encoder.
- `blink-ring`: the preallocated encoded-packet ring with whole-GOP eviction
  and GOP-aligned clip cuts, tested under Miri.
- `blink-mux`: a hand-written faststart MP4 writer for variable-frame-rate
  H.264 and AAC, including the `pasp` box for stretched output.
- `blink-config`: the shared TOML config with validation and atomic saves.
- `blink-ui`: the egui settings window, run in a separate `--settings`
  process.
- `blink-sys`: shared Win32 plumbing (QPC clock, thread priority, single
  instance, message pump) plus retry backoff, error-chain rendering, and
  rename helpers.
- `blink-app`: the tray app that wires the pipeline together, with the save
  hotkey, live settings reload, clip saving off the hot path, and file logging
  with crash reports.
- A per-user Windows installer (MSI) that needs no administrator rights, and
  an icon, version information, and application manifest embedded in
  `blink.exe`, which links the C runtime statically.
- Releases cut by release-plz from Conventional Commit titles, with the MSI
  and its SHA-256 checksum attached to each GitHub release.
- Strict workspace lints, `just` recipes, `lefthook` hooks, and CI covering
  formatting, clippy on the host and Windows target, tests, Miri, dependencies,
  and documentation.
- Design documentation (`docs/ARCHITECTURE.md`) and the benchmarking procedure
  (`docs/BENCHMARKING.md`).
- Open-source scaffolding: dual MIT/Apache-2.0 license, README, contributing
  guide, security policy, and issue and pull request templates.
