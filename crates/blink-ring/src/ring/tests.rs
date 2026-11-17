use super::*;

const MS: i64 = 10_000;
const FOREVER: Timestamp = Timestamp(i64::MAX);

fn video_format(width: u32) -> VideoFormat {
    VideoFormat {
        width,
        height: 48,
        fps: 30,
        sps: vec![0x67, 0x42, 0xc0, 0x0a],
        pps: vec![0x68, 0xce, 0x3c, 0x80],
        sample_aspect: None,
    }
}

fn audio_format(sample_rate: u32) -> AudioFormat {
    AudioFormat {
        sample_rate,
        channels: 2,
        audio_specific_config: vec![0x11, 0x90],
    }
}

fn video(pts_ms: i64, keyframe: bool) -> PacketMeta {
    PacketMeta {
        kind: StreamKind::Video,
        pts: Timestamp(pts_ms * MS),
        duration: Timestamp(33 * MS),
        keyframe,
    }
}

fn audio(pts_ms: i64) -> PacketMeta {
    PacketMeta {
        kind: StreamKind::Audio,
        pts: Timestamp(pts_ms * MS),
        duration: Timestamp(21 * MS),
        keyframe: false,
    }
}

fn ring(window_ms: i64, bytes: usize) -> ClipRing {
    let mut ring = ClipRing::with_capacity(Timestamp(window_ms * MS), bytes);
    ring.set_video_format(video_format(64));
    ring.set_audio_format(audio_format(48_000));
    ring
}

fn push(ring: &mut ClipRing, meta: PacketMeta, payload: &[u8]) -> PushOutcome {
    ring.push(meta, payload).expect("push accepted")
}

/// Pushes `gops` GOPs of `frames` video frames, 100 ms apart, starting at `start_ms`.
fn push_gops(ring: &mut ClipRing, start_ms: i64, gops: i64, frames: i64, size: usize) {
    for gop in 0..gops {
        for frame in 0..frames {
            let pts = start_ms + (gop * frames + frame) * 100;
            push(ring, video(pts, frame == 0), &vec![0xab; size]);
        }
    }
}

fn video_pts_ms(clip: &ClipBuf) -> Vec<i64> {
    clip.video().map(|(meta, _)| meta.pts.0 / MS).collect()
}

fn audio_pts_ms(clip: &ClipBuf) -> Vec<i64> {
    clip.audio().map(|(meta, _)| meta.pts.0 / MS).collect()
}

fn snapshot(ring: &ClipRing, now_ms: i64, duration_ms: i64) -> ClipBuf {
    let mut clip = ClipBuf::new();
    ring.snapshot(
        Timestamp(now_ms * MS),
        Timestamp(duration_ms * MS),
        &mut clip,
    )
    .expect("clip available");
    clip
}

/// Everything stored, from the oldest keyframe on.
fn snapshot_all(ring: &ClipRing) -> ClipBuf {
    let mut clip = ClipBuf::new();
    ring.snapshot(FOREVER, FOREVER, &mut clip)
        .expect("clip available");
    clip
}

fn bytes_in_entries(ring: &ClipRing) -> usize {
    ring.entries.iter().map(|entry| entry.len).sum()
}

#[test]
fn timestamp_helpers() {
    assert_eq!(Timestamp::from_secs(2), Timestamp(20_000_000));
    assert_eq!(Timestamp::from_millis(3), Timestamp(30_000));
    assert_eq!(Timestamp::from_secs(i64::MAX), Timestamp(i64::MAX));
    assert_eq!(
        Timestamp(i64::MIN).saturating_sub(Timestamp(1)),
        Timestamp(i64::MIN)
    );
    assert_eq!(Timestamp(5).saturating_add(Timestamp(2)).ticks(), 7);
}

#[test]
fn rejects_packets_without_a_format() {
    let mut ring = ClipRing::with_capacity(Timestamp::from_secs(1), 64);
    assert_eq!(
        ring.push(video(0, true), &[1]),
        Err(PushError::MissingVideoFormat)
    );
    assert_eq!(
        ring.push(audio(0), &[1]),
        Err(PushError::MissingAudioFormat)
    );
    let mut clip = ClipBuf::new();
    assert_eq!(
        ring.snapshot(FOREVER, FOREVER, &mut clip),
        Err(ClipError::NoKeyframe)
    );
}

#[test]
fn rejects_empty_and_oversized_payloads() {
    let mut ring = ring(1000, 16);
    assert_eq!(ring.push(video(0, true), &[]), Err(PushError::EmptyPayload));
    assert_eq!(
        ring.push(video(0, true), &[0; 17]),
        Err(PushError::PayloadTooLarge {
            len: 17,
            capacity: 16
        }),
    );
    assert!(ring.is_empty());
    assert_eq!(
        push(&mut ring, video(0, true), &[0; 16]),
        PushOutcome::Stored
    );
}

#[test]
fn zero_capacity_ring_rejects_everything() {
    let mut ring = ring(1000, 0);
    assert_eq!(ring.packet_capacity(), 1);
    assert!(matches!(
        ring.push(video(0, true), &[1]),
        Err(PushError::PayloadTooLarge { .. })
    ));
}

#[test]
fn limits_are_clamped() {
    let ring = ClipRing::with_limits(Timestamp::from_secs(1), 64, usize::MAX);
    assert_eq!(ring.packet_capacity(), 64);
    assert_eq!(max_allocation(0), max_allocation(1));
    assert!(max_allocation(size_of::<Entry>()) < max_allocation(1));
}

#[test]
fn recommends_byte_capacity_from_bitrate() {
    // 20 Mbps for 30 s plus 4 s of slack, with 25% headroom.
    assert_eq!(
        ClipRing::recommended_byte_capacity(Timestamp::from_secs(30), 20_000_000),
        106_250_000
    );
    assert_eq!(ClipRing::recommended_byte_capacity(Timestamp(-1), 8), 5);
}

#[test]
fn drops_delta_frames_until_the_first_keyframe() {
    let mut ring = ring(10_000, 1024);
    assert_eq!(
        push(&mut ring, video(0, false), &[1]),
        PushOutcome::DroppedAwaitingKeyframe
    );
    let mut clip = ClipBuf::new();
    assert_eq!(
        ring.snapshot(FOREVER, FOREVER, &mut clip),
        Err(ClipError::NoKeyframe)
    );
    assert_eq!(push(&mut ring, video(33, true), &[2]), PushOutcome::Stored);
    assert_eq!(push(&mut ring, video(66, false), &[3]), PushOutcome::Stored);
    assert_eq!(video_pts_ms(&snapshot_all(&ring)), [33, 66]);
}

#[test]
fn time_window_evicts_whole_gops() {
    // GOPs of 5 frames at 100 ms: keyframes at 0, 500, 1000, ...
    let mut ring = ring(1000, 1 << 12);
    push_gops(&mut ring, 0, 4, 5, 4);
    // Newest is 1900 ms; cutoff 900 ms. GOP 0 goes; GOP 2 (1000 ms) does not
    // yet cover the window, so GOP 1 (500 ms) is kept.
    let clip = snapshot_all(&ring);
    assert_eq!(video_pts_ms(&clip).first(), Some(&500));
    assert!(clip.video().next().is_some_and(|(meta, _)| meta.keyframe));
    assert_eq!(clip.video_len(), 15);
}

#[test]
fn byte_capacity_evicts_whole_gops() {
    // 10 bytes per frame, 5 frames per GOP, room for 2.4 GOPs.
    let mut ring = ring(60_000, 120);
    push_gops(&mut ring, 0, 5, 5, 10);
    assert!(ring.bytes_used() <= 120);
    let pts = video_pts_ms(&snapshot_all(&ring));
    assert_eq!(pts.first(), Some(&1500));
    assert_eq!(pts.len(), 10);
}

#[test]
fn packet_capacity_evicts_whole_gops() {
    let mut ring = ClipRing::with_limits(Timestamp::from_secs(60), 1 << 12, 8);
    ring.set_video_format(video_format(64));
    push_gops(&mut ring, 0, 3, 4, 1);
    assert_eq!(ring.len(), 8);
    assert_eq!(video_pts_ms(&snapshot_all(&ring)).first(), Some(&400));
}

#[test]
fn oversized_gop_is_dropped_and_resyncs_at_next_keyframe() {
    let mut ring = ring(60_000, 32);
    push(&mut ring, video(0, true), &[1; 16]);
    push(&mut ring, video(100, false), &[2; 12]);
    // The current GOP alone overflows: it is dropped, and so is this frame.
    assert_eq!(
        push(&mut ring, video(200, false), &[3; 8]),
        PushOutcome::DroppedAwaitingKeyframe
    );
    assert!(ring.is_empty());
    assert_eq!(
        push(&mut ring, video(300, false), &[4]),
        PushOutcome::DroppedAwaitingKeyframe
    );
    let mut clip = ClipBuf::new();
    assert_eq!(
        ring.snapshot(FOREVER, FOREVER, &mut clip),
        Err(ClipError::NoKeyframe)
    );
    assert_eq!(
        push(&mut ring, video(400, true), &[5; 30]),
        PushOutcome::Stored
    );
    assert_eq!(video_pts_ms(&snapshot_all(&ring)), [400]);
}

#[test]
fn audio_before_first_video_is_trimmed_and_selected_by_pts() {
    let mut ring = ring(1000, 1 << 12);
    for pts in (0..3000).step_by(100) {
        push(&mut ring, audio(pts), &[7; 4]);
    }
    // With no video, audio older than the window is evicted.
    assert_eq!(ring.len(), 11);
    // Video arrives late: audio at 3000 and 3100 is stored before the keyframe at 3000.
    push(&mut ring, audio(3000), &[7; 4]);
    push(&mut ring, audio(3100), &[7; 4]);
    push(&mut ring, video(3000, true), &[1; 4]);
    // Audio stored before the keyframe with an earlier PTS is useless and evicted.
    assert_eq!(ring.len(), 3);
    let clip = snapshot_all(&ring);
    assert_eq!(audio_pts_ms(&clip), [3000, 3100]);
    assert_eq!(video_pts_ms(&clip), [3000]);
}

#[test]
fn gop_eviction_keeps_audio_that_video_lagged_behind() {
    let mut ring = ring(500, 1 << 12);
    push(&mut ring, video(0, true), &[1]);
    // Audio runs 40 ms ahead of video in push order.
    let mut video_pts = 100;
    for audio_pts in (40..2000).step_by(20) {
        push(&mut ring, audio(audio_pts), &[2]);
        if audio_pts - 40 >= video_pts {
            push(&mut ring, video(video_pts, video_pts % 500 == 0), &[3]);
            video_pts += 100;
        }
    }
    // Keyframes at 0, 500, ...; the window evicted GOPs, and audio up to 40 ms
    // after each kept keyframe was pushed before it.
    let clip = snapshot(&ring, 1980, 500);
    let start = clip.start().0 / MS;
    assert_eq!(start, 1000);
    let audio = audio_pts_ms(&clip);
    // Preroll frame (within two frames before the keyframe), then every frame from it.
    assert_eq!(audio.first(), Some(&980));
    assert_eq!(audio.get(1), Some(&1000));
    assert!(audio.windows(2).all(|pair| pair[1] - pair[0] == 20));
}

#[test]
fn snapshot_cuts_at_the_save_time() {
    let mut ring = ring(60_000, 1 << 12);
    push_gops(&mut ring, 0, 4, 5, 2);
    for pts in (0..2000).step_by(50) {
        push(&mut ring, audio(pts), &[9; 2]);
    }
    // now - 1000 = 950: the latest keyframe at or before it is 500.
    let clip = snapshot(&ring, 1950, 1000);
    assert_eq!(video_pts_ms(&clip).first(), Some(&500));
    assert_eq!(clip.video_len(), 15);
    assert_eq!(audio_pts_ms(&clip).first(), Some(&500));
    assert_eq!(
        audio_pts_ms(&clip).last(),
        Some(&1900),
        "PTS at or after `now` is excluded"
    );
    assert_eq!(clip.start(), Timestamp(500 * MS));
    assert_eq!(clip.end(), Timestamp(1950 * MS));
    assert_eq!(clip.duration(), Timestamp(1450 * MS));
    assert!(clip.audio_format().is_some());
    // A cut exactly on a keyframe starts there.
    assert_eq!(
        video_pts_ms(&snapshot(&ring, 1950, 950)).first(),
        Some(&1000)
    );
    // Asking for more than is stored starts at the oldest keyframe.
    assert_eq!(
        video_pts_ms(&snapshot(&ring, 1950, 100_000)).first(),
        Some(&0)
    );
    // An earlier save time drops later packets.
    assert_eq!(
        video_pts_ms(&snapshot(&ring, 1250, 500)).last(),
        Some(&1200)
    );
}

#[test]
fn static_screen_clip_ends_at_the_save_time() {
    let mut ring = ring(60_000, 1 << 12);
    push_gops(&mut ring, 0, 1, 3, 2);
    for pts in (0..5000).step_by(20) {
        push(&mut ring, audio(pts), &[9; 2]);
    }
    // No video for almost 5 s; the clip still ends at `now`, not at the last frame.
    let clip = snapshot(&ring, 5000, 3000);
    assert_eq!(video_pts_ms(&clip), [0, 100, 200]);
    assert_eq!(clip.end(), Timestamp(5000 * MS));
    assert_eq!(audio_pts_ms(&clip).last(), Some(&4980));
}

#[test]
fn save_time_before_any_keyframe_has_no_clip() {
    let mut ring = ring(60_000, 1 << 12);
    push(&mut ring, video(500, true), &[1]);
    let mut clip = ClipBuf::new();
    assert_eq!(
        ring.snapshot(Timestamp(500 * MS), Timestamp(MS), &mut clip),
        Err(ClipError::NoKeyframe)
    );
}

#[test]
fn chunked_copy_matches_one_shot_and_sees_late_packets() {
    let mut ring = ring(60_000, 1 << 12);
    push_gops(&mut ring, 0, 2, 5, 7);
    for pts in (0..1000).step_by(25) {
        push(&mut ring, audio(pts), &[5; 3]);
    }
    let now = Timestamp(1000 * MS);
    let mut clip = ClipBuf::new();
    let mut cursor = ring.begin_clip(now, FOREVER, &mut clip).expect("clip");
    let mut calls = 0;
    while ring.copy_clip(&mut cursor, &mut clip, 10).expect("copy") == CopyProgress::More {
        calls += 1;
        if calls == 3 {
            // A frame still in the encoder at save time lands between chunks.
            push(&mut ring, video(999, false), &[6; 7]);
            push(&mut ring, video(1000, false), &[6; 7]);
        }
    }
    assert!(calls > 5);
    let mut expected = ClipBuf::new();
    ring.snapshot(now, FOREVER, &mut expected).expect("clip");
    assert_eq!(clip, expected);
    assert_eq!(video_pts_ms(&clip).last(), Some(&999));
}

#[test]
fn copy_reports_eviction_and_format_changes() {
    let mut ring = ClipRing::with_limits(Timestamp::from_secs(60), 1 << 12, 12);
    ring.set_video_format(video_format(64));
    ring.set_audio_format(audio_format(48_000));
    push_gops(&mut ring, 0, 2, 5, 1);
    let mut clip = ClipBuf::new();
    let mut cursor = ring.begin_clip(FOREVER, FOREVER, &mut clip).expect("clip");
    assert_eq!(
        ring.copy_clip(&mut cursor, &mut clip, 1),
        Ok(CopyProgress::More)
    );
    push_gops(&mut ring, 1000, 1, 5, 1);
    assert_eq!(
        ring.copy_clip(&mut cursor, &mut clip, 1),
        Err(ClipError::Evicted)
    );

    let mut cursor = ring.begin_clip(FOREVER, FOREVER, &mut clip).expect("clip");
    ring.set_audio_format(audio_format(44_100));
    assert_eq!(
        ring.copy_clip(&mut cursor, &mut clip, 1),
        Err(ClipError::FormatChanged)
    );
    let mut cursor = ring.begin_clip(FOREVER, FOREVER, &mut clip).expect("clip");
    ring.set_video_format(video_format(32));
    assert_eq!(
        ring.copy_clip(&mut cursor, &mut clip, 1),
        Err(ClipError::FormatChanged)
    );
}

#[test]
fn audio_preroll_frame_is_included() {
    let mut ring = ring(60_000, 1 << 12);
    for pts in (0..100).step_by(21) {
        push(&mut ring, audio(pts), &[1]);
    }
    push(&mut ring, video(100, true), &[2]);
    for pts in (105..300).step_by(21) {
        push(&mut ring, audio(pts), &[1]);
    }
    let clip = snapshot_all(&ring);
    // 84 is the last frame before the keyframe at 100.
    assert_eq!(audio_pts_ms(&clip).get(..3), Some(&[84, 105, 126][..]));
    assert_eq!(clip.start(), Timestamp(100 * MS));
}

#[test]
fn format_changes() {
    let mut ring = ring(60_000, 1 << 10);
    push_gops(&mut ring, 0, 2, 3, 4);
    push(&mut ring, audio(0), &[1]);
    ring.set_video_format(video_format(64));
    assert_eq!(ring.len(), 7, "same format keeps the segment");

    // An audio format change keeps video and drops earlier audio from clips.
    ring.set_audio_format(audio_format(44_100));
    push(&mut ring, audio(550), &[1]);
    let clip = snapshot_all(&ring);
    assert_eq!(clip.video_len(), 6);
    assert_eq!(audio_pts_ms(&clip), [550]);
    assert_eq!(
        clip.audio_format().map(|format| format.sample_rate),
        Some(44_100)
    );

    // A video format change starts a new segment.
    ring.set_video_format(video_format(32));
    assert!(ring.is_empty());
    assert_eq!(ring.bytes_used(), 0);
    assert_eq!(ring.newest_pts(), None);
    assert_eq!(
        push(&mut ring, video(900, false), &[1]),
        PushOutcome::DroppedAwaitingKeyframe
    );
    push(&mut ring, video(1000, true), &[1]);
    let clip = snapshot_all(&ring);
    assert_eq!(clip.video_format().map(|format| format.width), Some(32));
    assert_eq!(video_pts_ms(&clip), [1000]);
}

#[test]
fn warns_once_per_long_gop() {
    let mut ring = ring(1000, 1 << 12);
    push(&mut ring, video(0, true), &[1]);
    for pts in (0..1500).step_by(20) {
        push(&mut ring, audio(pts), &[1]);
    }
    assert!(ring.long_gop_warned);
    push(&mut ring, video(1500, true), &[1]);
    push(&mut ring, video(2600, true), &[1]);
    assert!(
        ring.long_gop_warned,
        "the GOP at 1500 is also longer than the window"
    );
    push(&mut ring, video(2700, true), &[1]);
    push(&mut ring, video(3700, true), &[1]);
    assert!(!ring.long_gop_warned, "the oldest GOP is now 100 ms");
}

#[test]
fn payloads_survive_wraparound() {
    let mut ring = ClipRing::with_limits(Timestamp::from_secs(60), 50, 16);
    ring.set_video_format(video_format(64));
    let mut expected = Vec::new();
    for index in 0..20_u8 {
        let keyframe = index % 3 == 0;
        let payload: Vec<u8> = (0..(index % 7 + 3)).map(|byte| byte ^ index).collect();
        let pts = i64::from(index) * 100;
        if keyframe {
            expected.clear();
        }
        expected.push((pts, payload.clone()));
        push(&mut ring, video(pts, keyframe), &payload);
    }
    let clip = snapshot_all(&ring);
    let stored: Vec<_> = clip
        .video()
        .map(|(meta, data)| (meta.pts.0 / MS, data.to_vec()))
        .collect();
    assert!(stored.ends_with(&expected));
    assert!(ring.bytes_used() <= 50);
}

struct Model {
    video: Vec<(PacketMeta, Vec<u8>)>,
    audio: Vec<(PacketMeta, Vec<u8>)>,
}

/// Packets of `list` with PTS in `start..end`.
fn model_range(
    list: &[(PacketMeta, Vec<u8>)],
    start: Timestamp,
    end: Timestamp,
) -> &[(PacketMeta, Vec<u8>)] {
    let from = list.partition_point(|(meta, _)| meta.pts < start);
    let to = list.partition_point(|(meta, _)| meta.pts < end);
    &list[from..to]
}

fn owned(clip_packets: impl Iterator<Item = (PacketMeta, Vec<u8>)>) -> Vec<(PacketMeta, Vec<u8>)> {
    clip_packets.collect()
}

/// Deterministic pseudo-random pushes checked against a model of everything stored.
///
/// Audio runs `skew_ms` ahead of video in push order. With `roomy` limits only
/// the time window evicts, so clips must hold exactly the model's packets and
/// the window invariant must hold; under pressure clip audio may lose a prefix.
fn randomized(byte_capacity: usize, packet_capacity: usize, skew_ms: i64, roomy: bool) {
    let iterations = if cfg!(miri) { 250 } else { 12_000 };
    let window = Timestamp(400 * MS);
    let mut ring = ClipRing::with_limits(window, byte_capacity, packet_capacity);
    ring.set_video_format(video_format(64));
    ring.set_audio_format(audio_format(48_000));
    let mut state: u32 = 0x1234_5678;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let mut model = Model {
        video: Vec::new(),
        audio: Vec::new(),
    };
    let mut clip = ClipBuf::new();
    for step in 0..iterations {
        let roll = next();
        let len = usize::try_from(roll % 40 + 1).expect("small");
        let fill = u8::try_from(step % 251).expect("small");
        let pts = i64::from(step) * 10;
        let meta = if roll % 3 == 0 {
            audio(pts + skew_ms)
        } else {
            video(pts, roll % 11 == 0)
        };
        if push(&mut ring, meta, &vec![fill; len]) == PushOutcome::Stored {
            let list = match meta.kind {
                StreamKind::Video => &mut model.video,
                StreamKind::Audio => &mut model.audio,
            };
            list.push((meta, vec![fill; len]));
        }
        assert_eq!(ring.bytes_used(), bytes_in_entries(&ring));
        assert!(ring.len() <= ring.packet_capacity());
        let Some(newest) = ring.newest_pts() else {
            continue;
        };
        if roomy {
            let cutoff = newest.saturating_sub(window);
            assert!(ring.keyframes.get(1).is_none_or(|next| next.pts > cutoff));
        }
        let end = newest.saturating_add(Timestamp(1));
        if ring.snapshot(end, window, &mut clip).is_err() {
            continue;
        }
        let start = clip.start();
        let video = owned(clip.video().map(|(meta, data)| (meta, data.to_vec())));
        assert!(
            video
                .first()
                .is_some_and(|(meta, _)| meta.keyframe && meta.pts == start)
        );
        assert_eq!(video, model_range(&model.video, start, end));

        let mut audio = owned(clip.audio().map(|(meta, data)| (meta, data.to_vec())));
        if audio.first().is_some_and(|(meta, _)| meta.pts < start) {
            let preroll = audio.remove(0);
            let before = model_range(&model.audio, Timestamp(i64::MIN), start);
            assert_eq!(before.last(), Some(&preroll));
        }
        let expected = model_range(&model.audio, start, end);
        if roomy {
            assert_eq!(audio, expected);
        } else {
            assert!(expected.ends_with(&audio));
        }
    }
}

#[test]
fn randomized_under_pressure() {
    randomized(256, 48, 0, false);
}

#[test]
fn randomized_under_pressure_with_skew() {
    randomized(256, 48, 30, false);
}

#[test]
fn randomized_window_only_with_skew() {
    randomized(1 << 16, 4096, 30, true);
}

#[test]
fn push_never_reallocates() {
    let iterations = if cfg!(miri) { 500 } else { 50_000 };
    let mut ring = ring(500, 512);
    let arena_ptr = ring.arena.as_ptr();
    let entries_capacity = ring.entries.capacity();
    let keyframes_capacity = ring.keyframes.capacity();
    for step in 0..iterations {
        let pts = i64::from(step) * 10;
        let len = usize::try_from(step % 29 + 1).expect("small");
        let meta = if step % 4 == 0 {
            audio(pts)
        } else {
            video(pts, step % 30 == 1)
        };
        let _ = push(&mut ring, meta, &[0x5a; 32][..len]);
    }
    assert!(!ring.is_empty());
    assert_eq!(ring.arena.as_ptr(), arena_ptr);
    assert_eq!(ring.byte_capacity(), 512);
    assert_eq!(ring.entries.capacity(), entries_capacity);
    assert_eq!(ring.keyframes.capacity(), keyframes_capacity);
}

#[test]
fn reused_clip_buffer_does_not_grow() {
    let mut ring = ring(500, 1 << 12);
    let mut clip = ClipBuf::with_capacity(1 << 12, 512);
    for step in 0..200 {
        push(&mut ring, video(step * 10, step % 30 == 0), &[1; 8]);
        if step % 7 == 0 {
            push(&mut ring, audio(step * 10), &[2; 4]);
        }
    }
    ring.snapshot(FOREVER, FOREVER, &mut clip).expect("clip");
    let data_len = clip.data_len();
    for step in 200..400 {
        push(&mut ring, video(step * 10, step % 30 == 0), &[1; 8]);
        ring.snapshot(FOREVER, FOREVER, &mut clip).expect("clip");
    }
    assert!(clip.data_len() <= 1 << 12);
    assert!(data_len > 0);
    assert_eq!(
        clip.audio_len(),
        0,
        "audio ended before the last clip's keyframe"
    );
}

#[test]
fn hand_built_clip() {
    let mut clip = ClipBuf::new();
    assert!(clip.video_format().is_none());
    clip.begin(&video_format(64), None, Timestamp(5), Timestamp(10));
    clip.push_video(video(0, true), &[1, 2]);
    clip.push_audio(audio(0), &[3]);
    assert_eq!(
        clip.video()
            .map(|(_, data)| data.to_vec())
            .collect::<Vec<_>>(),
        [vec![1, 2]]
    );
    assert_eq!(
        clip.audio()
            .map(|(_, data)| data.to_vec())
            .collect::<Vec<_>>(),
        [vec![3]]
    );
    assert_eq!(clip.duration(), Timestamp(5));
}

#[test]
fn default_packet_capacity_is_bounded() {
    assert_eq!(
        default_packet_capacity(Timestamp::from_secs(30), 1 << 30),
        34 * 512
    );
    assert_eq!(default_packet_capacity(Timestamp(1), 1 << 30), 5 * 512);
    assert_eq!(default_packet_capacity(Timestamp(-5), 1 << 30), 4 * 512);
    assert_eq!(default_packet_capacity(Timestamp::from_secs(30), 100), 100);
    assert_eq!(default_packet_capacity(Timestamp(i64::MAX), 1000), 1000);
}

#[test]
fn debug_output_summarizes() {
    let ring = ring(1000, 8);
    let text = format!("{ring:?}");
    assert!(text.contains("byte_capacity: 8"));
}

#[test]
fn prepare_sizes_the_buffer_for_the_whole_ring() {
    let mut ring = ring(10_000, 1 << 16);
    push_gops(&mut ring, 0, 3, 10, 100);
    ring.push(audio(50), &[1; 30]).expect("push accepted");
    let mut clip = snapshot(&ring, 3_000, 3_000);
    assert!(clip.video_len() > 0);

    clip.prepare(ring.bytes_used(), ring.len());
    assert_eq!(clip.data_len(), 0);
    assert_eq!(clip.video_len(), 0);
    assert_eq!(clip.audio_len(), 0);
    let capacity = clip.data_capacity();
    assert!(capacity >= ring.bytes_used());

    let mut cursor = ring
        .begin_clip(Timestamp(3_000 * MS), Timestamp(3_000 * MS), &mut clip)
        .expect("clip available");
    let progress = ring
        .copy_clip(&mut cursor, &mut clip, usize::MAX)
        .expect("copied");
    assert_eq!(progress, CopyProgress::Done);
    assert_eq!(clip.data_len(), ring.bytes_used());
    assert_eq!(
        clip.data_capacity(),
        capacity,
        "the copy did not reallocate"
    );
}
