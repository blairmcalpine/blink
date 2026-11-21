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

# Regenerate THIRD-PARTY-NOTICES.txt, which ships next to blink.exe.
notices:
    just _notices THIRD-PARTY-NOTICES.txt

# Fail if THIRD-PARTY-NOTICES.txt is out of date.
notices-check:
    #!/usr/bin/env bash
    set -euo pipefail
    fresh="$(mktemp)"
    trap 'rm -f "$fresh"' EXIT
    just _notices "$fresh"
    if ! diff -u THIRD-PARTY-NOTICES.txt "$fresh"; then
        echo "THIRD-PARTY-NOTICES.txt is out of date; run 'just notices'." >&2
        exit 1
    fi

# Regenerate crates/blink-app/assets/blink.ico from the tray icon drawing.
# `just test` fails while it is out of date.
icon:
    BLINK_WRITE_ICON=1 cargo test --quiet -p blink-app --bin blink -- icon::tests::ico_is_current

# cargo-about's crate licenses, then the licenses of the fonts egui embeds,
# which are in epaint_default_fonts but not its package metadata.
_notices out:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo fetch --locked --quiet
    cargo about generate --locked --offline about.hbs -o "{{ out }}"
    manifest="$(cargo metadata --format-version 1 --locked --offline \
        | grep -oE '"manifest_path":"[^"]*epaint_default_fonts-[0-9.]+[/\\]+Cargo.toml"' \
        | head -n 1 | cut -d '"' -f 4)"
    crate="$(dirname "$manifest")"
    rule="================================================================================"
    {
        printf '\n%s\nFonts embedded by egui (%s)\n' "$rule" "$(basename "$crate")"
        printf '\nUbuntu-Light:\n\n'
        cat "$crate/fonts/UFL.txt"
        printf '\nHack-Regular:\n\n'
        cat "$crate/fonts/Hack-Regular.txt"
        printf '\negui-icons (emoji-icon-font):\n\n'
        cat "$crate/fonts/emoji-icon-font-mit-license.txt"
    } >> "{{ out }}"

# Everything CI checks. Must pass before a pull request.
check: fmt-check taplo-check typos lint test miri deny doc machete notices-check
