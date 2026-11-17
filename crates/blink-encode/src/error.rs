//! Encoder errors.

use crate::{ConfigError, NvencStatus, ParameterSetError};

/// Errors from loading NVENC, configuring a session, and encoding.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EncodeError {
    /// `nvEncodeAPI64.dll` could not be loaded from System32, which means no
    /// NVIDIA driver (or one without NVENC) is installed.
    #[error("could not load nvEncodeAPI64.dll; is an NVIDIA GPU driver installed?")]
    NoNvidiaDriver(#[source] windows::core::Error),
    /// `nvEncodeAPI64.dll` lacks an export blink needs.
    #[error("nvEncodeAPI64.dll does not export {0}")]
    MissingExport(&'static str),
    /// The driver's NVENC API is older than the 12.1 headers blink is built
    /// against.
    #[error(
        "the NVIDIA driver supports NVENC API {driver_major}.{driver_minor}, but blink needs \
         {required_major}.{required_minor} (driver 531.61 or newer); update the GPU driver"
    )]
    DriverTooOld {
        /// Major API version the driver supports.
        driver_major: u32,
        /// Minor API version the driver supports.
        driver_minor: u32,
        /// Major API version blink requires.
        required_major: u32,
        /// Minor API version blink requires.
        required_minor: u32,
    },
    /// `NvEncodeAPICreateInstance` left a function blink needs null.
    #[error("the NVENC function table has no {0}")]
    MissingFunction(&'static str),
    /// An NVENC call returned an error status.
    #[error("{call} failed with {status}: {}", detail.as_deref().unwrap_or("no detail from the driver"))]
    Nvenc {
        /// The NVENC function that failed.
        call: &'static str,
        /// Its status code.
        status: NvencStatus,
        /// `nvEncGetLastErrorString` at the time of the failure, if any.
        detail: Option<String>,
    },
    /// An NVENC call reported success but returned a null handle or pointer.
    #[error("{call} succeeded but returned a null {what}")]
    NullOutput {
        /// The NVENC function.
        call: &'static str,
        /// The output that was null.
        what: &'static str,
    },
    /// The configuration or a reconfigure size was invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The texture's size differs from the encoder's current size; call
    /// [`Encoder::reconfigure`](crate::Encoder::reconfigure) first.
    #[error(
        "texture is {texture_width}x{texture_height} but the encoder is set to \
         {width}x{height}; reconfigure first"
    )]
    TextureSizeMismatch {
        /// Texture width.
        texture_width: u32,
        /// Texture height.
        texture_height: u32,
        /// Encoder width.
        width: u32,
        /// Encoder height.
        height: u32,
    },
    /// The texture is not `DXGI_FORMAT_B8G8R8A8_UNORM`.
    #[error("texture format {0} is not DXGI_FORMAT_B8G8R8A8_UNORM (87)")]
    UnsupportedTextureFormat(i32),
    /// The texture is an array, has mipmaps, or is multisampled; NVENC input
    /// must be a single-sampled 2D subresource.
    #[error(
        "texture has {array_size} array slices, {mip_levels} mip levels, and {sample_count} \
         samples; expected one of each"
    )]
    UnsupportedTextureLayout {
        /// `ArraySize` of the texture.
        array_size: u32,
        /// `MipLevels` of the texture.
        mip_levels: u32,
        /// `SampleDesc.Count` of the texture.
        sample_count: u32,
    },
    /// The sequence header NVENC returned could not be split into SPS and PPS.
    #[error("invalid NVENC sequence header")]
    SequenceHeader(#[from] ParameterSetError),
    /// `nvEncGetSequenceParams` reported a payload larger than the
    /// `NV_MAX_SEQ_HDR_LEN` (512) byte buffer it was given.
    #[error("NVENC reported a {0}-byte sequence header, larger than the 512-byte buffer")]
    SequenceHeaderTooLarge(u32),
}
