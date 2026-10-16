//! The `stage` settle probe counts a stage file from `open_stage` until its content has spawned.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{AssetPlugin, LoadState},
    prelude::*,
    world_serialization::{DynamicWorldBuilder, DynamicWorldRoot, WorldSerializationPlugin},
};
use ename_engine::{
    stage::{AssetRoot, StageId, StageLoading, StagePlugin, open_stage},
    wait::{SettleSample, SettleWaiter, WaitPlugin},
};
use uuid::Uuid;

fn test_app(asset_root: &std::path::Path) -> App {
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        AssetPlugin {
            file_path: asset_root.to_string_lossy().into_owned(),
            ..default()
        },
        WorldSerializationPlugin,
        StagePlugin,
        WaitPlugin,
    ))
    .insert_resource(AssetRoot(asset_root.to_owned()));
    app.world_mut().spawn(SettleWaiter);
    app
}

fn write_fixture_stage(dir: &std::path::Path, name: &str) {
    let mut world = World::new();
    let root = world.spawn(StageId(Uuid::new_v4())).id();
    let mut registry = bevy::reflect::TypeRegistry::default();
    registry.register::<StageId>();
    let ron = DynamicWorldBuilder::from_world(&world, &registry)
        .extract_entity(root)
        .build()
        .serialize(&registry)
        .expect("serializes");
    std::fs::write(dir.join(name), ron).expect("writes fixture");
}

fn stages_loading(app: &App) -> u32 {
    app.world()
        .resource::<SettleSample>()
        .busy
        .get("stage")
        .and_then(|activity| {
            activity
                .counters()
                .find(|(name, _)| *name == "stages_loading")
        })
        .map_or(0, |(_, count)| count)
}

fn update_until(app: &mut App, mut done: impl FnMut(&mut App) -> bool) {
    for _ in 0..500 {
        app.update();
        if done(app) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("did not happen within 500 frames");
}

#[test]
fn a_stage_counts_until_its_content_has_spawned() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture_stage(dir.path(), "target.scn.ron");
    let mut app = test_app(dir.path());
    app.update();

    let container = open_stage(app.world_mut(), "target.scn.ron");
    // Only `Last`: the probe samples before anything could have spawned the content.
    app.world_mut().run_schedule(Last);
    assert_eq!(stages_loading(&app), 1);

    update_until(&mut app, |app| {
        let loading = app.world().get::<StageLoading>(container).is_some();
        assert_eq!(stages_loading(app), u32::from(loading));
        !loading
    });
    app.update();
    assert_eq!(stages_loading(&app), 0);
    assert!(
        app.world().get_entity(container).is_ok(),
        "the container stays"
    );
}

#[test]
fn a_container_whose_file_failed_does_not_count() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path());
    let container = open_stage(app.world_mut(), "missing.scn.ron");
    update_until(&mut app, |app| {
        let id = app.world().get::<DynamicWorldRoot>(container).unwrap().id();
        matches!(
            app.world().resource::<AssetServer>().get_load_state(id),
            Some(LoadState::Failed(_))
        )
    });
    app.update();
    assert!(app.world().get::<StageLoading>(container).is_some());
    assert_eq!(stages_loading(&app), 0);
}
