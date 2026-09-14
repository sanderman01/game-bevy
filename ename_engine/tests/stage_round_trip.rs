//! Proves the full load path: `open_stage` spawns a container, `WorldInstanceReady` fires once
//! the file's content exists, and the membership-tagging observer stamps `StageId`/
//! `StageMembership`/`StageName` correctly. This is also where a `WorldAssetRoot`-style handle's
//! path-preservation would show up, if it didn't round-trip -- see `scratch/scenes-spec.md` risk
//! #2.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::AssetPlugin,
    ecs::system::RunSystemOnce,
    prelude::*,
    world_serialization::{DynamicWorldBuilder, WorldSerializationPlugin},
};
use ename_engine::stage::{
    DynamicWorldFormat, StageAppExt, StageFormats, StageId, StageMembership, StageName,
    StagePlugin, open_stage, save_stage,
};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Component, Reflect, Clone, PartialEq, Debug, Default)]
#[reflect(Component)]
struct Marker(i32);

/// A fresh temp directory per test run, so nothing lands in the repo and parallel test runs
/// don't collide.
fn temp_stage_dir(test_name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ename_engine_stage_round_trip_{test_name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp stage dir");
    dir
}

fn test_app(asset_root: &std::path::Path) -> App {
    let mut app = App::new();
    app.add_plugins(TaskPoolPlugin::default())
        .add_plugins(AssetPlugin {
            file_path: asset_root.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .add_plugins(WorldSerializationPlugin)
        .add_plugins(StagePlugin)
        .register_stage_format(DynamicWorldFormat)
        .register_type::<Marker>();
    app
}

/// Writes a `.scn.ron` file directly (bypassing `save_stage`, which Task 6 adds) containing one
/// stage-root entity with `StageId` and one child with `Marker`, so this test can exercise
/// `open_stage` on its own.
fn write_fixture_stage(dir: &std::path::Path, id: StageId) -> String {
    let mut world = World::new();
    let root = world.spawn(id).id();
    let child = world.spawn((Marker(7), ChildOf(root))).id();

    let mut type_registry = bevy::reflect::TypeRegistry::default();
    type_registry.register::<StageId>();
    type_registry.register::<Marker>();
    type_registry.register::<ChildOf>();

    let dynamic_world = DynamicWorldBuilder::from_world(&world, &type_registry)
        .extract_entities([root, child].into_iter())
        .build();
    let ron = dynamic_world.serialize(&type_registry).expect("serializes");
    std::fs::write(dir.join("demo.scn.ron"), &ron).expect("writes fixture");
    ron
}

#[test]
fn open_stage_tags_the_stage_root_and_its_descendants() {
    let dir = temp_stage_dir("open");
    let id = StageId(Uuid::new_v4());
    write_fixture_stage(&dir, id);

    let mut app = test_app(&dir);
    app.world_mut()
        .run_system_once(
            |mut commands: Commands, asset_server: Res<AssetServer>, formats: Res<StageFormats>| {
                open_stage(&mut commands, &asset_server, &formats, "demo.scn.ron");
            },
        )
        .expect("system runs");

    // Loading is async: run updates until the marker entity shows up or we give up.
    let mut marker_entity = None;
    for _ in 0..200 {
        app.update();
        if let Ok(entity) = app
            .world_mut()
            .query::<(Entity, &Marker)>()
            .single(app.world())
        {
            marker_entity = Some(entity.0);
            break;
        }
    }
    let marker_entity = marker_entity.expect("stage loads within 200 frames");

    let membership = app
        .world()
        .get::<StageMembership>(marker_entity)
        .expect("descendant is tagged with StageMembership");
    assert_eq!(membership.0, id);

    let mut roots = app.world_mut().query::<(Entity, &StageId, &StageName)>();
    let (root_entity, &root_id, root_name) = roots
        .single(app.world())
        .expect("exactly one entity carries both StageId and StageName");
    assert_eq!(root_id, id);
    assert_eq!(root_name.0, "demo");
    assert!(
        app.world().get::<ChildOf>(root_entity).is_none(),
        "a loaded stage's root must be a genuine top-level entity, not stay parented under the \
         transient load container -- see commands.rs's tag_stage_membership_on_ready Case 1"
    );
}

#[test]
fn save_then_open_preserves_stage_identity_membership_and_marker() {
    let dir = temp_stage_dir("save_open");
    let id = StageId(Uuid::new_v4());
    let path = dir.join("round_trip.scn.ron");

    let mut source_app = test_app(&dir);
    let root = source_app.world_mut().spawn((id, StageMembership(id))).id();
    source_app
        .world_mut()
        .spawn((Marker(7), StageMembership(id), ChildOf(root)));
    save_stage(&path.to_string_lossy(), source_app.world_mut(), id)
        .expect("save_stage writes a stage file");

    let mut loaded_app = test_app(&dir);
    loaded_app
        .world_mut()
        .run_system_once(
            |mut commands: Commands, asset_server: Res<AssetServer>, formats: Res<StageFormats>| {
                open_stage(&mut commands, &asset_server, &formats, "round_trip.scn.ron");
            },
        )
        .expect("open_stage system runs");

    let mut marker_entity = None;
    for _ in 0..200 {
        loaded_app.update();
        if let Ok(entity) = loaded_app
            .world_mut()
            .query::<(Entity, &Marker)>()
            .single(loaded_app.world())
        {
            marker_entity = Some(entity.0);
            break;
        }
    }
    let marker_entity = marker_entity.expect("saved stage loads within 200 frames");
    assert_eq!(
        loaded_app.world().get::<Marker>(marker_entity),
        Some(&Marker(7))
    );

    let root_entity = {
        let mut roots = loaded_app.world_mut().query::<(Entity, &StageId)>();
        let (entity, &root_id) = roots
            .single(loaded_app.world())
            .expect("exactly one loaded root carries StageId");
        assert_eq!(root_id, id);
        entity
    };
    assert_eq!(
        loaded_app.world().get::<StageMembership>(root_entity),
        Some(&StageMembership(id))
    );
    assert_eq!(
        loaded_app.world().get::<StageMembership>(marker_entity),
        Some(&StageMembership(id))
    );
}
