//! `ename_fix` -- writes missing `.alias` files and assigns missing guids. Never writes a `.meta`:
//! that file belongs to Bevy, and this crate only ever reads one.

use ename_asset_alias::{AliasFile, AliasOrigin, DiscoveredAsset};
use std::path::{Path, PathBuf};

/// What one run of `ename_fix` did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FixSummary {
    /// A new `.alias` written for a file a folder rule covered but nobody had named a sidecar for.
    pub created: usize,
    /// An existing `.alias` that had no `guid`, rewritten with a freshly generated one.
    pub guids_assigned: usize,
    /// A sidecar `ename_fix` could not read or could not write. Counted separately from
    /// `created`/`guids_assigned`, neither of which is incremented for the same asset.
    pub failed: usize,
}

/// Scans `asset_root`, then walks every discovered asset and repairs whichever guid is missing.
///
/// Operates on `Scan::packages[].assets`, i.e. after package resolution -- an asset belonging to a
/// disabled package is left alone, the same as `ename_check` and `ename_content_list` see it.
pub fn fix(
    asset_root: &Path,
    search_paths: &[String],
    load_order_path: Option<&Path>,
) -> FixSummary {
    let scan = crate::scan::scan(asset_root, search_paths, load_order_path);
    let mut summary = FixSummary::default();
    for package in &scan.packages {
        for asset in &package.assets {
            if asset.guid.is_none() {
                fix_one(asset_root, asset, &mut summary);
            }
        }
    }
    summary
}

/// The path of the `.alias` sidecar a discovered asset would have, whether or not it exists yet:
/// the full file name plus `.alias`, `airship.glb` -> `airship.glb.alias`. The inverse of
/// `ename_asset_alias::alias_sidecar_target`, and deliberately not `Path::with_extension`, which
/// would replace only the last extension rather than appending to the whole file name.
fn sidecar_path(asset_path: &Path) -> PathBuf {
    let mut sidecar = asset_path.as_os_str().to_owned();
    sidecar.push(".alias");
    PathBuf::from(sidecar)
}

fn fix_one(asset_root: &Path, asset: &DiscoveredAsset, summary: &mut FixSummary) {
    let full = asset_root.join(sidecar_path(&asset.path));
    let guid = uuid::Uuid::now_v7();

    match std::fs::read_to_string(&full) {
        Ok(text) => {
            // The sidecar exists but `asset.guid` is `None`, so its own `guid` field is what is
            // missing. Every other field -- an authored `alias`, a hand-set `include` -- is not
            // this command's to change.
            let Ok(mut file) = toml::from_str::<AliasFile>(&text) else {
                // Already reported by the scan as `UnparseableAliasFile`; nothing to fix here.
                return;
            };
            file.guid = Some(guid);
            match write_alias_file(&full, &file) {
                Ok(()) => summary.guids_assigned += 1,
                Err(err) => {
                    eprintln!("ename_fix: could not write {}: {err}", full.display());
                    summary.failed += 1;
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // No sidecar at all: the asset is covered purely by a folder rule. Writing the alias
            // down is what stops it moving when the rule or the file's position changes later --
            // the same reason `AliasOrigin::Derived` exists.
            let file = AliasFile {
                guid: Some(guid),
                alias: Some(asset.alias.clone()),
                alias_origin: AliasOrigin::Derived,
                ..AliasFile::default()
            };
            match write_alias_file(&full, &file) {
                Ok(()) => summary.created += 1,
                Err(err) => {
                    eprintln!("ename_fix: could not write {}: {err}", full.display());
                    summary.failed += 1;
                }
            }
        }
        Err(err) => {
            eprintln!("ename_fix: could not read {}: {err}", full.display());
            summary.failed += 1;
        }
    }
}

fn write_alias_file(path: &Path, file: &AliasFile) -> std::io::Result<()> {
    let text = toml::to_string_pretty(file).expect("an AliasFile always serializes");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, text)
}
