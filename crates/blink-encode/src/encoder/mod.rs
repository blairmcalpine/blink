//! The NVENC H.264 encoder.

mod api;
mod session;

use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::Interface;

use blink_ring::media::{PacketMeta, StreamKind, Timestamp, VideoFormat};

use self::api::Api;
use self::session::{Bitstream, PictureInput, Registered, Session};
use crate::annexb::parameter_sets;
use crate::config::bitrate_bps;
use crate::ffi::{
    NV_ENC_CODEC_H264_GUID, NV_ENC_CONFIG, NV_ENC_INITIALIZE_PARAMS, NV_ENC_PIC_FLAG_FORCEIDR,
    NV_ENC_PIC_FLAG_OUTPUT_SPSPPS, NV_ENC_RECONFIGURE_PARAMS, NV_ENC_RECONFIGURE_PARAMS_VER,
    NV_MAX_SEQ_HDR_LEN, RECONFIGURE_FORCE_IDR_BIT, RECONFIGURE_RESET_ENCODER_BIT, zeroed,
};
use crate::idr::IdrScheduler;
use crate::params::{TUNING, apply_config, fill_initialize, preset_guid};
use crate::{EncodeError, EncoderConfig};

/// Registered textures kept at once: the three-texture capture pool plus one
/// spare. Each registration holds a texture alive (about 33 MB at 4K), so the
/// cap bounds what a replaced pool can pin if `release_textures` is missed.
/// Beyond it the least recently registered texture is unregistered.
const MAX_REGISTERED_TEXTURES: usize = 4;

/// Initial capacity of the reused output buffer: one second at the target
/// bitrate, but at least 256 KiB, which covers a typical IDR frame. A larger
/// frame grows it once; it never shrinks.
const MIN_OUTPUT_CAPACITY: usize = 256 * 1024;

/// One encoded access unit, borrowed from the encoder until the next call.
#[derive(Debug, Clone, Copy)]
pub struct EncodedFrame<'a> {
    /// Ring metadata: video, the input timestamp as pts, `keyframe` when NVENC
    /// produced an IDR picture, and a nominal one-frame duration (the muxer
    /// derives real durations from the next packet's pts).
    pub meta: PacketMeta,
    /// The access unit in Annex B form (start codes included). IDR frames
    /// carry SPS and PPS in-band.
    pub data: &'a [u8],
}

struct Registration {
    /// Holds a reference so the texture cannot be freed and its address
    /// reused while it is registered.
    texture: ID3D11Texture2D,
    handle: Registered,
}

/// An NVENC H.264 session encoding BGRA D3D11 textures.
///
/// Create it on the capture thread with the device from
/// `blink_capture::create_device` and call [`encode`](Self::encode) with each
/// captured texture.
///
/// # Keyframes
///
/// NVENC never inserts IDR frames on its own (infinite GOP). The encoder
/// forces one on the first frame, on the first frame after
/// [`reconfigure`](Self::reconfigure), after [`request_idr`](Self::request_idr),
/// and whenever a second of PTS has passed since the last IDR, so variable-rate
/// capture still meets the ring's one-IDR-per-second contract. On a static
/// screen nothing is encoded at all, so the caller polls
/// [`idr_due`](Self::idr_due) and re-encodes its last captured texture when it
/// returns `true`.
///
/// # Synchronous encoding
///
/// NVENC is driven synchronously (`enableEncodeAsync = 0`, which the header
/// documents as supported and `FFmpeg` also uses): `encode` submits the frame
/// and then waits in `nvEncLockBitstream` until the NVENC block finishes it.
/// The encode itself runs on the dedicated encoder, not the shader cores, but
/// the wait stalls the capture thread, so a frame must finish inside the frame
/// budget (16.7 ms at 60 fps, 4.2 ms at 240 fps). Expected ceiling, to be
/// confirmed by benchmark: comfortable at 1080p and 1440p up to 144 fps and 4K
/// at 60 fps with P2; 240 fps at 1440p and 4K above 60 fps may not keep up
/// (fall back to P1, or go asynchronous). Benchmarks should record blink's CPU
/// usage too, since the driver may spin while it waits.
///
/// The asynchronous upgrade: `enableEncodeAsync = 1`, two or three bitstream
/// buffers each with a completion event registered through
/// `nvEncRegisterAsyncEvent`, and a separate thread that waits on the events
/// and locks the buffers in submit order, so capture only submits. Inputs stay
/// mapped until their output is locked, which the three-texture pool allows
/// for at most two frames in flight.
///
/// Not `Send`: it holds COM interfaces and an NVENC session bound to the
/// device whose immediate context the capture thread uses.
pub struct Encoder {
    // Field order: `Drop` releases registrations and the bitstream first; then
    // `session` drops, destroying the encoder and unloading the DLL.
    registrations: Vec<Registration>,
    bitstream: Bitstream,
    config: Box<NV_ENC_CONFIG>,
    init: Box<NV_ENC_INITIALIZE_PARAMS>,
    settings: EncoderConfig,
    format: VideoFormat,
    output: Vec<u8>,
    frame_duration: Timestamp,
    idr: IdrScheduler,
    session: Session,
}

impl Encoder {
    /// Loads NVENC, opens a session on `device`, and configures H.264 CBR at
    /// the requested size and rate.
    ///
    /// # Errors
    ///
    /// [`EncodeError::Config`] for an invalid `config`;
    /// [`EncodeError::NoNvidiaDriver`], [`EncodeError::DriverTooOld`],
    /// [`EncodeError::MissingExport`], or [`EncodeError::MissingFunction`] if
    /// NVENC is unusable; [`EncodeError::Nvenc`] if the driver rejects the
    /// session or configuration (for example, a size beyond the GPU's limit).
    pub fn new(device: &ID3D11Device, config: EncoderConfig) -> Result<Self, EncodeError> {
        config.validate()?;
        let api = Api::load()?;
        let session = Session::open(api, device)?;

        let preset =
            session.preset_config(NV_ENC_CODEC_H264_GUID, preset_guid(config.preset), TUNING)?;
        let mut encode_config = Box::new(preset.presetCfg);
        apply_config(&mut encode_config, &config)?;

        let mut init: Box<NV_ENC_INITIALIZE_PARAMS> = Box::new(zeroed());
        fill_initialize(&mut init, &config)?;
        init.encodeConfig = &raw mut *encode_config;
        session.initialize(&mut init)?;

        let bitstream = session.create_bitstream()?;
        let capacity = usize::try_from(bitrate_bps(config.bitrate_kbps)? / 8)
            .unwrap_or(MIN_OUTPUT_CAPACITY)
            .max(MIN_OUTPUT_CAPACITY);
        let format = read_format(&session, &config)?;
        let encoder = Self {
            registrations: Vec::with_capacity(MAX_REGISTERED_TEXTURES),
            bitstream,
            config: encode_config,
            init,
            settings: config,
            format,
            output: Vec::with_capacity(capacity),
            frame_duration: config.frame_duration(),
            idr: IdrScheduler::new(config.frame_duration()),
            session,
        };
        tracing::info!(
            width = config.width,
            height = config.height,
            max_width = config.max_width,
            max_height = config.max_height,
            fps = config.fps,
            bitrate_kbps = config.bitrate_kbps,
            preset = ?config.preset,
            stretch_to = ?config.stretch_to,
            "NVENC H.264 encoder ready"
        );
        Ok(encoder)
    }

    /// Encodes one BGRA texture captured at `timestamp`.
    ///
    /// The texture must be `DXGI_FORMAT_B8G8R8A8_UNORM`, single-sampled, on
    /// the encoder's device, and exactly the current encode size. It is
    /// registered with NVENC the first time it is seen and the registration
    /// is reused afterwards, so the capture pool's textures are each
    /// registered once. The frame is forced to an IDR when one is due (see
    /// the type docs). Returns the access unit, borrowed until the next call;
    /// steady state allocates nothing.
    ///
    /// NVENC reads the texture on the same device after capture's
    /// `CopyResource`, which the driver orders. **Needs hardware
    /// verification**: if torn or stale frames show up, capture should
    /// `Flush` its context after the copy.
    ///
    /// # Errors
    ///
    /// [`EncodeError::TextureSizeMismatch`] if the texture is not the current
    /// size (call [`reconfigure`](Self::reconfigure) first),
    /// [`EncodeError::UnsupportedTextureFormat`] or
    /// [`EncodeError::UnsupportedTextureLayout`] for a texture NVENC cannot
    /// read, and [`EncodeError::Nvenc`] if a driver call fails. A pending IDR
    /// stays pending after an error.
    pub fn encode(
        &mut self,
        texture: &ID3D11Texture2D,
        timestamp: Timestamp,
    ) -> Result<EncodedFrame<'_>, EncodeError> {
        let index = self.registration_for(texture)?;
        let registered = self
            .registrations
            .get(index)
            .map(|registration| &registration.handle)
            .ok_or(EncodeError::NullOutput {
                call: "nvEncRegisterResource",
                what: "cached registration",
            })?;

        let force_idr = self.idr.due(timestamp);
        let mapped = self.session.map(registered)?;
        let picture = PictureInput {
            input: &mapped,
            output: &self.bitstream,
            width: self.settings.width,
            height: self.settings.height,
            timestamp: timestamp.0.cast_unsigned(),
            duration: self.frame_duration.0.cast_unsigned(),
            flags: if force_idr {
                NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS
            } else {
                0
            },
        };
        let encoded = self.session.encode_picture(&picture).and_then(|()| {
            let output = &mut self.output;
            self.session.read_bitstream(&self.bitstream, |picture| {
                output.clear();
                output.extend_from_slice(picture.data);
                picture.idr
            })
        });
        let unmapped = self.session.unmap(&mapped);
        let keyframe = encoded?;
        unmapped?;

        self.idr.observe(timestamp, keyframe);
        if force_idr && !keyframe {
            tracing::warn!("NVENC ignored a forced IDR; retrying on the next frame");
        }
        Ok(EncodedFrame {
            meta: PacketMeta {
                kind: StreamKind::Video,
                pts: timestamp,
                duration: self.frame_duration,
                keyframe,
            },
            data: &self.output,
        })
    }

    /// Makes the next encoded frame an IDR. Call it whenever
    /// `ClipRing::push` returns `PushOutcome::DroppedAwaitingKeyframe`.
    pub const fn request_idr(&mut self) {
        self.idr.request();
    }

    /// Whether a frame encoded at `now` would be forced to an IDR: one was
    /// requested, none has been produced yet (or since a reconfigure), or a
    /// second of PTS has passed since the last one.
    ///
    /// On a static screen nothing is encoded, so the capture loop polls this
    /// and, when it returns `true`, re-encodes its last captured texture
    /// stamped `now`. That texture is still intact: the capture pool only
    /// reuses it after three newer frames.
    #[must_use]
    pub fn idr_due(&self, now: Timestamp) -> bool {
        self.idr.due(now)
    }

    /// Handles a capture mode change: unregisters every cached texture (the
    /// capture pool was recreated) and, if the size changed, resizes the
    /// session.
    ///
    /// Call it on every `CaptureEvent::ModeChanged`. A resize uses
    /// `nvEncReconfigureEncoder` with `resetEncoder` and `forceIDR`, so the
    /// next frame is an IDR with new SPS/PPS, and recomputes the sample aspect
    /// ratio from `stretch_to` for the new size. Returns `true` when the
    /// format changed; then push [`video_format`](Self::video_format) to
    /// `ClipRing::set_video_format` before the next packet. A size beyond the
    /// session maximum needs a new `Encoder`.
    ///
    /// # Errors
    ///
    /// [`EncodeError::Config`] if the size is zero or exceeds the session
    /// maximum; the encoder is unchanged apart from the released textures.
    /// [`EncodeError::Nvenc`] or [`EncodeError::SequenceHeader`] if the driver
    /// fails; the session may then be half-reconfigured, so drop the encoder
    /// and create a new one.
    pub fn reconfigure(&mut self, width: u32, height: u32) -> Result<bool, EncodeError> {
        self.release_textures();
        if (width, height) == (self.settings.width, self.settings.height) {
            return Ok(false);
        }
        let next = EncoderConfig {
            width,
            height,
            ..self.settings
        };
        next.validate()?;

        let mut params: Box<NV_ENC_RECONFIGURE_PARAMS> = Box::new(zeroed());
        params.version = NV_ENC_RECONFIGURE_PARAMS_VER;
        params.reInitEncodeParams = *self.init;
        fill_initialize(&mut params.reInitEncodeParams, &next)?;
        params.reInitEncodeParams.encodeConfig = &raw mut *self.config;
        params.bitFields = RECONFIGURE_RESET_ENCODER_BIT | RECONFIGURE_FORCE_IDR_BIT;
        self.session.reconfigure(&mut params)?;

        *self.init = params.reInitEncodeParams;
        self.settings = next;
        self.idr.reset();
        self.format = read_format(&self.session, &self.settings)?;
        tracing::info!(
            width,
            height,
            sample_aspect = ?self.format.sample_aspect,
            "NVENC encoder reconfigured"
        );
        Ok(true)
    }

    /// The current stream format: size, rate, SPS and PPS (raw NAL units,
    /// start codes stripped), and sample aspect ratio. Push it to
    /// `ClipRing::set_video_format` after [`new`](Self::new) and whenever
    /// [`reconfigure`](Self::reconfigure) returns `true`.
    #[must_use]
    pub const fn video_format(&self) -> &VideoFormat {
        &self.format
    }

    /// The current configuration, with the current encode size.
    #[must_use]
    pub const fn config(&self) -> &EncoderConfig {
        &self.settings
    }

    /// Unregisters every cached texture so NVENC drops its references to
    /// them. [`reconfigure`](Self::reconfigure) does this itself; call this
    /// directly only if the capture pool is replaced some other way.
    pub fn release_textures(&mut self) {
        for registration in self.registrations.drain(..) {
            unregister(&self.session, registration);
        }
    }

    /// The index of `texture`'s registration, registering it (and evicting
    /// the oldest entry when full) if needed.
    fn registration_for(&mut self, texture: &ID3D11Texture2D) -> Result<usize, EncodeError> {
        let raw = texture.as_raw();
        if let Some(index) = self
            .registrations
            .iter()
            .position(|registration| registration.texture.as_raw() == raw)
        {
            return Ok(index);
        }

        self.check_texture(texture)?;
        if self.registrations.len() >= MAX_REGISTERED_TEXTURES {
            let oldest = self.registrations.remove(0);
            unregister(&self.session, oldest);
        }
        let handle = self
            .session
            .register(texture, self.settings.width, self.settings.height)?;
        tracing::debug!(
            width = self.settings.width,
            height = self.settings.height,
            "registered capture texture with NVENC"
        );
        self.registrations.push(Registration {
            texture: texture.clone(),
            handle,
        });
        Ok(self.registrations.len() - 1)
    }

    fn check_texture(&self, texture: &ID3D11Texture2D) -> Result<(), EncodeError> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `texture` is a live interface and `desc` is a writable
        // descriptor of the type `GetDesc` fills.
        unsafe { texture.GetDesc(&raw mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(EncodeError::UnsupportedTextureFormat(desc.Format.0));
        }
        if desc.ArraySize != 1 || desc.MipLevels != 1 || desc.SampleDesc.Count != 1 {
            return Err(EncodeError::UnsupportedTextureLayout {
                array_size: desc.ArraySize,
                mip_levels: desc.MipLevels,
                sample_count: desc.SampleDesc.Count,
            });
        }
        if (desc.Width, desc.Height) != (self.settings.width, self.settings.height) {
            return Err(EncodeError::TextureSizeMismatch {
                texture_width: desc.Width,
                texture_height: desc.Height,
                width: self.settings.width,
                height: self.settings.height,
            });
        }
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.release_textures();
        if let Err(error) = self.session.destroy_bitstream(&self.bitstream) {
            tracing::warn!(%error, "failed to destroy the NVENC bitstream buffer");
        }
        // `session` drops next and destroys the encoder.
    }
}

impl core::fmt::Debug for Encoder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Encoder")
            .field("config", &self.settings)
            .field("registered_textures", &self.registrations.len())
            .finish_non_exhaustive()
    }
}

/// The stream format for `settings`, with SPS/PPS from
/// `nvEncGetSequenceParams`.
fn read_format(session: &Session, settings: &EncoderConfig) -> Result<VideoFormat, EncodeError> {
    let mut header = [0_u8; NV_MAX_SEQ_HDR_LEN];
    let header = session.sequence_params(&mut header)?;
    let (sps, pps) = parameter_sets(header)?;
    Ok(VideoFormat {
        width: settings.width,
        height: settings.height,
        fps: settings.fps,
        sps: sps.to_vec(),
        pps: pps.to_vec(),
        sample_aspect: settings.sample_aspect()?,
    })
}

/// Unregisters one texture. On failure NVENC may still reference the
/// texture, so the COM reference is leaked rather than released.
fn unregister(session: &Session, registration: Registration) {
    if let Err(error) = session.unregister(&registration.handle) {
        tracing::warn!(%error, "failed to unregister a texture from NVENC; leaking it");
        core::mem::forget(registration.texture);
    }
}
