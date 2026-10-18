//! Read-only commands over asset state: which indexed assets loaded, which failed and why
//! (`asset.list`).
//!
//! Bevy keeps its table of assets private, so `asset.list` enumerates the alias index
//! ([`ContentIndex`]) and asks the asset server about each alias's `alias://` path and physical
//! path by id, never taking a handle. Labeled sub-assets (`#Mesh0`) are found by walking every
//! reflect-registered asset type, since their root handle may be gone. Failed loads are kept in a
//! log of their own, because the server forgets a failure once its handle drops.

mod failures;
mod list;

use bevy::{
    asset::{AssetPath, LoadState, RecursiveDependencyLoadState, UntypedAssetId},
    prelude::*,
};
use schemars::JsonSchema;
use serde::Serialize;

use crate::command::{CommandAppExt, CommandError, CommandScope, CommandSpec};
use failures::{AssetLoadFailures, log_failures};

/// Default and largest `limit` of every list these commands return.
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 500;

/// Registers the failure log and the `asset.*` commands. Needs `AssetPlugin` and
/// [`CommandPlugin`](crate::command::CommandPlugin).
pub struct AssetInfoPlugin;

impl Plugin for AssetInfoPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AssetLoadFailures>()
            .add_systems(PostUpdate, log_failures)
            .register_command(
                CommandSpec::new(
                    "asset.list",
                    "List indexed assets by load state (default: every requested one), with each \
                     failure's error.",
                )
                .scope(CommandScope::Safe)
                .read_only(),
                list::list,
            );
    }
}

/// `limit` checked against [`MAX_LIMIT`], or the default.
fn limit(limit: Option<u32>) -> Result<usize, CommandError> {
    match limit.unwrap_or(DEFAULT_LIMIT) {
        limit @ 1..=MAX_LIMIT => Ok(limit as usize),
        other => Err(CommandError::invalid(
            "invalid_params",
            format!("`limit` must be 1 to {MAX_LIMIT}, not {other}"),
        )),
    }
}

/// One handle the asset server knows, or knew until it dropped.
#[derive(Serialize, JsonSchema, Clone, Debug)]
struct HandleInfo {
    /// `alias` for `alias://<alias>`, `path` for the physical path.
    via: &'static str,
    /// The asset type's path, when registered.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    type_path: Option<String>,
    /// `not_loaded`, `loading`, `loaded` or `failed`.
    state: &'static str,
    /// The state of everything it depends on, recursively.
    dependencies: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// The last logged failure for this path, when it has loaded since.
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_error: Option<String>,
    /// The handle is gone; state and error come from the failure log.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    dropped: bool,
}

fn load_state_name(state: &LoadState) -> &'static str {
    match state {
        LoadState::NotLoaded => "not_loaded",
        LoadState::Loading => "loading",
        LoadState::Loaded => "loaded",
        LoadState::Failed(_) => "failed",
    }
}

fn dependency_state_name(state: &RecursiveDependencyLoadState) -> &'static str {
    match state {
        RecursiveDependencyLoadState::NotLoaded => "not_loaded",
        RecursiveDependencyLoadState::Loading => "loading",
        RecursiveDependencyLoadState::Loaded => "loaded",
        RecursiveDependencyLoadState::Failed(_) => "failed",
    }
}

/// The asset type's registered path.
fn type_path(world: &World, id: UntypedAssetId) -> Option<String> {
    let registry = world.get_resource::<AppTypeRegistry>()?.read();
    Some(
        registry
            .get(id.type_id())?
            .type_info()
            .type_path()
            .to_owned(),
    )
}

/// What the server says about `id`, with the failure log's last word on `path`.
fn handle_info(
    world: &World,
    id: UntypedAssetId,
    via: &'static str,
    path: &AssetPath,
) -> HandleInfo {
    let server = world.resource::<AssetServer>();
    let (state, _, dependencies) = server.get_load_states(id).unwrap_or((
        LoadState::NotLoaded,
        bevy::asset::DependencyLoadState::NotLoaded,
        RecursiveDependencyLoadState::NotLoaded,
    ));
    let error = match &state {
        LoadState::Failed(error) => Some(error.to_string()),
        _ => None,
    };
    let previous_error = match error {
        Some(_) => None,
        None => world
            .resource::<AssetLoadFailures>()
            .latest(path)
            .map(|failure| failure.error.clone()),
    };
    HandleInfo {
        via,
        type_path: type_path(world, id),
        state: load_state_name(&state),
        dependencies: dependency_state_name(&dependencies),
        error,
        previous_error,
        dropped: false,
    }
}

/// A handle that failed and dropped, rebuilt from the failure log.
fn dropped_handle(world: &World, via: &'static str, path: &AssetPath) -> Option<HandleInfo> {
    let failure = world.resource::<AssetLoadFailures>().latest(path)?;
    Some(HandleInfo {
        via,
        type_path: None,
        state: "failed",
        dependencies: "not_loaded",
        error: Some(failure.error.clone()),
        previous_error: None,
        dropped: true,
    })
}
