//! Sample runs to command results: each series summarized on its own with its hitches split
//! out, views by label, passes by path, counters, `bound` and the warnings the numbers raise.
//!
//! Runs keep raw values; everything is rounded to 0.01 only here, when the result is built.

use std::{collections::BTreeMap, sync::Arc};

use bevy::{math::UVec2, window::PresentMode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    load::Load,
    stats::{Metric, Summary, aggregate_runs, median, split_hitches},
};

/// A frame slower than this times its window's median is a hitch.
const HITCH_FACTOR: f64 = 2.0;
/// More hitches than this fraction of the frames: `hitchy`.
const HITCHY_FRACTION: f64 = 0.05;
/// Passes listed with `detail: "summary"`, by GPU time.
const TOP_PASSES: usize = 10;
/// Hitches listed in `hitches.worst_ms`.
const WORST_HITCHES: usize = 5;
/// A frame median this close to a multiple of the refresh interval is capped by vsync.
const VSYNC_TOLERANCE: f64 = 0.03;
/// Past this many refresh intervals per frame, the 3 % bands cover too much to mean a cap.
const MAX_VSYNC_MULTIPLE: f64 = 4.0;
/// A series this close to the frame median bounds it.
const BOUND_FRACTION: f64 = 0.9;
/// `render` bounds only while the GPU total is under this fraction of the frame.
const RENDER_GPU_FRACTION: f64 = 0.5;

/// How much a result lists.
#[derive(Deserialize, JsonSchema, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Detail {
    /// The top 10 passes by GPU time, `entity_count` and the registered counters.
    #[default]
    Summary,
    /// Every pass and every diagnostic.
    Full,
}

/// One sample window's raw series.
#[derive(Clone, Debug, Default)]
pub(crate) struct Run {
    /// Wall time of the window.
    pub seconds: f64,
    pub frame_ms: Vec<f64>,
    pub cpu_main_ms: Vec<f64>,
    pub render_wall_ms: Vec<f64>,
    pub gpu_ms: Vec<f64>,
    /// Per GPU frame: the total minus the sum of its views.
    pub gpu_unattributed_ms: Vec<f64>,
    pub views: BTreeMap<Arc<str>, ViewSeries>,
    /// `None` without `RenderDiagnosticsPlugin`.
    pub passes: Option<BTreeMap<String, PassSeries>>,
    pub counters: BTreeMap<String, Vec<f64>>,
    pub gpu_frames_missing: u32,
    pub gpu_frames_skipped: u32,
}

/// One label's views in a window, per frame summed over its cameras.
#[derive(Clone, Debug, Default)]
pub(crate) struct ViewSeries {
    /// Most cameras under the label in one frame.
    pub count: usize,
    /// Largest physical viewport under the label.
    pub size: Option<UVec2>,
    pub cpu_ms: Vec<f64>,
    pub gpu_ms: Vec<f64>,
}

/// One pass path in a window, per delivered frame summed over its runs.
#[derive(Clone, Debug, Default)]
pub(crate) struct PassSeries {
    pub runs: Vec<f64>,
    pub cpu_ms: Vec<f64>,
    /// Only frames in which some run had a GPU time.
    pub gpu_ms: Vec<f64>,
}

/// Warnings by kind, each with the first message given for it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Warnings(BTreeMap<&'static str, String>);

impl Warnings {
    pub fn add(&mut self, kind: &'static str, message: impl Into<String>) {
        self.0.entry(kind).or_insert_with(|| message.into());
    }

    #[expect(dead_code, reason = "used by perf.compare")]
    pub fn extend(&mut self, other: &Warnings) {
        for (kind, message) in &other.0 {
            self.add(kind, message.clone());
        }
    }

    pub fn list(&self) -> Vec<Warning> {
        self.0
            .iter()
            .map(|(kind, message)| Warning {
                kind,
                message: message.clone(),
            })
            .collect()
    }
}

/// What a sample saw besides its runs.
#[derive(Clone, Debug, Default)]
pub(crate) struct Context {
    /// Primary window, physical pixels, at the first window.
    pub window: Option<UVec2>,
    /// Focused through every window.
    pub focused: bool,
    /// Occluded at some point of a window.
    pub occluded: bool,
    pub present_mode: Option<PresentMode>,
    pub refresh_ms: Option<f64>,
    /// The device has `TIMESTAMP_QUERY`.
    pub gpu_available: bool,
    pub load: Option<Load>,
    pub warnings: Warnings,
    pub reruns: Vec<Rerun>,
}

/// The runs of a sample, or of one arm of a compare.
#[derive(Clone, Debug, Default)]
pub(crate) struct Sampled {
    pub runs: Vec<Run>,
    pub context: Context,
}

impl Sampled {
    /// Adds a later sample of the same arm: its runs count as more runs.
    #[expect(dead_code, reason = "used by perf.compare")]
    pub fn merge(&mut self, other: Sampled) {
        if self.runs.is_empty() && self.context.window.is_none() {
            *self = other;
            return;
        }
        self.runs.extend(other.runs);
        let context = &mut self.context;
        let other = other.context;
        context.window = context.window.or(other.window);
        context.focused &= other.focused;
        context.occluded |= other.occluded;
        context.present_mode = context.present_mode.or(other.present_mode);
        context.refresh_ms = context.refresh_ms.or(other.refresh_ms);
        context.gpu_available &= other.gpu_available;
        // The busiest step's load, since that is the one that may have disturbed the arm.
        let cores = |load: &Option<Load>| load.as_ref().map_or(-1.0, |l| l.other_cpu_cores);
        if cores(&other.load) > cores(&context.load) {
            context.load = other.load;
        }
        context.warnings.extend(&other.warnings);
        context.reruns.extend(other.reruns);
    }
}

/// A run dropped and sampled again because the view changed during it.
#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub(crate) struct Rerun {
    /// 1-based.
    pub run: usize,
    pub reason: String,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub(crate) struct Warning {
    /// Stable snake_case name, such as `machine_busy` or `vsync_capped`.
    pub kind: &'static str,
    pub message: String,
}

/// A series over runs, unrounded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SeriesStat {
    pub metric: Metric,
    /// Median of the run p95s.
    pub p95: f64,
    /// Largest value kept, over all runs.
    pub max: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewStat {
    pub label: Arc<str>,
    pub count: usize,
    pub size: Option<UVec2>,
    pub cpu: Option<Metric>,
    pub gpu: Option<Metric>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PassStat {
    pub path: String,
    pub runs_per_frame: f64,
    pub cpu_ms: Option<f64>,
    pub gpu: Option<Metric>,
}

/// The headline numbers of a sample, unrounded, for `perf.compare`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Aggregate {
    pub frame_ms: Option<SeriesStat>,
    pub cpu_main_ms: Option<SeriesStat>,
    pub render_wall_ms: Option<SeriesStat>,
    pub gpu_ms: Option<SeriesStat>,
    pub views: Vec<ViewStat>,
    /// Every pass, busiest first; `None` without `RenderDiagnosticsPlugin`.
    #[expect(dead_code, reason = "used by perf.compare")]
    pub passes: Option<Vec<PassStat>>,
}

/// `perf.sample`'s result.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub(crate) struct SampleResult {
    /// Windows sampled.
    pub runs: usize,
    /// Main frames in all windows.
    pub frames: usize,
    pub seconds: f64,
    /// Primary window size, physical pixels.
    pub window: Option<[u32; 2]>,
    pub focused: bool,
    pub occluded: bool,
    /// Such as "fifo" or "auto_no_vsync".
    pub present_mode: Option<&'static str>,
    /// The monitor's refresh interval.
    pub refresh_ms: Option<f64>,
    /// Main loop interval (`Time<Real>::delta`), hitches removed.
    pub frame_ms: Option<SeriesOut>,
    /// Wall time of the main schedules.
    pub cpu_main_ms: Option<SeriesOut>,
    /// Render-world wall time, including swapchain and present waits.
    pub render_wall_ms: Option<SeriesOut>,
    /// GPU time per frame, all views and post-processing; "unavailable" without timestamps.
    pub gpu_ms: Option<Availability<SeriesOut>>,
    /// Per frame, the GPU total minus the sum of its views.
    pub gpu_unattributed_ms: Option<MetricOut>,
    /// Frames whose GPU times never arrived.
    pub gpu_frames_missing: u32,
    /// Frames with no GPU times because every timer slot was in flight or failed to map.
    pub gpu_frames_skipped: u32,
    /// A heuristic: what the frame time waits on.
    pub bound: Bound,
    /// By label, busiest first.
    pub views: Vec<ViewOut>,
    /// Bevy's pass spans, summed per frame over the cameras that ran them; "unavailable"
    /// without `RenderDiagnosticsPlugin`.
    pub passes: Availability<Vec<PassOut>>,
    /// A number, or `{median, min, max}` when it changed during the sample.
    pub counters: BTreeMap<String, CounterOut>,
    /// `frame_ms` frames above twice their window's median.
    pub hitches: HitchesOut,
    /// Hitches the other series dropped.
    pub hitch_counts: HitchCounts,
    /// Other processes' use of the machine over the sample.
    pub load: Option<Load>,
    /// Runs dropped and sampled again because the view changed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reruns: Vec<Rerun>,
    pub warnings: Vec<Warning>,
}

/// A value, or `"unavailable"` where this device or build cannot measure it.
#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub(crate) enum Availability<T> {
    Unavailable(Unavailable),
    Available(T),
}

#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Unavailable {
    Unavailable,
}

#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq)]
pub(crate) struct SeriesOut {
    /// Median of the run medians.
    pub median: f64,
    pub p95: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Range of the run medians; null with one run.
    pub noise: Option<f64>,
}

#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq)]
pub(crate) struct MetricOut {
    pub median: f64,
    pub noise: Option<f64>,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub(crate) struct ViewOut {
    pub label: String,
    /// Most cameras under this label in one frame.
    pub count: usize,
    /// Largest physical viewport size under this label.
    pub size: Option<[u32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_ms: Option<MetricOut>,
    /// Wall time of the camera's render schedule.
    pub cpu_ms: Option<MetricOut>,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub(crate) struct PassOut {
    pub path: String,
    pub runs_per_frame: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_ms: Option<MetricOut>,
    pub cpu_ms: Option<f64>,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub(crate) enum CounterOut {
    Value(f64),
    Changed { median: f64, min: f64, max: f64 },
}

#[derive(Serialize, JsonSchema, Clone, Debug, Default, PartialEq)]
pub(crate) struct HitchesOut {
    pub count: usize,
    /// Median over runs of each run's threshold.
    pub threshold_ms: Option<f64>,
    /// The worst, slowest first.
    pub worst_ms: Vec<f64>,
    /// When each of `worst_ms` happened, in seconds of sampling (runs back to back).
    pub at_seconds: Vec<f64>,
}

#[derive(Serialize, JsonSchema, Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct HitchCounts {
    pub cpu_main_ms: usize,
    pub render_wall_ms: usize,
    pub gpu_ms: usize,
}

/// What the frame time waits on, first match wins.
#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Bound {
    /// Capped at a multiple of the refresh interval.
    Vsync,
    /// The GPU total is at least 90 % of the frame.
    Gpu,
    /// The main schedules are.
    CpuMain,
    /// The render world's wall time is, while the GPU total is under half the frame.
    Render,
    Unclear,
}

/// Summarizes `sampled` as `perf.sample` reports it, and returns the unrounded headline numbers.
pub(crate) fn report(sampled: &Sampled, detail: Detail) -> (SampleResult, Aggregate) {
    let runs = &sampled.runs;
    let context = &sampled.context;
    let mut warnings = context.warnings.clone();

    let frame = Series::of(runs, |run| &run.frame_ms);
    let cpu_main = Series::of(runs, |run| &run.cpu_main_ms);
    let render_wall = Series::of(runs, |run| &run.render_wall_ms);
    let gpu = Series::of(runs, |run| &run.gpu_ms);
    let unattributed = Series::of(runs, |run| &run.gpu_unattributed_ms);

    let mut passes = pass_stats(runs);
    let aggregate = Aggregate {
        frame_ms: frame.stat,
        cpu_main_ms: cpu_main.stat,
        render_wall_ms: render_wall.stat,
        gpu_ms: gpu.stat,
        views: view_stats(runs),
        passes: passes.clone(),
    };
    if detail == Detail::Summary
        && let Some(passes) = &mut passes
    {
        passes.truncate(TOP_PASSES);
    }

    let frame_median = frame.stat.map(|s| s.metric.median);
    let vsync = frame_median.is_some_and(|median| vsync_capped(median, context.refresh_ms));
    if vsync {
        let cause = if is_vsync(context.present_mode) {
            "`uncapped: true` lifts the cap"
        } else {
            "The present mode does not wait for vsync, so the compositor or a frame limiter caps it"
        };
        warnings.add(
            "vsync_capped",
            format!(
                "the frame median sits at a multiple of the {} ms refresh interval, so frame time \
                 cannot show changes; GPU and CPU times still can. {cause}",
                round2(context.refresh_ms.unwrap_or_default())
            ),
        );
    }
    let frames = frame.frames;
    if frames > 0 && frame.hitches.len() as f64 > HITCHY_FRACTION * frames as f64 {
        warnings.add(
            "hitchy",
            format!(
                "{} of {frames} frames were hitches; the hitches are the finding",
                frame.hitches.len()
            ),
        );
    }

    let mut worst = frame.hitches.clone();
    worst.sort_by(|a, b| b.1.total_cmp(&a.1));
    worst.truncate(WORST_HITCHES);

    let result = SampleResult {
        runs: runs.len(),
        frames,
        seconds: round2(runs.iter().map(|run| run.seconds).sum()),
        window: context.window.map(|size| [size.x, size.y]),
        focused: context.focused,
        occluded: context.occluded,
        present_mode: context.present_mode.map(present_mode_name),
        refresh_ms: context.refresh_ms.map(round2),
        frame_ms: frame.stat.map(|s| series_out(s, true)),
        cpu_main_ms: cpu_main.stat.map(|s| series_out(s, false)),
        render_wall_ms: render_wall.stat.map(|s| series_out(s, false)),
        gpu_ms: if context.gpu_available {
            gpu.stat
                .map(|s| Availability::Available(series_out(s, false)))
        } else {
            Some(Availability::Unavailable(Unavailable::Unavailable))
        },
        gpu_unattributed_ms: unattributed.stat.map(|s| metric_out(s.metric)),
        gpu_frames_missing: runs.iter().map(|run| run.gpu_frames_missing).sum(),
        gpu_frames_skipped: runs.iter().map(|run| run.gpu_frames_skipped).sum(),
        bound: bound(&aggregate, vsync),
        views: aggregate
            .views
            .iter()
            .map(|view| ViewOut {
                label: view.label.to_string(),
                count: view.count,
                size: view.size.map(|size| [size.x, size.y]),
                gpu_ms: view.gpu.map(metric_out),
                cpu_ms: view.cpu.map(metric_out),
            })
            .collect(),
        passes: match passes {
            Some(passes) => Availability::Available(
                passes
                    .into_iter()
                    .map(|pass| PassOut {
                        path: pass.path,
                        runs_per_frame: round2(pass.runs_per_frame),
                        gpu_ms: pass.gpu.map(metric_out),
                        cpu_ms: pass.cpu_ms.map(round2),
                    })
                    .collect(),
            ),
            None => Availability::Unavailable(Unavailable::Unavailable),
        },
        counters: counters(runs),
        hitches: HitchesOut {
            count: frame.hitches.len(),
            threshold_ms: median(&frame.thresholds).map(round2),
            worst_ms: worst.iter().map(|hitch| round2(hitch.1)).collect(),
            at_seconds: worst.iter().map(|hitch| round2(hitch.0)).collect(),
        },
        hitch_counts: HitchCounts {
            cpu_main_ms: cpu_main.hitches.len(),
            render_wall_ms: render_wall.hitches.len(),
            gpu_ms: gpu.hitches.len(),
        },
        load: context.load.clone(),
        reruns: context.reruns.clone(),
        warnings: warnings.list(),
    };
    (result, aggregate)
}

/// One series over all runs, with each run's hitches split out against that run's median.
struct Series {
    stat: Option<SeriesStat>,
    /// `(seconds into the sampling, value)` of every hitch.
    hitches: Vec<(f64, f64)>,
    thresholds: Vec<f64>,
    frames: usize,
}

impl Series {
    fn of(runs: &[Run], values: impl Fn(&Run) -> &Vec<f64>) -> Self {
        let mut summaries = Vec::new();
        let mut hitches = Vec::new();
        let mut thresholds = Vec::new();
        let mut frames = 0;
        let mut offset = 0.0;
        for run in runs {
            let values = values(run);
            frames += values.len();
            let (kept, run_hitches) = split_hitches(values, HITCH_FACTOR);
            if let Some(median) = median(values) {
                thresholds.push(median * HITCH_FACTOR);
            }
            for hitch in run_hitches {
                let before: f64 = values[..hitch.index].iter().sum();
                hitches.push((offset + before / 1000.0, hitch.value));
            }
            summaries.extend(Summary::of(&kept));
            offset += run.seconds;
        }
        let stat = aggregate_runs(&summaries).map(|metric| SeriesStat {
            metric,
            p95: median(&summaries.iter().map(|s| s.p95).collect::<Vec<_>>()).unwrap_or_default(),
            max: summaries.iter().map(|s| s.max).fold(f64::MIN, f64::max),
        });
        Self {
            stat,
            hitches,
            thresholds,
            frames,
        }
    }
}

/// The run medians' median and range of a per-run series; runs where it is empty are left out.
fn metric(runs: &[Run], values: impl Fn(&Run) -> Option<&Vec<f64>>) -> Option<Metric> {
    let summaries: Vec<Summary> = runs
        .iter()
        .filter_map(|run| Summary::of(values(run)?))
        .collect();
    aggregate_runs(&summaries)
}

fn view_stats(runs: &[Run]) -> Vec<ViewStat> {
    let mut labels: Vec<&Arc<str>> = runs.iter().flat_map(|run| run.views.keys()).collect();
    labels.sort();
    labels.dedup();
    let mut views: Vec<ViewStat> = labels
        .into_iter()
        .map(|label| {
            let series = || runs.iter().filter_map(|run| run.views.get(label));
            ViewStat {
                label: label.clone(),
                count: series().map(|view| view.count).max().unwrap_or_default(),
                size: series()
                    .filter_map(|view| view.size)
                    .max_by_key(|size| u64::from(size.x) * u64::from(size.y)),
                cpu: metric(runs, |run| run.views.get(label).map(|view| &view.cpu_ms)),
                gpu: metric(runs, |run| run.views.get(label).map(|view| &view.gpu_ms)),
            }
        })
        .collect();
    let busiest = |view: &ViewStat| {
        view.gpu
            .or(view.cpu)
            .map_or(f64::MIN, |metric| metric.median)
    };
    views.sort_by(|a, b| busiest(b).total_cmp(&busiest(a)));
    views
}

fn pass_stats(runs: &[Run]) -> Option<Vec<PassStat>> {
    let all: Vec<&BTreeMap<String, PassSeries>> =
        runs.iter().filter_map(|run| run.passes.as_ref()).collect();
    if all.is_empty() {
        return None;
    }
    let mut paths: Vec<&String> = all.iter().flat_map(|passes| passes.keys()).collect();
    paths.sort();
    paths.dedup();
    let mut passes: Vec<PassStat> = paths
        .into_iter()
        .map(|path| {
            let series = |values: fn(&PassSeries) -> &Vec<f64>| {
                metric(runs, move |run| run.passes.as_ref()?.get(path).map(values))
            };
            PassStat {
                path: path.clone(),
                runs_per_frame: series(|pass| &pass.runs).map_or(0.0, |m| m.median),
                cpu_ms: series(|pass| &pass.cpu_ms).map(|m| m.median),
                gpu: series(|pass| &pass.gpu_ms),
            }
        })
        .collect();
    let busiest = |pass: &PassStat| {
        (
            pass.gpu.map_or(f64::MIN, |metric| metric.median),
            pass.cpu_ms.unwrap_or(f64::MIN),
        )
    };
    passes.sort_by(|a, b| {
        let (a, b) = (busiest(a), busiest(b));
        b.0.total_cmp(&a.0).then(b.1.total_cmp(&a.1))
    });
    Some(passes)
}

fn counters(runs: &[Run]) -> BTreeMap<String, CounterOut> {
    let mut values: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for run in runs {
        for (name, run_values) in &run.counters {
            values
                .entry(name)
                .or_default()
                .extend(run_values.iter().copied());
        }
    }
    values
        .into_iter()
        .filter_map(|(name, values)| {
            let min = values.iter().copied().reduce(f64::min)?;
            let max = values.iter().copied().reduce(f64::max)?;
            let value = if min == max {
                CounterOut::Value(round2(min))
            } else {
                CounterOut::Changed {
                    median: round2(median(&values)?),
                    min: round2(min),
                    max: round2(max),
                }
            };
            Some((name.to_owned(), value))
        })
        .collect()
}

/// The frame median within 3 % of one of the first few multiples of the refresh interval. Not
/// only under a vsync present mode: a compositor can throttle a window to its refresh anyway.
pub(crate) fn vsync_capped(frame_median: f64, refresh_ms: Option<f64>) -> bool {
    let Some(refresh) = refresh_ms.filter(|ms| *ms > 0.0) else {
        return false;
    };
    let multiple = (frame_median / refresh).round();
    (1.0..=MAX_VSYNC_MULTIPLE).contains(&multiple)
        && (frame_median - multiple * refresh).abs() <= VSYNC_TOLERANCE * multiple * refresh
}

fn is_vsync(mode: Option<PresentMode>) -> bool {
    matches!(
        mode,
        Some(PresentMode::AutoVsync | PresentMode::Fifo | PresentMode::FifoRelaxed)
    )
}

pub(crate) fn bound(aggregate: &Aggregate, vsync: bool) -> Bound {
    if vsync {
        return Bound::Vsync;
    }
    let Some(frame) = aggregate.frame_ms.map(|s| s.metric.median) else {
        return Bound::Unclear;
    };
    let median = |stat: Option<SeriesStat>| stat.map(|s| s.metric.median);
    let gpu = median(aggregate.gpu_ms);
    let at_frame = |value: Option<f64>| value.is_some_and(|ms| ms >= BOUND_FRACTION * frame);
    if at_frame(gpu) {
        Bound::Gpu
    } else if at_frame(median(aggregate.cpu_main_ms)) {
        Bound::CpuMain
    } else if at_frame(median(aggregate.render_wall_ms))
        && gpu.is_some_and(|ms| ms < RENDER_GPU_FRACTION * frame)
    {
        Bound::Render
    } else {
        Bound::Unclear
    }
}

pub(crate) fn present_mode_name(mode: PresentMode) -> &'static str {
    match mode {
        PresentMode::AutoVsync => "auto_vsync",
        PresentMode::AutoNoVsync => "auto_no_vsync",
        PresentMode::Fifo => "fifo",
        PresentMode::FifoRelaxed => "fifo_relaxed",
        PresentMode::Immediate => "immediate",
        PresentMode::Mailbox => "mailbox",
    }
}

fn series_out(stat: SeriesStat, with_max: bool) -> SeriesOut {
    SeriesOut {
        median: round2(stat.metric.median),
        p95: round2(stat.p95),
        max: with_max.then(|| round2(stat.max)),
        noise: stat.metric.noise.map(round2),
    }
}

pub(crate) fn metric_out(metric: Metric) -> MetricOut {
    MetricOut {
        median: round2(metric.median),
        noise: metric.noise.map(round2),
    }
}

pub(crate) fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
