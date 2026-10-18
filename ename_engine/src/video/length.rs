//! How long a recording runs.

use std::{num::NonZeroU32, time::Duration};

use super::VideoError;
use crate::command::parse_timecode;

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

    /// A `hh:mm:ss` timecode (see [`parse_timecode`]) as a [`VideoLength::Time`].
    pub fn from_timecode(text: &str) -> Result<Self, VideoError> {
        parse_timecode(text)
            .map(Self::Time)
            .map_err(VideoError::InvalidDuration)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecodes_become_times() {
        assert_eq!(
            VideoLength::from_timecode("00:00:10.5"),
            Ok(VideoLength::Time(Duration::from_millis(10500)))
        );
        assert!(matches!(
            VideoLength::from_timecode("24:00:01"),
            Err(VideoError::InvalidDuration(_))
        ));
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
