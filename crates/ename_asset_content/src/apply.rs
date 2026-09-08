//! Mapping manifests onto the alias index.
//!
//! This is the whole reason the crate exists: `[assets.add]` is alias-shaped, but neither
//! `ename_asset_package` nor `ename_asset_alias` should have to know that.

use bevy::log::{info, warn};
use ename_asset_alias::ContentIndex;
use ename_asset_package::{Manifest, Package};
use std::path::Path;

/// The delimiter between a package id and an alias within it.
const NAMESPACE_DELIM: &str = "::";

/// Folds an ordered package list into one index.
///
/// Order is load order: a later package's claim on an alias replaces an earlier one's, which is
/// how a mod overrides the base game.
pub fn build_index(packages: &[Package]) -> ContentIndex {
    let mut index = ContentIndex::default();
    for package in packages {
        info!(
            "  {:20} {}",
            package.manifest.package.id,
            package.root.display()
        );
        apply_manifest(&mut index, &package.manifest, &package.root);
    }
    info!(
        "Registered {} aliases from {} packages",
        index.len(),
        packages.len()
    );
    index
}

/// Applies one package's `[assets]` section to `index`.
///
/// `add` aliases are namespaced with the package id and their paths rooted at `package_root`.
/// `replace` and `remove` take an already-namespaced alias, because they name another package's
/// asset. A rejected alias is logged and skipped: one malformed entry must not cost the rest of
/// the package, and one malformed package must not cost the game.
pub fn apply_manifest(index: &mut ContentIndex, manifest: &Manifest, package_root: &Path) {
    let package_id = &manifest.package.id;

    if let Some(add) = &manifest.assets.add {
        for (alias, relative_path) in add {
            let namespaced = format!("{package_id}{NAMESPACE_DELIM}{alias}");
            let path = package_root.join(relative_path);
            match index.insert(&namespaced, &path) {
                Ok(Some(previous)) => info!(
                    "  ++ {namespaced:24} {} (was {})",
                    path.display(),
                    previous.display()
                ),
                Ok(None) => info!("  ++ {namespaced:24} {}", path.display()),
                Err(err) => warn!("  !! {package_id} declares an invalid alias: {err}"),
            }
        }
    }

    if let Some(replace) = &manifest.assets.replace {
        for (alias, relative_path) in replace {
            let path = package_root.join(relative_path);
            match index.insert(alias, &path) {
                Ok(Some(previous)) => info!(
                    "  := {alias:24} {} (was {})",
                    path.display(),
                    previous.display()
                ),
                // Phase 3 turns this into a reported contested claim. For now it is a warning,
                // because it is exactly the `core::airschip` typo in `assets/mods/example`.
                Ok(None) => warn!(
                    "  := {alias:24} {} (replaces nothing; is the alias spelled right?)",
                    path.display()
                ),
                Err(err) => warn!("  !! {package_id} declares an invalid alias: {err}"),
            }
        }
    }

    if let Some(remove) = &manifest.assets.remove {
        for alias in remove.keys() {
            match index.remove(alias) {
                Some(_) => info!("  -- {alias:24}"),
                None => warn!("  -- {alias:24} (removes nothing; is the alias spelled right?)"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_manifest, build_index};
    use ename_asset_alias::ContentIndex;
    use ename_asset_package::{Manifest, Package};
    use std::path::Path;

    fn manifest(toml_src: &str) -> Manifest {
        toml::from_str(toml_src).expect("test manifest parses")
    }

    #[test]
    fn add_namespaces_the_alias_and_roots_the_path() {
        let m = manifest(
            r#"
            [package]
            id = "core"
            version = "1.0.0"
            authors = []
            title = "Core"
            description = ""

            [assets.add]
            airship = "airship.glb"
            "#,
        );
        let mut index = ContentIndex::default();
        apply_manifest(&mut index, &m, Path::new("basegame/core"));
        assert_eq!(
            index.resolve("core::airship"),
            Some(Path::new("basegame/core/airship.glb"))
        );
    }

    /// `replace` takes an already-namespaced alias, because it is naming somebody else's asset.
    #[test]
    fn replace_uses_the_alias_verbatim() {
        let m = manifest(
            r#"
            [package]
            id = "bigships"
            version = "1.0.0"
            authors = []
            title = "Big Ships"
            description = ""

            [assets.replace]
            "core::airship" = "big_airship.glb"
            "#,
        );
        let mut index = ContentIndex::default();
        index
            .insert("core::airship", "basegame/core/airship.glb")
            .unwrap();
        apply_manifest(&mut index, &m, Path::new("mods/bigships"));
        assert_eq!(
            index.resolve("core::airship"),
            Some(Path::new("mods/bigships/big_airship.glb"))
        );
    }

    #[test]
    fn remove_drops_the_alias() {
        let m = manifest(
            r#"
            [package]
            id = "trimmed"
            version = "1.0.0"
            authors = []
            title = "Trimmed"
            description = ""

            [assets.remove]
            "core::banana" = ""
            "#,
        );
        let mut index = ContentIndex::default();
        index
            .insert("core::banana", "basegame/core/banana.glb")
            .unwrap();
        apply_manifest(&mut index, &m, Path::new("mods/trimmed"));
        assert_eq!(index.resolve("core::banana"), None);
    }

    /// One malformed entry must not cost the rest of the package.
    #[test]
    fn a_bad_alias_is_skipped_and_the_rest_still_registers() {
        let m = manifest(
            r#"
            [package]
            id = "core"
            version = "1.0.0"
            authors = []
            title = "Core"
            description = ""

            [assets.add]
            "air#ship" = "airship.glb"
            map = "map.glb"
            "#,
        );
        let mut index = ContentIndex::default();
        apply_manifest(&mut index, &m, Path::new("basegame/core"));
        assert_eq!(index.len(), 1);
        assert_eq!(
            index.resolve("core::map"),
            Some(Path::new("basegame/core/map.glb"))
        );
    }

    /// `build_index` folds in load order, so the last package to claim an alias keeps it.
    #[test]
    fn build_index_applies_packages_in_order() {
        let core = Package {
            manifest: manifest(
                r#"
                [package]
                id = "core"
                version = "1.0.0"
                authors = []
                title = "Core"
                description = ""

                [assets.add]
                greeting = "greeting.txt"
                "#,
            ),
            root: "base/core".into(),
        };
        let loud = Package {
            manifest: manifest(
                r#"
                [package]
                id = "loud"
                version = "1.0.0"
                authors = []
                title = "Loud"
                description = ""

                [assets.replace]
                "core::greeting" = "greeting.txt"
                "#,
            ),
            root: "mods/loud".into(),
        };

        let index = build_index(&[core, loud]);
        assert_eq!(
            index.resolve("core::greeting"),
            Some(Path::new("mods/loud/greeting.txt"))
        );
    }
}
