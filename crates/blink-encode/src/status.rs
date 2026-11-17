//! NVENC status codes and the driver's API version.

use core::fmt;

/// An `NVENCSTATUS` value returned by an NVENC call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NvencStatus(i32);

/// `NVENCSTATUS` names in enum order, so index `n` names code `n`.
const NAMES: [&str; 27] = [
    "NV_ENC_SUCCESS",
    "NV_ENC_ERR_NO_ENCODE_DEVICE",
    "NV_ENC_ERR_UNSUPPORTED_DEVICE",
    "NV_ENC_ERR_INVALID_ENCODERDEVICE",
    "NV_ENC_ERR_INVALID_DEVICE",
    "NV_ENC_ERR_DEVICE_NOT_EXIST",
    "NV_ENC_ERR_INVALID_PTR",
    "NV_ENC_ERR_INVALID_EVENT",
    "NV_ENC_ERR_INVALID_PARAM",
    "NV_ENC_ERR_INVALID_CALL",
    "NV_ENC_ERR_OUT_OF_MEMORY",
    "NV_ENC_ERR_ENCODER_NOT_INITIALIZED",
    "NV_ENC_ERR_UNSUPPORTED_PARAM",
    "NV_ENC_ERR_LOCK_BUSY",
    "NV_ENC_ERR_NOT_ENOUGH_BUFFER",
    "NV_ENC_ERR_INVALID_VERSION",
    "NV_ENC_ERR_MAP_FAILED",
    "NV_ENC_ERR_NEED_MORE_INPUT",
    "NV_ENC_ERR_ENCODER_BUSY",
    "NV_ENC_ERR_EVENT_NOT_REGISTERD",
    "NV_ENC_ERR_GENERIC",
    "NV_ENC_ERR_INCOMPATIBLE_CLIENT_KEY",
    "NV_ENC_ERR_UNIMPLEMENTED",
    "NV_ENC_ERR_RESOURCE_REGISTER_FAILED",
    "NV_ENC_ERR_RESOURCE_NOT_REGISTERED",
    "NV_ENC_ERR_RESOURCE_NOT_MAPPED",
    "NV_ENC_ERR_NEED_MORE_OUTPUT",
];

impl NvencStatus {
    /// `NV_ENC_SUCCESS`.
    pub const SUCCESS: Self = Self(0);

    /// Wraps a raw `NVENCSTATUS` value.
    #[must_use]
    pub const fn from_raw(code: i32) -> Self {
        Self(code)
    }

    /// The raw `NVENCSTATUS` value.
    #[must_use]
    pub const fn code(self) -> i32 {
        self.0
    }

    /// Whether this is `NV_ENC_SUCCESS`.
    #[must_use]
    pub const fn is_success(self) -> bool {
        self.0 == 0
    }

    /// The enumerator name from `nvEncodeAPI.h`, or `"unknown NVENCSTATUS"`
    /// for a value the 12.1 header does not define.
    #[must_use]
    pub fn name(self) -> &'static str {
        usize::try_from(self.0)
            .ok()
            .and_then(|index| NAMES.get(index))
            .copied()
            .unwrap_or("unknown NVENCSTATUS")
    }
}

impl fmt::Display for NvencStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name(), self.0)
    }
}

/// An NVENC API version as `NvEncodeAPIGetMaxSupportedVersion` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ApiVersion {
    pub(crate) major: u32,
    pub(crate) minor: u32,
}

impl ApiVersion {
    /// Decodes the driver's packed value: "The 4 least significant bits in the
    /// returned [value] indicate the minor version and the rest of the bits
    /// indicate the major version."
    pub(crate) const fn from_driver(packed: u32) -> Self {
        Self {
            major: packed >> 4,
            minor: packed & 0xF,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_header_enum_order() {
        assert_eq!(NvencStatus::SUCCESS.name(), "NV_ENC_SUCCESS");
        assert_eq!(NvencStatus::from_raw(8).name(), "NV_ENC_ERR_INVALID_PARAM");
        assert_eq!(
            NvencStatus::from_raw(17).name(),
            "NV_ENC_ERR_NEED_MORE_INPUT"
        );
        assert_eq!(NvencStatus::from_raw(20).name(), "NV_ENC_ERR_GENERIC");
        assert_eq!(
            NvencStatus::from_raw(26).name(),
            "NV_ENC_ERR_NEED_MORE_OUTPUT"
        );
    }

    #[test]
    fn unknown_codes_have_a_placeholder_name() {
        assert_eq!(NvencStatus::from_raw(27).name(), "unknown NVENCSTATUS");
        assert_eq!(NvencStatus::from_raw(-1).name(), "unknown NVENCSTATUS");
    }

    #[test]
    fn display_includes_name_and_code() {
        assert_eq!(
            NvencStatus::from_raw(15).to_string(),
            "NV_ENC_ERR_INVALID_VERSION (15)"
        );
        assert!(NvencStatus::SUCCESS.is_success());
        assert!(!NvencStatus::from_raw(1).is_success());
        assert_eq!(NvencStatus::from_raw(9).code(), 9);
    }

    #[test]
    fn driver_version_decodes_and_orders() {
        let driver = ApiVersion::from_driver(13 << 4);
        assert_eq!(
            driver,
            ApiVersion {
                major: 13,
                minor: 0
            }
        );
        let required = ApiVersion {
            major: 12,
            minor: 1,
        };
        assert!(driver >= required);
        assert!(ApiVersion::from_driver((12 << 4) | 1) >= required);
        assert!(ApiVersion::from_driver(12 << 4) < required);
        assert!(ApiVersion::from_driver((11 << 4) | 1) < required);
    }
}
