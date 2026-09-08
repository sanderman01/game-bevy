//! `ename_asset_alias` -- addressing an asset by a namespaced alias.
//!
//! A leaf of the layer graph with no first-party dependencies. It knows what an alias is, what it
//! resolves to, and how to serve it as a Bevy asset source. It does not know where an index comes
//! from: packages, manifests and scanning live in `ename_asset_package`, and the mapping between
//! them in `ename_asset_content`. See `docs/design/crate-layout.md`.

mod index;
mod reader;

pub use crate::index::{AliasError, ContentIndex, validate_alias};
pub use crate::reader::{ALIAS_SOURCE, AliasSourcePlugin, ContentIndexCell};
