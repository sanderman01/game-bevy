//! `ename_asset_package` -- finding content packages on disk and reading what they contain.
//!
//! A leaf of the layer graph with no first-party dependencies. It finds packages, parses their
//! manifests, and discovers each package's assets from `.alias` sidecars and `_rules.toml` folder
//! rules. It carries the alias *strings* it finds and never validates one -- validating needs the
//! alias type, and that type lives in the sibling leaf `ename_asset_alias`. `ename_asset_content`
//! is where the two meet. See `docs/design/crate-layout.md`.

mod alias_file;
mod manifest;
mod rules;
mod scan;
mod vfs;

pub use crate::alias_file::{ALIAS_EXTENSION, AliasFile, AliasOrigin, alias_sidecar_target};
pub use crate::manifest::{Manifest, PackageInfo, Version, VersionError};
pub use crate::rules::{CompiledRules, RULES_FILE, Rules, RulesError};
pub use crate::scan::{
    DiscoveredAsset, MANIFEST_FILE, Package, Problem, ProblemKind, Scan, scan_packages,
};
#[cfg(feature = "bevy")]
pub use crate::vfs::AssetReaderVfs;
pub use crate::vfs::{BoxedFuture, DirEntry, StdVfs, Vfs, VfsError};
