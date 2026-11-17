use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A frame size in pixels, written as `"1920x1080"` in the config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct Resolution {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Errors produced while parsing a [`Resolution`].
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolutionParseError {
    /// The text is not of the form `WIDTHxHEIGHT`.
    #[error("resolution {0:?} is not of the form WIDTHxHEIGHT, such as 1920x1080")]
    Format(String),
    /// The width or height is zero.
    #[error("resolution {0:?} has a zero dimension")]
    Zero(String),
}

impl FromStr for Resolution {
    type Err = ResolutionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let format_error = || ResolutionParseError::Format(s.to_owned());
        let (width, height) = s.trim().split_once(['x', 'X']).ok_or_else(format_error)?;
        let width: u32 = width.trim().parse().map_err(|_| format_error())?;
        let height: u32 = height.trim().parse().map_err(|_| format_error())?;
        if width == 0 || height == 0 {
            return Err(ResolutionParseError::Zero(s.to_owned()));
        }
        Ok(Self { width, height })
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

impl From<Resolution> for String {
    fn from(resolution: Resolution) -> Self {
        resolution.to_string()
    }
}

impl TryFrom<String> for Resolution {
    type Error = ResolutionParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays() {
        let r: Resolution = "1280x960".parse().unwrap();
        assert_eq!(
            r,
            Resolution {
                width: 1280,
                height: 960
            }
        );
        assert_eq!(r.to_string(), "1280x960");
    }

    #[test]
    fn parse_is_lenient_about_case_and_whitespace() {
        let r: Resolution = " 1920 X 1080 ".parse().unwrap();
        assert_eq!(r.to_string(), "1920x1080");
    }

    #[test]
    fn rejects_malformed() {
        for s in [
            "",
            "1920",
            "1920x",
            "x1080",
            "1920*1080",
            "-1x5",
            "1920x1080x1",
            "axb",
        ] {
            assert!(
                matches!(
                    s.parse::<Resolution>(),
                    Err(ResolutionParseError::Format(_))
                ),
                "{s:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_zero_dimensions() {
        for s in ["0x1080", "1920x0"] {
            assert!(matches!(
                s.parse::<Resolution>(),
                Err(ResolutionParseError::Zero(_))
            ));
        }
    }
}
