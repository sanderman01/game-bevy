//! `ename_asset_vfs` -- reading the asset tree as a filesystem.
//!
//! The leaf of the asset layers, with no first-party dependencies. It owns the [`Vfs`] trait the
//! alias walk and the package scan read through, [`StdVfs`] for tooling, and, behind the `bevy`
//! feature, [`AssetReaderVfs`] for the running game. See `docs/design/crate-layout.md`.
//!
//! It also mounts archives: with the `zip` or `tar` feature, `assets/basegame/core.zip` reads as
//! the directory `basegame/core`. [`Mounts`] is the Bevy-free core and [`ArchiveVfs`] its tooling
//! front; with `bevy`, [`ArchiveSourcePlugin`] puts them under Bevy's `Default` asset source. The
//! mount code is compiled even with no format, so the write guard and problem reporting exist
//! whichever crate turned a format on; without one, nothing ever mounts. See
//! `docs/assets-archives.md`.

mod archive_vfs;
mod mount;
mod vfs;

#[cfg(feature = "bevy")]
mod archive_source;

pub use crate::{
    archive_vfs::ArchiveVfs,
    mount::{Limits, MountProblem, MountProblemKind, Mounts, Origin},
    vfs::{BoxedFuture, DirEntry, StdVfs, Vfs, VfsError, is_hidden_from_listings},
};

#[cfg(feature = "bevy")]
pub use crate::{
    archive_source::{ArchiveMounts, ArchiveSourcePlugin, check_writable},
    vfs::AssetReaderVfs,
};
