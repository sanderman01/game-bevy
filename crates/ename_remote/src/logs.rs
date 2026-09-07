//! The method that reads the engine's log buffer.
//!
//! Structured records rather than formatted lines: level, target, message and timestamp are
//! separate fields, so the agent filters on them instead of pattern-matching rendered text.
//!
//! The buffer itself is `ename_engine::log`, because the editor's console reads it too.

use bevy::{
    log::Level,
    prelude::*,
    remote::{BrpResult, builtin_methods::parse},
};
use ename_engine::log::LogBuffer;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const GET_METHOD: &str = "game.logs.get";

/// How many entries one call returns when it does not say.
const DEFAULT_LIMIT: usize = 100;

/// One tracing event, as the agent sees it.
///
/// The buffer interns targets and keeps the level as a `tracing::Level`. Both are in-process
/// optimisations with no business crossing a socket, so this type spells them out.
#[derive(Serialize)]
pub(crate) struct Entry {
    /// Monotonic, so the agent can ask for "everything after what I already read".
    sequence: u64,
    /// Seconds since the process started.
    timestamp: f64,
    level: &'static str,
    target: String,
    message: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct GetParams {
    /// Lowest level to return, by name: `TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR`.
    min_level: Option<String>,
    /// Case-insensitive substring of the event's target, which is the emitting module path.
    target_contains: Option<String>,
    /// Case-insensitive substring of the message.
    message_contains: Option<String>,
    /// Only entries with a sequence number above this. For polling without re-reading.
    after_sequence: Option<u64>,
    limit: Option<usize>,
}

#[derive(Serialize)]
pub(crate) struct GetResponse {
    entries: Vec<Entry>,
    /// Matching entries older than the ones returned. The response holds the newest.
    older_matches: usize,
}

pub(crate) fn get(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: GetParams = params.map(parse).transpose()?.unwrap_or_default();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    let min_level = params.min_level.as_deref().map(level_of_name).transpose()?;
    let target_needle = params.target_contains.map(|t| t.to_lowercase());
    let message_needle = params.message_contains.map(|m| m.to_lowercase());

    let Some(buffer) = world.get_resource::<LogBuffer>() else {
        return crate::to_value(GetResponse {
            entries: Vec::new(),
            older_matches: 0,
        });
    };

    let matches: Vec<Entry> = buffer.read(|view| {
        view.iter()
            // `tracing` orders levels by verbosity, so a more severe level compares as less.
            .filter(|entry| min_level.is_none_or(|min| entry.level <= min))
            .filter(|entry| params.after_sequence.is_none_or(|s| entry.sequence > s))
            .filter(|entry| {
                target_needle
                    .as_ref()
                    .is_none_or(|n| entry.target_name.to_lowercase().contains(n))
            })
            .filter(|entry| {
                message_needle
                    .as_ref()
                    .is_none_or(|n| entry.message.to_lowercase().contains(n))
            })
            .map(|entry| Entry {
                sequence: entry.sequence,
                timestamp: entry.timestamp,
                level: entry.level.as_str(),
                target: entry.target_name.to_owned(),
                message: entry.message.to_owned(),
            })
            .collect()
    });

    let older_matches = matches.len().saturating_sub(limit);
    crate::to_value(GetResponse {
        entries: matches.into_iter().skip(older_matches).collect(),
        older_matches,
    })
}

fn level_of_name(name: &str) -> Result<Level, bevy::remote::BrpError> {
    match name.to_ascii_uppercase().as_str() {
        "TRACE" => Ok(Level::TRACE),
        "DEBUG" => Ok(Level::DEBUG),
        "INFO" => Ok(Level::INFO),
        "WARN" => Ok(Level::WARN),
        "ERROR" => Ok(Level::ERROR),
        other => Err(bevy::remote::BrpError {
            code: bevy::remote::error_codes::INVALID_PARAMS,
            message: format!("`{other}` is not a level. Use TRACE, DEBUG, INFO, WARN or ERROR."),
            data: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::level_of_name;
    use bevy::log::Level;

    #[test]
    fn min_level_filters_on_a_reversed_ordering() {
        // `tracing::Level` orders by verbosity, not severity, and `min_level` filters with `<=`
        // because of it. Getting this backwards silently returns the levels nobody asked for.
        assert!(Level::ERROR < Level::WARN);
        assert!(Level::WARN < Level::INFO);
        assert!(Level::INFO < Level::DEBUG);
        assert!(Level::DEBUG < Level::TRACE);
    }

    #[test]
    fn level_names_are_case_insensitive_and_checked() {
        assert_eq!(level_of_name("warn").unwrap(), Level::WARN);
        assert_eq!(level_of_name("WARN").unwrap(), Level::WARN);
        assert!(level_of_name("LOUD").is_err());
    }
}
