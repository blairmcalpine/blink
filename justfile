# blink task runner: the single entry point for humans, agents, and CI.
# Run `just` to list recipes and `just check` before opening a pull request.

set shell := ["bash", "-euo", "pipefail", "-c"]

windows_target := "x86_64-pc-windows-msvc"

# cargo-xwin needs clang and lld. These are the Homebrew locations on macOS;
# override with BLINK_LLVM_PATH elsewhere. Directories that don't exist are harmless.
llvm_path := env("BLINK_LLVM_PATH", "/opt/homebrew/opt/llvm/bin:/opt/homebrew/opt/lld/bin")

# A Windows host lints its own target natively; everywhere else goes through xwin.
xwin := if os() == "windows" { "" } else { "xwin" }

clippy_args := "--workspace --all-targets --all-features -- -D warnings"

# List recipes.
default:
    @just --list

# Format Rust and TOML.
fmt:
    cargo fmt --all
    taplo fmt

# Check Rust formatting.
fmt-check:
    cargo fmt --all --check

# Check TOML formatting.
taplo-check:
    taplo fmt --check

# Spell check the repository.
typos:
    typos

# Host clippy plus Windows clippy, both with -D warnings.
lint: lint-host lint-windows

# Clippy for the host target only.
lint-host:
    cargo clippy {{ clippy_args }}

# Clippy for x86_64-pc-windows-msvc (typecheck only, never links).
lint-windows:
    PATH="{{ llvm_path }}:$PATH" cargo {{ xwin }} clippy --target {{ windows_target }} {{ clippy_args }}

# Unit tests through nextest, then doctests (nextest does not run doctests).
test:
    cargo nextest run --workspace --all-features --no-tests=pass
    cargo test --doc --workspace --all-features

# Run blink-ring tests under Miri (nightly).
miri:
    cargo +nightly miri test -p blink-ring

# Licenses, advisories, dependency bans, and sources.
deny:
    cargo deny --all-features check

# Build docs with warnings denied.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features

# Detect unused dependencies.
machete:
    cargo machete

# Everything CI checks. Must pass before a pull request.
check: fmt-check taplo-check typos lint test miri deny doc machete
