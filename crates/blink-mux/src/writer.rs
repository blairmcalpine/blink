//! Faststart MP4 layout: `ftyp`, then `moov`, then `mdat`.

mod annexb;
mod avcc;
mod boxes;
mod moov;
mod track;

use std::io::{BufWriter, Seek, Write};

use blink_ring::ClipBuf;

use self::boxes::{Put, widen};
use self::moov::exceeds_u32;
use self::track::{Media, Movie};
use crate::MuxError;

/// Writes `clip` as a faststart MP4 to `out`, starting at its current position.
///
/// The file holds an `avc1` video track and, if the clip has audio packets, an
/// `mp4a` audio track. Video access units are converted from Annex B to
/// length-prefixed NAL units while writing, borrowing each NAL unit from the
/// clip, with SPS, PPS, and AUD NAL units removed; the parameter sets live in
/// `avcC` instead. `stss` marks keyframes, `pasp` is written when the format
/// has a sample aspect ratio, and `co64` replaces `stco` when offsets pass
/// 4 GiB.
///
/// Presentation starts at [`ClipBuf::start`] and runs to [`ClipBuf::end`]:
///
/// - Each video frame lasts until the next one starts, which keeps variable
///   frame rate timing. The last frame lasts until the clip end, so a static
///   screen before the save shows for its full length.
/// - Each AAC frame lasts its nominal 1024 samples. A real gap or overlap in
///   the audio timestamps is absorbed into the previous frame's duration and
///   logged.
/// - A track whose first sample is after the clip start gets an empty edit; a
///   track whose first sample is before it (the AAC preroll frame) gets an
///   edit `media_time` that skips the excess. Sub-frame audio offsets are kept.
///
/// Video uses a 90 kHz timescale, audio its sample rate, and the movie 90 kHz.
///
/// Output is buffered internally; `out` does not need to be a `BufWriter`.
///
/// # Errors
///
/// Returns [`MuxError`] if the clip has no video format or packets, has audio
/// without an audio format, has malformed parameter sets or access units,
/// holds values the MP4 fields cannot represent, or if writing fails.
pub fn write_mp4<W: Write + Seek>(out: W, clip: &ClipBuf) -> Result<(), MuxError> {
    let movie = Movie::plan(clip)?;
    let mut out = BufWriter::new(out);
    let start = out.stream_position()?;
    let ftyp = moov::ftyp()?;
    let data_len = movie.data_len();
    let mdat_header = mdat_header(data_len)?;
    let prefix = start
        .saturating_add(widen(ftyp.len()))
        .saturating_add(widen(mdat_header.len()));
    let moov = layout_moov(&movie, prefix, data_len)?;

    out.write_all(&ftyp)?;
    out.write_all(&moov)?;
    out.write_all(&mdat_header)?;
    for (_, track, chunk) in movie.ordered_chunks() {
        for sample in track.chunk_samples(chunk) {
            match track.media {
                Media::Video(_) => annexb::write_length_prefixed(&mut out, sample.data)?,
                Media::Audio(_) => out.write_all(sample.data)?,
            }
        }
    }
    out.flush()?;
    Ok(())
}

/// Builds the `moov` with final chunk offsets.
///
/// The `moov` size depends only on whether offsets are 32 or 64 bits, not on
/// their values, so it is built once with placeholder offsets to learn its
/// size, then again with the real ones. `prefix` is everything before the
/// sample data except the `moov` itself.
fn layout_moov(movie: &Movie<'_>, prefix: u64, data_len: u64) -> Result<Vec<u8>, MuxError> {
    let placeholder = movie.chunk_offsets(0);
    let narrow_len = widen(moov::moov(movie, &placeholder, false)?.len());
    let data_start = prefix.saturating_add(narrow_len);
    if !exceeds_u32(data_start.saturating_add(data_len)) {
        return moov::moov(movie, &movie.chunk_offsets(data_start), false);
    }
    let wide_len = widen(moov::moov(movie, &placeholder, true)?.len());
    moov::moov(
        movie,
        &movie.chunk_offsets(prefix.saturating_add(wide_len)),
        true,
    )
}

/// The `mdat` header for `data_len` bytes of samples, 64-bit if needed.
fn mdat_header(data_len: u64) -> Result<Vec<u8>, MuxError> {
    let mut header = Vec::with_capacity(16);
    if let Some(size) = data_len
        .checked_add(8)
        .and_then(|size| u32::try_from(size).ok())
    {
        header.u32(size);
        header.put(b"mdat");
    } else {
        let size = data_len
            .checked_add(16)
            .ok_or(MuxError::ValueOutOfRange { field: "mdat size" })?;
        header.u32(1);
        header.put(b"mdat");
        header.u64(size);
    }
    Ok(header)
}

#[cfg(test)]
mod tests {
    use blink_ring::media::{PacketMeta, StreamKind, Timestamp, VideoFormat};

    use super::*;

    #[test]
    fn mdat_header_switches_to_largesize() {
        assert_eq!(
            mdat_header(4).expect("fits"),
            [0, 0, 0, 12, b'm', b'd', b'a', b't']
        );
        let large = mdat_header(u64::from(u32::MAX)).expect("fits");
        assert_eq!(large.len(), 16);
        assert_eq!(
            large.get(..8),
            Some(&[0, 0, 0, 1, b'm', b'd', b'a', b't'][..])
        );
        assert_eq!(
            large.get(8..),
            Some(&(u64::from(u32::MAX) + 16).to_be_bytes()[..])
        );
        assert!(mdat_header(u64::MAX).is_err());
    }

    fn format() -> VideoFormat {
        VideoFormat {
            width: 64,
            height: 64,
            fps: 30,
            sps: vec![0x67, 0x42, 0xc0, 0x0a],
            pps: vec![0x68, 0xce, 0x3c, 0x80],
            sample_aspect: None,
        }
    }

    fn meta(pts: i64, keyframe: bool) -> PacketMeta {
        PacketMeta {
            kind: StreamKind::Video,
            pts: Timestamp::from_ticks(pts),
            duration: Timestamp::from_ticks(333_333),
            keyframe,
        }
    }

    fn clip_with(format: &VideoFormat, extra: &[(PacketMeta, &[u8])]) -> ClipBuf {
        let mut clip = ClipBuf::new();
        clip.begin(
            format,
            None,
            Timestamp::from_ticks(0),
            Timestamp::from_ticks(666_666),
        );
        clip.push_video(meta(0, true), &[0, 0, 0, 1, 0x65, 0x88]);
        clip.push_video(meta(333_333, false), &[0, 0, 1, 0x41, 0x9a]);
        for (meta, payload) in extra {
            if meta.kind == StreamKind::Video {
                clip.push_video(*meta, payload);
            } else {
                clip.push_audio(*meta, payload);
            }
        }
        clip
    }

    fn tiny_clip() -> ClipBuf {
        clip_with(&format(), &[])
    }

    fn write(clip: &ClipBuf) -> Result<(), MuxError> {
        write_mp4(std::io::Cursor::new(Vec::new()), clip)
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn wide_layout_uses_co64() {
        let clip = tiny_clip();
        let movie = Movie::plan(&clip).expect("valid");
        let offsets = movie.chunk_offsets(u64::from(u32::MAX) + 1);
        let moov = moov::moov(&movie, &offsets, true).expect("builds");
        assert!(contains(&moov, b"co64"));
        assert!(!contains(&moov, b"stco"));
        assert!(contains(&moov, &(u64::from(u32::MAX) + 1).to_be_bytes()));
        assert!(moov::moov(&movie, &offsets, false).is_err());
    }

    #[test]
    fn offsets_account_for_writer_position() {
        let clip = tiny_clip();
        let mut out = std::io::Cursor::new(vec![0xee; 100]);
        out.set_position(100);
        write_mp4(&mut out, &clip).expect("writes");
        let bytes = out.into_inner();
        assert_eq!(bytes.get(104..108), Some(&b"ftyp"[..]));
        // The single chunk starts right after the mdat header at the end.
        let data_start = u32::try_from(bytes.len() - 12).expect("small");
        assert!(contains(
            &bytes,
            &[
                &b"stco"[..],
                &[0, 0, 0, 0, 0, 0, 0, 1],
                &data_start.to_be_bytes()
            ]
            .concat()
        ));
    }

    #[test]
    fn rejects_unusable_clips() {
        assert!(matches!(
            write(&ClipBuf::new()),
            Err(MuxError::MissingVideoFormat)
        ));
        let mut empty = ClipBuf::new();
        empty.begin(
            &format(),
            None,
            Timestamp::from_ticks(0),
            Timestamp::from_ticks(1),
        );
        assert!(matches!(write(&empty), Err(MuxError::NoVideo)));
        let delimiter_only: &[u8] = &[0, 0, 1, 0x09, 0xf0];
        let clip = clip_with(&format(), &[(meta(666_666, false), delimiter_only)]);
        assert!(matches!(
            write(&clip),
            Err(MuxError::InvalidAccessUnit { index: 2 })
        ));
        let audio = PacketMeta {
            kind: StreamKind::Audio,
            ..meta(0, true)
        };
        let clip = clip_with(&format(), &[(audio, &[1])]);
        assert!(matches!(write(&clip), Err(MuxError::MissingAudioFormat)));
        let mut wide = format();
        wide.width = 70_000;
        assert!(matches!(
            write(&clip_with(&wide, &[])),
            Err(MuxError::ValueOutOfRange { .. })
        ));
    }
}
