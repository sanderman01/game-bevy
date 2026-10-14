//! The tooling over archived packages: `ename_check` fails on a broken archive, `ename_fix` leaves
//! archived assets alone, and `ename_mv` refuses to move into or out of one.

use ename_xtask::{
    check::check,
    fix::fix,
    mv::{MvError, mv},
};
use std::{io::Write, path::Path};
use tempfile::TempDir;

const ARCHIVES: &str = "../ename_asset_vfs/tests/fixtures/archives";

fn root_with(files: &[(&str, &[u8])]) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    for (path, bytes) in files {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    root
}

fn archive(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(ARCHIVES).join(name)).unwrap()
}

/// The `clean` fixture's package, zipped.
fn clean_zip() -> Vec<u8> {
    let package = Path::new("tests/fixtures/clean/basegame/core");
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for name in [
        "manifest.toml",
        "ships/airship.glb",
        "ships/airship.glb.alias",
    ] {
        let Ok(bytes) = std::fs::read(package.join(name)) else {
            continue;
        };
        writer.start_file(name, options).unwrap();
        writer.write_all(&bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
fn check_passes_a_clean_archive_and_fails_a_corrupt_one() {
    let clean = root_with(&[("basegame/core.zip", &clean_zip())]);
    assert!(check(clean.path(), &["basegame".to_owned()], None));
    let corrupt = root_with(&[
        ("basegame/core.zip", &clean_zip()),
        ("basegame/bad.zip", &archive("bad/truncated.zip")),
    ]);
    assert!(!check(corrupt.path(), &["basegame".to_owned()], None));
}

#[test]
fn fix_skips_assets_inside_an_archive() {
    let root = root_with(&[("basegame/basic.zip", &archive("basic.zip"))]);
    let summary = fix(root.path(), &["basegame".to_owned()], None);
    // `b.ron` and `sub/deep/c.txt` have no sidecar; `a.txt`'s has a guid.
    assert_eq!(summary.archived, 2, "{summary:?}");
    assert_eq!(summary.created + summary.guids_assigned + summary.failed, 0);
    assert!(!root.path().join("basegame/basic").exists());
}

#[test]
fn mv_refuses_paths_inside_an_archive() {
    let root = root_with(&[
        ("basegame/basic.zip", &archive("basic.zip")),
        ("loose.txt", b"loose"),
    ]);
    let inside = root.path().join("basegame/basic/a.txt");
    let loose = root.path().join("loose.txt");
    assert!(matches!(
        mv(&inside, &root.path().join("out.txt")),
        Err(MvError::InsideArchive { .. })
    ));
    assert!(matches!(
        mv(&loose, &root.path().join("basegame/basic/new.txt")),
        Err(MvError::InsideArchive { .. })
    ));
    assert!(loose.exists());
    assert!(!root.path().join("basegame/basic").exists());
}
