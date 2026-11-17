//! The `ftyp` and `moov` boxes.

use blink_ring::media::{AudioFormat, VideoFormat};

use super::avcc;
use super::boxes::{BoxResult, Put, full, narrow, plain};
use super::track::{MOVIE_TIMESCALE, Media, Movie, Track, sample_count};
use crate::MuxError;

/// Unity transformation matrix in 16.16 and 2.30 fixed point.
const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

/// ISO 639-2 "und", packed as three 5-bit letters.
const LANGUAGE_UND: u16 = 0x55c4;

/// Builds the `ftyp` box.
pub(super) fn ftyp() -> Result<Vec<u8>, MuxError> {
    let mut out = Vec::new();
    plain(&mut out, *b"ftyp", |ftyp| {
        ftyp.put(b"isom");
        ftyp.u32(0x200);
        for brand in [b"isom", b"iso2", b"avc1", b"mp41"] {
            ftyp.put(brand);
        }
        Ok(())
    })?;
    Ok(out)
}

/// Builds the `moov` box. `offsets` holds each track's absolute chunk
/// offsets; `wide_offsets` selects `co64` over `stco`.
pub(super) fn moov(
    movie: &Movie<'_>,
    offsets: &[Vec<u64>],
    wide_offsets: bool,
) -> Result<Vec<u8>, MuxError> {
    let mut out = Vec::new();
    plain(&mut out, *b"moov", |moov| {
        mvhd(moov, movie)?;
        for (track, offsets) in movie.tracks.iter().zip(offsets) {
            trak(moov, track, offsets, wide_offsets)?;
        }
        Ok(())
    })?;
    Ok(out)
}

fn mvhd(out: &mut Vec<u8>, movie: &Movie<'_>) -> BoxResult {
    let wide = exceeds_u32(movie.duration);
    full(out, *b"mvhd", u8::from(wide), 0, |mvhd| {
        mvhd.versioned(wide, 0)?; // creation_time
        mvhd.versioned(wide, 0)?; // modification_time
        mvhd.u32(MOVIE_TIMESCALE);
        mvhd.versioned(wide, movie.duration)?;
        mvhd.u32(0x0001_0000); // rate 1.0
        mvhd.u16(0x0100); // volume 1.0
        mvhd.zeros(10);
        put_matrix(mvhd);
        mvhd.zeros(24); // pre_defined
        mvhd.u32(narrow(movie.tracks.len() + 1, "next track id")?);
        Ok(())
    })
}

fn trak(out: &mut Vec<u8>, track: &Track<'_>, offsets: &[u64], wide_offsets: bool) -> BoxResult {
    plain(out, *b"trak", |trak| {
        tkhd(trak, track)?;
        edts(trak, track)?;
        plain(trak, *b"mdia", |mdia| {
            mdhd(mdia, track)?;
            hdlr(mdia, track.media)?;
            plain(mdia, *b"minf", |minf| {
                media_header(minf, track.media)?;
                dinf(minf)?;
                stbl(minf, track, offsets, wide_offsets)
            })
        })
    })
}

fn tkhd(out: &mut Vec<u8>, track: &Track<'_>) -> BoxResult {
    let duration = track.start_delay.saturating_add(track.edit_duration);
    let wide = exceeds_u32(duration);
    let (volume, width, height) = match track.media {
        Media::Video(format) => (0, display_width(format)?, fixed_16_16(format.height)?),
        Media::Audio(_) => (0x0100, 0, 0),
    };
    // Flags: track enabled and in movie.
    full(out, *b"tkhd", u8::from(wide), 0x3, |tkhd| {
        tkhd.versioned(wide, 0)?; // creation_time
        tkhd.versioned(wide, 0)?; // modification_time
        tkhd.u32(track.id);
        tkhd.zeros(4);
        tkhd.versioned(wide, duration)?;
        tkhd.zeros(8);
        tkhd.u16(0); // layer
        tkhd.u16(0); // alternate_group
        tkhd.u16(volume);
        tkhd.zeros(2);
        put_matrix(tkhd);
        tkhd.u32(width);
        tkhd.u32(height);
        Ok(())
    })
}

/// Edit list: an empty edit for a track that starts after the clip start,
/// then the media from `media_time`, which skips samples before the clip
/// start (the AAC preroll frame).
fn edts(out: &mut Vec<u8>, track: &Track<'_>) -> BoxResult {
    let media_time = i64::try_from(track.media_time).map_err(|_| MuxError::ValueOutOfRange {
        field: "edit media time",
    })?;
    let mut entries = Vec::with_capacity(2);
    if track.start_delay > 0 {
        entries.push((track.start_delay, -1_i64));
    }
    entries.push((track.edit_duration, media_time));
    let wide = entries
        .iter()
        .any(|&(duration, time)| exceeds_u32(duration) || i32::try_from(time).is_err());
    plain(out, *b"edts", |edts| {
        full(edts, *b"elst", u8::from(wide), 0, |elst| {
            elst.u32(narrow(entries.len(), "edit count")?);
            for &(duration, media_time) in &entries {
                elst.versioned(wide, duration)?;
                if wide {
                    elst.put(&media_time.to_be_bytes());
                } else {
                    let media_time =
                        i32::try_from(media_time).map_err(|_| MuxError::ValueOutOfRange {
                            field: "edit media time",
                        })?;
                    elst.put(&media_time.to_be_bytes());
                }
                elst.u32(0x0001_0000); // media rate 1.0
            }
            Ok(())
        })
    })
}

fn mdhd(out: &mut Vec<u8>, track: &Track<'_>) -> BoxResult {
    let wide = exceeds_u32(track.media_duration);
    full(out, *b"mdhd", u8::from(wide), 0, |mdhd| {
        mdhd.versioned(wide, 0)?; // creation_time
        mdhd.versioned(wide, 0)?; // modification_time
        mdhd.u32(track.timescale);
        mdhd.versioned(wide, track.media_duration)?;
        mdhd.u16(LANGUAGE_UND);
        mdhd.u16(0);
        Ok(())
    })
}

fn hdlr(out: &mut Vec<u8>, media: Media<'_>) -> BoxResult {
    let (handler, name): (&[u8; 4], &[u8]) = match media {
        Media::Video(_) => (b"vide", b"VideoHandler\0"),
        Media::Audio(_) => (b"soun", b"SoundHandler\0"),
    };
    full(out, *b"hdlr", 0, 0, |hdlr| {
        hdlr.u32(0); // pre_defined
        hdlr.put(handler);
        hdlr.zeros(12);
        hdlr.put(name);
        Ok(())
    })
}

fn media_header(out: &mut Vec<u8>, media: Media<'_>) -> BoxResult {
    match media {
        // graphicsmode copy, opcolor zero.
        Media::Video(_) => full(out, *b"vmhd", 0, 1, |vmhd| {
            vmhd.zeros(8);
            Ok(())
        }),
        // Balance centered.
        Media::Audio(_) => full(out, *b"smhd", 0, 0, |smhd| {
            smhd.zeros(4);
            Ok(())
        }),
    }
}

/// Data reference: samples are in this file.
fn dinf(out: &mut Vec<u8>) -> BoxResult {
    plain(out, *b"dinf", |dinf| {
        full(dinf, *b"dref", 0, 0, |dref| {
            dref.u32(1);
            full(dref, *b"url ", 0, 1, |_| Ok(()))
        })
    })
}

fn stbl(out: &mut Vec<u8>, track: &Track<'_>, offsets: &[u64], wide_offsets: bool) -> BoxResult {
    plain(out, *b"stbl", |stbl| {
        stsd(stbl, track)?;
        stts(stbl, track)?;
        if matches!(track.media, Media::Video(_)) {
            stss(stbl, track)?;
        }
        stsc(stbl, track)?;
        stsz(stbl, track)?;
        chunk_offsets(stbl, offsets, wide_offsets)
    })
}

fn stsd(out: &mut Vec<u8>, track: &Track<'_>) -> BoxResult {
    full(out, *b"stsd", 0, 0, |stsd| {
        stsd.u32(1);
        match track.media {
            Media::Video(format) => avc1(stsd, format),
            Media::Audio(format) => mp4a(stsd, track, format),
        }
    })
}

fn avc1(out: &mut Vec<u8>, format: &VideoFormat) -> BoxResult {
    let config = avcc::decoder_config(&format.sps, &format.pps)?;
    plain(out, *b"avc1", |avc1| {
        avc1.zeros(6);
        avc1.u16(1); // data_reference_index
        avc1.zeros(16); // pre_defined and reserved
        avc1.u16(dimension(format.width)?);
        avc1.u16(dimension(format.height)?);
        avc1.u32(0x0048_0000); // 72 dpi horizontal
        avc1.u32(0x0048_0000); // 72 dpi vertical
        avc1.u32(0);
        avc1.u16(1); // frame_count
        avc1.zeros(32); // compressorname
        avc1.u16(0x0018); // depth
        avc1.u16(0xffff); // pre_defined -1
        plain(avc1, *b"avcC", |avcc| {
            avcc.put(&config);
            Ok(())
        })?;
        if let Some((horizontal, vertical)) = format.sample_aspect {
            plain(avc1, *b"pasp", |pasp| {
                pasp.u32(horizontal);
                pasp.u32(vertical);
                Ok(())
            })?;
        }
        Ok(())
    })
}

fn mp4a(out: &mut Vec<u8>, track: &Track<'_>, format: &AudioFormat) -> BoxResult {
    plain(out, *b"mp4a", |mp4a| {
        mp4a.zeros(6);
        mp4a.u16(1); // data_reference_index
        mp4a.zeros(8);
        mp4a.u16(format.channels);
        mp4a.u16(16); // sample size
        mp4a.zeros(4);
        // 16.16 sample rate; rates above 65535 Hz write 0 and rely on the
        // AudioSpecificConfig, as the field cannot hold them.
        mp4a.u32(u16::try_from(format.sample_rate).map_or(0, |rate| u32::from(rate) << 16));
        esds(mp4a, track, format)
    })
}

/// Elementary stream descriptor carrying the AAC `AudioSpecificConfig`.
fn esds(out: &mut Vec<u8>, track: &Track<'_>, format: &AudioFormat) -> BoxResult {
    let max_sample = track
        .samples
        .iter()
        .map(|sample| sample.size)
        .max()
        .unwrap_or(0);
    let bitrate = average_bitrate(track);
    let mut decoder_specific = Vec::new();
    descriptor(&mut decoder_specific, 0x05, &format.audio_specific_config)?;
    let mut decoder_config = vec![0x40, 0x15]; // MPEG-4 audio, audio stream
    let [_, b1, b2, b3] = max_sample.min(0x00ff_ffff).to_be_bytes();
    decoder_config.put(&[b1, b2, b3]); // bufferSizeDB
    decoder_config.u32(bitrate); // maxBitrate
    decoder_config.u32(bitrate); // avgBitrate
    decoder_config.put(&decoder_specific);
    let mut es = Vec::new();
    es.u16(u16::try_from(track.id).unwrap_or(0));
    es.push(0); // no stream dependence, URL, or OCR
    descriptor(&mut es, 0x04, &decoder_config)?;
    descriptor(&mut es, 0x06, &[0x02])?; // SLConfig: predefined MP4
    full(out, *b"esds", 0, 0, |esds| descriptor(esds, 0x03, &es))
}

/// Appends an MPEG-4 descriptor with a 4-byte expandable length.
fn descriptor(out: &mut Vec<u8>, tag: u8, body: &[u8]) -> BoxResult {
    let len = narrow(body.len(), "descriptor length")?;
    if len >= 1 << 28 {
        return Err(MuxError::ValueOutOfRange {
            field: "descriptor length",
        });
    }
    out.push(tag);
    out.put(&[
        0x80 | low7(len >> 21),
        0x80 | low7(len >> 14),
        0x80 | low7(len >> 7),
        low7(len),
    ]);
    out.put(body);
    Ok(())
}

const fn low7(value: u32) -> u8 {
    let [low, ..] = value.to_le_bytes();
    low & 0x7f
}

fn average_bitrate(track: &Track<'_>) -> u32 {
    let bytes: u64 = track
        .samples
        .iter()
        .map(|sample| u64::from(sample.size))
        .sum();
    let bits_per_second = u128::from(bytes) * 8 * u128::from(track.timescale)
        / u128::from(track.media_duration.max(1));
    u32::try_from(bits_per_second).unwrap_or(u32::MAX)
}

/// Decoding time to sample: run-length encoded durations.
fn stts(out: &mut Vec<u8>, track: &Track<'_>) -> BoxResult {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for sample in &track.samples {
        match runs.last_mut() {
            Some((count, duration)) if *duration == sample.duration => *count += 1,
            _ => runs.push((1, sample.duration)),
        }
    }
    full(out, *b"stts", 0, 0, |stts| {
        stts.u32(narrow(runs.len(), "stts entry count")?);
        for (count, duration) in runs {
            stts.u32(count);
            stts.u32(duration);
        }
        Ok(())
    })
}

/// Sync samples: 1-based numbers of the keyframes.
fn stss(out: &mut Vec<u8>, track: &Track<'_>) -> BoxResult {
    let sync: Vec<u32> = track
        .samples
        .iter()
        .enumerate()
        .filter(|(_, sample)| sample.keyframe)
        .map(|(index, _)| narrow(index + 1, "sample number"))
        .collect::<Result<_, _>>()?;
    full(out, *b"stss", 0, 0, |stss| {
        stss.u32(narrow(sync.len(), "stss entry count")?);
        for number in sync {
            stss.u32(number);
        }
        Ok(())
    })
}

/// Sample to chunk: runs of chunks with the same sample count.
fn stsc(out: &mut Vec<u8>, track: &Track<'_>) -> BoxResult {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (index, chunk) in track.chunks.iter().enumerate() {
        if runs.last().is_none_or(|&(_, count)| count != chunk.count) {
            runs.push((index + 1, chunk.count));
        }
    }
    full(out, *b"stsc", 0, 0, |stsc| {
        stsc.u32(narrow(runs.len(), "stsc entry count")?);
        for (first_chunk, count) in runs {
            stsc.u32(narrow(first_chunk, "chunk number")?);
            stsc.u32(narrow(count, "samples per chunk")?);
            stsc.u32(1); // sample_description_index
        }
        Ok(())
    })
}

fn stsz(out: &mut Vec<u8>, track: &Track<'_>) -> BoxResult {
    full(out, *b"stsz", 0, 0, |stsz| {
        stsz.u32(0); // sizes vary
        stsz.u32(sample_count(track)?);
        for sample in &track.samples {
            stsz.u32(sample.size);
        }
        Ok(())
    })
}

fn chunk_offsets(out: &mut Vec<u8>, offsets: &[u64], wide: bool) -> BoxResult {
    let kind = if wide { *b"co64" } else { *b"stco" };
    full(out, kind, 0, 0, |table| {
        table.u32(narrow(offsets.len(), "chunk count")?);
        for &offset in offsets {
            if wide {
                table.u64(offset);
            } else {
                table.u32(narrow(offset, "chunk offset")?);
            }
        }
        Ok(())
    })
}

fn put_matrix(out: &mut Vec<u8>) {
    for value in MATRIX {
        out.u32(value);
    }
}

pub(super) const fn exceeds_u32(value: u64) -> bool {
    value > 0xffff_ffff
}

fn dimension(value: u32) -> Result<u16, MuxError> {
    u16::try_from(value).map_err(|_| MuxError::ValueOutOfRange {
        field: "video dimension",
    })
}

/// A pixel dimension as 16.16 fixed point.
fn fixed_16_16(value: u32) -> Result<u32, MuxError> {
    Ok(u32::from(dimension(value)?) << 16)
}

/// Track header width: the coded width scaled by the sample aspect ratio, so
/// players that read `tkhd` rather than `pasp` also show the stretched shape.
fn display_width(format: &VideoFormat) -> Result<u32, MuxError> {
    let Some((horizontal, vertical)) = format.sample_aspect.filter(|&(_, v)| v > 0) else {
        return fixed_16_16(format.width);
    };
    let scaled = (u64::from(format.width) * u64::from(horizontal) + u64::from(vertical) / 2)
        / u64::from(vertical);
    fixed_16_16(narrow(scaled, "display width")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_uses_expandable_length() {
        let mut out = Vec::new();
        descriptor(&mut out, 0x05, &[0x11, 0x90]).expect("small");
        assert_eq!(out, [0x05, 0x80, 0x80, 0x80, 0x02, 0x11, 0x90]);
        let mut out = Vec::new();
        descriptor(&mut out, 0x03, &[0; 300]).expect("small");
        assert_eq!(out.get(..5), Some(&[0x03, 0x80, 0x80, 0x82, 0x2c][..]));
    }

    #[test]
    fn display_width_applies_sample_aspect() {
        let mut format = VideoFormat {
            width: 1280,
            height: 960,
            fps: 60,
            sps: Vec::new(),
            pps: Vec::new(),
            sample_aspect: Some((4, 3)),
        };
        assert_eq!(display_width(&format).expect("fits"), 1707 << 16);
        format.sample_aspect = Some((1, 0));
        assert_eq!(display_width(&format).expect("fits"), 1280 << 16);
        format.width = 70_000;
        assert!(display_width(&format).is_err());
    }

    #[test]
    fn ftyp_lists_brands() {
        let ftyp = ftyp().expect("small");
        assert_eq!(ftyp.len(), 32);
        assert_eq!(ftyp.get(4..12), Some(&b"ftypisom"[..]));
    }
}
