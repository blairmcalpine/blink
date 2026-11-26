//! Filling the NVENC configuration structs. Pure functions over the FFI
//! layouts, so they build and are tested on every host.

use crate::config::{EncoderConfig, EncoderConfigError, Preset, bitrate_bps};
use crate::ffi::{
    GUID, H264_DISABLE_SPSPPS_BIT, H264_ENABLE_INTRA_REFRESH_BIT, H264_ENABLE_LTR_BIT,
    H264_ENABLE_VFR_BIT, H264_HIERARCHICAL_B_FRAMES_BIT, H264_HIERARCHICAL_P_FRAMES_BIT,
    H264_OUTPUT_AUD_BIT, H264_REPEAT_SPSPPS_BIT, NV_ENC_BFRAME_REF_MODE_DISABLED,
    NV_ENC_CODEC_H264_GUID, NV_ENC_CONFIG, NV_ENC_CONFIG_VER, NV_ENC_H264_PROFILE_HIGH_GUID,
    NV_ENC_INITIALIZE_PARAMS, NV_ENC_INITIALIZE_PARAMS_VER, NV_ENC_LEVEL_AUTOSELECT,
    NV_ENC_MULTI_PASS_DISABLED, NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME, NV_ENC_PARAMS_RC_CBR,
    NV_ENC_PRESET_P1_GUID, NV_ENC_PRESET_P2_GUID, NV_ENC_PRESET_P3_GUID, NV_ENC_RC_PARAMS_VER,
    NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY, NV_ENC_VUI_COLOR_PRIMARIES_BT709,
    NV_ENC_VUI_MATRIX_COEFFS_BT709, NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709,
    NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED, NVENC_INFINITE_GOPLENGTH, RC_AQ_STRENGTH_MASK,
    RC_ENABLE_AQ_BIT, RC_ENABLE_EXT_LOOKAHEAD_BIT, RC_ENABLE_LOOKAHEAD_BIT,
    RC_ENABLE_TEMPORAL_AQ_BIT,
};

/// The NVENC tuning blink always uses.
pub(crate) const TUNING: u32 = NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;

/// The preset GUID for `preset`.
pub(crate) const fn preset_guid(preset: Preset) -> GUID {
    match preset {
        Preset::P1 => NV_ENC_PRESET_P1_GUID,
        Preset::P2 => NV_ENC_PRESET_P2_GUID,
        Preset::P3 => NV_ENC_PRESET_P3_GUID,
    }
}

/// Overrides the preset's config with blink's fixed choices: H.264 High, CBR
/// with a one-second VBV, P-frames only, an infinite GOP (IDRs are forced by
/// timestamp, see `idr.rs`), and nothing that adds latency or runs on CUDA
/// cores (lookahead, AQ, multipass).
pub(crate) fn apply_config(
    config: &mut NV_ENC_CONFIG,
    settings: &EncoderConfig,
) -> Result<(), EncoderConfigError> {
    let bps = bitrate_bps(settings.bitrate_kbps)?;

    config.version = NV_ENC_CONFIG_VER;
    config.profileGUID = NV_ENC_H264_PROFILE_HIGH_GUID;
    config.gopLength = NVENC_INFINITE_GOPLENGTH;
    // Required by the header with an infinite GOP; also means no B-frames.
    config.frameIntervalP = 1;
    config.frameFieldMode = NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME;

    let rc = &mut config.rcParams;
    rc.version = NV_ENC_RC_PARAMS_VER;
    rc.rateControlMode = NV_ENC_PARAMS_RC_CBR;
    rc.averageBitRate = bps;
    rc.maxBitRate = bps;
    rc.vbvBufferSize = bps;
    rc.vbvInitialDelay = bps;
    rc.bitFields &= !(RC_ENABLE_AQ_BIT
        | RC_ENABLE_TEMPORAL_AQ_BIT
        | RC_AQ_STRENGTH_MASK
        | RC_ENABLE_LOOKAHEAD_BIT
        | RC_ENABLE_EXT_LOOKAHEAD_BIT);
    rc.lookaheadDepth = 0;
    rc.multiPass = NV_ENC_MULTI_PASS_DISABLED;

    let h264 = &mut config.encodeCodecConfig.h264Config;
    // enableVFR only clears fixed_frame_rate_flag in the VUI: capture is
    // variable-rate, so the stream must not claim a fixed rate.
    h264.bitFields |= H264_REPEAT_SPSPPS_BIT | H264_ENABLE_VFR_BIT;
    h264.bitFields &= !(H264_OUTPUT_AUD_BIT
        | H264_DISABLE_SPSPPS_BIT
        | H264_ENABLE_INTRA_REFRESH_BIT
        | H264_ENABLE_LTR_BIT
        | H264_HIERARCHICAL_P_FRAMES_BIT
        | H264_HIERARCHICAL_B_FRAMES_BIT);
    h264.idrPeriod = NVENC_INFINITE_GOPLENGTH;
    h264.level = NV_ENC_LEVEL_AUTOSELECT;
    h264.chromaFormatIDC = 1;
    h264.useBFramesAsRef = NV_ENC_BFRAME_REF_MODE_DISABLED;

    // Desktop content as limited-range BT.709 YUV 4:2:0, labelled so players
    // do not guess BT.601. Capture's NV12 conversion produces exactly that.
    // The BGRA fallback relies on NVENC's ARGB conversion, which needs
    // hardware verification that it uses BT.709 coefficients too (encode
    // SMPTE colour bars, decode, compare).
    let vui = &mut h264.h264VUIParameters;
    vui.videoSignalTypePresentFlag = 1;
    vui.videoFormat = NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED;
    vui.videoFullRangeFlag = 0;
    vui.colourDescriptionPresentFlag = 1;
    vui.colourPrimaries = NV_ENC_VUI_COLOR_PRIMARIES_BT709;
    vui.transferCharacteristics = NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709;
    vui.colourMatrix = NV_ENC_VUI_MATRIX_COEFFS_BT709;
    Ok(())
}

/// Fills the initialize parameters for `settings`' current size. Leaves
/// `encodeConfig` for the caller, which must point it at a live config.
pub(crate) fn fill_initialize(
    init: &mut NV_ENC_INITIALIZE_PARAMS,
    settings: &EncoderConfig,
) -> Result<(), EncoderConfigError> {
    let (dar_width, dar_height) = settings.display_aspect()?;
    init.version = NV_ENC_INITIALIZE_PARAMS_VER;
    init.encodeGUID = NV_ENC_CODEC_H264_GUID;
    init.presetGUID = preset_guid(settings.preset);
    init.encodeWidth = settings.width;
    init.encodeHeight = settings.height;
    init.darWidth = dar_width;
    init.darHeight = dar_height;
    init.frameRateNum = settings.fps;
    init.frameRateDen = 1;
    // Synchronous: see the `Encoder` docs for the tradeoff.
    init.enableEncodeAsync = 0;
    // Picture type decision stays on; NV_ENC_PIC_FLAG_FORCEIDR needs it.
    init.enablePTD = 1;
    init.maxEncodeWidth = settings.max_width;
    init.maxEncodeHeight = settings.max_height;
    init.tuningInfo = TUNING;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::zeroed;

    fn settings() -> EncoderConfig {
        EncoderConfig {
            max_width: 2560,
            max_height: 1440,
            ..EncoderConfig::new(1280, 960, 60, 20_000)
        }
    }

    /// A config as if the preset had turned every flag on.
    fn noisy_preset() -> Box<NV_ENC_CONFIG> {
        let mut config: Box<NV_ENC_CONFIG> = Box::new(zeroed());
        config.rcParams.bitFields = u32::MAX;
        config.rcParams.lookaheadDepth = 32;
        config.rcParams.multiPass = 2;
        config.encodeCodecConfig.h264Config.bitFields = u32::MAX;
        config.gopLength = 250;
        config.frameIntervalP = 3;
        config
    }

    #[test]
    fn config_clears_latency_and_cuda_features() {
        let mut config = noisy_preset();
        apply_config(&mut config, &settings()).unwrap();
        let rc = &config.rcParams;
        assert_eq!(rc.bitFields & RC_ENABLE_AQ_BIT, 0);
        assert_eq!(rc.bitFields & RC_ENABLE_TEMPORAL_AQ_BIT, 0);
        assert_eq!(rc.bitFields & RC_AQ_STRENGTH_MASK, 0);
        assert_eq!(rc.bitFields & RC_ENABLE_LOOKAHEAD_BIT, 0);
        assert_eq!(rc.bitFields & RC_ENABLE_EXT_LOOKAHEAD_BIT, 0);
        assert_eq!(rc.lookaheadDepth, 0);
        assert_eq!(rc.multiPass, NV_ENC_MULTI_PASS_DISABLED);
        // Bits blink does not manage are left as the preset set them.
        assert_eq!(rc.bitFields & 1, 1);
    }

    #[test]
    fn config_is_cbr_with_a_one_second_vbv() {
        let mut config = noisy_preset();
        apply_config(&mut config, &settings()).unwrap();
        let rc = &config.rcParams;
        assert_eq!(rc.version, NV_ENC_RC_PARAMS_VER);
        assert_eq!(rc.rateControlMode, NV_ENC_PARAMS_RC_CBR);
        assert_eq!(rc.averageBitRate, 20_000_000);
        assert_eq!(rc.maxBitRate, 20_000_000);
        assert_eq!(rc.vbvBufferSize, 20_000_000);
        assert_eq!(rc.vbvInitialDelay, 20_000_000);
    }

    #[test]
    fn config_uses_an_infinite_p_only_gop() {
        let mut config = noisy_preset();
        apply_config(&mut config, &settings()).unwrap();
        assert_eq!(config.version, NV_ENC_CONFIG_VER);
        assert_eq!(config.profileGUID, NV_ENC_H264_PROFILE_HIGH_GUID);
        assert_eq!(config.gopLength, NVENC_INFINITE_GOPLENGTH);
        assert_eq!(config.frameIntervalP, 1);
        assert_eq!(
            config.encodeCodecConfig.h264Config.idrPeriod,
            NVENC_INFINITE_GOPLENGTH
        );
    }

    #[test]
    fn config_sets_the_h264_flags() {
        let mut config = noisy_preset();
        apply_config(&mut config, &settings()).unwrap();
        let h264 = &config.encodeCodecConfig.h264Config;
        assert_ne!(h264.bitFields & H264_REPEAT_SPSPPS_BIT, 0);
        assert_ne!(h264.bitFields & H264_ENABLE_VFR_BIT, 0);
        for cleared in [
            H264_OUTPUT_AUD_BIT,
            H264_DISABLE_SPSPPS_BIT,
            H264_ENABLE_INTRA_REFRESH_BIT,
            H264_ENABLE_LTR_BIT,
            H264_HIERARCHICAL_P_FRAMES_BIT,
            H264_HIERARCHICAL_B_FRAMES_BIT,
        ] {
            assert_eq!(h264.bitFields & cleared, 0, "bit {cleared:#x}");
        }
        assert_eq!(h264.chromaFormatIDC, 1);
        let vui = &h264.h264VUIParameters;
        assert_eq!(vui.colourMatrix, NV_ENC_VUI_MATRIX_COEFFS_BT709);
        assert_eq!(vui.videoFullRangeFlag, 0);
    }

    #[test]
    fn config_rejects_a_bad_bitrate() {
        let mut config = noisy_preset();
        let bad = EncoderConfig {
            bitrate_kbps: 0,
            ..settings()
        };
        assert_eq!(
            apply_config(&mut config, &bad),
            Err(EncoderConfigError::Bitrate(0))
        );
    }

    #[test]
    fn initialize_takes_the_dar_from_stretch_to() {
        let mut init: Box<NV_ENC_INITIALIZE_PARAMS> = Box::new(zeroed());
        let stretched = EncoderConfig {
            stretch_to: Some((1920, 1080)),
            ..settings()
        };
        fill_initialize(&mut init, &stretched).unwrap();
        assert_eq!((init.encodeWidth, init.encodeHeight), (1280, 960));
        assert_eq!((init.darWidth, init.darHeight), (16, 9));
        assert_eq!((init.maxEncodeWidth, init.maxEncodeHeight), (2560, 1440));

        fill_initialize(&mut init, &settings()).unwrap();
        assert_eq!((init.darWidth, init.darHeight), (4, 3));
    }

    #[test]
    fn initialize_is_synchronous_with_ptd_and_ull_tuning() {
        let mut init: Box<NV_ENC_INITIALIZE_PARAMS> = Box::new(zeroed());
        fill_initialize(&mut init, &settings()).unwrap();
        assert_eq!(init.version, NV_ENC_INITIALIZE_PARAMS_VER);
        assert_eq!(init.encodeGUID, NV_ENC_CODEC_H264_GUID);
        assert_eq!(init.presetGUID, NV_ENC_PRESET_P2_GUID);
        assert_eq!(init.enableEncodeAsync, 0);
        assert_eq!(init.enablePTD, 1);
        assert_eq!(init.tuningInfo, NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY);
        assert_eq!((init.frameRateNum, init.frameRateDen), (60, 1));
        assert!(init.encodeConfig.is_null());
    }
}
