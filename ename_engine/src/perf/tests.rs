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
        PerfDemand, PerfDemandHolder, PerfFrame, PerfOverlay, PerfPlugin,
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
        app.world_mut().resource_mut::<PerfOverlay>().stats = true;
        app.update();
        assert!(on(&app));
        // A holder ending while the panel stays on keeps the records.
        let holder = app.world_mut().spawn(PerfDemandHolder).id();
        app.update();
        app.world_mut().despawn(holder);
        app.update();
        assert!(on(&app));
        assert_eq!(app.world().resource::<PerfRecords>().main.len(), 3);

        app.world_mut().resource_mut::<PerfOverlay>().stats = false;
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

mod panel {
    use bevy::prelude::*;

    use crate::{
        camera::MainCamera,
        command::CommandPlugin,
        debug_switch::{DebugSwitchPlugin, DebugSwitchValue, DebugSwitchWorldExt},
        perf::{PerfPlugin, StatsPanelHost, StatsPanelOf},
    };

    // No `FeathersCorePlugin`: the roots stay empty, which is all the reconcile needs.
    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            CommandPlugin,
            DebugSwitchPlugin,
            PerfPlugin,
        ));
        app.finish();
        app.cleanup();
        app
    }

    fn set_stats(app: &mut App, on: bool) {
        app.world_mut()
            .set_debug_switch("overlay.stats", DebugSwitchValue::Bool(on), "test")
            .unwrap();
        app.update();
    }

    /// Hosts with a panel root, sorted.
    fn panel_hosts(app: &mut App) -> Vec<Entity> {
        let world = app.world_mut();
        let mut hosts: Vec<Entity> = world
            .query::<&StatsPanelOf>()
            .iter(world)
            .map(|of| of.0)
            .collect();
        hosts.sort();
        hosts
    }

    fn root_of(app: &mut App, host: Entity) -> Entity {
        let world = app.world_mut();
        world
            .query::<(Entity, &StatsPanelOf)>()
            .iter(world)
            .find(|(_, of)| of.0 == host)
            .map(|(root, _)| root)
            .unwrap()
    }

    #[test]
    fn panels_follow_the_switch_and_their_hosts() {
        let mut app = app();
        let marked = app
            .world_mut()
            .spawn((Camera::default(), StatsPanelHost))
            .id();
        let main = app.world_mut().spawn((Camera::default(), MainCamera)).id();
        app.world_mut().spawn(Camera::default());
        app.update();
        assert!(panel_hosts(&mut app).is_empty());

        set_stats(&mut app, true);
        let mut both = vec![marked, main];
        both.sort();
        assert_eq!(panel_hosts(&mut app), both);
        // Reconciling again spawns no second root.
        app.update();
        assert_eq!(panel_hosts(&mut app), both);

        app.world_mut()
            .entity_mut(marked)
            .remove::<StatsPanelHost>();
        app.update();
        assert_eq!(panel_hosts(&mut app), [main]);

        // A second main camera: the filter no longer selects one.
        let second = app.world_mut().spawn((Camera::default(), MainCamera)).id();
        app.update();
        assert!(panel_hosts(&mut app).is_empty());
        app.world_mut().despawn(second);
        app.update();
        assert_eq!(panel_hosts(&mut app), [main]);

        let root = root_of(&mut app, main);
        app.world_mut().despawn(root);
        assert!(app.world().get_entity(main).is_ok());

        app.update();
        let root = root_of(&mut app, main);
        app.world_mut().despawn(main);
        assert!(app.world().get_entity(root).is_err());

        let host = app
            .world_mut()
            .spawn((Camera::default(), StatsPanelHost))
            .id();
        app.update();
        assert_eq!(panel_hosts(&mut app), [host]);
        set_stats(&mut app, false);
        assert!(panel_hosts(&mut app).is_empty());
    }
}

mod load {
    use std::time::Duration;

    use crate::perf::load::LoadSnapshot;

    #[test]
    fn a_snapshot_diff_reports_this_machine() {
        let before = LoadSnapshot::take();
        let after = LoadSnapshot::take();
        let load = LoadSnapshot::diff(&before, &after, Duration::from_secs(1));
        assert!(load.cpus >= 1);
        assert!(load.other_cpu_cores >= 0.0);
        assert!(load.top.iter().all(|p| p.pid != std::process::id()));
        assert_eq!(load.loadavg_1m.is_none(), cfg!(windows));
        assert_eq!(load.gpu_others_ms.is_some(), cfg!(target_os = "linux"));
        assert!(load.gpu_top.iter().all(|p| p.pid != std::process::id()));
        assert!(!load.machine_busy || load.other_cpu_cores > 0.0 || load.gpu_others_ms > Some(0.0));
    }

    #[cfg(target_os = "linux")]
    mod drm {
        use std::time::Duration;

        use crate::perf::load::drm::{DrmFdinfo, GpuSnapshot, parse_fdinfo};

        const OWN: u32 = 1;
        const SECOND: Duration = Duration::from_secs(1);

        fn amdgpu(client_id: u64, gfx_ns: u64) -> String {
            format!(
                "pos:\t0\nflags:\t02100002\nmnt_id:\t26\ndrm-driver:\tamdgpu\n\
                 drm-client-id:\t{client_id}\ndrm-pdev:\t0000:03:00.0\npasid:\t32789\n\
                 drm-memory-vram:\t12345 KiB\ndrm-engine-gfx:\t{gfx_ns} ns\n\
                 drm-engine-compute:\t500000 ns\ndrm-engine-capacity-gfx:\t2\n"
            )
        }

        fn xe(busy: u64, total: u64) -> String {
            format!(
                "drm-driver:\txe\ndrm-client-id:\t7\ndrm-pdev:\t0000:00:02.0\n\
                 drm-cycles-rcs:\t{busy}\ndrm-total-cycles-rcs:\t{total}\n\
                 drm-cycles-bcs:\t0\ndrm-total-cycles-bcs:\t{total}\n\
                 drm-engine-capacity-rcs:\t1\n"
            )
        }

        fn info(text: &str) -> DrmFdinfo {
            parse_fdinfo(text).expect("a DRM fdinfo")
        }

        fn snapshot(fds: &[(u32, &str)]) -> GpuSnapshot {
            GpuSnapshot::from_fds(fds.iter().map(|&(pid, text)| (pid, info(text))), OWN)
        }

        #[test]
        fn fdinfo_with_several_engines() {
            let parsed = info(&amdgpu(14, 1_000_000));
            assert_eq!(parsed.pdev, "0000:03:00.0");
            assert_eq!(parsed.client_id, Some(14));
            assert_eq!(parsed.engines_ns.len(), 2);
            assert_eq!(parsed.engines_ns["gfx"], 1_000_000);
            assert_eq!(parsed.engines_ns["compute"], 500_000);
            assert!(parsed.cycles.is_empty());
        }

        #[test]
        fn a_non_drm_fd_is_not_parsed() {
            assert_eq!(parse_fdinfo("pos:\t0\nflags:\t02\nmnt_id:\t26\n"), None);
        }

        #[test]
        fn one_client_through_two_fds_and_two_processes_counts_once() {
            let before = amdgpu(14, 1_000_000);
            let after = amdgpu(14, 3_000_000);
            let before = snapshot(&[(200, &before), (100, &before), (100, &before)]);
            let after = snapshot(&[(100, &after), (200, &after), (100, &after)]);
            let usage = GpuSnapshot::diff(&before, &after, SECOND);
            assert_eq!(usage.per_pid.len(), 1);
            assert!((usage.per_pid[&100] - 2.0).abs() < 1e-9);
        }

        #[test]
        fn our_own_clients_are_skipped() {
            let before = amdgpu(14, 0);
            let after = amdgpu(14, 9_000_000);
            let usage = GpuSnapshot::diff(
                &snapshot(&[(OWN, &before), (300, &before)]),
                &snapshot(&[(OWN, &after), (300, &after)]),
                SECOND,
            );
            assert!(usage.per_pid.is_empty());
        }

        #[test]
        fn a_counter_that_went_down_counts_zero() {
            let usage = GpuSnapshot::diff(
                &snapshot(&[(100, &amdgpu(14, 5_000_000))]),
                &snapshot(&[(100, &amdgpu(14, 2_000_000))]),
                SECOND,
            );
            assert_eq!(usage.per_pid[&100], 0.0);
        }

        #[test]
        fn xe_cycles_give_a_busy_fraction_of_the_window() {
            let parsed = info(&xe(1_000, 10_000));
            assert_eq!(parsed.cycles["rcs"], (1_000, 10_000));
            assert!(parsed.engines_ns.is_empty());
            let usage = GpuSnapshot::diff(
                &snapshot(&[(100, &xe(1_000, 10_000))]),
                &snapshot(&[(100, &xe(3_000, 20_000))]),
                SECOND,
            );
            assert!((usage.per_pid[&100] - 200.0).abs() < 1e-9);
        }

        #[test]
        fn a_driver_without_usage_stats_is_unmeasured() {
            let bare = "drm-driver:\tnouveau\ndrm-pdev:\t0000:01:00.0\n";
            let usage = GpuSnapshot::diff(
                &snapshot(&[(400, bare)]),
                &snapshot(&[(400, bare), (100, &amdgpu(14, 0))]),
                SECOND,
            );
            assert!(usage.unmeasured.contains(&400));
            assert!(!usage.per_pid.contains_key(&400));
            assert_eq!(usage.per_pid[&100], 0.5);
        }
    }
}

mod report {
    use serde_json::json;

    use super::super::report::*;

    fn run(frame_ms: &[f64]) -> Run {
        Run {
            seconds: frame_ms.iter().sum::<f64>() / 1000.0,
            frame_ms: frame_ms.to_vec(),
            cpu_main_ms: frame_ms.iter().map(|ms| ms / 2.0).collect(),
            render_wall_ms: frame_ms.to_vec(),
            gpu_ms: frame_ms.iter().map(|ms| ms * 0.95).collect(),
            ..Default::default()
        }
    }

    fn sampled(runs: Vec<Run>) -> Sampled {
        Sampled {
            runs,
            context: Context {
                gpu_available: true,
                ..Default::default()
            },
        }
    }

    #[test]
    fn hitches_leave_the_statistics_and_keep_their_time() {
        let (result, _) = report(
            &sampled(vec![run(&[10.0, 10.0, 50.0, 10.0]), run(&[12.0; 4])]),
            Detail::Summary,
        );
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["frame_ms"]["max"], json!(12.0));
        assert_eq!(value["frame_ms"]["noise"], json!(2.0));
        assert_eq!(value["hitches"]["count"], json!(1));
        assert_eq!(value["hitches"]["worst_ms"], json!([50.0]));
        assert_eq!(value["hitches"]["at_seconds"], json!([0.02]));
        assert_eq!(value["passes"], json!("unavailable"));
        assert_eq!(value["bound"], json!("gpu"));
    }

    #[test]
    fn one_run_reports_null_noise() {
        let (result, _) = report(&sampled(vec![run(&[10.0; 5])]), Detail::Summary);
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["frame_ms"]["noise"], json!(null));
    }

    #[test]
    fn no_timestamps_reports_gpu_unavailable() {
        let mut sampled = sampled(vec![run(&[10.0; 5])]);
        sampled.context.gpu_available = false;
        let (result, _) = report(&sampled, Detail::Summary);
        assert_eq!(
            serde_json::to_value(&result).unwrap()["gpu_ms"],
            "unavailable"
        );
    }

    #[test]
    fn vsync_cap_needs_one_of_the_first_multiples() {
        assert!(vsync_capped(16.7, Some(16.67)));
        assert!(vsync_capped(33.0, Some(16.67)));
        assert!(!vsync_capped(25.0, Some(16.67)));
        assert!(!vsync_capped(150.0, Some(16.67)));
        assert!(!vsync_capped(16.7, None));
    }

    #[test]
    fn a_changing_counter_reports_its_range() {
        let mut steady = run(&[10.0; 3]);
        steady
            .counters
            .insert("entity_count".into(), vec![5.0, 5.0, 5.0]);
        steady.counters.insert("tiles".into(), vec![1.0, 2.0, 3.0]);
        let (result, _) = report(&sampled(vec![steady]), Detail::Summary);
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["counters"]["entity_count"], json!(5.0));
        assert_eq!(
            value["counters"]["tiles"],
            json!({"median": 2.0, "min": 1.0, "max": 3.0})
        );
    }
}
