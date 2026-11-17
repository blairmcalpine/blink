//! Safe wrappers over one NVENC encode session.
//!
//! Every NVENC call is synchronous on the calling thread. Handles returned by
//! the session (`Bitstream`, `Registered`, `Mapped`) are plain newtypes over
//! the driver's pointers; `Encoder` owns them and releases them before the
//! session is destroyed.

use core::ffi::c_void;
use core::ptr::{NonNull, null_mut};

use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::core::Interface;

use super::api::Api;
use crate::EncodeError;
use crate::ffi::{
    GUID, NV_ENC_BUFFER_FORMAT_ARGB, NV_ENC_CONFIG_VER, NV_ENC_CREATE_BITSTREAM_BUFFER,
    NV_ENC_CREATE_BITSTREAM_BUFFER_VER, NV_ENC_DEVICE_TYPE_DIRECTX, NV_ENC_INITIALIZE_PARAMS,
    NV_ENC_INPUT_IMAGE, NV_ENC_INPUT_PTR, NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX,
    NV_ENC_LOCK_BITSTREAM, NV_ENC_LOCK_BITSTREAM_VER, NV_ENC_MAP_INPUT_RESOURCE,
    NV_ENC_MAP_INPUT_RESOURCE_VER, NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS,
    NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER, NV_ENC_OUTPUT_PTR, NV_ENC_PIC_PARAMS,
    NV_ENC_PIC_PARAMS_VER, NV_ENC_PIC_STRUCT_FRAME, NV_ENC_PIC_TYPE_IDR, NV_ENC_PRESET_CONFIG,
    NV_ENC_PRESET_CONFIG_VER, NV_ENC_RECONFIGURE_PARAMS, NV_ENC_REGISTER_RESOURCE,
    NV_ENC_REGISTER_RESOURCE_VER, NV_ENC_REGISTERED_PTR, NV_ENC_SEQUENCE_PARAM_PAYLOAD,
    NV_ENC_SEQUENCE_PARAM_PAYLOAD_VER, NV_MAX_SEQ_HDR_LEN, NVENCAPI_VERSION, zeroed,
};

/// A bitstream (output) buffer.
pub(super) struct Bitstream(NonNull<c_void>);

/// A registered input texture.
pub(super) struct Registered(NonNull<c_void>);

/// A registered texture mapped for one encode.
pub(super) struct Mapped(NonNull<c_void>);

/// What `lock_bitstream` reports about the encoded picture.
pub(super) struct Picture<'a> {
    pub(super) data: &'a [u8],
    pub(super) idr: bool,
}

/// One frame's input to `encode_picture`.
pub(super) struct PictureInput<'a> {
    pub(super) input: &'a Mapped,
    pub(super) output: &'a Bitstream,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) timestamp: u64,
    pub(super) duration: u64,
    /// `NV_ENC_PIC_FLAGS` for this picture.
    pub(super) flags: u32,
}

/// An open NVENC session on a D3D11 device. Destroyed on drop.
pub(super) struct Session {
    encoder: NonNull<c_void>,
    api: Api,
    /// Kept alive for as long as the session uses it.
    _device: ID3D11Device,
}

impl Session {
    /// `nvEncOpenEncodeSessionEx` with a DirectX device.
    pub(super) fn open(api: Api, device: &ID3D11Device) -> Result<Self, EncodeError> {
        let mut params: NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = zeroed();
        params.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
        params.deviceType = NV_ENC_DEVICE_TYPE_DIRECTX;
        params.device = device.as_raw();
        params.apiVersion = NVENCAPI_VERSION;
        let mut encoder = null_mut();
        // SAFETY: `params` is fully initialized with a live `ID3D11Device`
        // pointer (borrowed for the call; a clone is stored below), and
        // `encoder` is a writable out pointer.
        let status = unsafe { (api.open_encode_session_ex)(&raw mut params, &raw mut encoder) };
        let result = api.check(encoder, "nvEncOpenEncodeSessionEx", status);
        if let Err(error) = result {
            if !encoder.is_null() {
                // SAFETY: nvEncodeAPI.h: "If the creation of encoder session
                // fails, the client must call NvEncDestroyEncoder API before
                // exiting." The handle is not used again.
                let _status = unsafe { (api.destroy_encoder)(encoder) };
            }
            return Err(error);
        }
        let encoder = NonNull::new(encoder).ok_or(EncodeError::NullOutput {
            call: "nvEncOpenEncodeSessionEx",
            what: "encoder",
        })?;
        Ok(Self {
            encoder,
            api,
            _device: device.clone(),
        })
    }

    const fn handle(&self) -> *mut c_void {
        self.encoder.as_ptr()
    }

    fn check(&self, call: &'static str, status: i32) -> Result<(), EncodeError> {
        self.api.check(self.handle(), call, status)
    }

    /// `nvEncGetEncodePresetConfigEx` for H.264, boxed (5 KB).
    pub(super) fn preset_config(
        &self,
        codec: GUID,
        preset: GUID,
        tuning: u32,
    ) -> Result<Box<NV_ENC_PRESET_CONFIG>, EncodeError> {
        let mut config: Box<NV_ENC_PRESET_CONFIG> = Box::new(zeroed());
        config.version = NV_ENC_PRESET_CONFIG_VER;
        config.presetCfg.version = NV_ENC_CONFIG_VER;
        // SAFETY: live session, versions set as the API requires, and the
        // boxed struct is writable for its full size.
        let status = unsafe {
            (self.api.get_encode_preset_config_ex)(
                self.handle(),
                codec,
                preset,
                tuning,
                &raw mut *config,
            )
        };
        self.check("nvEncGetEncodePresetConfigEx", status)?;
        Ok(config)
    }

    /// `nvEncInitializeEncoder`. `params.encodeConfig` must point at a live
    /// config for the duration of the call.
    pub(super) fn initialize(
        &self,
        params: &mut NV_ENC_INITIALIZE_PARAMS,
    ) -> Result<(), EncodeError> {
        // SAFETY: live session; `params` is a versioned initialize struct
        // whose `encodeConfig` the caller keeps alive across the call.
        let status = unsafe { (self.api.initialize_encoder)(self.handle(), params) };
        self.check("nvEncInitializeEncoder", status)
    }

    /// `nvEncReconfigureEncoder`, with the same pointer requirement as
    /// `initialize`.
    pub(super) fn reconfigure(
        &self,
        params: &mut NV_ENC_RECONFIGURE_PARAMS,
    ) -> Result<(), EncodeError> {
        // SAFETY: as for `initialize`.
        let status = unsafe { (self.api.reconfigure_encoder)(self.handle(), params) };
        self.check("nvEncReconfigureEncoder", status)
    }

    /// `nvEncCreateBitstreamBuffer`. The driver sizes the buffer for the
    /// largest picture at the maximum encode size.
    pub(super) fn create_bitstream(&self) -> Result<Bitstream, EncodeError> {
        let mut params: NV_ENC_CREATE_BITSTREAM_BUFFER = zeroed();
        params.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
        // SAFETY: live, initialized session; versioned zeroed params.
        let status = unsafe { (self.api.create_bitstream_buffer)(self.handle(), &raw mut params) };
        self.check("nvEncCreateBitstreamBuffer", status)?;
        NonNull::new(params.bitstreamBuffer)
            .map(Bitstream)
            .ok_or(EncodeError::NullOutput {
                call: "nvEncCreateBitstreamBuffer",
                what: "bitstreamBuffer",
            })
    }

    /// `nvEncDestroyBitstreamBuffer`. The handle must not be used afterwards.
    pub(super) fn destroy_bitstream(&self, bitstream: &Bitstream) -> Result<(), EncodeError> {
        let buffer: NV_ENC_OUTPUT_PTR = bitstream.0.as_ptr();
        // SAFETY: the buffer was created on this session and is not locked.
        let status = unsafe { (self.api.destroy_bitstream_buffer)(self.handle(), buffer) };
        self.check("nvEncDestroyBitstreamBuffer", status)
    }

    /// `nvEncRegisterResource` for a BGRA D3D11 texture.
    ///
    /// `NV_ENC_BUFFER_FORMAT_ARGB` is word-ordered with blue in the low byte,
    /// so its memory order is B, G, R, A: `DXGI_FORMAT_B8G8R8A8_UNORM`.
    pub(super) fn register(
        &self,
        texture: &ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Result<Registered, EncodeError> {
        let mut params: NV_ENC_REGISTER_RESOURCE = zeroed();
        params.version = NV_ENC_REGISTER_RESOURCE_VER;
        params.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
        params.width = width;
        params.height = height;
        // Ignored for DirectX resources.
        params.pitch = 0;
        params.subResourceIndex = 0;
        params.resourceToRegister = texture.as_raw();
        params.bufferFormat = NV_ENC_BUFFER_FORMAT_ARGB;
        params.bufferUsage = NV_ENC_INPUT_IMAGE;
        // SAFETY: live session; the texture pointer is live for the call, and
        // the caller keeps a reference until the resource is unregistered.
        let status = unsafe { (self.api.register_resource)(self.handle(), &raw mut params) };
        self.check("nvEncRegisterResource", status)?;
        NonNull::new(params.registeredResource)
            .map(Registered)
            .ok_or(EncodeError::NullOutput {
                call: "nvEncRegisterResource",
                what: "registeredResource",
            })
    }

    /// `nvEncUnregisterResource`. The handle must not be used afterwards.
    pub(super) fn unregister(&self, registered: &Registered) -> Result<(), EncodeError> {
        let resource: NV_ENC_REGISTERED_PTR = registered.0.as_ptr();
        // SAFETY: the resource was registered on this session and is not mapped.
        let status = unsafe { (self.api.unregister_resource)(self.handle(), resource) };
        self.check("nvEncUnregisterResource", status)
    }

    /// `nvEncMapInputResource`.
    pub(super) fn map(&self, registered: &Registered) -> Result<Mapped, EncodeError> {
        let mut params: NV_ENC_MAP_INPUT_RESOURCE = zeroed();
        params.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
        params.registeredResource = registered.0.as_ptr();
        // SAFETY: live session; the resource is registered and not mapped.
        let status = unsafe { (self.api.map_input_resource)(self.handle(), &raw mut params) };
        self.check("nvEncMapInputResource", status)?;
        NonNull::new(params.mappedResource)
            .map(Mapped)
            .ok_or(EncodeError::NullOutput {
                call: "nvEncMapInputResource",
                what: "mappedResource",
            })
    }

    /// `nvEncUnmapInputResource`. The handle must not be used afterwards.
    pub(super) fn unmap(&self, mapped: &Mapped) -> Result<(), EncodeError> {
        let input: NV_ENC_INPUT_PTR = mapped.0.as_ptr();
        // SAFETY: the input was mapped on this session and its encode has
        // completed (the bitstream was locked, which waits for it).
        let status = unsafe { (self.api.unmap_input_resource)(self.handle(), input) };
        self.check("nvEncUnmapInputResource", status)
    }

    /// `nvEncEncodePicture` for one progressive frame.
    ///
    /// With `enableEncodeAsync = 0`, no B-frames, and no lookahead, NVENC
    /// emits a picture for every input, so `NV_ENC_ERR_NEED_MORE_INPUT` is
    /// reported as an error rather than buffered.
    pub(super) fn encode_picture(&self, picture: &PictureInput<'_>) -> Result<(), EncodeError> {
        let mut params: NV_ENC_PIC_PARAMS = zeroed();
        params.version = NV_ENC_PIC_PARAMS_VER;
        params.inputWidth = picture.width;
        params.inputHeight = picture.height;
        params.inputPitch = picture.width;
        params.encodePicFlags = picture.flags;
        params.inputTimeStamp = picture.timestamp;
        params.inputDuration = picture.duration;
        params.inputBuffer = picture.input.0.as_ptr();
        params.outputBitstream = picture.output.0.as_ptr();
        params.bufferFmt = NV_ENC_BUFFER_FORMAT_ARGB;
        params.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
        // SAFETY: live session; the input is mapped and the output buffer is
        // unlocked; both belong to this session.
        let status = unsafe { (self.api.encode_picture)(self.handle(), &raw mut params) };
        self.check("nvEncEncodePicture", status)
    }

    /// Locks the bitstream (waiting for the encode to finish), passes the
    /// picture to `read`, and unlocks it again.
    pub(super) fn read_bitstream<R>(
        &self,
        bitstream: &Bitstream,
        read: impl FnOnce(Picture<'_>) -> R,
    ) -> Result<R, EncodeError> {
        let mut params: NV_ENC_LOCK_BITSTREAM = zeroed();
        params.version = NV_ENC_LOCK_BITSTREAM_VER;
        params.outputBitstream = bitstream.0.as_ptr();
        // SAFETY: live session; the buffer belongs to it and is not locked.
        let status = unsafe { (self.api.lock_bitstream)(self.handle(), &raw mut params) };
        self.check("nvEncLockBitstream", status)?;

        let result = locked_bytes(&params).map(|data| {
            read(Picture {
                data,
                idr: params.pictureType == NV_ENC_PIC_TYPE_IDR,
            })
        });

        // SAFETY: the buffer was locked above, and the slice into it was
        // consumed by `read`, which cannot retain it past this point.
        let status = unsafe { (self.api.unlock_bitstream)(self.handle(), bitstream.0.as_ptr()) };
        self.check("nvEncUnlockBitstream", status)?;
        result
    }

    /// `nvEncGetSequenceParams`: the Annex B SPS and PPS for the current
    /// configuration, written into `buffer`.
    pub(super) fn sequence_params<'b>(
        &self,
        buffer: &'b mut [u8; NV_MAX_SEQ_HDR_LEN],
    ) -> Result<&'b [u8], EncodeError> {
        let mut size = 0_u32;
        let mut params: NV_ENC_SEQUENCE_PARAM_PAYLOAD = zeroed();
        params.version = NV_ENC_SEQUENCE_PARAM_PAYLOAD_VER;
        params.inBufferSize = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        params.spsppsBuffer = buffer.as_mut_ptr().cast();
        params.outSPSPPSPayloadSize = &raw mut size;
        // SAFETY: live, initialized session; `spsppsBuffer` points at
        // `inBufferSize` writable bytes and the size pointer is writable.
        let status = unsafe { (self.api.get_sequence_params)(self.handle(), &raw mut params) };
        self.check("nvEncGetSequenceParams", status)?;
        let len = usize::try_from(size).unwrap_or(usize::MAX);
        buffer
            .get(..len)
            .ok_or(EncodeError::SequenceHeaderTooLarge(size))
    }
}

/// The locked bitstream as a byte slice.
fn locked_bytes(params: &NV_ENC_LOCK_BITSTREAM) -> Result<&[u8], EncodeError> {
    let len = usize::try_from(params.bitstreamSizeInBytes).unwrap_or(usize::MAX);
    if len == 0 {
        return Ok(&[]);
    }
    let data = params.bitstreamBufferPtr.cast::<u8>();
    if data.is_null() {
        return Err(EncodeError::NullOutput {
            call: "nvEncLockBitstream",
            what: "bitstreamBufferPtr",
        });
    }
    // SAFETY: while the bitstream is locked NVENC guarantees
    // `bitstreamBufferPtr` points at `bitstreamSizeInBytes` initialized bytes
    // that nothing else writes; the slice borrows `params`, and the caller
    // drops it before unlocking.
    Ok(unsafe { core::slice::from_raw_parts(data, len) })
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the session is live and destroyed exactly once, here. The
        // owning `Encoder` has already unregistered its resources and
        // destroyed its bitstream buffers.
        let status = unsafe { (self.api.destroy_encoder)(self.handle()) };
        if status != crate::ffi::NV_ENC_SUCCESS {
            tracing::warn!(
                status = %crate::NvencStatus::from_raw(status),
                "nvEncDestroyEncoder failed"
            );
        }
    }
}
