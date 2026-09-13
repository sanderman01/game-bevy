//! Proves the full load path: `open_scene` spawns a container, `WorldInstanceReady` fires once
//! the file's content exists, and the membership-tagging observer stamps `SceneId`/
//! `SceneMembership`/`SceneName` correctly. This is also where a `WorldAssetRoot`-style handle's
//! path-preservation would show up, if it didn't round-trip -- see `scratch/scenes-spec.md` risk
//! #2.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::AssetPlugin,
    ecs::system::RunSystemOnce,
    prelude::*,
    world_serialization::{DynamicWorldBuilder, WorldSerializationPlugin},
};
use ename_engine::scene::{
    DynamicWorldFormat, SceneAppExt, SceneFormats, SceneId, SceneMembership, SceneName,
    ScenePlugin, open_scene, save_scene,
};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Component, Reflect, Clone, PartialEq, Debug, Default)]
#[reflect(Component)]
struct Marker(i32);

/// A fresh temp directory per test run, so nothing lands in the repo and parallel test runs
/// don't collide.
fn temp_scene_dir(test_name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ename_engine_scene_round_trip_{test_name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp scene dir");
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
        .add_plugins(ScenePlugin)
        .register_scene_format(DynamicWorldFormat)
        .register_type::<Marker>();
    app
}

/// Writes a `.scn.ron` file directly (bypassing `save_scene`, which Task 6 adds) containing one
/// scene-root entity with `SceneId` and one child with `Marker`, so this test can exercise
/// `open_scene` on its own.
fn write_fixture_scene(dir: &std::path::Path, id: SceneId) -> String {
    let mut world = World::new();
    let root = world.spawn(id).id();
    let child = world.spawn((Marker(7), ChildOf(root))).id();

    let mut type_registry = bevy::reflect::TypeRegistry::default();
    type_registry.register::<SceneId>();
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
fn open_scene_tags_the_scene_root_and_its_descendants() {
    let dir = temp_scene_dir("open");
    let id = SceneId(Uuid::new_v4());
    write_fixture_scene(&dir, id);

    let mut app = test_app(&dir);
    app.world_mut()
        .run_system_once(
            |mut commands: Commands, asset_server: Res<AssetServer>, formats: Res<SceneFormats>| {
                open_scene(&mut commands, &asset_server, &formats, "demo.scn.ron");
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
    let marker_entity = marker_entity.expect("scene loads within 200 frames");

    let membership = app
        .world()
        .get::<SceneMembership>(marker_entity)
        .expect("descendant is tagged with SceneMembership");
    assert_eq!(membership.0, id);

    let mut roots = app.world_mut().query::<(Entity, &SceneId, &SceneName)>();
    let (root_entity, &root_id, root_name) = roots
        .single(app.world())
        .expect("exactly one entity carries both SceneId and SceneName");
    assert_eq!(root_id, id);
    assert_eq!(root_name.0, "demo");
    assert!(
        app.world().get::<ChildOf>(root_entity).is_none(),
        "a loaded scene's root must be a genuine top-level entity, not stay parented under the \
         transient load container -- see commands.rs's tag_membership_on_ready Case 1"
    );
}

#[test]
fn save_then_open_preserves_scene_identity_membership_and_marker() {
    let dir = temp_scene_dir("save_open");
    let id = SceneId(Uuid::new_v4());
    let path = dir.join("round_trip.scn.ron");

    let mut source_app = test_app(&dir);
    let root = source_app.world_mut().spawn((id, SceneMembership(id))).id();
    source_app
        .world_mut()
        .spawn((Marker(7), SceneMembership(id), ChildOf(root)));
    save_scene(&path.to_string_lossy(), source_app.world_mut(), id)
        .expect("save_scene writes a scene file");

    let mut loaded_app = test_app(&dir);
    loaded_app
        .world_mut()
        .run_system_once(
            |mut commands: Commands, asset_server: Res<AssetServer>, formats: Res<SceneFormats>| {
                open_scene(&mut commands, &asset_server, &formats, "round_trip.scn.ron");
            },
        )
        .expect("open_scene system runs");

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
    let marker_entity = marker_entity.expect("saved scene loads within 200 frames");
    assert_eq!(
        loaded_app.world().get::<Marker>(marker_entity),
        Some(&Marker(7))
    );

    let root_entity = {
        let mut roots = loaded_app.world_mut().query::<(Entity, &SceneId)>();
        let (entity, &root_id) = roots
            .single(loaded_app.world())
            .expect("exactly one loaded root carries SceneId");
        assert_eq!(root_id, id);
        entity
    };
    assert_eq!(
        loaded_app.world().get::<SceneMembership>(root_entity),
        Some(&SceneMembership(id))
    );
    assert_eq!(
        loaded_app.world().get::<SceneMembership>(marker_entity),
        Some(&SceneMembership(id))
    );
}
