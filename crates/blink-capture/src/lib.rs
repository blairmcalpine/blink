//! DXGI Desktop Duplication capture for blink.
//!
//! Owns the D3D11 device and the duplication session, and hands duplicated
//! textures to `blink-encode` without leaving the GPU. Recovers from
//! `DXGI_ERROR_ACCESS_LOST` on display mode changes.
//!
//! Windows only. Every module is gated behind `#[cfg(windows)]`, so on other
//! hosts this crate compiles to an empty stub and the workspace still builds.
//! Every `unsafe` block needs a `// SAFETY:` comment and may contain only one
//! unsafe operation.
//!
//! Nothing is implemented yet.
