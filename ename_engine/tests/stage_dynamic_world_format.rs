//! `DynamicWorldFormat` wraps `bevy_world_serialization`. This proves `spawn_root` produces a
//! loadable `DynamicWorldRoot` and `serialize` round-trips a component through RON bytes -- not
//! yet through the full open/save pipeline, which Task 5 and 6 add.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::AssetPlugin,
    ecs::system::RunSystemOnce,
    prelude::*,
    world_serialization::WorldSerializationPlugin,
};
use ename_engine::stage::{DynamicWorldFormat, SourcePath, StageFormat};

const FIXTURE_ROOT: &str = "ename_engine/tests/fixtures";

#[derive(Component, Reflect, Clone, PartialEq, Debug, Default)]
#[reflect(Component)]
struct Marker(i32);

fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(TaskPoolPlugin::default())
        .add_plugins(AssetPlugin {
            file_path: FIXTURE_ROOT.to_owned(),
            ..Default::default()
        })
        .add_plugins(WorldSerializationPlugin)
        .register_type::<Marker>();
    app
}

#[test]
fn spawn_root_creates_a_dynamic_world_root_with_the_source_path_recorded() {
    let mut app = test_app();
    let format = DynamicWorldFormat;
    let asset_server = app.world().resource::<AssetServer>().clone();

    let container = app
        .world_mut()
        .run_system_once(move |mut commands: Commands| {
            format.spawn_root(&mut commands, &asset_server, "scenes/demo.scn.ron")
        })
        .expect("system runs");

    assert!(app.world().get::<DynamicWorldRoot>(container).is_some());
    assert_eq!(
        app.world()
            .get::<SourcePath>(container)
            .map(|p| p.0.as_str()),
        Some("scenes/demo.scn.ron")
    );
}

#[test]
fn serialize_round_trips_a_component_through_ron() {
    let mut app = test_app();
    let entity = app.world_mut().spawn(Marker(7)).id();

    let format = DynamicWorldFormat;
    let bytes = format
        .serialize(app.world(), &[entity])
        .expect("serialize succeeds");
    let text = String::from_utf8(bytes).expect("ron is valid utf8");

    assert!(
        text.contains("Marker"),
        "serialized RON should name the component type: {text}"
    );
    assert!(
        text.contains('7'),
        "serialized RON should carry the component's data: {text}"
    );
}

/// Regression test: a stage's root can have a child that is not part of the extracted entity set
/// (e.g. the editor's always-present `GridFollowCamera`, parented under a stage's `Grid` while
/// attached, but never itself a `StageMember`). `bevy_world_serialization` serializes `Children`
/// verbatim, with no filtering to the extracted set, so before this fix such a child's entity id
/// would be written into the root's `Children` list with no corresponding entry in the file's own
/// entity map -- a dangling reference that resolves to a phantom entity on load (see
/// `dynamic_world_format.rs`'s `serialize`). Denying `Children` from serialization avoids this
/// entirely: every child already carries `ChildOf`, whose insert hook rebuilds `Children` on load.
#[test]
fn serialize_denies_children_so_an_entity_outside_the_extracted_set_leaves_no_dangling_reference() {
    let mut app = test_app();
    app.register_type::<ChildOf>().register_type::<Children>();

    let root = app.world_mut().spawn(Marker(1)).id();
    let extracted_child = app.world_mut().spawn((Marker(2), ChildOf(root))).id();
    // Not passed to `serialize` below -- mirrors a `GridFollowCamera` that is parented under the
    // stage's Grid but carries no `StageMember`, so it is never part of the extracted set.
    let _unextracted_child = app.world_mut().spawn((Marker(3), ChildOf(root))).id();

    let format = DynamicWorldFormat;
    let bytes = format
        .serialize(app.world(), &[root, extracted_child])
        .expect("serialize succeeds");
    let text = String::from_utf8(bytes).expect("ron is valid utf8");

    assert!(
        !text.contains("bevy_ecs::hierarchy::Children"),
        "Children must be denied from stage serialization -- otherwise the root's Children list \
         can reference an entity outside the file's own entity map, a dangling reference that \
         resolves to a phantom entity on load: {text}"
    );
}
