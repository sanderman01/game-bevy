//! How long a recording runs, and the `hh:mm:ss` form of it.

use std::{num::NonZeroU32, time::Duration};

use super::VideoError;

/// The longest recording accepted.
const MAX_LENGTH: Duration = Duration::from_secs(24 * 60 * 60);

/// How long to record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoLength {
    /// Exactly this many frames in the file, one per rendered frame the encoder accepted. Plays
    /// back in real time only if the game ran at the recording's frame rate.
    Frames(NonZeroU32),
    /// This much real time, resampled to a constant frame rate.
    Time(Duration),
}

impl VideoLength {
    /// `frames` as a [`VideoLength::Frames`], if it is at least 1.
    pub fn from_frames(frames: u32) -> Result<Self, VideoError> {
        NonZeroU32::new(frames)
            .map(Self::Frames)
            .ok_or_else(|| VideoError::InvalidDuration("0 frames".into()))
    }

    /// `seconds` as a [`VideoLength::Time`], if it is finite, positive and at most 24 h.
    pub fn from_seconds(seconds: f64) -> Result<Self, VideoError> {
        if !seconds.is_finite() || seconds <= 0.0 || seconds > MAX_LENGTH.as_secs_f64() {
            return Err(VideoError::InvalidDuration(format!(
                "{seconds} seconds is not between 0 and 24 hours"
            )));
        }
        Ok(Self::Time(Duration::from_secs_f64(seconds)))
    }

    pub(crate) fn validate(self) -> Result<(), VideoError> {
        match self {
            Self::Frames(_) => Ok(()),
            Self::Time(length) if length.is_zero() || length > MAX_LENGTH => Err(
                VideoError::InvalidDuration(format!("{length:?} is not between 0 and 24 hours")),
            ),
            Self::Time(_) => Ok(()),
        }
    }
}

/// Parses `H:MM:SS` or `H:MM:SS.fff`. Hours may have any number of digits; minutes and seconds
/// are two digits below 60. The result must be above zero and at most 24 hours.
pub fn parse_timecode(text: &str) -> Result<Duration, VideoError> {
    let invalid = || {
        VideoError::InvalidDuration(format!(
            "`{text}` is not a timecode of the form hh:mm:ss or hh:mm:ss.fff"
        ))
    };
    let mut parts = text.split(':');
    let (Some(hours), Some(minutes), Some(seconds), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid());
    };
    let (whole, fraction) = match seconds.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (seconds, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(hours)
        || minutes.len() != 2
        || !digits(minutes)
        || whole.len() != 2
        || !digits(whole)
        || fraction.is_some_and(|f| !digits(f) || f.len() > 9)
    {
        return Err(invalid());
    }
    let hours: u64 = hours.parse().map_err(|_| invalid())?;
    let minutes: u64 = minutes.parse().map_err(|_| invalid())?;
    let whole: u64 = whole.parse().map_err(|_| invalid())?;
    if minutes >= 60 || whole >= 60 {
        return Err(invalid());
    }
    let nanos = match fraction {
        Some(fraction) => format!("{fraction:0<9}").parse().map_err(|_| invalid())?,
        None => 0,
    };
    let length = hours
        .checked_mul(3600)
        .and_then(|h| h.checked_add(minutes * 60 + whole))
        .map(|secs| Duration::new(secs, nanos))
        .ok_or_else(invalid)?;
    VideoLength::Time(length).validate()?;
    Ok(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecodes_parse() {
        assert_eq!(parse_timecode("00:00:10"), Ok(Duration::from_secs(10)));
        assert_eq!(parse_timecode("1:30:00"), Ok(Duration::from_secs(5400)));
        assert_eq!(
            parse_timecode("00:00:10.5"),
            Ok(Duration::from_millis(10500))
        );
        assert_eq!(parse_timecode("24:00:00"), Ok(Duration::from_secs(86400)));
    }

    #[test]
    fn bad_timecodes_are_rejected() {
        for bad in [
            "100:00:00",
            "24:00:01",
            "00:60:00",
            "00:00:60",
            "10",
            "aa:bb:cc",
            "",
            "00:00:00",
            "0:0:10",
            "00:00:10.",
            "00:00:10.x",
            "-1:00:00",
            "00:00:00:10",
        ] {
            assert!(
                matches!(parse_timecode(bad), Err(VideoError::InvalidDuration(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn seconds_are_validated() {
        assert_eq!(
            VideoLength::from_seconds(10.5),
            Ok(VideoLength::Time(Duration::from_millis(10500)))
        );
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 86400.5] {
            assert!(VideoLength::from_seconds(bad).is_err(), "{bad}");
        }
    }
}
