//! Frame records: the main world's own frame, and the render world's `CpuFrame` and `GpuFrame`
//! drained from one channel, kept for a bounded time while timing is demanded.

use std::{
    collections::VecDeque,
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

use bevy::{
    app::MainScheduleOrder,
    diagnostic::{DiagnosticPath, DiagnosticsStore, EntityCountDiagnosticsPlugin},
    ecs::schedule::ScheduleLabel,
    platform::cell::SyncCell,
    prelude::*,
};

use super::{
    PerfDemand, PerfDemandHolder, PerfFrame,
    gpu_timer::{CpuFrame, GpuFrame, RenderRecord},
};

/// Records are kept at least this long, so the panel's 0.5 s mean always has its frames.
const MIN_RECORD_AGE: Duration = Duration::from_secs(1);

/// Keeps records for this long while its entity lives, for a consumer that reads a whole window
/// at its end. Holds demand too.
#[derive(Component, Clone, Copy, Debug)]
#[require(PerfDemandHolder)]
pub(crate) struct PerfRecordAge(pub Duration);

/// A record with the main frame and the time it reached the main world.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stamped<T> {
    /// The [`PerfFrame`] in which the main world received it.
    pub arrived: u64,
    pub at: Instant,
    pub record: T,
}

/// The main world's side of one frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MainFrame {
    pub frame: u64,
    /// `Time<Real>::delta`: the main loop's interval, not the cost of this frame's render.
    pub frame_ms: f64,
    /// Wall time of the main schedules, from before the first to after the last.
    pub cpu_main_ms: f64,
    /// Latest `EntityCountDiagnosticsPlugin::ENTITY_COUNT` value.
    pub entity_count: Option<f64>,
    /// Latest value of each [`PerfCounters`] path, in registration order.
    pub counters: Vec<Option<f64>>,
}

/// Every record of the last [`PerfRecords::keep`], oldest first. Empty while nothing demands
/// timing. The panel and the perf jobs read it; nothing else touches the render channel.
#[derive(Resource, Default)]
pub(crate) struct PerfRecords {
    pub main: VecDeque<Stamped<MainFrame>>,
    pub cpu: VecDeque<Stamped<CpuFrame>>,
    pub gpu: VecDeque<Stamped<GpuFrame>>,
    /// Frames whose timestamps failed to map; they count as skipped.
    pub gpu_lost: VecDeque<Stamped<u64>>,
    keep: Duration,
}

impl PerfRecords {
    pub(crate) fn keep(&self) -> Duration {
        self.keep
    }

    pub(super) fn clear(&mut self) {
        self.main.clear();
        self.cpu.clear();
        self.gpu.clear();
        self.gpu_lost.clear();
    }

    fn push(&mut self, arrived: u64, at: Instant, record: RenderRecord) {
        match record {
            RenderRecord::Cpu(record) => self.cpu.push_back(Stamped {
                arrived,
                at,
                record,
            }),
            RenderRecord::Gpu(record) => self.gpu.push_back(Stamped {
                arrived,
                at,
                record,
            }),
            RenderRecord::GpuLost(record) => self.gpu_lost.push_back(Stamped {
                arrived,
                at,
                record,
            }),
        }
    }

    /// Drops records that arrived more than `keep` before `now`.
    pub(super) fn prune(&mut self, now: Instant, keep: Duration) {
        self.keep = keep;
        prune(&mut self.main, now, keep);
        prune(&mut self.cpu, now, keep);
        prune(&mut self.gpu, now, keep);
        prune(&mut self.gpu_lost, now, keep);
    }
}

pub(crate) fn prune<T>(records: &mut VecDeque<Stamped<T>>, now: Instant, keep: Duration) {
    while records
        .front()
        .is_some_and(|record| now.saturating_duration_since(record.at) > keep)
    {
        records.pop_front();
    }
}

/// The main world's end of the render world's record channel.
#[derive(Resource)]
pub(super) struct RecordReceiver(SyncCell<Receiver<RenderRecord>>);

impl RecordReceiver {
    pub(super) fn new(receiver: Receiver<RenderRecord>) -> Self {
        Self(SyncCell::new(receiver))
    }

    /// Discards what the render world sent before the current demand began.
    pub(super) fn discard(&mut self) {
        while self.0.get().try_recv().is_ok() {}
    }
}

/// Diagnostics reported as perf counters, through `PerfAppExt::register_perf_counter`.
#[derive(Resource, Default)]
pub(crate) struct PerfCounters(pub Vec<DiagnosticPath>);

/// Runs before the first main schedule and after the last, for `cpu_main_ms`.
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
struct PerfMainStart;

#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
struct PerfMainEnd;

#[derive(Resource)]
struct MainFrameStart(Instant);

pub(super) fn build(app: &mut App) {
    app.init_resource::<PerfRecords>()
        .init_resource::<PerfCounters>()
        .insert_resource(MainFrameStart(Instant::now()))
        .add_systems(PerfMainStart, mark_main_start)
        .add_systems(PerfMainEnd, record_main_frame.run_if(super::demanded));
}

/// In `finish`, so the markers enclose every schedule other plugins put in the order at build.
pub(super) fn finish(app: &mut App) {
    let mut order = app.world_mut().resource_mut::<MainScheduleOrder>();
    order.labels.insert(0, PerfMainStart.intern());
    order.labels.push(PerfMainEnd.intern());
}

fn mark_main_start(mut start: ResMut<MainFrameStart>) {
    start.0 = Instant::now();
}

fn record_main_frame(
    mut records: ResMut<PerfRecords>,
    frame: Res<PerfFrame>,
    start: Res<MainFrameStart>,
    time: Res<Time<Real>>,
    store: Option<Res<DiagnosticsStore>>,
    counters: Res<PerfCounters>,
) {
    let now = Instant::now();
    let value = |path: &DiagnosticPath| store.as_ref()?.get(path)?.value();
    records.main.push_back(Stamped {
        arrived: frame.0,
        at: now,
        record: MainFrame {
            frame: frame.0,
            frame_ms: time.delta_secs_f64() * 1000.0,
            cpu_main_ms: now.saturating_duration_since(start.0).as_secs_f64() * 1000.0,
            entity_count: value(&EntityCountDiagnosticsPlugin::ENTITY_COUNT),
            counters: counters.0.iter().map(value).collect(),
        },
    });
}

/// The single collector: drains the render channel into [`PerfRecords`] and ages records out.
pub(super) fn collect_records(
    mut records: ResMut<PerfRecords>,
    mut receiver: ResMut<RecordReceiver>,
    frame: Res<PerfFrame>,
    ages: Query<&PerfRecordAge>,
) {
    let now = Instant::now();
    let keep = ages
        .iter()
        .map(|age| age.0)
        .fold(MIN_RECORD_AGE, Duration::max);
    while let Ok(record) = receiver.0.get().try_recv() {
        records.push(frame.0, now, record);
    }
    records.prune(now, keep);
}

/// Keeps `PerfDemand` derived from its sources; on every change the records start empty and
/// what the render world sent before is dropped.
pub(super) fn update_demand(
    holders: Query<(), With<PerfDemandHolder>>,
    mut demand: ResMut<PerfDemand>,
    mut records: ResMut<PerfRecords>,
    mut receiver: ResMut<RecordReceiver>,
) {
    let on = demand.panel || !holders.is_empty();
    if demand.on == on {
        return;
    }
    demand.on = on;
    records.clear();
    receiver.discard();
}
