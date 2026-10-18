//! `asset.list`: every alias in the index with what the asset server and the failure log know
//! about it.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use bevy::{
    asset::{AssetPath, ReflectAsset, UntypedAssetId, io::AssetSourceId},
    prelude::*,
};
use ename_asset_alias::{ALIAS_SOURCE, ContentIndex};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    HandleInfo, Items, Probe, State, failures::AssetLoadFailures, failures::LoggedFailure, limit,
    not_scanned, via,
};
use crate::command::{Call, CommandError};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListParams {
    /// Which assets to list. Default `requested`: every one the server has seen.
    #[serde(default)]
    state: StateFilter,
    /// Only aliases or paths containing this, ignoring case.
    #[serde(default)]
    contains: Option<String>,
    /// Most assets, and most outside-index failures, returned: 1 to 500. Default 50.
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Deserialize, JsonSchema, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StateFilter {
    #[default]
    Requested,
    Loading,
    Loaded,
    Failed,
    NotRequested,
    Any,
}

/// An indexed asset's state: its worst handle, label or dependency.
#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum AssetState {
    NotRequested,
    Loaded,
    Loading,
    Failed,
}

impl From<State> for AssetState {
    fn from(state: State) -> Self {
        match state {
            State::NotLoaded => Self::NotRequested,
            State::Loaded => Self::Loaded,
            State::Loading => Self::Loading,
            State::Failed => Self::Failed,
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(super) struct Listed {
    /// Over the whole index, before `contains` and `state` filter it.
    counts: Counts,
    /// Sorted by alias.
    assets: Vec<AssetEntry>,
    /// Unresolved logged failures whose path no alias in the index claims, newest first.
    failures_outside_index: Items<OutsideFailure>,
    /// More assets matched than `limit`.
    truncated: bool,
}

#[derive(Serialize, JsonSchema, Default)]
struct Counts {
    aliases: usize,
    not_requested: usize,
    loading: usize,
    loaded: usize,
    failed: usize,
}

#[derive(Serialize, JsonSchema)]
struct AssetEntry {
    alias: String,
    /// Under the asset root, with `/`.
    path: String,
    /// The worst of its handles, labels and their dependencies.
    state: AssetState,
    /// Handles for the alias path and the physical path, and for each labeled path that failed.
    handles: Vec<HandleInfo>,
    /// Live labeled sub-assets (`#Mesh0`) of reflect-registered types, by state.
    #[serde(skip_serializing_if = "Option::is_none")]
    labeled: Option<LabeledCounts>,
}

#[derive(Serialize, JsonSchema, Default)]
struct LabeledCounts {
    count: usize,
    loading: usize,
    loaded: usize,
    failed: usize,
}

#[derive(Serialize, JsonSchema)]
struct OutsideFailure {
    path: String,
    error: String,
    /// `FrameCount` when it was logged.
    frame: u32,
}

pub(super) fn list(In(call): In<Call<ListParams>>, world: &World) -> Result<Listed, CommandError> {
    let ListParams {
        state: filter,
        contains,
        limit: requested,
    } = call.params;
    let limit = limit(requested)?;
    let Some(index) = world.get_resource::<ContentIndex>() else {
        return Err(not_scanned());
    };
    let contains = contains.map(|text| text.to_lowercase());
    let aliases = Aliases::new(index);
    let latest = world.resource::<AssetLoadFailures>().latest_by_path();
    let labeled = labeled_by_alias(world, &aliases);

    // Labeled failures go to their alias; any failure no alias claims is listed on its own.
    let mut labeled_failures: HashMap<&str, Vec<&LoggedFailure>> = HashMap::new();
    let mut outside: Vec<&LoggedFailure> = Vec::new();
    for failure in latest.values() {
        let owners = aliases.of(&failure.path.without_label());
        if owners.is_empty() {
            if !failure.resolved {
                outside.push(failure);
            }
        } else if failure.path.label().is_some() {
            for owner in owners {
                labeled_failures.entry(owner).or_default().push(failure);
            }
        }
    }
    outside.sort_by_key(|failure| std::cmp::Reverse(failure.frame));
    let outside: Vec<OutsideFailure> = outside
        .into_iter()
        .filter(|failure| {
            contains.as_ref().is_none_or(|text| {
                failure
                    .path
                    .to_string()
                    .to_lowercase()
                    .contains(text.as_str())
            })
        })
        .map(|failure| OutsideFailure {
            path: failure.path.to_string(),
            error: failure.error.clone(),
            frame: failure.frame,
        })
        .collect();

    let mut counts = Counts::default();
    let mut matched = 0;
    let mut assets = Vec::new();
    let mut entries: Vec<(&str, &Path)> = index.iter().collect();
    entries.sort_by_key(|(alias, _)| *alias);
    for (alias, path) in entries {
        let probe = AliasProbe::new(
            world,
            alias,
            path,
            &latest,
            labeled.get(alias).map_or(&[][..], Vec::as_slice),
            labeled_failures.get(alias).map_or(&[][..], Vec::as_slice),
        );
        let state = probe.state();
        counts.aliases += 1;
        match state {
            AssetState::Failed => counts.failed += 1,
            AssetState::Loading => counts.loading += 1,
            AssetState::Loaded => counts.loaded += 1,
            AssetState::NotRequested => counts.not_requested += 1,
        }
        let wanted = match filter {
            StateFilter::Requested => state != AssetState::NotRequested,
            StateFilter::Loading => state == AssetState::Loading,
            StateFilter::Loaded => state == AssetState::Loaded,
            StateFilter::Failed => state == AssetState::Failed,
            StateFilter::NotRequested => state == AssetState::NotRequested,
            StateFilter::Any => true,
        };
        let text_matches = contains.as_ref().is_none_or(|text| {
            alias.to_lowercase().contains(text.as_str())
                || asset_path_string(path)
                    .to_lowercase()
                    .contains(text.as_str())
        });
        if wanted && text_matches {
            matched += 1;
            // Entries are only built for what is returned; the rest is only counted.
            if assets.len() < limit {
                assets.push(probe.entry(world, state));
            }
        }
    }

    Ok(Listed {
        counts,
        assets,
        failures_outside_index: Items::of(outside.into_iter(), limit),
        truncated: matched > limit,
    })
}

/// The index both ways: which aliases claim a physical path (several may), and whether a name is
/// an alias at all.
struct Aliases<'a> {
    names: HashSet<&'a str>,
    by_path: HashMap<String, Vec<&'a str>>,
}

impl<'a> Aliases<'a> {
    fn new(index: &'a ContentIndex) -> Self {
        let mut names = HashSet::new();
        let mut by_path: HashMap<String, Vec<&'a str>> = HashMap::new();
        for (alias, path) in index.iter() {
            names.insert(alias);
            by_path
                .entry(asset_path_string(path))
                .or_default()
                .push(alias);
        }
        Self { names, by_path }
    }

    /// The aliases a path without a label belongs to: its own name under `alias://` if the index
    /// has it, or every alias claiming its physical path.
    fn of(&self, path: &AssetPath) -> Vec<&'a str> {
        let text = asset_path_string(path.path());
        match path.source() {
            AssetSourceId::Name(name) if name.as_ref() == ALIAS_SOURCE => {
                self.names.get(text.as_str()).copied().into_iter().collect()
            }
            AssetSourceId::Default => self.by_path.get(&text).cloned().unwrap_or_default(),
            AssetSourceId::Name(_) => Vec::new(),
        }
    }
}

/// One path's live ids, with what the server says about each, and its latest logged failure.
struct PathProbe<'w> {
    path: AssetPath<'static>,
    ids: Vec<(UntypedAssetId, Probe)>,
    logged: Option<&'w LoggedFailure>,
}

impl<'w> PathProbe<'w> {
    fn new(
        server: &AssetServer,
        path: AssetPath<'static>,
        logged: Option<&'w LoggedFailure>,
    ) -> Self {
        let ids = server
            .get_path_ids(&path)
            .into_iter()
            .map(|id| (id, Probe::of(server, id)))
            .collect();
        Self { path, ids, logged }
    }

    /// The worst live state, or `failed` from an unresolved failure once every handle dropped.
    fn state(&self) -> Option<State> {
        if self.ids.is_empty() {
            return self
                .logged
                .filter(|failure| !failure.resolved)
                .map(|_| State::Failed);
        }
        self.ids.iter().map(|(_, probe)| probe.effective()).max()
    }

    fn handles(&self, world: &World) -> Vec<HandleInfo> {
        let via = via(&self.path);
        if self.ids.is_empty() {
            return self
                .logged
                .filter(|failure| !failure.resolved)
                .map(|failure| HandleInfo::dropped(via, failure))
                .into_iter()
                .collect();
        }
        self.ids
            .iter()
            .map(|(id, probe)| HandleInfo::live(world, probe, *id, via, &self.path, self.logged))
            .collect()
    }
}

/// Everything known about one alias, cheap enough to gather for every alias in a large index.
struct AliasProbe<'w> {
    alias: &'w str,
    path: &'w Path,
    /// The alias path and the physical path.
    direct: [PathProbe<'w>; 2],
    /// Live labeled assets, from `Assets<T>`.
    labeled: Vec<Probe>,
    /// Labeled paths with a logged failure: a failed label never reaches `Assets<T>`.
    labeled_failed: Vec<PathProbe<'w>>,
}

impl<'w> AliasProbe<'w> {
    fn new(
        world: &'w World,
        alias: &'w str,
        path: &'w Path,
        latest: &HashMap<String, &'w LoggedFailure>,
        labeled: &[UntypedAssetId],
        labeled_failures: &[&'w LoggedFailure],
    ) -> Self {
        let server = world.resource::<AssetServer>();
        let probe = |path: AssetPath<'static>| {
            let logged = latest.get(&path.to_string()).copied();
            PathProbe::new(server, path, logged)
        };
        Self {
            alias,
            path,
            direct: [
                probe(alias_asset_path(alias)),
                probe(AssetPath::from_path(path).into_owned()),
            ],
            labeled: labeled.iter().map(|id| Probe::of(server, *id)).collect(),
            labeled_failed: labeled_failures
                .iter()
                .map(|failure| PathProbe::new(server, failure.path.clone(), Some(failure)))
                .collect(),
        }
    }

    fn state(&self) -> AssetState {
        self.direct
            .iter()
            .chain(&self.labeled_failed)
            .filter_map(PathProbe::state)
            .chain(self.labeled.iter().map(Probe::effective))
            .map(AssetState::from)
            .max()
            .unwrap_or(AssetState::NotRequested)
    }

    fn entry(&self, world: &World, state: AssetState) -> AssetEntry {
        let mut handles: Vec<HandleInfo> = self
            .direct
            .iter()
            .flat_map(|probe| probe.handles(world))
            .collect();
        // A labeled path is listed only while it is failing; a loaded one is counted below.
        handles.extend(
            self.labeled_failed
                .iter()
                .filter(|probe| probe.state() == Some(State::Failed))
                .flat_map(|probe| probe.handles(world)),
        );
        let labeled = (!self.labeled.is_empty()).then(|| {
            let mut counts = LabeledCounts {
                count: self.labeled.len(),
                ..default()
            };
            for probe in &self.labeled {
                match probe.effective() {
                    State::Loading => counts.loading += 1,
                    State::Loaded => counts.loaded += 1,
                    State::Failed => counts.failed += 1,
                    State::NotLoaded => {}
                }
            }
            counts
        });
        AssetEntry {
            alias: self.alias.to_owned(),
            path: asset_path_string(self.path),
            state,
            handles,
            labeled,
        }
    }
}

/// Live labeled assets of every reflect-registered type, filed under each alias of the file they
/// came from.
fn labeled_by_alias<'a>(
    world: &World,
    aliases: &Aliases<'a>,
) -> HashMap<&'a str, Vec<UntypedAssetId>> {
    let server = world.resource::<AssetServer>();
    let mut labeled: HashMap<&'a str, Vec<UntypedAssetId>> = HashMap::new();
    let Some(registry) = world.get_resource::<AppTypeRegistry>() else {
        return labeled;
    };
    let registry = registry.read();
    for registration in registry.iter() {
        let Some(reflect_asset) = registration.data::<ReflectAsset>() else {
            continue;
        };
        for id in reflect_asset.ids(world) {
            let Some(path) = server.get_path(id) else {
                continue;
            };
            if path.label().is_none() {
                continue;
            }
            for alias in aliases.of(&path.without_label()) {
                labeled.entry(alias).or_default().push(id);
            }
        }
    }
    labeled
}

pub(super) fn alias_asset_path(alias: &str) -> AssetPath<'static> {
    // Index aliases are validated to survive this round trip (no `://`, `#` or whitespace).
    AssetPath::from(format!("{ALIAS_SOURCE}://{alias}"))
}

/// A path with `/` separators on every OS, as an asset path prints.
pub(super) fn asset_path_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
