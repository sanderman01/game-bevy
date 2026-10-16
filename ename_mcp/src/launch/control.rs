//! `start`, `stop`, `restart` and `status`: what callers do. They read and edit `state.json`,
//! spawn supervisors, probe ports and wait, always for a bounded time.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail};
use serde::Deserialize;
use serde_json::json;

use super::{
    BuildSummary, DEFAULT_INSTANCE, DEFAULT_PORT, GameState, LaunchParams, LaunchUpdate,
    LaunchedBy, PendingStatus, Project, Status,
    build_report::BuildReport,
    crash::{Crash, ExitInfo, ExitReason},
    now, now_f64,
    port::{self, Holder, Probe},
    process::{self, ProcessRef},
    spawn,
    state::{
        BUILD_JSON, BUILD_LOG, EXIT_FILE, GAME_LOG, Instance, Phase, Run, State, StopRequest,
        read_json, selected_path, write_selected,
    },
    supervise::DEFAULT_GRACE,
};

const POLL: Duration = Duration::from_millis(250);
/// How long a caller waits for a requested stop past its grace.
const STOP_MARGIN: Duration = Duration::from_secs(5);
/// Starting for longer than this with the process alive earns a hint.
const SLOW_START: f64 = 90.0;

/// Who is asking: their owner token, or `None` for the user.
#[derive(Clone, Debug, Default)]
pub struct Caller {
    pub owner: Option<String>,
    /// How a caller should follow a launch still in progress, for the note it gets.
    pub follow_hint: &'static str,
}

impl Caller {
    fn owns(&self, state: &State) -> bool {
        state.owner == self.owner
    }
}

/// The launcher for one workspace.
#[derive(Clone, Debug)]
pub struct Launcher {
    pub project: Project,
}

/// Options shared by `start` and `restart`.
#[derive(Clone, Debug, Default)]
pub struct LaunchRequest {
    pub instance: String,
    pub port: Option<u16>,
    pub update: LaunchUpdate,
    pub take_over: bool,
    pub wait: Duration,
}

impl Launcher {
    pub fn new(project: Project) -> Self {
        Self { project }
    }

    fn instance(&self, name: &str) -> anyhow::Result<Instance> {
        Instance::new(&self.project, name)
    }

    /// Where an instance is now, or once it leaves `building`/`starting` within `wait`.
    pub async fn status(
        &self,
        instance: &str,
        caller: &Caller,
        wait: Duration,
    ) -> anyhow::Result<Status> {
        let instance = self.instance(instance)?;
        let mut status = self.wait_settled(&instance, caller, wait).await?;
        status.yours = Some(self.yours(caller));
        status.caller = Some(
            caller
                .owner
                .clone()
                .unwrap_or_else(|| "null (the user)".into()),
        );
        Ok(status)
    }

    /// The caller's instances that still run.
    pub fn yours(&self, caller: &Caller) -> Vec<String> {
        Instance::all(&self.project)
            .into_iter()
            .filter(|instance| {
                instance
                    .read()
                    .ok()
                    .flatten()
                    .is_some_and(|state| state.is_live() && caller.owns(&state))
            })
            .map(|instance| instance.name)
            .collect()
    }

    pub async fn start(&self, request: LaunchRequest, caller: &Caller) -> anyhow::Result<Status> {
        let instance = self.instance(&request.instance)?;
        let deadline = Instant::now() + request.wait;

        if let Some(state) = instance.read()?.filter(State::is_live) {
            return self
                .join(&instance, &state, &request, caller, deadline)
                .await;
        }
        let launch = LaunchParams::default().merged(request.update.clone());
        self.launch_new(&instance, &request, launch, caller, deadline)
            .await
    }

    /// Starts a new run on an instance where nothing of ours runs.
    async fn launch_new(
        &self,
        instance: &Instance,
        request: &LaunchRequest,
        launch: LaunchParams,
        caller: &Caller,
        deadline: Instant,
    ) -> anyhow::Result<Status> {
        let previous = instance.read()?.filter(State::is_this_boot);
        let port = self.port_for(instance, request.port, previous.as_ref())?;
        if let Some(status) = self.port_conflict(instance, port).await {
            return Ok(status);
        }
        let joined = self.create_run(instance, port, launch, caller)?;
        let mut status = self.wait_settled_until(instance, caller, deadline).await?;
        if joined {
            status.add_note("joined a launch that started at the same time.");
        }
        self.finish_launch(instance, request, caller, &mut status);
        Ok(status)
    }

    /// `start` on an instance that already runs: report it, waiting on a launch in progress.
    async fn join(
        &self,
        instance: &Instance,
        state: &State,
        request: &LaunchRequest,
        caller: &Caller,
        deadline: Instant,
    ) -> anyhow::Result<Status> {
        // A finished restart's failure was reported when it happened.
        instance.update(|state| {
            if let Some(state) = state.as_mut()
                && state
                    .pending
                    .as_ref()
                    .is_some_and(|run| run.phase.is_final())
            {
                state.pending = None;
            }
            Ok(())
        })?;
        let mut status = self.wait_settled_until(instance, caller, deadline).await?;
        if status.state == GameState::Ready {
            status.add_note("already running; game_restart rebuilds.");
        } else if status.state.is_transient() {
            status.add_note("joined the launch in progress.");
        }
        if request.port.is_some_and(|port| port != state.port) {
            status.add_note(format!(
                "it runs on port {}; `port` was ignored. Stop it to change its port.",
                state.port
            ));
        }
        self.finish_launch(instance, request, caller, &mut status);
        Ok(status)
    }

    /// Selection and notes after a launch returns.
    fn finish_launch(
        &self,
        instance: &Instance,
        request: &LaunchRequest,
        caller: &Caller,
        status: &mut Status,
    ) {
        if status.state == GameState::Ready
            && let Err(error) =
                write_selected(&self.project, caller.owner.as_deref(), &instance.name)
        {
            status.add_note(format!("could not record the selection: {error:#}."));
        }
        if request.update.settle == Some(true) {
            status.add_note("`settle` is not available yet (no wait.settled); returned at ready.");
        }
    }

    pub async fn restart(&self, request: LaunchRequest, caller: &Caller) -> anyhow::Result<Status> {
        let instance = self.instance(&request.instance)?;
        let deadline = Instant::now() + request.wait;
        let state = instance.read()?.filter(State::is_this_boot);

        let Some(state) = state.clone().filter(State::is_live) else {
            // Nothing of ours runs: take over an external game if asked, then start with the
            // last launch's params.
            let port = self.port_for(&instance, request.port, state.as_ref())?;
            if let Probe::Game { os_pid, .. } = port::probe(port).await {
                if !request.take_over {
                    bail!(not_owner_external(&instance.name, port, os_pid));
                }
                self.stop_external(&instance, port, os_pid, DEFAULT_GRACE)
                    .await?;
            }
            let launch = state
                .as_ref()
                .and_then(|state| state.current.as_ref())
                .map(|run| run.launch.clone())
                .unwrap_or_default()
                .merged(request.update.clone());
            return self
                .launch_new(&instance, &request, launch, caller, deadline)
                .await;
        };

        if !request.take_over && !caller.owns(&state) {
            bail!(not_owner(&state));
        }
        if request.port.is_some_and(|port| port != state.port) {
            bail!(
                "instance `{}` runs on port {}; a restart keeps it. Stop it, then start it with \
                 the new port.",
                instance.name,
                state.port
            );
        }

        let generation = instance.update(|state| {
            let state = state.as_mut().context("state.json disappeared")?;
            let base = state
                .pending
                .as_ref()
                .or(state.current.as_ref())
                .map(|run| run.launch.clone())
                .unwrap_or_default();
            // A second restart replaces the first one's pending run; its supervisor sees that.
            state.last_generation += 1;
            let generation = state.last_generation;
            state.pending = Some(Run::new(generation, base.merged(request.update.clone())));
            state.owner = caller.owner.clone();
            let pid = spawn::supervisor(&instance.dir, generation)?;
            if let Some(run) = state.pending.as_mut() {
                run.supervisor = ProcessRef::of(pid);
            }
            Ok(generation)
        })?;

        let mut status = loop {
            let status = self.snapshot(&instance, caller).await?;
            let state = instance.read()?;
            let pending_done = state
                .as_ref()
                .and_then(|state| state.pending.as_ref())
                .is_none_or(|run| run.generation != generation || run.phase.is_final());
            let promoted = state
                .as_ref()
                .and_then(|state| state.current.as_ref())
                .is_some_and(|run| run.generation == generation);
            let settled = pending_done && (!promoted || !status.state.is_transient());
            if settled || Instant::now() >= deadline {
                break status;
            }
            tokio::time::sleep(POLL).await;
        };
        if status
            .pending
            .as_ref()
            .is_some_and(|pending| pending.state == GameState::BuildFailed)
        {
            status.add_note("the build failed; the previous game is still running.");
        }
        self.add_follow_hint(&mut status, caller);
        self.finish_launch(&instance, &request, caller, &mut status);
        Ok(status)
    }

    pub async fn stop(
        &self,
        instance: &str,
        take_over: bool,
        grace_seconds: f64,
        caller: &Caller,
    ) -> anyhow::Result<Status> {
        let instance = self.instance(instance)?;
        let grace_seconds = grace_seconds.clamp(0.0, 60.0);
        let state = instance.read()?.filter(State::is_live);

        let Some(state) = state else {
            let port = instance
                .read()?
                .filter(State::is_this_boot)
                .map(|state| state.port)
                .or_else(|| default_port(&instance.name));
            let probe = match port {
                Some(port) => port::probe(port).await,
                None => Probe::Free,
            };
            let port = port.unwrap_or_default();
            return match probe {
                Probe::Game { os_pid, .. } if take_over => {
                    self.stop_external(&instance, port, os_pid, grace_seconds)
                        .await
                }
                Probe::Game { os_pid, .. } => {
                    bail!(not_owner_external(&instance.name, port, os_pid))
                }
                Probe::Other => {
                    let mut status = self.snapshot(&instance, caller).await?;
                    status.add_note("a process without BRP holds the port; not stopping it.");
                    Ok(status)
                }
                Probe::Free => {
                    let mut status = self.snapshot(&instance, caller).await?;
                    status.add_note("nothing was running.");
                    Ok(status)
                }
            };
        };

        if !take_over && !caller.owns(&state) {
            bail!(not_owner(&state));
        }
        instance.update(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(());
            };
            for run in [state.current.as_mut(), state.pending.as_mut()]
                .into_iter()
                .flatten()
            {
                if !run.phase.is_final() && run.stop_requested.is_none() {
                    run.stop_requested = Some(StopRequest { grace_seconds });
                }
            }
            Ok(())
        })?;

        let deadline = Instant::now() + Duration::from_secs_f64(grace_seconds) + STOP_MARGIN;
        let mut killed_orphan = false;
        loop {
            let Some(state) = instance.read()? else {
                break;
            };
            let runs: Vec<&Run> = [state.current.as_ref(), state.pending.as_ref()]
                .into_iter()
                .flatten()
                .collect();
            // A game whose supervisor died is stopped here, by pid, as the external path does.
            for run in &runs {
                if !run.supervisor_alive()
                    && let Some(game) = run.game.filter(ProcessRef::is_alive)
                {
                    killed_orphan |= self.kill_game(game)?;
                }
            }
            let done = runs
                .iter()
                .all(|run| !run.supervisor_alive() && !run.game_alive());
            if done || Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        self.clear_selection(&instance, caller);
        let mut status = self.snapshot(&instance, caller).await?;
        if killed_orphan {
            status.add_note(
                "its supervisor was gone, so the game was killed by pid; exit status unknown.",
            );
        }
        if status.state.is_transient() || status.state == GameState::Ready {
            status.add_note("it did not stop in time.");
        }
        Ok(status)
    }

    /// Stops a game this launcher did not start: `game.exit`, then a kill by pid, each only after
    /// checking it is this workspace's editor.
    async fn stop_external(
        &self,
        instance: &Instance,
        port: u16,
        os_pid: u32,
        grace_seconds: f64,
    ) -> anyhow::Result<Status> {
        let holder = Holder::of(os_pid);
        let game = ProcessRef::of(os_pid).context("the game holding the port is gone")?;
        let binary = self.project.binary_path();
        let info = process::info(os_pid).context("the game holding the port is gone")?;
        let exe_ok = info
            .exe
            .as_deref()
            .is_some_and(|exe| is_game_binary(&self.project, exe, &binary));
        let cwd_ok = info
            .cwd
            .as_deref()
            .is_some_and(|cwd| is_inside(cwd, &self.project.workspace_root));
        if !exe_ok || !cwd_ok {
            bail!(
                "the game on port {port} ({}) is not this workspace's `{}`; not stopping it.",
                holder.describe(),
                self.project.config.package
            );
        }

        let _ = port::client(port).call_raw("game.exit", json!({})).await;
        let deadline = Instant::now() + Duration::from_secs_f64(grace_seconds);
        while game.is_alive() && Instant::now() < deadline {
            tokio::time::sleep(POLL).await;
        }
        let graceful = !game.is_alive();
        if !graceful {
            self.kill_game(game)?;
            let deadline = Instant::now() + STOP_MARGIN;
            while game.is_alive() && Instant::now() < deadline {
                tokio::time::sleep(POLL).await;
            }
        }
        Ok(Status {
            instance: instance.name.clone(),
            state: if game.is_alive() {
                GameState::External
            } else {
                GameState::Stopped
            },
            port,
            os_pid: Some(os_pid),
            package: self.project.config.package.clone(),
            launched_by: Some(LaunchedBy::External),
            holder: Some(holder),
            graceful: Some(graceful),
            note: Some("not our child; exit status unknown.".to_owned()),
            ..Status::default()
        })
    }

    fn kill_game(&self, game: ProcessRef) -> anyhow::Result<bool> {
        let binary = self.project.binary_path();
        game.kill_checked(|exe| is_game_binary(&self.project, exe, &binary))
    }

    fn clear_selection(&self, instance: &Instance, caller: &Caller) {
        let owner = caller.owner.as_deref();
        let path = selected_path(&self.project, owner);
        let selected: Option<String> = read_json(&path).ok().flatten();
        if selected.as_deref() == Some(instance.name.as_str()) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// The port an instance gets: the caller's, its recorded one, 15702 for `default`, else the
    /// first free pair.
    fn port_for(
        &self,
        instance: &Instance,
        requested: Option<u16>,
        previous: Option<&State>,
    ) -> anyhow::Result<u16> {
        if let Some(port) = requested {
            if port == u16::MAX {
                bail!("`port` needs a free port after it for the render world");
            }
            return Ok(port);
        }
        if let Some(state) = previous {
            return Ok(state.port);
        }
        if instance.name == DEFAULT_INSTANCE {
            return Ok(DEFAULT_PORT);
        }
        port::free_pair(&self.project, &instance.name)
    }

    /// `port_in_use` when anything else holds the port.
    async fn port_conflict(&self, instance: &Instance, port: u16) -> Option<Status> {
        let mut status = Status {
            instance: instance.name.clone(),
            state: GameState::PortInUse,
            port,
            package: self.project.config.package.clone(),
            ..Status::default()
        };
        match port::probe(port).await {
            Probe::Free => return None,
            Probe::Game { os_pid, .. } => {
                status.holder = Some(Holder::of(os_pid));
                status.add_note(format!(
                    "a game this launcher did not start holds port {port}. Pass `instance` (or \
                     `port`) to start another one beside it."
                ));
            }
            Probe::Other => status.add_note(format!(
                "a process without BRP holds port {port} or {}.",
                port + 1
            )),
        }
        Some(status)
    }

    /// Writes a new run as `current` and spawns its supervisor, under the lock. Returns true when
    /// another caller started one first, which is then joined instead.
    fn create_run(
        &self,
        instance: &Instance,
        port: u16,
        launch: LaunchParams,
        caller: &Caller,
    ) -> anyhow::Result<bool> {
        instance.update(|state| {
            if state.as_ref().is_some_and(State::is_live) {
                return Ok(true);
            }
            let last_generation = state.as_ref().map_or(0, |state| state.last_generation);
            let generation = last_generation + 1;
            let mut run = Run::new(generation, launch);
            let pid = spawn::supervisor(&instance.dir, generation)?;
            run.supervisor = ProcessRef::of(pid);
            *state = Some(State {
                instance: instance.name.clone(),
                workspace_root: self.project.workspace_root.clone(),
                boot_time: process::boot_time(),
                port,
                render_port: port + 1,
                owner: caller.owner.clone(),
                last_generation: generation,
                current: Some(run),
                pending: None,
            });
            Ok(false)
        })
    }

    async fn wait_settled(
        &self,
        instance: &Instance,
        caller: &Caller,
        wait: Duration,
    ) -> anyhow::Result<Status> {
        self.wait_settled_until(instance, caller, Instant::now() + wait)
            .await
    }

    /// Polls until the instance leaves `building`/`starting` or `deadline` passes.
    async fn wait_settled_until(
        &self,
        instance: &Instance,
        caller: &Caller,
        deadline: Instant,
    ) -> anyhow::Result<Status> {
        let mut status = loop {
            let status = self.snapshot(instance, caller).await?;
            let busy = status.state.is_transient()
                || status
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.state.is_transient());
            if !busy || Instant::now() >= deadline {
                break status;
            }
            tokio::time::sleep(POLL).await;
        };
        self.add_follow_hint(&mut status, caller);
        Ok(status)
    }

    fn add_follow_hint(&self, status: &mut Status, caller: &Caller) {
        let busy = status.state.is_transient()
            || status
                .pending
                .as_ref()
                .is_some_and(|pending| pending.state.is_transient());
        if busy && !caller.follow_hint.is_empty() {
            status.add_note(format!("still in progress; {}.", caller.follow_hint));
        }
    }

    /// Where an instance is right now.
    pub async fn snapshot(&self, instance: &Instance, caller: &Caller) -> anyhow::Result<Status> {
        let state = instance.read()?.filter(State::is_this_boot);
        let mut status = Status {
            instance: instance.name.clone(),
            // 0 for an instance that never had a port.
            port: state
                .as_ref()
                .map(|state| state.port)
                .or_else(|| default_port(&instance.name))
                .unwrap_or_default(),
            package: self.project.config.package.clone(),
            log: Some(instance.path(GAME_LOG).display().to_string()),
            ..Status::default()
        };
        if let Some(state) = &state {
            status.owner = state.owner.clone();
            status.owned_by_caller = caller.owns(state);
            if let Some(run) = &state.current {
                self.describe_run(instance, state, run, &mut status).await;
            }
            if let Some(run) = &state.pending {
                status.pending = Some(Box::new(self.describe_pending(instance, run)));
            }
        }
        if matches!(
            status.state,
            GameState::Stopped | GameState::Exited | GameState::BuildFailed
        ) {
            self.find_external(&mut status).await;
        }
        Ok(status)
    }

    async fn describe_run(
        &self,
        instance: &Instance,
        state: &State,
        run: &Run,
        status: &mut Status,
    ) {
        status.launched_by = Some(LaunchedBy::Launcher);
        status.generation = Some(run.generation);
        status.started_at = Some(run.started_at);
        status.build = run.build_seconds.map(|seconds| BuildSummary {
            seconds: Some(seconds),
            warning_count: run.warning_count.unwrap_or(0),
        });
        if let (Some(spawned), Some(ready)) = (run.spawned_at, run.ready_at) {
            status.startup_seconds = Some(((ready - spawned) * 10.0).round() / 10.0);
        }
        let supervisor_alive = run.supervisor_alive();
        let game = run.game.filter(ProcessRef::is_alive);
        status.os_pid = game.map(|game| game.pid);
        status.stale_binary = game.is_some_and(|game| self.is_stale(game));

        status.state = match run.phase {
            Phase::Building if supervisor_alive => GameState::Building,
            Phase::Building => {
                status.add_note("its supervisor died during the build; see supervise.log.");
                GameState::Stopped
            }
            Phase::BuildFailed => {
                let report =
                    BuildReport::read(&instance.path(BUILD_JSON), &instance.path(BUILD_LOG));
                status.error_count = Some(report.error_count);
                status.errors = report.errors;
                GameState::BuildFailed
            }
            Phase::Stopped => GameState::Stopped,
            Phase::Starting | Phase::Ready if game.is_some() || supervisor_alive => {
                self.describe_live(state, run, game, status).await
            }
            Phase::Starting | Phase::Ready | Phase::Exited => {
                self.describe_exit(instance, run, supervisor_alive, status);
                status.state
            }
        };
    }

    /// `ready` only when the recorded game answers on the port and its schedule advanced.
    async fn describe_live(
        &self,
        state: &State,
        run: &Run,
        game: Option<ProcessRef>,
        status: &mut Status,
    ) -> GameState {
        #[derive(Deserialize)]
        struct Pid {
            pid: String,
            os_pid: u32,
        }
        let Some(game) = game else {
            if let Some(spawned) = run.spawned_at
                && now_f64() - spawned > SLOW_START
            {
                status.add_note(
                    "starting for over 90 s without a BRP answer: is ename_remote linked, and \
                     is the port free?",
                );
            }
            return GameState::Starting;
        };
        if run.phase != Phase::Ready {
            return GameState::Starting;
        }
        match port::client(state.port)
            .call::<Pid>("game.pid.get", json!({}))
            .await
        {
            Ok(answer) if answer.os_pid == game.pid => {
                status.pid = Some(answer.pid);
                GameState::Ready
            }
            Ok(answer) => {
                status.add_note(format!(
                    "port {} answers from os pid {}, not this game.",
                    state.port, answer.os_pid
                ));
                GameState::Starting
            }
            Err(_) => {
                status.add_note("the game process is alive but does not answer BRP.");
                GameState::Starting
            }
        }
    }

    fn describe_exit(
        &self,
        instance: &Instance,
        run: &Run,
        supervisor_alive: bool,
        status: &mut Status,
    ) {
        let exit: Option<ExitInfo> = read_json::<ExitInfo>(&instance.path(EXIT_FILE))
            .ok()
            .flatten()
            .filter(|exit| exit.generation == run.generation);
        let Some(exit) = exit else {
            status.state = if supervisor_alive {
                GameState::Starting
            } else {
                status
                    .add_note("its supervisor is gone and left no exit record; see supervise.log.");
                GameState::Exited
            };
            return;
        };
        status.state = if exit.reason == ExitReason::Requested {
            GameState::Stopped
        } else {
            GameState::Exited
        };
        status.graceful = exit.graceful;
        if exit.is_failure() {
            status.crash = Some(Crash::new(&exit, &instance.path(GAME_LOG)));
        }
        status.exit = Some(exit);
    }

    fn describe_pending(&self, instance: &Instance, run: &Run) -> PendingStatus {
        let alive = run.supervisor_alive();
        let state = match run.phase {
            Phase::Building if alive => GameState::Building,
            Phase::BuildFailed => GameState::BuildFailed,
            Phase::Starting | Phase::Ready if alive => GameState::Starting,
            _ => GameState::Stopped,
        };
        let mut pending = PendingStatus {
            state,
            generation: run.generation,
            build: run.build_seconds.map(|seconds| BuildSummary {
                seconds: Some(seconds),
                warning_count: run.warning_count.unwrap_or(0),
            }),
            errors: Vec::new(),
            error_count: None,
        };
        if state == GameState::BuildFailed {
            let report = BuildReport::read(&instance.path(BUILD_JSON), &instance.path(BUILD_LOG));
            pending.error_count = Some(report.error_count);
            pending.errors = report.errors;
        }
        pending
    }

    /// A game this launcher did not start, on a port no run of ours holds.
    async fn find_external(&self, status: &mut Status) {
        if status.port == 0 {
            return;
        }
        match port::probe(status.port).await {
            Probe::Free => {}
            Probe::Game { os_pid, pid } => {
                status.state = GameState::External;
                status.launched_by = Some(LaunchedBy::External);
                status.os_pid = Some(os_pid);
                status.pid = Some(pid);
                status.holder = Some(Holder::of(os_pid));
                status.owner = None;
                status.owned_by_caller = false;
                status.crash = None;
                status.add_note("a game this launcher did not start holds the port.");
            }
            Probe::Other => {
                status.state = GameState::PortInUse;
                status.add_note("a process without BRP holds the port.");
            }
        }
    }

    /// Whether the executable on disk is newer than the running game.
    fn is_stale(&self, game: ProcessRef) -> bool {
        let modified = std::fs::metadata(self.project.binary_path())
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
        modified.is_some_and(|modified| modified.as_secs() > game.start_time)
    }
}

/// The port an instance without a state would get without asking: only `default` has one.
fn default_port(instance: &str) -> Option<u16> {
    (instance == DEFAULT_INSTANCE).then_some(DEFAULT_PORT)
}

/// `<target_directory>/<profile>/<binary>` of this workspace's editor.
pub(super) fn is_game_binary(project: &Project, exe: &Path, binary: &Path) -> bool {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let target = canonical(&project.target_directory);
    exe.file_name() == binary.file_name()
        && exe
            .parent()
            .and_then(Path::parent)
            .is_some_and(|dir| canonical(dir) == target)
}

fn is_inside(path: &Path, root: &Path) -> bool {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical(path).starts_with(canonical(root))
}

fn not_owner(state: &State) -> String {
    let owner = state
        .owner
        .as_deref()
        .map_or_else(|| "the user".to_owned(), |owner| format!("`{owner}`"));
    let started = state
        .current
        .as_ref()
        .map(|run| format!(" {} ago", ago(run.started_at)))
        .unwrap_or_default();
    format!(
        "not_owner: instance `{}` was started{started} by {owner}. Only its owner may stop or \
         restart it; pass take_over: true only when the user asked you to.",
        state.instance
    )
}

fn not_owner_external(instance: &str, port: u16, os_pid: u32) -> String {
    format!(
        "not_owner: the game on port {port} for instance `{instance}` ({}) was not started by \
         this launcher. Pass take_over: true only when the user asked you to stop it.",
        Holder::of(os_pid).describe()
    )
}

fn ago(unix: u64) -> String {
    let seconds = now().saturating_sub(unix);
    match seconds {
        0..120 => format!("{seconds} s"),
        120..7200 => format!("{} min", seconds / 60),
        _ => format!("{} h", seconds / 3600),
    }
}
