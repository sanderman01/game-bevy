//! Archive mounts over a real filesystem: the committed fixtures in `tests/fixtures/archives`, and
//! small archives built per test for the cases a fixture can't show.

use ename_asset_vfs::{
    ArchiveVfs, DirEntry, Limits, MountProblemKind, Mounts, Origin, StdVfs, Vfs, VfsError,
};
use futures_lite::future::block_on;
use std::{
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};
use tempfile::TempDir;

const FIXTURES: &str = "tests/fixtures/archives";

/// A temporary asset root holding `files`, each `(path, bytes)`.
fn root_with(files: &[(&str, &[u8])]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, bytes) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    dir
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(FIXTURES).join(name)).unwrap()
}

/// A Stored zip of `members`. A name ending in `/` is a directory entry.
fn zip_of(members: &[(&str, &[u8])]) -> Vec<u8> {
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in members {
        if name.ends_with('/') {
            writer.add_directory(*name, options).unwrap();
        } else {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
    }
    writer.finish().unwrap().into_inner()
}

/// Every file under `dir`, relative to it, with its bytes.
fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                out.push((path.strip_prefix(dir).unwrap().to_path_buf(), bytes));
            }
        }
    }
    out.sort();
    out
}

fn listing(vfs: &dyn Vfs, path: &str) -> Vec<DirEntry> {
    block_on(vfs.read_dir(Path::new(path))).unwrap()
}

/// Each committed archive holds exactly `basic/`, byte for byte, and lists like it. This is what
/// keeps the fixtures from drifting from their source.
#[test]
fn every_fixture_archive_serves_the_basic_tree() {
    let source = tree(&Path::new(FIXTURES).join("basic"));
    let loose = StdVfs::new(Path::new(FIXTURES).join("basic"));
    for archive in ["basic.zip", "basic.tar", "zip64.zip"] {
        let extension = Path::new(archive).extension().unwrap().to_str().unwrap();
        let root = root_with(&[(&format!("pkg.{extension}"), &fixture(archive))]);
        let mounts = Mounts::new(root.path());
        for (path, bytes) in &source {
            let logical = Path::new("pkg").join(path);
            let read = mounts.read_file(&logical).expect("mounted").unwrap();
            assert_eq!(&read, bytes, "{archive}: {}", path.display());
        }
        let vfs = ArchiveVfs::new(root.path());
        for dir in ["", "sub", "sub/deep"] {
            let expected: Vec<DirEntry> = listing(&loose, dir)
                .into_iter()
                .map(|entry| DirEntry {
                    path: Path::new("pkg").join(entry.path),
                    is_dir: entry.is_dir,
                })
                .collect();
            let logical = Path::new("pkg").join(dir);
            let got = listing(&vfs, logical.to_str().unwrap());
            assert_eq!(got, expected, "{archive}: {dir}");
        }
        assert!(mounts.problems().is_empty(), "{archive}");
    }
}

/// The archive file itself never lists; its mount directory does, beside loose entries.
#[test]
fn a_listing_shows_an_archive_as_its_directory() {
    let root = root_with(&[
        ("mods/core.zip", &fixture("basic.zip")),
        ("mods/loose/x.txt", b"x"),
        ("mods/readme.txt", b"r"),
    ]);
    let vfs = ArchiveVfs::new(root.path());
    let entries = listing(&vfs, "mods");
    let expected = [
        ("mods/core", true),
        ("mods/loose", true),
        ("mods/readme.txt", false),
    ];
    let got: Vec<(&str, bool)> = entries
        .iter()
        .map(|e| (e.path.to_str().unwrap(), e.is_dir))
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn macosx_metadata_is_skipped() {
    let root = root_with(&[("pkg.zip", &fixture("macosx.zip"))]);
    let vfs = ArchiveVfs::new(root.path());
    let names: Vec<PathBuf> = listing(&vfs, "pkg").into_iter().map(|e| e.path).collect();
    assert_eq!(names, [PathBuf::from("pkg/a.txt")]);
}

#[test]
fn a_nested_archive_is_an_ordinary_file() {
    let inner = zip_of(&[("deep.txt", b"deep")]);
    let outer = zip_of(&[("inner.zip", &inner)]);
    let root = root_with(&[("pkg.zip", &outer)]);
    let vfs = ArchiveVfs::new(root.path());
    let entries = listing(&vfs, "pkg");
    assert_eq!(
        entries,
        [DirEntry {
            path: "pkg/inner.zip".into(),
            is_dir: false
        }]
    );
    assert_eq!(
        block_on(vfs.read_file(Path::new("pkg/inner.zip"))).unwrap(),
        inner
    );
    assert!(matches!(
        block_on(vfs.read_file(Path::new("pkg/inner/deep.txt"))),
        Err(VfsError::NotFound(_))
    ));
}

#[test]
fn origin_classifies_paths_whether_or_not_they_exist() {
    let root = root_with(&[("a/pkg.zip", &fixture("basic.zip")), ("a/loose.txt", b"l")]);
    let mounts = Mounts::new(root.path());
    let archive = root.path().join("a/pkg.zip");
    assert_eq!(mounts.origin(Path::new("a/loose.txt")), Origin::Loose);
    assert_eq!(mounts.origin(Path::new("a/new/thing.txt")), Origin::Loose);
    assert_eq!(
        mounts.origin(Path::new("a/pkg/sub/deep/c.txt")),
        Origin::Member {
            archive: archive.clone(),
            member: "sub/deep/c.txt".into()
        }
    );
    for path in ["a/pkg", "a/pkg/sub", "a/pkg/new.txt", "a/pkg/new/dir/x.txt"] {
        assert_eq!(
            mounts.origin(Path::new(path)),
            Origin::InMount {
                archive: archive.clone()
            },
            "{path}"
        );
    }
    // A filesystem path, absolute and not yet existing.
    assert!(matches!(
        mounts.origin_of_file(&root.path().join("a/pkg/new.scn.ron")),
        Origin::InMount { .. }
    ));
    assert_eq!(
        mounts.origin_of_file(&root.path().join("a/new.scn.ron")),
        Origin::Loose
    );
}

#[test]
fn a_directory_shadows_an_archive_of_the_same_name() {
    let root = root_with(&[
        ("core/x.txt", b"loose"),
        ("core.zip", &fixture("basic.zip")),
    ]);
    let vfs = ArchiveVfs::new(root.path());
    let entries = listing(&vfs, "");
    assert_eq!(
        entries,
        [DirEntry {
            path: "core".into(),
            is_dir: true
        }]
    );
    assert_eq!(
        block_on(vfs.read_file(Path::new("core/x.txt"))).unwrap(),
        b"loose"
    );
    let problems = vfs.mounts().problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].kind, MountProblemKind::ShadowedArchive);
    assert_eq!(problems[0].physical, root.path().join("core.zip"));
}

/// A conflicted mount still lists, so the scan reports it, and reading it is an IO error rather
/// than `NotFound`, which would let the package vanish quietly.
#[test]
fn a_zip_and_a_tar_of_the_same_name_conflict() {
    let root = root_with(&[
        ("core.zip", &fixture("basic.zip")),
        ("core.tar", &fixture("basic.tar")),
        ("other.zip", &fixture("basic.zip")),
    ]);
    let vfs = ArchiveVfs::new(root.path());
    let names: Vec<PathBuf> = listing(&vfs, "").into_iter().map(|e| e.path).collect();
    assert_eq!(names, [PathBuf::from("core"), PathBuf::from("other")]);
    assert!(matches!(
        block_on(vfs.read_file(Path::new("core/manifest.toml"))),
        Err(VfsError::Io { .. })
    ));
    assert!(block_on(vfs.read_file(Path::new("other/manifest.toml"))).is_ok());
    let kinds: Vec<MountProblemKind> = vfs.mounts().problems().iter().map(|p| p.kind).collect();
    assert_eq!(
        kinds,
        [
            MountProblemKind::ConflictingArchive,
            MountProblemKind::ConflictingArchive
        ]
    );
}

#[test]
fn a_file_beside_an_archive_of_its_name_conflicts() {
    let root = root_with(&[("core", b"a file"), ("core.zip", &fixture("basic.zip"))]);
    let mounts = Mounts::new(root.path());
    let err = mounts
        .read_file(Path::new("core/manifest.toml"))
        .unwrap()
        .unwrap_err();
    assert_ne!(err.kind(), ErrorKind::NotFound);
    assert_eq!(
        mounts.problems()[0].kind,
        MountProblemKind::ConflictingArchive
    );
}

/// A corrupt archive costs that mount and nothing else.
#[test]
fn a_truncated_archive_is_unavailable_without_hiding_its_siblings() {
    let root = root_with(&[
        ("mods/bad.zip", &fixture("bad/truncated.zip")),
        ("mods/good.zip", &fixture("basic.zip")),
    ]);
    let vfs = ArchiveVfs::new(root.path());
    let names: Vec<PathBuf> = listing(&vfs, "mods").into_iter().map(|e| e.path).collect();
    assert_eq!(
        names,
        [PathBuf::from("mods/bad"), PathBuf::from("mods/good")]
    );
    assert!(matches!(
        block_on(vfs.read_dir(Path::new("mods/bad"))),
        Err(VfsError::Io { .. })
    ));
    assert!(matches!(
        block_on(vfs.read_file(Path::new("mods/bad/manifest.toml"))),
        Err(VfsError::Io { .. })
    ));
    assert!(block_on(vfs.read_file(Path::new("mods/good/manifest.toml"))).is_ok());
    let problems = vfs.mounts().problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].kind, MountProblemKind::CorruptArchive);
    assert_eq!(problems[0].logical, Path::new("mods/bad"));
}

#[test]
fn a_member_whose_crc_does_not_match_is_invalid_data() {
    let root = root_with(&[("pkg.zip", &fixture("bad/crc.zip"))]);
    let mounts = Mounts::new(root.path());
    let err = mounts
        .read_file(Path::new("pkg/a.txt"))
        .unwrap()
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidData, "{err}");
    assert!(mounts.read_file(Path::new("pkg/b.ron")).unwrap().is_ok());
    let problems = mounts.problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].detail.contains("a.txt"), "{}", problems[0]);
}

/// A member that inflates past the size it declares is cut off there, never decoded in full.
#[test]
fn a_member_that_inflates_past_its_declared_size_is_refused() {
    let mut bytes = fixture("basic.zip");
    // `sub/deep/c.txt` inflates to 308 bytes. Declare 100 in the central directory.
    let name = b"sub/deep/c.txt";
    let central = (0..bytes.len() - 46)
        .find(|&i| bytes[i..i + 4] == [0x50, 0x4b, 0x01, 0x02] && bytes[i + 46..].starts_with(name))
        .unwrap();
    bytes[central + 24..central + 28].copy_from_slice(&100u32.to_le_bytes());
    let root = root_with(&[("pkg.zip", &bytes)]);
    let mounts = Mounts::new(root.path());
    let err = mounts
        .read_file(Path::new("pkg/sub/deep/c.txt"))
        .unwrap()
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidData, "{err}");
}

#[test]
fn limits_refuse_an_archive_before_reading_its_members() {
    let root = root_with(&[("pkg.zip", &fixture("basic.zip"))]);
    let too_many = Mounts::new(root.path()).with_limits(Limits {
        max_members: 3,
        ..Limits::default()
    });
    let too_big = Mounts::new(root.path()).with_limits(Limits {
        max_member_size: 100,
        ..Limits::default()
    });
    for mounts in [too_many, too_big] {
        assert!(mounts.read_file(Path::new("pkg/a.txt")).unwrap().is_err());
        assert_eq!(mounts.problems()[0].kind, MountProblemKind::CorruptArchive);
    }
}

fn tar_of(build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    build(&mut builder);
    builder.into_inner().unwrap()
}

fn tar_file(builder: &mut tar::Builder<Vec<u8>>, path: &str, bytes: &[u8]) {
    let mut header = tar::Header::new_ustar();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, path, bytes).unwrap();
}

#[test]
fn a_tar_with_a_link_or_a_duplicate_is_refused() {
    let link = tar_of(|builder| {
        tar_file(builder, "a.txt", b"a");
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        builder.append_link(&mut header, "b.txt", "a.txt").unwrap();
    });
    let duplicate = tar_of(|builder| {
        tar_file(builder, "a.txt", b"a");
        tar_file(builder, "./a.txt", b"b");
    });
    let collision = zip_of(&[("a", b"file"), ("a/b.txt", b"under it")]);
    for (name, bytes) in [
        ("link.tar", link),
        ("duplicate.tar", duplicate),
        ("collision.zip", collision),
    ] {
        let root = root_with(&[(name, &bytes)]);
        let mounts = Mounts::new(root.path());
        let stem = Path::new(name).with_extension("");
        let read = mounts.read_file(&stem.join("a.txt")).unwrap();
        assert!(read.is_err(), "{name}");
        let problems = mounts.problems();
        assert_eq!(problems.len(), 1, "{name}: {problems:?}");
        assert_eq!(problems[0].kind, MountProblemKind::CorruptArchive, "{name}");
    }
}
