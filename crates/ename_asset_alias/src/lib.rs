//! `ename_asset_alias` -- addressing an asset by a namespaced alias.
//!
//! A leaf of the layer graph with no first-party dependencies. It owns the alias itself: what one
//! is, the `.alias` and `_rules.toml` files that name one, the walk that finds them, and the
//! `alias://` asset source that serves them. Adding [`AliasPlugins`] to an `App` is the whole
//! setup -- the tree under the asset root is scanned on startup and
//! `asset_server.load("alias://core::airship")` works.
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
#[cfg(feature = "bevy")]
mod scan_plugin;

pub use crate::alias_file::{ALIAS_EXTENSION, AliasFile, AliasOrigin, alias_sidecar_target};
pub use crate::discovery::{AliasScan, DiscoveredAsset, Problem, ProblemKind, scan_aliases};
pub use crate::index::{AliasError, ContentIndex, validate_alias};
pub use crate::rules::{CompiledRules, RULES_FILE, Rules, RulesError};
pub use crate::vfs::{BoxedFuture, DirEntry, StdVfs, Vfs, VfsError};

#[cfg(feature = "bevy")]
pub use crate::reader::{ALIAS_SOURCE, AliasSourcePlugin, ContentIndexCell};
#[cfg(feature = "bevy")]
pub use crate::scan_plugin::AliasScanPlugin;
#[cfg(feature = "bevy")]
pub use crate::vfs::AssetReaderVfs;

#[cfg(feature = "bevy")]
use bevy::app::{PluginGroup, PluginGroupBuilder};

/// The `alias://` source and the startup scan that fills it: everything a project needs to address
/// its assets by alias.
///
/// **Must be added before `AssetPlugin`.** [`AliasSourcePlugin`] registers an asset source, and
/// `AssetPlugin` turns registered sources into live ones exactly once, when it builds.
///
/// A project that decides load order for itself -- several content roots, packages that override
/// one another -- adds [`AliasSourcePlugin`] alone and fills [`ContentIndexCell`] its own way.
/// That is what `ename_asset_content` does.
#[cfg(feature = "bevy")]
pub struct AliasPlugins {
    asset_root: String,
}

#[cfg(feature = "bevy")]
impl Default for AliasPlugins {
    fn default() -> Self {
        Self {
            asset_root: "assets".to_owned(),
        }
    }
}

#[cfg(feature = "bevy")]
impl AliasPlugins {
    /// Sets the asset root both plugins read through. Must match `AssetPlugin::file_path`.
    pub fn with_asset_root(mut self, path: impl Into<String>) -> Self {
        self.asset_root = path.into();
        self
    }
}

#[cfg(feature = "bevy")]
impl PluginGroup for AliasPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(AliasSourcePlugin::default().with_asset_root(self.asset_root.clone()))
            .add(AliasScanPlugin::default().with_asset_root(self.asset_root))
    }
}
