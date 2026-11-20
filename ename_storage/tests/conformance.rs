//! The conformance suite against both shipped backends, as a third-party backend would run it.

use ename_storage::{
    FaultPoint, FaultSwitch, FsStorage, MemoryStorage, UserStorage,
    conformance::{Harness, run_all},
};
use tempfile::TempDir;

struct Memory;

impl Harness for Memory {
    fn reset(&mut self) {}

    fn open(&mut self, faults: FaultSwitch) -> UserStorage {
        UserStorage::new(MemoryStorage::with_faults(faults))
    }

    fn persistent(&self) -> bool {
        false
    }

    fn fault_points(&self) -> Vec<FaultPoint> {
        MemoryStorage::FAULT_POINTS.to_vec()
    }
}

/// A store in a fresh temporary directory per check.
struct Filesystem(TempDir);

impl Harness for Filesystem {
    fn reset(&mut self) {
        self.0 = tempfile::tempdir().unwrap();
    }

    fn open(&mut self, faults: FaultSwitch) -> UserStorage {
        UserStorage::new(FsStorage::open_with_faults(
            self.0.path().join("storage"),
            faults,
        ))
    }

    fn persistent(&self) -> bool {
        true
    }

    fn fault_points(&self) -> Vec<FaultPoint> {
        FsStorage::FAULT_POINTS.to_vec()
    }
}

#[test]
fn memory_backend_conforms() {
    run_all(&mut Memory);
}

#[test]
fn filesystem_backend_conforms() {
    run_all(&mut Filesystem(tempfile::tempdir().unwrap()));
}
