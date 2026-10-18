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

    #[test]
    fn a_main_camera_that_is_also_a_host_gets_one_panel() {
        let mut app = app();
        let main = app
            .world_mut()
            .spawn((Camera::default(), MainCamera, StatsPanelHost))
            .id();
        set_stats(&mut app, true);
        assert_eq!(panel_hosts(&mut app), [main]);
        app.update();
        assert_eq!(panel_hosts(&mut app), [main]);
    }

    #[test]
    fn reconcile_keeps_one_panel_per_wanted_host() {
        use crate::perf::panel::reconcile;

        let [a, b, c, p1, p2, p3] =
            [1, 2, 3, 11, 12, 13].map(|index| Entity::from_raw_u32(index).unwrap());
        // `a` wanted twice and doubled, `b` unwanted, `c` new.
        let (despawn, spawn) = reconcile([a, a, c], [(p1, a), (p2, a), (p3, b)]);
        assert_eq!(despawn, [p2, p3]);
        assert_eq!(spawn, [c]);
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
        // Unknown when some client reports no usage.
        let all_measured = load.gpu_top.iter().all(|p| p.gpu_ms.is_some());
        assert_eq!(
            load.gpu_others_ms.is_some(),
            cfg!(target_os = "linux") && all_measured
        );
        assert_eq!(
            load.gpu_others_measured_ms.is_some(),
            cfg!(target_os = "linux") && !all_measured
        );
        assert!(load.gpu_top.iter().all(|p| p.pid != std::process::id()));
        let gpu = load.gpu_others_ms.or(load.gpu_others_measured_ms);
        assert!(!load.machine_busy || load.other_cpu_cores > 0.0 || gpu > Some(0.0));
    }

    #[test]
    fn unmeasured_gpu_use_has_no_total() {
        use crate::perf::load::GpuUse;

        let mut usage = GpuUse::default();
        usage.per_pid.insert(100, 2.0);
        assert_eq!(usage.totals(), (Some(2.0), None));
        usage.unmeasured.insert(400);
        assert_eq!(usage.totals(), (None, Some(2.0)));
    }

    #[test]
    fn the_busiest_load_wins() {
        use crate::perf::load::Load;

        let load = |cores: f64, busy: bool| Load {
            loadavg_1m: None,
            cpus: 8,
            other_cpu_cores: cores,
            top: Vec::new(),
            gpu_others_ms: None,
            gpu_others_measured_ms: None,
            gpu_top: Vec::new(),
            machine_busy: busy,
        };
        let busiest = Load::busiest([load(0.5, false), load(0.9, true), load(0.7, false)]);
        assert_eq!(busiest, Some(load(0.9, true)));
        assert_eq!(Load::busiest([]), None);
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
            assert!(usage.unmeasured.is_empty());
        }

        #[test]
        fn a_new_cycles_client_is_unmeasured() {
            let usage = GpuSnapshot::diff(
                &snapshot(&[]),
                &snapshot(&[(100, &xe(3_000, 20_000))]),
                SECOND,
            );
            assert!(usage.unmeasured.contains(&100));
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
    use bevy::prelude::{Entity, UVec2};
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

    fn layout(window: [u32; 2], camera: u32, viewport: [u32; 2]) -> Layout {
        Layout {
            window: Some(window.into()),
            views: vec![LayoutView {
                camera: Entity::from_raw_u32(camera).unwrap(),
                label: "main_camera".into(),
                viewport: Some(viewport.into()),
                target: Some(window.into()),
            }],
        }
    }

    fn with_layout(layout: Layout) -> Sampled {
        let mut sampled = sampled(vec![run(&[10.0; 3])]);
        sampled.context.layout = Some(layout);
        sampled
    }

    fn warning_kinds(sampled: &Sampled) -> Vec<&'static str> {
        sampled
            .context
            .warnings
            .list()
            .iter()
            .map(|w| w.kind)
            .collect()
    }

    #[test]
    fn steps_under_another_layout_merge_with_a_warning() {
        let mut arm = Sampled::default();
        arm.merge(with_layout(layout([800, 600], 1, [800, 600])));
        // A respawned camera with the same sizes is the same layout.
        arm.merge(with_layout(layout([800, 600], 2, [800, 600])));
        assert!(warning_kinds(&arm).is_empty());
        arm.merge(with_layout(layout([1024, 768], 1, [1024, 768])));
        assert_eq!(arm.runs.len(), 3);
        assert_eq!(warning_kinds(&arm), ["unstable_view"]);
        let (result, _) = report(&arm, Detail::Summary);
        assert_eq!(result.window, Some([800, 600]));
    }

    #[test]
    fn a_run_without_views_or_a_zero_window_rendered_nothing() {
        use crate::perf::sample::rendered_nothing;

        let mut drawn = run(&[10.0; 3]);
        assert!(rendered_nothing(&drawn, Some(UVec2::new(800, 600))));
        drawn.views.insert(
            "main_camera".into(),
            ViewSeries {
                cpu_ms: vec![1.0; 3],
                ..Default::default()
            },
        );
        assert!(!rendered_nothing(&drawn, Some(UVec2::new(800, 600))));
        assert!(!rendered_nothing(&drawn, None));
        assert!(rendered_nothing(&drawn, Some(UVec2::new(0, 0))));
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

mod panel_content {
    use std::time::Instant;

    use bevy::{ecs::system::SystemIdMarker, prelude::*};

    use crate::{
        command::CommandPlugin,
        debug_switch::{DebugSwitchPlugin, DebugSwitchValue, DebugSwitchWorldExt},
        perf::{
            PerfPlugin, StatsCells, StatsFpsSection, StatsPanelHost, StatsPanelOf, StatsRow,
            StatsSceneSection, StatsSection, StatsTimingSection,
            gpu_timer::GpuFrame,
            panel::{
                content::{self, Layout, StatsPanelValues, StatsWarning, StatsWarnings},
                refresh_panels,
            },
            records::{MainFrame, PerfRecords, Stamped},
        },
    };

    /// No feathers: panels get values but no UI, which is all these need.
    fn app() -> (App, Entity) {
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
        let host = app
            .world_mut()
            .spawn((Camera::default(), StatsPanelHost, Name::new("host a")))
            .id();
        app.world_mut()
            .set_debug_switch("overlay.stats", DebugSwitchValue::Bool(true), "test")
            .unwrap();
        app.update();
        (app, host)
    }

    fn layout(app: &mut App) -> Layout {
        content::layout(app.world_mut(), &mut StatsWarnings::default())
    }

    fn name(app: &App, entity: Entity) -> String {
        app.world().get::<Name>(entity).unwrap().to_string()
    }

    /// Section names with their row labels, in panel order.
    fn outline(app: &mut App) -> Vec<(String, Vec<String>)> {
        let layout = layout(app);
        layout
            .0
            .iter()
            .map(|section| {
                let rows = section.rows.iter().map(|row| row.label.to_string());
                (name(app, section.entity), rows.collect())
            })
            .collect()
    }

    fn section<T: Component>(app: &mut App) -> Entity {
        let world = app.world_mut();
        world
            .query_filtered::<Entity, With<T>>()
            .single(world)
            .unwrap()
    }

    fn refresh(app: &mut App) {
        refresh_panels(app.world_mut());
    }

    fn values(app: &mut App, host: Entity) -> &StatsPanelValues {
        let world = app.world_mut();
        let panel = world
            .query::<(Entity, &StatsPanelOf)>()
            .iter(world)
            .find(|(_, of)| of.0 == host)
            .map(|(panel, _)| panel)
            .unwrap();
        app.world().get::<StatsPanelValues>(panel).unwrap()
    }

    fn cells(app: &mut App, host: Entity, row: Entity) -> Option<Vec<String>> {
        values(app, host).row(row).map(<[String]>::to_vec)
    }

    fn warned(app: &App, row: Entity, warning: StatsWarning) -> usize {
        let warnings = &app.world().resource::<StatsWarnings>().0;
        warnings.iter().filter(|w| **w == (row, warning)).count()
    }

    fn text(text: &'static str) -> impl Fn(In<Entity>) -> Option<StatsCells> {
        move |_| Some(StatsCells::one(text))
    }

    fn strings(cells: &[&str]) -> Option<Vec<String>> {
        Some(cells.iter().map(|cell| cell.to_string()).collect())
    }

    #[test]
    fn sections_and_rows_sort_by_order_with_ties_in_child_order() {
        let (mut app, _) = app();
        let world = app.world_mut();
        world.spawn((
            StatsSection::titled("A", 5),
            children![
                StatsRow::system("a1", 10, text("1")),
                StatsRow::system("a2", 0, text("2")),
                StatsRow::system("a3", 10, text("3")),
            ],
        ));
        world.spawn(StatsSection::titled("B", 5));
        world.spawn(StatsSection::titled("C", -1));
        app.update();
        let outline = outline(&mut app);
        let sections: Vec<&str> = outline.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(sections, ["C", "FPS", "A", "B", "Timing", "Scene"]);
        assert_eq!(outline[2].1, ["a2", "a1", "a3"]);
        assert_eq!(outline[4].1, ["Frame", "CPU", "GPU"]);
        assert_eq!(outline[5].1, ["Entities", "View"]);
    }

    #[test]
    fn rows_added_and_despawned_at_runtime_follow() {
        let (mut app, host) = app();
        let scene = section::<StatsSceneSection>(&mut app);
        let row = app
            .world_mut()
            .spawn((StatsRow::system("Extra", 20, text("x")), ChildOf(scene)))
            .id();
        refresh(&mut app);
        assert_eq!(outline(&mut app)[2].1, ["Entities", "View", "Extra"]);
        assert_eq!(cells(&mut app, host, row), strings(&["x"]));

        app.world_mut().despawn(row);
        refresh(&mut app);
        assert_eq!(outline(&mut app)[2].1, ["Entities", "View"]);
        assert_eq!(cells(&mut app, host, row), None);
    }

    #[test]
    fn a_row_moves_with_its_parent() {
        let (mut app, _) = app();
        let scene = section::<StatsSceneSection>(&mut app);
        let fps = section::<StatsFpsSection>(&mut app);
        let row = app
            .world_mut()
            .spawn((StatsRow::system("Moving", 5, text("m")), ChildOf(scene)))
            .id();
        assert_eq!(outline(&mut app)[2].1, ["Entities", "Moving", "View"]);
        app.world_mut().entity_mut(row).insert(ChildOf(fps));
        let outline = outline(&mut app);
        assert_eq!(outline[0].1, ["FPS", "Moving"]);
        assert_eq!(outline[2].1, ["Entities", "View"]);
    }

    #[test]
    fn field_edits_and_child_swaps_change_the_layout() {
        let (mut app, _) = app();
        let section = app
            .world_mut()
            .spawn((
                StatsSection::titled("Edits", 100),
                children![
                    StatsRow::system("one", 0, text("1")),
                    StatsRow::system("two", 0, text("2")),
                ],
            ))
            .id();
        app.update();
        let before = layout(&mut app);
        let rows = &before.0.last().unwrap().rows;
        let (first, second) = (rows[0].entity, rows[1].entity);

        app.world_mut().get_mut::<StatsRow>(first).unwrap().order = 1;
        let reordered = layout(&mut app);
        assert_ne!(reordered, before);
        assert_eq!(outline(&mut app)[3].1, ["two", "one"]);
        app.world_mut().get_mut::<StatsRow>(first).unwrap().order = 0;
        assert_eq!(layout(&mut app), before);

        app.world_mut().get_mut::<StatsRow>(second).unwrap().label = "deux".into();
        assert_ne!(layout(&mut app), before);
        app.world_mut().get_mut::<StatsRow>(second).unwrap().label = "two".into();
        assert_eq!(layout(&mut app), before);

        app.world_mut()
            .get_mut::<Children>(section)
            .unwrap()
            .swap(0, 1);
        assert_ne!(layout(&mut app), before);
        assert_eq!(outline(&mut app)[3].1, ["two", "one"]);
    }

    #[test]
    fn a_despawned_section_takes_its_rows() {
        let (mut app, _) = app();
        let section = app
            .world_mut()
            .spawn((
                StatsSection::titled("Gone", 100),
                children![StatsRow::system("row", 0, text("r"))],
            ))
            .id();
        app.update();
        let row = layout(&mut app).0.last().unwrap().rows[0].entity;
        app.world_mut().despawn(section);
        assert!(app.world().get_entity(row).is_err());
        assert_eq!(outline(&mut app).len(), 3);
    }

    #[test]
    fn removed_rows_leave_no_system_entities() {
        let (mut app, _) = app();
        let scene = section::<StatsSceneSection>(&mut app);
        let count = |app: &mut App| {
            let world = app.world_mut();
            world.query::<&SystemIdMarker>().iter(world).count()
        };
        refresh(&mut app);
        app.update();
        let base = count(&mut app);
        for _ in 0..3 {
            let row = app
                .world_mut()
                .spawn((StatsRow::system("Temp", 50, text("t")), ChildOf(scene)))
                .id();
            refresh(&mut app);
            assert_eq!(count(&mut app), base + 1);
            app.world_mut().despawn(row);
            app.update();
            assert_eq!(count(&mut app), base);
        }
    }

    #[derive(Resource)]
    struct Missing;

    #[test]
    fn a_failing_or_removed_system_hides_its_row_and_warns_once() {
        let (mut app, host) = app();
        let scene = section::<StatsSceneSection>(&mut app);
        let failing = app
            .world_mut()
            .spawn((
                StatsRow::system("Failing", 50, |_: In<Entity>, _: Res<Missing>| {
                    Some(StatsCells::one("never"))
                }),
                ChildOf(scene),
            ))
            .id();
        refresh(&mut app);
        refresh(&mut app);
        assert_eq!(cells(&mut app, host, failing), None);
        assert_eq!(warned(&app, failing, StatsWarning::Failed), 1);

        let systems = |app: &mut App| {
            let world = app.world_mut();
            let mut systems: Vec<Entity> = world
                .query_filtered::<Entity, With<SystemIdMarker>>()
                .iter(world)
                .collect();
            systems.sort();
            systems
        };
        let before = systems(&mut app);
        let removed = app
            .world_mut()
            .spawn((StatsRow::system("Removed", 60, text("r")), ChildOf(scene)))
            .id();
        refresh(&mut app);
        assert_eq!(cells(&mut app, host, removed), strings(&["r"]));
        let added: Vec<Entity> = systems(&mut app)
            .into_iter()
            .filter(|system| !before.contains(system))
            .collect();
        assert_eq!(added.len(), 1);
        app.world_mut().despawn(added[0]);
        refresh(&mut app);
        refresh(&mut app);
        assert_eq!(cells(&mut app, host, removed), None);
        assert_eq!(warned(&app, removed, StatsWarning::Failed), 1);
    }

    #[test]
    fn a_row_can_differ_per_host() {
        let (mut app, a) = app();
        let b = app
            .world_mut()
            .spawn((Camera::default(), StatsPanelHost, Name::new("host b")))
            .id();
        app.update();
        let scene = section::<StatsSceneSection>(&mut app);
        let row = app
            .world_mut()
            .spawn((
                StatsRow::system("Host", 50, |In(host): In<Entity>, names: Query<&Name>| {
                    Some(StatsCells::one(names.get(host).ok()?.as_str()))
                }),
                ChildOf(scene),
            ))
            .id();
        refresh(&mut app);
        assert_eq!(cells(&mut app, a, row), strings(&["host a"]));
        assert_eq!(cells(&mut app, b, row), strings(&["host b"]));
    }

    #[test]
    fn a_cell_count_mismatch_is_joined_into_the_last_column_and_warns_once() {
        let (mut app, host) = app();
        let timing = section::<StatsTimingSection>(&mut app);
        let scene = section::<StatsSceneSection>(&mut app);
        let three = app
            .world_mut()
            .spawn((
                StatsRow::system("Three", 50, |_: In<Entity>| {
                    Some(StatsCells::from(["a", "b", "c"]))
                }),
                ChildOf(timing),
            ))
            .id();
        let two = app
            .world_mut()
            .spawn((
                StatsRow::system("Two", 50, |_: In<Entity>| {
                    Some(StatsCells::from(["x", "y"]))
                }),
                ChildOf(scene),
            ))
            .id();
        refresh(&mut app);
        refresh(&mut app);
        assert_eq!(cells(&mut app, host, three), strings(&["", "a b c"]));
        assert_eq!(cells(&mut app, host, two), strings(&["x y"]));
        assert_eq!(warned(&app, three, StatsWarning::Cells), 1);
        assert_eq!(warned(&app, two, StatsWarning::Cells), 1);
    }

    #[test]
    fn a_section_whose_rows_all_hide_is_hidden() {
        let (mut app, host) = app();
        let section = app
            .world_mut()
            .spawn((
                StatsSection::titled("Quiet", 100).with_header(["", "value"]),
                children![
                    StatsRow::system("none", 0, |_: In<Entity>| None),
                    StatsRow::system("skipped", 10, |_: In<Entity>, _: Single<&Missing>| {
                        Some(StatsCells::one("never"))
                    }),
                ],
            ))
            .id();
        refresh(&mut app);
        let layout = layout(&mut app);
        let quiet = layout.0.iter().find(|s| s.entity == section).unwrap();
        assert!(!values(&mut app, host).shows(quiet));
        // `Skipped` is not a failure.
        assert_eq!(app.world().resource::<StatsWarnings>().0.len(), 0);

        app.world_mut()
            .spawn((StatsRow::system("some", 20, text("1")), ChildOf(section)));
        refresh(&mut app);
        let layout = self::layout(&mut app);
        let quiet = layout.0.iter().find(|s| s.entity == section).unwrap();
        assert!(values(&mut app, host).shows(quiet));
    }

    #[test]
    fn a_row_without_a_section_is_ignored_and_warns_once() {
        let (mut app, host) = app();
        let loose = app
            .world_mut()
            .spawn(StatsRow::system("Loose", 0, text("l")))
            .id();
        let parent = app.world_mut().spawn_empty().id();
        let misplaced = app
            .world_mut()
            .spawn((StatsRow::system("Misplaced", 0, text("m")), ChildOf(parent)))
            .id();
        refresh(&mut app);
        refresh(&mut app);
        let outline = outline(&mut app);
        assert!(outline.iter().all(|(_, rows)| rows.len() <= 3));
        assert_eq!(cells(&mut app, host, loose), None);
        assert_eq!(cells(&mut app, host, misplaced), None);
        assert_eq!(warned(&app, loose, StatsWarning::Unplaced), 1);
        assert_eq!(warned(&app, misplaced, StatsWarning::Unplaced), 1);
    }

    #[test]
    fn built_in_rows_cover_the_last_120_frames() {
        let (mut app, host) = app();
        let at = Instant::now();
        let mut records = app.world_mut().resource_mut::<PerfRecords>();
        records.clear();
        for frame in 0..130_u64 {
            // The first ten fall outside the window.
            let old = frame < 10;
            let record = MainFrame {
                frame,
                frame_ms: if old {
                    1000.0
                } else if frame == 70 {
                    40.0
                } else {
                    10.0
                },
                cpu_main_ms: if frame == 80 { 5.0 } else { 2.0 },
                entity_count: Some(frame as f64),
                counters: Vec::new(),
            };
            records.main.push_back(Stamped {
                arrived: frame,
                at,
                record,
            });
            let record = GpuFrame {
                frame,
                total_ms: if old { 99.0 } else { 1.5 },
                views: Vec::new(),
                untimed_views: 0,
            };
            records.gpu.push_back(Stamped {
                arrived: frame,
                at,
                record,
            });
        }
        refresh(&mut app);
        let row = |app: &mut App, label: &str| {
            let world = app.world_mut();
            world
                .query::<(Entity, &StatsRow)>()
                .iter(world)
                .find(|(_, row)| row.label == label)
                .unwrap()
                .0
        };
        // Mean frame (119 * 10 + 40) / 120 = 10.25 ms: 97.56 FPS, truncated.
        let fps = row(&mut app, "FPS");
        assert_eq!(cells(&mut app, host, fps), strings(&["97"]));
        let frame = row(&mut app, "Frame");
        assert_eq!(cells(&mut app, host, frame), strings(&["10.00", "40.00"]));
        let cpu = row(&mut app, "CPU");
        assert_eq!(cells(&mut app, host, cpu), strings(&["2.00", "5.00"]));
        let gpu = row(&mut app, "GPU");
        assert_eq!(cells(&mut app, host, gpu), strings(&["1.50", "1.50"]));
        let entities = row(&mut app, "Entities");
        assert_eq!(cells(&mut app, host, entities), strings(&["129"]));
    }
}
