//! WASAPI shared-mode capture: loopback of the default render endpoint (the
//! desktop audio), or the default recording endpoint (the microphone).
//!
//! Both use the `eConsole` role, the device Sound settings shows as the
//! default. Windows treats a stream on the `eCommunications` default (the
//! "default communication device") as communications activity, which can
//! trigger its automatic ducking of other sounds, game audio included; voice
//! chat apps choose their own device anyway.
//!
//! Loopback adds `AUDCLNT_STREAMFLAGS_LOOPBACK`; everything else is shared.
//! Format negotiation, in order:
//!
//! 1. 48 kHz stereo 32-bit float, then 16-bit PCM, with
//!    `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY`
//!    so the audio engine converts the mix format (any rate, any channel
//!    layout, such as a mono 44.1 kHz microphone) for us. Float is tried first because it matches the engine's
//!    internal format; the clamp to 16 bits then happens in [`crate::pcm`].
//! 2. If the engine refuses both, the endpoint's own mix format, used only if
//!    it already is 48 kHz stereo float or 16-bit PCM. Resampling and
//!    downmixing are out of scope, so any other mix format is an error.
//!
//! Each format is tried event-driven (`AUDCLNT_STREAMFLAGS_EVENTCALLBACK`)
//! first, then with timer polling. Event-driven loopback is only supported
//! from Windows 10 1703; on older builds `Initialize` may fail (then polling
//! is used) or succeed without ever signaling, which the capture loop
//! tolerates because it drains the stream on every wait timeout as well.

use std::ptr;
use std::thread;
use std::time::Duration;

use blink_sys::ErrorChain;
use tracing::debug;
use windows::Win32::Foundation::{E_ACCESSDENIED, ERROR_NOT_FOUND, HANDLE, WAIT_FAILED};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, EDataFlow,
    IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, WAVE_FORMAT_PCM,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0, eCapture, eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::{
    KSDATAFORMAT_SUBTYPE_PCM, SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT, WAVE_FORMAT_EXTENSIBLE,
};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::{GUID, HRESULT, Owned, PCWSTR};

use super::com::CoTaskMem;
use super::{Context, Fault, has_flag};
use crate::pcm::SampleFormat;
use crate::timeline::Packet;
use crate::{AudioError, CHANNELS, SAMPLE_RATE};

/// Requested shared-mode buffer: 500 ms. The engine may allocate more. A
/// large buffer lets the below-normal-priority audio thread be starved for
/// a while without losing data; an overrun still only costs a gap that the
/// timeline fills with silence.
const BUFFER_DURATION_HNS: i64 = 5_000_000;

/// Longest wait for the audio event before draining anyway.
const EVENT_TIMEOUT_MS: u32 = 20;

/// Sleep between drains when the stream is not event-driven: about one
/// shared-mode period.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One captured packet's flags, decoded.
#[derive(Debug, Clone, Copy)]
pub(super) struct PacketInfo {
    /// QPC time of the packet's first frame in 100 ns ticks, or `None` if
    /// WASAPI flagged the timestamp invalid.
    pub(super) timestamp: Option<i64>,
    /// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`: data was lost before this
    /// packet (also set on the first packet after `Start`).
    pub(super) discontinuity: bool,
}

/// Which endpoint a [`CaptureStream`] records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    /// Loopback of the default render endpoint: what the desktop plays.
    Desktop,
    /// The default recording endpoint.
    Microphone,
}

impl Source {
    const fn data_flow(self) -> EDataFlow {
        match self {
            Self::Desktop => eRender,
            Self::Microphone => eCapture,
        }
    }

    const fn stream_flags(self) -> u32 {
        match self {
            Self::Desktop => AUDCLNT_STREAMFLAGS_LOOPBACK,
            Self::Microphone => 0,
        }
    }

    const fn find_default(self) -> &'static str {
        match self {
            Self::Desktop => "find the default render endpoint",
            Self::Microphone => "find the default recording endpoint",
        }
    }
}

/// A started capture stream on one endpoint. Stops on drop.
pub(super) struct CaptureStream {
    source: Source,
    client: IAudioClient,
    capture: IAudioCaptureClient,
    format: SampleFormat,
    frame_bytes: usize,
    device_id: CoTaskMem<u16>,
    // Declared last so the handle is closed after the client is released.
    event: Option<Owned<HANDLE>>,
}

impl CaptureStream {
    /// Opens and starts capture of `source` on its current default console
    /// endpoint.
    pub(super) fn open(
        enumerator: &IMMDeviceEnumerator,
        source: Source,
    ) -> Result<Self, AudioError> {
        let device = default_device(enumerator, source)?;
        let device_id = device_id(&device)?;
        let (client, format, event) = initialize(&device, source)?;
        // SAFETY: `client` was successfully initialized, which `GetService`
        // requires.
        let capture: IAudioCaptureClient =
            unsafe { client.GetService() }.context("get the capture client")?;
        // SAFETY: the client is initialized and, if event-driven, its event
        // handle is set, as `Start` requires.
        unsafe { client.Start() }.context("start the capture stream")?;
        Ok(Self {
            source,
            client,
            capture,
            format,
            frame_bytes: format.bytes_per_sample() * usize::from(CHANNELS),
            device_id,
            event,
        })
    }

    /// Sample format the engine delivers.
    pub(super) const fn format(&self) -> SampleFormat {
        self.format
    }

    /// Whether the stream signals an event per period (otherwise it is polled).
    pub(super) const fn is_event_driven(&self) -> bool {
        self.event.is_some()
    }

    /// Blocks until the stream has data or a short timeout passes.
    pub(super) fn wait(&self) {
        let Some(event) = &self.event else {
            thread::sleep(POLL_INTERVAL);
            return;
        };
        // SAFETY: the handle is a live auto-reset event owned by `self`.
        let result = unsafe { WaitForSingleObject(**event, EVENT_TIMEOUT_MS) };
        if result == WAIT_FAILED {
            // Never spin: fall back to a timed sleep.
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Reads every available packet and hands it to `on_packet`.
    ///
    /// WASAPI failures (device invalidated, audio service restarted) are
    /// [`Fault::Device`]; errors from `on_packet` are [`Fault::Fatal`].
    pub(super) fn drain(
        &self,
        on_packet: &mut impl FnMut(Packet<'_>, PacketInfo) -> Result<(), AudioError>,
    ) -> Result<(), Fault> {
        loop {
            // SAFETY: plain COM call on a started capture client.
            let next = unsafe { self.capture.GetNextPacketSize() }
                .context("query the next capture packet")
                .map_err(Fault::Device)?;
            if next == 0 {
                return Ok(());
            }
            let mut data: *mut u8 = ptr::null_mut();
            let mut frames = 0_u32;
            let mut flags = 0_u32;
            let mut qpc_position = 0_u64;
            // SAFETY: every out-pointer refers to a live local of the type the
            // API writes; the device-position pointer is omitted (`None`).
            unsafe {
                self.capture.GetBuffer(
                    &raw mut data,
                    &raw mut frames,
                    &raw mut flags,
                    None,
                    Some(&raw mut qpc_position),
                )
            }
            .context("read a capture packet")
            .map_err(Fault::Device)?;
            if frames == 0 {
                // `AUDCLNT_S_BUFFER_EMPTY`: no packet was acquired, so there is
                // nothing to release.
                return Ok(());
            }
            let info = PacketInfo {
                // `u64QPCPosition` is already in 100 ns units.
                timestamp: if has_flag(flags, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0) {
                    None
                } else {
                    i64::try_from(qpc_position).ok()
                },
                discontinuity: has_flag(flags, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0),
            };
            let len = usize::try_from(frames)
                .ok()
                .and_then(|frames| frames.checked_mul(self.frame_bytes));
            let packet = match len {
                Some(len) if !data.is_null() && !has_flag(flags, AUDCLNT_BUFFERFLAGS_SILENT.0) => {
                    // SAFETY: `GetBuffer` succeeded with a non-null pointer to
                    // `frames` frames of `frame_bytes` bytes each (the block
                    // alignment of the negotiated format), readable until
                    // `ReleaseBuffer` below; the slice does not escape this
                    // iteration.
                    let data = unsafe { std::slice::from_raw_parts(data, len) };
                    Packet::Pcm {
                        format: self.format,
                        data,
                    }
                }
                _ => Packet::Silent { frames },
            };
            let handled = on_packet(packet, info);
            // SAFETY: releases exactly the packet acquired above, with the
            // frame count `GetBuffer` returned.
            let released =
                unsafe { self.capture.ReleaseBuffer(frames) }.context("release a capture packet");
            handled.map_err(Fault::Fatal)?;
            released.map_err(Fault::Device)?;
        }
    }

    /// Whether this stream's endpoint is still the default console endpoint
    /// for its source.
    pub(super) fn is_default(&self, enumerator: &IMMDeviceEnumerator) -> Result<bool, AudioError> {
        let device = default_device(enumerator, self.source)?;
        Ok(device_id(&device)?.as_wide() == self.device_id.as_wide())
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        // SAFETY: plain COM call on the started client; an error (for example
        // an invalidated device) leaves nothing to clean up.
        if let Err(error) = unsafe { self.client.Stop() } {
            debug!(error = %ErrorChain(&error), "IAudioClient::Stop failed");
        }
    }
}

/// The default console endpoint for `source`. Fails with `E_NOTFOUND` when
/// there is none, such as a machine without a microphone.
fn default_device(
    enumerator: &IMMDeviceEnumerator,
    source: Source,
) -> Result<IMMDevice, AudioError> {
    // SAFETY: plain COM call on a live enumerator with enum arguments.
    unsafe { enumerator.GetDefaultAudioEndpoint(source.data_flow(), eConsole) }
        .context(source.find_default())
}

fn device_id(device: &IMMDevice) -> Result<CoTaskMem<u16>, AudioError> {
    // SAFETY: plain COM call on a live device.
    let id = unsafe { device.GetId() }.context("read the endpoint id")?;
    // SAFETY: `GetId` returns a NUL-terminated string allocated with
    // `CoTaskMemAlloc` that the caller must free.
    Ok(unsafe { CoTaskMem::from_pwstr(id) })
}

/// Initializes a fresh `IAudioClient` per attempt (a client whose
/// `Initialize` failed must not be reused) following the negotiation order in
/// the module documentation.
fn initialize(
    device: &IMMDevice,
    source: Source,
) -> Result<(IAudioClient, SampleFormat, Option<Owned<HANDLE>>), AudioError> {
    let convert = source.stream_flags()
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    for format in [SampleFormat::F32, SampleFormat::S16] {
        let wave = requested_format(format);
        for event_driven in [true, false] {
            match try_initialize(device, (&raw const wave).cast(), convert, event_driven) {
                Ok((client, event)) => return Ok((client, format, event)),
                // Privacy settings refuse every format alike.
                Err(error) if is_access_denied(&error) => return Err(error),
                Err(error) => {
                    debug!(?source, ?format, event_driven, error = %ErrorChain(&error), "capture format refused");
                }
            }
        }
    }
    let client = activate(device)?;
    // SAFETY: plain COM call on an activated, uninitialized client.
    let mix = unsafe { client.GetMixFormat() }.context("read the endpoint mix format")?;
    // SAFETY: `GetMixFormat` returns a `CoTaskMemAlloc` allocation that the
    // caller must free.
    let mix = unsafe { CoTaskMem::new(mix) };
    let (wave, sub_format) = read_mix(&mix)?;
    let (tag, sample_rate, channels, bits) = (
        wave.wFormatTag,
        wave.nSamplesPerSec,
        wave.nChannels,
        wave.wBitsPerSample,
    );
    // Logged once per open attempt; a failure is warned about by the caller.
    debug!(
        ?source,
        tag,
        sample_rate,
        channels,
        bits,
        ?sub_format,
        "audio engine refused converted 48 kHz stereo capture; trying the endpoint mix format"
    );
    let format = classify(&wave, sub_format)?;
    let flags = source.stream_flags();
    try_initialize(device, mix.as_ptr(), flags, true)
        .or_else(|_| try_initialize(device, mix.as_ptr(), flags, false))
        .map(|(client, event)| (client, format, event))
}

/// Whether `error` is `E_ACCESSDENIED`, which a recording endpoint returns
/// while the Windows privacy settings deny desktop apps the microphone.
pub(super) fn is_access_denied(error: &AudioError) -> bool {
    matches!(error, AudioError::Windows { source, .. } if source.code() == E_ACCESSDENIED)
}

/// Whether `error` is `E_NOTFOUND`: there is no endpoint of the kind asked
/// for.
pub(super) fn is_not_found(error: &AudioError) -> bool {
    matches!(
        error,
        AudioError::Windows { source, .. }
            if source.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0)
    )
}

fn activate(device: &IMMDevice) -> Result<IAudioClient, AudioError> {
    // SAFETY: plain COM activation on a live device without activation
    // parameters.
    unsafe { device.Activate(CLSCTX_ALL, None) }.context("activate an audio client")
}

fn try_initialize(
    device: &IMMDevice,
    format: *const WAVEFORMATEX,
    extra_flags: u32,
    event_driven: bool,
) -> Result<(IAudioClient, Option<Owned<HANDLE>>), AudioError> {
    let client = activate(device)?;
    let mut flags = extra_flags;
    if event_driven {
        flags |= AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
    }
    // SAFETY: `format` points to a valid `WAVEFORMATEX` (or extensible) that
    // outlives this call, which copies it. Shared mode requires a periodicity
    // of 0. No session GUID is passed, so the default session is used.
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            flags,
            BUFFER_DURATION_HNS,
            0,
            format,
            None,
        )
    }
    .context("initialize the capture stream")?;
    if !event_driven {
        return Ok((client, None));
    }
    // SAFETY: no security attributes or name; an unnamed auto-reset event,
    // initially unsignaled.
    let event = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
        .context("create the audio event")?;
    // SAFETY: `CreateEventW` returned a fresh handle that nothing else owns.
    let event = unsafe { Owned::new(event) };
    // SAFETY: the client was initialized with `EVENTCALLBACK` and the handle is
    // a live event that outlives the client (`CaptureStream` field order).
    unsafe { client.SetEventHandle(*event) }.context("set the audio event")?;
    Ok((client, Some(event)))
}

/// 48 kHz stereo in `format`, as `WAVEFORMATEXTENSIBLE`.
fn requested_format(format: SampleFormat) -> WAVEFORMATEXTENSIBLE {
    let bits = format.bits();
    let block_align = bits / 8 * CHANNELS;
    let extension = size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>();
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: u16::try_from(WAVE_FORMAT_EXTENSIBLE).unwrap_or_default(),
            nChannels: CHANNELS,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * u32::from(block_align),
            nBlockAlign: block_align,
            wBitsPerSample: bits,
            cbSize: u16::try_from(extension).unwrap_or_default(),
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: bits,
        },
        dwChannelMask: SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
        SubFormat: match format {
            SampleFormat::F32 => KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
            SampleFormat::S16 => KSDATAFORMAT_SUBTYPE_PCM,
        },
    }
}

/// Reads the mix format header and, for `WAVE_FORMAT_EXTENSIBLE`, its
/// sub-format.
fn read_mix(mix: &CoTaskMem<WAVEFORMATEX>) -> Result<(WAVEFORMATEX, Option<GUID>), AudioError> {
    let ptr = mix.as_ptr();
    if ptr.is_null() {
        return Err(AudioError::NullOutput {
            call: "IAudioClient::GetMixFormat",
            what: "mix format",
        });
    }
    // SAFETY: `GetMixFormat` succeeded, so `ptr` points to a valid
    // `WAVEFORMATEX` that `mix` keeps allocated. The struct is packed, hence
    // the unaligned read.
    let wave = unsafe { ptr.read_unaligned() };
    let extension = size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>();
    let sub_format = if u32::from(wave.wFormatTag) == WAVE_FORMAT_EXTENSIBLE
        && usize::from(wave.cbSize) >= extension
    {
        // SAFETY: `cbSize` says the allocation extends far enough past the
        // `WAVEFORMATEX` header to hold a whole `WAVEFORMATEXTENSIBLE`.
        let extensible = unsafe { ptr.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
        Some(extensible.SubFormat)
    } else {
        None
    };
    Ok((wave, sub_format))
}

/// The mix format as a [`SampleFormat`], if it is 48 kHz stereo float or
/// 16-bit PCM.
fn classify(wave: &WAVEFORMATEX, sub_format: Option<GUID>) -> Result<SampleFormat, AudioError> {
    let tag = u32::from(wave.wFormatTag);
    let is_float =
        tag == WAVE_FORMAT_IEEE_FLOAT || sub_format == Some(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
    let is_pcm = tag == WAVE_FORMAT_PCM || sub_format == Some(KSDATAFORMAT_SUBTYPE_PCM);
    let (sample_rate, channels, bits) = (wave.nSamplesPerSec, wave.nChannels, wave.wBitsPerSample);
    match (sample_rate, channels, bits) {
        (SAMPLE_RATE, CHANNELS, 32) if is_float => Ok(SampleFormat::F32),
        (SAMPLE_RATE, CHANNELS, 16) if is_pcm => Ok(SampleFormat::S16),
        _ => Err(AudioError::UnsupportedMixFormat {
            tag: wave.wFormatTag,
            sample_rate,
            channels,
            bits,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wave(tag: u32, sample_rate: u32, channels: u16, bits: u16) -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: u16::try_from(tag).unwrap(),
            nChannels: channels,
            nSamplesPerSec: sample_rate,
            nAvgBytesPerSec: 0,
            nBlockAlign: 0,
            wBitsPerSample: bits,
            cbSize: 0,
        }
    }

    #[test]
    fn requested_formats_are_consistent() {
        for (format, bits, sub) in [
            (SampleFormat::F32, 32, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT),
            (SampleFormat::S16, 16, KSDATAFORMAT_SUBTYPE_PCM),
        ] {
            let ext = requested_format(format);
            let header = ext.Format;
            let (tag, rate, channels, align, avg, size, bits_per_sample) = (
                header.wFormatTag,
                header.nSamplesPerSec,
                header.nChannels,
                header.nBlockAlign,
                header.nAvgBytesPerSec,
                header.cbSize,
                header.wBitsPerSample,
            );
            assert_eq!(u32::from(tag), WAVE_FORMAT_EXTENSIBLE);
            assert_eq!((rate, channels, bits_per_sample), (48_000, 2, bits));
            assert_eq!(align, bits / 4);
            assert_eq!(avg, 48_000 * u32::from(align));
            assert_eq!(usize::from(size), 22);
            let (sub_format, mask) = (ext.SubFormat, ext.dwChannelMask);
            assert_eq!(sub_format, sub);
            assert_eq!(mask, SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT);
            assert_eq!(classify(&header, Some(sub_format)).unwrap(), format);
        }
    }

    #[test]
    fn classifies_supported_mix_formats() {
        let float = wave(WAVE_FORMAT_IEEE_FLOAT, 48_000, 2, 32);
        assert_eq!(classify(&float, None).unwrap(), SampleFormat::F32);
        let pcm = wave(WAVE_FORMAT_PCM, 48_000, 2, 16);
        assert_eq!(classify(&pcm, None).unwrap(), SampleFormat::S16);
        let ext = wave(WAVE_FORMAT_EXTENSIBLE, 48_000, 2, 32);
        assert_eq!(
            classify(&ext, Some(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT)).unwrap(),
            SampleFormat::F32
        );
    }

    #[test]
    fn rejects_unsupported_mix_formats() {
        for (wave, sub) in [
            (wave(WAVE_FORMAT_IEEE_FLOAT, 44_100, 2, 32), None),
            (wave(WAVE_FORMAT_IEEE_FLOAT, 48_000, 6, 32), None),
            (wave(WAVE_FORMAT_PCM, 48_000, 2, 24), None),
            (wave(WAVE_FORMAT_PCM, 48_000, 2, 32), None),
            (wave(WAVE_FORMAT_EXTENSIBLE, 48_000, 2, 32), None),
            (
                wave(WAVE_FORMAT_EXTENSIBLE, 48_000, 2, 16),
                Some(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT),
            ),
        ] {
            assert!(matches!(
                classify(&wave, sub),
                Err(AudioError::UnsupportedMixFormat { .. })
            ));
        }
    }
}
