# Contributing to blink

Thanks for taking the time to contribute. blink is a performance-first project:
correctness and predictable frame pacing matter more than feature count, and the
review bar reflects that.

## Prerequisites

Install the tools below before your first build.

### Rust

Install [rustup](https://rustup.rs). The pinned toolchain lives in
`rust-toolchain.toml`; rustup installs it automatically on your first `cargo`
command. Do not change the toolchain to "make something build" in a feature PR.

A nightly toolchain is also needed, but only for Miri:

```sh
rustup toolchain install nightly
```

### Development tools

| Tool | Purpose |
| --- | --- |
| `just` | Task runner; the single entry point for fmt, lint, test, and docs |
| `cargo-nextest` | Test runner used by `just test` |
| `cargo-deny` | License, advisory, ban, and source checks |
| `cargo-machete` | Unused dependency detection |
| `cargo-xwin` | Lints Windows code from macOS or Linux |
| `taplo-cli` | TOML formatting and checking |
| `typos-cli` | Spell checking |
| `lefthook` | Local git hooks |

```sh
cargo install --locked just cargo-nextest cargo-deny cargo-machete cargo-xwin \
  taplo-cli typos-cli lefthook
```

`cargo-xwin` needs LLVM, because it typechecks against Windows headers and may
build C dependencies:

- macOS: `brew install llvm` and put `llvm-config` on `PATH`
- Linux: install `clang`, `lld`, and `llvm` from your package manager
- Windows: not needed; native `cargo clippy` covers the Windows target

Then enable the git hooks once per clone:

```sh
lefthook install
```

The hooks run `cargo fmt --check`, `taplo fmt --check`, and `typos` before a
commit, and `just lint test` before a push.

### Windows-specific work

`cargo xwin clippy --target x86_64-pc-windows-msvc` typechecks Windows code but
never links or runs it. That is enough to catch most mistakes in a
`#[cfg(windows)]` module from a Mac, but it can break on C build scripts (for
example a `bindgen`-generated `-sys` crate that needs libclang). `windows-latest`
CI is authoritative for anything that must actually run on Windows.

Capture, encode, and audio changes cannot be exercised locally on macOS. Say so
in the PR and describe what you tested on hardware.

## Workflow

Before opening a pull request:

```sh
just check
```

It must pass. CI runs the same set of checks.

### Recipes

| Recipe | What it does |
| --- | --- |
| `just fmt` | Format Rust and TOML |
| `just lint` | Host clippy plus Windows clippy through `cargo xwin`, both `-D warnings` |
| `just lint-host` | Clippy for the host target only |
| `just lint-windows` | Clippy for `x86_64-pc-windows-msvc` only |
| `just test` | `cargo nextest` on the host. Windows-only tests run in CI |
| `just miri` | Run `blink-ring` tests under Miri (nightly) |
| `just deny` | Licenses, advisories, dependency bans, and sources |
| `just doc` | Build docs with `RUSTDOCFLAGS=-D warnings` |
| `just machete` | Detect unused dependencies |
| `just check` | All of the above |

## Code rules

These are enforced by the workspace lints, but read them before you write code
that fights the compiler.

- No `unwrap`, `expect`, or `panic` outside tests. Libraries use `thiserror`;
  `anyhow` is allowed only in `blink-app`. Use
  `#[expect(clippy::unwrap_used, reason = "...")]` sparingly, and only where the
  invariant is genuinely unrepresentable in the type system.
- Every `unsafe` block carries a `// SAFETY:` comment that states why the
  operation is sound. One unsafe operation per block; split blocks instead of
  explaining several operations at once.
- Use `#[expect(lint, reason = "...")]`, never `#[allow]`. `#[expect]` errors
  once the exception is stale, which keeps suppressions from rotting. A reason
  is mandatory.
- All output goes through `tracing`. `println!` and `eprintln!` are denied.
  Never log a line from the capture or encode threads at a level that is enabled
  by default.
- Windows code stays behind `#[cfg(windows)]`. FFI crates declare their
  dependencies under `[target.'cfg(windows)'.dependencies]` and compile to a
  stub elsewhere, so the whole workspace keeps building on macOS and Linux.
- No gratuitous `as` casts; prefer `From`/`TryFrom`. Indexing that could panic is
  denied in `blink-ring` and `blink-mux`.
- Read the performance rules in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
  before touching the capture, encode, ring, or mux path. Zero-copy, no per-frame
  allocation, and no hot-path blocking I/O are hard requirements, not
  suggestions.

## Benchmarks

Any change that touches the hot path (capture, color conversion, encode, ring
buffering, muxing, thread priorities) needs before-and-after numbers recorded in
the pull request description, following
[docs/BENCHMARKING.md](docs/BENCHMARKING.md). A change without numbers will be
asked for them during review.

Hosted CI runners have no NVIDIA GPU, so CI proves build, lint, and unit tests
only. The pull request template includes a manual smoke checklist for hardware.

## Toolchain bumps

`rust-toolchain.toml` and the workspace `rust-version` are bumped deliberately,
in a PR that does nothing else:

1. Edit `channel` in `rust-toolchain.toml` to the new exact stable version.
2. Update `rust-version` in `[workspace.package]` only when the new stable
   release becomes the minimum supported Rust version you intend to support.
   These are separate decisions; a toolchain bump does not automatically raise
   the MSRV.
3. Fix any new lints in the same PR. Do not paper over them with `#[expect]`
   unless the lint is genuinely wrong for this code.
4. Run `just check` and confirm the MSRV CI job passes.

Dependabot does not bump `rust-toolchain.toml`, so this is always a manual
change.

## Pull requests

- Keep changes focused. Small PRs get reviewed faster.
- Open pull requests as drafts until they are ready for review.
- Fill in the template, including the checklist and the smoke-test section.
- By contributing you agree that your contribution is dual licensed under MIT
  and Apache-2.0, as described in the README.
