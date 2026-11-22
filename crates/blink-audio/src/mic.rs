//! What the microphone capture is doing, for the tray status.

/// The microphone capture's state, as last reported by the audio thread.
///
/// A microphone that cannot be opened never stops audio capture: the desktop
/// audio keeps recording, and the thread keeps retrying with backoff.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MicState {
    /// Not recorded: turned off in the settings, or audio capture is not
    /// running.
    #[default]
    Off,
    /// Looking for the default recording device.
    Starting,
    /// Recording, mixed into the clip audio.
    Recording,
    /// There is no recording device.
    NotFound,
    /// The Windows privacy settings deny desktop apps the microphone.
    Denied,
    /// The device exists but could not be opened or was lost (for example
    /// held in exclusive mode by another app, or in a format the audio
    /// engine cannot convert).
    Unavailable,
}

/// Why an endpoint is not open.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
pub(crate) enum OpenFailure {
    /// There is no default endpoint of this kind.
    NotFound,
    /// The privacy settings deny access.
    Denied,
    /// Anything else.
    Other,
}

#[cfg(any(windows, test))]
impl MicState {
    /// The state of a microphone endpoint that has a stream open (`open`),
    /// or else last failed to open with `failure` (`None` before the first
    /// attempt).
    pub(crate) const fn of_endpoint(open: bool, failure: Option<OpenFailure>) -> Self {
        if open {
            return Self::Recording;
        }
        match failure {
            None => Self::Starting,
            Some(OpenFailure::NotFound) => Self::NotFound,
            Some(OpenFailure::Denied) => Self::Denied,
            Some(OpenFailure::Other) => Self::Unavailable,
        }
    }

    /// Every state, in [`Self::to_u8`] order.
    const ALL: [Self; 6] = [
        Self::Off,
        Self::Starting,
        Self::Recording,
        Self::NotFound,
        Self::Denied,
        Self::Unavailable,
    ];

    /// The state as a byte, for an atomic shared with the audio thread.
    pub(crate) const fn to_u8(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Starting => 1,
            Self::Recording => 2,
            Self::NotFound => 3,
            Self::Denied => 4,
            Self::Unavailable => 5,
        }
    }

    /// The state [`Self::to_u8`] encoded as `byte`; [`Self::Off`] for any
    /// other byte.
    pub(crate) fn from_u8(byte: u8) -> Self {
        Self::ALL
            .into_iter()
            .find(|state| state.to_u8() == byte)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_round_trip_through_a_byte() {
        for state in MicState::ALL {
            assert_eq!(MicState::from_u8(state.to_u8()), state);
        }
        assert_eq!(MicState::from_u8(200), MicState::Off);
        assert_eq!(MicState::default(), MicState::Off);
    }

    #[test]
    fn an_open_stream_records_whatever_failed_before() {
        let failures = [
            None,
            Some(OpenFailure::NotFound),
            Some(OpenFailure::Denied),
            Some(OpenFailure::Other),
        ];
        for failure in failures {
            assert_eq!(MicState::of_endpoint(true, failure), MicState::Recording);
        }
        let closed = failures.map(|failure| MicState::of_endpoint(false, failure));
        assert_eq!(
            closed,
            [
                MicState::Starting,
                MicState::NotFound,
                MicState::Denied,
                MicState::Unavailable
            ]
        );
    }
}
