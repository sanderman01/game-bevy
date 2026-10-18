//! Performance sampling: frame, CPU and GPU times per camera, summarized against run-to-run noise.
//!
//! Timing runs only while something demands it: an entity with [`PerfDemandHolder`] (a perf job's
//! work entity) or the statistics panel. Off, only the two main-schedule markers and the
//! render world's extraction check run.

mod gpu_timer;
mod passes;
mod records;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by perf.sample and perf.compare")
)]
mod stats;

#[cfg(test)]
mod tests;

use std::{borrow::Cow, sync::mpsc::channel};

use bevy::{diagnostic::DiagnosticPath, prelude::*, render::diagnostic::RenderDiagnosticsPlugin};

use gpu_timer::TimestampSupport;
use records::{PerfCounters, RecordReceiver};

/// Groups cameras under one label in perf results, such as every face of a cubemap probe.
/// Without one, the `MainCameraFilter` camera is `main_camera`, others go by `Name`, else
/// `camera <order>`.
#[derive(Component, Reflect, Clone, Debug)]
#[reflect(Component)]
pub struct PerfViewLabel(pub Cow<'static, str>);

/// Keeps perf timing and records on while it exists. A perf job puts it on its work entity.
#[derive(Component, Reflect, Clone, Copy, Debug, Default)]
#[reflect(Component, Default)]
pub struct PerfDemandHolder;

pub trait PerfAppExt {
    /// Reports this diagnostic in `counters`. Settling is AGENTDX_04's probes, not this.
    fn register_perf_counter(&mut self, path: DiagnosticPath) -> &mut Self;
}

impl PerfAppExt for App {
    fn register_perf_counter(&mut self, path: DiagnosticPath) -> &mut Self {
        let mut counters = self.world_mut().get_resource_or_init::<PerfCounters>();
        if !counters.0.contains(&path) {
            counters.0.push(path);
        }
        self
    }
}

/// Whether timing is on, derived in `First` from its sources each frame.
#[derive(Resource, Default)]
pub(crate) struct PerfDemand {
    /// The statistics panel's source.
    pub(crate) panel: bool,
    on: bool,
}

impl PerfDemand {
    pub(crate) fn on(&self) -> bool {
        self.on
    }
}

/// Counts main frames while timing is on; every record carries the id of the frame it belongs to.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PerfFrame(pub(crate) u64);

fn demanded(demand: Res<PerfDemand>) -> bool {
    demand.on
}

fn advance_frame(mut frame: ResMut<PerfFrame>) {
    frame.0 += 1;
}

/// The engine's perf timing: [`PerfFrame`], the render world's GPU timer, the frame records and
/// [`PerfViewLabel`]. In `EnginePlugins`, so editor and release builds both have it.
pub struct PerfPlugin;

impl Plugin for PerfPlugin {
    fn build(&self, app: &mut App) {
        let (sender, receiver) = channel();
        let support = TimestampSupport::default();
        gpu_timer::build(app, sender, support.clone());
        records::build(app);
        app.register_type::<PerfViewLabel>()
            .register_type::<PerfDemandHolder>()
            .init_resource::<PerfDemand>()
            .init_resource::<PerfFrame>()
            .insert_resource(support)
            .insert_resource(RecordReceiver::new(receiver))
            .add_systems(
                First,
                (
                    records::update_demand,
                    (advance_frame, records::collect_records)
                        .chain()
                        .run_if(demanded),
                )
                    .chain()
                    .in_set(PerfSystems),
            );
    }

    fn finish(&self, app: &mut App) {
        records::finish(app);
    }
}

/// Where demand is derived, [`PerfFrame`] advances and the records are collected, in `First`.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PerfSystems;

/// The perf commands and Bevy's pass timings grouped per frame. Editor builds only, after
/// [`PerfPlugin`]. Never adds `RenderDiagnosticsPlugin`; without it, passes are unavailable.
pub struct PerfCommandsPlugin;

impl Plugin for PerfCommandsPlugin {
    fn build(&self, _app: &mut App) {}

    fn finish(&self, app: &mut App) {
        assert!(
            app.is_plugin_added::<PerfPlugin>(),
            "PerfCommandsPlugin needs PerfPlugin (in EnginePlugins)"
        );
        if app.is_plugin_added::<RenderDiagnosticsPlugin>() {
            app.init_resource::<passes::PassRecords>().add_systems(
                Last,
                passes::collect_passes.run_if(any_with_component::<PerfDemandHolder>),
            );
        }
    }
}
