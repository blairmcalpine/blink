//! The sound played on the tray thread when a save finishes.
//!
//! Each sound is synthesized once into a WAV image that lives until the
//! process exits, so playback, which keeps reading it after `play` returns,
//! allocates nothing and never outlives its buffer.

use std::sync::LazyLock;

use blink_sys::ErrorChain;

use crate::chime::{self, Chime};
use crate::status::SaveResult;

static SAVED: LazyLock<Box<[u8]>> = LazyLock::new(|| chime::wav(Chime::Saved).into());
static FAILED: LazyLock<Box<[u8]>> = LazyLock::new(|| chime::wav(Chime::Failed).into());
static WARM_UP: LazyLock<Box<[u8]>> = LazyLock::new(|| chime::warm_up_wav().into());

/// Synthesizes the sounds now, so a save never waits for it.
pub(super) fn prepare() {
    LazyLock::force(&SAVED);
    LazyLock::force(&FAILED);
}

/// Plays a moment of silence, so winmm's first-call setup (its playback
/// thread and the output device) happens now rather than when a save
/// finishes.
pub(super) fn warm_up() {
    start(&WARM_UP, "cannot warm up the save sound");
}

/// Starts the sound for `result` and returns without waiting for it.
pub(super) fn play(result: &SaveResult) {
    let wav: &'static [u8] = match result {
        SaveResult::Saved(_) => &SAVED,
        SaveResult::Failed(_) => &FAILED,
    };
    start(wav, "cannot play the save sound");
}

fn start(wav: &'static [u8], failure: &str) {
    if let Err(error) = blink_sys::play_wav(wav) {
        tracing::warn!(error = %ErrorChain(&error), "{failure}");
    }
}
