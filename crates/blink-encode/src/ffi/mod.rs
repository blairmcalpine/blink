//! Hand-written bindings for the subset of `nvEncodeAPI.h` that blink uses.
//!
//! Source: `include/ffnvcodec/nvEncodeAPI.h` from `FFmpeg`'s `nv-codec-headers`,
//! tag `n12.1.14.0` (commit `1889e62e2d35ff7aa9baca2bceb14f053785e6f1`; the file
//! is byte-identical in `n12.1.14.1`). That header declares
//! `NVENCAPI_MAJOR_VERSION 12` and `NVENCAPI_MINOR_VERSION 1` (Video Codec SDK
//! 12.1) and carries NVIDIA's MIT license: "Copyright (c) 2010-2023 NVIDIA
//! Corporation", permission to use, copy, modify, merge, publish, distribute,
//! sublicense, and/or sell, provided the notice is kept. See `README.md` next to
//! this file for the full notice. Minimum driver for API 12.1: 531.61 on
//! Windows.
//!
//! Only declarations are transcribed; no NVIDIA code is vendored or linked.
//! Names mirror the header so each item can be audited against it. Every
//! struct is the full C layout, reserved arrays included, and its size (plus
//! the offset of every field blink reads or writes) is checked at compile time
//! against `clang --target=x86_64-pc-windows-msvc -Xclang
//! -fdump-record-layouts` run on the header. Deviations from the header:
//!
//! - C `enum` fields are `u32` (MSVC enums are 4-byte `int`; every value used
//!   is non-negative). `NVENCSTATUS` is `i32` so unexpected values stay
//!   representable.
//! - C bit-fields are one `u32` named `bitFields`; the `*_BIT` constants give
//!   each flag's position (MSVC allocates `uint32_t` bit-fields from the least
//!   significant bit).
//! - The `NV_ENC_CODEC_CONFIG` union is a struct holding only `h264Config`. H.264
//!   is the largest member (1792 bytes, 8-byte aligned), so size, alignment, and
//!   offset are unchanged.
//! - The `NV_ENC_CODEC_PIC_PARAMS` union is an opaque, zeroed `[u64; 194]` (1552
//!   bytes, 8-byte aligned): blink sets no per-picture codec parameters.
//! - Function pointers blink never calls are `*mut c_void`.

#![expect(
    non_camel_case_types,
    non_snake_case,
    clippy::upper_case_acronyms,
    reason = "names mirror nvEncodeAPI.h so the bindings can be audited against it"
)]
#![expect(
    dead_code,
    reason = "structs mirror the full C layout; reserved and unused fields only hold offsets"
)]

use core::ffi::{c_char, c_void};
use core::mem::{align_of, offset_of, size_of};

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

/// `NVENCAPI_MAJOR_VERSION`.
pub(crate) const NVENCAPI_MAJOR_VERSION: u32 = 12;
/// `NVENCAPI_MINOR_VERSION`.
pub(crate) const NVENCAPI_MINOR_VERSION: u32 = 1;
/// `NVENCAPI_VERSION`: `major | (minor << 24)`.
pub(crate) const NVENCAPI_VERSION: u32 = NVENCAPI_MAJOR_VERSION | (NVENCAPI_MINOR_VERSION << 24);

/// `NVENCAPI_STRUCT_VERSION(ver)`.
const fn nvencapi_struct_version(ver: u32) -> u32 {
    NVENCAPI_VERSION | (ver << 16) | (0x7 << 28)
}

/// The `| (1u << 31)` some struct version macros add.
const STRUCT_VERSION_HIGH_BIT: u32 = 1 << 31;

pub(crate) const NV_ENCODE_API_FUNCTION_LIST_VER: u32 = nvencapi_struct_version(2);
pub(crate) const NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER: u32 = nvencapi_struct_version(1);
pub(crate) const NV_ENC_PRESET_CONFIG_VER: u32 =
    nvencapi_struct_version(4) | STRUCT_VERSION_HIGH_BIT;
pub(crate) const NV_ENC_CONFIG_VER: u32 = nvencapi_struct_version(8) | STRUCT_VERSION_HIGH_BIT;
pub(crate) const NV_ENC_RC_PARAMS_VER: u32 = nvencapi_struct_version(1);
pub(crate) const NV_ENC_INITIALIZE_PARAMS_VER: u32 =
    nvencapi_struct_version(6) | STRUCT_VERSION_HIGH_BIT;
pub(crate) const NV_ENC_RECONFIGURE_PARAMS_VER: u32 =
    nvencapi_struct_version(1) | STRUCT_VERSION_HIGH_BIT;
pub(crate) const NV_ENC_CREATE_BITSTREAM_BUFFER_VER: u32 = nvencapi_struct_version(1);
pub(crate) const NV_ENC_REGISTER_RESOURCE_VER: u32 = nvencapi_struct_version(4);
pub(crate) const NV_ENC_MAP_INPUT_RESOURCE_VER: u32 = nvencapi_struct_version(4);
pub(crate) const NV_ENC_PIC_PARAMS_VER: u32 = nvencapi_struct_version(6) | STRUCT_VERSION_HIGH_BIT;
pub(crate) const NV_ENC_LOCK_BITSTREAM_VER: u32 =
    nvencapi_struct_version(1) | STRUCT_VERSION_HIGH_BIT;
pub(crate) const NV_ENC_SEQUENCE_PARAM_PAYLOAD_VER: u32 = nvencapi_struct_version(1);

/// `NV_MAX_SEQ_HDR_LEN`: buffer size for `NvEncGetSequenceParams`.
pub(crate) const NV_MAX_SEQ_HDR_LEN: usize = 512;

/// `NVENC_INFINITE_GOPLENGTH`: no automatic keyframes. Requires
/// `frameIntervalP = 1`.
pub(crate) const NVENC_INFINITE_GOPLENGTH: u32 = 0xffff_ffff;

// ---------------------------------------------------------------------------
// GUIDs
// ---------------------------------------------------------------------------

/// `GUID` (the header's non-Windows definition, identical in layout to the
/// Windows one).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GUID {
    pub(crate) Data1: u32,
    pub(crate) Data2: u16,
    pub(crate) Data3: u16,
    pub(crate) Data4: [u8; 8],
}

/// `NV_ENC_CODEC_H264_GUID` {6BC82762-4E63-4ca4-AA85-1E50F321F6BF}.
pub(crate) const NV_ENC_CODEC_H264_GUID: GUID = GUID {
    Data1: 0x6bc8_2762,
    Data2: 0x4e63,
    Data3: 0x4ca4,
    Data4: [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
};

/// `NV_ENC_H264_PROFILE_HIGH_GUID` {E7CBC309-4F7A-4b89-AF2A-D537C92BE310}.
pub(crate) const NV_ENC_H264_PROFILE_HIGH_GUID: GUID = GUID {
    Data1: 0xe7cb_c309,
    Data2: 0x4f7a,
    Data3: 0x4b89,
    Data4: [0xaf, 0x2a, 0xd5, 0x37, 0xc9, 0x2b, 0xe3, 0x10],
};

/// `NV_ENC_PRESET_P1_GUID` {FC0A8D3E-45F8-4CF8-80C7-298871590EBF}.
pub(crate) const NV_ENC_PRESET_P1_GUID: GUID = GUID {
    Data1: 0xfc0a_8d3e,
    Data2: 0x45f8,
    Data3: 0x4cf8,
    Data4: [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf],
};

/// `NV_ENC_PRESET_P2_GUID` {F581CFB8-88D6-4381-93F0-DF13F9C27DAB}.
pub(crate) const NV_ENC_PRESET_P2_GUID: GUID = GUID {
    Data1: 0xf581_cfb8,
    Data2: 0x88d6,
    Data3: 0x4381,
    Data4: [0x93, 0xf0, 0xdf, 0x13, 0xf9, 0xc2, 0x7d, 0xab],
};

/// `NV_ENC_PRESET_P3_GUID` {36850110-3A07-441F-94D5-3670631F91F6}.
pub(crate) const NV_ENC_PRESET_P3_GUID: GUID = GUID {
    Data1: 0x3685_0110,
    Data2: 0x3a07,
    Data3: 0x441f,
    Data4: [0x94, 0xd5, 0x36, 0x70, 0x63, 0x1f, 0x91, 0xf6],
};

// ---------------------------------------------------------------------------
// Enum values
// ---------------------------------------------------------------------------

/// `NVENCSTATUS`.
pub(crate) type NVENCSTATUS = i32;
/// `NV_ENC_SUCCESS`.
pub(crate) const NV_ENC_SUCCESS: NVENCSTATUS = 0;

/// `NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME`.
pub(crate) const NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME: u32 = 0x01;
/// `NV_ENC_PARAMS_RC_CBR`.
pub(crate) const NV_ENC_PARAMS_RC_CBR: u32 = 0x2;
/// `NV_ENC_MULTI_PASS_DISABLED`.
pub(crate) const NV_ENC_MULTI_PASS_DISABLED: u32 = 0x0;
/// `NV_ENC_PIC_STRUCT_FRAME`.
pub(crate) const NV_ENC_PIC_STRUCT_FRAME: u32 = 0x01;
/// `NV_ENC_PIC_TYPE_IDR`.
pub(crate) const NV_ENC_PIC_TYPE_IDR: u32 = 0x03;
/// `NV_ENC_PIC_FLAG_FORCEIDR`: encode this picture as an IDR (honoured with
/// `enablePTD = 1`).
pub(crate) const NV_ENC_PIC_FLAG_FORCEIDR: u32 = 0x2;
/// `NV_ENC_PIC_FLAG_OUTPUT_SPSPPS`: write SPS and PPS with this picture.
pub(crate) const NV_ENC_PIC_FLAG_OUTPUT_SPSPPS: u32 = 0x4;
/// `NV_ENC_BUFFER_FORMAT_ARGB`: "8 bit Packed A8R8G8B8. This is a word-ordered
/// format where a pixel is represented by a 32-bit word with B in the lowest 8
/// bits, G in the next 8 bits, R in the 8 bits after that and A in the highest 8
/// bits." On little-endian memory that is bytes B, G, R, A, which is
/// `DXGI_FORMAT_B8G8R8A8_UNORM`.
pub(crate) const NV_ENC_BUFFER_FORMAT_ARGB: u32 = 0x0100_0000;
/// `NV_ENC_LEVEL_AUTOSELECT`.
pub(crate) const NV_ENC_LEVEL_AUTOSELECT: u32 = 0;
/// `NV_ENC_BFRAME_REF_MODE_DISABLED`.
pub(crate) const NV_ENC_BFRAME_REF_MODE_DISABLED: u32 = 0x0;
/// `NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX`.
pub(crate) const NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX: u32 = 0x0;
/// `NV_ENC_INPUT_IMAGE` (`NV_ENC_BUFFER_USAGE`).
pub(crate) const NV_ENC_INPUT_IMAGE: u32 = 0x0;
/// `NV_ENC_DEVICE_TYPE_DIRECTX`.
pub(crate) const NV_ENC_DEVICE_TYPE_DIRECTX: u32 = 0x0;
/// `NV_ENC_TUNING_INFO_LOW_LATENCY`.
pub(crate) const NV_ENC_TUNING_INFO_LOW_LATENCY: u32 = 2;
/// `NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY`.
pub(crate) const NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY: u32 = 3;
/// `NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED`.
pub(crate) const NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED: u32 = 5;
/// `NV_ENC_VUI_COLOR_PRIMARIES_BT709`.
pub(crate) const NV_ENC_VUI_COLOR_PRIMARIES_BT709: u32 = 1;
/// `NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709`.
pub(crate) const NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709: u32 = 1;
/// `NV_ENC_VUI_MATRIX_COEFFS_BT709`.
pub(crate) const NV_ENC_VUI_MATRIX_COEFFS_BT709: u32 = 1;

// ---------------------------------------------------------------------------
// Bit-field positions
// ---------------------------------------------------------------------------

/// `NV_ENC_CONFIG_H264::outputAUD` (dump: byte 0 bit 6).
pub(crate) const H264_OUTPUT_AUD_BIT: u32 = 1 << 6;
/// `NV_ENC_CONFIG_H264::disableSPSPPS` (byte 0 bit 7).
pub(crate) const H264_DISABLE_SPSPPS_BIT: u32 = 1 << 7;
/// `NV_ENC_CONFIG_H264::hierarchicalPFrames` (byte 0 bit 2).
pub(crate) const H264_HIERARCHICAL_P_FRAMES_BIT: u32 = 1 << 2;
/// `NV_ENC_CONFIG_H264::hierarchicalBFrames` (byte 0 bit 3).
pub(crate) const H264_HIERARCHICAL_B_FRAMES_BIT: u32 = 1 << 3;
/// `NV_ENC_CONFIG_H264::enableIntraRefresh` (byte 1 bit 2).
pub(crate) const H264_ENABLE_INTRA_REFRESH_BIT: u32 = 1 << 10;
/// `NV_ENC_CONFIG_H264::repeatSPSPPS` (byte 1 bit 4).
pub(crate) const H264_REPEAT_SPSPPS_BIT: u32 = 1 << 12;
/// `NV_ENC_CONFIG_H264::enableVFR` (byte 1 bit 5): clears
/// `fixed_frame_rate_flag` in the VUI.
pub(crate) const H264_ENABLE_VFR_BIT: u32 = 1 << 13;
/// `NV_ENC_CONFIG_H264::enableLTR` (byte 1 bit 6).
pub(crate) const H264_ENABLE_LTR_BIT: u32 = 1 << 14;

/// `NV_ENC_RC_PARAMS::enableAQ` (dump: byte 36 bit 3, so bit 3 of `bitFields`).
pub(crate) const RC_ENABLE_AQ_BIT: u32 = 1 << 3;
/// `NV_ENC_RC_PARAMS::enableLookahead` (byte 36 bit 5).
pub(crate) const RC_ENABLE_LOOKAHEAD_BIT: u32 = 1 << 5;
/// `NV_ENC_RC_PARAMS::enableTemporalAQ` (byte 37 bit 0).
pub(crate) const RC_ENABLE_TEMPORAL_AQ_BIT: u32 = 1 << 8;
/// `NV_ENC_RC_PARAMS::aqStrength` (byte 37 bits 4-7, a 4-bit field).
pub(crate) const RC_AQ_STRENGTH_MASK: u32 = 0xF << 12;
/// `NV_ENC_RC_PARAMS::enableExtLookahead` (byte 38 bit 0).
pub(crate) const RC_ENABLE_EXT_LOOKAHEAD_BIT: u32 = 1 << 16;

/// `NV_ENC_RECONFIGURE_PARAMS::resetEncoder` (dump: byte 1816 bit 0).
pub(crate) const RECONFIGURE_RESET_ENCODER_BIT: u32 = 1 << 0;
/// `NV_ENC_RECONFIGURE_PARAMS::forceIDR` (byte 1816 bit 1).
pub(crate) const RECONFIGURE_FORCE_IDR_BIT: u32 = 1 << 1;

// ---------------------------------------------------------------------------
// Opaque handles
// ---------------------------------------------------------------------------

/// `NV_ENC_INPUT_PTR`.
pub(crate) type NV_ENC_INPUT_PTR = *mut c_void;
/// `NV_ENC_OUTPUT_PTR`.
pub(crate) type NV_ENC_OUTPUT_PTR = *mut c_void;
/// `NV_ENC_REGISTERED_PTR`.
pub(crate) type NV_ENC_REGISTERED_PTR = *mut c_void;

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

/// Types for which the all-zero bit pattern is a valid value.
///
/// # Safety
///
/// Implementors must consist only of integers, raw pointers, `Option` of
/// function pointers, and arrays or structs of those, so that
/// `core::mem::zeroed` produces a valid value. Every NVENC parameter struct
/// is meant to be zero-initialized before the fields in use are set.
pub(crate) unsafe trait Zeroable: Sized {}

/// Returns an all-zero `T`, the starting point for every NVENC parameter struct.
pub(crate) const fn zeroed<T: Zeroable>() -> T {
    // SAFETY: `Zeroable` guarantees the all-zero bit pattern is a valid `T`.
    unsafe { core::mem::zeroed() }
}

/// `NV_ENC_QP`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct NV_ENC_QP {
    pub(crate) qpInterP: u32,
    pub(crate) qpInterB: u32,
    pub(crate) qpIntra: u32,
}

/// `NV_ENC_RC_PARAMS`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct NV_ENC_RC_PARAMS {
    pub(crate) version: u32,
    pub(crate) rateControlMode: u32,
    pub(crate) constQP: NV_ENC_QP,
    pub(crate) averageBitRate: u32,
    pub(crate) maxBitRate: u32,
    pub(crate) vbvBufferSize: u32,
    pub(crate) vbvInitialDelay: u32,
    /// enableMinQP:1 .. enableExtLookahead:1, reservedBitFields:15.
    pub(crate) bitFields: u32,
    pub(crate) minQP: NV_ENC_QP,
    pub(crate) maxQP: NV_ENC_QP,
    pub(crate) initialRCQP: NV_ENC_QP,
    pub(crate) temporallayerIdxMask: u32,
    pub(crate) temporalLayerQP: [u8; 8],
    pub(crate) targetQuality: u8,
    pub(crate) targetQualityLSB: u8,
    pub(crate) lookaheadDepth: u16,
    pub(crate) lowDelayKeyFrameScale: u8,
    pub(crate) yDcQPIndexOffset: i8,
    pub(crate) uDcQPIndexOffset: i8,
    pub(crate) vDcQPIndexOffset: i8,
    pub(crate) qpMapMode: u32,
    pub(crate) multiPass: u32,
    pub(crate) alphaLayerBitrateRatio: u32,
    pub(crate) cbQPIndexOffset: i8,
    pub(crate) crQPIndexOffset: i8,
    pub(crate) reserved2: u16,
    pub(crate) reserved: [u32; 4],
}

/// `NV_ENC_CONFIG_H264_VUI_PARAMETERS`.
///
/// There is no aspect-ratio field in SDK 12.1; the sample aspect ratio is
/// derived by NVENC from `NV_ENC_INITIALIZE_PARAMS::darWidth`/`darHeight`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct NV_ENC_CONFIG_H264_VUI_PARAMETERS {
    pub(crate) overscanInfoPresentFlag: u32,
    pub(crate) overscanInfo: u32,
    pub(crate) videoSignalTypePresentFlag: u32,
    pub(crate) videoFormat: u32,
    pub(crate) videoFullRangeFlag: u32,
    pub(crate) colourDescriptionPresentFlag: u32,
    pub(crate) colourPrimaries: u32,
    pub(crate) transferCharacteristics: u32,
    pub(crate) colourMatrix: u32,
    pub(crate) chromaSampleLocationFlag: u32,
    pub(crate) chromaSampleLocationTop: u32,
    pub(crate) chromaSampleLocationBot: u32,
    pub(crate) bitstreamRestrictionFlag: u32,
    pub(crate) timingInfoPresentFlag: u32,
    pub(crate) numUnitInTicks: u32,
    pub(crate) timeScale: u32,
    pub(crate) reserved: [u32; 12],
}

/// `NV_ENC_CONFIG_H264`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct NV_ENC_CONFIG_H264 {
    /// enableTemporalSVC:1 .. enableTimeCode:1, reservedBitFields:10.
    pub(crate) bitFields: u32,
    pub(crate) level: u32,
    pub(crate) idrPeriod: u32,
    pub(crate) separateColourPlaneFlag: u32,
    pub(crate) disableDeblockingFilterIDC: u32,
    pub(crate) numTemporalLayers: u32,
    pub(crate) spsId: u32,
    pub(crate) ppsId: u32,
    pub(crate) adaptiveTransformMode: u32,
    pub(crate) fmoMode: u32,
    pub(crate) bdirectMode: u32,
    pub(crate) entropyCodingMode: u32,
    pub(crate) stereoMode: u32,
    pub(crate) intraRefreshPeriod: u32,
    pub(crate) intraRefreshCnt: u32,
    pub(crate) maxNumRefFrames: u32,
    pub(crate) sliceMode: u32,
    pub(crate) sliceModeData: u32,
    pub(crate) h264VUIParameters: NV_ENC_CONFIG_H264_VUI_PARAMETERS,
    pub(crate) ltrNumFrames: u32,
    pub(crate) ltrTrustMode: u32,
    pub(crate) chromaFormatIDC: u32,
    pub(crate) maxTemporalLayers: u32,
    pub(crate) useBFramesAsRef: u32,
    pub(crate) numRefL0: u32,
    pub(crate) numRefL1: u32,
    pub(crate) reserved1: [u32; 267],
    pub(crate) reserved2: [*mut c_void; 64],
}

/// `NV_ENC_CODEC_CONFIG`, reduced to its H.264 member (see the module docs).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct NV_ENC_CODEC_CONFIG {
    pub(crate) h264Config: NV_ENC_CONFIG_H264,
}

/// `NV_ENC_CONFIG`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct NV_ENC_CONFIG {
    pub(crate) version: u32,
    pub(crate) profileGUID: GUID,
    pub(crate) gopLength: u32,
    pub(crate) frameIntervalP: i32,
    pub(crate) monoChromeEncoding: u32,
    pub(crate) frameFieldMode: u32,
    pub(crate) mvPrecision: u32,
    pub(crate) rcParams: NV_ENC_RC_PARAMS,
    pub(crate) encodeCodecConfig: NV_ENC_CODEC_CONFIG,
    pub(crate) reserved: [u32; 278],
    pub(crate) reserved2: [*mut c_void; 64],
}

/// `NV_ENC_PRESET_CONFIG`.
#[repr(C)]
pub(crate) struct NV_ENC_PRESET_CONFIG {
    pub(crate) version: u32,
    pub(crate) presetCfg: NV_ENC_CONFIG,
    pub(crate) reserved1: [u32; 255],
    pub(crate) reserved2: [*mut c_void; 64],
}

/// `NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE {
    /// numCandsPerBlk16x16:4 .. numCandsPerSb:8, reserved:8.
    pub(crate) bitFields: u32,
    pub(crate) reserved1: [u32; 3],
}

/// `NV_ENC_INITIALIZE_PARAMS`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct NV_ENC_INITIALIZE_PARAMS {
    pub(crate) version: u32,
    pub(crate) encodeGUID: GUID,
    pub(crate) presetGUID: GUID,
    pub(crate) encodeWidth: u32,
    pub(crate) encodeHeight: u32,
    pub(crate) darWidth: u32,
    pub(crate) darHeight: u32,
    pub(crate) frameRateNum: u32,
    pub(crate) frameRateDen: u32,
    pub(crate) enableEncodeAsync: u32,
    pub(crate) enablePTD: u32,
    /// reportSliceOffsets:1 .. enableOutputStats:1, reservedBitFields:20.
    pub(crate) bitFields: u32,
    pub(crate) privDataSize: u32,
    pub(crate) privData: *mut c_void,
    pub(crate) encodeConfig: *mut NV_ENC_CONFIG,
    pub(crate) maxEncodeWidth: u32,
    pub(crate) maxEncodeHeight: u32,
    pub(crate) maxMEHintCountsPerBlock: [NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE; 2],
    pub(crate) tuningInfo: u32,
    pub(crate) bufferFormat: u32,
    pub(crate) numStateBuffers: u32,
    pub(crate) outputStatsLevel: u32,
    pub(crate) reserved: [u32; 285],
    pub(crate) reserved2: [*mut c_void; 64],
}

/// `NV_ENC_RECONFIGURE_PARAMS`.
#[repr(C)]
pub(crate) struct NV_ENC_RECONFIGURE_PARAMS {
    pub(crate) version: u32,
    pub(crate) reInitEncodeParams: NV_ENC_INITIALIZE_PARAMS,
    /// resetEncoder:1, forceIDR:1, reserved:30.
    pub(crate) bitFields: u32,
}

/// `NV_ENC_CREATE_BITSTREAM_BUFFER`.
#[repr(C)]
pub(crate) struct NV_ENC_CREATE_BITSTREAM_BUFFER {
    pub(crate) version: u32,
    /// Deprecated; must be 0.
    pub(crate) size: u32,
    /// Deprecated; must be 0.
    pub(crate) memoryHeap: u32,
    pub(crate) reserved: u32,
    pub(crate) bitstreamBuffer: NV_ENC_OUTPUT_PTR,
    pub(crate) bitstreamBufferPtr: *mut c_void,
    pub(crate) reserved1: [u32; 58],
    pub(crate) reserved2: [*mut c_void; 64],
}

/// `NV_ENC_REGISTER_RESOURCE`.
#[repr(C)]
pub(crate) struct NV_ENC_REGISTER_RESOURCE {
    pub(crate) version: u32,
    pub(crate) resourceType: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pitch: u32,
    pub(crate) subResourceIndex: u32,
    pub(crate) resourceToRegister: *mut c_void,
    pub(crate) registeredResource: NV_ENC_REGISTERED_PTR,
    pub(crate) bufferFormat: u32,
    pub(crate) bufferUsage: u32,
    /// `NV_ENC_FENCE_POINT_D3D12*`; D3D12 only, null for D3D11.
    pub(crate) pInputFencePoint: *mut c_void,
    pub(crate) chromaOffset: [u32; 2],
    pub(crate) reserved1: [u32; 245],
    pub(crate) reserved2: [*mut c_void; 61],
}

/// `NV_ENC_MAP_INPUT_RESOURCE`.
#[repr(C)]
pub(crate) struct NV_ENC_MAP_INPUT_RESOURCE {
    pub(crate) version: u32,
    /// Deprecated.
    pub(crate) subResourceIndex: u32,
    /// Deprecated.
    pub(crate) inputResource: *mut c_void,
    pub(crate) registeredResource: NV_ENC_REGISTERED_PTR,
    pub(crate) mappedResource: NV_ENC_INPUT_PTR,
    pub(crate) mappedBufferFmt: u32,
    pub(crate) reserved1: [u32; 251],
    pub(crate) reserved2: [*mut c_void; 63],
}

/// `NV_ENC_PIC_PARAMS`.
#[repr(C)]
pub(crate) struct NV_ENC_PIC_PARAMS {
    pub(crate) version: u32,
    pub(crate) inputWidth: u32,
    pub(crate) inputHeight: u32,
    pub(crate) inputPitch: u32,
    pub(crate) encodePicFlags: u32,
    pub(crate) frameIdx: u32,
    pub(crate) inputTimeStamp: u64,
    pub(crate) inputDuration: u64,
    pub(crate) inputBuffer: NV_ENC_INPUT_PTR,
    pub(crate) outputBitstream: NV_ENC_OUTPUT_PTR,
    pub(crate) completionEvent: *mut c_void,
    pub(crate) bufferFmt: u32,
    pub(crate) pictureStruct: u32,
    pub(crate) pictureType: u32,
    /// The `NV_ENC_CODEC_PIC_PARAMS` union, left zeroed (see the module docs).
    pub(crate) codecPicParams: [u64; 194],
    pub(crate) meHintCountsPerBlock: [NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE; 2],
    /// `NVENC_EXTERNAL_ME_HINT*`.
    pub(crate) meExternalHints: *mut c_void,
    pub(crate) reserved1: [u32; 6],
    pub(crate) reserved2: [*mut c_void; 2],
    pub(crate) qpDeltaMap: *mut i8,
    pub(crate) qpDeltaMapSize: u32,
    pub(crate) reservedBitFields: u32,
    pub(crate) meHintRefPicDist: [u16; 2],
    pub(crate) alphaBuffer: NV_ENC_INPUT_PTR,
    /// `NVENC_EXTERNAL_ME_SB_HINT*`.
    pub(crate) meExternalSbHints: *mut c_void,
    pub(crate) meSbHintsCount: u32,
    pub(crate) stateBufferIdx: u32,
    pub(crate) outputReconBuffer: NV_ENC_OUTPUT_PTR,
    pub(crate) reserved3: [u32; 284],
    pub(crate) reserved4: [*mut c_void; 57],
}

/// `NV_ENC_LOCK_BITSTREAM`.
#[repr(C)]
pub(crate) struct NV_ENC_LOCK_BITSTREAM {
    pub(crate) version: u32,
    /// doNotWait:1, ltrFrame:1, getRCStats:1, reservedBitFields:29.
    pub(crate) bitFields: u32,
    pub(crate) outputBitstream: *mut c_void,
    pub(crate) sliceOffsets: *mut u32,
    pub(crate) frameIdx: u32,
    pub(crate) hwEncodeStatus: u32,
    pub(crate) numSlices: u32,
    pub(crate) bitstreamSizeInBytes: u32,
    pub(crate) outputTimeStamp: u64,
    pub(crate) outputDuration: u64,
    pub(crate) bitstreamBufferPtr: *mut c_void,
    pub(crate) pictureType: u32,
    pub(crate) pictureStruct: u32,
    pub(crate) frameAvgQP: u32,
    pub(crate) frameSatd: u32,
    pub(crate) ltrFrameIdx: u32,
    pub(crate) ltrFrameBitmap: u32,
    pub(crate) temporalId: u32,
    pub(crate) intraMBCount: u32,
    pub(crate) interMBCount: u32,
    pub(crate) averageMVX: i32,
    pub(crate) averageMVY: i32,
    pub(crate) alphaLayerSizeInBytes: u32,
    pub(crate) outputStatsPtrSize: u32,
    pub(crate) outputStatsPtr: *mut c_void,
    pub(crate) frameIdxDisplay: u32,
    pub(crate) reserved1: [u32; 220],
    pub(crate) reserved2: [*mut c_void; 63],
    pub(crate) reservedInternal: [u32; 8],
}

/// `NV_ENC_SEQUENCE_PARAM_PAYLOAD`.
#[repr(C)]
pub(crate) struct NV_ENC_SEQUENCE_PARAM_PAYLOAD {
    pub(crate) version: u32,
    pub(crate) inBufferSize: u32,
    pub(crate) spsId: u32,
    pub(crate) ppsId: u32,
    pub(crate) spsppsBuffer: *mut c_void,
    pub(crate) outSPSPPSPayloadSize: *mut u32,
    pub(crate) reserved: [u32; 250],
    pub(crate) reserved2: [*mut c_void; 64],
}

/// `NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS`.
#[repr(C)]
pub(crate) struct NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
    pub(crate) version: u32,
    pub(crate) deviceType: u32,
    pub(crate) device: *mut c_void,
    pub(crate) reserved: *mut c_void,
    pub(crate) apiVersion: u32,
    pub(crate) reserved1: [u32; 253],
    pub(crate) reserved2: [*mut c_void; 64],
}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

// `NVENCAPI` is `__stdcall` on `_WIN32`, which is `extern "system"`.

/// `NvEncodeAPICreateInstance`, exported by `nvEncodeAPI64.dll`.
pub(crate) type PNVENCODEAPICREATEINSTANCE =
    unsafe extern "system" fn(functionList: *mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS;
/// `NvEncodeAPIGetMaxSupportedVersion`, exported by `nvEncodeAPI64.dll`. The
/// result is `(major << 4) | minor`.
pub(crate) type PNVENCODEAPIGETMAXSUPPORTEDVERSION =
    unsafe extern "system" fn(version: *mut u32) -> NVENCSTATUS;

pub(crate) type PNVENCOPENENCODESESSIONEX = unsafe extern "system" fn(
    openSessionExParams: *mut NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS,
    encoder: *mut *mut c_void,
) -> NVENCSTATUS;
pub(crate) type PNVENCGETENCODEPRESETCONFIGEX = unsafe extern "system" fn(
    encoder: *mut c_void,
    encodeGUID: GUID,
    presetGUID: GUID,
    tuningInfo: u32,
    presetConfig: *mut NV_ENC_PRESET_CONFIG,
) -> NVENCSTATUS;
pub(crate) type PNVENCINITIALIZEENCODER = unsafe extern "system" fn(
    encoder: *mut c_void,
    createEncodeParams: *mut NV_ENC_INITIALIZE_PARAMS,
) -> NVENCSTATUS;
pub(crate) type PNVENCCREATEBITSTREAMBUFFER = unsafe extern "system" fn(
    encoder: *mut c_void,
    createBitstreamBufferParams: *mut NV_ENC_CREATE_BITSTREAM_BUFFER,
) -> NVENCSTATUS;
pub(crate) type PNVENCDESTROYBITSTREAMBUFFER = unsafe extern "system" fn(
    encoder: *mut c_void,
    bitstreamBuffer: NV_ENC_OUTPUT_PTR,
) -> NVENCSTATUS;
pub(crate) type PNVENCENCODEPICTURE = unsafe extern "system" fn(
    encoder: *mut c_void,
    encodePicParams: *mut NV_ENC_PIC_PARAMS,
) -> NVENCSTATUS;
pub(crate) type PNVENCLOCKBITSTREAM = unsafe extern "system" fn(
    encoder: *mut c_void,
    lockBitstreamBufferParams: *mut NV_ENC_LOCK_BITSTREAM,
) -> NVENCSTATUS;
pub(crate) type PNVENCUNLOCKBITSTREAM = unsafe extern "system" fn(
    encoder: *mut c_void,
    bitstreamBuffer: NV_ENC_OUTPUT_PTR,
) -> NVENCSTATUS;
pub(crate) type PNVENCGETSEQUENCEPARAMS = unsafe extern "system" fn(
    encoder: *mut c_void,
    sequenceParamPayload: *mut NV_ENC_SEQUENCE_PARAM_PAYLOAD,
) -> NVENCSTATUS;
pub(crate) type PNVENCMAPINPUTRESOURCE = unsafe extern "system" fn(
    encoder: *mut c_void,
    mapInputResParams: *mut NV_ENC_MAP_INPUT_RESOURCE,
) -> NVENCSTATUS;
pub(crate) type PNVENCUNMAPINPUTRESOURCE = unsafe extern "system" fn(
    encoder: *mut c_void,
    mappedInputBuffer: NV_ENC_INPUT_PTR,
) -> NVENCSTATUS;
pub(crate) type PNVENCDESTROYENCODER =
    unsafe extern "system" fn(encoder: *mut c_void) -> NVENCSTATUS;
pub(crate) type PNVENCREGISTERRESOURCE = unsafe extern "system" fn(
    encoder: *mut c_void,
    registerResParams: *mut NV_ENC_REGISTER_RESOURCE,
) -> NVENCSTATUS;
pub(crate) type PNVENCUNREGISTERRESOURCE = unsafe extern "system" fn(
    encoder: *mut c_void,
    registeredRes: NV_ENC_REGISTERED_PTR,
) -> NVENCSTATUS;
pub(crate) type PNVENCRECONFIGUREENCODER = unsafe extern "system" fn(
    encoder: *mut c_void,
    reInitEncodeParams: *mut NV_ENC_RECONFIGURE_PARAMS,
) -> NVENCSTATUS;
pub(crate) type PNVENCGETLASTERROR =
    unsafe extern "system" fn(encoder: *mut c_void) -> *const c_char;

/// `NV_ENCODE_API_FUNCTION_LIST`. Entries blink never calls are `*mut c_void`.
#[repr(C)]
pub(crate) struct NV_ENCODE_API_FUNCTION_LIST {
    pub(crate) version: u32,
    pub(crate) reserved: u32,
    pub(crate) nvEncOpenEncodeSession: *mut c_void,
    pub(crate) nvEncGetEncodeGUIDCount: *mut c_void,
    pub(crate) nvEncGetEncodeProfileGUIDCount: *mut c_void,
    pub(crate) nvEncGetEncodeProfileGUIDs: *mut c_void,
    pub(crate) nvEncGetEncodeGUIDs: *mut c_void,
    pub(crate) nvEncGetInputFormatCount: *mut c_void,
    pub(crate) nvEncGetInputFormats: *mut c_void,
    pub(crate) nvEncGetEncodeCaps: *mut c_void,
    pub(crate) nvEncGetEncodePresetCount: *mut c_void,
    pub(crate) nvEncGetEncodePresetGUIDs: *mut c_void,
    pub(crate) nvEncGetEncodePresetConfig: *mut c_void,
    pub(crate) nvEncInitializeEncoder: Option<PNVENCINITIALIZEENCODER>,
    pub(crate) nvEncCreateInputBuffer: *mut c_void,
    pub(crate) nvEncDestroyInputBuffer: *mut c_void,
    pub(crate) nvEncCreateBitstreamBuffer: Option<PNVENCCREATEBITSTREAMBUFFER>,
    pub(crate) nvEncDestroyBitstreamBuffer: Option<PNVENCDESTROYBITSTREAMBUFFER>,
    pub(crate) nvEncEncodePicture: Option<PNVENCENCODEPICTURE>,
    pub(crate) nvEncLockBitstream: Option<PNVENCLOCKBITSTREAM>,
    pub(crate) nvEncUnlockBitstream: Option<PNVENCUNLOCKBITSTREAM>,
    pub(crate) nvEncLockInputBuffer: *mut c_void,
    pub(crate) nvEncUnlockInputBuffer: *mut c_void,
    pub(crate) nvEncGetEncodeStats: *mut c_void,
    pub(crate) nvEncGetSequenceParams: Option<PNVENCGETSEQUENCEPARAMS>,
    pub(crate) nvEncRegisterAsyncEvent: *mut c_void,
    pub(crate) nvEncUnregisterAsyncEvent: *mut c_void,
    pub(crate) nvEncMapInputResource: Option<PNVENCMAPINPUTRESOURCE>,
    pub(crate) nvEncUnmapInputResource: Option<PNVENCUNMAPINPUTRESOURCE>,
    pub(crate) nvEncDestroyEncoder: Option<PNVENCDESTROYENCODER>,
    pub(crate) nvEncInvalidateRefFrames: *mut c_void,
    pub(crate) nvEncOpenEncodeSessionEx: Option<PNVENCOPENENCODESESSIONEX>,
    pub(crate) nvEncRegisterResource: Option<PNVENCREGISTERRESOURCE>,
    pub(crate) nvEncUnregisterResource: Option<PNVENCUNREGISTERRESOURCE>,
    pub(crate) nvEncReconfigureEncoder: Option<PNVENCRECONFIGUREENCODER>,
    pub(crate) reserved1: *mut c_void,
    pub(crate) nvEncCreateMVBuffer: *mut c_void,
    pub(crate) nvEncDestroyMVBuffer: *mut c_void,
    pub(crate) nvEncRunMotionEstimationOnly: *mut c_void,
    pub(crate) nvEncGetLastErrorString: Option<PNVENCGETLASTERROR>,
    pub(crate) nvEncSetIOCudaStreams: *mut c_void,
    pub(crate) nvEncGetEncodePresetConfigEx: Option<PNVENCGETENCODEPRESETCONFIGEX>,
    pub(crate) nvEncGetSequenceParamEx: *mut c_void,
    pub(crate) nvEncRestoreEncoderState: *mut c_void,
    pub(crate) nvEncLookaheadPicture: *mut c_void,
    pub(crate) reserved2: [*mut c_void; 275],
}

// SAFETY: this and every struct below contain only integers, raw pointers,
// `Option`s of function pointers (whose `None` is the null pointer), and
// arrays or nested structs of those, so the all-zero pattern is valid.
unsafe impl Zeroable for NV_ENC_CONFIG {}
// SAFETY: as above.
unsafe impl Zeroable for NV_ENC_PRESET_CONFIG {}
// SAFETY: as above.
unsafe impl Zeroable for NV_ENC_INITIALIZE_PARAMS {}
// SAFETY: as above.
unsafe impl Zeroable for NV_ENC_RECONFIGURE_PARAMS {}
// SAFETY: as above.
unsafe impl Zeroable for NV_ENC_CREATE_BITSTREAM_BUFFER {}
// SAFETY: as above.
unsafe impl Zeroable for NV_ENC_REGISTER_RESOURCE {}
// SAFETY: as above.
unsafe impl Zeroable for NV_ENC_MAP_INPUT_RESOURCE {}
// SAFETY: as above.
unsafe impl Zeroable for NV_ENC_PIC_PARAMS {}
// SAFETY: as above.
unsafe impl Zeroable for NV_ENC_LOCK_BITSTREAM {}
// SAFETY: as above.
unsafe impl Zeroable for NV_ENC_SEQUENCE_PARAM_PAYLOAD {}
// SAFETY: as above.
unsafe impl Zeroable for NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {}
// SAFETY: as above.
unsafe impl Zeroable for NV_ENCODE_API_FUNCTION_LIST {}

// ---------------------------------------------------------------------------
// Layout assertions
// ---------------------------------------------------------------------------

// Expected values come from clang's record-layout dump of the header for
// x86_64-pc-windows-msvc (and agree with `sizeof`/`offsetof` from the same
// header compiled for the host). They assume 64-bit pointers.
const _: () = {
    assert!(
        size_of::<*mut c_void>() == 8,
        "bindings assume a 64-bit target"
    );

    assert!(size_of::<GUID>() == 16);
    assert!(size_of::<NV_ENC_QP>() == 12);
    assert!(size_of::<NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE>() == 16);

    assert!(size_of::<NV_ENC_RC_PARAMS>() == 128);
    assert!(offset_of!(NV_ENC_RC_PARAMS, averageBitRate) == 20);
    assert!(offset_of!(NV_ENC_RC_PARAMS, vbvInitialDelay) == 32);
    assert!(offset_of!(NV_ENC_RC_PARAMS, bitFields) == 36);
    assert!(offset_of!(NV_ENC_RC_PARAMS, minQP) == 40);
    assert!(offset_of!(NV_ENC_RC_PARAMS, lookaheadDepth) == 90);
    assert!(offset_of!(NV_ENC_RC_PARAMS, qpMapMode) == 96);
    assert!(offset_of!(NV_ENC_RC_PARAMS, multiPass) == 100);
    assert!(offset_of!(NV_ENC_RC_PARAMS, reserved) == 112);

    assert!(size_of::<NV_ENC_CONFIG_H264_VUI_PARAMETERS>() == 112);
    assert!(offset_of!(NV_ENC_CONFIG_H264_VUI_PARAMETERS, colourMatrix) == 32);
    assert!(offset_of!(NV_ENC_CONFIG_H264_VUI_PARAMETERS, reserved) == 64);

    assert!(size_of::<NV_ENC_CONFIG_H264>() == 1792);
    assert!(align_of::<NV_ENC_CONFIG_H264>() == 8);
    assert!(offset_of!(NV_ENC_CONFIG_H264, level) == 4);
    assert!(offset_of!(NV_ENC_CONFIG_H264, idrPeriod) == 8);
    assert!(offset_of!(NV_ENC_CONFIG_H264, h264VUIParameters) == 72);
    assert!(offset_of!(NV_ENC_CONFIG_H264, ltrNumFrames) == 184);
    assert!(offset_of!(NV_ENC_CONFIG_H264, chromaFormatIDC) == 192);
    assert!(offset_of!(NV_ENC_CONFIG_H264, useBFramesAsRef) == 200);
    assert!(offset_of!(NV_ENC_CONFIG_H264, numRefL1) == 208);
    assert!(offset_of!(NV_ENC_CONFIG_H264, reserved1) == 212);
    assert!(offset_of!(NV_ENC_CONFIG_H264, reserved2) == 1280);

    // The real union is 1792 bytes, 8-byte aligned (its H.264 member).
    assert!(size_of::<NV_ENC_CODEC_CONFIG>() == 1792);
    assert!(align_of::<NV_ENC_CODEC_CONFIG>() == 8);

    assert!(size_of::<NV_ENC_CONFIG>() == 3584);
    assert!(offset_of!(NV_ENC_CONFIG, profileGUID) == 4);
    assert!(offset_of!(NV_ENC_CONFIG, gopLength) == 20);
    assert!(offset_of!(NV_ENC_CONFIG, frameIntervalP) == 24);
    assert!(offset_of!(NV_ENC_CONFIG, frameFieldMode) == 32);
    assert!(offset_of!(NV_ENC_CONFIG, rcParams) == 40);
    assert!(offset_of!(NV_ENC_CONFIG, encodeCodecConfig) == 168);
    assert!(offset_of!(NV_ENC_CONFIG, reserved) == 1960);
    assert!(offset_of!(NV_ENC_CONFIG, reserved2) == 3072);

    assert!(size_of::<NV_ENC_PRESET_CONFIG>() == 5128);
    assert!(offset_of!(NV_ENC_PRESET_CONFIG, presetCfg) == 8);
    assert!(offset_of!(NV_ENC_PRESET_CONFIG, reserved1) == 3592);
    assert!(offset_of!(NV_ENC_PRESET_CONFIG, reserved2) == 4616);

    assert!(size_of::<NV_ENC_INITIALIZE_PARAMS>() == 1808);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, encodeGUID) == 4);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, presetGUID) == 20);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, encodeWidth) == 36);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, darWidth) == 44);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, frameRateNum) == 52);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, enableEncodeAsync) == 60);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, enablePTD) == 64);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, bitFields) == 68);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, privDataSize) == 72);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, privData) == 80);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, encodeConfig) == 88);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, maxEncodeWidth) == 96);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, maxEncodeHeight) == 100);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, tuningInfo) == 136);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, bufferFormat) == 140);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, outputStatsLevel) == 148);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, reserved) == 152);
    assert!(offset_of!(NV_ENC_INITIALIZE_PARAMS, reserved2) == 1296);

    assert!(size_of::<NV_ENC_RECONFIGURE_PARAMS>() == 1824);
    assert!(offset_of!(NV_ENC_RECONFIGURE_PARAMS, reInitEncodeParams) == 8);
    assert!(offset_of!(NV_ENC_RECONFIGURE_PARAMS, bitFields) == 1816);

    assert!(size_of::<NV_ENC_CREATE_BITSTREAM_BUFFER>() == 776);
    assert!(offset_of!(NV_ENC_CREATE_BITSTREAM_BUFFER, bitstreamBuffer) == 16);
    assert!(offset_of!(NV_ENC_CREATE_BITSTREAM_BUFFER, reserved2) == 264);

    assert!(size_of::<NV_ENC_REGISTER_RESOURCE>() == 1536);
    assert!(offset_of!(NV_ENC_REGISTER_RESOURCE, resourceType) == 4);
    assert!(offset_of!(NV_ENC_REGISTER_RESOURCE, width) == 8);
    assert!(offset_of!(NV_ENC_REGISTER_RESOURCE, resourceToRegister) == 24);
    assert!(offset_of!(NV_ENC_REGISTER_RESOURCE, registeredResource) == 32);
    assert!(offset_of!(NV_ENC_REGISTER_RESOURCE, bufferFormat) == 40);
    assert!(offset_of!(NV_ENC_REGISTER_RESOURCE, bufferUsage) == 44);
    assert!(offset_of!(NV_ENC_REGISTER_RESOURCE, pInputFencePoint) == 48);
    assert!(offset_of!(NV_ENC_REGISTER_RESOURCE, reserved1) == 64);
    assert!(offset_of!(NV_ENC_REGISTER_RESOURCE, reserved2) == 1048);

    assert!(size_of::<NV_ENC_MAP_INPUT_RESOURCE>() == 1544);
    assert!(offset_of!(NV_ENC_MAP_INPUT_RESOURCE, registeredResource) == 16);
    assert!(offset_of!(NV_ENC_MAP_INPUT_RESOURCE, mappedResource) == 24);
    assert!(offset_of!(NV_ENC_MAP_INPUT_RESOURCE, mappedBufferFmt) == 32);
    assert!(offset_of!(NV_ENC_MAP_INPUT_RESOURCE, reserved2) == 1040);

    // The real union is 1552 bytes, 8-byte aligned (its AV1 member).
    assert!(size_of::<[u64; 194]>() == 1552);
    assert!(size_of::<NV_ENC_PIC_PARAMS>() == 3360);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, encodePicFlags) == 16);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, inputTimeStamp) == 24);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, inputDuration) == 32);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, inputBuffer) == 40);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, outputBitstream) == 48);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, completionEvent) == 56);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, bufferFmt) == 64);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, pictureStruct) == 68);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, pictureType) == 72);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, codecPicParams) == 80);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, meHintCountsPerBlock) == 1632);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, meExternalHints) == 1664);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, qpDeltaMap) == 1712);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, meHintRefPicDist) == 1728);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, alphaBuffer) == 1736);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, outputReconBuffer) == 1760);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, reserved3) == 1768);
    assert!(offset_of!(NV_ENC_PIC_PARAMS, reserved4) == 2904);

    assert!(size_of::<NV_ENC_LOCK_BITSTREAM>() == 1552);
    assert!(offset_of!(NV_ENC_LOCK_BITSTREAM, bitFields) == 4);
    assert!(offset_of!(NV_ENC_LOCK_BITSTREAM, outputBitstream) == 8);
    assert!(offset_of!(NV_ENC_LOCK_BITSTREAM, bitstreamSizeInBytes) == 36);
    assert!(offset_of!(NV_ENC_LOCK_BITSTREAM, outputTimeStamp) == 40);
    assert!(offset_of!(NV_ENC_LOCK_BITSTREAM, bitstreamBufferPtr) == 56);
    assert!(offset_of!(NV_ENC_LOCK_BITSTREAM, pictureType) == 64);
    assert!(offset_of!(NV_ENC_LOCK_BITSTREAM, outputStatsPtr) == 120);
    assert!(offset_of!(NV_ENC_LOCK_BITSTREAM, frameIdxDisplay) == 128);
    assert!(offset_of!(NV_ENC_LOCK_BITSTREAM, reserved2) == 1016);
    assert!(offset_of!(NV_ENC_LOCK_BITSTREAM, reservedInternal) == 1520);

    assert!(size_of::<NV_ENC_SEQUENCE_PARAM_PAYLOAD>() == 1544);
    assert!(offset_of!(NV_ENC_SEQUENCE_PARAM_PAYLOAD, spsppsBuffer) == 16);
    assert!(offset_of!(NV_ENC_SEQUENCE_PARAM_PAYLOAD, outSPSPPSPayloadSize) == 24);
    assert!(offset_of!(NV_ENC_SEQUENCE_PARAM_PAYLOAD, reserved2) == 1032);

    assert!(size_of::<NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS>() == 1552);
    assert!(offset_of!(NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS, device) == 8);
    assert!(offset_of!(NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS, apiVersion) == 24);
    assert!(offset_of!(NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS, reserved2) == 1040);

    assert!(size_of::<NV_ENCODE_API_FUNCTION_LIST>() == 2552);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncGetEncodePresetConfig) == 88);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncInitializeEncoder) == 96);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncCreateBitstreamBuffer) == 120);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncDestroyBitstreamBuffer) == 128);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncEncodePicture) == 136);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncLockBitstream) == 144);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncUnlockBitstream) == 152);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncGetSequenceParams) == 184);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncMapInputResource) == 208);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncUnmapInputResource) == 216);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncDestroyEncoder) == 224);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncOpenEncodeSessionEx) == 240);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncRegisterResource) == 248);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncUnregisterResource) == 256);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncReconfigureEncoder) == 264);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncGetLastErrorString) == 304);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncGetEncodePresetConfigEx) == 320);
    assert!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, reserved2) == 352);
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Values printed by a C program that includes the header and evaluates
    /// each macro.
    #[test]
    fn version_macros_match_the_header() {
        assert_eq!(NVENCAPI_VERSION, 0x0100_000C);
        assert_eq!(NV_ENCODE_API_FUNCTION_LIST_VER, 0x7102_000C);
        assert_eq!(NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER, 0x7101_000C);
        assert_eq!(NV_ENC_PRESET_CONFIG_VER, 0xF104_000C);
        assert_eq!(NV_ENC_CONFIG_VER, 0xF108_000C);
        assert_eq!(NV_ENC_RC_PARAMS_VER, 0x7101_000C);
        assert_eq!(NV_ENC_INITIALIZE_PARAMS_VER, 0xF106_000C);
        assert_eq!(NV_ENC_RECONFIGURE_PARAMS_VER, 0xF101_000C);
        assert_eq!(NV_ENC_CREATE_BITSTREAM_BUFFER_VER, 0x7101_000C);
        assert_eq!(NV_ENC_REGISTER_RESOURCE_VER, 0x7104_000C);
        assert_eq!(NV_ENC_MAP_INPUT_RESOURCE_VER, 0x7104_000C);
        assert_eq!(NV_ENC_PIC_PARAMS_VER, 0xF106_000C);
        assert_eq!(NV_ENC_LOCK_BITSTREAM_VER, 0xF101_000C);
        assert_eq!(NV_ENC_SEQUENCE_PARAM_PAYLOAD_VER, 0x7101_000C);
    }

    #[test]
    fn zeroed_function_list_has_no_entries() {
        let list: NV_ENCODE_API_FUNCTION_LIST = zeroed();
        assert!(list.nvEncOpenEncodeSessionEx.is_none());
        assert!(list.nvEncGetLastErrorString.is_none());
    }
}
