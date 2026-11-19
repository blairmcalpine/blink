//! Win32 plumbing shared by blink's crates.
//!
//! The capture, audio, and app crates all need a few small Windows services
//! that have nothing to do with capture or audio themselves: the shared QPC
//! clock ([`QpcClock`], `qpc_now`), thread priority
//! (`set_current_thread_priority`, `BackgroundMode`), a single-instance mutex
//! (`SingleInstance`), and a message pump for the tray thread
//! (`pump_messages`). They live here once, as safe wrappers, so `blink-app`
//! (which forbids `unsafe`) can use them and the FFI crates do not duplicate
//! them.
//!
//! A few portable helpers live here for the same reason, because every crate
//! that needs them already depends on this one: [`Backoff`], the retry delay
//! every reconnect loop uses; [`ErrorChain`], which renders an error with its
//! sources for logs and dialogs; and [`rename_replacing`] and
//! [`rename_no_replace`], which retry the sharing violations antivirus causes
//! on Windows.
//!
//! On other hosts only the portable items exist ([`QpcClock`]'s arithmetic,
//! [`SysError`], and the helpers above), so the workspace still builds and
//! their logic is unit tested everywhere. Every `unsafe` block needs a
//! `// SAFETY:` comment and may contain only one unsafe operation.

mod backoff;
mod clock;
#[cfg_attr(
    windows,
    expect(
        clippy::redundant_pub_crate,
        reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
    )
)]
mod error;
mod error_chain;
mod fs;
#[cfg(windows)]
mod instance;
#[cfg(windows)]
mod messages;
#[cfg(windows)]
mod thread;

pub use backoff::Backoff;
pub use clock::QpcClock;
#[cfg(windows)]
pub use clock::{qpc_counter, qpc_now};
pub use error::SysError;
pub use error_chain::ErrorChain;
pub use fs::{rename_no_replace, rename_replacing};
#[cfg(windows)]
pub use instance::SingleInstance;
#[cfg(windows)]
pub use messages::pump_messages;
#[cfg(windows)]
pub use thread::{BackgroundMode, ThreadPriority, set_current_thread_priority};
