//! A length param shared by commands that run for a while: frames, seconds or a timecode.

use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;

/// The longest timecode accepted.
const MAX_TIMECODE: Duration = Duration::from_secs(24 * 60 * 60);

/// A length with its unit as the key, so `10` is never read as frames when seconds were meant.
// Named `Duration` in schemas: `video.record` published it under that name.
#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[schemars(rename = "Duration")]
pub enum Length {
    Frames(u32),
    Seconds(f64),
    Timecode(String),
}

/// Parses `H:MM:SS` or `H:MM:SS.fff`. Hours may have any number of digits; minutes and seconds
/// are two digits below 60. The result must be above zero and at most 24 hours. The error is a
/// message for the caller.
pub fn parse_timecode(text: &str) -> Result<Duration, String> {
    let invalid = || format!("`{text}` is not a timecode of the form hh:mm:ss or hh:mm:ss.fff");
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
    if length.is_zero() || length > MAX_TIMECODE {
        return Err(format!("{length:?} is not between 0 and 24 hours"));
    }
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
            assert!(parse_timecode(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn lengths_take_their_unit_as_the_key() {
        let parse = |json| serde_json::from_value::<Length>(json);
        assert_eq!(
            parse(serde_json::json!({"frames": 600})).unwrap(),
            Length::Frames(600)
        );
        assert_eq!(
            parse(serde_json::json!({"timecode": "00:01:30"})).unwrap(),
            Length::Timecode("00:01:30".into())
        );
        assert!(parse(serde_json::json!(10)).is_err());
        assert!(parse(serde_json::json!({"seconds": 1, "frames": 2})).is_err());
    }
}
