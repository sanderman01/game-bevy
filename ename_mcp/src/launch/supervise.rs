//! The `supervise` role: one detached process per run that builds the editor, swaps it in for the
//! previous run, runs it, stops it on request and records how it exited.
//!
//! It owns everything that must not depend on a caller staying alive. Callers only set
//! `stop_requested` in `state.json` and read what this writes.

use std::{
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail};
use process_wrap::tokio::ChildWrapper;
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;

use super::{
    Project,
    build_report::BuildReport,
    cargo,
    crash::ExitInfo,
    now_f64, port,
    process::ProcessRef,
    spawn,
    state::{
        BUILD_JSON, BUILD_LOG, EXIT_FILE, GAME_LOG, GAME_PREV_LOG, Instance, Phase, Run, State,
        StopRequest, write_json,
    },
};

const POLL: Duration = Duration::from_millis(200);
/// The grace a stop gets when the request names none.
pub const DEFAULT_GRACE: f64 = 5.0;
/// How long a restart waits for the previous run's supervisor to record its exit, past its
/// grace.
const STOP_MARGIN: Duration = Duration::from_secs(10);

/// `ename_mcp supervise <instance-dir> <generation>`.
pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let [dir, generation] = args else {
        bail!("usage: ename_mcp supervise <instance-dir> <generation>");
    };
    let dir = PathBuf::from(dir);
    let generation: u64 = generation.parse().context("generation is not a number")?;
    let instance = Instance {
        name: dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        dir,
    };
    let supervisor = Supervisor::new(instance, generation).await?;
    let result = supervisor.supervise().await;
    if let Err(error) = &result {
        tracing::error!("supervisor failed: {error:#}");
        // A run nobody finishes would read as building forever.
        let _ = supervisor.set_phase(Phase::Exited);
    }
    result
}

struct Supervisor {
    instance: Instance,
    generation: u64,
    project: Project,
    port: u16,
}

impl Supervisor {
    async fn new(instance: Instance, generation: u64) -> anyhow::Result<Self> {
        let me = ProcessRef::current();
        let state = instance.update(|state| {
            let state = state.as_mut().context("no state.json")?;
            let run = state
                .run_mut(generation)
                .context("this run is no longer in state.json")?;
            run.supervisor = me;
            Ok(state.clone())
        })?;
        let project = Project::discover(&state.workspace_root).await?;
        remove_old_copies(&instance.dir, generation);
        Ok(Self {
            instance,
            generation,
            project,
            port: state.port,
        })
    }

    async fn supervise(&self) -> anyhow::Result<()> {
        let is_restart =
            self.read()?.pending.as_ref().map(|run| run.generation) == Some(self.generation);
        // Windows will not let the build replace the running game's executable: stop it first.
        if is_restart && cfg!(windows) {
            self.stop_previous().await?;
        }

        let build_started = Instant::now();
        let built = self.build().await?;
        let seconds = build_started.elapsed().as_secs_f64();
        let report = BuildReport::read(&self.path(BUILD_JSON), &self.path(BUILD_LOG));
        self.edit_run(|run| {
            run.build_seconds = Some(seconds);
            run.warning_count = Some(report.warning_count);
        })?;
        match built {
            Build::Stopped => return self.set_phase(Phase::Stopped),
            Build::Failed => return self.set_phase(Phase::BuildFailed),
            Build::Done => {}
        }
        if self.stop_requested()?.is_some() {
            return self.set_phase(Phase::Stopped);
        }

        if is_restart {
            self.stop_previous().await?;
        }
        if !self.promote()? {
            return self.set_phase(Phase::Stopped);
        }
        self.run_game().await
    }

    fn read(&self) -> anyhow::Result<State> {
        self.instance.read()?.context("state.json disappeared")
    }

    /// The stop request on this run. A run that is no longer in the state was replaced by a
    /// newer restart, which counts as a request.
    fn stop_requested(&self) -> anyhow::Result<Option<StopRequest>> {
        let state = self.read()?;
        Ok(match state.run(self.generation) {
            Some(run) => run.stop_requested,
            None => Some(StopRequest {
                grace_seconds: DEFAULT_GRACE,
            }),
        })
    }

    fn edit_run(&self, edit: impl FnOnce(&mut Run)) -> anyhow::Result<()> {
        self.instance.update(|state| {
            if let Some(run) = state.as_mut().and_then(|s| s.run_mut(self.generation)) {
                edit(run);
            }
            Ok(())
        })
    }

    fn set_phase(&self, phase: Phase) -> anyhow::Result<()> {
        tracing::info!(?phase, "run {}", self.generation);
        self.edit_run(|run| run.phase = phase)
    }

    fn path(&self, file: &str) -> PathBuf {
        self.instance.path(file)
    }

    async fn build(&self) -> anyhow::Result<Build> {
        let run = self
            .read()?
            .run(self.generation)
            .cloned()
            .context("this run is no longer in state.json")?;
        let mut command = Command::new(cargo());
        command
            .args(["build", "-p", &self.project.config.package])
            .args(features(&self.project, &run))
            .arg("--message-format=json-diagnostic-short")
            .current_dir(&self.project.workspace_root)
            .env("CARGO_TERM_COLOR", "never")
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(self.path(BUILD_JSON))?)
            .stderr(std::fs::File::create(self.path(BUILD_LOG))?);
        let mut child = spawn::grouped(command).context("cannot start cargo build")?;
        tracing::info!(
            "run {}: building {}",
            self.generation,
            self.project.config.package
        );
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(if status.success() {
                    Build::Done
                } else {
                    Build::Failed
                });
            }
            if self.stop_requested()?.is_some() {
                Box::into_pin(child.kill()).await?;
                return Ok(Build::Stopped);
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Stops the run this restart replaces, through its own supervisor when it has one.
    async fn stop_previous(&self) -> anyhow::Result<()> {
        let state = self.read()?;
        let Some(previous) = state.current.clone() else {
            return Ok(());
        };
        if previous.generation == self.generation || previous.phase.is_final() {
            return Ok(());
        }
        let grace = previous
            .stop_requested
            .map_or(DEFAULT_GRACE, |request| request.grace_seconds);
        self.instance.update(|state| {
            if let Some(run) = state.as_mut().and_then(|s| s.run_mut(previous.generation))
                && run.stop_requested.is_none()
            {
                run.stop_requested = Some(StopRequest {
                    grace_seconds: grace,
                });
            }
            Ok(())
        })?;
        let deadline = Instant::now() + Duration::from_secs_f64(grace) + STOP_MARGIN;
        while Instant::now() < deadline {
            let Some(run) = self.read()?.run(previous.generation).cloned() else {
                return Ok(());
            };
            if run.phase.is_final() {
                return Ok(());
            }
            if !run.supervisor_alive() {
                // Its supervisor died: the game is ours to stop by pid.
                if let Some(game) = run.game.filter(|game| game.is_alive()) {
                    let binary = self.project.binary_path();
                    game.kill_checked(|exe| {
                        super::control::is_game_binary(&self.project, exe, &binary)
                    })?;
                }
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
        }
        tracing::warn!("run {} did not stop in time", previous.generation);
        Ok(())
    }

    /// Makes this run `current`. Returns false when it was replaced in the meantime.
    fn promote(&self) -> anyhow::Result<bool> {
        self.instance.update(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if state.pending.as_ref().map(|run| run.generation) == Some(self.generation) {
                state.current = state.pending.take();
            }
            let Some(run) = state
                .current
                .as_mut()
                .filter(|run| run.generation == self.generation)
            else {
                return Ok(false);
            };
            run.phase = Phase::Starting;
            Ok(true)
        })
    }

    async fn run_game(&self) -> anyhow::Result<()> {
        let run = self
            .read()?
            .run(self.generation)
            .cloned()
            .context("this run is no longer in state.json")?;
        let log_path = self.path(GAME_LOG);
        if log_path.exists() {
            let _ = std::fs::rename(&log_path, self.path(GAME_PREV_LOG));
        }
        let log = std::fs::File::create(&log_path)?;

        let mut command = Command::new(cargo());
        command
            .args(["run", "-q", "-p", &self.project.config.package])
            .args(features(&self.project, &run))
            .current_dir(&self.project.workspace_root)
            .env("NO_COLOR", "1")
            .env("CARGO_TERM_COLOR", "never")
            .env("RUST_BACKTRACE", "1")
            .env("ENAME_BRP_PORT", self.port.to_string())
            .env("ENAME_INSTANCE", &self.instance.name)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        if let Some([width, height]) = run.launch.window_size {
            command.env("ENAME_WINDOW_SIZE", format!("{width}x{height}"));
        }
        if let Some([width, height]) = run.launch.render_size {
            command.env("ENAME_RENDER_SIZE", format!("{width}x{height}"));
        }
        command.envs(&run.launch.env);

        let mut child = spawn::grouped(command).context("cannot start the game")?;
        let child_pid = child
            .id()
            .context("the game exited before its pid was read")?;
        tracing::info!(
            child_pid,
            port = self.port,
            "run {}: game spawned",
            self.generation
        );
        self.edit_run(|run| run.spawned_at = Some(now_f64()))?;

        let mut watch = Watch::default();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            self.watch_game(&mut watch, child_pid, child.as_mut())
                .await?;
            tokio::time::sleep(POLL).await;
        };
        // Reaps the rest of the group too.
        let status = child.wait().await.unwrap_or(status);
        self.record_exit(status, &watch)
    }

    /// One poll of a running game: notice it answering, then honour a stop request.
    async fn watch_game(
        &self,
        watch: &mut Watch,
        child_pid: u32,
        child: &mut dyn ChildWrapper,
    ) -> anyhow::Result<()> {
        if !watch.ready {
            self.check_ready(watch, child_pid).await?;
        }
        if watch.stop_deadline.is_none()
            && let Some(request) = self.stop_requested()?
        {
            watch.stop_deadline =
                Some(Instant::now() + Duration::from_secs_f64(request.grace_seconds.max(0.0)));
            if watch.game.is_some() {
                // Errors are fine: the game may be exiting already, and the grace bounds it.
                let _ = port::client(self.port)
                    .call_raw("game.exit", json!({}))
                    .await;
            } else {
                // Nothing answers yet, so nothing can exit gracefully.
                watch.stop_deadline = Some(Instant::now());
            }
        }
        if let Some(deadline) = watch.stop_deadline
            && Instant::now() >= deadline
            && !watch.killed
        {
            watch.killed = true;
            child.start_kill()?;
        }
        Ok(())
    }

    /// Records the game once BRP answers from our own process, and `ready` once its main schedule
    /// has advanced two frames.
    async fn check_ready(&self, watch: &mut Watch, child_pid: u32) -> anyhow::Result<()> {
        #[derive(Deserialize)]
        struct Pid {
            os_pid: u32,
        }
        #[derive(Deserialize)]
        struct Time {
            frame: u64,
        }
        let client = port::client(self.port);
        if watch.game.is_none() {
            let Ok(answer) = client.call::<Pid>("game.pid.get", json!({})).await else {
                return Ok(());
            };
            // On Unix `cargo run` execs the game, so it is the child itself. On Windows cargo
            // stays the parent.
            let ours = answer.os_pid == child_pid
                || (cfg!(windows)
                    && super::process::info(answer.os_pid)
                        .is_some_and(|info| info.parent == Some(child_pid)));
            if !ours {
                return Ok(());
            }
            let Some(game) = ProcessRef::of(answer.os_pid) else {
                return Ok(());
            };
            watch.game = Some(game);
            self.edit_run(|run| run.game = Some(game))?;
        }
        let Ok(time) = client.call::<Time>("game.time.get", json!({})).await else {
            return Ok(());
        };
        if time.frame >= 2 {
            watch.ready = true;
            tracing::info!(
                os_pid = watch.game.map(|game| game.pid),
                "run {}: ready",
                self.generation
            );
            self.edit_run(|run| {
                run.phase = Phase::Ready;
                run.ready_at = Some(now_f64());
            })?;
        }
        Ok(())
    }

    fn record_exit(&self, status: ExitStatus, watch: &Watch) -> anyhow::Result<()> {
        let requested = watch.stop_deadline.is_some();
        let mut exit = ExitInfo::from_status(self.generation, status, requested);
        if requested {
            exit.graceful = Some(!watch.killed);
        }
        tracing::info!(?exit, "game exited");
        write_json(&self.path(EXIT_FILE), &exit)?;
        self.set_phase(Phase::Exited)
    }
}

enum Build {
    Done,
    Failed,
    Stopped,
}

#[derive(Default)]
struct Watch {
    game: Option<ProcessRef>,
    ready: bool,
    stop_deadline: Option<Instant>,
    killed: bool,
}

/// `--features` for the workspace's features plus the run's.
fn features(project: &Project, run: &Run) -> Vec<String> {
    let all: Vec<&str> = project
        .config
        .features
        .iter()
        .chain(&run.launch.features)
        .map(String::as_str)
        .collect();
    if all.is_empty() {
        Vec::new()
    } else {
        vec!["--features".to_owned(), all.join(",")]
    }
}

/// Deletes earlier generations' executable copies. One still running cannot be deleted on
/// Windows, and that is fine: the next generation tries again.
fn remove_old_copies(dir: &Path, generation: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let own = format!("supervise-{generation}{}", std::env::consts::EXE_SUFFIX);
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("supervise-") && name != own {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
