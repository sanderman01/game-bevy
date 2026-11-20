//! `ename_storage` -- user storage: saves, custom presets and other data a player creates.
//!
//! No game code reads or writes user files directly. It goes through [`UserStorage`], a handle to
//! one [`StorageBackend`] the app installs: records addressed by logical [`Key`]s, each a small
//! set of named blobs, written in transactions that commit all or nothing. Every operation
//! returns a future, so it works on wasm and never blocks a frame.
//!
//! Backends: [`FsStorage`] (desktop, a write-ahead journal makes multi-record commits atomic),
//! [`MemoryStorage`] (volatile, for the browser and tests), and any a project writes itself.
//! [`conformance`] checks a backend, including recovery from a commit stopped at each
//! [`FaultPoint`]. With the `bevy` feature, [`StoragePlugin`] installs one.
//!
//! A leaf crate: no first-party dependencies. `ename_engine` adds the plugin with the persistent
//! data directory. See `docs/user-storage.md` and `docs/design/crate-layout.md`.

mod backend;
pub mod conformance;
mod fault;
#[cfg(not(target_arch = "wasm32"))]
mod fs;
mod key;
mod memory;
#[cfg(feature = "bevy")]
mod plugin;
mod storage;

pub use crate::{
    backend::{
        BoxedFuture, Changes, Listing, Record, RecordChange, StorageBackend, StorageError,
        SyncStatus,
    },
    fault::{FaultPoint, FaultSwitch},
    key::{
        BlobName, InvalidName, Key, KeyPrefix, MAX_BLOB_NAME_LEN, MAX_KEY_DEPTH, MAX_KEY_LEN,
        MAX_SEGMENT_LEN, NameError,
    },
    memory::MemoryStorage,
    storage::{Transaction, UserStorage},
};

#[cfg(not(target_arch = "wasm32"))]
pub use crate::fs::FsStorage;
#[cfg(feature = "bevy")]
pub use crate::plugin::{StoragePlugin, StorageTask};
