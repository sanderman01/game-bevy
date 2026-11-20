//! Bakes the package discovery for the browser, which cannot list directories.
//!
//! The bake stops before load order: the game resolves it at startup, so the player's load order
//! still applies. Archives are refused, because the browser reads loose files only.

use ename_asset_alias::IgnoredFiles;
use ename_asset_package::{Discovery, PACKAGE_INFO_FILE, discover_packages};
use ename_asset_vfs::ArchiveVfs;
use std::path::{Path, PathBuf};

/// Discovers the packages under `search_paths` in `asset_root`, as the game would at startup.
pub fn discover(asset_root: &Path, search_paths: &[String]) -> Result<Discovery, String> {
    let vfs = ArchiveVfs::new(asset_root);
    let discovery = futures_lite::future::block_on(discover_packages(
        &vfs,
        search_paths,
        &IgnoredFiles::default(),
    ));
    let archived: Vec<PathBuf> = discovery
        .packages
        .iter()
        .filter(|package| {
            vfs.mounts()
                .origin_of_file(&package.root.join(PACKAGE_INFO_FILE))
                .is_ok_and(|origin| !origin.is_loose())
        })
        .map(|package| package.root.clone())
        .collect();
    if archived.is_empty() {
        Ok(discovery)
    } else {
        Err(format!(
            "packages inside archives cannot be served to the browser; unpack them: {archived:?}"
        ))
    }
}

/// Writes [`discover`]'s result to `out`. Returns the package count.
pub fn bake(asset_root: &Path, search_paths: &[String], out: &Path) -> Result<usize, String> {
    let discovery = discover(asset_root, search_paths)?;
    for problem in &discovery.problems {
        eprintln!(
            "  !! {} at {}: {}",
            problem.kind,
            problem.path.display(),
            problem.detail
        );
    }
    let bytes = discovery.encode().map_err(|err| err.to_string())?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    }
    std::fs::write(out, bytes).map_err(|err| format!("{}: {err}", out.display()))?;
    Ok(discovery.packages.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ename_asset_package::{LoadOrder, build_index, resolve_discovery, scan_packages};
    use ename_asset_vfs::StdVfs;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const MANIFEST: &str = r#"
[package]
id = "core"
version = "1.0.0"
authors = []
title = "Core"
description = ""
"#;

    #[test]
    fn a_baked_discovery_resolves_like_a_live_scan() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "game/core/package-info.toml", MANIFEST);
        write(root.path(), "game/core/props/crate.glb", "");
        write(root.path(), "game/core/stage.scn.ron", "");
        let search_paths = vec!["game".to_owned()];

        let out = root.path().join("out/discovery.ron");
        assert_eq!(bake(root.path(), &search_paths, &out).unwrap(), 1);
        let baked = Discovery::decode(&std::fs::read(&out).unwrap()).unwrap();

        let order = LoadOrder::default();
        let live = futures_lite::future::block_on(scan_packages(
            &StdVfs::new(root.path()),
            &search_paths,
            &order,
            &IgnoredFiles::default(),
        ));
        let (baked_index, _) = build_index(&resolve_discovery(baked, &order));
        let (live_index, _) = build_index(&live);
        let sorted = |index: &ename_asset_alias::ContentIndex| {
            let mut pairs: Vec<(String, PathBuf)> = index
                .iter()
                .map(|(alias, path)| (alias.to_owned(), path.to_path_buf()))
                .collect();
            pairs.sort();
            pairs
        };
        assert_eq!(sorted(&baked_index), sorted(&live_index));
        assert!(baked_index.resolve("core::crate").is_some());
    }
}
