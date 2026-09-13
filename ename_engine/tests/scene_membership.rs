//! `save_scene` must only ever capture entities tagged with the scene's `SceneMembership` --
//! never editor UI, gizmos, or anything else untagged.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::AssetPlugin,
    prelude::*,
    world_serialization::WorldSerializationPlugin,
};
use ename_engine::scene::{
    DynamicWorldFormat, SceneAppExt, SceneId, SceneMembership, ScenePlugin, save_scene,
};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Component, Reflect, Clone, PartialEq, Debug, Default)]
#[reflect(Component)]
struct Marker(i32);

#[derive(Component, Reflect, Clone, Debug, Default)]
#[reflect(Component)]
struct EditorOnly;

fn temp_scene_dir(test_name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ename_engine_scene_membership_{test_name}_{}",
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
        .register_type::<Marker>()
        .register_type::<EditorOnly>();
    app
}

#[test]
fn untagged_entities_never_appear_in_the_saved_output() {
    let dir = temp_scene_dir("save");
    let mut app = test_app(&dir);
    let id = SceneId(Uuid::new_v4());

    app.world_mut().spawn((id, SceneMembership(id), Marker(1)));
    app.world_mut().spawn(EditorOnly);

    let path = dir.join("demo.scn.ron");
    save_scene(&path.to_string_lossy(), app.world_mut(), id).expect("save succeeds");

    let text = std::fs::read_to_string(&path).expect("file was written");
    assert!(
        text.contains("Marker"),
        "the tagged entity's component must be present: {text}"
    );
    assert!(
        !text.contains("EditorOnly"),
        "an untagged entity must never be captured: {text}"
    );
}

#[test]
fn multiple_top_level_entities_are_reparented_under_a_synthetic_root() {
    let dir = temp_scene_dir("synthetic_root");
    let mut app = test_app(&dir);
    let id = SceneId(Uuid::new_v4());

    // Two independent top-level entities, neither carrying SceneId -- there is no single natural
    // root, so save_scene must invent one.
    app.world_mut().spawn((SceneMembership(id), Marker(1)));
    app.world_mut().spawn((SceneMembership(id), Marker(2)));

    let path = dir.join("demo.scn.ron");
    save_scene(&path.to_string_lossy(), app.world_mut(), id).expect("save succeeds");

    let mut roots = app.world_mut().query::<&SceneId>();
    assert_eq!(
        roots.iter(app.world()).count(),
        1,
        "exactly one entity must carry SceneId after save"
    );
}
