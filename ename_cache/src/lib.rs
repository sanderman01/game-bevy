//! `ename_cache` -- a disk cache for derived data: output that is slow to compute and a pure
//! function of its inputs, such as baked textures or captures.
//!
//! Entries are content addressed. A [`CacheKey`] hashes everything the output depends on, built
//! with a [`KeyHasher`]; a changed input is a different key, so an entry is never stale, only
//! unused. Hash the producing code as well (`include_bytes!` of its source files), so editing it
//! invalidates its entries; a dependency upgrade that changes output needs a manual key bump.
//! [`DiskCache::prune`] removes entries nobody has read for a while.
//!
//! A [`DiskCache`] is one namespace, a folder under the project's cache directory
//! ([`ename_application::Paths::ensure_cache_path`]). Every entry is checksummed and written
//! atomically, so a crash, a second running game or a damaged file reads as a miss. Without a cache
//! directory (the browser, no home) every lookup misses and every store does nothing.
//!
//! Filesystem only, no Bevy, and `ename_application` as its only first-party dependency, so the
//! engine, games and tools can all use it. See `docs/design/crate-layout.md`.

mod key;
mod store;

pub use key::{CacheKey, KeyHasher};
pub use store::{DiskCache, Pruned};
