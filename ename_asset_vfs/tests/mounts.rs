//! Archive mounts over a real filesystem: the committed fixtures in `tests/fixtures/archives`, and
//! small archives built per test for the cases a fixture can't show.

use ename_asset_vfs::{
    ArchiveVfs, DirEntry, Limits, MountProblemKind, Mounts, Origin, StdVfs, Vfs, VfsError,
};
use futures_lite::future::block_on;
use std::{
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::Arc,
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
    assert_eq!(
        mounts.origin(Path::new("a/loose.txt")).unwrap(),
        Origin::Loose
    );
    assert_eq!(
        mounts.origin(Path::new("a/new/thing.txt")).unwrap(),
        Origin::Loose
    );
    assert_eq!(
        mounts.origin(Path::new("a/pkg/sub/deep/c.txt")).unwrap(),
        Origin::Member {
            archive: archive.clone(),
            member: "sub/deep/c.txt".into()
        }
    );
    for path in ["a/pkg", "a/pkg/sub", "a/pkg/new.txt", "a/pkg/new/dir/x.txt"] {
        assert_eq!(
            mounts.origin(Path::new(path)).unwrap(),
            Origin::InMount {
                archive: archive.clone()
            },
            "{path}"
        );
    }
    // A filesystem path, absolute and not yet existing.
    assert!(matches!(
        mounts
            .origin_of_file(&root.path().join("a/pkg/new.scn.ron"))
            .unwrap(),
        Origin::InMount { .. }
    ));
    assert_eq!(
        mounts
            .origin_of_file(&root.path().join("a/new.scn.ron"))
            .unwrap(),
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
    let vfs = ArchiveVfs::new(root.path());
    // The file wins, exactly as the platform reader would see it, and the archive never lists.
    assert_eq!(
        listing(&vfs, ""),
        [DirEntry {
            path: "core".into(),
            is_dir: false
        }]
    );
    let mounts = vfs.mounts();
    assert!(mounts.read_file(Path::new("core/manifest.toml")).is_none());
    assert!(mounts.is_dir(Path::new("core")).is_none());
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

/// A directory whose name ends in `.zip` is an ordinary directory.
#[test]
fn a_directory_named_like_an_archive_is_ordinary() {
    let root = root_with(&[("odd.zip/x.txt", b"x"), ("odd.tar/y.txt", b"y")]);
    let vfs = ArchiveVfs::new(root.path());
    let names: Vec<(PathBuf, bool)> = listing(&vfs, "")
        .into_iter()
        .map(|e| (e.path, e.is_dir))
        .collect();
    assert_eq!(names, [("odd.tar".into(), true), ("odd.zip".into(), true)]);
    assert_eq!(
        block_on(vfs.read_file(Path::new("odd.zip/x.txt"))).unwrap(),
        b"x"
    );
    assert_eq!(
        vfs.mounts().origin(Path::new("odd.zip/x.txt")).unwrap(),
        Origin::Loose
    );
    assert!(vfs.mounts().problems().is_empty());
}

/// Names differing only in case are separate mounts and directories on a case-sensitive
/// filesystem. The clash is a warning, not a problem; extensions match exactly, so `.ZIP` is a
/// plain file.
#[test]
fn case_only_differences_are_kept_apart() {
    let root = root_with(&[
        ("Core/x.txt", b"x"),
        ("core.zip", &fixture("basic.zip")),
        ("other.ZIP", &fixture("basic.zip")),
    ]);
    let vfs = ArchiveVfs::new(root.path());
    let names: Vec<(PathBuf, bool)> = listing(&vfs, "")
        .into_iter()
        .map(|e| (e.path, e.is_dir))
        .collect();
    assert_eq!(
        names,
        [
            ("Core".into(), true),
            ("core".into(), true),
            ("other.ZIP".into(), false)
        ]
    );
    assert!(block_on(vfs.read_file(Path::new("core/a.txt"))).is_ok());
    assert!(vfs.mounts().problems().is_empty());
}

#[test]
fn origin_normalizes_and_refuses_paths_outside_the_root() {
    let root = root_with(&[("pkg.zip", &fixture("basic.zip"))]);
    let mounts = Mounts::new(root.path());
    let archive = root.path().join("pkg.zip");
    assert!(matches!(
        mounts.origin(Path::new("other/../pkg/./a.txt")).unwrap(),
        Origin::Member { .. }
    ));
    assert!(matches!(
        mounts.origin(Path::new("pkg/../pkg/new.txt")).unwrap(),
        Origin::InMount { .. }
    ));
    assert_eq!(
        mounts.origin(Path::new("../outside.txt")).unwrap(),
        Origin::Outside
    );
    assert_eq!(
        mounts.origin(&root.path().join("x.txt")).unwrap(),
        Origin::Outside
    );
    assert_eq!(
        mounts.origin(Path::new("pkg.zip")).unwrap(),
        Origin::Archive { archive }
    );
}

/// Patches the central directory record of `name` in `zip` with `patch(record)`.
fn patch_central(zip: &mut [u8], name: &[u8], patch: impl FnOnce(&mut [u8])) {
    let at = (0..zip.len() - 46)
        .find(|&i| zip[i..i + 4] == [0x50, 0x4b, 0x01, 0x02] && zip[i + 46..].starts_with(name))
        .unwrap();
    patch(&mut zip[at..at + 46 + name.len()]);
}

#[test]
fn duplicate_zip_member_names_are_refused() {
    let mut bytes = fixture("basic.zip");
    // `b.ron` and `a.txt` have names of one length: rename one onto the other.
    patch_central(&mut bytes, b"b.ron", |record| {
        record[46..51].copy_from_slice(b"a.txt");
    });
    let root = root_with(&[("pkg.zip", &bytes)]);
    let mounts = Mounts::new(root.path());
    assert!(mounts.read_file(Path::new("pkg/a.txt")).unwrap().is_err());
    let problems = mounts.problems();
    assert!(problems[0].detail.contains("duplicate"), "{problems:?}");
}

/// Members whose data runs into the next local header overlap: the overlapping-file zip bomb.
#[test]
fn overlapping_zip_members_are_refused() {
    let mut bytes = fixture("basic.zip");
    // `a.txt` is stored, 6 bytes. Claim 200, which runs over the members after it.
    patch_central(&mut bytes, b"a.txt", |record| {
        record[20..24].copy_from_slice(&200u32.to_le_bytes());
        record[24..28].copy_from_slice(&200u32.to_le_bytes());
    });
    let root = root_with(&[("pkg.zip", &bytes)]);
    let mounts = Mounts::new(root.path());
    assert!(mounts.read_file(Path::new("pkg/b.ron")).unwrap().is_err());
    let problems = mounts.problems();
    assert!(problems[0].detail.contains("overlaps"), "{problems:?}");
}

#[test]
fn directory_records_count_against_the_member_limit() {
    let zip = zip_of(&[("a/", b""), ("b/", b""), ("c/", b""), ("x.txt", b"x")]);
    let root = root_with(&[("pkg.zip", &zip)]);
    let mounts = Mounts::new(root.path()).with_limits(Limits {
        max_members: 3,
        ..Limits::default()
    });
    assert!(mounts.read_file(Path::new("pkg/x.txt")).unwrap().is_err());
}

#[test]
fn tar_long_names_are_read_and_global_or_huge_headers_refused() {
    let long = format!("{}/file.txt", "d".repeat(150));
    let gnu = tar_of(|builder| {
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        builder
            .append_data(&mut header, &long, &b"long"[..])
            .unwrap();
    });
    let root = root_with(&[("pkg.tar", &gnu)]);
    let mounts = Mounts::new(root.path());
    let path = Path::new("pkg").join(&long);
    assert_eq!(mounts.read_file(&path).unwrap().unwrap(), b"long");

    let extension = |kind: tar::EntryType, body: Vec<u8>| {
        tar_of(|builder| {
            let mut header = tar::Header::new_ustar();
            header.set_entry_type(kind);
            header.set_size(body.len() as u64);
            header.set_cksum();
            builder.append(&header, body.as_slice()).unwrap();
            tar_file(builder, "a.txt", b"a");
        })
    };
    for (name, bytes) in [
        (
            "global",
            extension(tar::EntryType::XGlobalHeader, b"6 a=b\n".to_vec()),
        ),
        (
            "huge",
            extension(tar::EntryType::XHeader, vec![b'0'; 128 * 1024]),
        ),
        (
            "sparse",
            extension(tar::EntryType::XHeader, b"22 GNU.sparse.major=1\n".to_vec()),
        ),
    ] {
        let root = root_with(&[("pkg.tar", &bytes)]);
        let mounts = Mounts::new(root.path());
        assert!(
            mounts.read_file(Path::new("pkg/a.txt")).unwrap().is_err(),
            "{name}"
        );
        assert_eq!(
            mounts.problems()[0].kind,
            MountProblemKind::CorruptArchive,
            "{name}"
        );
    }
}

/// More archives than stay open: evicted ones reopen, from many threads at once.
#[test]
fn reads_survive_eviction_of_open_archives() {
    let zip = fixture("basic.zip");
    let files: Vec<(String, &[u8])> = (0..80)
        .map(|i| (format!("pkg{i:02}.zip"), zip.as_slice()))
        .collect();
    let files: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), *b)).collect();
    let root = root_with(&files);
    let mounts = Arc::new(Mounts::new(root.path()));
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let mounts = mounts.clone();
            std::thread::spawn(move || {
                for round in 0..3 {
                    for i in 0..80 {
                        let i = (i + t * 10 + round) % 80;
                        let path = format!("pkg{i:02}/sub/deep/c.txt");
                        let bytes = mounts.read_file(Path::new(&path)).unwrap().unwrap();
                        assert!(bytes.starts_with(b"third"));
                    }
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert!(mounts.problems().is_empty());
}

/// An archive replaced after it was indexed fails its reads instead of serving the new bytes
/// through the old index, once its file handle has been evicted.
#[test]
fn a_replaced_archive_is_detected_on_reopen() {
    let zip = fixture("basic.zip");
    let files: Vec<(String, &[u8])> = (0..70)
        .map(|i| (format!("pkg{i:02}.zip"), zip.as_slice()))
        .collect();
    let files: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), *b)).collect();
    let root = root_with(&files);
    let mounts = Mounts::new(root.path());
    for i in 0..70 {
        let path = format!("pkg{i:02}/a.txt");
        assert!(mounts.read_file(Path::new(&path)).unwrap().is_ok());
    }
    // `pkg00` was evicted. Replace it with a different archive.
    let replacement = root.path().join("replacement.zip");
    std::fs::write(&replacement, zip_of(&[("a.txt", b"something else")])).unwrap();
    std::fs::rename(&replacement, root.path().join("pkg00.zip")).unwrap();
    let err = mounts
        .read_file(Path::new("pkg00/a.txt"))
        .unwrap()
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidData, "{err}");
}

/// Reads `pkg/a.txt` from `bytes` mounted as `pkg.zip`, or the problem that made it unavailable.
fn mount_zip(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let root = root_with(&[("pkg.zip", bytes)]);
    let mounts = Mounts::new(root.path());
    mounts
        .read_file(Path::new("pkg/a.txt"))
        .unwrap()
        .map_err(|err| format!("{err} {:?}", mounts.problems()))
}

/// The end record of `zip` and its offset.
fn eocd_offset(zip: &[u8]) -> usize {
    (0..zip.len() - 3)
        .rev()
        .find(|&i| zip[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .unwrap()
}

#[test]
fn a_zip_comment_holding_the_end_signature_is_not_mistaken_for_the_end() {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("a.txt", options).unwrap();
    writer.write_all(b"first").unwrap();
    let mut comment = b"PK\x05\x06".to_vec();
    comment.extend([0xff; 18]);
    writer.set_raw_comment(comment.into()).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    assert_eq!(mount_zip(&bytes).unwrap(), b"first");
}

#[test]
fn a_zip_whose_disk_and_total_counts_differ_is_refused() {
    let mut bytes = fixture("basic.zip");
    let at = eocd_offset(&bytes);
    bytes[at + 8..at + 10].copy_from_slice(&3u16.to_le_bytes());
    assert!(mount_zip(&bytes).unwrap_err().contains("disk"));
}

/// `zip64.zip` saturates only the central directory offset, so its counts come from the Zip64
/// record. Changing them there, and nowhere else, must be noticed.
#[test]
fn zip64_counts_and_record_size_are_checked() {
    let original = fixture("zip64.zip");
    assert!(mount_zip(&original).is_ok());
    let record = (0..original.len() - 3)
        .rev()
        .find(|&i| original[i..i + 4] == [0x50, 0x4b, 0x06, 0x06])
        .unwrap();
    let mut counts = original.clone();
    counts[record + 24..record + 32].copy_from_slice(&9u64.to_le_bytes());
    counts[record + 32..record + 40].copy_from_slice(&9u64.to_le_bytes());
    assert!(mount_zip(&counts).is_err());
    let mut record_size = original.clone();
    record_size[record + 4..record + 12].copy_from_slice(&(1u64 << 40).to_le_bytes());
    assert!(
        mount_zip(&record_size)
            .unwrap_err()
            .contains("Zip64 record")
    );
}

/// Reads `pkg/a.txt` from `bytes` mounted as `pkg.tar`, or the problem that made it unavailable.
fn mount_tar(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let root = root_with(&[("pkg.tar", bytes)]);
    let mounts = Mounts::new(root.path());
    mounts
        .read_file(Path::new("pkg/a.txt"))
        .unwrap()
        .map_err(|err| format!("{err} {:?}", mounts.problems()))
}

#[test]
fn a_tar_without_its_end_block_is_refused() {
    let full = tar_of(|builder| tar_file(builder, "a.txt", b"a"));
    assert_eq!(mount_tar(&full).unwrap(), b"a");
    // Header and one data block, then nothing.
    assert!(mount_tar(&full[..1024]).unwrap_err().contains("truncated"));
    assert!(mount_tar(&full[..1100]).unwrap_err().contains("truncated"));
    assert!(
        mount_tar(b"not a tar at all")
            .unwrap_err()
            .contains("truncated")
    );
    assert!(mount_tar(b"").unwrap_err().contains("truncated"));
}

#[test]
fn a_tar_ending_on_a_long_name_is_refused() {
    let mut bytes = tar_of(|builder| {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::GNULongName);
        header.set_size(6);
        header.set_cksum();
        builder.append(&header, &b"a.txt\0"[..]).unwrap();
    });
    bytes.extend([0; 1024]);
    assert!(mount_tar(&bytes).unwrap_err().contains("no entry after it"));
}

/// Historic tars summed the header as signed bytes; that sum differs once a name has a byte
/// above 0x7f.
#[test]
fn a_tar_with_a_signed_header_checksum_is_read() {
    let mut bytes = tar_of(|builder| tar_file(builder, "é.txt", b"e"));
    let signed: i64 = bytes[..512]
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                i64::from(b as i8)
            }
        })
        .sum();
    bytes[148..156].copy_from_slice(format!("{signed:06o}\0 ").as_bytes());
    let root = root_with(&[("pkg.tar", &bytes)]);
    let mounts = Mounts::new(root.path());
    assert_eq!(
        mounts.read_file(Path::new("pkg/é.txt")).unwrap().unwrap(),
        b"e"
    );
}

/// Pre-POSIX tar wrote directories as regular entries with a trailing slash.
#[test]
fn a_v7_tar_directory_is_a_directory() {
    let bytes = tar_of(|builder| {
        let mut header = tar::Header::new_old();
        header.set_size(0);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append_data(&mut header, "dir/", &b""[..]).unwrap();
        tar_file(builder, "dir/a.txt", b"a");
    });
    let root = root_with(&[("pkg.tar", &bytes)]);
    let mounts = Mounts::new(root.path());
    assert!(mounts.is_dir(Path::new("pkg/dir")).unwrap().unwrap());
    assert_eq!(
        mounts
            .read_file(Path::new("pkg/dir/a.txt"))
            .unwrap()
            .unwrap(),
        b"a"
    );
}

/// What a recursive remove or a rename of a directory would take with it.
#[test]
fn holds_archive_sees_archives_anywhere_below() {
    let root = root_with(&[
        ("mods/deep/pkg.zip", &fixture("basic.zip")),
        ("loose/a.txt", b"a"),
    ]);
    let mounts = Mounts::new(root.path());
    for path in [
        "",
        "mods",
        "mods/deep",
        "mods/deep/pkg",
        "mods/deep/pkg.zip",
    ] {
        assert!(mounts.holds_archive(Path::new(path)).unwrap(), "{path}");
    }
    for path in ["loose", "loose/a.txt", "missing"] {
        assert!(!mounts.holds_archive(Path::new(path)).unwrap(), "{path}");
    }
}
