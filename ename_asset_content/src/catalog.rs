//! Content kinds a game registers, and loading every descriptor before the catalog is ready.
//!
//! The scan builds the [`ContentCatalog`] from the resolved index. This module then loads the
//! descriptor behind every entry, by alias, and publishes the catalog once each has loaded or
//! failed: a failed one is reported as [`ProblemKind::UnloadableContent`] and left out. Until then
//! `Res<ContentCatalog>` does not exist and [`ContentCatalogState`] is `Loading`.

use bevy::{
    app::App,
    asset::{Asset, AssetServer, LoadState, UntypedHandle},
    ecs::{
        resource::Resource,
        system::{Commands, Res, ResMut},
    },
    log::{info, warn},
    state::prelude::{NextState, States},
};
use ename_asset_alias::{ALIAS_SOURCE, ContentIndex, Problem, ProblemKind};
use ename_asset_package::{ContentCatalog, ContentKinds, ContentReport};
use std::{collections::BTreeMap, path::PathBuf};

/// Whether the [`ContentCatalog`] is ready. Initialised only where `StatesPlugin` is added, which
/// `DefaultPlugins` does; without it, the catalog resource appearing is the only signal.
#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ContentCatalogState {
    /// The scan is running or descriptors are still loading.
    #[default]
    Loading,
    /// Every descriptor has loaded or failed, and `Res<ContentCatalog>` exists.
    Ready,
}

/// Loads one descriptor by its `alias://` path, as the asset type its kind was registered with.
type LoadDescriptor = fn(&AssetServer, String) -> UntypedHandle;

/// The kinds registered with [`ContentAppExt::register_content_kind`].
#[derive(Resource, Default)]
pub(crate) struct ContentKindRegistry {
    pub(crate) kinds: ContentKinds,
    loaders: BTreeMap<String, LoadDescriptor>,
}

/// Registering content kinds on an [`App`].
pub trait ContentAppExt {
    /// Every file whose name ends in `.{extension}` and that wins its alias becomes a catalog
    /// entry of `kind`, loaded as `A` at startup: `register_content_kind::<Map>("map", "map.ron")`.
    /// `A` needs a loader for the extension.
    ///
    /// Register before `Startup`, when the scan reads the kinds. Panics on a kind or an extension
    /// registered twice.
    fn register_content_kind<A: Asset>(&mut self, kind: &str, extension: &str) -> &mut Self;
}

impl ContentAppExt for App {
    fn register_content_kind<A: Asset>(&mut self, kind: &str, extension: &str) -> &mut Self {
        let mut registry = self
            .world_mut()
            .get_resource_or_init::<ContentKindRegistry>();
        if let Err(err) = registry.kinds.insert(kind, extension) {
            panic!("cannot register content kind `{kind}`: {err}");
        }
        registry
            .loaders
            .insert(kind.to_owned(), load_descriptor::<A>);
        self
    }
}

fn load_descriptor<A: Asset>(server: &AssetServer, path: String) -> UntypedHandle {
    server.load::<A>(path).untyped()
}

/// The scanned catalog while its descriptors load.
#[derive(Resource)]
pub(crate) struct PendingCatalog {
    catalog: ContentCatalog,
    /// `(kind, id, handle)` per entry.
    descriptors: Vec<(String, String, UntypedHandle)>,
}

/// Strong handles to every catalogued descriptor, so none unloads while the game runs.
#[derive(Resource)]
pub(crate) struct ContentDescriptors {
    _handles: Vec<UntypedHandle>,
}

impl PendingCatalog {
    /// Starts loading the descriptor of every entry.
    pub(crate) fn start(
        catalog: ContentCatalog,
        registry: &ContentKindRegistry,
        server: &AssetServer,
    ) -> Self {
        let descriptors = catalog
            .iter()
            .filter_map(|entry| {
                let load = registry.loaders.get(&entry.kind)?;
                let handle = load(server, format!("{ALIAS_SOURCE}://{}", entry.id));
                Some((entry.kind.clone(), entry.id.clone(), handle))
            })
            .collect::<Vec<_>>();
        if !descriptors.is_empty() {
            info!("Loading {} content descriptors", descriptors.len());
        }
        Self {
            catalog,
            descriptors,
        }
    }
}

/// Publishes the catalog once every descriptor has loaded or failed.
pub(crate) fn finish_content_catalog(
    mut commands: Commands,
    pending: Res<PendingCatalog>,
    server: Res<AssetServer>,
    index: Option<Res<ContentIndex>>,
    mut report: ResMut<ContentReport>,
    next: Option<ResMut<NextState<ContentCatalogState>>>,
) {
    let mut failed = Vec::new();
    for (kind, id, handle) in &pending.descriptors {
        match server.get_load_state(handle.id()) {
            Some(LoadState::Loaded) => {}
            Some(LoadState::Failed(err)) => failed.push((kind, id, err.to_string())),
            _ => return,
        }
    }

    let mut catalog = pending.catalog.clone();
    for (kind, id, err) in failed {
        catalog.remove(kind, id);
        let detail = format!("`{id}` ({kind}) failed to load and is left out: {err}");
        warn!("{detail}");
        report.problems.push(Problem {
            path: index
                .as_ref()
                .and_then(|index| index.resolve(id))
                .map_or_else(|| PathBuf::from(id), PathBuf::from),
            kind: ProblemKind::UnloadableContent,
            detail,
        });
    }
    info!("Content catalog ready: {} entries", catalog.len());
    let handles = pending
        .descriptors
        .iter()
        .map(|(_, _, handle)| handle.clone())
        .collect();
    commands.insert_resource(ContentDescriptors { _handles: handles });
    commands.insert_resource(catalog);
    commands.remove_resource::<PendingCatalog>();
    if let Some(mut next) = next {
        next.set(ContentCatalogState::Ready);
    }
}
