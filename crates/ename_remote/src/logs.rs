//! A bounded ring buffer of recent tracing events, and the method that drains it.
//!
//! Structured records rather than formatted lines: level, target, message and timestamp are
//! separate fields, so the agent filters on them instead of pattern-matching rendered text.

use std::sync::{Arc, Mutex};

use bevy::{
    log::BoxedLayer,
    prelude::*,
    remote::{BrpResult, builtin_methods::parse},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;

pub const GET_METHOD: &str = "game.logs.get";

/// Entries kept before the oldest is dropped. Large enough to hold a startup sequence,
/// small enough that the buffer is not a memory leak in a session that runs for hours.
const CAPACITY: usize = 4096;

/// How many entries one call returns when it does not say.
const DEFAULT_LIMIT: usize = 100;

/// One tracing event, as the agent sees it.
#[derive(Clone, Serialize)]
pub(crate) struct LogEntry {
    /// Monotonic, so the agent can ask for "everything after what I already read".
    sequence: u64,
    /// Seconds since the process started.
    timestamp: f64,
    level: &'static str,
    target: String,
    message: String,
}

/// The buffer itself.
///
/// Shared with a `tracing` layer that is installed before the `App` exists, so it is an `Arc`
/// rather than plain resource data. `Mutex` and not a channel because the reader wants the
/// last N entries, not the ones that arrived since it last looked.
#[derive(Resource, Clone, Default)]
pub(crate) struct LogBuffer(Arc<Mutex<Ring>>);

#[derive(Default)]
pub(crate) struct Ring {
    entries: std::collections::VecDeque<LogEntry>,
    next_sequence: u64,
}

impl LogBuffer {
    fn push(&self, timestamp: f64, level: &'static str, target: String, message: String) {
        let Ok(mut ring) = self.0.lock() else {
            // A poisoned lock means a previous writer panicked mid-push. Losing log entries is
            // not worth taking the process down for.
            return;
        };
        let sequence = ring.next_sequence;
        ring.next_sequence += 1;
        if ring.entries.len() == CAPACITY {
            ring.entries.pop_front();
        }
        ring.entries.push_back(LogEntry {
            sequence,
            timestamp,
            level,
            target,
            message,
        });
    }

    fn snapshot(&self) -> Vec<LogEntry> {
        self.0
            .lock()
            .map(|ring| ring.entries.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// The layer handed to `LogPlugin::custom_layer`.
///
/// Also inserts the buffer as a resource, which is why it takes the `App`: the layer and the
/// method have to share one buffer and this is the only point where both are reachable.
pub fn capture_layer(app: &mut App) -> Option<BoxedLayer> {
    let buffer = LogBuffer::default();
    app.insert_resource(buffer.clone());
    Some(Box::new(CaptureLayer {
        buffer,
        start: std::time::Instant::now(),
    }))
}

struct CaptureLayer {
    buffer: LogBuffer,
    start: std::time::Instant,
}

impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut message = MessageVisitor(String::new());
        event.record(&mut message);
        self.buffer.push(
            self.start.elapsed().as_secs_f64(),
            event.metadata().level().as_str(),
            event.metadata().target().to_owned(),
            message.0,
        );
    }
}

/// Pulls the `message` field out of an event, ignoring the structured fields around it.
struct MessageVisitor(String);

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_owned();
        }
    }
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
    entries: Vec<LogEntry>,
    /// Matching entries older than the ones returned. The response holds the newest.
    older_matches: usize,
}

pub(crate) fn get(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: GetParams = params.map(parse).transpose()?.unwrap_or_default();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    let min_level = params
        .min_level
        .as_deref()
        .map(severity_of_name)
        .transpose()?
        .unwrap_or(0);
    let target_needle = params.target_contains.map(|t| t.to_lowercase());
    let message_needle = params.message_contains.map(|m| m.to_lowercase());

    let Some(buffer) = world.get_resource::<LogBuffer>() else {
        return crate::position::to_value(GetResponse {
            entries: Vec::new(),
            older_matches: 0,
        });
    };

    let matches: Vec<LogEntry> = buffer
        .snapshot()
        .into_iter()
        .filter(|entry| severity_of_level(entry.level) >= min_level)
        .filter(|entry| params.after_sequence.is_none_or(|s| entry.sequence > s))
        .filter(|entry| {
            target_needle
                .as_ref()
                .is_none_or(|n| entry.target.to_lowercase().contains(n))
        })
        .filter(|entry| {
            message_needle
                .as_ref()
                .is_none_or(|n| entry.message.to_lowercase().contains(n))
        })
        .collect();

    let older_matches = matches.len().saturating_sub(limit);
    crate::position::to_value(GetResponse {
        entries: matches[older_matches..].to_vec(),
        older_matches,
    })
}

fn severity_of_level(level: &str) -> u8 {
    severity_of_name(level).unwrap_or(0)
}

fn severity_of_name(name: &str) -> Result<u8, bevy::remote::BrpError> {
    match name.to_ascii_uppercase().as_str() {
        "TRACE" => Ok(0),
        "DEBUG" => Ok(1),
        "INFO" => Ok(2),
        "WARN" => Ok(3),
        "ERROR" => Ok(4),
        other => Err(bevy::remote::BrpError {
            code: bevy::remote::error_codes::INVALID_PARAMS,
            message: format!("`{other}` is not a level. Use TRACE, DEBUG, INFO, WARN or ERROR."),
            data: None,
        }),
    }
}
