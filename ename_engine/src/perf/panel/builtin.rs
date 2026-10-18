//! The panel's own rows: FPS, the timing rows, Entities and View, as ordinary content.

use bevy::prelude::*;

use super::{
    PANEL_FRAMES,
    content::{StatsCells, StatsRow, StatsSection, content_root},
};
use crate::perf::{records::PerfRecords, stats::Summary};

/// The built-in section holding the FPS row (order 0).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct StatsFpsSection;

/// The built-in section with the `ms median max` header and the Frame, CPU and GPU rows
/// (orders 0, 10, 20). Section order 10.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct StatsTimingSection;

/// The built-in section with the Entities and View rows (orders 0, 10). Section order 20.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct StatsSceneSection;

/// The last [`PANEL_FRAMES`] frames of [`PerfRecords`], computed once per refresh for the
/// built-in rows.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub(crate) struct PanelSummary {
    /// `1000 /` the mean frame time.
    pub fps: Option<f64>,
    pub frame_ms: Option<Summary>,
    pub cpu_main_ms: Option<Summary>,
    pub gpu_ms: Option<Summary>,
    /// The latest count.
    pub entities: Option<f64>,
}

impl PanelSummary {
    pub(crate) fn of(records: &PerfRecords) -> Self {
        let frame_ms = last_frames(records.main.iter().map(|m| m.record.frame_ms));
        let mean_frame_ms =
            (!frame_ms.is_empty()).then(|| frame_ms.iter().sum::<f64>() / frame_ms.len() as f64);
        Self {
            fps: mean_frame_ms.filter(|ms| *ms > 0.0).map(|ms| 1000.0 / ms),
            frame_ms: Summary::of(&frame_ms),
            cpu_main_ms: Summary::of(&last_frames(
                records.main.iter().map(|m| m.record.cpu_main_ms),
            )),
            gpu_ms: Summary::of(&last_frames(records.gpu.iter().map(|g| g.record.total_ms))),
            entities: records.main.back().and_then(|m| m.record.entity_count),
        }
    }
}

/// The newest [`PANEL_FRAMES`] values of a series, oldest first.
fn last_frames(values: impl DoubleEndedIterator<Item = f64>) -> Vec<f64> {
    let mut last: Vec<f64> = values.rev().take(PANEL_FRAMES).collect();
    last.reverse();
    last
}

pub(super) fn build(app: &mut App) {
    app.init_resource::<PanelSummary>()
        .add_systems(PreStartup, spawn_builtin_content);
}

pub(super) fn update_summary(world: &mut World) {
    let summary = PanelSummary::of(world.resource::<PerfRecords>());
    if *world.resource::<PanelSummary>() != summary {
        world.insert_resource(summary);
    }
}

fn spawn_builtin_content(world: &mut World) {
    let root = content_root(world);
    world.spawn((
        Name::new("FPS"),
        StatsSection::new(0),
        StatsFpsSection,
        ChildOf(root),
        children![StatsRow::system("FPS", 0, fps_cells)],
    ));
    world.spawn((
        Name::new("Timing"),
        StatsSection::new(10).with_header(["ms", "median", "max"]),
        StatsTimingSection,
        ChildOf(root),
        children![
            StatsRow::system("Frame", 0, frame_cells),
            StatsRow::system("CPU", 10, cpu_cells),
            StatsRow::system("GPU", 20, gpu_cells),
        ],
    ));
    world.spawn((
        Name::new("Scene"),
        StatsSection::new(20),
        StatsSceneSection,
        ChildOf(root),
        children![
            StatsRow::system("Entities", 0, entity_cells),
            StatsRow::system("View", 10, view_cells),
        ],
    ));
}

fn fps_cells(_host: In<Entity>, summary: Res<PanelSummary>) -> Option<StatsCells> {
    summary
        .fps
        .map(|fps| StatsCells::one(format!("{}", fps.trunc())))
}

fn timing_cells(summary: Option<Summary>) -> Option<StatsCells> {
    summary.map(|s| StatsCells::from([format!("{:.2}", s.median), format!("{:.2}", s.max)]))
}

fn frame_cells(_host: In<Entity>, summary: Res<PanelSummary>) -> Option<StatsCells> {
    timing_cells(summary.frame_ms)
}

fn cpu_cells(_host: In<Entity>, summary: Res<PanelSummary>) -> Option<StatsCells> {
    timing_cells(summary.cpu_main_ms)
}

/// Absent without `TIMESTAMP_QUERY`, which hides the row.
fn gpu_cells(_host: In<Entity>, summary: Res<PanelSummary>) -> Option<StatsCells> {
    timing_cells(summary.gpu_ms)
}

fn entity_cells(_host: In<Entity>, summary: Res<PanelSummary>) -> Option<StatsCells> {
    summary
        .entities
        .map(|count| StatsCells::one(format!("{count:.0}")))
}

fn view_cells(In(host): In<Entity>, cameras: Query<&Camera>) -> Option<StatsCells> {
    let size = cameras.get(host).ok()?.physical_viewport_size()?;
    Some(StatsCells::one(format!("{} × {}", size.x, size.y)))
}
