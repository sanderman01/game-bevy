//! The package scan over archives finds exactly what it finds over the same packages loose.
//!
//! Each fixture package is packed into a `.zip` and then a `.tar` at test time, in a temporary
//! copy of the tree, and scanned through `ArchiveVfs`. Packages, problems and the folded index
//! must match the loose scan of `tests/fixtures`.

use ename_asset_alias::IgnoredFiles;
use ename_asset_package::{LoadOrder, Scan, build_index, scan_packages};
use ename_asset_vfs::{ArchiveVfs, StdVfs, Vfs};
use futures_lite::future::block_on;
use std::{
    io::Write,
    path::{Path, PathBuf},
};

const FIXTURE_ROOT: &str = "tests/fixtures";

const SEARCH_PATH_SETS: &[&[&str]] = &[
    &["base", "mods"],
    &["ordered/base", "ordered/mods"],
    &["order"],
    &["defaulting"],
];

#[derive(Clone, Copy, Debug)]
enum Format {
    Zip,
    Tar,
}

fn scan(vfs: &dyn Vfs, search_paths: &[&str]) -> Scan {
    let paths: Vec<String> = search_paths.iter().map(|p| (*p).to_owned()).collect();
    block_on(scan_packages(
        vfs,
        &paths,
        &LoadOrder::default(),
        &IgnoredFiles::default(),
    ))
}

/// Everything a scan decides, in a comparable form.
fn outcome(scan: &Scan) -> String {
    let (index, report) = build_index(scan);
    let mut index: Vec<(&str, &Path)> = index.iter().collect();
    index.sort();
    format!(
        "{:#?}\n{:#?}\n{:#?}\n{index:#?}\n{report:#?}",
        scan.packages, scan.disabled, scan.problems
    )
}

/// Every file under `dir`, relative to it, sorted.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path.strip_prefix(dir).unwrap().to_path_buf());
            }
        }
    }
    out.sort();
    out
}

fn pack(dir: &Path, archive: &Path, format: Format) {
    let out = std::fs::File::create(archive).unwrap();
    match format {
        Format::Zip => {
            let mut writer = zip::ZipWriter::new(out);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for file in files(dir) {
                writer
                    .start_file(file.to_str().unwrap().replace('\\', "/"), options)
                    .unwrap();
                writer
                    .write_all(&std::fs::read(dir.join(&file)).unwrap())
                    .unwrap();
            }
            writer.finish().unwrap();
        }
        Format::Tar => {
            let mut builder = tar::Builder::new(out);
            for file in files(dir) {
                builder
                    .append_path_with_name(dir.join(&file), &file)
                    .unwrap();
            }
            builder.finish().unwrap();
        }
    }
}

/// A copy of the fixture tree with every package directory under `search_paths` replaced by an
/// archive of it.
fn packed_copy(search_paths: &[&str], format: Format) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for file in files(Path::new(FIXTURE_ROOT)) {
        let target = root.path().join(&file);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(Path::new(FIXTURE_ROOT).join(&file), target).unwrap();
    }
    let extension = match format {
        Format::Zip => "zip",
        Format::Tar => "tar",
    };
    for search_path in search_paths {
        for entry in std::fs::read_dir(root.path().join(search_path)).unwrap() {
            let package = entry.unwrap().path();
            if package.is_dir() {
                pack(&package, &package.with_extension(extension), format);
                std::fs::remove_dir_all(&package).unwrap();
            }
        }
    }
    root
}

#[test]
fn archived_packages_scan_exactly_like_loose_ones() {
    for search_paths in SEARCH_PATH_SETS {
        let loose = outcome(&scan(&StdVfs::new(FIXTURE_ROOT), search_paths));
        for format in [Format::Zip, Format::Tar] {
            let root = packed_copy(search_paths, format);
            let vfs = ArchiveVfs::new(root.path());
            let archived = outcome(&scan(&vfs, search_paths));
            assert_eq!(archived, loose, "{search_paths:?} as {format:?}");
            assert!(
                vfs.mounts().problems().is_empty(),
                "{search_paths:?} as {format:?}: {:?}",
                vfs.mounts().problems()
            );
        }
    }
}
