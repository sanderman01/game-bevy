//! The method that reads the engine's log buffer.
//!
//! Structured records rather than formatted lines: level, target, message and timestamp are
//! separate fields, so the agent filters on them instead of pattern-matching rendered text.
//!
//! The buffer itself is `ename_engine::log`, because the editor's console reads it too.

use bevy::{
    prelude::*,
    remote::{BrpResult, builtin_methods::parse},
};
use ename_engine::log::{LogBuffer, LogFilter};
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
    let filter = LogFilter::new(
        params.min_level.as_deref(),
        params.target_contains.as_deref(),
        params.message_contains.as_deref(),
        params.after_sequence,
    )
    .map_err(|err| bevy::remote::BrpError {
        code: bevy::remote::error_codes::INVALID_PARAMS,
        message: err.to_string(),
        data: None,
    })?;

    let Some(buffer) = world.get_resource::<LogBuffer>() else {
        return crate::to_value(GetResponse {
            entries: Vec::new(),
            older_matches: 0,
        });
    };

    let matches: Vec<Entry> = buffer.read(|view| {
        view.iter()
            .filter(|entry| filter.matches(entry))
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
