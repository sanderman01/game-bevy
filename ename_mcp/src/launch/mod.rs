//! The game launcher: build, start, stop, restart and report the workspace's editor binary.
//!
//! One detached supervisor process per run owns the build, the game process and its exit record,
//! so a caller that times out or dies changes nothing. Callers (the MCP tools and `cargo game`)
//! only read and edit a small state file and wait. See `AGENTDX_01_GAME_LIFECYCLE_PLAN.md` and
//! `docs/design/agent-tooling.md`.
//!
//! Nothing here kills by name or pattern. The supervisor kills only its own child's process group
//! or job; a game this launcher did not start is killed by pid only after its start time and
//! executable were checked again.

mod build_report;
pub mod cli;
mod control;
mod crash;
mod port;
mod process;
mod spawn;
mod state;
pub mod supervise;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Context as _, bail};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

pub use control::{Caller, LaunchRequest, Launcher};
pub use port::url as port_url;
pub use state::{Stamp, read_selected, selected_path};
pub use supervise::DEFAULT_GRACE;

/// The port of the `default` instance, matching the game's and the sidecar's default.
pub const DEFAULT_PORT: u16 = 15702;
/// Where other instances' port pairs start.
const FIRST_EXTRA_PORT: u16 = 15704;
pub const DEFAULT_INSTANCE: &str = "default";

/// The executable this process started from, resolved once: on Linux the path gains a
/// ` (deleted)` suffix as soon as cargo replaces the file.
static CURRENT_EXE: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Records this process's executable path. Call once at startup.
pub fn record_exe() {
    CURRENT_EXE.get_or_init(|| std::env::current_exe().ok());
}

fn current_exe() -> anyhow::Result<PathBuf> {
    CURRENT_EXE
        .get_or_init(|| std::env::current_exe().ok())
        .clone()
        .context("cannot find this executable to start a supervisor from")
}

/// The MCP session's owner token: the sidecar's parent process, the client that spawned it from
/// `.mcp.json`. It survives `/mcp reconnect`, differs between client sessions, and is shared
/// by an agent and the subagents on its connection.
pub fn mcp_owner() -> Option<String> {
    let parent = process::parent_of_current()?;
    Some(format!("mcp-{}-{}", parent.pid, parent.start_time))
}

/// The workspace the launcher runs the editor of: what one `cargo metadata` call says.
#[derive(Clone, Debug)]
pub struct Project {
    pub workspace_root: PathBuf,
    pub target_directory: PathBuf,
    pub config: LaunchConfig,
}

/// `[workspace.metadata.ename.launch]` in the workspace's root `Cargo.toml`.
#[derive(Clone, Debug, Deserialize)]
pub struct LaunchConfig {
    pub package: String,
    #[serde(default)]
    pub features: Vec<String>,
    /// The package's binary name, from its targets.
    #[serde(skip)]
    pub binary: String,
}

impl Project {
    /// Runs `cargo metadata` in `cwd`.
    pub async fn discover(cwd: &Path) -> anyhow::Result<Self> {
        let output = tokio::process::Command::new(cargo())
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .context("cannot run `cargo metadata`")?;
        if !output.status.success() {
            bail!(
                "`cargo metadata` failed in {}: {}",
                cwd.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Self::from_metadata(&output.stdout)
    }

    fn from_metadata(json: &[u8]) -> anyhow::Result<Self> {
        #[derive(Deserialize)]
        struct Metadata {
            workspace_root: PathBuf,
            target_directory: PathBuf,
            #[serde(default)]
            metadata: Option<serde_json::Value>,
            packages: Vec<Package>,
        }
        #[derive(Deserialize)]
        struct Package {
            name: String,
            targets: Vec<Target>,
        }
        #[derive(Deserialize)]
        struct Target {
            name: String,
            kind: Vec<String>,
        }

        let metadata: Metadata = serde_json::from_slice(json)
            .context("`cargo metadata` printed something unexpected")?;
        let table = metadata
            .metadata
            .as_ref()
            .and_then(|value| value.pointer("/ename/launch"))
            .cloned()
            .with_context(|| {
                format!(
                    "{}/Cargo.toml has no `[workspace.metadata.ename.launch]` table. Add one \
                     naming the editor package: `package = \"<editor package>\"` and \
                     `features = [...]`.",
                    metadata.workspace_root.display()
                )
            })?;
        let mut config: LaunchConfig = serde_json::from_value(table)
            .context("`[workspace.metadata.ename.launch]` needs `package` and `features`")?;
        let package = metadata
            .packages
            .iter()
            .find(|package| package.name == config.package)
            .with_context(|| {
                format!(
                    "`[workspace.metadata.ename.launch]` names package `{}`, which is not in the \
                     workspace",
                    config.package
                )
            })?;
        config.binary = package
            .targets
            .iter()
            .find(|target| target.kind.iter().any(|kind| kind == "bin"))
            .map(|target| target.name.clone())
            .with_context(|| format!("package `{}` has no binary", config.package))?;

        Ok(Self {
            workspace_root: metadata.workspace_root,
            target_directory: metadata.target_directory,
            config,
        })
    }

    /// `<target>/ename-launch/<workspace>`. The workspace key keeps two worktrees sharing one
    /// `CARGO_TARGET_DIR` apart.
    pub fn state_root(&self) -> PathBuf {
        self.target_directory
            .join("ename-launch")
            .join(workspace_key(&self.workspace_root))
    }

    /// The binary cargo builds for the editor, in the dev profile.
    fn binary_path(&self) -> PathBuf {
        self.target_directory.join("debug").join(format!(
            "{}{}",
            self.config.binary,
            std::env::consts::EXE_SUFFIX
        ))
    }
}

/// The workspace directory's name plus a hash of its canonical path.
fn workspace_key(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let name = canonical
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".to_owned());
    // FNV-1a rather than `DefaultHasher`, whose output may change between Rust releases and would
    // then lose track of a running game.
    let hash = canonical
        .to_string_lossy()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("{name}-{:08x}", hash as u32)
}

/// The cargo that runs this process, when there is one, so `cargo +toolchain game` builds with
/// the same toolchain.
fn cargo() -> std::ffi::OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// What `game_start` and `game_restart` take, and what a restart reuses from the last launch.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LaunchParams {
    /// Map of extra environment variables for the game, e.g. {"AIRSHIP_DIAGNOSTICS": "1"}.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Extra cargo features for the editor package, e.g. ["trace"], added to the workspace's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    /// Window size in physical pixels, [width, height]. Passed as ENAME_WINDOW_SIZE.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_size: Option<[u32; 2]>,
    /// Render size, [width, height]. Passed as ENAME_RENDER_SIZE.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_size: Option<[u32; 2]>,
    /// Wait for the game to settle after it is ready. Not available yet; ignored with a note.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub settle: bool,
}

impl LaunchParams {
    /// `self` with the fields `update` gives replaced.
    pub fn merged(&self, update: LaunchUpdate) -> Self {
        Self {
            env: update.env.unwrap_or_else(|| self.env.clone()),
            features: update.features.unwrap_or_else(|| self.features.clone()),
            window_size: update.window_size.or(self.window_size),
            render_size: update.render_size.or(self.render_size),
            settle: update.settle.unwrap_or(self.settle),
        }
    }
}

/// Launch params as a caller gives them: absent fields keep the previous launch's.
#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct LaunchUpdate {
    /// Map of extra environment variables for the game, e.g. {"AIRSHIP_DIAGNOSTICS": "1"}.
    /// game_restart keeps the previous launch's when omitted.
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    /// Extra cargo features for the editor package, e.g. ["trace"].
    #[serde(default)]
    pub features: Option<Vec<String>>,
    /// Window size in physical pixels, [width, height].
    #[serde(default)]
    pub window_size: Option<[u32; 2]>,
    /// Render size, [width, height].
    #[serde(default)]
    pub render_size: Option<[u32; 2]>,
    /// Wait for streaming to finish after ready. Defaults to false. Not available yet.
    #[serde(default)]
    pub settle: Option<bool>,
}

/// Where a run is, as every `game_*` tool and `cargo game` verb reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GameState {
    #[default]
    Stopped,
    Building,
    BuildFailed,
    Starting,
    Ready,
    Exited,
    External,
    PortInUse,
}

impl GameState {
    /// Still on its way to ready or a failure, so worth waiting on.
    pub fn is_transient(self) -> bool {
        matches!(self, Self::Building | Self::Starting)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Building => "building",
            Self::BuildFailed => "build_failed",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Exited => "exited",
            Self::External => "external",
            Self::PortInUse => "port_in_use",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LaunchedBy {
    Launcher,
    External,
}

/// The result of every lifecycle operation.
#[derive(Clone, Debug, Default, Serialize, schemars::JsonSchema)]
pub struct Status {
    pub instance: String,
    pub state: GameState,
    pub port: u16,
    /// The game's run id (`game.pid.get`), present only when the game answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<String>,
    /// The game's OS process id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_pid: Option<u32>,
    pub package: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launched_by: Option<LaunchedBy>,
    /// Which run of this instance, increasing per launch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildSummary>,
    /// From spawning the game to its first ready answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup_seconds: Option<f64>,
    /// The game's stdout and stderr, plain text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// The executable on disk is newer than the running game.
    pub stale_binary: bool,
    /// Who started it: an owner token, or null for the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub owned_by_caller: bool,
    /// Unix seconds when this run was launched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_count: Option<usize>,
    /// How the game exited. Null after stopping a game this launcher did not start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit: Option<crash::ExitInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crash: Option<crash::Crash>,
    /// The process holding the port, for `port_in_use` and `external`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<port::Holder>,
    /// Whether a stop ended in the game's own exit rather than a kill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graceful: Option<bool>,
    /// A restart's new run, while it builds next to the running one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<Box<PendingStatus>>,
    /// The caller's running instances, which it must stop before it finishes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yours: Option<Vec<String>>,
    /// The caller's owner token, to hand to a separate-process agent as ENAME_GAME_OWNER.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Something about the sidecar itself that needs acting on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// The game is gone but its exit is not recorded yet: worth waiting on.
    #[serde(skip)]
    pub recording_exit: bool,
}

impl Status {
    fn add_note(&mut self, note: impl AsRef<str>) {
        let note = note.as_ref();
        self.note = Some(match self.note.take() {
            Some(existing) => format!("{existing} {note}"),
            None => note.to_owned(),
        });
    }

    /// A launch that ended in a failure the caller must act on.
    pub fn is_failure(&self) -> bool {
        match self.state {
            GameState::BuildFailed | GameState::PortInUse => true,
            GameState::Exited => self.crash.is_some(),
            _ => self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.state == GameState::BuildFailed),
        }
    }

    /// One line for a human or the text of an error.
    pub fn summary(&self) -> String {
        let mut line = format!("{} {}", self.instance, self.state.as_str());
        if let Some(os_pid) = self.os_pid {
            line.push_str(&format!(" (os pid {os_pid}, port {})", self.port));
        } else {
            line.push_str(&format!(" (port {})", self.port));
        }
        if let Some(pending) = &self.pending {
            line.push_str(&format!("; restart {}", pending.state.as_str()));
        }
        if let Some(count) = self.error_count {
            line.push_str(&format!(": {count} build errors"));
        }
        if let Some(crash) = &self.crash {
            line.push_str(&format!(": {}", crash.cause));
        } else if let Some(exit) = &self.exit {
            line.push_str(&format!(": exit {}", exit.describe()));
        }
        if let Some(holder) = &self.holder {
            line.push_str(&format!(": held by {}", holder.describe()));
        }
        if let Some(note) = &self.note {
            line.push_str(&format!(". {note}"));
        }
        let errors = self.errors.iter().chain(
            self.pending
                .as_ref()
                .map(|pending| pending.errors.iter())
                .into_iter()
                .flatten(),
        );
        for error in errors {
            line.push_str(&format!("\n  {error}"));
        }
        if let Some(crash) = &self.crash {
            for tail in &crash.log_tail {
                line.push_str(&format!("\n  | {tail}"));
            }
        }
        line
    }
}

#[derive(Clone, Debug, Default, Serialize, schemars::JsonSchema)]
pub struct BuildSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    pub warning_count: usize,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct PendingStatus {
    pub state: GameState,
    pub generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_count: Option<usize>,
}

/// Seconds since the Unix epoch.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_launch_table_and_the_binary_name() {
        let json = br#"{
            "workspace_root": "/w", "target_directory": "/w/target",
            "metadata": {"ename": {"launch": {"package": "ed", "features": ["dev"]}}},
            "packages": [{"name": "ed", "targets": [{"name": "ed_bin", "kind": ["bin"]}]}]
        }"#;
        let project = Project::from_metadata(json).unwrap();
        assert_eq!(project.config.package, "ed");
        assert_eq!(project.config.binary, "ed_bin");
        assert_eq!(project.config.features, ["dev"]);
    }

    #[test]
    fn a_missing_table_names_the_table() {
        let json = br#"{"workspace_root": "/w", "target_directory": "/w/target",
            "metadata": null, "packages": []}"#;
        let error = Project::from_metadata(json).unwrap_err().to_string();
        assert!(
            error.contains("[workspace.metadata.ename.launch]"),
            "{error}"
        );
    }

    /// Two worktrees of one repo sharing a `CARGO_TARGET_DIR` must not share state.
    #[test]
    fn worktrees_with_one_name_get_separate_keys() {
        let a = workspace_key(Path::new("/nonexistent/one/game"));
        let b = workspace_key(Path::new("/nonexistent/two/game"));
        assert!(a.starts_with("game-") && b.starts_with("game-"));
        assert_ne!(a, b);
        assert_eq!(a, workspace_key(Path::new("/nonexistent/one/game")));
    }

    #[test]
    fn restart_params_replace_only_what_is_given() {
        let first = LaunchParams {
            env: [("A".to_owned(), "1".to_owned())].into(),
            window_size: Some([800, 600]),
            ..Default::default()
        };
        let second = first.merged(LaunchUpdate {
            features: Some(vec!["trace".to_owned()]),
            ..Default::default()
        });
        assert_eq!(second.env, first.env);
        assert_eq!(second.window_size, Some([800, 600]));
        assert_eq!(second.features, ["trace"]);
    }
}
