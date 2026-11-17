//! NVENC H.264 encoding for blink.
//!
//! Loads `nvEncodeAPI64.dll` from the installed driver at runtime, registers
//! duplicated D3D11 textures with NVENC, and produces encoded packets for the
//! ring. See `docs/ARCHITECTURE.md` for the preset and rate-control rules.
//!
//! Windows only. Every module is gated behind `#[cfg(windows)]`, so on other
//! hosts this crate compiles to an empty stub and the workspace still builds.
//! Every `unsafe` block needs a `// SAFETY:` comment and may contain only one
//! unsafe operation.
//!
//! Nothing is implemented yet.
