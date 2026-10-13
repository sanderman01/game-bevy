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

/// Translates `events` onto `sink` until either channel closes.
///
/// Awaits nothing but `events`, so it can never hang on an index that never arrives. An event seen
/// before the index and the server mode are known is dropped: every alias load waits for the index
/// and reads the file after, so it sees the edit anyway, and forwarding it would start a second
/// load racing the first.
///
/// Under a processed server nothing reads this source's unprocessed events (the asset processor
/// builds them regardless of `watch_for_changes_override`), so it stops rather than fill a queue
/// nobody drains.
pub(crate) async fn forward(
    events: Receiver<AssetSourceEvent>,
    sink: Sender<AssetSourceEvent>,
    index: Arc<OnceCell<ContentIndex>>,
    mode: Arc<OnceCell<AssetServerMode>>,
) {
    let mut reverse = None;
    while let Ok(event) = events.recv().await {
        let (Some(index), Some(mode)) = (index.get(), mode.get()) else {
            continue;
        };
        if *mode == AssetServerMode::Processed {
            return;
        }
        let reverse = reverse.get_or_insert_with(|| ReverseIndex::new(index));
        for event in translate(reverse, &event) {
            if sink.send(event).await.is_err() {
                return;
            }
        }
    }
}

/// Every alias resolving to each path. More than one alias may name a file.
pub(crate) struct ReverseIndex(HashMap<PathBuf, Vec<String>>);

impl ReverseIndex {
    pub(crate) fn new(index: &ContentIndex) -> Self {
        let mut map: HashMap<PathBuf, Vec<String>> = HashMap::new();
        for (alias, path) in index.iter() {
            map.entry(path.to_path_buf())
                .or_default()
                .push(alias.to_owned());
        }
        Self(map)
    }
}

/// One file event in, an event per alias of that file out.
///
/// An added or renamed-onto file counts as modified: editors that save by writing a temp file and
/// renaming it over the target produce exactly that, and Bevy ignores `RenamedAsset`. A removed
/// file emits nothing, so the last good asset stays. A removed `.meta` reloads with the meta the
/// reader synthesizes.
pub(crate) fn translate(reverse: &ReverseIndex, event: &AssetSourceEvent) -> Vec<AssetSourceEvent> {
    use AssetSourceEvent as E;
    let (path, make): (_, fn(PathBuf) -> AssetSourceEvent) = match event {
        E::ModifiedAsset(path) | E::AddedAsset(path) | E::RenamedAsset { new: path, .. } => {
            (path, E::ModifiedAsset)
        }
        E::ModifiedMeta(path)
        | E::AddedMeta(path)
        | E::RenamedMeta { new: path, .. }
        | E::RemovedMeta(path)
        | E::RemovedUnknown {
            path,
            is_meta: true,
        } => (path, E::ModifiedMeta),
        _ => return Vec::new(),
    };
    reverse
        .0
        .get(path)
        .into_iter()
        .flatten()
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

    fn sorted(mut events: Vec<E>) -> Vec<E> {
        events.sort_by_key(|event| format!("{event:?}"));
        events
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
                old: p("x.meta"),
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
        let events = translate(&reverse, &E::ModifiedAsset(p("base/core/shared.ron")));
        assert_eq!(
            sorted(events),
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
    fn forward_drops_events_before_the_index_and_mode_are_known() {
        let edit = || vec![E::ModifiedAsset(p("base/core/sky.ron"))];
        assert!(run_forward(edit(), None, Some(AssetServerMode::Unprocessed)).is_empty());
        assert!(run_forward(edit(), Some(index()), None).is_empty());
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

    #[test]
    fn forward_stops_when_the_sink_is_dropped() {
        let (sender, receiver) = async_channel::unbounded();
        let (sink, out) = async_channel::unbounded();
        drop(out);
        // The input stays open: only the closed sink can end the loop.
        sender
            .send_blocking(E::ModifiedAsset(p("base/core/sky.ron")))
            .unwrap();
        let index_cell = Arc::new(OnceCell::new());
        index_cell.set_blocking(index()).unwrap();
        let mode_cell = Arc::new(OnceCell::new());
        mode_cell
            .set_blocking(AssetServerMode::Unprocessed)
            .unwrap();
        block_on(forward(receiver, sink, index_cell, mode_cell));
    }
}
