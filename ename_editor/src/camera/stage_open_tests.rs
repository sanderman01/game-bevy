//! Where the editor camera goes when a stage is opened: to the new stage's Main Camera, or the
//! default start pose without one; nowhere on a hot reload or an additive open.

use std::{path::Path, time::Duration};

use bevy::{
    asset::{AssetPlugin, uuid::Uuid},
    math::{DQuat, DVec3},
    time::TimeUpdateStrategy,
    world_serialization::{
        DynamicWorld, DynamicWorldBuilder, DynamicWorldRoot, WorldSerializationPlugin,
    },
};
use ename_engine::{
    bigspace::{BigSpace, BigSpaceDefaultPlugins, BigSpacePlugin, CellCoord, Grid},
    stage::{AssetRoot, StagePlugin, open_stage, open_stage_additive},
};

use super::*;
use crate::panels::UiState;

/// A Main Camera pose in a stage: cell, local translation, yaw.
#[derive(Clone, Copy)]
struct Shot {
    cell: CellCoord,
    translation: Vec3,
    yaw: f32,
}

const EDGE: f32 = 40.0;

/// Writes a stage whose root grid has `EDGE` metre cells and, with `shot`, a Main Camera there.
fn write_stage(app: &App, dir: &Path, name: &str, shot: Option<Shot>) {
    let mut source = World::new();
    let id = StageId(Uuid::new_v4());
    let root = source
        .spawn((id, BigSpace::default(), Grid::new(EDGE, 0.0)))
        .id();
    if let Some(shot) = shot {
        source.spawn((
            MainCamera,
            shot.cell,
            Transform::from_translation(shot.translation)
                .with_rotation(Quat::from_rotation_y(shot.yaw)),
            ChildOf(root),
        ));
    }
    let registry = app.world().resource::<AppTypeRegistry>().read();
    let dynamic_world = DynamicWorldBuilder::from_world(&source, &registry)
        .extract_entities(source.iter_entities().map(|entity| entity.id()))
        .build();
    let ron = dynamic_world.serialize(&registry).unwrap();
    std::fs::write(dir.join(name), ron).unwrap();
}

fn app(dir: &Path) -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        bevy::input::InputPlugin,
        AssetPlugin {
            file_path: dir.to_string_lossy().into_owned(),
            ..default()
        },
        WorldSerializationPlugin,
        StagePlugin,
        BigSpaceDefaultPlugins,
        BigSpacePlugin,
        EditorCameraPlugin {
            settings: default(),
        },
    ))
    .register_type::<MainCamera>()
    .insert_resource(AssetRoot(dir.to_owned()))
    .insert_resource(UiState::new())
    .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
        1.0 / 60.0,
    )));
    app.update();
    app
}

fn camera(app: &mut App) -> Entity {
    app.world_mut()
        .query_filtered::<Entity, With<EditorCamera>>()
        .single(app.world())
        .unwrap()
}

/// The root grid of the stage opened from `name`.
fn stage_root(app: &mut App, name: &str) -> Option<Entity> {
    let stem = name.trim_end_matches(".scn.ron");
    app.world_mut()
        .query_filtered::<(Entity, &Name), With<StageId>>()
        .iter(app.world())
        .find(|(_, found)| found.as_str() == stem)
        .map(|(entity, _)| entity)
}

/// Opens `name` the way `open` does and updates until it has loaded and the camera has settled.
fn open(app: &mut App, name: &str, open: fn(&mut World, &str) -> Entity) -> Entity {
    open(app.world_mut(), name);
    for _ in 0..200 {
        app.update();
        if let Some(root) = stage_root(app, name) {
            for _ in 0..3 {
                app.update();
            }
            return root;
        }
    }
    panic!("{name} never loaded");
}

/// The camera's grid and pose in that grid's frame.
fn pose(app: &mut App) -> (Entity, DVec3, DQuat) {
    let camera = camera(app);
    let world = app.world();
    let grid = grid::absolute_position(world, camera).unwrap().grid;
    let (position, rotation) = grid::absolute_pose(world, camera).unwrap();
    (grid, position, rotation)
}

fn assert_pose(app: &mut App, grid: Entity, position: DVec3, rotation: DQuat, context: &str) {
    let (found_grid, found_position, found_rotation) = pose(app);
    assert_eq!(found_grid, grid, "{context}: in the wrong grid");
    assert!(
        found_position.distance(position) < 1e-3,
        "{context}: at {found_position}, expected {position}"
    );
    assert!(
        found_rotation.dot(rotation).abs() > 1.0 - 1e-6,
        "{context}: rotation {found_rotation:?}, expected {rotation:?}"
    );
}

fn shot_position(shot: Shot) -> DVec3 {
    let grid = Grid::new(EDGE, 0.0);
    grid.grid_position_double(&shot.cell, &Transform::from_translation(shot.translation))
}

/// Moves the camera far from wherever it is, in its grid.
fn fly_away(app: &mut App) {
    let camera = camera(app);
    *app.world_mut().get_mut::<CellCoord>(camera).unwrap() = CellCoord::new(80, 12, 300);
    app.update();
}

#[test]
fn opening_a_stage_moves_the_camera_to_its_main_camera_or_the_default_pose() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    let first = Shot {
        cell: CellCoord::new(2, 0, -1),
        translation: Vec3::new(5.0, 3.0, 1.0),
        yaw: 0.7,
    };
    let second = Shot {
        cell: CellCoord::new(-3, 1, 4),
        translation: Vec3::new(-7.0, 2.0, 9.0),
        yaw: -1.2,
    };
    write_stage(&app, dir.path(), "first.scn.ron", Some(first));
    write_stage(&app, dir.path(), "second.scn.ron", Some(second));
    write_stage(&app, dir.path(), "empty.scn.ron", None);

    let root = open(&mut app, "first.scn.ron", open_stage);
    assert_pose(
        &mut app,
        root,
        shot_position(first),
        DQuat::from_rotation_y(0.7),
        "first",
    );

    fly_away(&mut app);
    let root = open(&mut app, "second.scn.ron", open_stage);
    assert_pose(
        &mut app,
        root,
        shot_position(second),
        DQuat::from_rotation_y(-1.2),
        "second",
    );

    fly_away(&mut app);
    let root = open(&mut app, "empty.scn.ron", open_stage);
    let default = default_camera_transform();
    assert_pose(
        &mut app,
        root,
        default.translation.as_dvec3(),
        default.rotation.as_dquat(),
        "empty",
    );
}

#[test]
fn a_hot_reload_or_an_additive_open_keeps_the_camera_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    let shot = Shot {
        cell: CellCoord::new(1, 0, 1),
        translation: Vec3::new(2.0, 2.0, 2.0),
        yaw: 0.3,
    };
    write_stage(&app, dir.path(), "first.scn.ron", Some(shot));
    write_stage(&app, dir.path(), "other.scn.ron", Some(shot));
    open(&mut app, "first.scn.ron", open_stage);
    fly_away(&mut app);
    let (_, kept_position, kept_rotation) = pose(&mut app);

    // A file watcher's reload: the asset changes and Bevy respawns the stage.
    let container = app
        .world_mut()
        .query_filtered::<Entity, With<DynamicWorldRoot>>()
        .single(app.world())
        .unwrap();
    let handle = app
        .world()
        .get::<DynamicWorldRoot>(container)
        .unwrap()
        .0
        .clone();
    for _ in 0..4 {
        app.update();
    }
    let before = stage_root(&mut app, "first.scn.ron");
    app.world_mut()
        .resource_mut::<Assets<DynamicWorld>>()
        .get_mut(&handle)
        .unwrap()
        .into_inner();
    for _ in 0..10 {
        app.update();
    }
    let root = stage_root(&mut app, "first.scn.ron").unwrap();
    assert_ne!(Some(root), before, "the stage was not respawned");
    assert_pose(&mut app, root, kept_position, kept_rotation, "hot reload");

    open(&mut app, "other.scn.ron", open_stage_additive);
    let (_, position, rotation) = pose(&mut app);
    assert!(
        position.distance(kept_position) < 1e-3 && rotation.dot(kept_rotation).abs() > 1.0 - 1e-6,
        "an additive open moved the camera to {position}"
    );
}
