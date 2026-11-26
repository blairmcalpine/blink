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
| `cargo-about` | Generates `THIRD-PARTY-NOTICES.txt` (install with `--features cli`) |
| `cargo-xwin` | Lints Windows code from macOS or Linux |
| `taplo-cli` | TOML formatting and checking |
| `typos-cli` | Spell checking |
| `lefthook` | Local git hooks |

```sh
cargo install --locked just cargo-nextest cargo-deny cargo-machete cargo-xwin \
  taplo-cli typos-cli lefthook
cargo install --locked --features cli cargo-about
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
| `just fmt-check` | Fail if Rust code is not formatted |
| `just taplo-check` | Fail if TOML files are not formatted |
| `just typos` | Spell-check the repository |
| `just lint` | Host clippy plus Windows clippy through `cargo xwin`, both `-D warnings` |
| `just lint-host` | Clippy for the host target only |
| `just lint-windows` | Clippy for `x86_64-pc-windows-msvc` only |
| `just test` | `cargo nextest` on the host. Windows-only tests run in CI |
| `just miri` | Run `blink-ring` tests under Miri (nightly) |
| `just deny` | Licenses, advisories, dependency bans, and sources |
| `just doc` | Build docs with `RUSTDOCFLAGS=-D warnings` |
| `just machete` | Detect unused dependencies |
| `just notices-check` | Fail if `THIRD-PARTY-NOTICES.txt` is out of date |
| `just notices` | Regenerate `THIRD-PARTY-NOTICES.txt`; run it after any dependency change |
| `just icon` | Regenerate `crates/blink-app/assets/blink.ico` after changing the tray icon drawing |
| `just check` | All of the above except `just fmt`, `just notices`, and `just icon` |

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
  The capture and encode threads never log per frame at a level that is
  enabled by default; state changes, retries, and failures are fine. Log
  errors as `error = %ErrorChain(&error)` (`blink_sys::ErrorChain`) so the
  source chain, including any HRESULT, reaches the log.
- Windows code stays behind `#[cfg(windows)]`. FFI crates declare their
  dependencies under `[target.'cfg(windows)'.dependencies]` and export only
  their portable items elsewhere, so the whole workspace keeps building on
  macOS and Linux.
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
- Title the PR as a [Conventional Commit](https://www.conventionalcommits.org/),
  such as `feat: add a clip-saved sound` or `fix(capture): recover from a TDR`.
  PRs are squash-merged with the title as the commit message, and releases are
  versioned from those messages. A CI check flags other titles. Commits inside
  the PR can say whatever helps review.
- Fill in the template, including the checklist and the smoke-test section.
- By contributing you agree that your contribution is dual licensed under MIT
  and Apache-2.0, as described in the README.

## Releasing

Releases are cut by [release-plz](https://release-plz.dev), configured in
`release-plz.toml` and run by `.github/workflows/release.yml` on every push to
`main`. Git tags are the source of truth: the last release is the newest
`v{version}` tag, and nothing is published to crates.io. Every crate shares the
workspace version, and the release covers them all.

1. After a merge to `main`, release-plz opens or updates a release PR titled
   `chore: release v{version}`. It bumps the workspace version from the
   Conventional Commits since the last tag and adds their entries to
   `CHANGELOG.md`. While the version is `0.x`, a breaking change bumps the
   minor version and anything else bumps the patch. Edit the changelog in that
   PR if the generated wording needs work.
2. Merging the release PR tags the merge commit `v{version}` and creates a
   draft GitHub release whose notes are the new changelog entry.
3. The same workflow builds the MSI from the tag on `windows-latest`, signs
   it, attaches it and its `.sha256` file to the release, and publishes the
   release. Signing waits up to an hour for a maintainer to approve the
   signing request in SignPath.

### Code signing

Releases are signed through the SignPath Foundation
([policy](docs/CODE_SIGNING.md)). The publish job signs only when the
`SIGNPATH_PROJECT_SLUG` repository variable is set; without it, releases ship
unsigned. Setup, once SignPath approves the project:

1. In SignPath, link the project to the GitHub.com trusted build system and
   paste `packaging/windows/signpath-artifact-configuration.xml` into its
   default artifact configuration. Keep the two in sync.
2. Create an API token for a user with submitter rights on the release signing
   policy, and store it as the `SIGNPATH_API_TOKEN` repository secret.
3. Set the `SIGNPATH_ORGANIZATION_ID`, `SIGNPATH_PROJECT_SLUG`, and
   `SIGNPATH_SIGNING_POLICY_SLUG` repository variables.

Signing changes the MSI's bytes, so the `.sha256` file is written after it.

If packaging fails, the release stays a draft. If the cause was transient,
use **Re-run failed jobs**; re-running all jobs does nothing, because the tag
already exists. To finish a draft from a later run, start the Release workflow
from the Actions tab with the draft's tag, such as `v1.2.3`; it skips
release-plz, then packages and publishes that draft. If the fix needs code,
delete the draft release and its tag and merge the fix; the next push to
`main` releases the same version again.

### Building the installer

On Windows, with the pinned Rust toolchain, the Visual Studio C++ build tools,
the .NET SDK, and WiX Toolset 5 (the commands are in
`packaging/windows/build-msi.ps1`):

```powershell
./packaging/windows/build-exe.ps1
./packaging/windows/build-msi.ps1
```

The MSI lands in `dist/`. `.github/workflows/package-windows.yml` runs the same
scripts for pull requests that touch packaging inputs (`blink-app`, manifests,
the lockfile, `packaging/`), on every push to main, and on demand from the
Actions tab, and uploads the MSI as an artifact. Only that workflow builds and
ICE-validates `blink.wxs`, so check its run after any installer change.

### Installer options

The options dialog sets two public properties, which a quiet install
(`msiexec /i blink.msi /qn`) can set on the command line:

| Property | Default | Effect when `1` |
| --- | --- | --- |
| `STARTMENUSHORTCUT` | `1` | Adds the Start menu shortcut |
| `AUTOSTART` | `1` | Writes blink's value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` |

Any other value skips that part. Without a value on the command line, the
default depends on what is installed:

- A first install, or an upgrade from a version without options (0.0.x),
  turns both on.
- Later upgrades and repairs keep the current state. The shortcut stays if
  the last install added it, which `HKCU\Software\blink\Installer` records;
  starting at sign-in stays on only if blink's Run value still exists. Any
  value named `blink` counts, so one that starts another copy (a dev build,
  say) is pointed at the installed `blink.exe`.

The Run value is the one record of whether blink starts at sign-in. The
settings window's **Start blink when I sign in** reads and writes it
(`blink_sys::read_autostart` and `write_autostart`) instead of a config
field, so an upgrade never re-adds a value the user turned off there. The
`Autostart` component is transitive for the same reason: a repair follows
the current value rather than restoring the install-time choice. A Run value
that starts a different `blink.exe` reads as off, and saving the box checked
points it at the running copy.

Windows' startup apps switch records a disabled entry under
`Explorer\StartupApproved\Run` without touching the Run value; blink's
settings then show the box unchecked with a note, and turning it on there
deletes that entry. A full uninstall deletes both values through `reg.exe`
(`WixQuietExec`), whichever of the installer or blink created them. Those
deletes have no rollback, so an uninstall that fails after them leaves blink
installed but no longer starting at sign-in. During an upgrade they skip,
the new version rewrites the Run value if it existed, and a disabled
startup-apps entry stays disabled.
