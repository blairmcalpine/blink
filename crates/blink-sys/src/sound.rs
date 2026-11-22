//! Short notification sounds played from memory through winmm's
//! `PlaySoundW`.
//!
//! With `SND_ASYNC`, `PlaySoundW` hands playback to winmm's own thread
//! instead of playing the sound before it returns. With `SND_MEMORY` that
//! thread reads the WAV image in place for the whole playback, which is why
//! [`play_wav`] takes a `'static` slice. A new sound from this process stops
//! the one still playing. No `SND_SYSTEM`, so the sound follows blink's own
//! volume mixer entry rather than the system sounds slider.

#[cfg(windows)]
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
#[cfg(windows)]
use windows::core::PCWSTR;

#[cfg(windows)]
use crate::SysError;

/// Bytes in a chunk header: a four-byte id and a little-endian `u32` size.
const CHUNK_HEADER_LEN: usize = 8;

/// The smallest `fmt ` chunk body: a PCM `WAVEFORMAT` plus bits per sample.
const MIN_FMT_LEN: usize = 16;

/// Starts playing `wav`, a complete WAV file image, and returns without
/// waiting for the sound to finish.
///
/// # Errors
///
/// [`SysError::InvalidWav`] if `wav` is not a RIFF `WAVE` image whose RIFF
/// length and chunk sizes all stay inside the slice, with a `fmt ` chunk
/// before the `data` chunk, and [`SysError::PlaySound`] if Windows cannot play it (no output device,
/// say, or a format it does not support).
#[cfg(windows)]
pub fn play_wav(wav: &'static [u8]) -> Result<(), SysError> {
    if !is_wav_image(wav) {
        return Err(SysError::InvalidWav);
    }
    // SAFETY: SND_MEMORY makes the first argument a pointer to a WAV image
    // rather than a string, which winmm reads as bytes by following the
    // RIFF header and chunk sizes. `is_wav_image` checked that the RIFF
    // length is exactly the slice, and that every chunk header and body
    // (with its pad byte) lies inside it, so following them stays in
    // bounds. The slice is 'static, so it outlives the asynchronous
    // playback that keeps reading it after this call returns. SND_NODEFAULT
    // stops a rejected image from falling back to the default system sound.
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

/// Whether `bytes` is a RIFF `WAVE` image that a reader trusting its sizes
/// cannot overrun: the RIFF length covers exactly the slice, the chunks
/// from offset 12 tile the rest with each header, body, and odd-size pad
/// byte inside it, and a `fmt ` chunk of at least 16 bytes comes before the
/// `data` chunk.
fn is_wav_image(bytes: &[u8]) -> bool {
    let riff_len = read_u32(bytes, 4).and_then(|len| len.checked_add(8));
    if bytes.get(0..4) != Some(b"RIFF")
        || bytes.get(8..12) != Some(b"WAVE")
        || riff_len != Some(bytes.len())
    {
        return false;
    }
    let mut rest = bytes.get(12..).unwrap_or_default();
    let mut fmt = false;
    let mut data = false;
    while !rest.is_empty() {
        let Some(size) = read_u32(rest, 4) else {
            return false;
        };
        let padded = size.checked_add(size % 2);
        let Some(next) = padded
            .and_then(|padded| padded.checked_add(CHUNK_HEADER_LEN))
            .and_then(|end| rest.get(end..))
        else {
            return false;
        };
        match rest.get(0..4) {
            Some(b"fmt ") => fmt = size >= MIN_FMT_LEN,
            Some(b"data") if !fmt => return false,
            Some(b"data") => data = true,
            _ => {}
        }
        rest = next;
    }
    data
}

/// The little-endian `u32` at `at` in `bytes`, as a `usize`.
fn read_u32(bytes: &[u8], at: usize) -> Option<usize> {
    let end = at.checked_add(4)?;
    let raw = <[u8; 4]>::try_from(bytes.get(at..end)?).ok()?;
    usize::try_from(u32::from_le_bytes(raw)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: &[u8], size: u32, body: usize) -> Vec<u8> {
        let mut bytes = id.to_vec();
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.resize(8 + body, 0);
        bytes
    }

    /// A RIFF image of `form` holding `chunks`, with a correct RIFF length.
    fn image(form: &[u8], chunks: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = chunks.concat();
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&u32::try_from(4 + body.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(form);
        bytes.extend_from_slice(&body);
        bytes
    }

    fn fmt() -> Vec<u8> {
        chunk(b"fmt ", 16, 16)
    }

    fn wav(data: usize) -> Vec<u8> {
        image(
            b"WAVE",
            &[fmt(), chunk(b"data", u32::try_from(data).unwrap(), data)],
        )
    }

    #[test]
    fn accepts_well_formed_images() {
        assert!(is_wav_image(&wav(0)));
        assert!(is_wav_image(&wav(480)));
        let padded = image(b"WAVE", &[fmt(), chunk(b"data", 3, 4)]);
        assert!(is_wav_image(&padded));
        let extra = image(
            b"WAVE",
            &[
                chunk(b"LIST", 6, 6),
                fmt(),
                chunk(b"data", 2, 2),
                chunk(b"junk", 0, 0),
            ],
        );
        assert!(is_wav_image(&extra));
    }

    #[test]
    fn rejects_riff_lengths_that_do_not_match_the_slice() {
        for riff_len in [0, 35, 37, u32::MAX] {
            let mut bytes = wav(0);
            bytes[4..8].copy_from_slice(&riff_len.to_le_bytes());
            assert!(!is_wav_image(&bytes), "{riff_len}");
        }
    }

    #[test]
    fn rejects_inner_chunks_that_overrun_the_slice() {
        let cases = [
            image(b"WAVE", &[fmt(), chunk(b"data", 481, 480)]),
            image(b"WAVE", &[fmt(), chunk(b"data", u32::MAX, 480)]),
            image(b"WAVE", &[chunk(b"fmt ", 1_000, 16), chunk(b"data", 2, 2)]),
            // An odd size needs its pad byte inside the slice too.
            image(b"WAVE", &[fmt(), chunk(b"data", 3, 3)]),
            // A trailing partial chunk header.
            image(b"WAVE", &[fmt(), chunk(b"data", 2, 2), b"jun".to_vec()]),
        ];
        for bytes in cases {
            assert!(!is_wav_image(&bytes), "{bytes:?}");
        }
    }

    #[test]
    fn requires_a_full_fmt_chunk_before_data() {
        let short_fmt = image(b"WAVE", &[chunk(b"fmt ", 14, 14), chunk(b"data", 2, 2)]);
        let data_first = image(b"WAVE", &[chunk(b"data", 2, 2), fmt()]);
        let no_data = image(b"WAVE", &[fmt()]);
        let no_fmt = image(b"WAVE", &[chunk(b"data", 2, 2)]);
        for bytes in [short_fmt, data_first, no_data, no_fmt] {
            assert!(!is_wav_image(&bytes), "{bytes:?}");
        }
    }

    #[test]
    fn rejects_other_forms_and_short_input() {
        let mut avi = wav(2);
        avi[8..12].copy_from_slice(b"AVI ");
        assert!(!is_wav_image(&avi));
        let mut not_riff = wav(2);
        not_riff[0] = b'X';
        assert!(!is_wav_image(&not_riff));
        assert!(!is_wav_image(b""));
        assert!(!is_wav_image(b"RIFF\x04\x00\x00\x00WAV"));
        assert!(!is_wav_image(b"RIFF\x04\x00\x00\x00WAVE"));
    }
}
