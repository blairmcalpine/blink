# blink-mux test fixtures

Tiny elementary streams that stand in for what NVENC and the Media Foundation
AAC encoder emit. They are committed so the structural tests need no external
tools. Regenerate them with FFmpeg (built with libx264) from this directory:

```sh
# H.264 Baseline, 64x64, 30 fps, 2 s (60 frames), IDR every 30 frames, no
# B-frames. Annex B with an AUD before every access unit and SPS/PPS repeated
# on every IDR, like NVENC with repeated headers.
ffmpeg -y -v error -f lavfi -i testsrc2=size=64x64:rate=30 -t 2 \
  -c:v libx264 -profile:v baseline -preset veryfast -bf 0 -g 30 -keyint_min 30 \
  -sc_threshold 0 -x264-params aud=1:repeat-headers=1 -pix_fmt yuv420p \
  -f h264 video_baseline.h264

# H.264 High, 64x48, 30 fps, 1 s (30 frames), IDR every 15 frames. Exercises
# the avcC High-profile extension and an SPS with an emulation-prevention byte.
ffmpeg -y -v error -f lavfi -i testsrc2=size=64x48:rate=30 -t 1 \
  -c:v libx264 -profile:v high -preset veryfast -bf 0 -g 15 -keyint_min 15 \
  -sc_threshold 0 -x264-params aud=1:repeat-headers=1 -pix_fmt yuv420p \
  -f h264 video_high.h264

# AAC-LC, 48 kHz stereo, 2 s, as ADTS. The tests strip the ADTS headers to get
# raw AAC frames and build the AudioSpecificConfig from the first header.
ffmpeg -y -v error -f lavfi -i sine=frequency=440:sample_rate=48000 -ac 2 -t 2 \
  -c:a aac -profile:a aac_low -b:a 32k -f adts audio_48k_stereo.aac
```

| File | Size | Contents |
| --- | --- | --- |
| `video_baseline.h264` | 17.9 KB | 60 access units, 2 IDRs |
| `video_high.h264` | 3.1 KB | 30 access units, 2 IDRs |
| `audio_48k_stereo.aac` | 8.6 KB | 95 ADTS frames |

Generated with FFmpeg 9.0.2. The tests split the video at AUD NAL units and
assign timestamps from the frame index, so the files carry no timing of their
own.
