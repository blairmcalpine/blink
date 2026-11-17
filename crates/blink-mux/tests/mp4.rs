//! Muxes the committed H.264 and AAC fixtures and checks the MP4 structure.
//!
//! The structural tests need no external tools. `ffprobe_accepts_output` is
//! ignored by default; run it with `cargo test -p blink-mux -- --ignored` when
//! `FFmpeg` is installed.

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::process::Command;

    use blink_mux::write_mp4;
    use blink_ring::ClipBuf;
    use blink_ring::media::{AudioFormat, PacketMeta, StreamKind, Timestamp, VideoFormat};

    const AUDIO_OFFSET_TICKS: i64 = 1_000_000; // 100 ms
    const SECOND: i64 = Timestamp::TICKS_PER_SECOND;

    /// An owned, editable clip; [`Fixture::clip`] copies it into a `ClipBuf`.
    struct Fixture {
        video_format: VideoFormat,
        audio_format: Option<AudioFormat>,
        video: Vec<(PacketMeta, Vec<u8>)>,
        audio: Vec<(PacketMeta, Vec<u8>)>,
        start: Timestamp,
        end: Timestamp,
    }

    impl Fixture {
        fn clip(&self) -> ClipBuf {
            let mut clip = ClipBuf::new();
            clip.begin(
                &self.video_format,
                self.audio_format.as_ref(),
                self.start,
                self.end,
            );
            for (meta, data) in &self.video {
                clip.push_video(*meta, data);
            }
            for (meta, data) in &self.audio {
                clip.push_audio(*meta, data);
            }
            clip
        }

        fn shift_video_from(&mut self, index: usize, ticks: i64) {
            for (meta, _) in &mut self.video[index..] {
                meta.pts = Timestamp(meta.pts.0 + ticks);
            }
        }
    }

    fn mux(fixture: &Fixture) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        write_mp4(&mut out, &fixture.clip()).expect("mux succeeds");
        out.into_inner()
    }

    fn baseline_with_audio() -> Fixture {
        load_clip(
            "video_baseline.h264",
            Some(("audio_48k_stereo.aac", AUDIO_OFFSET_TICKS)),
        )
    }

    fn tracks(root: &[Mp4Box]) -> Vec<&Mp4Box> {
        find(root, "moov").expect("moov").children_of("trak")
    }

    #[test]
    fn layout_is_faststart() {
        let file = mux(&baseline_with_audio());
        let root = parse(&file, 0);
        let kinds: Vec<&str> = root.iter().map(|b| b.kind.as_str()).collect();
        assert_eq!(kinds, ["ftyp", "moov", "mdat"]);
        let ftyp = &root[0];
        assert_eq!(&ftyp.body[..4], b"isom");
        assert_eq!(&ftyp.body[8..], b"isomiso2avc1mp41");
        let mdat = &root[2];
        assert_eq!(mdat.start + mdat.size, file.len());
    }

    #[test]
    fn writes_from_a_ring_snapshot() {
        let fixture = baseline_with_audio();
        let mut ring = blink_ring::ClipRing::with_capacity(Timestamp(10 * SECOND), 1 << 20);
        ring.set_video_format(fixture.video_format.clone());
        ring.set_audio_format(fixture.audio_format.clone().expect("audio"));
        let mut packets: Vec<&(PacketMeta, Vec<u8>)> =
            fixture.video.iter().chain(&fixture.audio).collect();
        packets.sort_by_key(|(meta, _)| meta.pts);
        for (meta, data) in packets {
            ring.push(*meta, data).expect("push");
        }
        let mut clip = ClipBuf::new();
        ring.snapshot(Timestamp(2 * SECOND), Timestamp(2 * SECOND), &mut clip)
            .expect("snapshot");
        assert_eq!(clip.video_len(), 60);
        let mut out = Cursor::new(Vec::new());
        write_mp4(&mut out, &clip).expect("mux succeeds");
        let file = out.into_inner();
        let root = parse(&file, 0);
        let video_elst = &tracks(&root)[0].get("edts/elst").body;
        assert_eq!(
            u32_at(video_elst, 8),
            180_000,
            "video ends at the save time"
        );
    }

    #[test]
    fn has_video_and_audio_tracks() {
        let clip = baseline_with_audio();
        let file = mux(&clip);
        let root = parse(&file, 0);
        let traks = tracks(&root);
        assert_eq!(traks.len(), 2);

        let video = traks[0];
        assert_eq!(&video.get("mdia/hdlr").body[8..12], b"vide");
        assert_eq!(u32_at(&video.get("mdia/mdhd").body, 12), 90_000);
        assert_eq!(u32_at(&video.get("tkhd").body, 12), 1);
        let stbl = video.get("mdia/minf/stbl");
        assert_eq!(u32_at(&stbl.get("stsz").body, 8), 60);
        let stss = &stbl.get("stss").body;
        assert_eq!(u32_at(stss, 4), 2);
        assert_eq!([u32_at(stss, 8), u32_at(stss, 12)], [1, 31]);
        assert!(stbl.find("co64").is_none());
        assert!(stbl.find("stco").is_some());

        let avc1 = stbl.get("stsd/avc1");
        assert_eq!(u16::from_be_bytes([avc1.body[24], avc1.body[25]]), 64);
        let config = &avc1.get("avcC").body;
        let sps = &clip.video_format.sps;
        assert_eq!(
            stts(&video.get("mdia/minf/stbl/stts").body),
            [(60, 3000)],
            "constant frame rate is one stts entry"
        );
        assert_eq!(&config[..4], &[1, sps[1], sps[2], sps[3]]);
        assert_eq!(&config[8..8 + sps.len()], sps.as_slice());
        assert!(avc1.find("pasp").is_none());

        let audio = traks[1];
        assert_eq!(&audio.get("mdia/hdlr").body[8..12], b"soun");
        assert_eq!(u32_at(&audio.get("mdia/mdhd").body, 12), 48_000);
        let stbl = audio.get("mdia/minf/stbl");
        assert_eq!(u32_at(&stbl.get("stsz").body, 8), 95);
        assert_eq!(stts(&stbl.get("stts").body), [(95, 1024)]);
        assert!(stbl.find("stss").is_none());
        let esds = &stbl.get("stsd/mp4a/esds").body;
        let asc = clip
            .audio_format
            .as_ref()
            .expect("audio")
            .audio_specific_config
            .clone();
        assert!(
            esds.windows(asc.len() + 1)
                .any(|w| usize::from(w[0]) == asc.len() && w[1..] == asc[..])
        );
    }

    #[test]
    fn video_samples_are_length_prefixed_without_parameter_sets() {
        let clip = baseline_with_audio();
        let file = mux(&clip);
        let root = parse(&file, 0);
        let samples = samples(&file, tracks(&root)[0]);
        assert_eq!(samples.len(), clip.video.len());
        for (index, sample) in samples.iter().enumerate() {
            let types = length_prefixed_types(sample);
            assert_ne!(types, []);
            assert!(
                types.iter().all(|t| !matches!(t, 7..=9)),
                "sample {index}: {types:?}"
            );
            let is_idr = types.contains(&5);
            assert_eq!(is_idr, clip.video[index].0.keyframe, "sample {index}");
        }
    }

    #[test]
    fn audio_samples_match_input_frames() {
        let clip = baseline_with_audio();
        let file = mux(&clip);
        let root = parse(&file, 0);
        let samples = samples(&file, tracks(&root)[1]);
        let input: Vec<&[u8]> = clip.audio.iter().map(|(_, data)| data.as_slice()).collect();
        assert_eq!(samples, input);
    }

    #[test]
    fn chunks_interleave_by_time() {
        let file = mux(&baseline_with_audio());
        let root = parse(&file, 0);
        let first_offsets: Vec<u32> = tracks(&root)
            .iter()
            .map(|trak| u32_at(&trak.get("mdia/minf/stbl/stco").body, 8))
            .collect();
        let mdat = &root[2];
        assert_eq!(usz(first_offsets[0]), mdat.start + 8);
        let video_chunks = u32_at(&tracks(&root)[0].get("mdia/minf/stbl/stco").body, 4);
        assert_eq!(video_chunks, 4, "2 s of video in 0.5 s chunks");
        // Audio starts 100 ms in, so its first chunk follows the first video chunk.
        let last_video_offset = {
            let stco = &tracks(&root)[0].get("mdia/minf/stbl/stco").body;
            u32_at(stco, 8 + 4 * (usz(video_chunks) - 1))
        };
        assert!(first_offsets[1] > first_offsets[0]);
        assert!(first_offsets[1] < last_video_offset);
    }

    #[test]
    fn edit_list_preserves_audio_offset() {
        let file = mux(&baseline_with_audio());
        let root = parse(&file, 0);
        let traks = tracks(&root);
        let video_elst = &traks[0].get("edts/elst").body;
        assert_eq!(u32_at(video_elst, 4), 1);
        assert_eq!(u32_at(video_elst, 12), 0, "video media starts at 0");
        assert_eq!(
            u32_at(video_elst, 8),
            180_000,
            "60 frames at 30 fps in 90 kHz"
        );

        let audio_elst = &traks[1].get("edts/elst").body;
        assert_eq!(u32_at(audio_elst, 4), 2);
        assert_eq!(u32_at(audio_elst, 8), 9_000, "100 ms empty edit in 90 kHz");
        assert_eq!(u32_at(audio_elst, 12), u32::MAX, "media_time -1");
        assert_eq!(u32_at(audio_elst, 24), 0);

        let mvhd = &find(&root, "moov").expect("moov").get("mvhd").body;
        let movie_duration = u32_at(mvhd, 16);
        let audio_end = 9_000 + u32_at(audio_elst, 20);
        assert_eq!(movie_duration, audio_end.max(180_000));
    }

    #[test]
    fn edit_list_keeps_sub_frame_audio_offset() {
        let file = mux(&load_clip(
            "video_baseline.h264",
            Some(("audio_48k_stereo.aac", 70_000)),
        ));
        let root = parse(&file, 0);
        let audio_elst = &tracks(&root)[1].get("edts/elst").body;
        assert_eq!(u32_at(audio_elst, 4), 2);
        assert_eq!(u32_at(audio_elst, 8), 630, "7 ms in 90 kHz");
        assert_eq!(u32_at(audio_elst, 12), u32::MAX);
    }

    #[test]
    fn edit_list_trims_audio_preroll() {
        // The preroll frame starts 15 ms before the keyframe at the clip start.
        let file = mux(&load_clip(
            "video_baseline.h264",
            Some(("audio_48k_stereo.aac", -150_000)),
        ));
        let root = parse(&file, 0);
        let traks = tracks(&root);
        let video_elst = &traks[0].get("edts/elst").body;
        assert_eq!([u32_at(video_elst, 4), u32_at(video_elst, 12)], [1, 0]);
        let audio_elst = &traks[1].get("edts/elst").body;
        assert_eq!(u32_at(audio_elst, 4), 1, "no empty edit");
        assert_eq!(
            u32_at(audio_elst, 12),
            720,
            "media_time skips 15 ms at 48 kHz"
        );
        assert_eq!(
            u32_at(audio_elst, 8),
            (95 * 1024 - 720) * 15 / 8,
            "the presented audio excludes the skipped part"
        );
    }

    #[test]
    fn edit_list_delays_video_after_the_clip_start() {
        let mut fixture = load_clip("video_baseline.h264", None);
        fixture.start = Timestamp(-10_000);
        let file = mux(&fixture);
        let root = parse(&file, 0);
        let video_elst = &tracks(&root)[0].get("edts/elst").body;
        assert_eq!(u32_at(video_elst, 4), 2);
        assert_eq!(u32_at(video_elst, 8), 90, "1 ms empty edit");
        assert_eq!(u32_at(video_elst, 12), u32::MAX);
    }

    #[test]
    fn variable_frame_rate_and_static_tail_keep_wall_clock_timing() {
        let fixture = stalled_clip();
        let file = mux(&fixture);
        let root = parse(&file, 0);
        let video = tracks(&root)[0];
        assert_eq!(
            stts(&video.get("mdia/minf/stbl/stts").body),
            [(29, 3000), (1, 93_000), (29, 3000), (1, 93_000)],
            "a 1 s stall lengthens the frame before it; the last frame lasts until the save"
        );
        let elst = &video.get("edts/elst").body;
        assert_eq!(u32_at(elst, 8), 360_000, "4 s clip");
    }

    /// 60 frames at 30 fps with a 1 s stall before frame 30 and a static
    /// screen for 1 s after the last frame.
    fn stalled_clip() -> Fixture {
        let mut fixture = load_clip("video_baseline.h264", None);
        fixture.shift_video_from(30, SECOND);
        fixture.end = Timestamp(4 * SECOND);
        fixture
    }

    #[test]
    fn sample_aspect_writes_pasp_and_display_width() {
        let mut clip = load_clip("video_baseline.h264", None);
        clip.video_format.sample_aspect = Some((4, 3));
        let file = mux(&clip);
        let root = parse(&file, 0);
        let traks = tracks(&root);
        assert_eq!(traks.len(), 1, "no audio track without audio packets");
        let pasp = &traks[0].get("mdia/minf/stbl/stsd/avc1/pasp").body;
        assert_eq!([u32_at(pasp, 0), u32_at(pasp, 4)], [4, 3]);
        let tkhd = &traks[0].get("tkhd").body;
        assert_eq!(u32_at(tkhd, 76), 85 << 16, "64 * 4 / 3 rounded");
        assert_eq!(u32_at(tkhd, 80), 64 << 16);
    }

    #[test]
    fn high_profile_avcc_has_chroma_extension() {
        let clip = load_clip("video_high.h264", None);
        let file = mux(&clip);
        let root = parse(&file, 0);
        let avc1 = tracks(&root)[0].get("mdia/minf/stbl/stsd/avc1");
        let config = &avc1.get("avcC").body;
        let (sps, pps) = (&clip.video_format.sps, &clip.video_format.pps);
        assert_eq!(config[1], 100);
        assert_eq!(config.len(), 6 + 2 + sps.len() + 1 + 2 + pps.len() + 4);
        assert_eq!(&config[config.len() - 4..], &[0xfd, 0xf8, 0xf8, 0]);
        let stss = &tracks(&root)[0].get("mdia/minf/stbl/stss").body;
        assert_eq!(
            [u32_at(stss, 4), u32_at(stss, 8), u32_at(stss, 12)],
            [2, 1, 16]
        );
    }

    #[test]
    #[ignore = "needs ffprobe and ffmpeg on PATH"]
    fn ffprobe_accepts_output() {
        let dir = env!("CARGO_TARGET_TMPDIR");
        let mut stretched = load_clip("video_high.h264", None);
        stretched.video_format.sample_aspect = Some((4, 3));
        let preroll = load_clip(
            "video_baseline.h264",
            Some(("audio_48k_stereo.aac", -150_000)),
        );
        let cases = [
            ("baseline_aac.mp4", baseline_with_audio(), "2.000000"),
            ("baseline_preroll.mp4", preroll, "2.000000"),
            ("high.mp4", load_clip("video_high.h264", None), "1.000000"),
            ("high_stretched.mp4", stretched, "1.000000"),
            ("stalled.mp4", stalled_clip(), "4.000000"),
        ];
        for (name, fixture, video_duration) in cases {
            let path = format!("{dir}/{name}");
            std::fs::write(&path, mux(&fixture)).expect("write output");

            let video = ffprobe(
                &path,
                "v:0",
                "stream=codec_name,nb_frames,start_time,duration,sample_aspect_ratio",
            );
            let sar = fixture.video_format.sample_aspect.unwrap_or((1, 1));
            let sar = format!("sample_aspect_ratio={}:{}", sar.0, sar.1);
            assert!(video.contains(&sar), "{name}: {video}");
            assert!(video.contains("codec_name=h264"), "{name}: {video}");
            let frames = format!("nb_frames={}", fixture.video.len());
            assert!(video.contains(&frames), "{name}: {video}");
            assert!(video.contains("start_time=0.000000"), "{name}: {video}");
            let duration = format!("duration={video_duration}");
            assert!(video.contains(&duration), "{name}: {video}");

            let audio = ffprobe(&path, "a", "stream=codec_name,nb_frames,start_time");
            if fixture.audio.is_empty() {
                assert!(audio.is_empty(), "{name}: {audio}");
            } else {
                assert!(audio.contains("codec_name=aac"), "{name}: {audio}");
                assert!(audio.contains("nb_frames=95"), "{name}: {audio}");
                let offset = fixture.audio[0].0.pts.0.max(0);
                let start = format!("start_time={:.6}", secs(offset));
                assert!(audio.contains(&start), "{name}: {audio}");
            }

            decode(&path, name);
        }
    }

    #[test]
    #[ignore = "needs ffprobe on PATH"]
    fn ffprobe_packet_durations_follow_stall() {
        let path = format!("{}/stalled_packets.mp4", env!("CARGO_TARGET_TMPDIR"));
        std::fs::write(&path, mux(&stalled_clip())).expect("write output");
        let packets = ffprobe(&path, "v:0", "packet=pts_time,duration_time");
        let durations: Vec<&str> = packets
            .lines()
            .filter_map(|line| line.strip_prefix("duration_time="))
            .collect();
        assert_eq!(durations.len(), 60, "{packets}");
        assert_eq!(durations[0], "0.033333");
        assert_eq!(durations[29], "1.033333", "frame before the stall");
        assert_eq!(durations[30], "0.033333");
        assert_eq!(durations[59], "1.033333", "last frame lasts until the save");
    }

    fn secs(ticks: i64) -> f64 {
        let ticks = u32::try_from(ticks).expect("small");
        f64::from(ticks) / 1e7
    }

    fn ffprobe(path: &str, streams: &str, entries: &str) -> String {
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                streams,
                "-show_entries",
                entries,
            ])
            .args(["-of", "default=noprint_wrappers=1", path])
            .output()
            .expect("run ffprobe");
        let stderr = String::from_utf8_lossy(&probe.stderr);
        assert!(
            probe.status.success() && stderr.is_empty(),
            "{path}: {stderr}"
        );
        String::from_utf8_lossy(&probe.stdout).into_owned()
    }

    fn decode(path: &str, name: &str) {
        let decode = Command::new("ffmpeg")
            .args(["-v", "error", "-i", path, "-f", "null", "-"])
            .output()
            .expect("run ffmpeg");
        let stderr = String::from_utf8_lossy(&decode.stderr);
        assert!(
            decode.status.success() && stderr.is_empty(),
            "{name}: {stderr}"
        );
    }

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path).unwrap_or_else(|err| panic!("read {path}: {err}"))
    }

    /// Start-code positions as `(start_code_start, nal_start)`.
    fn start_codes(data: &[u8]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + 3 <= data.len() {
            if data[i..i + 3] == [0, 0, 1] {
                let start = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
                out.push((start, i + 3));
                i += 3;
            } else {
                i += 1;
            }
        }
        out
    }

    fn nals(data: &[u8]) -> Vec<&[u8]> {
        let codes = start_codes(data);
        codes
            .iter()
            .enumerate()
            .map(|(k, &(_, nal_start))| {
                let end = codes.get(k + 1).map_or(data.len(), |&(next, _)| next);
                &data[nal_start..end]
            })
            .collect()
    }

    /// Splits an Annex B stream into access units at each AUD.
    fn access_units(data: &[u8]) -> Vec<&[u8]> {
        let bounds: Vec<usize> = start_codes(data)
            .into_iter()
            .filter(|&(_, nal)| data[nal] & 0x1f == 9)
            .map(|(start, _)| start)
            .collect();
        bounds
            .iter()
            .enumerate()
            .map(|(k, &start)| &data[start..*bounds.get(k + 1).unwrap_or(&data.len())])
            .collect()
    }

    /// Splits ADTS into raw AAC frames and builds the `AudioSpecificConfig`.
    fn adts_frames(data: &[u8]) -> (Vec<&[u8]>, AudioFormat) {
        let mut frames = Vec::new();
        let mut format = None;
        let mut pos = 0;
        while pos + 7 <= data.len() {
            let h = &data[pos..pos + 7];
            assert_eq!((h[0], h[1] & 0xf6), (0xff, 0xf0), "ADTS sync at {pos}");
            let header_len = if h[1] & 1 == 1 { 7 } else { 9 };
            let object_type = (h[2] >> 6) + 1;
            let rate_index = (h[2] >> 2) & 0xf;
            let channels = ((h[2] & 1) << 2) | (h[3] >> 6);
            let frame_len =
                (usize::from(h[3] & 3) << 11) | (usize::from(h[4]) << 3) | usize::from(h[5] >> 5);
            frames.push(&data[pos + header_len..pos + frame_len]);
            format.get_or_insert_with(|| {
                let asc = (u16::from(object_type) << 11)
                    | (u16::from(rate_index) << 7)
                    | (u16::from(channels) << 3);
                let rates = [
                    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050,
                ];
                AudioFormat {
                    sample_rate: rates[usize::from(rate_index)],
                    channels: u16::from(channels),
                    audio_specific_config: asc.to_be_bytes().to_vec(),
                }
            });
            pos += frame_len;
        }
        (frames, format.expect("at least one frame"))
    }

    /// Builds a clip from fixture files. Video is 30 fps from PTS 0; audio, if
    /// given, starts `offset` ticks later. The clip runs from 0 to the end of
    /// the last video frame.
    fn load_clip(video: &str, audio: Option<(&str, i64)>) -> Fixture {
        let stream = fixture(video);
        let units = access_units(&stream);
        let first = nals(units[0]);
        let find = |nal_type: u8| -> Vec<u8> {
            let nal = first
                .iter()
                .find(|n| n[0] & 0x1f == nal_type)
                .expect("parameter set");
            let end = nal.iter().rposition(|&b| b != 0).expect("nonzero") + 1;
            nal[..end].to_vec()
        };
        let video_format = VideoFormat {
            width: 64,
            height: if video.contains("high") { 48 } else { 64 },
            fps: 30,
            sps: find(7),
            pps: find(8),
            sample_aspect: None,
        };
        let frame_ticks = |i: i64| i * 10_000_000 / 30;
        let end = Timestamp(frame_ticks(i64::try_from(units.len()).expect("small")));
        let video = units
            .iter()
            .zip(0_i64..)
            .map(|(unit, i)| {
                let meta = PacketMeta {
                    kind: StreamKind::Video,
                    pts: Timestamp(frame_ticks(i)),
                    duration: Timestamp(frame_ticks(i + 1) - frame_ticks(i)),
                    keyframe: nals(unit).iter().any(|n| n[0] & 0x1f == 5),
                };
                (meta, unit.to_vec())
            })
            .collect();
        let (audio_format, audio) = match audio {
            None => (None, Vec::new()),
            Some((name, offset)) => {
                let data = fixture(name);
                let (frames, format) = adts_frames(&data);
                let rate = i64::from(format.sample_rate);
                let packets = frames
                    .iter()
                    .zip(0_i64..)
                    .map(|(frame, i)| {
                        let meta = PacketMeta {
                            kind: StreamKind::Audio,
                            pts: Timestamp(offset + i * 1024 * 10_000_000 / rate),
                            duration: Timestamp(1024 * 10_000_000 / rate),
                            keyframe: false,
                        };
                        (meta, frame.to_vec())
                    })
                    .collect();
                (Some(format), packets)
            }
        };
        Fixture {
            video_format,
            audio_format,
            video,
            audio,
            start: Timestamp::ZERO,
            end,
        }
    }

    /// `(count, duration)` runs of an `stts` body.
    fn stts(body: &[u8]) -> Vec<(u32, u32)> {
        (0..usz(u32_at(body, 4)))
            .map(|i| (u32_at(body, 8 + 8 * i), u32_at(body, 12 + 8 * i)))
            .collect()
    }

    fn usz(value: u32) -> usize {
        usize::try_from(value).expect("fits")
    }

    fn u32_at(data: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(data[at..at + 4].try_into().expect("4 bytes"))
    }

    #[derive(Debug)]
    struct Mp4Box {
        kind: String,
        /// Absolute offset of the box header in the file.
        start: usize,
        size: usize,
        body: Vec<u8>,
        children: Vec<Self>,
    }

    impl Mp4Box {
        fn find(&self, path: &str) -> Option<&Self> {
            find(&self.children, path)
        }

        fn get(&self, path: &str) -> &Self {
            self.find(path)
                .unwrap_or_else(|| panic!("{} has no {path}", self.kind))
        }

        fn children_of(&self, kind: &str) -> Vec<&Self> {
            self.children.iter().filter(|b| b.kind == kind).collect()
        }
    }

    fn find<'a>(boxes: &'a [Mp4Box], path: &str) -> Option<&'a Mp4Box> {
        let (head, rest) = path
            .split_once('/')
            .map_or((path, None), |(h, r)| (h, Some(r)));
        let found = boxes.iter().find(|b| b.kind == head)?;
        rest.map_or(Some(found), |rest| found.find(rest))
    }

    /// Bytes to skip in a box body before its child boxes, if it has any.
    fn child_offset(kind: &str) -> Option<usize> {
        match kind {
            "moov" | "trak" | "edts" | "mdia" | "minf" | "dinf" | "stbl" => Some(0),
            "stsd" | "dref" => Some(8),
            "avc1" => Some(78),
            "mp4a" => Some(28),
            _ => None,
        }
    }

    /// Parses a sequence of boxes; `base` is the absolute offset of `data`.
    fn parse(data: &[u8], base: usize) -> Vec<Mp4Box> {
        let mut boxes = Vec::new();
        let mut pos = 0;
        while pos < data.len() {
            let kind = String::from_utf8(data[pos + 4..pos + 8].to_vec()).expect("ascii type");
            let (size, header) = match usz(u32_at(data, pos)) {
                1 => {
                    let large =
                        u64::from_be_bytes(data[pos + 8..pos + 16].try_into().expect("8 bytes"));
                    (usize::try_from(large).expect("fits"), 16)
                }
                size => (size, 8),
            };
            assert!(
                size >= header && pos + size <= data.len(),
                "box {kind} overflows"
            );
            let body = data[pos + header..pos + size].to_vec();
            let children = child_offset(&kind).map_or_else(Vec::new, |skip| {
                parse(&body[skip..], base + pos + header + skip)
            });
            let body = if kind == "mdat" { Vec::new() } else { body };
            boxes.push(Mp4Box {
                kind,
                start: base + pos,
                size,
                body,
                children,
            });
            pos += size;
        }
        boxes
    }

    /// Reads every sample of `trak` from `file` using stsc, stsz, and stco.
    fn samples<'a>(file: &'a [u8], trak: &Mp4Box) -> Vec<&'a [u8]> {
        let stbl = trak.get("mdia/minf/stbl");
        let size_table = &stbl.get("stsz").body;
        let sizes: Vec<usize> = (0..usz(u32_at(size_table, 8)))
            .map(|i| usz(u32_at(size_table, 12 + 4 * i)))
            .collect();
        let stco = &stbl.get("stco").body;
        let offsets: Vec<usize> = (0..usz(u32_at(stco, 4)))
            .map(|i| usz(u32_at(stco, 8 + 4 * i)))
            .collect();
        let stsc = &stbl.get("stsc").body;
        let runs: Vec<(usize, usize)> = (0..usz(u32_at(stsc, 4)))
            .map(|i| {
                (
                    usz(u32_at(stsc, 8 + 12 * i)),
                    usz(u32_at(stsc, 12 + 12 * i)),
                )
            })
            .collect();
        let mut out = Vec::new();
        let mut sample = 0;
        for (chunk, &offset) in offsets.iter().enumerate() {
            let per_chunk = runs
                .iter()
                .rev()
                .find(|&&(first, _)| first <= chunk + 1)
                .expect("run")
                .1;
            let mut pos = offset;
            for _ in 0..per_chunk {
                out.push(&file[pos..pos + sizes[sample]]);
                pos += sizes[sample];
                sample += 1;
            }
        }
        assert_eq!(sample, sizes.len());
        out
    }

    /// NAL unit types of a length-prefixed sample; panics if lengths are inconsistent.
    fn length_prefixed_types(sample: &[u8]) -> Vec<u8> {
        let mut types = Vec::new();
        let mut pos = 0;
        while pos < sample.len() {
            let len = usz(u32_at(sample, pos));
            assert!(len > 0 && pos + 4 + len <= sample.len());
            types.push(sample[pos + 4] & 0x1f);
            pos += 4 + len;
        }
        types
    }
}
