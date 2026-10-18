//! The log of failed loads. The asset server forgets a failed asset once its last handle drops,
//! so without this a failure that was not caught at the time leaves no trace but a log line.

use std::collections::VecDeque;

use bevy::{
    asset::{AssetPath, UntypedAssetLoadFailedEvent},
    diagnostic::FrameCount,
    prelude::*,
};

/// Most failures kept; the oldest is dropped first.
pub(super) const CAPACITY: usize = 256;

pub(super) struct LoggedFailure {
    pub path: AssetPath<'static>,
    pub error: String,
    /// `FrameCount` when it was read, or 0 without one.
    pub frame: u32,
}

#[derive(Resource, Default)]
pub(super) struct AssetLoadFailures(VecDeque<LoggedFailure>);

impl AssetLoadFailures {
    pub(super) fn iter(&self) -> impl DoubleEndedIterator<Item = &LoggedFailure> {
        self.0.iter()
    }

    /// The newest failure logged for exactly `path`, label included.
    pub(super) fn latest(&self, path: &AssetPath) -> Option<&LoggedFailure> {
        self.0.iter().rev().find(|failure| failure.path == *path)
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
            error: super::error_text(&failure.error),
            frame,
        });
    }
}
