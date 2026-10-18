//! The log of failed loads. The asset server forgets a failed asset once its last handle drops,
//! so without this a failure that was not caught at the time leaves no trace but a log line.

use std::collections::{HashMap, VecDeque};

use bevy::{
    asset::{AssetPath, LoadState, UntypedAssetLoadFailedEvent},
    diagnostic::FrameCount,
    prelude::*,
};

use super::error_text;

/// Most failures kept; the oldest is dropped first.
pub(super) const CAPACITY: usize = 256;

pub(super) struct LoggedFailure {
    /// As requested, label included: a failed `#Scene0` is logged under the labeled path.
    pub path: AssetPath<'static>,
    pub error: String,
    /// `FrameCount` when it was read, or 0 without one.
    pub frame: u32,
    /// The path has loaded since. Kept for `previous_error`, but no longer a current failure.
    pub resolved: bool,
}

#[derive(Resource, Default)]
pub(super) struct AssetLoadFailures(VecDeque<LoggedFailure>);

impl AssetLoadFailures {
    /// The newest failure per path, keyed by the path's text. Built once per command call, so a
    /// lookup per alias does not scan the log.
    pub(super) fn latest_by_path(&self) -> HashMap<String, &LoggedFailure> {
        let mut latest = HashMap::new();
        for failure in &self.0 {
            latest.insert(failure.path.to_string(), failure);
        }
        latest
    }
}

pub(super) fn log_failures(
    mut failures: MessageReader<UntypedAssetLoadFailedEvent>,
    mut log: ResMut<AssetLoadFailures>,
    frame: Option<Res<FrameCount>>,
) {
    let frame = frame.map_or(0, |frame| frame.0);
    for failure in failures.read() {
        if log.0.len() == CAPACITY {
            log.0.pop_front();
        }
        log.0.push_back(LoggedFailure {
            path: failure.path.clone(),
            error: error_text(&failure.error),
            frame,
            resolved: false,
        });
    }
}

/// Marks a failure resolved once its path has a loaded asset, so a recovered asset whose handle
/// later drops does not read as failed again. Only unresolved entries are checked.
pub(super) fn resolve_failures(server: Res<AssetServer>, mut log: ResMut<AssetLoadFailures>) {
    // Skips the mutable borrow, and so change detection, on the usual frame with nothing to do.
    if log.0.iter().all(|failure| failure.resolved) {
        return;
    }
    for failure in log.0.iter_mut().filter(|failure| !failure.resolved) {
        failure.resolved = server
            .get_path_ids(&failure.path)
            .into_iter()
            .any(|id| matches!(server.get_load_state(id), Some(LoadState::Loaded)));
    }
}
