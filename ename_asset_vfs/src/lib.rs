//! `ename_asset_vfs` -- reading the asset tree as a filesystem.
//!
//! The leaf of the asset layers, with no first-party dependencies. It owns the [`Vfs`] trait the
//! alias walk and the package scan read through, [`StdVfs`] for tooling, and, behind the `bevy`
//! feature, [`AssetReaderVfs`] for the running game. See `docs/design/crate-layout.md`.
//!
//! With the `zip` or `tar` feature it also mounts archives: `assets/basegame/core.zip` reads as
//! the directory `basegame/core`. [`Mounts`] is the Bevy-free core and [`ArchiveVfs`] its tooling
//! front. See `docs/assets-archives.md`.

mod vfs;

#[cfg(any(feature = "zip", feature = "tar"))]
mod archive_vfs;
#[cfg(any(feature = "zip", feature = "tar"))]
mod mount;

pub use crate::vfs::{BoxedFuture, DirEntry, StdVfs, Vfs, VfsError, is_hidden_from_listings};

#[cfg(feature = "bevy")]
pub use crate::vfs::AssetReaderVfs;

#[cfg(any(feature = "zip", feature = "tar"))]
pub use crate::{
    archive_vfs::ArchiveVfs,
    mount::{Limits, MountProblem, MountProblemKind, Mounts, Origin},
};
