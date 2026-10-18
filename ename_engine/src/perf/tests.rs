use super::stats::*;

fn summary(median: f64) -> Summary {
    Summary {
        median,
        p95: median,
        max: median,
    }
}

#[test]
fn odd_median_is_the_middle_value() {
    assert_eq!(median(&[5.0, 1.0, 3.0]), Some(3.0));
}

#[test]
fn even_median_is_the_mean_of_the_middle_two() {
    assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
}

#[test]
fn empty_series_has_no_summary() {
    assert_eq!(median(&[]), None);
    assert_eq!(Summary::of(&[]), None);
    assert_eq!(aggregate_runs(&[]), None);
}

#[test]
fn summary_uses_nearest_rank_p95() {
    let values: Vec<f64> = (1..=20).map(f64::from).collect();
    let summary = Summary::of(&values).unwrap();
    assert_eq!(summary.median, 10.5);
    assert_eq!(summary.p95, 19.0);
    assert_eq!(summary.max, 20.0);
    assert_eq!(Summary::of(&[7.0]).unwrap().p95, 7.0);
}

#[test]
fn hitches_are_split_out_in_order() {
    let (kept, hitches) = split_hitches(&[10.0, 30.0, 10.0, 11.0, 50.0, 9.0], 2.0);
    assert_eq!(kept, [10.0, 10.0, 11.0, 9.0]);
    assert_eq!(
        hitches,
        [
            Hitch {
                index: 1,
                value: 30.0
            },
            Hitch {
                index: 4,
                value: 50.0
            },
        ]
    );
}

#[test]
fn a_frame_at_the_threshold_is_kept() {
    let (kept, hitches) = split_hitches(&[10.0, 10.0, 20.0], 2.0);
    assert_eq!(kept.len(), 3);
    assert!(hitches.is_empty());
}

#[test]
fn a_uniformly_slow_window_has_no_hitches() {
    // Hitches are relative to the window's own median, so a window of only slow frames keeps them.
    let (kept, hitches) = split_hitches(&[300.0, 840.0, 500.0], 2.0);
    assert_eq!(kept.len(), 3);
    assert!(hitches.is_empty());
}

#[test]
fn an_empty_window_splits_into_nothing() {
    let (kept, hitches) = split_hitches(&[], 2.0);
    assert!(kept.is_empty() && hitches.is_empty());
}

#[test]
fn one_run_has_unknown_noise() {
    let metric = aggregate_runs(&[summary(16.9)]).unwrap();
    assert_eq!(metric.median, 16.9);
    assert_eq!(metric.noise, None);
}

#[test]
fn runs_aggregate_to_median_and_range() {
    let metric = aggregate_runs(&[summary(17.0), summary(16.5), summary(17.5)]).unwrap();
    assert_eq!(metric.median, 17.0);
    assert_eq!(metric.noise, Some(1.0));
    let metric = aggregate_runs(&[summary(2.0), summary(2.0)]).unwrap();
    assert_eq!(metric.noise, Some(0.0));
}

#[test]
fn verdict_against_the_larger_noise() {
    let metric = |median, noise| Metric { median, noise };
    assert_eq!(
        verdict(metric(16.9, Some(0.4)), metric(14.0, Some(0.2))),
        Verdict::BFaster
    );
    assert_eq!(
        verdict(metric(14.0, Some(0.2)), metric(16.9, Some(0.4))),
        Verdict::BSlower
    );
    assert_eq!(
        verdict(metric(16.9, Some(0.1)), metric(16.6, Some(0.5))),
        Verdict::WithinNoise
    );
}

#[test]
fn verdict_ties_are_within_noise() {
    let metric = |median, noise| Metric { median, noise };
    // A difference equal to the noise is not a change.
    assert_eq!(
        verdict(metric(10.0, Some(0.5)), metric(10.5, Some(0.25))),
        Verdict::WithinNoise
    );
    // Equal medians are within noise even when the noise is unknown.
    assert_eq!(
        verdict(metric(10.0, None), metric(10.0, None)),
        Verdict::WithinNoise
    );
    assert_eq!(
        verdict(metric(10.0, None), metric(10.1, None)),
        Verdict::BSlower
    );
}

#[test]
fn verdict_serializes_snake_case() {
    assert_eq!(
        serde_json::to_value(Verdict::WithinNoise).unwrap(),
        "within_noise"
    );
}
