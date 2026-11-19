//! AAC-LC encoding through the Media Foundation AAC encoder MFT.
//!
//! The encoder (`CLSID_AACMFTEncoder`, a synchronous MFT with one input and
//! one output stream, both with ID 0) takes 16-bit PCM at 48 kHz stereo and
//! produces raw AAC-LC access units (`MF_MT_AAC_PAYLOAD_TYPE` 0, no ADTS) at
//! the configured `MF_MT_AUDIO_AVG_BYTES_PER_SECOND`. blink feeds it exactly
//! one 1024-frame chunk per `ProcessInput` and drains `ProcessOutput` until
//! `MF_E_TRANSFORM_NEED_MORE_INPUT` after every chunk.
//!
//! Buffers are allocated once: a small pool of input samples used round robin
//! and, unless the MFT allocates its own, one output sample. Encoded frames are
//! handed to the sink straight from the locked output buffer.

use std::mem::ManuallyDrop;

use blink_ring::media::{AudioFormat, PacketMeta, StreamKind, Timestamp};
use blink_sys::ErrorChain;
use tracing::{debug, warn};
use windows::Win32::Media::MediaFoundation::{
    AACMFTEncoder, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION,
    MF_MT_AAC_PAYLOAD_TYPE, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_MT_USER_DATA, MFAudioFormat_AAC, MFAudioFormat_PCM,
    MFCreateAlignedMemoryBuffer, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Audio, MFT_INPUT_STREAM_DOES_NOT_ADDREF, MFT_INPUT_STREAM_INFO,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_END_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_TYPE, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::GUID;

use super::{Context, has_flag};
use crate::asc::{self, AscInfo};
use crate::pcm;
use crate::pts_queue::PtsQueue;
use crate::timeline::{CHUNK_SAMPLES, Chunk, frames_to_ticks};
use crate::{AudioConfig, AudioError, AudioSink, CHANNELS, PACKET_DURATION, SAMPLE_RATE};

/// Input and output stream ID. The AAC encoder has fixed streams numbered
/// from zero (`GetStreamIDs` returns `E_NOTIMPL`).
const STREAM: u32 = 0;

/// Bytes in one input chunk: 1024 frames of two 16-bit samples.
const INPUT_BYTES: u32 = 4096;

/// Input samples used round robin. The MFT may keep a reference to an input
/// sample after `ProcessInput` returns (unless it reports
/// `MFT_INPUT_STREAM_DOES_NOT_ADDREF`), so a sample is only rewritten after
/// seven later chunks (150 ms) have gone through. The encoder's look-ahead is
/// about two frames.
const INPUT_POOL: usize = 8;

/// Encoder priming delay, in frames, subtracted from every output timestamp so
/// decoded audio lines up with the capture clock. Zero until measured: the
/// Media Foundation encoder's delay (1024 or 2112 frames are both plausible)
/// is not documented, and guessing wrong shifts audio early instead of late.
/// Measure with a loopback click test against the video clock, then set it.
const ENCODER_DELAY_FRAMES: u64 = 0;

/// Largest AAC-LC stereo access unit: 6144 bits per channel.
const MAX_FRAME_BYTES: u32 = 1536;

/// `MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION` for AAC Profile L2 (up to
/// 48 kHz stereo), the encoder's default.
const AAC_PROFILE_L2: u32 = 0x29;

/// A sample and its single buffer, allocated once and reused.
struct MediaSlot {
    sample: IMFSample,
    buffer: IMFMediaBuffer,
}

/// A configured, streaming AAC encoder.
pub(super) struct AacEncoder {
    transform: IMFTransform,
    /// Used round robin: the head is the next to fill, then rotated to the back.
    inputs: [MediaSlot; INPUT_POOL],
    /// `None` when the MFT allocates its own output samples.
    output: Option<MediaSlot>,
    pts: PtsQueue,
    format: AudioFormat,
    first_output_logged: bool,
    /// Whether this segment already warned about a full [`PtsQueue`].
    overflow_warned: bool,
}

impl AacEncoder {
    /// Creates and configures the encoder for `config.bitrate`.
    pub(super) fn new(config: AudioConfig) -> Result<Self, AudioError> {
        let avg_bytes_per_second = config.bitrate.avg_bytes_per_second();
        // SAFETY: COM is initialized on this thread (`MediaRuntime`); creates an
        // in-process object with no aggregation.
        let transform: IMFTransform =
            unsafe { CoCreateInstance(&AACMFTEncoder, None, CLSCTX_INPROC_SERVER) }
                .context("create the Media Foundation AAC encoder")?;
        let input_type = pcm_type()?;
        let output_type = aac_type(avg_bytes_per_second)?;
        set_types(&transform, &input_type, &output_type)?;
        let format = audio_format(&transform)?;

        let mut input_info = MFT_INPUT_STREAM_INFO::default();
        // SAFETY: the out-pointer refers to a live local of the written type.
        unsafe { transform.GetInputStreamInfo(STREAM, &raw mut input_info) }
            .context("query the encoder input stream")?;
        // SAFETY: plain COM call; both media types are set.
        let output_info = unsafe { transform.GetOutputStreamInfo(STREAM) }
            .context("query the encoder output stream")?;
        let provides_samples = has_flag(output_info.dwFlags, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0);
        debug!(
            input_flags = input_info.dwFlags,
            input_does_not_addref =
                has_flag(input_info.dwFlags, MFT_INPUT_STREAM_DOES_NOT_ADDREF.0),
            output_flags = output_info.dwFlags,
            output_size = output_info.cbSize,
            output_alignment = output_info.cbAlignment,
            provides_samples,
            "AAC encoder stream info"
        );

        let input = || media_slot(INPUT_BYTES, input_info.cbAlignment);
        let inputs = [
            input()?,
            input()?,
            input()?,
            input()?,
            input()?,
            input()?,
            input()?,
            input()?,
        ];
        let output = if provides_samples {
            None
        } else {
            let size = output_info.cbSize.max(MAX_FRAME_BYTES);
            Some(media_slot(size, output_info.cbAlignment)?)
        };

        let encoder = Self {
            transform,
            inputs,
            output,
            pts: PtsQueue::with_delay(Timestamp::from_ticks(frames_to_ticks(ENCODER_DELAY_FRAMES))),
            format,
            first_output_logged: false,
            overflow_warned: false,
        };
        encoder.message(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, "begin streaming")?;
        encoder.message(MFT_MESSAGE_NOTIFY_START_OF_STREAM, "start the stream")?;
        Ok(encoder)
    }

    /// The AAC stream format, including its `AudioSpecificConfig`.
    pub(super) const fn format(&self) -> &AudioFormat {
        &self.format
    }

    /// Encodes one 1024-frame chunk and passes every frame the encoder
    /// produces to `sink`.
    pub(super) fn encode(
        &mut self,
        chunk: Chunk<'_>,
        sink: &mut impl AudioSink,
    ) -> Result<(), AudioError> {
        let [slot, ..] = &self.inputs;
        fill_input(slot, chunk)?;
        // SAFETY: plain COM call with a fully written input sample.
        let result = unsafe { self.transform.ProcessInput(STREAM, &slot.sample, 0) };
        match result {
            Err(error) if error.code() == MF_E_NOTACCEPTING => {
                // The MFT still holds output; drain it and retry once.
                self.pull(sink)?;
                let [slot, ..] = &self.inputs;
                // SAFETY: as above; the output was just drained.
                unsafe { self.transform.ProcessInput(STREAM, &slot.sample, 0) }
                    .context("feed PCM to the AAC encoder")?;
            }
            other => other.context("feed PCM to the AAC encoder")?,
        }
        self.inputs.rotate_left(1);
        if !self.pts.push(chunk.pts) && !self.overflow_warned {
            self.overflow_warned = true;
            warn!("AAC encoder is holding more input than expected; timestamps may skew");
        }
        self.pull(sink)
    }

    /// Flushes every buffered frame to `sink` and readies the encoder for a
    /// new segment.
    pub(super) fn drain(&mut self, sink: &mut impl AudioSink) -> Result<(), AudioError> {
        self.message(MFT_MESSAGE_NOTIFY_END_OF_STREAM, "end the stream")?;
        self.message(MFT_MESSAGE_COMMAND_DRAIN, "drain the encoder")?;
        self.pull(sink)?;
        self.pts.clear();
        self.overflow_warned = false;
        self.message(MFT_MESSAGE_NOTIFY_START_OF_STREAM, "restart the stream")
    }

    fn message(&self, message: MFT_MESSAGE_TYPE, context: &'static str) -> Result<(), AudioError> {
        // SAFETY: plain COM call; none of the messages sent here use the
        // parameter.
        unsafe { self.transform.ProcessMessage(message, 0) }.context(context)
    }

    /// Calls `ProcessOutput` until the encoder needs more input.
    fn pull(&mut self, sink: &mut impl AudioSink) -> Result<(), AudioError> {
        loop {
            if let Some(output) = &self.output {
                // SAFETY: plain COM call on a buffer this encoder owns.
                unsafe { output.buffer.SetCurrentLength(0) }
                    .context("reset the AAC output buffer")?;
            }
            // `clone` adds a COM reference (no allocation) that the MFT may use;
            // it is released when the buffer's sample is dropped below.
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: STREAM,
                pSample: ManuallyDrop::new(self.output.as_ref().map(|slot| slot.sample.clone())),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0_u32;
            // SAFETY: `buffers` describes one output stream with either our
            // preallocated sample or none (when the MFT provides samples); the
            // status out-pointer refers to a live local.
            let result = unsafe {
                self.transform
                    .ProcessOutput(0, &mut buffers, &raw mut status)
            };
            let [buffer] = buffers;
            let sample = ManuallyDrop::into_inner(buffer.pSample);
            drop(ManuallyDrop::into_inner(buffer.pEvents));
            match result {
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                other => other.context("pull AAC from the encoder")?,
            }
            let Some(sample) = sample else {
                return Ok(());
            };
            self.emit(&sample, sink)?;
        }
    }

    fn emit(&mut self, sample: &IMFSample, sink: &mut impl AudioSink) -> Result<(), AudioError> {
        // SAFETY: plain COM call; for a single-buffer sample this returns that
        // buffer without copying.
        let buffer =
            unsafe { sample.ConvertToContiguousBuffer() }.context("read the AAC output buffer")?;
        let locked = Locked::new(&buffer)?;
        let payload = locked.current();
        if payload.is_empty() {
            return Ok(());
        }
        let pts = self.pts.next(PACKET_DURATION);
        if !self.first_output_logged {
            self.first_output_logged = true;
            // SAFETY: plain COM call on a live sample.
            let mft_time = unsafe { sample.GetSampleTime() }.ok();
            debug!(
                ?mft_time,
                pts = pts.ticks(),
                bytes = payload.len(),
                "first AAC frame (MFT time vs blink pts, for A/V offset checks)"
            );
        }
        sink.packet(
            PacketMeta {
                kind: StreamKind::Audio,
                pts,
                duration: PACKET_DURATION,
                keyframe: true,
            },
            payload,
        );
        Ok(())
    }
}

impl Drop for AacEncoder {
    fn drop(&mut self) {
        if let Err(error) = self.message(MFT_MESSAGE_NOTIFY_END_STREAMING, "end streaming") {
            debug!(error = %ErrorChain(&error), "AAC encoder did not accept end of streaming");
        }
    }
}

/// Writes `chunk` into `slot` and stamps it.
fn fill_input(slot: &MediaSlot, chunk: Chunk<'_>) -> Result<(), AudioError> {
    let written = {
        let mut locked = Locked::new(&slot.buffer)?;
        pcm::write_le(chunk.pcm, locked.whole())
    };
    let expected = CHUNK_SAMPLES * 2;
    if written != expected {
        return Err(AudioError::InvalidEncoderBuffer(written));
    }
    let length = u32::try_from(written).map_err(|_| AudioError::InvalidEncoderBuffer(written))?;
    // SAFETY: plain COM call; `length` is within the buffer's maximum length
    // (it was just written).
    unsafe { slot.buffer.SetCurrentLength(length) }.context("size the PCM input buffer")?;
    // SAFETY: plain COM call; Media Foundation time is also in 100 ns units.
    unsafe { slot.sample.SetSampleTime(chunk.pts.ticks()) }.context("stamp the PCM input")?;
    // SAFETY: plain COM call.
    unsafe { slot.sample.SetSampleDuration(PACKET_DURATION.ticks()) }
        .context("set the PCM input duration")
}

/// An `IMFMediaBuffer` locked for CPU access; unlocks on drop.
struct Locked<'a> {
    buffer: &'a IMFMediaBuffer,
    data: *mut u8,
    max_length: usize,
    current_length: usize,
}

impl<'a> Locked<'a> {
    fn new(buffer: &'a IMFMediaBuffer) -> Result<Self, AudioError> {
        let mut data = std::ptr::null_mut();
        let mut max_length = 0_u32;
        let mut current_length = 0_u32;
        // SAFETY: the out-pointers refer to live locals of the written types.
        unsafe {
            buffer.Lock(
                &raw mut data,
                Some(&raw mut max_length),
                Some(&raw mut current_length),
            )
        }
        .context("lock a media buffer")?;
        let locked = Self {
            buffer,
            data,
            max_length: usize::try_from(max_length).unwrap_or(0),
            current_length: usize::try_from(current_length).unwrap_or(0),
        };
        if locked.data.is_null() || locked.current_length > locked.max_length {
            return Err(AudioError::InvalidEncoderBuffer(locked.current_length));
        }
        Ok(locked)
    }

    /// The valid bytes (`0..current_length`).
    const fn current(&self) -> &[u8] {
        // SAFETY: `Lock` succeeded with a non-null pointer to `max_length`
        // bytes, `current_length <= max_length` (checked in `new`), and the
        // buffer stays locked for the borrow's lifetime.
        unsafe { std::slice::from_raw_parts(self.data, self.current_length) }
    }

    /// The whole buffer (`0..max_length`) for writing.
    const fn whole(&mut self) -> &mut [u8] {
        // SAFETY: `Lock` succeeded with a non-null pointer to `max_length`
        // writable bytes, locked and exclusively borrowed through `self` for
        // the slice's lifetime.
        unsafe { std::slice::from_raw_parts_mut(self.data, self.max_length) }
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: balances the successful `Lock` in `new`.
        if let Err(error) = unsafe { self.buffer.Unlock() } {
            debug!(error = %ErrorChain(&error), "IMFMediaBuffer::Unlock failed");
        }
    }
}

fn media_slot(size: u32, alignment: u32) -> Result<MediaSlot, AudioError> {
    let buffer = if alignment > 1 {
        // SAFETY: plain allocation call; the second argument is the
        // alignment minus one (for example `MF_16_BYTE_ALIGNMENT` is 15), as
        // reported alignments are powers of two.
        unsafe { MFCreateAlignedMemoryBuffer(size, alignment - 1) }
    } else {
        // SAFETY: plain allocation call.
        unsafe { MFCreateMemoryBuffer(size) }
    }
    .context("allocate a media buffer")?;
    // SAFETY: plain allocation call.
    let sample = unsafe { MFCreateSample() }.context("allocate a media sample")?;
    // SAFETY: plain COM call attaching a live buffer.
    unsafe { sample.AddBuffer(&buffer) }.context("attach a media buffer")?;
    Ok(MediaSlot { sample, buffer })
}

fn new_audio_type(subtype: &GUID) -> Result<IMFMediaType, AudioError> {
    // SAFETY: plain allocation call.
    let media_type = unsafe { MFCreateMediaType() }.context("create a media type")?;
    set_guid(&media_type, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
    set_guid(&media_type, &MF_MT_SUBTYPE, subtype)?;
    set_u32(&media_type, &MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
    set_u32(&media_type, &MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
    set_u32(&media_type, &MF_MT_AUDIO_NUM_CHANNELS, u32::from(CHANNELS))?;
    Ok(media_type)
}

/// 16-bit PCM, 48 kHz, stereo.
fn pcm_type() -> Result<IMFMediaType, AudioError> {
    let media_type = new_audio_type(&MFAudioFormat_PCM)?;
    let block_align = 2 * u32::from(CHANNELS);
    set_u32(&media_type, &MF_MT_AUDIO_BLOCK_ALIGNMENT, block_align)?;
    set_u32(
        &media_type,
        &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
        SAMPLE_RATE * block_align,
    )?;
    Ok(media_type)
}

/// Raw AAC-LC, 48 kHz, stereo, at `avg_bytes_per_second`.
fn aac_type(avg_bytes_per_second: u32) -> Result<IMFMediaType, AudioError> {
    let media_type = new_audio_type(&MFAudioFormat_AAC)?;
    set_u32(
        &media_type,
        &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
        avg_bytes_per_second,
    )?;
    set_u32(&media_type, &MF_MT_AAC_PAYLOAD_TYPE, 0)?;
    set_u32(
        &media_type,
        &MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION,
        AAC_PROFILE_L2,
    )?;
    Ok(media_type)
}

fn set_guid(media_type: &IMFMediaType, key: &GUID, value: &GUID) -> Result<(), AudioError> {
    // SAFETY: both pointers come from references valid for the call, which
    // copies the value.
    unsafe { media_type.SetGUID(key, value) }.context("set a media type GUID")
}

fn set_u32(media_type: &IMFMediaType, key: &GUID, value: u32) -> Result<(), AudioError> {
    // SAFETY: the key pointer comes from a reference valid for the call.
    unsafe { media_type.SetUINT32(key, value) }.context("set a media type attribute")
}

/// Sets the output type, then the input type; if the encoder refuses that
/// order, tries input first.
fn set_types(
    transform: &IMFTransform,
    input: &IMFMediaType,
    output: &IMFMediaType,
) -> Result<(), AudioError> {
    // SAFETY: plain COM call with a live media type.
    let output_first = unsafe { transform.SetOutputType(STREAM, output, 0) }.and_then(|()| {
        // SAFETY: as above.
        unsafe { transform.SetInputType(STREAM, input, 0) }
    });
    let Err(error) = output_first else {
        return Ok(());
    };
    debug!(
        error = %ErrorChain(&error),
        "AAC encoder refused output-then-input; trying input first"
    );
    // A half-applied output type would constrain the input type; clear it.
    // SAFETY: plain COM call; a null type clears the stream's output type.
    if let Err(error) = unsafe { transform.SetOutputType(STREAM, None::<&IMFMediaType>, 0) } {
        debug!(error = %ErrorChain(&error), "could not clear the AAC encoder output type");
    }
    // SAFETY: plain COM call with a live media type.
    unsafe { transform.SetInputType(STREAM, input, 0) }
        .context("set the AAC encoder input type")?;
    // SAFETY: plain COM call with a live media type.
    unsafe { transform.SetOutputType(STREAM, output, 0) }.context("set the AAC encoder output type")
}

/// Reads the `AudioSpecificConfig` the encoder reports in the current output
/// type's `MF_MT_USER_DATA` (`HEAACWAVEINFO` minus its `WAVEFORMATEX`, see
/// [`asc::HEAAC_INFO_LEN`]) and checks it against the one blink would build.
/// Falls back to the built one if the encoder reports none.
fn audio_format(transform: &IMFTransform) -> Result<AudioFormat, AudioError> {
    let built = asc::aac_lc(SAMPLE_RATE, CHANNELS)
        .ok_or_else(|| AudioError::InvalidAudioSpecificConfig(Vec::new()))?;
    // SAFETY: plain COM call; the output type is set.
    let current = unsafe { transform.GetOutputCurrentType(STREAM) }
        .context("read the AAC encoder output type")?;
    let mut blob = [0_u8; 64];
    let mut size = 0_u32;
    // SAFETY: `blob` is a live buffer whose length the binding passes along;
    // the size out-pointer refers to a live local.
    let read = unsafe { current.GetBlob(&MF_MT_USER_DATA, &mut blob, Some(&raw mut size)) };
    let reported = match read {
        Ok(()) => usize::try_from(size)
            .ok()
            .and_then(|size| blob.get(..size))
            .and_then(asc::from_user_data),
        Err(error) => {
            debug!(
                error = %ErrorChain(&error),
                "AAC encoder output type has no usable MF_MT_USER_DATA"
            );
            None
        }
    };
    let Some(reported) = reported else {
        warn!("AAC encoder reported no AudioSpecificConfig; using the built one");
        return Ok(AudioFormat {
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            audio_specific_config: built.to_vec(),
        });
    };
    let expected = AscInfo {
        object_type: 2,
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
    };
    if asc::parse(reported) != Some(expected) {
        return Err(AudioError::InvalidAudioSpecificConfig(reported.to_vec()));
    }
    if reported != built {
        warn!(
            ?reported,
            ?built,
            "encoder AudioSpecificConfig differs from the built one; using the encoder's"
        );
    }
    Ok(AudioFormat {
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        audio_specific_config: reported.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::imp::com::MediaRuntime;

    /// Encodes one second of a 1 kHz tone through the real Media Foundation
    /// encoder (Windows only) and checks the stream shape.
    #[test]
    fn encodes_a_tone() {
        let _runtime = MediaRuntime::init().unwrap();
        let mut encoder = AacEncoder::new(AudioConfig::default()).unwrap();
        assert_eq!(encoder.format().audio_specific_config, [0x11, 0x90]);

        let (tx, rx) = mpsc::channel();
        let mut sink = move |meta: PacketMeta, payload: &[u8]| {
            tx.send((meta, payload.len())).unwrap();
        };
        let mut pcm = [0_i16; CHUNK_SAMPLES];
        let mut frame = 0_u32;
        for chunk_index in 0..50_u64 {
            for pair in pcm.as_chunks_mut::<2>().0 {
                // 1 kHz at 48 kHz: 48 frames per period.
                let phase = f32::from(u16::try_from(frame % 48).unwrap()) / 48.0;
                let value = crate::pcm::f32_to_s16((phase * std::f32::consts::TAU).sin() * 0.25);
                *pair = [value; 2];
                frame += 1;
            }
            let chunk = Chunk {
                pcm: &pcm,
                pts: Timestamp::from_ticks(frames_to_ticks(chunk_index * 1024)),
            };
            encoder.encode(chunk, &mut sink).unwrap();
        }
        encoder.drain(&mut sink).unwrap();
        drop(sink);

        let frames: Vec<(PacketMeta, usize)> = rx.into_iter().collect();
        assert!((50..=53).contains(&frames.len()), "{} frames", frames.len());
        assert_eq!(
            frames.first().map(|(meta, _)| meta.pts),
            Some(Timestamp::ZERO)
        );
        for (meta, len) in &frames {
            assert_eq!(meta.kind, StreamKind::Audio);
            assert!(meta.keyframe);
            assert!((1..=1536).contains(len), "{len} bytes");
        }
        for pair in frames.windows(2) {
            assert!(pair[0].0.pts < pair[1].0.pts);
        }
    }
}
