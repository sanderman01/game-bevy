//! Content kinds a game registers, and loading every descriptor before the catalog is ready.
//!
//! The scan builds the [`ContentCatalog`] from the resolved index. This module then loads the
//! descriptor behind every entry, by alias, and publishes the catalog once each has loaded or
//! failed: a failed one is reported as [`ProblemKind::UnloadableContent`] and left out. Until then
//! `Res<ContentCatalog>` does not exist and [`ContentCatalogState`] is `Loading`.
//!
//! The catalog stays fixed after that, and only the report follows later reloads; see
//! `watch_content_descriptors`.

use crate::lenient::RonContentLoader;
use bevy::{
    app::App,
    asset::{Asset, AssetApp, AssetServer, Assets, LoadState, UntypedHandle},
    ecs::{
        resource::Resource,
        system::{Commands, Res, ResMut},
        world::World,
    },
    log::{info, warn},
    state::prelude::{NextState, States},
};
use ename_asset_alias::{ALIAS_SOURCE, ContentIndex, Problem, ProblemKind};
use ename_asset_package::{ContentCatalog, ContentKinds, ContentReport};
use serde::de::DeserializeOwned;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

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
    loaders: BTreeMap<String, KindLoader>,
}

/// How one kind's descriptors load, and the check that they can.
#[derive(Clone, Copy)]
struct KindLoader {
    load: LoadDescriptor,
    /// Whether `Assets<A>` exists, and `A`'s type name for the message when it does not.
    is_asset_type: fn(&World) -> bool,
    type_name: &'static str,
}

impl ContentKindRegistry {
    /// Panics, naming the kind, when a kind's asset type is not an asset type of the app. Run once
    /// every plugin has built, so a missing `init_asset` shows at startup and not when the first
    /// file of that kind is added.
    pub(crate) fn assert_asset_types(&self, world: &World) {
        for (kind, loader) in &self.loaders {
            assert!(
                (loader.is_asset_type)(world),
                "content kind `{kind}` loads as `{}`, which is not an asset type of this app: \
                 call `app.init_asset::<{}>()`, or register the kind with \
                 `register_ron_content_kind`, which does",
                loader.type_name,
                loader.type_name
            );
        }
    }
}

/// Registering content kinds on an [`App`].
pub trait ContentAppExt {
    /// Every file whose name ends in `.{extension}` and that wins its alias becomes a catalog
    /// entry of `kind`, loaded as `A` at startup: `register_content_kind::<Map>("map", "map.ron")`.
    /// `A` needs a loader of its own for the extension; one that reads RON should use
    /// [`read_ron_lenient`](crate::read_ron_lenient) so unknown fields are reported.
    ///
    /// Register before `Startup`, when the scan reads the kinds. Panics on a kind or an extension
    /// registered twice, and at startup when `A` is not an asset type of the app (`init_asset`).
    fn register_content_kind<A: Asset>(&mut self, kind: &str, extension: &str) -> &mut Self;

    /// [`register_content_kind`](Self::register_content_kind) with a
    /// [`RonContentLoader`](crate::RonContentLoader) for `A`, which reads RON leniently and
    /// reports unknown fields. Makes `A` an asset type of the app if it is not one yet.
    fn register_ron_content_kind<A: Asset + DeserializeOwned>(
        &mut self,
        kind: &str,
        extension: &'static str,
    ) -> &mut Self;

    /// Every kind registered so far. A game test compares this with
    /// [`ContentKinds::from_workspace_manifest`], the list `ename_check` reads, so the two cannot
    /// drift apart.
    fn registered_content_kinds(&self) -> ContentKinds;
}

impl ContentAppExt for App {
    fn register_content_kind<A: Asset>(&mut self, kind: &str, extension: &str) -> &mut Self {
        let mut registry = self
            .world_mut()
            .get_resource_or_init::<ContentKindRegistry>();
        if let Err(err) = registry.kinds.insert(kind, extension) {
            panic!("cannot register content kind `{kind}`: {err}");
        }
        registry.loaders.insert(
            kind.to_owned(),
            KindLoader {
                load: load_descriptor::<A>,
                is_asset_type: |world| world.contains_resource::<Assets<A>>(),
                type_name: std::any::type_name::<A>(),
            },
        );
        self
    }

    fn register_ron_content_kind<A: Asset + DeserializeOwned>(
        &mut self,
        kind: &str,
        extension: &'static str,
    ) -> &mut Self {
        // Guarded, so a game that already called `init_asset` is not registered twice.
        if !self.world().contains_resource::<Assets<A>>() {
            self.init_asset::<A>();
        }
        let loader = RonContentLoader::<A>::new(extension, self.world_mut());
        self.register_asset_loader(loader)
            .register_content_kind::<A>(kind, extension)
    }

    fn registered_content_kinds(&self) -> ContentKinds {
        self.world()
            .get_resource::<ContentKindRegistry>()
            .map(|registry| registry.kinds.clone())
            .unwrap_or_default()
    }
}

fn load_descriptor<A: Asset>(server: &AssetServer, path: String) -> UntypedHandle {
    server.load::<A>(path).untyped()
}

/// One catalogued descriptor and its handle.
struct Descriptor {
    kind: String,
    id: String,
    handle: UntypedHandle,
}

/// The scanned catalog while its descriptors load.
#[derive(Resource)]
pub(crate) struct PendingCatalog {
    catalog: ContentCatalog,
    descriptors: Vec<Descriptor>,
}

/// Every catalogued descriptor after the catalog is ready. The handles are strong, so none
/// unloads while the game runs.
#[derive(Resource)]
pub(crate) struct ContentDescriptors {
    descriptors: Vec<Descriptor>,
    /// Indices into `descriptors` of the ones failing now.
    failed: BTreeSet<usize>,
    /// Indices of the ones that failed at startup and so are not in the catalog. Never cleared:
    /// they stay out until a restart, whatever their load state.
    excluded: BTreeSet<usize>,
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
                let loader = registry.loaders.get(&entry.kind)?;
                Some(Descriptor {
                    kind: entry.kind.clone(),
                    id: entry.id.clone(),
                    handle: (loader.load)(server, format!("{ALIAS_SOURCE}://{}", entry.id)),
                })
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
    server: Res<AssetServer>,
    index: Option<Res<ContentIndex>>,
    mut report: ResMut<ContentReport>,
    next: Option<ResMut<NextState<ContentCatalogState>>>,
    pending: Res<PendingCatalog>,
) {
    let mut errors = BTreeMap::new();
    for (i, descriptor) in pending.descriptors.iter().enumerate() {
        match server.get_load_state(descriptor.handle.id()) {
            Some(LoadState::Loaded) => {}
            Some(LoadState::Failed(err)) => {
                errors.insert(i, err.to_string());
            }
            _ => return,
        }
    }

    for (&i, err) in &errors {
        report.problems.push(unloadable(
            index.as_deref(),
            &pending.descriptors[i],
            &excluded_failure(err),
        ));
    }
    let failed: BTreeSet<usize> = errors.into_keys().collect();

    let catalog = pending.catalog.clone().without(failed.iter().map(|&i| {
        let descriptor = &pending.descriptors[i];
        (descriptor.kind.as_str(), descriptor.id.as_str())
    }));
    info!("Content catalog ready: {} entries", catalog.len());
    commands.remove_resource::<PendingCatalog>();
    commands.insert_resource(catalog);
    commands.insert_resource(ContentDescriptors {
        descriptors: pending
            .descriptors
            .iter()
            .map(|descriptor| Descriptor {
                kind: descriptor.kind.clone(),
                id: descriptor.id.clone(),
                handle: descriptor.handle.clone(),
            })
            .collect(),
        excluded: failed.clone(),
        failed,
    });
    if let Some(mut next) = next {
        next.set(ContentCatalogState::Ready);
    }
}

/// After the catalog is ready, keeps each descriptor's `UnloadableContent` line in step with its
/// load state.
///
/// A descriptor in the catalog gains a line when a reload fails and loses it when it loads again.
/// One left out at startup always has a line, since its entry is missing until a restart: the
/// failure while it fails, and a note that it joins after a restart once it loads.
pub(crate) fn watch_content_descriptors(
    mut descriptors: ResMut<ContentDescriptors>,
    server: Res<AssetServer>,
    index: Option<Res<ContentIndex>>,
    mut report: ResMut<ContentReport>,
) {
    let ContentDescriptors {
        descriptors,
        failed,
        excluded,
    } = &mut *descriptors;
    let index = index.as_deref();
    for (i, descriptor) in descriptors.iter().enumerate() {
        let line = match server.get_load_state(descriptor.handle.id()) {
            Some(LoadState::Failed(err)) if failed.insert(i) => Some(if excluded.contains(&i) {
                excluded_failure(&err)
            } else {
                format!("failed to reload; the catalog keeps it until a restart: {err}")
            }),
            Some(LoadState::Loaded) if failed.remove(&i) => {
                info!("`{}` ({}) loads again", descriptor.id, descriptor.kind);
                excluded.contains(&i).then(|| {
                    "loads now, and joins the catalog after a restart; it failed at startup"
                        .to_owned()
                })
            }
            _ => continue,
        };
        let path = descriptor_path(index, &descriptor.id);
        report.problems.retain(|problem| {
            problem.kind != ProblemKind::UnloadableContent || problem.path != path
        });
        if let Some(line) = line {
            report.problems.push(unloadable(index, descriptor, &line));
        }
    }
}

fn excluded_failure(err: &impl std::fmt::Display) -> String {
    format!("failed to load at startup and is left out of the catalog until a restart: {err}")
}

fn descriptor_path(index: Option<&ContentIndex>, id: &str) -> PathBuf {
    index
        .and_then(|index| index.resolve(id))
        .map_or_else(|| PathBuf::from(id), PathBuf::from)
}

fn unloadable(index: Option<&ContentIndex>, descriptor: &Descriptor, what: &str) -> Problem {
    let detail = format!("`{}` ({}) {what}", descriptor.id, descriptor.kind);
    warn!("{detail}");
    Problem {
        path: descriptor_path(index, &descriptor.id),
        kind: ProblemKind::UnloadableContent,
        detail,
        other: None,
    }
}
