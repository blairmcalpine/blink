//! WASAPI loopback audio capture for blink.
//!
//! Captures system audio through WASAPI loopback, fills silent gaps on the
//! shared QPC timeline, and encodes AAC with the Media Foundation MFT that
//! ships with Windows.
//!
//! Windows only. Every module is gated behind `#[cfg(windows)]`, so on other
//! hosts this crate compiles to an empty stub and the workspace still builds.
//! Every `unsafe` block needs a `// SAFETY:` comment and may contain only one
//! unsafe operation.
//!
//! Nothing is implemented yet.
