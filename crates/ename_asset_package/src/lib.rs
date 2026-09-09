//! `ename_asset_package` -- finding content packages on disk and reading what they contain.
//!
//! It finds packages, parses their manifests, and puts them in load order. Discovering the assets
//! inside one is not its job: `.alias` sidecars, `_alias_rules.toml` folder rules and the walk over
//! them belong to `ename_asset_alias`, which is the crate that knows what an alias is. This crate
//! calls that walk once per package root and adds the only thing it has that the walk does not --
//! an order. See `docs/design/crate-layout.md`.
//!
//! Its one first-party dependency is `ename_asset_alias`, taken with `default-features = false`,
//! so a command line tool can link both without a renderer.

mod manifest;
mod scan;

pub use crate::manifest::{
    Manifest, PackageIdError, PackageInfo, Requirement, RequirementError, Version, VersionReq,
    validate_package_id,
};
pub use crate::scan::{MANIFEST_FILE, Package, Scan, scan_packages};
