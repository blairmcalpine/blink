//! The blink binary.
//!
//! Registers the save-clip hotkey, loads configuration, and wires capture,
//! encode, the packet ring, and the muxer together. None of that exists yet;
//! for now it only sets up `tracing`.

#![forbid(unsafe_code)]

use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init()
        .map_err(|err| anyhow::anyhow!("failed to install tracing subscriber: {err}"))?;

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "blink starting");

    Ok(())
}
