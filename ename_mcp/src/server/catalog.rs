//! The game's commands as MCP tools, and telling the client when they change.
//!
//! The catalogue is whatever `game.command.list` last answered, with the run that answered it. A
//! watcher re-lists every [`WATCH_INTERVAL`], so a game that starts after the session, or restarts
//! with different commands, shows up without the agent asking. Refreshes are serialized, so an
//! older answer never overwrites a newer one, and the client is told only when the emitted tools
//! actually changed. A game that goes down leaves the last catalogue in place: its tools then fail
//! like every other tool, and nothing churns on each restart.
//!
//! Two MCP lifecycles carry the notification. A legacy client (`initialize`, up to 2025-11-25)
//! sends `notifications/initialized`, and the change goes to its peer. A 2026-07-28 client never
//! does; it opens `subscriptions/listen`, and the change goes to every open subscription.

use std::{
    collections::{BTreeMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use rmcp::{
    RoleServer,
    model::{JsonObject, Tool, ToolAnnotations},
    service::{Peer, SubscriptionSendError, SubscriptionSink},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::brp::{BrpClient, Endpoint};

/// How often the watcher re-lists the game's commands.
pub const WATCH_INTERVAL: Duration = Duration::from_secs(2);
/// How long one listing may take before the watcher gives up on it until the next tick.
const LIST_TIMEOUT: Duration = Duration::from_secs(1);

/// One command as `game.command.list` describes it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CommandInfo {
    pub name: String,
    pub description: String,
    /// "instant" or "job".
    pub kind: String,
    pub scope: String,
    #[serde(default)]
    pub read_only: bool,
    pub params: Value,
    #[serde(default)]
    pub result: Value,
}

impl CommandInfo {
    pub fn is_job(&self) -> bool {
        self.kind == "job"
    }

    /// The MCP tool name: the dot replaced, because Claude's tool names allow none.
    pub fn tool_name(&self) -> String {
        tool_name(&self.name)
    }
}

pub fn tool_name(command: &str) -> String {
    command.replace('.', "_")
}

/// What `game.command.list` answers.
#[derive(Debug, Deserialize)]
pub struct Listing {
    pub run: String,
    pub commands: Vec<CommandInfo>,
}

/// A command offered as its own tool.
#[derive(Clone, Debug)]
pub struct DynamicTool {
    pub command: CommandInfo,
    pub tool: Tool,
}

#[derive(Default)]
struct State {
    run: Option<String>,
    commands: Vec<CommandInfo>,
    /// Tool name to tool. Commands a static tool shadows are not here.
    tools: BTreeMap<String, DynamicTool>,
    fingerprint: u64,
}

/// The game's commands as last listed, and who to tell when they change.
#[derive(Default)]
pub struct Catalog {
    state: RwLock<State>,
    /// Held from the request to publication, so refreshes publish in the order they started.
    refreshing: tokio::sync::Mutex<()>,
    /// Static tool names, which a command may not take over.
    static_tools: OnceLock<HashSet<String>>,
    legacy_peer: OnceLock<Peer<RoleServer>>,
    /// Open subscriptions by a stable id, so overlapping notifies and a `listen` ending remove
    /// exactly the one they mean.
    subscriptions: Mutex<BTreeMap<u64, SubscriptionSink>>,
    next_subscription: AtomicU64,
    watching: OnceLock<()>,
}

impl Catalog {
    pub fn new(static_tools: impl IntoIterator<Item = String>) -> Self {
        let catalog = Self::default();
        let _ = catalog.static_tools.set(static_tools.into_iter().collect());
        catalog
    }

    pub fn tools(&self) -> Vec<Tool> {
        let state = self.state.read().unwrap();
        state.tools.values().map(|tool| tool.tool.clone()).collect()
    }

    pub fn get(&self, tool_name: &str) -> Option<DynamicTool> {
        self.state.read().unwrap().tools.get(tool_name).cloned()
    }

    /// A tool and the run its catalogue came from, read together, so a refresh in between cannot
    /// pair a command listed by one run with another run's id.
    pub fn lookup(&self, tool_name: &str) -> Option<(DynamicTool, String)> {
        let state = self.state.read().unwrap();
        let tool = state.tools.get(tool_name)?.clone();
        Some((tool, state.run.clone()?))
    }

    pub fn is_loaded(&self) -> bool {
        self.state.read().unwrap().run.is_some()
    }

    /// Whether `command`'s tool name is taken by a static tool, so the command is reachable only
    /// through `command_call`.
    pub fn is_shadowed(&self, command: &str) -> bool {
        self.static_tools
            .get()
            .is_some_and(|names| names.contains(&tool_name(command)))
    }

    /// Rewrites the dotted command names a game-written hint mentions in backticks into the names
    /// the agent can call: the tool, or `command_call` when a static tool shadows it.
    pub fn rewrite(&self, text: &str) -> String {
        let state = self.state.read().unwrap();
        let mut text = text.to_owned();
        for command in &state.commands {
            let quoted = format!("`{}`", command.name);
            if !text.contains(&quoted) {
                continue;
            }
            let replacement = if self.is_shadowed(&command.name) {
                format!("`command_call` with command `{}`", command.name)
            } else {
                format!("`{}`", command.tool_name())
            };
            text = text.replace(&quoted, &replacement);
        }
        text
    }

    /// Lists the game's commands and publishes them. Returns whether the emitted tools changed.
    /// A game that does not answer leaves the catalogue as it was.
    pub async fn refresh(&self, brp: &BrpClient) -> anyhow::Result<bool> {
        let _order = self.refreshing.lock().await;
        let listing: Listing =
            tokio::time::timeout(LIST_TIMEOUT, brp.call("game.command.list", json!({})))
                .await
                .map_err(|_| anyhow::anyhow!("listing the game's commands timed out"))??;
        Ok(self.publish(listing))
    }

    /// Replaces the catalogue with `listing`. Returns whether the emitted tools changed.
    pub fn publish(&self, listing: Listing) -> bool {
        let static_tools = self.static_tools.get();
        let tools: BTreeMap<String, DynamicTool> = listing
            .commands
            .iter()
            .filter(|command| {
                static_tools.is_none_or(|names| !names.contains(&command.tool_name()))
            })
            .map(|command| {
                (
                    command.tool_name(),
                    DynamicTool {
                        command: command.clone(),
                        tool: to_tool(command),
                    },
                )
            })
            .collect();
        for command in &listing.commands {
            if static_tools.is_some_and(|names| names.contains(&command.tool_name())) {
                tracing::warn!(
                    command = command.name,
                    "a static tool has this command's tool name; reach it with command_call"
                );
            }
        }
        let fingerprint = fingerprint(tools.values().map(|tool| &tool.tool));
        let mut state = self.state.write().unwrap();
        let changed = state.fingerprint != fingerprint;
        *state = State {
            run: Some(listing.run),
            commands: listing.commands,
            tools,
            fingerprint,
        };
        changed
    }

    /// A legacy client finished initializing: tell it about changes through its peer.
    pub fn set_legacy_peer(&self, peer: Peer<RoleServer>) {
        let _ = self.legacy_peer.set(peer);
    }

    /// A 2026-07-28 client opened a subscription that accepted `tools_list_changed`. Returns the
    /// id to remove it with when it ends.
    pub fn add_subscription(&self, sink: SubscriptionSink) -> u64 {
        let id = self.next_subscription.fetch_add(1, Ordering::Relaxed);
        self.subscriptions.lock().unwrap().insert(id, sink);
        id
    }

    pub fn remove_subscription(&self, id: u64) {
        self.subscriptions.lock().unwrap().remove(&id);
    }

    #[cfg(test)]
    pub fn subscription_count(&self) -> usize {
        self.subscriptions.lock().unwrap().len()
    }

    /// Tells every client channel that the tools changed, dropping closed subscriptions.
    pub async fn notify(&self) {
        if let Some(peer) = self.legacy_peer.get()
            && let Err(err) = peer.notify_tool_list_changed().await
        {
            tracing::warn!("failed to send tools/list_changed: {err}");
        }
        let sinks = self.subscriptions.lock().unwrap().clone();
        for (id, sink) in sinks {
            match sink.notify_tool_list_changed().await {
                Ok(()) => {}
                Err(SubscriptionSendError::SubscriptionClosed) => self.remove_subscription(id),
                Err(err) => tracing::warn!("failed to send tools/list_changed: {err}"),
            }
        }
    }

    /// Refreshes, and notifies when the tools changed.
    pub async fn refresh_and_notify(&self, brp: &BrpClient) {
        match self.refresh(brp).await {
            Ok(true) => self.notify().await,
            Ok(false) => {}
            Err(err) => tracing::debug!("the game's commands are not available: {err:#}"),
        }
    }

    /// Starts the watcher the first time it is asked, whichever request comes first. Needs a
    /// Tokio runtime, which every request handler runs on.
    /// Each tick lists the game the session currently selects.
    pub fn watch(self: &Arc<Self>, endpoint: &Arc<Endpoint>) {
        if self.watching.set(()).is_err() {
            return;
        }
        let catalog = Arc::clone(self);
        let endpoint = Arc::clone(endpoint);
        tokio::spawn(async move {
            loop {
                catalog.refresh_and_notify(&endpoint.snapshot()).await;
                tokio::time::sleep(WATCH_INTERVAL).await;
            }
        });
    }
}

/// One command as an MCP tool. The params schema goes out as the game wrote it, `$defs` and all;
/// a job's gets `wait_seconds` added and nothing else.
fn to_tool(command: &CommandInfo) -> Tool {
    let mut schema: JsonObject = command.params.as_object().cloned().unwrap_or_default();
    let mut description = command.description.clone();
    if command.is_job() {
        let properties = schema
            .entry("properties")
            .or_insert_with(|| json!({}))
            .as_object_mut();
        if let Some(properties) = properties {
            properties.insert(
                "wait_seconds".into(),
                json!({
                    "type": "number",
                    "description": "How long to wait for the result, 0 to 50 seconds. Defaults to 50.",
                }),
            );
        }
        description.push_str(
            " Waits up to `wait_seconds` (default and maximum 50) for the result; a longer run \
             returns a job id for job_wait.",
        );
    }
    Tool::new(command.tool_name(), description, Arc::new(schema))
        .annotate(ToolAnnotations::new().read_only(command.read_only))
}

/// A hash of every emitted tool, whole, so a change in any of it (schema, description,
/// annotations) notifies.
fn fingerprint<'a>(tools: impl Iterator<Item = &'a Tool>) -> u64 {
    let mut hasher = DefaultHasher::new();
    for tool in tools {
        serde_json::to_string(tool)
            .unwrap_or_default()
            .hash(&mut hasher);
    }
    hasher.finish()
}
