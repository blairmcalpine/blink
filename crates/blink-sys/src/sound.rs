//! Short notification sounds played from memory through winmm's
//! `PlaySoundW`.
//!
//! `PlaySoundW` with `SND_ASYNC` returns once playback has been handed to
//! winmm's own thread, so the caller never waits for the sound. With
//! `SND_MEMORY` that thread reads the WAV image in place for the whole
//! playback, which is why [`play_wav`] takes a `'static` slice. A new sound
//! from this process stops the one still playing.

#[cfg(windows)]
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
#[cfg(windows)]
use windows::core::PCWSTR;

#[cfg(windows)]
use crate::SysError;

/// Starts playing `wav`, a complete WAV file image, and returns at once.
///
/// # Errors
///
/// [`SysError::InvalidWav`] if `wav` is not a RIFF `WAVE` image whose
/// header length matches the slice, and [`SysError::PlaySound`] if Windows
/// cannot play it (no output device, say, or a format it does not support).
#[cfg(windows)]
pub fn play_wav(wav: &'static [u8]) -> Result<(), SysError> {
    if !is_wav_image(wav) {
        return Err(SysError::InvalidWav);
    }
    // SAFETY: SND_MEMORY makes the first argument a pointer to a WAV image
    // rather than a string. winmm reads it as bytes, bounded by the RIFF
    // length, which `is_wav_image` checked against the slice. The slice is
    // 'static, so it outlives the asynchronous playback that keeps reading
    // it after this call returns. SND_NODEFAULT stops a bad image from
    // falling back to the default system sound.
    let played = unsafe {
        PlaySoundW(
            PCWSTR(wav.as_ptr().cast()),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        )
    };
    if played.as_bool() {
        Ok(())
    } else {
        Err(SysError::PlaySound)
    }
}

/// Whether `bytes` starts with a RIFF `WAVE` header whose length field
/// covers exactly the slice, so a reader that trusts the header stays in
/// bounds.
fn is_wav_image(bytes: &[u8]) -> bool {
    let riff_len = bytes
        .get(4..8)
        .and_then(|len| <[u8; 4]>::try_from(len).ok())
        .map(u32::from_le_bytes)
        .and_then(|len| usize::try_from(len).ok());
    bytes.get(0..4) == Some(b"RIFF")
        && bytes.get(8..12) == Some(b"WAVE")
        && riff_len.and_then(|len| len.checked_add(8)) == Some(bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(riff_len: u32, form: &[u8], body: usize) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&riff_len.to_le_bytes());
        bytes.extend_from_slice(form);
        bytes.resize(12 + body, 0);
        bytes
    }

    #[test]
    fn accepts_a_header_that_matches_the_slice() {
        assert!(is_wav_image(&image(4, b"WAVE", 0)));
        assert!(is_wav_image(&image(40, b"WAVE", 36)));
    }

    #[test]
    fn rejects_lengths_that_overrun_or_underrun_the_slice() {
        assert!(!is_wav_image(&image(41, b"WAVE", 36)));
        assert!(!is_wav_image(&image(39, b"WAVE", 36)));
        assert!(!is_wav_image(&image(u32::MAX, b"WAVE", 36)));
    }

    #[test]
    fn rejects_other_forms_and_short_input() {
        assert!(!is_wav_image(&image(40, b"AVI ", 36)));
        let mut not_riff = image(40, b"WAVE", 36);
        not_riff[0] = b'X';
        assert!(!is_wav_image(&not_riff));
        assert!(!is_wav_image(b""));
        assert!(!is_wav_image(b"RIFF\x04\x00\x00\x00WAV"));
    }
}
