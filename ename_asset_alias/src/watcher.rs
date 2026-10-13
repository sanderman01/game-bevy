//! Live reload for the `alias://` source.
//!
//! Bevy reloads by `AssetPath` and source. The default source's watcher reports
//! `basegame/core/x.ron`, but every handle was loaded as `alias://core::x`, so nothing matches. This
//! watcher runs a second platform watcher over the asset root and forwards its events renamed to
//! the aliases that resolve to each path.
//!
//! Only content and `.meta` edits of files already in the index reload. New files, removals, and
//! edits to `.alias`, `_alias_rules.toml` or `manifest.toml` need a restart, because the index is
//! filled once.

use crate::ContentIndex;
use async_channel::{Receiver, Sender};
use async_lock::OnceCell;
use bevy::{
    asset::{
        AssetServerMode,
        io::{AssetSource, AssetSourceEvent, AssetWatcher},
    },
    tasks::{IoTaskPool, Task},
};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

/// Matches the default source's watcher, so both report an edit at about the same time.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// The inner platform watcher and the task forwarding its events. Dropping it cancels the task.
struct AliasWatcher {
    _inner: Box<dyn AssetWatcher>,
    _task: Task<()>,
}

impl AssetWatcher for AliasWatcher {}

/// The closure `AssetSourceBuilder::with_watcher` takes, watching `root`.
///
/// Returns `None` where the platform has no watcher (no `file_watcher` feature, wasm, Android).
pub(crate) fn alias_watcher(
    index: Arc<OnceCell<ContentIndex>>,
    mode: Arc<OnceCell<AssetServerMode>>,
    root: String,
) -> impl FnMut(Sender<AssetSourceEvent>) -> Option<Box<dyn AssetWatcher>> + Send + Sync + 'static {
    let mut make_inner = AssetSource::get_default_watcher(root, DEBOUNCE);
    move |sink| {
        let (sender, events) = async_channel::unbounded();
        let inner = make_inner(sender)?;
        let task = IoTaskPool::get().spawn(forward(events, sink, index.clone(), mode.clone()));
        Some(Box::new(AliasWatcher {
            _inner: inner,
            _task: task,
        }))
    }
}

/// Translates `events` onto `sink` until the inner watcher closes `events`.
///
/// Awaits nothing but `events`, so it can never hang on an index that never arrives. It keeps
/// draining `events` even once it stops sending: Bevy's file watcher unwraps its send, so a
/// dropped receiver would panic the watcher's thread.
///
/// An event that arrives before the index exists is dropped: no alias has been read yet, and every
/// alias load reads its file after the index fills. An edit made just before the fill can still
/// reach `sink` after it and reload a file that was read fresh; that costs one redundant load.
///
/// Under a processed server nothing reads this source's unprocessed events (the asset processor
/// builds them regardless of `watch_for_changes_override`), so once the mode is known to be
/// processed it stops sending. Until `Startup` records the mode, events are forwarded.
async fn forward(
    events: Receiver<AssetSourceEvent>,
    sink: Sender<AssetSourceEvent>,
    index: Arc<OnceCell<ContentIndex>>,
    mode: Arc<OnceCell<AssetServerMode>>,
) {
    let mut reverse = None;
    let mut sending = true;
    while let Ok(event) = events.recv().await {
        sending &= mode.get() != Some(&AssetServerMode::Processed);
        let Some(index) = index.get().filter(|_| sending) else {
            continue;
        };
        let reverse = reverse.get_or_insert_with(|| ReverseIndex::new(index));
        for event in translate(reverse, &event) {
            if sink.send(event).await.is_err() {
                sending = false;
                break;
            }
        }
    }
}

/// Every alias resolving to each path. More than one alias may name a file.
struct ReverseIndex(HashMap<PathBuf, Vec<String>>);

impl ReverseIndex {
    fn new(index: &ContentIndex) -> Self {
        let mut map: HashMap<PathBuf, Vec<String>> = HashMap::new();
        for (alias, path) in index.iter() {
            map.entry(path.to_path_buf())
                .or_default()
                .push(alias.to_owned());
        }
        Self(map)
    }
}

/// One file event in, an event per affected alias out, sorted and without repeats.
///
/// An added or renamed-onto file counts as modified: editors that save by writing a temp file and
/// renaming it over the target produce exactly that, and Bevy ignores `RenamedAsset`. A removed
/// file emits nothing, so the last good asset stays. A removed `.meta` reloads with the meta the
/// reader synthesizes; a renamed one reloads both the asset it left and the one it now describes.
fn translate(reverse: &ReverseIndex, event: &AssetSourceEvent) -> Vec<AssetSourceEvent> {
    use AssetSourceEvent as E;
    let (paths, meta) = match event {
        E::ModifiedAsset(path) | E::AddedAsset(path) | E::RenamedAsset { new: path, .. } => {
            ([Some(path), None], false)
        }
        E::RenamedMeta { old, new } => ([Some(old), Some(new)], true),
        E::ModifiedMeta(path)
        | E::AddedMeta(path)
        | E::RemovedMeta(path)
        | E::RemovedUnknown {
            path,
            is_meta: true,
        } => ([Some(path), None], true),
        _ => return Vec::new(),
    };
    let make = if meta {
        E::ModifiedMeta
    } else {
        E::ModifiedAsset
    };
    let mut aliases: Vec<&str> = paths
        .into_iter()
        .flatten()
        .filter_map(|path| reverse.0.get(path))
        .flatten()
        .map(String::as_str)
        .collect();
    aliases.sort_unstable();
    aliases.dedup();
    aliases
        .into_iter()
        .map(|alias| make(PathBuf::from(alias)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{AssetSourceEvent as E, ReverseIndex, forward, translate};
    use crate::ContentIndex;
    use async_lock::OnceCell;
    use bevy::asset::AssetServerMode;
    use futures_lite::future::block_on;
    use std::{path::PathBuf, sync::Arc};

    fn index() -> ContentIndex {
        let mut index = ContentIndex::default();
        index.insert("core::sky", "base/core/sky.ron").unwrap();
        index.insert("core::a", "base/core/shared.ron").unwrap();
        index.insert("core::b", "base/core/shared.ron").unwrap();
        // `core::ship` overridden by a mod: the base file is shadowed.
        index.insert("core::ship", "mods/big/ship.ron").unwrap();
        index
    }

    fn p(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    #[test]
    fn content_edits_become_modified_aliases() {
        let reverse = ReverseIndex::new(&index());
        let modified = vec![E::ModifiedAsset(p("core::sky"))];
        assert_eq!(
            translate(&reverse, &E::ModifiedAsset(p("base/core/sky.ron"))),
            modified
        );
        assert_eq!(
            translate(&reverse, &E::AddedAsset(p("base/core/sky.ron"))),
            modified
        );
        let atomic_save = E::RenamedAsset {
            old: p("base/core/.sky.ron.tmp"),
            new: p("base/core/sky.ron"),
        };
        assert_eq!(translate(&reverse, &atomic_save), modified);
    }

    #[test]
    fn meta_changes_including_removal_become_modified_meta() {
        let reverse = ReverseIndex::new(&index());
        let expected = vec![E::ModifiedMeta(p("core::sky"))];
        let path = p("base/core/sky.ron");
        for event in [
            E::ModifiedMeta(path.clone()),
            E::AddedMeta(path.clone()),
            E::RemovedMeta(path.clone()),
            E::RenamedMeta {
                old: p("base/core/unaliased.ron"),
                new: path.clone(),
            },
            E::RemovedUnknown {
                path: path.clone(),
                is_meta: true,
            },
        ] {
            assert_eq!(translate(&reverse, &event), expected, "{event:?}");
        }
    }

    /// The asset the `.meta` left reloads with the synthesized meta, the one it moved to with it.
    #[test]
    fn a_renamed_meta_reloads_both_ends_once() {
        let reverse = ReverseIndex::new(&index());
        let rename = |old: &str, new: &str| E::RenamedMeta {
            old: p(old),
            new: p(new),
        };
        assert_eq!(
            translate(&reverse, &rename("mods/big/ship.ron", "base/core/sky.ron")),
            vec![
                E::ModifiedMeta(p("core::ship")),
                E::ModifiedMeta(p("core::sky"))
            ]
        );
        // Both ends name the same aliases: each reloads once.
        assert_eq!(
            translate(
                &reverse,
                &rename("base/core/shared.ron", "base/core/shared.ron")
            ),
            vec![E::ModifiedMeta(p("core::a")), E::ModifiedMeta(p("core::b"))]
        );
    }

    #[test]
    fn removals_and_folders_emit_nothing() {
        let reverse = ReverseIndex::new(&index());
        let path = p("base/core/sky.ron");
        for event in [
            E::RemovedAsset(path.clone()),
            E::RemovedUnknown {
                path: path.clone(),
                is_meta: false,
            },
            E::AddedFolder(p("base/core")),
            E::RemovedFolder(p("base/core")),
        ] {
            assert!(translate(&reverse, &event).is_empty(), "{event:?}");
        }
    }

    #[test]
    fn paths_outside_the_index_emit_nothing() {
        let reverse = ReverseIndex::new(&index());
        for path in [
            "base/core/ship.ron",
            "base/core/unaliased.ron",
            "base/core/sky.ron.alias",
        ] {
            assert!(
                translate(&reverse, &E::ModifiedAsset(p(path))).is_empty(),
                "{path}"
            );
        }
    }

    #[test]
    fn every_alias_of_a_shared_file_is_modified() {
        let reverse = ReverseIndex::new(&index());
        assert_eq!(
            translate(&reverse, &E::ModifiedAsset(p("base/core/shared.ron"))),
            vec![
                E::ModifiedAsset(p("core::a")),
                E::ModifiedAsset(p("core::b"))
            ]
        );
    }

    /// Runs `forward` over `events` with the given cells, returning what reached the sink.
    fn run_forward(
        events: Vec<E>,
        index: Option<ContentIndex>,
        mode: Option<AssetServerMode>,
    ) -> Vec<E> {
        let (sender, receiver) = async_channel::unbounded();
        let (sink, out) = async_channel::unbounded();
        for event in events {
            sender.send_blocking(event).unwrap();
        }
        drop(sender);
        let index_cell = Arc::new(OnceCell::new());
        if let Some(index) = index {
            index_cell.set_blocking(index).unwrap();
        }
        let mode_cell = Arc::new(OnceCell::new());
        if let Some(mode) = mode {
            mode_cell.set_blocking(mode).unwrap();
        }
        // Returns because the input channel is closed: the shutdown path.
        block_on(forward(receiver, sink, index_cell, mode_cell));
        std::iter::from_fn(|| out.try_recv().ok()).collect()
    }

    #[test]
    fn forward_routes_events_and_ends_when_the_watcher_closes() {
        let out = run_forward(
            vec![
                E::ModifiedAsset(p("base/core/sky.ron")),
                E::ModifiedAsset(p("base/core/unaliased.ron")),
                E::RemovedMeta(p("mods/big/ship.ron")),
            ],
            Some(index()),
            Some(AssetServerMode::Unprocessed),
        );
        assert_eq!(
            out,
            vec![
                E::ModifiedAsset(p("core::sky")),
                E::ModifiedMeta(p("core::ship"))
            ]
        );
    }

    #[test]
    fn forward_drops_events_only_before_the_index() {
        let edit = || vec![E::ModifiedAsset(p("base/core/sky.ron"))];
        assert!(run_forward(edit(), None, Some(AssetServerMode::Unprocessed)).is_empty());
        // `Startup` has not recorded the mode yet, but an alias may already have been read.
        assert_eq!(
            run_forward(edit(), Some(index()), None),
            vec![E::ModifiedAsset(p("core::sky"))]
        );
    }

    #[test]
    fn forward_sends_nothing_under_a_processed_server() {
        let out = run_forward(
            vec![E::ModifiedAsset(p("base/core/sky.ron"))],
            Some(index()),
            Some(AssetServerMode::Processed),
        );
        assert!(out.is_empty());
    }

    /// Runs `forward` on a thread with its input open, sending `edits` events one at a time, each
    /// only after the previous one was drained. A `forward` that stopped receiving fails a send or
    /// times out. Returns what reached the sink, or nothing if `drop_sink`.
    fn assert_keeps_draining(mode: AssetServerMode, drop_sink: bool, edits: usize) -> Vec<E> {
        let (sender, receiver) = async_channel::unbounded();
        let (sink, out) = async_channel::unbounded();
        let index_cell = Arc::new(OnceCell::new());
        index_cell.set_blocking(index()).unwrap();
        let mode_cell = Arc::new(OnceCell::new());
        mode_cell.set_blocking(mode).unwrap();
        let out = if drop_sink {
            drop(out);
            None
        } else {
            Some(out)
        };
        let task = std::thread::spawn(move || {
            block_on(forward(receiver, sink, index_cell, mode_cell));
        });
        for _ in 0..edits {
            sender
                .send_blocking(E::ModifiedAsset(p("base/core/sky.ron")))
                .expect("forward dropped its receiver");
            let start = std::time::Instant::now();
            while !sender.is_empty() {
                assert!(
                    start.elapsed() < std::time::Duration::from_secs(5),
                    "forward stopped draining"
                );
                std::thread::yield_now();
            }
        }
        drop(sender);
        task.join().expect("forward panicked");
        out.map(|out| std::iter::from_fn(|| out.try_recv().ok()).collect())
            .unwrap_or_default()
    }

    /// Bevy's file watcher unwraps its send, so `forward` must keep receiving after it decides
    /// to stop sending.
    #[test]
    fn forward_keeps_draining_under_a_processed_server() {
        assert!(assert_keeps_draining(AssetServerMode::Processed, false, 3).is_empty());
    }

    #[test]
    fn forward_keeps_draining_after_the_sink_closes() {
        assert_keeps_draining(AssetServerMode::Unprocessed, true, 3);
    }
}
