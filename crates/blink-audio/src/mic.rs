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

#[cfg(any(windows, test))]
impl MicState {
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
}
