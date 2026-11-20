//! Named points in a commit where a test can stop it, as if the process had died there.

use crate::StorageError;
use std::{
    fmt,
    sync::{Arc, Mutex, PoisonError},
};

/// A point in a commit. A backend implements the points that exist in its commit and lists them
/// for the conformance suite; the names follow the filesystem backend's sequence, the one that
/// implements them all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaultPoint {
    /// The new records are prepared and durable; nothing is committed.
    Staged,
    /// The journal is written under its temporary name, not yet renamed into place.
    JournalWritten,
    /// The commit point has passed: the journal is in place, nothing is applied.
    Committed,
    /// One record's old version has been moved aside. Once per replaced or removed record that
    /// existed.
    OldMovedAside,
    /// One record's new version has been moved in. Once per replaced record.
    NewMovedIn,
    /// Every record is applied; the journal is still in place.
    Applied,
    /// The journal is removed; leftovers are not yet cleaned up.
    JournalRemoved,
}

impl FaultPoint {
    pub const ALL: [Self; 7] = [
        Self::Staged,
        Self::JournalWritten,
        Self::Committed,
        Self::OldMovedAside,
        Self::NewMovedIn,
        Self::Applied,
        Self::JournalRemoved,
    ];

    /// Whether the commit point has passed, so a stop here must recover to the new state.
    pub fn is_committed(self) -> bool {
        !matches!(self, Self::Staged | Self::JournalWritten)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::JournalWritten => "journal_written",
            Self::Committed => "committed",
            Self::OldMovedAside => "old_moved_aside",
            Self::NewMovedIn => "new_moved_in",
            Self::Applied => "applied",
            Self::JournalRemoved => "journal_removed",
        }
    }
}

impl fmt::Display for FaultPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Arms one fault point; shared between a test and the backend it opened. Unarmed, which is how a
/// backend runs outside tests, [`FaultSwitch::check`] is a lock and a compare.
#[derive(Clone, Debug, Default)]
pub struct FaultSwitch(Arc<Mutex<Option<Armed>>>);

#[derive(Clone, Copy, Debug)]
struct Armed {
    point: FaultPoint,
    /// Passes of `point` still to let through.
    skip: u32,
}

impl FaultSwitch {
    /// Stops the commit the `occurrence`-th time (from 0) it passes `point`. Replaces an earlier
    /// arming.
    pub fn arm(&self, point: FaultPoint, occurrence: u32) {
        *self.lock() = Some(Armed {
            point,
            skip: occurrence,
        });
    }

    pub fn disarm(&self) {
        *self.lock() = None;
    }

    /// Called by a backend as its commit passes `point`. Returns [`StorageError::Fault`] when the
    /// switch is armed for this pass, and disarms; the backend then stops at once, leaving
    /// everything as it is, and behaves as a dead process: every later call fails until the store
    /// is opened again.
    pub fn check(&self, point: FaultPoint) -> Result<(), StorageError> {
        let mut armed = self.lock();
        let Some(current) = armed.as_mut() else {
            return Ok(());
        };
        if current.point != point {
            return Ok(());
        }
        if current.skip > 0 {
            current.skip -= 1;
            return Ok(());
        }
        *armed = None;
        Err(StorageError::Fault(point))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Armed>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_at_the_armed_occurrence_once() {
        let faults = FaultSwitch::default();
        assert!(faults.check(FaultPoint::Staged).is_ok());
        faults.arm(FaultPoint::NewMovedIn, 1);
        assert!(faults.check(FaultPoint::Staged).is_ok());
        assert!(faults.check(FaultPoint::NewMovedIn).is_ok());
        assert!(matches!(
            faults.check(FaultPoint::NewMovedIn),
            Err(StorageError::Fault(FaultPoint::NewMovedIn))
        ));
        assert!(faults.check(FaultPoint::NewMovedIn).is_ok());
    }

    #[test]
    fn only_staging_points_are_before_the_commit() {
        let before: Vec<_> = FaultPoint::ALL
            .into_iter()
            .filter(|point| !point.is_committed())
            .collect();
        assert_eq!(before, [FaultPoint::Staged, FaultPoint::JournalWritten]);
    }
}
