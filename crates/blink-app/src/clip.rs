//! Naming saved clips and describing their length.

use std::fmt;

use blink_ring::media::Timestamp;

/// A local wall-clock time, to the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalTime {
    /// Calendar year.
    pub(crate) year: i32,
    /// Month, 1 to 12.
    pub(crate) month: u32,
    /// Day of the month, 1 to 31.
    pub(crate) day: u32,
    /// Hour, 0 to 23.
    pub(crate) hour: u32,
    /// Minute, 0 to 59.
    pub(crate) minute: u32,
    /// Second, 0 to 60 (leap second).
    pub(crate) second: u32,
}

/// `blink-YYYYMMDD-HHMMSS.mp4`. A nonzero `attempt` adds `-N` (counting from
/// 2) for a second clip saved within the same second.
pub(crate) fn file_name(time: LocalTime, attempt: u32) -> String {
    let LocalTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    } = time;
    let stem = format!("blink-{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}");
    match attempt {
        0 => format!("{stem}.mp4"),
        n => format!("{stem}-{}.mp4", n.saturating_add(1)),
    }
}

/// The hidden temp file a clip is written to before it is renamed to
/// `final_name`, so a half-written clip never has the final name.
pub(crate) fn temp_name(final_name: &str) -> String {
    format!(".{final_name}.partial")
}

/// Whether `name` is a temp file [`temp_name`] made, left behind when blink
/// stopped mid-save.
pub(crate) fn is_temp_name(name: &str) -> bool {
    name.strip_prefix(".blink-")
        .and_then(|rest| rest.strip_suffix(".mp4.partial"))
        .is_some_and(|stem| !stem.is_empty())
}

/// A duration shown as seconds with millisecond precision, such as `12.345 s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Seconds(pub(crate) Timestamp);

impl fmt::Display for Seconds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let millis = self.0.ticks() / (Timestamp::TICKS_PER_SECOND / 1000);
        let sign = if millis < 0 { "-" } else { "" };
        let millis = millis.unsigned_abs();
        write!(f, "{sign}{}.{:03} s", millis / 1000, millis % 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIME: LocalTime = LocalTime {
        year: 2026,
        month: 3,
        day: 7,
        hour: 9,
        minute: 5,
        second: 2,
    };

    #[test]
    fn file_name_is_zero_padded_local_time() {
        assert_eq!(file_name(TIME, 0), "blink-20260307-090502.mp4");
        let late = LocalTime {
            month: 12,
            day: 31,
            hour: 23,
            minute: 59,
            second: 59,
            ..TIME
        };
        assert_eq!(file_name(late, 0), "blink-20261231-235959.mp4");
    }

    #[test]
    fn collisions_get_a_counter() {
        assert_eq!(file_name(TIME, 1), "blink-20260307-090502-2.mp4");
        assert_eq!(file_name(TIME, 9), "blink-20260307-090502-10.mp4");
    }

    #[test]
    fn temp_name_is_hidden_and_distinct() {
        let name = file_name(TIME, 0);
        let temp = temp_name(&name);
        assert_eq!(temp, ".blink-20260307-090502.mp4.partial");
        assert_ne!(temp, name);
        assert!(is_temp_name(&temp));
        assert!(is_temp_name(&temp_name(&file_name(TIME, 3))));
    }

    #[test]
    fn only_blink_temp_files_are_stale() {
        for name in [
            "blink-20260307-090502.mp4",
            ".blink-.mp4.partial",
            ".other-20260307.mp4.partial",
            ".blink-20260307-090502.mp4",
            "notes.partial",
        ] {
            assert!(!is_temp_name(name), "{name}");
        }
    }

    #[test]
    fn seconds_display() {
        assert_eq!(Seconds(Timestamp::from_secs(30)).to_string(), "30.000 s");
        assert_eq!(Seconds(Timestamp(12_345_678)).to_string(), "1.234 s");
        assert_eq!(
            Seconds(Timestamp::from_millis(-1500)).to_string(),
            "-1.500 s"
        );
    }
}
