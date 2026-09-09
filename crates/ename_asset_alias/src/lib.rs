//! `ename_asset_alias` -- addressing an asset by a namespaced alias.
//!
//! A leaf of the layer graph with no first-party dependencies. It owns the alias itself: what one
//! is, the `.alias` and `_rules.toml` files that name one, the walk that finds them, and the
//! `alias://` asset source that serves them.
//!
//! What it deliberately does not know is *packages*: manifests, versions, load order and
//! cross-package overrides live in `ename_asset_package`, which drives the same walk one package
//! root at a time and orders the results itself. See `docs/design/crate-layout.md`.
//!
//! Everything to do with Bevy is behind the `bevy` feature, on by default. With it off the crate
//! is the schema and the walk and nothing else, which is what lets a command line tool link it
//! without a renderer.

mod alias_file;
mod discovery;
mod index;
mod rules;
mod vfs;

#[cfg(feature = "bevy")]
mod reader;

pub use crate::alias_file::{ALIAS_EXTENSION, AliasFile, AliasOrigin, alias_sidecar_target};
pub use crate::discovery::{AliasScan, DiscoveredAsset, Problem, ProblemKind, scan_aliases};
pub use crate::index::{AliasError, ContentIndex, validate_alias};
pub use crate::rules::{CompiledRules, RULES_FILE, Rules, RulesError};
pub use crate::vfs::{BoxedFuture, DirEntry, StdVfs, Vfs, VfsError};

#[cfg(feature = "bevy")]
pub use crate::reader::{ALIAS_SOURCE, AliasSourcePlugin, ContentIndexCell};
#[cfg(feature = "bevy")]
pub use crate::vfs::AssetReaderVfs;
