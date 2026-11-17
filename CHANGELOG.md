# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Repository foundation: Cargo workspace with `blink-app`, `blink-ring`,
  `blink-mux`, `blink-capture`, `blink-audio`, and `blink-encode` crate
  skeletons, with Windows-only code gated behind `cfg(windows)`.
- Strict workspace lints, `just` recipes, `lefthook` hooks, and CI covering
  formatting, clippy on the host and Windows target, tests, Miri, dependencies,
  and documentation.
- Design documentation (`docs/ARCHITECTURE.md`) and the benchmarking procedure
  (`docs/BENCHMARKING.md`).
- Open-source scaffolding: dual MIT/Apache-2.0 license, README, contributing
  guide, code of conduct, security policy, and issue and pull request templates.

No functional capture, encode, or muxing code exists yet.
