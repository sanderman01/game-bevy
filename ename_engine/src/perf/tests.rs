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

mod demand {
    use std::time::{Duration, Instant};

    use bevy::prelude::*;

    use crate::perf::{
        PerfDemand, PerfDemandHolder, PerfFrame, PerfPlugin,
        records::{MainFrame, PerfRecords, Stamped},
    };

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, PerfPlugin));
        app.finish();
        app.cleanup();
        app
    }

    fn on(app: &App) -> bool {
        app.world().resource::<PerfDemand>().on()
    }

    #[test]
    fn demand_follows_a_holder_entity() {
        let mut app = app();
        app.update();
        assert!(!on(&app));
        assert_eq!(app.world().resource::<PerfFrame>().0, 0);
        assert!(app.world().resource::<PerfRecords>().main.is_empty());

        let holder = app.world_mut().spawn(PerfDemandHolder).id();
        app.update();
        app.update();
        assert!(on(&app));
        assert_eq!(app.world().resource::<PerfFrame>().0, 2);
        let frames: Vec<u64> = app
            .world()
            .resource::<PerfRecords>()
            .main
            .iter()
            .map(|m| m.record.frame)
            .collect();
        assert_eq!(frames, [1, 2]);

        app.world_mut().despawn(holder);
        app.update();
        assert!(!on(&app));
        assert_eq!(app.world().resource::<PerfFrame>().0, 2);
        assert!(app.world().resource::<PerfRecords>().main.is_empty());
    }

    #[test]
    fn demand_follows_the_panel() {
        let mut app = app();
        app.world_mut().resource_mut::<PerfDemand>().panel = true;
        app.update();
        assert!(on(&app));
        // A holder ending while the panel stays on keeps the records.
        let holder = app.world_mut().spawn(PerfDemandHolder).id();
        app.update();
        app.world_mut().despawn(holder);
        app.update();
        assert!(on(&app));
        assert_eq!(app.world().resource::<PerfRecords>().main.len(), 3);

        app.world_mut().resource_mut::<PerfDemand>().panel = false;
        app.update();
        assert!(!on(&app));
    }

    #[test]
    fn records_age_out() {
        let now = Instant::now();
        let frame = |frame, age_ms| Stamped {
            arrived: frame,
            at: now - Duration::from_millis(age_ms),
            record: MainFrame {
                frame,
                frame_ms: 16.0,
                cpu_main_ms: 4.0,
                entity_count: None,
                counters: Vec::new(),
            },
        };
        let mut records = PerfRecords::default();
        records
            .main
            .extend([frame(1, 1500), frame(2, 1001), frame(3, 999), frame(4, 0)]);
        records.prune(now, Duration::from_secs(1));
        let kept: Vec<u64> = records.main.iter().map(|m| m.arrived).collect();
        assert_eq!(kept, [3, 4]);
        assert_eq!(records.keep(), Duration::from_secs(1));
    }
}

mod passes {
    use crate::perf::{
        gpu_timer::Timestamps,
        passes::{PassGroup, PassTime, PassWarning},
    };

    fn group(runs: u32, gpu_runs: u32) -> PassGroup {
        PassGroup {
            passes: vec![PassTime {
                path: "main_opaque_pass_3d".into(),
                runs,
                cpu_ms: 0.5,
                gpu_runs,
                gpu_ms: 1.0,
            }],
        }
    }

    const ALL: Timestamps = Timestamps {
        views: true,
        passes: true,
    };
    const VIEWS_ONLY: Timestamps = Timestamps {
        views: true,
        passes: false,
    };

    #[test]
    fn complete_groups_warn_nothing() {
        assert_eq!(group(2, 2).warning(Some(ALL)), None);
        assert_eq!(group(2, 2).warning(Some(VIEWS_ONLY)), None);
    }

    #[test]
    fn missing_gpu_times_are_budget_or_unsupported_by_device_features() {
        assert_eq!(
            group(3, 1).warning(Some(ALL)),
            Some(PassWarning::TimestampBudget)
        );
        assert_eq!(
            group(3, 0).warning(Some(VIEWS_ONLY)),
            Some(PassWarning::PassGpuUnsupported)
        );
        assert_eq!(group(3, 0).warning(None), None);
    }
}
