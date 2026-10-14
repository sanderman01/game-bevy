//! `ename_asset_vfs` -- reading the asset tree as a filesystem.
//!
//! The leaf of the asset layers, with no first-party dependencies. It owns the [`Vfs`] trait the
//! alias walk and the package scan read through, [`StdVfs`] for tooling, and, behind the `bevy`
//! feature, [`AssetReaderVfs`] for the running game. See `docs/design/crate-layout.md`.

mod vfs;

pub use crate::vfs::{BoxedFuture, DirEntry, StdVfs, Vfs, VfsError, is_hidden_from_listings};

#[cfg(feature = "bevy")]
pub use crate::vfs::AssetReaderVfs;
