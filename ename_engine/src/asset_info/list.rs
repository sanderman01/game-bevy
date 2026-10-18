//! `asset.list`: every alias in the index with what the asset server knows about it.

use std::collections::{BTreeMap, HashMap};

use bevy::{
    asset::{AssetPath, LoadState, ReflectAsset, UntypedAssetId, io::AssetSourceId},
    prelude::*,
};
use ename_asset_alias::{ALIAS_SOURCE, ContentIndex};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    HandleInfo, dropped_handle, failures::AssetLoadFailures, handle_info, limit, not_scanned,
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
    /// Most assets returned, 1 to 500. Default 50.
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

#[derive(Serialize, JsonSchema)]
pub(super) struct Listed {
    /// Over the whole index, before `contains` and `state` filter it.
    counts: Counts,
    /// Sorted by alias.
    assets: Vec<AssetEntry>,
    /// Logged failures whose path no alias claims.
    failures_outside_index: Vec<OutsideFailure>,
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
    /// The worst of its handles: `failed`, `loading`, `loaded`, or `not_requested`.
    state: &'static str,
    /// Handles for the alias path and the physical path themselves.
    handles: Vec<HandleInfo>,
    /// Labeled sub-assets (`#Mesh0`) of reflect-registered types, by state.
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
    let by_path: HashMap<String, &str> = index
        .iter()
        .map(|(alias, path)| (asset_path_string(path), alias))
        .collect();
    let labeled = labeled_by_alias(world, &by_path);

    let mut counts = Counts::default();
    let mut matched = Vec::new();
    let mut entries: Vec<(&str, &std::path::Path)> = index.iter().collect();
    entries.sort_by_key(|(alias, _)| *alias);
    for (alias, path) in entries {
        counts.aliases += 1;
        let entry = entry(world, alias, path, labeled.get(alias));
        match entry.state {
            "failed" => counts.failed += 1,
            "loading" => counts.loading += 1,
            "loaded" => counts.loaded += 1,
            _ => counts.not_requested += 1,
        }
        let wanted = match filter {
            StateFilter::Requested => entry.state != "not_requested",
            StateFilter::Loading => entry.state == "loading",
            StateFilter::Loaded => entry.state == "loaded",
            StateFilter::Failed => entry.state == "failed",
            StateFilter::NotRequested => entry.state == "not_requested",
            StateFilter::Any => true,
        };
        let text_matches = contains.as_ref().is_none_or(|text| {
            entry.alias.to_lowercase().contains(text) || entry.path.to_lowercase().contains(text)
        });
        if wanted && text_matches {
            matched.push(entry);
        }
    }
    let truncated = matched.len() > limit;
    matched.truncate(limit);

    Ok(Listed {
        counts,
        assets: matched,
        failures_outside_index: outside_index(world, &by_path, contains.as_deref(), limit),
        truncated,
    })
}

fn entry(
    world: &World,
    alias: &str,
    path: &std::path::Path,
    labeled: Option<&Vec<UntypedAssetId>>,
) -> AssetEntry {
    let server = world.resource::<AssetServer>();
    let alias_path = alias_asset_path(alias);
    let physical = AssetPath::from_path(path);
    let mut handles = Vec::new();
    for (via, asset_path) in [("alias", &alias_path), ("path", &physical)] {
        let ids = server.get_path_ids(asset_path);
        if ids.is_empty() {
            handles.extend(dropped_handle(world, via, asset_path));
        }
        handles.extend(
            ids.into_iter()
                .map(|id| handle_info(world, id, via, asset_path)),
        );
    }
    let labeled = labeled.map(|ids| {
        let mut counts = LabeledCounts {
            count: ids.len(),
            ..default()
        };
        for id in ids {
            match server.get_load_state(*id) {
                Some(LoadState::Loading) => counts.loading += 1,
                Some(LoadState::Loaded) => counts.loaded += 1,
                Some(LoadState::Failed(_)) => counts.failed += 1,
                _ => {}
            }
        }
        counts
    });
    let states = handles.iter().map(|handle| handle.state).chain(
        labeled
            .iter()
            .flat_map(|l| {
                [
                    ("failed", l.failed),
                    ("loading", l.loading),
                    ("loaded", l.loaded),
                ]
            })
            .filter(|(_, n)| *n > 0)
            .map(|(state, _)| state),
    );
    AssetEntry {
        alias: alias.to_owned(),
        path: asset_path_string(path),
        state: worst(states),
        handles,
        labeled,
    }
}

/// `failed` over `loading` over `loaded`; nothing else means the asset was never requested.
fn worst(states: impl Iterator<Item = &'static str>) -> &'static str {
    let mut worst = "not_requested";
    for state in states {
        let rank = |state: &str| match state {
            "failed" => 3,
            "loading" => 2,
            "loaded" => 1,
            _ => 0,
        };
        if rank(state) > rank(worst) {
            worst = state;
        }
    }
    worst
}

/// Live labeled assets of every reflect-registered type, filed under the alias of the file they
/// came from.
fn labeled_by_alias(
    world: &World,
    by_path: &HashMap<String, &str>,
) -> BTreeMap<String, Vec<UntypedAssetId>> {
    let server = world.resource::<AssetServer>();
    let mut labeled: BTreeMap<String, Vec<UntypedAssetId>> = BTreeMap::new();
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
            if let Some(alias) = alias_of(&path, by_path) {
                labeled.entry(alias).or_default().push(id);
            }
        }
    }
    labeled
}

/// The alias a path (label ignored) belongs to: its own name under `alias://`, or the alias that
/// claims its physical path.
fn alias_of(path: &AssetPath, by_path: &HashMap<String, &str>) -> Option<String> {
    let path_text = asset_path_string(path.path());
    match path.source() {
        AssetSourceId::Name(name) if name.as_ref() == ALIAS_SOURCE => Some(path_text),
        AssetSourceId::Default => by_path.get(&path_text).map(|alias| (*alias).to_owned()),
        AssetSourceId::Name(_) => None,
    }
}

/// Logged failures no alias claims, newest per path, newest first.
fn outside_index(
    world: &World,
    by_path: &HashMap<String, &str>,
    contains: Option<&str>,
    limit: usize,
) -> Vec<OutsideFailure> {
    let mut seen = std::collections::HashSet::new();
    world
        .resource::<AssetLoadFailures>()
        .iter()
        .rev()
        .filter(|failure| alias_of(&failure.path.without_label(), by_path).is_none())
        .filter(|failure| seen.insert(failure.path.to_string()))
        .filter(|failure| {
            contains.is_none_or(|text| failure.path.to_string().to_lowercase().contains(text))
        })
        .take(limit)
        .map(|failure| OutsideFailure {
            path: failure.path.to_string(),
            error: failure.error.clone(),
            frame: failure.frame,
        })
        .collect()
}

pub(super) fn alias_asset_path(alias: &str) -> AssetPath<'static> {
    AssetPath::from(format!("{ALIAS_SOURCE}://{alias}"))
}

/// A path with `/` separators on every OS, as an asset path prints.
pub(super) fn asset_path_string(path: &std::path::Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
