//! The sounds played when a save finishes, synthesized in code so blink
//! ships no audio file and needs no license for one.
//!
//! The engine renders each sound once at startup into a WAV image that lives
//! for the rest of the process, which is what asynchronous playback from
//! memory needs. Everything is integer arithmetic, so the output is exact on
//! every host, and a test pins a checksum of each image.

/// Sample rate of the rendered sounds: half the 48 kHz rate Windows usually
/// mixes at, and plenty for tones under 1 kHz.
pub(crate) const SAMPLE_RATE: u32 = 24_000;

/// One in Q15 fixed point.
const ONE: i64 = 1 << 15;

/// Peak amplitude of each note, in Q15: about -12 dBFS, so two overlapping
/// notes stay well clear of clipping and the chime sits under game audio.
const NOTE_GAIN: i64 = ONE / 4;

/// How long each note takes to reach full volume, in milliseconds. Long
/// enough to avoid a click, short enough to sound struck.
const ATTACK_MS: u32 = 6;

/// Silence before the first note, in milliseconds. A Bluetooth (A2DP)
/// output that has gone idle can drop the first moments of audio while its
/// stream wakes, which would otherwise clip the first note.
const LEAD_IN_MS: u32 = 40;

/// Length of the silent warm-up sound, in milliseconds.
const WARM_UP_MS: u32 = 10;

/// Bytes in the RIFF, `fmt `, and `data` headers of a PCM WAV image.
const WAV_HEADER_LEN: usize = 44;

/// Which save outcome a sound announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Chime {
    /// A rising two-note chime: the clip was written.
    Saved,
    /// A lower, falling pair of notes: the save failed.
    Failed,
}

/// A sine tone that starts at `start_ms`, rises over [`ATTACK_MS`], and
/// decays to silence by the end of `length_ms`.
struct Note {
    start_ms: u32,
    length_ms: u32,
    hz: u32,
}

impl Chime {
    /// The notes, in start order. Each starts before the previous one has
    /// faded, so they ring together like a struck chime.
    const fn notes(self) -> &'static [Note] {
        match self {
            // E5 then A5, a rising fourth: 250 ms after the lead-in.
            Self::Saved => &[
                Note {
                    start_ms: 0,
                    length_ms: 150,
                    hz: 659,
                },
                Note {
                    start_ms: 80,
                    length_ms: 170,
                    hz: 880,
                },
            ],
            // A4 then E4, the same interval falling an octave lower: 300 ms
            // after the lead-in.
            Self::Failed => &[
                Note {
                    start_ms: 0,
                    length_ms: 170,
                    hz: 440,
                },
                Note {
                    start_ms: 120,
                    length_ms: 180,
                    hz: 330,
                },
            ],
        }
    }
}

/// The sound for `chime` as a complete 16-bit mono PCM WAV file image.
pub(crate) fn wav(chime: Chime) -> Vec<u8> {
    wav_image(&samples(chime))
}

/// A WAV image of [`WARM_UP_MS`] of silence, played once so the first real
/// sound does not pay for opening the output device.
pub(crate) fn warm_up_wav() -> Vec<u8> {
    wav_image(&vec![0; samples_in(WARM_UP_MS)])
}

/// `samples` as a complete 16-bit mono PCM WAV file image.
fn wav_image(samples: &[i16]) -> Vec<u8> {
    let data_len = u32::try_from(samples.len() * 2).unwrap_or(0);
    let mut out = Vec::with_capacity(WAV_HEADER_LEN + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1_u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // bytes per second
    out.extend_from_slice(&2_u16.to_le_bytes()); // bytes per frame
    out.extend_from_slice(&16_u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// The mixed notes of `chime` as signed 16-bit samples.
fn samples(chime: Chime) -> Vec<i16> {
    let notes = chime.notes();
    let len = notes
        .iter()
        .map(|note| samples_in(LEAD_IN_MS + note.start_ms + note.length_ms))
        .max()
        .unwrap_or(0);
    let mut mix = vec![0_i64; len];
    for note in notes {
        let length = samples_in(note.length_ms);
        let step = phase_step(note.hz);
        let mut phase = 0_u32;
        let start = samples_in(LEAD_IN_MS + note.start_ms);
        let span = mix.iter_mut().skip(start).take(length);
        for (index, out) in span.enumerate() {
            *out += sine(phase) * envelope(index, length) / ONE * NOTE_GAIN / ONE;
            phase = phase.wrapping_add(step);
        }
    }
    mix.into_iter()
        .map(|sample| {
            let clamped = sample.clamp(i64::from(i16::MIN), i64::from(i16::MAX));
            i16::try_from(clamped).unwrap_or_default()
        })
        .collect()
}

/// The number of samples in `ms` milliseconds.
fn samples_in(ms: u32) -> usize {
    usize::try_from(ms * SAMPLE_RATE / 1000).unwrap_or(0)
}

/// How far a tone of `hz` advances its phase per sample, where a full cycle
/// is 2^32.
fn phase_step(hz: u32) -> u32 {
    u32::try_from((u64::from(hz) << 32) / u64::from(SAMPLE_RATE)).unwrap_or(0)
}

/// The sine of `phase` (a full cycle is 2^32) in Q15, by Bhaskara I's
/// rational approximation. Its error is under 0.2% of full scale, inaudible
/// at chime volume.
fn sine(phase: u32) -> i64 {
    const HALF_CYCLE: u32 = 1 << 31;
    /// Half a cycle in the reduced units the formula works in.
    const HALF: i64 = 1 << 15;
    let (within, sign) = if phase < HALF_CYCLE {
        (phase, 1)
    } else {
        (phase - HALF_CYCLE, -1)
    };
    let t = i64::from(within >> 16);
    let p = t * (HALF - t);
    sign * 16 * p * (ONE - 1) / (5 * HALF * HALF - 4 * p)
}

/// The volume of sample `index` of a `length`-sample note, in Q15: a linear
/// rise over [`ATTACK_MS`], then a quadratic fall that reaches zero at the
/// end, so the note neither clicks in nor cuts off.
fn envelope(index: usize, length: usize) -> i64 {
    let attack = i64::try_from(samples_in(ATTACK_MS)).unwrap_or(1).max(1);
    let index = i64::try_from(index).unwrap_or(0);
    let length = i64::try_from(length).unwrap_or(0);
    if index < attack {
        return ONE * index / attack;
    }
    let decay = (length - attack).max(1);
    let left = (length - index).max(0);
    ONE * left * left / (decay * decay)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    fn millis(samples: &[i16]) -> usize {
        samples.len() * 1000 / usize::try_from(SAMPLE_RATE).unwrap()
    }

    /// Sign changes per second over `samples`, about twice the frequency.
    fn crossings_per_second(samples: &[i16]) -> usize {
        let crossings = samples
            .windows(2)
            .filter(|pair| (pair[0] < 0) != (pair[1] < 0))
            .count();
        crossings * usize::try_from(SAMPLE_RATE).unwrap() / samples.len()
    }

    #[test]
    fn wav_header_describes_the_samples() {
        for chime in [Chime::Saved, Chime::Failed] {
            let wav = wav(chime);
            let data = u32::try_from(samples(chime).len() * 2).unwrap();
            assert_eq!(&wav[0..4], b"RIFF");
            assert_eq!(u32_at(&wav, 4), u32::try_from(wav.len() - 8).unwrap());
            assert_eq!(&wav[8..16], b"WAVEfmt ");
            assert_eq!(u32_at(&wav, 16), 16);
            assert_eq!(u16_at(&wav, 20), 1, "PCM");
            assert_eq!(u16_at(&wav, 22), 1, "mono");
            assert_eq!(u32_at(&wav, 24), SAMPLE_RATE);
            assert_eq!(u32_at(&wav, 28), SAMPLE_RATE * 2);
            assert_eq!(u16_at(&wav, 32), 2);
            assert_eq!(u16_at(&wav, 34), 16);
            assert_eq!(&wav[36..40], b"data");
            assert_eq!(u32_at(&wav, 40), data);
            assert_eq!(wav.len(), WAV_HEADER_LEN + usize::try_from(data).unwrap());
            let first = i16::from_le_bytes([wav[44], wav[45]]);
            assert_eq!(first, samples(chime)[0]);
        }
    }

    #[test]
    fn sounds_are_short_and_modest() {
        for (chime, ms) in [(Chime::Saved, 290), (Chime::Failed, 340)] {
            let samples = samples(chime);
            assert_eq!(millis(&samples), ms, "{chime:?}");
            let lead_in = samples_in(LEAD_IN_MS);
            assert!(samples[..lead_in].iter().all(|&s| s == 0), "{chime:?}");
            assert_ne!(samples[lead_in + 24], 0, "{chime:?}");
            let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
            assert!((6_000..=16_384).contains(&peak), "{chime:?} peak {peak}");
        }
    }

    #[test]
    fn sounds_fade_in_and_out_without_clicks() {
        for chime in [Chime::Saved, Chime::Failed] {
            let samples = samples(chime);
            assert_eq!(samples[0], 0, "{chime:?}");
            let tail = &samples[samples.len() - 24..];
            assert!(
                tail.iter().all(|s| s.unsigned_abs() < 64),
                "{chime:?} {tail:?}"
            );
            let jump = samples
                .windows(2)
                .map(|pair| (i32::from(pair[1]) - i32::from(pair[0])).unsigned_abs())
                .max()
                .unwrap();
            // A 1 kHz sine at the peak changes by at most 2*pi*f/rate of
            // full scale per sample; anything larger would be a click.
            assert!(jump < 3_000, "{chime:?} jump {jump}");
        }
    }

    #[test]
    fn success_rises_and_failure_falls_lower() {
        let saved = samples(Chime::Saved);
        let failed = samples(Chime::Failed);
        let rate = usize::try_from(SAMPLE_RATE).unwrap();
        let lead_in = samples_in(LEAD_IN_MS);
        let first = |s: &[i16]| crossings_per_second(&s[lead_in..lead_in + rate * 60 / 1000]);
        let last = |s: &[i16]| crossings_per_second(&s[s.len() - rate * 60 / 1000..]);
        let near = |measured: usize, hz: usize| measured.abs_diff(2 * hz) < 2 * hz / 10;
        assert!(near(first(&saved), 659), "{}", first(&saved));
        assert!(near(last(&saved), 880), "{}", last(&saved));
        assert!(near(first(&failed), 440), "{}", first(&failed));
        assert!(near(last(&failed), 330), "{}", last(&failed));
    }

    #[test]
    fn warm_up_is_a_short_silent_wav() {
        let wav = warm_up_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32_at(&wav, 4), u32::try_from(wav.len() - 8).unwrap());
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32_at(&wav, 40), 480, "10 ms at 24 kHz");
        assert!(wav[WAV_HEADER_LEN..].iter().all(|&b| b == 0));
    }

    /// 64-bit FNV-1a, a stable checksum for the golden test.
    fn fnv1a(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    /// Any change to the synthesis changes these; update them only after
    /// listening to the new sound.
    #[test]
    fn images_match_their_golden_checksums() {
        let checksums = [
            fnv1a(&wav(Chime::Saved)),
            fnv1a(&wav(Chime::Failed)),
            fnv1a(&warm_up_wav()),
        ];
        assert_eq!(
            checksums,
            [
                0x703d_4259_0e39_5b8e,
                0xb20a_2883_7dfc_460e,
                0x39af_38e3_a334_cbdd
            ],
            "{checksums:#018x?}"
        );
    }

    #[test]
    fn sine_matches_the_float_reference() {
        for step in 0..256_u32 {
            let phase = step << 24;
            let reference = (f64::from(phase) / f64::from(u32::MAX) * std::f64::consts::TAU).sin();
            let error =
                (f64::from(i32::try_from(sine(phase)).unwrap()) / 32_767.0 - reference).abs();
            assert!(error < 0.002, "phase {phase}: error {error}");
        }
    }
}
