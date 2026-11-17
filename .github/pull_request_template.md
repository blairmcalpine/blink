## Summary

<!-- What does this change do, in a sentence or two? -->

## Why

<!-- The problem this solves, or the design decision it implements. Link related issues. -->

## Checklist

- [ ] `just check` passes locally
- [ ] Every new `unsafe` block has a `// SAFETY:` comment explaining why it is sound
- [ ] Hot-path changes include before-and-after benchmark numbers per `docs/BENCHMARKING.md` (capture, color conversion, encode, ring, muxing, thread priorities)
- [ ] No `unwrap`, `expect`, or `panic` outside tests, and no new `#[allow]`
- [ ] Windows-specific code is behind `#[cfg(windows)]`
- [ ] Docs and `CHANGELOG.md` updated if behavior or design changed

## Benchmark numbers

<!-- Required for hot-path changes. Paste the results table from docs/BENCHMARKING.md. -->

## Manual smoke test

Hosted CI runners have no NVIDIA GPU, so the following must be checked on real
hardware. Check only what this change can plausibly affect, but check it for any
capture, encode, audio, or mux change.

- [ ] Blink starts on Windows 10 or 11 with an NVIDIA GPU and reports the encoder as available
- [ ] A hotkey press saves a clip to the configured folder
- [ ] The saved clip plays end to end with both video and audio in sync
- [ ] No dropped or duplicated audio, and no audio drift across a long clip
- [ ] CS2 fullscreen at a stretched 4:3 resolution, alt-tab out and back twice, save a clip, confirm it plays and (with `stretch_to` set) displays stretched
- [ ] In-game FPS impact is within a few percent of baseline, or matches the numbers reported above
- [ ] Nothing unexpected in the log, and nothing logged from the hot path at a default-enabled level

### Environment

| Field | Value |
| --- | --- |
| Windows version and build | |
| GPU model | |
| NVIDIA driver version | |
| blink commit | |
