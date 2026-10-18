//! Read-only commands over asset and content state: the package scan as the running game sees it
//! (`content.report`), alias to path and back (`content.resolve`), which indexed assets loaded,
//! which failed and why (`asset.list`), and what one of them holds (`asset.inspect`).
//!
//! Bevy keeps its table of assets private, so `asset.list` enumerates the alias index
//! ([`ContentIndex`](ename_asset_content::ContentIndex)) and asks the asset server about each
//! alias's `alias://` path and physical path by id, never taking a handle. Labeled sub-assets
//! (`#Mesh0`) are found by walking every reflect-registered asset type, since their root handle
//! may be gone. Failed loads are kept in a log of their own, because the server forgets a failure
//! once its handle drops; a failed label never reaches `Assets<T>`, so only the log finds it.

mod content;
mod failures;
mod inspect;
mod list;

use bevy::{
    asset::{
        AssetPath, DependencyLoadState, LoadState, RecursiveDependencyLoadState, UntypedAssetId,
    },
    prelude::*,
};
use schemars::JsonSchema;
use serde::Serialize;

use crate::command::{CommandAppExt, CommandError, CommandScope, CommandSpec};
use failures::{AssetLoadFailures, LoggedFailure, log_failures, resolve_failures};

/// Default and largest `limit` of every list these commands return.
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 500;

/// Registers the failure log and the `content.*` and `asset.*` commands. Needs `AssetPlugin` and
/// [`CommandPlugin`](crate::command::CommandPlugin); the commands that read the scan answer
/// `no_content_scan` without `AssetContentPlugin`.
pub struct AssetInfoPlugin;

impl Plugin for AssetInfoPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AssetLoadFailures>()
            // Resolving after logging, so a failure and its recovery in one frame end resolved.
            .add_systems(PostUpdate, (log_failures, resolve_failures).chain())
            .register_command(
                CommandSpec::new(
                    "content.report",
                    "The package scan as the game sees it: asset root, search paths, load order \
                     file, packages, disabled packages, contests and problems.",
                )
                .scope(CommandScope::Safe)
                .read_only(),
                content::report,
            )
            .register_command(
                CommandSpec::new(
                    "content.resolve",
                    "Resolve an alias to its path, a path to the aliases claiming it, or search \
                     both by substring; each with its package.",
                )
                .scope(CommandScope::Safe)
                .read_only(),
                content::resolve,
            )
            .register_command(
                CommandSpec::new(
                    "asset.list",
                    "List indexed assets by load state (default: every requested one), with each \
                     failure's error, labeled and dependency failures included.",
                )
                .scope(CommandScope::Safe)
                .read_only(),
                list::list,
            )
            .register_command(
                CommandSpec::new(
                    "asset.inspect",
                    "One asset's handles, load states, errors, and a summary of the loaded Image, \
                     Mesh, StandardMaterial, GltfMaterial or Gltf.",
                )
                .scope(CommandScope::Safe)
                .read_only(),
                inspect::inspect,
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

/// A list cut to `limit`, with its full length.
#[derive(Serialize, JsonSchema)]
struct Items<T> {
    count: usize,
    truncated: bool,
    items: Vec<T>,
}

impl<T> Items<T> {
    fn of(all: impl ExactSizeIterator<Item = T>, limit: usize) -> Self {
        let count = all.len();
        Self {
            count,
            truncated: count > limit,
            items: all.take(limit).collect(),
        }
    }
}

/// An error's message without the backtrace a `BevyError` appends when backtraces are on: it
/// starts at an indented `0:` frame followed by an `at <file>` line.
fn error_text(error: &dyn std::fmt::Display) -> String {
    let text = error.to_string();
    let lines: Vec<&str> = text.lines().collect();
    let backtrace = lines.windows(2).position(|pair| {
        pair[0].starts_with(' ')
            && pair[0].trim_start().starts_with("0: ")
            && pair[1].trim_start().starts_with("at ")
    });
    let kept = backtrace.map_or(&lines[..], |start| &lines[..start]);
    kept.join("\n").trim_end().to_owned()
}

fn not_scanned() -> CommandError {
    CommandError::failed(
        "content_not_scanned",
        "the package scan has not finished, so there is no alias index yet",
    )
    .with_hint("`content.report` says when the scan is done")
}

/// A load state, ordered from least to most in need of attention.
#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum State {
    NotLoaded,
    Loaded,
    Loading,
    Failed,
}

impl State {
    fn of_load(state: &LoadState) -> Self {
        match state {
            LoadState::NotLoaded => Self::NotLoaded,
            LoadState::Loading => Self::Loading,
            LoadState::Loaded => Self::Loaded,
            LoadState::Failed(_) => Self::Failed,
        }
    }

    fn of_dependencies(state: &RecursiveDependencyLoadState) -> Self {
        match state {
            RecursiveDependencyLoadState::NotLoaded => Self::NotLoaded,
            RecursiveDependencyLoadState::Loading => Self::Loading,
            RecursiveDependencyLoadState::Loaded => Self::Loaded,
            RecursiveDependencyLoadState::Failed(_) => Self::Failed,
        }
    }
}

/// What the server knows about one id, cheap enough to ask for every alias.
struct Probe {
    own: LoadState,
    recursive: RecursiveDependencyLoadState,
}

impl Probe {
    fn of(server: &AssetServer, id: UntypedAssetId) -> Self {
        let (own, _, recursive) = server.get_load_states(id).unwrap_or((
            LoadState::NotLoaded,
            DependencyLoadState::NotLoaded,
            RecursiveDependencyLoadState::NotLoaded,
        ));
        Self { own, recursive }
    }

    /// The asset's own state, raised to `loading` or `failed` by what it depends on.
    fn effective(&self) -> State {
        let own = State::of_load(&self.own);
        let dependencies = State::of_dependencies(&self.recursive);
        if own == State::Loaded {
            own.max(dependencies)
        } else {
            own
        }
    }
}

/// One handle the asset server knows, or knew until it dropped.
#[derive(Serialize, JsonSchema, Clone, Debug)]
struct HandleInfo {
    /// `alias` for an `alias://` path, `path` for a physical one.
    via: &'static str,
    /// The asset path, label included.
    path: String,
    /// The asset type's path, when registered.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    type_path: Option<String>,
    /// The asset's own load state.
    state: State,
    /// The state of everything it depends on, recursively.
    dependencies: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Why a dependency failed, when one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    dependency_error: Option<String>,
    /// The last logged failure for this path, when it has loaded since.
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_error: Option<String>,
    /// The handle is gone; state and error come from the failure log.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    dropped: bool,
}

impl HandleInfo {
    /// What the server says about `id`, with the failure log's last word on `path`.
    fn live(
        world: &World,
        probe: &Probe,
        id: UntypedAssetId,
        via: &'static str,
        path: &AssetPath,
        logged: Option<&LoggedFailure>,
    ) -> Self {
        let error = match &probe.own {
            LoadState::Failed(error) => Some(error_text(error)),
            _ => None,
        };
        // Bevy copies an asset's own failure into its dependency states too.
        let dependency_error = match &probe.recursive {
            RecursiveDependencyLoadState::Failed(cause) if error.is_none() => {
                Some(error_text(cause))
            }
            _ => None,
        };
        Self {
            via,
            path: path.to_string(),
            type_path: type_path(world, id),
            state: State::of_load(&probe.own),
            dependencies: State::of_dependencies(&probe.recursive),
            previous_error: logged
                .filter(|_| error.is_none())
                .map(|failure| failure.error.clone()),
            error,
            dependency_error,
            dropped: false,
        }
    }

    /// A handle that failed and dropped, rebuilt from the failure log.
    fn dropped(via: &'static str, failure: &LoggedFailure) -> Self {
        Self {
            via,
            path: failure.path.to_string(),
            type_path: None,
            state: State::Failed,
            dependencies: State::NotLoaded,
            error: Some(failure.error.clone()),
            dependency_error: None,
            previous_error: None,
            dropped: true,
        }
    }
}

/// `alias` for the alias source, `path` for the rest.
fn via(path: &AssetPath) -> &'static str {
    match path.source() {
        bevy::asset::io::AssetSourceId::Name(name)
            if name.as_ref() == ename_asset_alias::ALIAS_SOURCE =>
        {
            "alias"
        }
        _ => "path",
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

#[cfg(test)]
mod tests {
    use super::error_text;

    #[test]
    fn error_text_drops_an_appended_backtrace() {
        let text = "Failed to load 'a': Expected comma\n   0: <x as y>::from\n             at a.rs:1:2\n  12: poll\nnote: Some \"noisy\" backtrace lines have been filtered out.\n";
        assert_eq!(error_text(&text), "Failed to load 'a': Expected comma");
    }

    #[test]
    fn error_text_keeps_numbered_lines_of_the_error_itself() {
        let text = "Model validation failed:\n  1: missing NORMAL\n  2: invalid material";
        assert_eq!(error_text(&text), text);
    }
}
