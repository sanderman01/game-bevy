//! Shared test support: copying a checked-in fixture tree into a scratch `TempDir` so tests that
//! write files -- `ename_fix`, `ename_mv` -- never touch what is checked into the repository.

use std::path::Path;
use tempfile::TempDir;

/// Copies `fixture` (a directory under `tests/fixtures/`) into a fresh temporary directory and
/// returns it. Keep the `TempDir` alive for the length of the test; it cleans itself up on drop.
pub fn copy_fixture(fixture: &str) -> TempDir {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let dir = TempDir::new().expect("a scratch directory");
    copy_dir(&src, dir.path());
    dir
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create the destination directory");
    for entry in std::fs::read_dir(src).expect("read the fixture directory") {
        let entry = entry.expect("a directory entry");
        let dst_path = dst.join(entry.file_name());
        if entry.file_type().expect("a file type").is_dir() {
            copy_dir(&entry.path(), &dst_path);
        } else {
            std::fs::copy(entry.path(), &dst_path).expect("copy a fixture file");
        }
    }
}
