//! Frame statistics: summaries, the hitch split, run aggregation and the A/B verdict. Pure.
//!
//! Values are times in ms, so lower is faster.

use schemars::JsonSchema;
use serde::Serialize;

/// One window's series, hitches already removed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Summary {
    pub median: f64,
    /// Nearest rank, so always a value of the series.
    pub p95: f64,
    pub max: f64,
}

impl Summary {
    /// `None` for an empty series.
    pub fn of(values: &[f64]) -> Option<Self> {
        let sorted = sorted(values);
        Some(Self {
            median: median_of_sorted(&sorted)?,
            p95: percentile_of_sorted(&sorted, 0.95)?,
            max: *sorted.last()?,
        })
    }
}

/// A frame dropped from the statistics for being slower than the window's median times the factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Hitch {
    /// Position in the window's series.
    pub index: usize,
    pub value: f64,
}

/// Several runs of one metric: the median of the run medians and their range.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(crate) struct Metric {
    pub median: f64,
    /// Largest run median minus the smallest; `None` with a single run, where it is unknown.
    pub noise: Option<f64>,
}

/// How arm B compares with arm A.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Verdict {
    BFaster,
    BSlower,
    WithinNoise,
}

/// The median of `values`; the mean of the middle two for an even count. `None` when empty.
pub(crate) fn median(values: &[f64]) -> Option<f64> {
    median_of_sorted(&sorted(values))
}

/// Splits `values` into the frames kept for statistics and the hitches: frames above `factor`
/// times the median of all of `values`. Order is kept in both.
pub(crate) fn split_hitches(values: &[f64], factor: f64) -> (Vec<f64>, Vec<Hitch>) {
    let Some(median) = median(values) else {
        return (Vec::new(), Vec::new());
    };
    let threshold = median * factor;
    let mut kept = Vec::with_capacity(values.len());
    let mut hitches = Vec::new();
    for (index, &value) in values.iter().enumerate() {
        if value > threshold {
            hitches.push(Hitch { index, value });
        } else {
            kept.push(value);
        }
    }
    (kept, hitches)
}

/// Aggregates per-run summaries into one metric. `None` without runs.
pub(crate) fn aggregate_runs(runs: &[Summary]) -> Option<Metric> {
    let medians: Vec<f64> = runs.iter().map(|run| run.median).collect();
    let sorted = sorted(&medians);
    Some(Metric {
        median: median_of_sorted(&sorted)?,
        noise: (sorted.len() > 1).then(|| sorted[sorted.len() - 1] - sorted[0]),
    })
}

/// B against A: within noise when the difference of medians is at most the larger noise of the
/// two. An unknown noise counts as zero.
pub(crate) fn verdict(a: Metric, b: Metric) -> Verdict {
    let diff = b.median - a.median;
    let noise = a.noise.unwrap_or(0.0).max(b.noise.unwrap_or(0.0));
    // The tolerance absorbs float error when both sides are rounded to 0.01.
    if diff.abs() <= noise + 1e-9 {
        Verdict::WithinNoise
    } else if diff < 0.0 {
        Verdict::BFaster
    } else {
        Verdict::BSlower
    }
}

fn sorted(values: &[f64]) -> Vec<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted
}

fn median_of_sorted(sorted: &[f64]) -> Option<f64> {
    let mid = sorted.len() / 2;
    match sorted.len() {
        0 => None,
        len if len % 2 == 1 => Some(sorted[mid]),
        _ => Some((sorted[mid - 1] + sorted[mid]) / 2.0),
    }
}

/// Nearest-rank percentile, `p` in 0..=1.
fn percentile_of_sorted(sorted: &[f64], p: f64) -> Option<f64> {
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted.get(rank.max(1) - 1).copied()
}
