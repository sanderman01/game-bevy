//! `ename_asset_package` -- finding content packages on disk and reading their manifests.
//!
//! A leaf of the layer graph with no first-party dependencies. It deliberately knows nothing about
//! aliases: a manifest's `[assets]` table is alias-shaped, but discovering packages, parsing them
//! and ordering them is not, and keeping the mapping out of here is what lets a command line tool
//! link this crate without the Bevy asset source. `ename_asset_content` owns the mapping.
//! See `docs/design/crate-layout.md`.

mod manifest;
mod scan;

pub use crate::manifest::{AssetsInfo, Manifest, PackageInfo, Version, VersionError};
pub use crate::scan::{MANIFEST_FILE, Package, scan_packages};
