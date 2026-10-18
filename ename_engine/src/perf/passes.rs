//! Bevy's per-pass render diagnostics, grouped per delivered frame.
//!
//! `RenderDiagnosticsPlugin` stamps every measurement of a delivered frame with the one `Instant`
//! of its sync and carries no frame id, so measurements are grouped by that instant. A pass that
//! ran for several cameras has one measurement per run under the same path; a group sums them
//! with the run count. Frames Bevy drops in its single-slot mutex are simply missing.

use std::{
    collections::{BTreeMap, VecDeque},
    time::Instant,
};

use bevy::{diagnostic::DiagnosticsStore, prelude::*};

use super::{
    PerfDemand, PerfFrame,
    gpu_timer::Timestamps,
    records::{PerfRecords, Stamped, prune},
};

const PATH_PREFIX: &str = "render/";
const CPU_SUFFIX: &str = "/elapsed_cpu";
const GPU_SUFFIX: &str = "/elapsed_gpu";

/// One span path's runs in one delivered frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PassTime {
    /// The span path without `render/` and the field, such as `main_opaque_pass_3d`.
    pub path: String,
    pub runs: u32,
    pub cpu_ms: f64,
    /// Runs that also have a GPU time; fewer than `runs` when timestamps were missing.
    pub gpu_runs: u32,
    pub gpu_ms: f64,
}

/// Every pass of one delivered frame, by path.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PassGroup {
    pub passes: Vec<PassTime>,
}

/// Why a pass has CPU but not GPU times.
#[cfg_attr(not(test), expect(dead_code, reason = "read by perf.sample"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PassWarning {
    /// The device has the timestamp features, so Bevy's 256 queries ran out.
    TimestampBudget,
    /// The device cannot write timestamps inside encoders or passes.
    PassGpuUnsupported,
}

impl PassGroup {
    /// `None` when every run has a GPU time, or before the device's features are known.
    #[cfg_attr(not(test), expect(dead_code, reason = "read by perf.sample"))]
    pub(crate) fn warning(&self, timestamps: Option<Timestamps>) -> Option<PassWarning> {
        let missing = self.passes.iter().any(|pass| pass.gpu_runs < pass.runs);
        match timestamps? {
            _ if !missing => None,
            Timestamps { passes: true, .. } => Some(PassWarning::TimestampBudget),
            Timestamps { passes: false, .. } => Some(PassWarning::PassGpuUnsupported),
        }
    }
}

/// Groups of the last [`PerfRecords::keep`], oldest first; `arrived` is the main frame of the
/// sync. Exists only when `RenderDiagnosticsPlugin` is added.
#[derive(Resource, Default)]
pub(crate) struct PassRecords {
    pub groups: VecDeque<Stamped<PassGroup>>,
    last_sync: Option<Instant>,
    /// The frame this last ran in; a gap means the jobs' demand ended in between.
    last_frame: Option<u64>,
}

/// Groups the newest sync's measurements. Runs every frame while a perf job holds demand; only
/// the newest sync is taken, since Bevy syncs at most once per frame.
pub(super) fn collect_passes(
    store: Res<DiagnosticsStore>,
    frame: Res<PerfFrame>,
    records: Res<PerfRecords>,
    demand: Res<PerfDemand>,
    mut passes: ResMut<PassRecords>,
) {
    // A new demand starts empty, like `PerfRecords`.
    if demand.is_changed() || passes.last_frame.is_some_and(|last| last + 1 != frame.0) {
        passes.groups.clear();
    }
    passes.last_frame = Some(frame.0);
    let now = Instant::now();
    prune(&mut passes.groups, now, records.keep());

    let spans: Vec<_> = store
        .iter()
        .filter_map(|diagnostic| span_of(diagnostic.path().as_str()).map(|span| (span, diagnostic)))
        .collect();
    let Some(newest) = spans
        .iter()
        .filter_map(|(_, diagnostic)| diagnostic.measurement().map(|m| m.time))
        .max()
    else {
        return;
    };
    if passes.last_sync.is_some_and(|last| newest <= last) {
        return;
    }
    passes.last_sync = Some(newest);

    let mut by_path: BTreeMap<&str, PassTime> = BTreeMap::new();
    for ((path, gpu), diagnostic) in spans {
        let pass = by_path.entry(path).or_insert_with(|| PassTime {
            path: path.to_owned(),
            runs: 0,
            cpu_ms: 0.0,
            gpu_runs: 0,
            gpu_ms: 0.0,
        });
        for measurement in diagnostic.measurements().filter(|m| m.time == newest) {
            if gpu {
                pass.gpu_runs += 1;
                pass.gpu_ms += measurement.value;
            } else {
                pass.runs += 1;
                pass.cpu_ms += measurement.value;
            }
        }
    }
    let group = PassGroup {
        passes: by_path
            .into_values()
            .filter(|pass| pass.runs > 0 || pass.gpu_runs > 0)
            .collect(),
    };
    passes.groups.push_back(Stamped {
        arrived: frame.0,
        at: newest,
        record: group,
    });
}

/// `(span path, is GPU)` for a Bevy pass timing path.
fn span_of(path: &str) -> Option<(&str, bool)> {
    let span = path.strip_prefix(PATH_PREFIX)?;
    if let Some(span) = span.strip_suffix(CPU_SUFFIX) {
        Some((span, false))
    } else {
        span.strip_suffix(GPU_SUFFIX).map(|span| (span, true))
    }
}
