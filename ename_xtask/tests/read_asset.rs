//! `read_asset` over a temporary tree: a filesystem path, a path under the asset root, an alias,
//! an asset inside an archive, and sibling and `.meta` reads.

use ename_xtask::read_asset::{ReadError, ScanPolicy, read_asset};
use std::io::Write;
use tempfile::TempDir;

const MANIFEST: &str = r#"[package]
id = "core"
version = "1.0.0"
authors = ["Test"]
title = "Core"
description = "read_asset fixture."
"#;

fn root_with(files: &[(&str, &[u8])]) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    for (path, bytes) in files {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    root
}

fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in files {
        writer.start_file(*name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn read(root: &TempDir, arg: &str) -> Result<ename_xtask::read_asset::AssetFile, ReadError> {
    let search_paths = ["basegame".to_owned()];
    read_asset(arg, &ScanPolicy::new(root.path(), &search_paths, None))
}

#[test]
fn a_label_or_a_drive_relative_sibling_is_refused() {
    let root = tree();
    assert!(matches!(
        read(&root, "core::ship#Mesh0"),
        Err(ReadError::Label(_))
    ));
    let file = read(&root, "basegame/core/ships/ship.gltf").unwrap();
    assert!(matches!(
        file.read_sibling("/etc/passwd"),
        Err(ReadError::OutsideRoot(_))
    ));
}

fn tree() -> TempDir {
    root_with(&[
        ("basegame/core/manifest.toml", MANIFEST.as_bytes()),
        ("basegame/core/ships/ship.gltf", b"gltf"),
        (
            "basegame/core/ships/ship.gltf.alias",
            b"alias = \"core::ship\"\n",
        ),
        ("basegame/core/ships/ship_data.bin", b"bin"),
        ("basegame/core/ships/ship.gltf.meta", b"meta"),
    ])
}

#[test]
fn a_filesystem_path_reads_the_file_and_names_it_as_given() {
    let root = tree();
    let path = root.path().join("basegame/core/ships/ship.gltf");
    let file = read(&root, path.to_str().unwrap()).unwrap();
    assert_eq!(file.bytes, b"gltf");
    assert_eq!(file.name(), path.display().to_string());
    assert_eq!(file.extension().as_deref(), Some("gltf"));
    assert_eq!(file.read_sibling("ship_data.bin").unwrap(), b"bin");
    assert_eq!(file.meta().unwrap().as_deref(), Some(&b"meta"[..]));
}

#[test]
fn a_path_under_the_asset_root_reads_through_the_vfs() {
    let root = tree();
    let file = read(&root, "basegame/core/ships/ship.gltf").unwrap();
    assert_eq!(file.bytes, b"gltf");
    assert_eq!(file.name(), "basegame/core/ships/ship.gltf");
    assert_eq!(file.read_sibling("./ship_data.bin").unwrap(), b"bin");
    assert!(matches!(
        file.read_sibling("../../../../x"),
        Err(ReadError::OutsideRoot(_))
    ));
}

#[test]
fn an_alias_resolves_through_the_scan() {
    let root = tree();
    let file = read(&root, "core::ship").unwrap();
    assert_eq!(file.bytes, b"gltf");
    assert_eq!(file.name(), "basegame/core/ships/ship.gltf");
    assert_eq!(
        read(&root, "core::nothing").err(),
        Some(ReadError::UnknownAlias("core::nothing".into()))
    );
}

#[test]
fn an_asset_inside_an_archive_reads_with_its_siblings() {
    let archive = zip(&[
        ("manifest.toml", MANIFEST.as_bytes()),
        ("img/a.png", b"png"),
        ("img/a.png.meta", b"meta"),
    ]);
    let root = root_with(&[("basegame/core.zip", &archive)]);
    let file = read(&root, "basegame/core/img/a.png").unwrap();
    assert_eq!(file.bytes, b"png");
    assert_eq!(file.meta().unwrap().as_deref(), Some(&b"meta"[..]));
}

#[test]
fn a_missing_asset_is_not_found_and_a_missing_meta_is_none() {
    let root = tree();
    assert_eq!(
        read(&root, "basegame/none.png").err(),
        Some(ReadError::NotFound("basegame/none.png".into()))
    );
    let file = read(&root, "basegame/core/ships/ship_data.bin").unwrap();
    assert_eq!(file.meta().unwrap(), None);
}
