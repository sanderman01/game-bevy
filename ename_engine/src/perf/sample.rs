//! `perf.sample`: settle, warm up, then sample `repeats` windows back to back and summarize them.
//!
//! A [`Sampler`] on a work entity is driven once a frame in `Last`: settling (a [`SettleGate`],
//! then a steady frame time), warm-up, each window, and the wait for the window's GPU frames. A
//! window is a range of [`PerfFrame`] ids. A window during which the primary window's size, the
//! active cameras' labels or a view's size changed is dropped and sampled again once; a second
//! change keeps it with an `unstable_view` warning. Load snapshots run on the IO task pool before
//! the first window and after each one, never during a window. `perf.compare` drives a
//! [`Sampler`] per step.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use bevy::{
    diagnostic::DiagnosticsStore,
    ecs::{lifecycle::HookContext, system::SystemParam, world::DeferredWorld},
    prelude::*,
    tasks::{IoTaskPool, Task, futures::check_ready},
    window::{Monitor, PresentMode, PrimaryMonitor, PrimaryWindow, WindowOccluded, WindowPosition},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    PerfFrame, PerfOverlay, PerfViewLabel,
    gpu_timer::{GpuTiming, TimestampSupport, Timestamps, ViewTime, view_label},
    load::{Load, LoadSnapshot},
    passes::{PassRecords, PassWarning},
    records::{PerfCounters, PerfRecordAge, PerfRecords},
    report::{
        Context, Detail, PassSeries, Rerun, Run, SampleResult, Sampled, ViewSeries, Warnings,
        report,
    },
    stats::median,
};
use crate::{
    camera::MainCameraFilter,
    command::{
        Call, CommandAppExt, CommandError, CommandSpec, JobCancelRequested, JobHandle, JobWork,
        Jobs, Length, OnCancel, parse_timecode,
    },
    video::VideoRequest,
    wait::{Activity, SettleGate, SettleParams, SettleTimedOut, Settled, WaitSystems},
};

const DEFAULT_SECONDS: f64 = 5.0;
const MAX_WINDOW: Duration = Duration::from_secs(600);
const DEFAULT_REPEATS: u32 = 3;
const MAX_REPEATS: u32 = 20;
const DEFAULT_WARMUP_FRAMES: u32 = 30;
const MAX_WARMUP_FRAMES: u32 = 10_000;
/// The steady check compares the median of the last this many frames with the ones before.
const STEADY_FRAMES: usize = 30;
const STEADY_TOLERANCE: f64 = 0.05;
const STEADY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long after a window its last GPU frames may take to arrive.
const GPU_WAIT: Duration = Duration::from_secs(1);
/// Records are kept this long beyond the current phase's start.
const RECORD_MARGIN: Duration = Duration::from_secs(5);
/// Diagnostics under this prefix are Bevy's pass timings, reported as passes.
const PASS_PREFIX: &str = "render/";

#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct SampleParams {
    /// How long each window runs. Exactly one key: {"frames": 600}, {"seconds": 5} or
    /// {"timecode": "00:00:05"}; at most 10 minutes. Defaults to 5 seconds.
    #[serde(default)]
    pub duration: Option<Length>,
    /// Windows sampled back to back, 1 to 20. Their spread is the `noise`. Defaults to 3.
    #[serde(default)]
    pub repeats: Option<u32>,
    /// How to settle first, as `wait.settled` does; `{}` or left out uses its defaults. A timeout
    /// samples anyway, with a `not_settled` warning.
    #[serde(default)]
    pub settle: Option<SettleParams>,
    /// Sample at once: no settle and no steady frame time check.
    #[serde(default)]
    pub skip_settle: bool,
    /// Frames dropped before the first window, 0 to 10000. Defaults to 30.
    #[serde(default)]
    pub warmup_frames: Option<u32>,
    /// Switch the window to `AutoNoVsync` for the sample and restore it after, so frame time is
    /// not capped at the refresh rate. Changes what the user sees.
    #[serde(default)]
    pub uncapped: bool,
    /// "summary" lists the top 10 passes by GPU time and the registered counters; "full" every
    /// pass and every diagnostic. Defaults to "summary".
    #[serde(default)]
    pub detail: Detail,
}

#[derive(Clone, Copy, Debug)]
enum WindowLength {
    Frames(u32),
    Time(Duration),
}

/// Checked [`SampleParams`].
#[derive(Clone, Debug)]
pub(crate) struct SamplePlan {
    length: WindowLength,
    repeats: u32,
    settle: Option<SettleParams>,
    warmup_frames: u32,
    uncapped: bool,
    pub detail: Detail,
}

impl SamplePlan {
    /// `repeats` defaults to `default_repeats` when the params leave it out.
    pub fn new(
        params: &SampleParams,
        default_repeats: u32,
        world: &World,
    ) -> Result<Self, CommandError> {
        let invalid = |message: String| CommandError::invalid("invalid_params", message);
        let length = match &params.duration {
            None => WindowLength::Time(Duration::from_secs_f64(DEFAULT_SECONDS)),
            Some(Length::Frames(0)) => return Err(invalid("`frames` must be above 0".into())),
            Some(Length::Frames(frames)) => WindowLength::Frames(*frames),
            Some(Length::Seconds(seconds)) => {
                if !(*seconds > 0.0 && *seconds <= MAX_WINDOW.as_secs_f64()) {
                    return Err(invalid(format!(
                        "`seconds` must be above 0 and at most {}, not {seconds}",
                        MAX_WINDOW.as_secs()
                    )));
                }
                WindowLength::Time(Duration::from_secs_f64(*seconds))
            }
            Some(Length::Timecode(text)) => {
                let length = parse_timecode(text).map_err(invalid)?;
                if length > MAX_WINDOW {
                    return Err(invalid(format!(
                        "a window is at most {} s, not {text}",
                        MAX_WINDOW.as_secs()
                    )));
                }
                WindowLength::Time(length)
            }
        };
        let repeats = params.repeats.unwrap_or(default_repeats);
        if !(1..=MAX_REPEATS).contains(&repeats) {
            return Err(invalid(format!(
                "`repeats` must be 1 to {MAX_REPEATS}, not {repeats}"
            )));
        }
        let warmup_frames = params.warmup_frames.unwrap_or(DEFAULT_WARMUP_FRAMES);
        if warmup_frames > MAX_WARMUP_FRAMES {
            return Err(invalid(format!(
                "`warmup_frames` must be at most {MAX_WARMUP_FRAMES}, not {warmup_frames}"
            )));
        }
        let settle = (!params.skip_settle).then(|| params.settle.clone().unwrap_or_default());
        if let Some(settle) = &settle {
            SettleGate::new(settle, world)?;
        }
        Ok(Self {
            length,
            repeats,
            settle,
            warmup_frames,
            uncapped: params.uncapped,
            detail: params.detail,
        })
    }
}

/// Marks the work entity of a running `perf.sample` or `perf.compare`: one at a time.
#[derive(Component, Default)]
pub(crate) struct PerfJob;

/// Fails `perf_busy` while another perf job runs.
pub(crate) fn check_not_busy(world: &mut World) -> Result<(), CommandError> {
    let busy = world
        .query_filtered::<(), With<PerfJob>>()
        .iter(world)
        .next()
        .is_some();
    if busy {
        return Err(CommandError::failed(
            "perf_busy",
            "another `perf.sample` or `perf.compare` is running",
        )
        .with_hint("Wait for that job to finish, or cancel it."));
    }
    Ok(())
}

/// What a sampler reports while it runs.
#[derive(Serialize, Clone, Debug)]
pub(crate) struct Progress {
    /// "settling", "sampling" or "waiting_gpu".
    phase: &'static str,
    /// The window being sampled, 1-based.
    run: usize,
    of: u32,
    /// What the settle step still waits on.
    #[serde(skip_serializing_if = "Option::is_none")]
    busy: Option<BTreeMap<&'static str, Activity>>,
}

/// A sample in progress. Spawn with [`spawn_sampler`]; when [`Sampler::take_outcome`] returns
/// something, the owner reports it and despawns the entity.
#[derive(Component)]
pub(crate) struct Sampler {
    plan: SamplePlan,
    phase: Phase,
    runs: Vec<Run>,
    context: Context,
    /// The run that was already sampled again after a view change.
    rerun: Option<usize>,
    stop: bool,
    /// The latest load snapshot: before the first window, then after each one.
    last_load: Option<LoadStamp>,
    /// Other processes' load over each window, from the snapshots around it.
    loads: Vec<Load>,
    load_task: Option<Task<LoadStamp>>,
    /// Records are kept from here on; the start of the current phase.
    anchor: Instant,
    progress: Progress,
    outcome: Option<Result<Sampled, CommandError>>,
}

type LoadStamp = (Instant, LoadSnapshot);

enum Phase {
    /// A [`SettleGate`] is on the entity; its observers move on.
    Settling,
    /// Waiting for the frame time to stop drifting, from frame `from`.
    Steady {
        from: u64,
        since: Instant,
    },
    /// The first window starts at frame `until`.
    Warmup {
        until: u64,
    },
    Window(OpenWindow),
    WaitingGpu {
        window: ClosedWindow,
        since: Instant,
    },
    Done,
}

struct OpenWindow {
    start: u64,
    started: Instant,
    /// The layout of the window's first frame.
    layout: Option<Layout>,
    sizes: BTreeMap<String, UVec2>,
    /// Every diagnostic per frame, with `detail: "full"`.
    diagnostics: BTreeMap<String, Vec<f64>>,
}

struct ClosedWindow {
    start: u64,
    end: u64,
    seconds: f64,
    sizes: BTreeMap<String, UVec2>,
    diagnostics: Option<BTreeMap<String, Vec<f64>>>,
}

impl Sampler {
    fn new(plan: SamplePlan, frame: u64, settling: bool) -> Self {
        let phase = if settling {
            Phase::Settling
        } else {
            Phase::Warmup {
                until: frame + 1 + u64::from(plan.warmup_frames),
            }
        };
        let of = plan.repeats;
        Self {
            plan,
            phase,
            runs: Vec::new(),
            context: Context {
                focused: true,
                gpu_available: true,
                ..default()
            },
            rerun: None,
            stop: false,
            last_load: None,
            loads: Vec::new(),
            load_task: None,
            anchor: Instant::now(),
            progress: Progress {
                phase: if settling { "settling" } else { "sampling" },
                run: 1,
                of,
                busy: None,
            },
            outcome: None,
        }
    }

    pub fn progress(&self) -> &Progress {
        &self.progress
    }

    /// Ends the sample with the runs finished so far.
    pub fn stop(&mut self) {
        self.stop = true;
    }

    /// The finished sample, once: the runs, or why there are none (`cancelled`, `no_frames`).
    pub fn take_outcome(&mut self) -> Option<Result<Sampled, CommandError>> {
        self.outcome.take()
    }

    fn enter_steady(&mut self, frame: u64) {
        self.phase = Phase::Steady {
            from: frame + 1,
            since: Instant::now(),
        };
        self.anchor = Instant::now();
    }

    fn enter_warmup(&mut self, frame: u64) {
        self.phase = Phase::Warmup {
            until: frame + 1 + u64::from(self.plan.warmup_frames),
        };
    }

    fn open_window(&mut self, frame: u64) {
        self.phase = Phase::Window(OpenWindow {
            start: frame + 1,
            started: Instant::now(),
            layout: None,
            sizes: BTreeMap::new(),
            diagnostics: BTreeMap::new(),
        });
        self.anchor = Instant::now();
    }

    fn start_load_snapshot(&mut self) {
        self.load_task =
            Some(IoTaskPool::get().spawn(async { (Instant::now(), LoadSnapshot::take()) }));
    }

    fn poll_load_snapshot(&mut self) {
        let Some(stamp) = self.load_task.as_mut().and_then(check_ready) else {
            return;
        };
        self.load_task = None;
        if let Some((start, before)) = &self.last_load {
            let (end, after) = &stamp;
            let window = end.saturating_duration_since(*start);
            self.loads.push(LoadSnapshot::diff(before, after, window));
        }
        self.last_load = Some(stamp);
    }

    fn finish(&mut self, outcome: Result<(), CommandError>) {
        self.phase = Phase::Done;
        self.load_task = None;
        self.outcome = Some(outcome.map(|()| self.sampled()));
    }

    fn finish_stopped(&mut self) {
        if self.runs.is_empty() {
            self.finish(Err(CommandError::failed(
                "cancelled",
                "cancelled before a window finished",
            )));
            return;
        }
        let message = format!(
            "cancelled after {} of {} runs",
            self.runs.len(),
            self.plan.repeats
        );
        self.context.warnings.add("cancelled", message);
        self.finish(Ok(()));
    }

    fn sampled(&mut self) -> Sampled {
        let mut context = std::mem::take(&mut self.context);
        if let Some(load) = Load::busiest(std::mem::take(&mut self.loads)) {
            if load.machine_busy {
                let gpu = match (load.gpu_others_ms, load.gpu_others_measured_ms) {
                    (Some(ms), _) => format!(" and {ms} ms of GPU time"),
                    (None, Some(ms)) => format!(" and at least {ms} ms of GPU time"),
                    (None, None) => String::new(),
                };
                context.warnings.add(
                    "machine_busy",
                    format!(
                        "other processes used {} cores{gpu} during a sample window; see `load.top`",
                        load.other_cpu_cores
                    ),
                );
            }
            context.load = Some(load);
        }
        Sampled {
            runs: std::mem::take(&mut self.runs),
            context,
        }
    }
}

/// Restores the primary window's present mode when the sample that uncapped it ends.
#[derive(Component, Clone, Copy)]
#[component(on_remove = restore_present_mode)]
struct Uncapped {
    window: Entity,
    previous: PresentMode,
}

fn restore_present_mode(mut world: DeferredWorld, context: HookContext) {
    let Some(&Uncapped { window, previous }) = world.get::<Uncapped>(context.entity) else {
        return;
    };
    if let Some(mut window) = world.get_mut::<Window>(window)
        && window.present_mode != previous
    {
        window.present_mode = previous;
    }
}

/// Spawns a sampler for `plan` and returns its entity. Holds perf demand while it lives.
pub(crate) fn spawn_sampler(world: &mut World, plan: SamplePlan) -> Result<Entity, CommandError> {
    let gate = plan
        .settle
        .as_ref()
        .map(|settle| SettleGate::new(settle, world))
        .transpose()?;
    let uncapped = if plan.uncapped { uncap(world) } else { None };
    let frame = world.resource::<PerfFrame>().0;
    let sampler = Sampler::new(plan, frame, gate.is_some());
    let mut entity = world.spawn((sampler, PerfRecordAge(RECORD_MARGIN)));
    if let Some(gate) = gate {
        entity.insert(gate);
    }
    if let Some(uncapped) = uncapped {
        entity.insert(uncapped);
    }
    Ok(entity.id())
}

fn uncap(world: &mut World) -> Option<Uncapped> {
    let mut windows = world.query_filtered::<(Entity, &mut Window), With<PrimaryWindow>>();
    let (entity, mut window) = windows.single_mut(world).ok()?;
    let previous = window.present_mode;
    if previous != PresentMode::AutoNoVsync {
        window.present_mode = PresentMode::AutoNoVsync;
    }
    Some(Uncapped {
        window: entity,
        previous,
    })
}

/// The primary window's occlusion, as winit last reported it.
#[derive(Resource, Default)]
struct WindowOcclusion(bool);

fn track_occlusion(
    mut occluded: MessageReader<WindowOccluded>,
    primary: Query<(), With<PrimaryWindow>>,
    mut occlusion: ResMut<WindowOcclusion>,
) {
    for message in occluded.read() {
        if primary.contains(message.window) {
            occlusion.0 = message.occluded;
        }
    }
}

/// The view a window measures. A change during a window invalidates it.
#[derive(Clone, Debug, PartialEq)]
struct Layout {
    window: Option<UVec2>,
    /// Active cameras by entity.
    views: Vec<LayoutView>,
}

#[derive(Clone, Debug, PartialEq)]
struct LayoutView {
    camera: Entity,
    label: String,
    viewport: Option<UVec2>,
    target: Option<UVec2>,
}

impl Layout {
    /// Why `self` and `new` differ.
    fn change(&self, new: &Layout) -> String {
        let size = |size: Option<UVec2>| size.map_or("none".into(), |s| format!("{}x{}", s.x, s.y));
        if self.window != new.window {
            return format!(
                "the window resized from {} to {}",
                size(self.window),
                size(new.window)
            );
        }
        let cameras = |layout: &Layout| {
            layout
                .views
                .iter()
                .map(|view| (view.camera, view.label.clone()))
                .collect::<Vec<_>>()
        };
        if cameras(self) != cameras(new) {
            let labels = |layout: &Layout| {
                layout
                    .views
                    .iter()
                    .map(|view| view.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            return format!(
                "the cameras changed from [{}] to [{}]",
                labels(self),
                labels(new)
            );
        }
        self.views
            .iter()
            .zip(&new.views)
            .find(|(old, new)| old != new)
            .map_or_else(
                || "a view changed".to_owned(),
                |(old, new)| {
                    format!(
                        "`{}` resized from {} to {}",
                        old.label,
                        size(old.viewport.or(old.target)),
                        size(new.viewport.or(new.target))
                    )
                },
            )
    }
}

#[derive(SystemParam)]
struct ViewState<'w, 's> {
    window: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    cameras: Query<
        'w,
        's,
        (
            Entity,
            &'static Camera,
            Option<&'static PerfViewLabel>,
            Option<&'static Name>,
        ),
    >,
    main_camera: Query<'w, 's, Entity, MainCameraFilter>,
    monitors: Query<'w, 's, (&'static Monitor, Has<PrimaryMonitor>)>,
    occlusion: Res<'w, WindowOcclusion>,
    overlay: Res<'w, PerfOverlay>,
    recordings: Query<'w, 's, (), With<VideoRequest>>,
}

impl ViewState<'_, '_> {
    fn layout(&self) -> Layout {
        let main_camera = self.main_camera.single().ok();
        let mut views: Vec<LayoutView> = self
            .cameras
            .iter()
            .filter(|(_, camera, ..)| camera.is_active)
            .map(|(entity, camera, perf_label, name)| LayoutView {
                camera: entity,
                label: view_label(entity, camera, perf_label, name, main_camera).into_owned(),
                viewport: camera.physical_viewport_size(),
                target: camera.physical_target_size(),
            })
            .collect();
        views.sort_by_key(|view| view.camera);
        Layout {
            window: self.window.single().ok().map(Window::physical_size),
            views,
        }
    }

    /// The monitor the window is on, as far as the platform tells; else the primary one.
    fn refresh_ms(&self) -> Option<f64> {
        let window = self.window.single().ok()?;
        let at = match window.position {
            WindowPosition::At(position) => Some(position),
            _ => None,
        };
        let contains = |monitor: &Monitor, at: IVec2| {
            let size = IVec2::new(
                monitor.physical_width as i32,
                monitor.physical_height as i32,
            );
            at.cmpge(monitor.physical_position).all()
                && at.cmplt(monitor.physical_position + size).all()
        };
        let (monitor, _) = at
            .and_then(|at| self.monitors.iter().find(|(m, _)| contains(m, at)))
            .or_else(|| self.monitors.iter().find(|(_, primary)| *primary))
            .or_else(|| self.monitors.iter().next())?;
        monitor
            .refresh_rate_millihertz
            .filter(|mhz| *mhz > 0)
            .map(|mhz| 1_000_000.0 / f64::from(mhz))
    }
}

/// What a window's frame reads besides the records.
struct FrameInputs<'a> {
    frame: u64,
    records: &'a PerfRecords,
    passes: Option<&'a PassRecords>,
    timestamps: Option<Timestamps>,
    counters: &'a PerfCounters,
    store: Option<&'a DiagnosticsStore>,
}

#[expect(clippy::too_many_arguments, reason = "a system's parameters")]
fn drive_samplers(
    mut samplers: Query<(
        Entity,
        &mut Sampler,
        &mut PerfRecordAge,
        Option<&SettleGate>,
    )>,
    frame: Res<PerfFrame>,
    records: Res<PerfRecords>,
    passes: Option<Res<PassRecords>>,
    support: Res<TimestampSupport>,
    counters: Res<PerfCounters>,
    store: Option<Res<DiagnosticsStore>>,
    view: ViewState,
    mut commands: Commands,
) {
    let inputs = FrameInputs {
        frame: frame.0,
        records: &records,
        passes: passes.as_deref(),
        timestamps: support.get(),
        counters: &counters,
        store: store.as_deref(),
    };
    let mut layout = None;
    for (entity, mut sampler, mut age, gate) in &mut samplers {
        if matches!(sampler.phase, Phase::Done) {
            continue;
        }
        if sampler.stop {
            sampler.finish_stopped();
            continue;
        }
        sampler.poll_load_snapshot();
        age.0 = sampler.anchor.elapsed() + RECORD_MARGIN;
        let layout = layout.get_or_insert_with(|| view.layout());
        drive(&mut sampler, entity, &inputs, &view, layout, &mut commands);
        let phase = match sampler.phase {
            Phase::Settling | Phase::Steady { .. } => "settling",
            Phase::Warmup { .. } | Phase::Window(_) | Phase::Done => "sampling",
            Phase::WaitingGpu { .. } => "waiting_gpu",
        };
        let run = (sampler.runs.len() + 1).min(sampler.plan.repeats as usize);
        sampler.progress = Progress {
            phase,
            run,
            of: sampler.plan.repeats,
            busy: gate.map(|gate| gate.progress().busy),
        };
    }
}

fn drive(
    sampler: &mut Sampler,
    entity: Entity,
    inputs: &FrameInputs,
    view: &ViewState,
    layout: &Layout,
    commands: &mut Commands,
) {
    let frame = inputs.frame;
    match &mut sampler.phase {
        Phase::Settling | Phase::Done => {}
        Phase::Steady { from, since } => {
            let (from, since) = (*from, *since);
            let times: Vec<f64> = inputs
                .records
                .main
                .iter()
                .filter(|main| main.record.frame >= from)
                .map(|main| main.record.frame_ms)
                .collect();
            if steady(&times) {
                sampler.enter_warmup(frame);
            } else if since.elapsed() >= STEADY_TIMEOUT {
                sampler.context.warnings.add(
                    "not_steady",
                    format!(
                        "the frame time was still drifting after {} s",
                        STEADY_TIMEOUT.as_secs()
                    ),
                );
                sampler.enter_warmup(frame);
            }
        }
        Phase::Warmup { until } => {
            let until = *until;
            if sampler.last_load.is_none() && sampler.load_task.is_none() {
                sampler.start_load_snapshot();
            }
            if frame + 1 >= until && sampler.last_load.is_some() {
                sampler.open_window(frame);
            }
        }
        Phase::Window(window) => {
            if frame < window.start {
                return;
            }
            match &window.layout {
                None => {
                    window.layout = Some(layout.clone());
                    let context = &mut sampler.context;
                    if context.window.is_none() {
                        context.window = layout.window;
                        context.present_mode = view.window.single().ok().map(|w| w.present_mode);
                        context.refresh_ms = view.refresh_ms();
                    }
                }
                Some(base) if base != layout => {
                    let reason = base.change(layout);
                    let run = sampler.runs.len();
                    if sampler.rerun == Some(run) {
                        sampler.context.warnings.add(
                            "unstable_view",
                            format!("run {} changed again after a re-run: {reason}", run + 1),
                        );
                        window.layout = Some(layout.clone());
                    } else {
                        sampler.rerun = Some(run);
                        sampler.context.reruns.push(Rerun {
                            run: run + 1,
                            reason,
                        });
                        restart(sampler, entity, frame, commands);
                        return;
                    }
                }
                Some(_) => {}
            }
            for view in &layout.views {
                let Some(size) = view.viewport else {
                    continue;
                };
                let largest = window.sizes.entry(view.label.clone()).or_insert(size);
                if u64::from(size.x) * u64::from(size.y)
                    > u64::from(largest.x) * u64::from(largest.y)
                {
                    *largest = size;
                }
            }
            if sampler.plan.detail == Detail::Full
                && let Some(store) = inputs.store
            {
                for diagnostic in store.iter() {
                    let path = diagnostic.path().as_str();
                    if let Some(value) = diagnostic.value()
                        && !path.starts_with(PASS_PREFIX)
                    {
                        window
                            .diagnostics
                            .entry(path.to_owned())
                            .or_default()
                            .push(value);
                    }
                }
            }
            observe_frame(&mut sampler.context, view);

            let done = match sampler.plan.length {
                WindowLength::Frames(frames) => frame + 1 >= window.start + u64::from(frames),
                WindowLength::Time(length) => window.started.elapsed() >= length,
            };
            if done {
                let closed = ClosedWindow {
                    start: window.start,
                    end: frame,
                    seconds: window.started.elapsed().as_secs_f64(),
                    sizes: std::mem::take(&mut window.sizes),
                    diagnostics: (sampler.plan.detail == Detail::Full)
                        .then(|| std::mem::take(&mut window.diagnostics)),
                };
                sampler.phase = Phase::WaitingGpu {
                    window: closed,
                    since: Instant::now(),
                };
                sampler.start_load_snapshot();
            }
        }
        Phase::WaitingGpu { window, since } => {
            let arrived = window_arrived(inputs.records, window.start, window.end);
            let main_in = inputs
                .records
                .main
                .back()
                .is_some_and(|main| main.record.frame >= window.end);
            if sampler.load_task.is_some() || !main_in || !(arrived || since.elapsed() >= GPU_WAIT)
            {
                return;
            }
            let run = collect_run(window, inputs, &mut sampler.context.warnings);
            if run.render_wall_ms.is_empty() {
                sampler.finish(Err(CommandError::failed(
                    "no_frames",
                    "the window rendered no frames, as when it is minimized",
                )
                .with_hint("Show the game window and sample again.")));
                return;
            }
            if inputs.timestamps.is_some_and(|t| !t.views) {
                sampler.context.gpu_available = false;
            }
            sampler.runs.push(run);
            if sampler.runs.len() >= sampler.plan.repeats as usize {
                sampler.finish(Ok(()));
            } else {
                sampler.open_window(frame);
            }
        }
    }
}

/// After a view change: settle again (or warm up again with `skip_settle`), then re-sample.
fn restart(sampler: &mut Sampler, entity: Entity, frame: u64, commands: &mut Commands) {
    let Some(settle) = sampler.plan.settle.clone() else {
        sampler.enter_warmup(frame);
        return;
    };
    sampler.phase = Phase::Settling;
    commands.queue(move |world: &mut World| {
        // The params were checked when the job started, and probes are not unregistered.
        if let Ok(gate) = SettleGate::new(&settle, world)
            && let Ok(mut entity) = world.get_entity_mut(entity)
        {
            entity.insert(gate);
        }
    });
}

/// Folds one window frame's window state into the context.
fn observe_frame(context: &mut Context, view: &ViewState) {
    if let Ok(window) = view.window.single() {
        context.focused &= window.focused;
    }
    if view.occlusion.0 {
        context.occluded = true;
        context.warnings.add(
            "window_occluded",
            "the window was occluded or minimized, and the compositor may throttle its frames",
        );
    }
    if view.overlay.stats {
        context.warnings.add(
            "stats_panel_on",
            "the statistics panel (`overlay.stats`) was on; its UI and text cost is in the numbers",
        );
    }
    if !view.recordings.is_empty() {
        context.warnings.add(
            "video_recording",
            "a video recording ran during the sample; its readback and encoder cost is in the \
             numbers",
        );
    }
}

/// The frame time's last 30-frame median is within 5 % of the 30 frames before.
fn steady(times: &[f64]) -> bool {
    let len = times.len();
    if len < 2 * STEADY_FRAMES {
        return false;
    }
    let recent = median(&times[len - STEADY_FRAMES..]);
    let before = median(&times[len - 2 * STEADY_FRAMES..len - STEADY_FRAMES]);
    match (recent, before) {
        (Some(recent), Some(before)) => (recent - before).abs() <= STEADY_TOLERANCE * before,
        _ => false,
    }
}

/// Every frame of `start..=end` has its CPU record, and its GPU record or loss when it recorded
/// timestamps.
fn window_arrived(records: &PerfRecords, start: u64, end: u64) -> bool {
    let in_window = |frame: u64| (start..=end).contains(&frame);
    let cpu: Vec<_> = records
        .cpu
        .iter()
        .filter(|cpu| in_window(cpu.record.frame))
        .collect();
    if cpu.len() as u64 != end - start + 1 {
        return false;
    }
    let gpu = records
        .gpu
        .iter()
        .filter(|gpu| in_window(gpu.record.frame))
        .count()
        + records
            .gpu_lost
            .iter()
            .filter(|lost| in_window(lost.record))
            .count();
    let recorded = cpu
        .iter()
        .filter(|cpu| cpu.record.gpu == GpuTiming::Recorded)
        .count();
    gpu >= recorded
}

/// Collects a closed window's records into a run.
fn collect_run(window: &mut ClosedWindow, inputs: &FrameInputs, warnings: &mut Warnings) -> Run {
    let records = inputs.records;
    let in_window = |frame: u64| (window.start..=window.end).contains(&frame);
    let main: Vec<_> = records
        .main
        .iter()
        .filter(|main| in_window(main.record.frame))
        .map(|main| &main.record)
        .collect();
    let cpu: Vec<_> = records
        .cpu
        .iter()
        .filter(|cpu| in_window(cpu.record.frame))
        .collect();
    let gpu: Vec<_> = records
        .gpu
        .iter()
        .filter(|gpu| in_window(gpu.record.frame))
        .collect();
    let lost: Vec<u64> = records
        .gpu_lost
        .iter()
        .map(|lost| lost.record)
        .filter(|frame| in_window(*frame))
        .collect();

    let mut run = Run {
        seconds: window.seconds,
        frame_ms: main.iter().map(|main| main.frame_ms).collect(),
        cpu_main_ms: main.iter().map(|main| main.cpu_main_ms).collect(),
        render_wall_ms: cpu.iter().map(|cpu| cpu.record.render_wall_ms).collect(),
        gpu_ms: gpu.iter().map(|gpu| gpu.record.total_ms).collect(),
        gpu_unattributed_ms: gpu
            .iter()
            .map(|gpu| gpu.record.total_ms - gpu.record.views.iter().map(|v| v.ms).sum::<f64>())
            .collect(),
        ..default()
    };

    for cpu in &cpu {
        for (label, (count, ms)) in by_label(&cpu.record.views) {
            let view = run.views.entry(label).or_default();
            view.count = view.count.max(count);
            view.cpu_ms.push(ms);
        }
    }
    for gpu in &gpu {
        for (label, (count, ms)) in by_label(&gpu.record.views) {
            let view: &mut ViewSeries = run.views.entry(label).or_default();
            view.count = view.count.max(count);
            view.gpu_ms.push(ms);
        }
        if gpu.record.untimed_views > 0 {
            warnings.add(
                "views_truncated",
                format!(
                    "{} views in a frame had no GPU timestamps left; their time is only in the \
                     frame total",
                    gpu.record.untimed_views
                ),
            );
        }
    }
    for (label, view) in &mut run.views {
        view.size = window.sizes.get(label.as_ref()).copied();
    }

    let timed: Vec<u64> = gpu
        .iter()
        .map(|gpu| gpu.record.frame)
        .chain(lost.iter().copied())
        .collect();
    run.gpu_frames_skipped = cpu
        .iter()
        .filter(|cpu| cpu.record.gpu == GpuTiming::Skipped)
        .count() as u32
        + lost.len() as u32;
    run.gpu_frames_missing = cpu
        .iter()
        .filter(|cpu| cpu.record.gpu == GpuTiming::Recorded && !timed.contains(&cpu.record.frame))
        .count() as u32;

    run.counters = match window.diagnostics.take() {
        Some(diagnostics) => diagnostics,
        None => {
            let mut counters = BTreeMap::new();
            let entity_count: Vec<f64> = main.iter().filter_map(|m| m.entity_count).collect();
            if !entity_count.is_empty() {
                counters.insert("entity_count".to_owned(), entity_count);
            }
            for (index, path) in inputs.counters.0.iter().enumerate() {
                let values: Vec<f64> = main
                    .iter()
                    .filter_map(|m| m.counters.get(index).copied().flatten())
                    .collect();
                if !values.is_empty() {
                    counters.insert(path.as_str().to_owned(), values);
                }
            }
            counters
        }
    };

    run.passes = match inputs.passes {
        Some(passes) => {
            // Bevy's pass results come back through buffer mapping like our GPU frames, so
            // the groups delivered while the window's GPU frames arrived are the window's.
            let arrivals: Vec<u64> = if gpu.is_empty() {
                cpu.iter().map(|cpu| cpu.arrived).collect()
            } else {
                gpu.iter().map(|gpu| gpu.arrived).collect()
            };
            let first = arrivals.iter().copied().min().unwrap_or(u64::MAX);
            let last = arrivals.iter().copied().max().unwrap_or(0);
            let mut series: BTreeMap<String, PassSeries> = BTreeMap::new();
            for group in passes
                .groups
                .iter()
                .filter(|group| (first..=last).contains(&group.arrived))
            {
                match group.record.warning(inputs.timestamps) {
                    Some(PassWarning::TimestampBudget) => warnings.add(
                        "timestamp_budget",
                        "Bevy's 256 timestamp queries ran out in some frames; passes past them \
                         have CPU time only, and their GPU sums are low",
                    ),
                    Some(PassWarning::PassGpuUnsupported) => warnings.add(
                        "pass_gpu_unsupported",
                        "the device cannot write timestamps inside passes, so passes report \
                         CPU time only",
                    ),
                    None => {}
                }
                for pass in &group.record.passes {
                    let entry = series.entry(pass.path.clone()).or_default();
                    entry.runs.push(f64::from(pass.runs));
                    entry.cpu_ms.push(pass.cpu_ms);
                    if pass.gpu_runs > 0 {
                        entry.gpu_ms.push(pass.gpu_ms);
                    }
                }
            }
            Some(series)
        }
        None => {
            warnings.add(
                "no_render_diagnostics",
                "this build has no `RenderDiagnosticsPlugin`, so there are no pass times",
            );
            None
        }
    };
    run
}

/// One frame's views summed per label, with the number of cameras under each.
fn by_label(views: &[ViewTime]) -> BTreeMap<Arc<str>, (usize, f64)> {
    let mut labels: BTreeMap<Arc<str>, (usize, f64)> = BTreeMap::new();
    for view in views {
        let entry = labels.entry(view.label.clone()).or_default();
        entry.0 += 1;
        entry.1 += view.ms;
    }
    labels
}

fn sampler_settled(settled: On<Settled>, mut samplers: Query<&mut Sampler>, frame: Res<PerfFrame>) {
    if let Ok(mut sampler) = samplers.get_mut(settled.entity) {
        sampler.enter_steady(frame.0);
    }
}

/// A sample of an unsettled scene is still worth having when labelled.
fn sampler_timed_out(
    timeout: On<SettleTimedOut>,
    mut samplers: Query<&mut Sampler>,
    frame: Res<PerfFrame>,
) {
    if let Ok(mut sampler) = samplers.get_mut(timeout.entity) {
        let error = timeout.error();
        let message = match error.hint {
            Some(hint) => format!("{} {hint}", error.message),
            None => error.message,
        };
        sampler.context.warnings.add("not_settled", message);
        sampler.enter_steady(frame.0);
    }
}

/// A `perf.sample` job on its sampler's entity.
#[derive(Component)]
struct SampleJob {
    job: JobHandle<SampleResult>,
    detail: Detail,
}

fn stop_on_cancel(cancel: On<JobCancelRequested>, mut samplers: Query<&mut Sampler>) {
    if let Ok(mut sampler) = samplers.get_mut(cancel.entity) {
        sampler.stop();
    }
}

/// Reports each finished `perf.sample` and despawns its entity, which restores the present mode.
fn finish_sample_jobs(
    mut samplers: Query<(Entity, &mut Sampler, &SampleJob, &JobWork)>,
    mut jobs: ResMut<Jobs>,
    mut commands: Commands,
) {
    for (entity, mut sampler, job, work) in &mut samplers {
        let progress = sampler.progress().clone();
        let Some(outcome) = sampler.take_outcome() else {
            jobs.set_progress(work.job(), progress);
            continue;
        };
        match outcome {
            Ok(sampled) => jobs.finish(&job.job, Ok(report(&sampled, job.detail).0)),
            Err(err) if err.kind == "cancelled" => jobs.cancelled(job.job.id()),
            Err(err) => jobs.finish(&job.job, Err(err)),
        }
        commands.entity(entity).despawn();
    }
}

/// Where samplers advance, in `Last`, after the settle gates resolved.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SampleSystems;

pub(super) fn register(app: &mut App) {
    app.init_resource::<WindowOcclusion>()
        .add_message::<WindowOccluded>()
        .add_observer(sampler_settled)
        .add_observer(sampler_timed_out)
        .add_observer(stop_on_cancel)
        .configure_sets(Last, SampleSystems.after(WaitSystems::Resolve))
        .add_systems(PreUpdate, track_occlusion)
        .add_systems(
            Last,
            (
                drive_samplers.run_if(any_with_component::<Sampler>),
                finish_sample_jobs.run_if(any_with_component::<SampleJob>),
            )
                .chain()
                .in_set(SampleSystems),
        )
        .register_job_command(
            CommandSpec::new(
                "perf.sample",
                "Measure frame, CPU and GPU time per camera and per render pass over a few \
                 windows, with run-to-run noise, hitches and machine-load warnings.",
            ),
            sample,
        );
}

fn sample(
    In(call): In<Call<SampleParams>>,
    world: &mut World,
) -> Result<JobHandle<SampleResult>, CommandError> {
    check_not_busy(world)?;
    let plan = SamplePlan::new(&call.params, DEFAULT_REPEATS, world)?;
    let detail = plan.detail;
    let work = spawn_sampler(world, plan)?;
    let job = match call.start_job::<SampleResult>(world, work, OnCancel::Notify) {
        Ok(job) => job,
        Err(err) => {
            world.despawn(work);
            return Err(err);
        }
    };
    world.entity_mut(work).insert((
        PerfJob,
        SampleJob {
            job: job.clone(),
            detail,
        },
    ));
    Ok(job)
}
