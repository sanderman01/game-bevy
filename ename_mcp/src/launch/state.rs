//! The state directory: one per instance, plus each owner's selected instance.
//!
//! `state.json` is the only file more than one process edits. Edits are read-modify-write under
//! the instance's `lock` file, taken with a bounded `try_lock` and never held across a build or
//! a wait. Every write is a temp file renamed over the old one, so readers need no lock.

use std::{
    fs::File,
    io::Write as _,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{LaunchParams, Project, process::ProcessRef};

/// How long an edit waits for another process's edit to finish.
const LOCK_WAIT: Duration = Duration::from_secs(2);
/// How long a run without a recorded supervisor counts as alive: the spawner records it right
/// after the spawn.
const SPAWN_GRACE_SECONDS: u64 = 10;

/// One instance's `state.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub instance: String,
    pub workspace_root: PathBuf,
    /// The machine's boot time; a state from an earlier boot describes no live process.
    pub boot_time: u64,
    pub port: u16,
    pub render_port: u16,
    /// The owner token of whoever started the current run; `None` is the user.
    pub owner: Option<String>,
    pub last_generation: u64,
    pub current: Option<Run>,
    /// A restart's new run, while it builds next to `current`.
    pub pending: Option<Run>,
}

/// One launch of the game.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub generation: u64,
    pub phase: Phase,
    pub supervisor: Option<ProcessRef>,
    pub game: Option<ProcessRef>,
    pub stop_requested: Option<StopRequest>,
    pub launch: LaunchParams,
    /// Unix seconds when the run was created.
    pub started_at: u64,
    pub build_seconds: Option<f64>,
    pub warning_count: Option<usize>,
    /// Unix seconds, fractional, when the game process was spawned and when it was first ready.
    pub spawned_at: Option<f64>,
    pub ready_at: Option<f64>,
    /// Why the supervisor gave up on this run, when it did.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Building,
    BuildFailed,
    /// Stopped before a game was spawned.
    Stopped,
    Starting,
    Ready,
    Exited,
}

impl Phase {
    /// The supervisor is done with this run.
    pub fn is_final(self) -> bool {
        matches!(self, Self::BuildFailed | Self::Stopped | Self::Exited)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct StopRequest {
    pub grace_seconds: f64,
}

impl Run {
    pub fn new(generation: u64, launch: LaunchParams) -> Self {
        Self {
            generation,
            phase: Phase::Building,
            supervisor: None,
            game: None,
            stop_requested: None,
            launch,
            started_at: super::now(),
            build_seconds: None,
            warning_count: None,
            spawned_at: None,
            ready_at: None,
            error: None,
        }
    }

    /// Whether a supervisor still works on this run.
    pub fn supervisor_alive(&self) -> bool {
        if self.phase.is_final() {
            return false;
        }
        match self.supervisor {
            Some(supervisor) => supervisor.is_alive(),
            None => super::now().saturating_sub(self.started_at) < SPAWN_GRACE_SECONDS,
        }
    }

    pub fn game_alive(&self) -> bool {
        self.game.is_some_and(|game| game.is_alive())
    }
}

impl State {
    /// Whether this state was written in this boot. Process refs from another boot mean nothing.
    /// A few seconds of slack, because boot time is derived from uptime on some platforms.
    pub fn is_this_boot(&self) -> bool {
        self.boot_time.abs_diff(super::process::boot_time()) <= 5
    }

    pub fn run(&self, generation: u64) -> Option<&Run> {
        [self.current.as_ref(), self.pending.as_ref()]
            .into_iter()
            .flatten()
            .find(|run| run.generation == generation)
    }

    pub fn run_mut(&mut self, generation: u64) -> Option<&mut Run> {
        [self.current.as_mut(), self.pending.as_mut()]
            .into_iter()
            .flatten()
            .find(|run| run.generation == generation)
    }

    /// Whether anything of this instance still runs: a supervisor or a game.
    pub fn is_live(&self) -> bool {
        self.is_this_boot()
            && [self.current.as_ref(), self.pending.as_ref()]
                .into_iter()
                .flatten()
                .any(|run| run.supervisor_alive() || run.game_alive())
    }
}

/// One instance's directory.
#[derive(Clone, Debug)]
pub struct Instance {
    pub name: String,
    pub dir: PathBuf,
}

impl Instance {
    pub fn new(project: &Project, name: &str) -> anyhow::Result<Self> {
        let valid = !name.is_empty()
            && name.len() <= 32
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && name != SELECTED_DIR;
        if !valid {
            bail!("instance names are 1 to 32 letters, digits, `-` or `_`, not `{name}`");
        }
        Ok(Self {
            name: name.to_owned(),
            dir: project.state_root().join(name),
        })
    }

    /// Every instance that has a state directory.
    pub fn all(project: &Project) -> Vec<Self> {
        let Ok(entries) = std::fs::read_dir(project.state_root()) else {
            return Vec::new();
        };
        let mut instances: Vec<Self> = entries
            .flatten()
            .filter(|entry| entry.path().join(STATE_FILE).is_file())
            .filter_map(|entry| Self::new(project, &entry.file_name().to_string_lossy()).ok())
            .collect();
        instances.sort_by(|a, b| a.name.cmp(&b.name));
        instances
    }

    pub fn path(&self, file: &str) -> PathBuf {
        self.dir.join(file)
    }

    pub fn read(&self) -> anyhow::Result<Option<State>> {
        read_json(&self.path(STATE_FILE))
    }

    /// Reads, edits and writes `state.json` under the instance lock. `edit` sees `None` when
    /// there is no state yet; the file is written only when `edit` leaves `Some`.
    pub fn update<R>(
        &self,
        edit: impl FnOnce(&mut Option<State>) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("cannot create {}", self.dir.display()))?;
        let _lock = lock(&self.path(LOCK_FILE))?;
        let mut state = self.read()?;
        let result = edit(&mut state)?;
        if let Some(state) = &state {
            write_json(&self.path(STATE_FILE), state)?;
        }
        Ok(result)
    }
}

pub const STATE_FILE: &str = "state.json";
pub const EXIT_FILE: &str = "exit.json";
pub const BUILD_JSON: &str = "build.jsonl";
pub const BUILD_LOG: &str = "build.log";
pub const GAME_LOG: &str = "game.log";
pub const GAME_PREV_LOG: &str = "game.prev.log";
pub const SUPERVISOR_LOG: &str = "supervise.log";
const LOCK_FILE: &str = "lock";
const SELECTED_DIR: &str = "selected";

/// Holds the lock until dropped.
fn lock(path: &Path) -> anyhow::Result<File> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                bail!("{} stayed locked for {LOCK_WAIT:?}", path.display())
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("cannot lock {}", path.display()));
            }
        }
    }
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("{} is not valid", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Writes a temp file and renames it over `path`, so a reader sees the old or the new file.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    // Unique per write, so two threads of one process never share a temp file.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file =
        File::create(&temp).with_context(|| format!("cannot write {}", temp.display()))?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    drop(file);
    // Windows refuses to replace a file another process has open for a moment, such as a reader
    // in `read_json`: access denied, or a sharing violation (32). About a second of retries.
    const SHARING_VIOLATION: i32 = 32;
    let mut attempts = 0;
    loop {
        match std::fs::rename(&temp, path) {
            Ok(()) => return Ok(()),
            Err(error)
                if (error.kind() == std::io::ErrorKind::PermissionDenied
                    || (cfg!(windows) && error.raw_os_error() == Some(SHARING_VIOLATION)))
                    && attempts < 50 =>
            {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temp);
                return Err(error).with_context(|| format!("cannot replace {}", path.display()));
            }
        }
    }
}

/// Whether `token` may be an owner token: it becomes a file name.
pub fn is_valid_owner(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 64
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The file naming the instance an owner's sessions talk to. `None` is the user.
pub fn selected_path(project: &Project, owner: Option<&str>) -> PathBuf {
    let name = match owner {
        Some(owner) if is_valid_owner(owner) => format!("owner-{owner}"),
        // Never a path outside the directory, whatever a caller passes.
        Some(_) => "owner-invalid".to_owned(),
        None => "user".to_owned(),
    };
    project.state_root().join(SELECTED_DIR).join(name)
}

/// What a selection depends on: the selection file and the selected instance's state, so a
/// change to either is noticed with two `stat`s and one small read.
pub fn selection_stamp(project: &Project, owner: Option<&str>) -> (Option<Stamp>, Option<Stamp>) {
    let path = selected_path(project, owner);
    let instance = read_json::<String>(&path)
        .ok()
        .flatten()
        .and_then(|name| Instance::new(project, &name).ok());
    (
        Stamp::read(&path),
        instance.and_then(|instance| Stamp::read(&instance.path(STATE_FILE))),
    )
}

pub fn write_selected(
    project: &Project,
    owner: Option<&str>,
    instance: &str,
) -> anyhow::Result<()> {
    let path = selected_path(project, owner);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_json(&path, &instance)
}

/// The instance an owner selected, and its port, when that instance is live.
pub fn read_selected(project: &Project, owner: Option<&str>) -> Option<(String, u16)> {
    let name: String = read_json(&selected_path(project, owner)).ok()??;
    let state = Instance::new(project, &name).ok()?.read().ok()??;
    state.is_live().then_some((name, state.port))
}

/// Length and modification time, the two facts every platform reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    pub fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}
