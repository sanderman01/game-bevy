//! Where the editor camera goes when a stage is opened: to the new stage's Main Camera, in its
//! root grid's frame, or the default start pose without one; nowhere on a hot reload or an
//! additive open.

use std::{path::Path, time::Duration};

use bevy::{
    asset::{AssetPlugin, uuid::Uuid},
    math::{DQuat, DVec3},
    time::TimeUpdateStrategy,
    world_serialization::{DynamicWorld, DynamicWorldRoot, WorldSerializationPlugin},
};
use ename_engine::{
    bigspace::{BigSpace, BigSpaceDefaultPlugins, BigSpacePlugin, CellCoord, Grid},
    camera::MainCamera,
    stage::{
        AssetRoot, DynamicWorldFormat, StageFormat, StagePlugin, new_stage, open_stage,
        open_stage_additive,
    },
};

use super::*;
use crate::panels::UiState;

const EDGE: f32 = 40.0;

/// A Main Camera's place: cell in the stage's root grid, local translation, yaw.
#[derive(Clone, Copy)]
struct Shot {
    cell: CellCoord,
    translation: Vec3,
    yaw: f32,
}

impl Shot {
    fn position(self) -> DVec3 {
        Grid::new(EDGE, 0.0)
            .grid_position_double(&self.cell, &Transform::from_translation(self.translation))
    }

    fn rotation(self) -> DQuat {
        DQuat::from_rotation_y(f64::from(self.yaw))
    }

    fn transform(self) -> Transform {
        Transform::from_translation(self.translation).with_rotation(Quat::from_rotation_y(self.yaw))
    }
}

/// A Main Camera, as a stage file holds one.
fn main_camera(transform: Transform) -> impl Bundle {
    (MainCamera, Camera::default(), transform)
}

/// Writes a stage file: a root, a grid with `EDGE` metre cells when `grid` is set, with whatever
/// `content` spawns under it.
fn write_stage(
    app: &App,
    dir: &Path,
    name: &str,
    grid: bool,
    content: impl FnOnce(&mut World, Entity),
) {
    let mut source = World::new();
    let root = source.spawn(StageId(Uuid::new_v4())).id();
    if grid {
        source
            .entity_mut(root)
            .insert((BigSpace::default(), Grid::new(EDGE, 0.0)));
    }
    content(&mut source, root);
    // Saved as a stage save would, which leaves out what a camera recreates on load.
    source.insert_resource(app.world().resource::<AppTypeRegistry>().clone());
    let entities: Vec<Entity> = source.iter_entities().map(|entity| entity.id()).collect();
    let bytes = DynamicWorldFormat.serialize(&source, &entities).unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
}

/// A stage file with a grid and, with `shot`, a Main Camera directly in it.
fn write_shot_stage(app: &App, dir: &Path, name: &str, shot: Option<Shot>) {
    write_stage(app, dir, name, true, |world, root| {
        if let Some(shot) = shot {
            world.spawn((main_camera(shot.transform()), shot.cell, ChildOf(root)));
        }
    });
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
    .register_type::<Camera>()
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

/// The root of the stage opened from `name`.
fn stage_root(app: &mut App, name: &str) -> Option<Entity> {
    let stem = name.trim_end_matches(".scn.ron");
    app.world_mut()
        .query_filtered::<(Entity, &Name), With<StageId>>()
        .iter(app.world())
        .find(|(_, found)| found.as_str() == stem)
        .map(|(entity, _)| entity)
}

/// Opens `name` with `open` and updates until it has loaded and the camera has settled.
fn open(app: &mut App, name: &str, open: fn(&mut World, &str) -> Entity) -> Entity {
    open(app.world_mut(), name);
    for _ in 0..200 {
        app.update();
        if let Some(root) = stage_root(app, name) {
            for _ in 0..10 {
                app.update();
            }
            return root;
        }
    }
    panic!("{name} never loaded");
}

/// The camera's parent grid, if any, and pose: in that grid's frame, or absolute.
fn pose(app: &mut App) -> (Option<Entity>, DVec3, DQuat) {
    let camera = camera(app);
    let world = app.world();
    match grid::absolute_pose(world, camera) {
        Ok((position, rotation)) => (
            Some(grid::absolute_position(world, camera).unwrap().grid),
            position,
            rotation,
        ),
        Err(_) => {
            let transform = world.get::<Transform>(camera).unwrap();
            (
                None,
                transform.translation.as_dvec3(),
                transform.rotation.as_dquat(),
            )
        }
    }
}

fn assert_pose(
    app: &mut App,
    grid: Option<Entity>,
    position: DVec3,
    rotation: DQuat,
    context: &str,
) {
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

fn assert_default_pose(app: &mut App, grid: Option<Entity>, context: &str) {
    let default = default_camera_transform();
    assert_pose(
        app,
        grid,
        default.translation.as_dvec3(),
        default.rotation.as_dquat(),
        context,
    );
}

/// The orbit pivot sits in the camera's grid, the default distance ahead of it.
fn assert_fresh_pivot(app: &mut App, context: &str) {
    let camera = camera(app);
    let world = app.world();
    let orbit = *world.get::<OrbitFocus>(camera).unwrap();
    assert!(
        (orbit.distance - DEFAULT_FOCUS_DISTANCE).abs() < 1e-3,
        "{context}: orbit distance {}",
        orbit.distance
    );
    assert_eq!(
        world.get::<ChildOf>(orbit.entity).map(ChildOf::parent),
        world.get::<ChildOf>(camera).map(ChildOf::parent),
        "{context}: the pivot is not in the camera's grid"
    );
}

/// Moves the camera far from wherever it is, in its grid, and spreads the orbit.
fn fly_away(app: &mut App) {
    let camera = camera(app);
    let world = app.world_mut();
    *world.get_mut::<CellCoord>(camera).unwrap() = CellCoord::new(80, 12, 300);
    world.get_mut::<OrbitFocus>(camera).unwrap().distance = 80.0;
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
    write_shot_stage(&app, dir.path(), "first.scn.ron", Some(first));
    write_shot_stage(&app, dir.path(), "second.scn.ron", Some(second));
    write_shot_stage(&app, dir.path(), "empty.scn.ron", None);

    // The startup open: the camera comes from its spawn pose, on no grid.
    let root = open(&mut app, "first.scn.ron", open_stage);
    assert_pose(
        &mut app,
        Some(root),
        first.position(),
        first.rotation(),
        "first",
    );
    assert_fresh_pivot(&mut app, "first");

    fly_away(&mut app);
    let root = open(&mut app, "second.scn.ron", open_stage);
    assert_pose(
        &mut app,
        Some(root),
        second.position(),
        second.rotation(),
        "second",
    );
    assert_fresh_pivot(&mut app, "second");

    fly_away(&mut app);
    let root = open(&mut app, "empty.scn.ron", open_stage);
    assert_default_pose(&mut app, Some(root), "empty");
    assert_fresh_pivot(&mut app, "empty");
}

#[test]
fn a_new_stage_puts_the_camera_at_the_default_pose_on_no_grid() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    let shot = Shot {
        cell: CellCoord::new(4, 0, 4),
        translation: Vec3::new(1.0, 2.0, 3.0),
        yaw: 0.2,
    };
    write_shot_stage(&app, dir.path(), "first.scn.ron", Some(shot));
    open(&mut app, "first.scn.ron", open_stage);
    fly_away(&mut app);

    new_stage(app.world_mut());
    for _ in 0..3 {
        app.update();
    }

    assert_default_pose(&mut app, None, "new stage");
}

#[test]
fn a_main_camera_in_a_sub_grid_places_the_camera_and_pivot_in_the_root_grid() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    write_stage(&app, dir.path(), "decks.scn.ron", true, |world, root| {
        let deck = world
            .spawn((
                Grid::new(10.0, 0.0),
                CellCoord::new(1, 0, 0),
                Transform::from_xyz(2.0, 0.0, 0.0),
                ChildOf(root),
            ))
            .id();
        world.spawn((
            main_camera(Transform::from_xyz(3.0, 1.0, 0.0)),
            CellCoord::new(1, 0, 0),
            ChildOf(deck),
        ));
    });

    let root = open(&mut app, "decks.scn.ron", open_stage);

    // Deck at 40 + 2 m, camera 10 + 3 m into it.
    assert_pose(
        &mut app,
        Some(root),
        DVec3::new(55.0, 1.0, 0.0),
        DQuat::IDENTITY,
        "in a sub-grid",
    );
    assert_fresh_pivot(&mut app, "in a sub-grid");
}

#[test]
fn a_main_camera_under_an_ordinary_parent_in_a_gridless_stage_is_followed() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    write_stage(&app, dir.path(), "plain.scn.ron", false, |world, root| {
        let rig = world
            .spawn((
                Transform::from_xyz(100.0, 0.0, 0.0)
                    .with_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)),
                ChildOf(root),
            ))
            .id();
        world.spawn((
            main_camera(Transform::from_xyz(5.0, 0.0, 0.0)),
            ChildOf(rig),
        ));
    });

    open(&mut app, "plain.scn.ron", open_stage);

    assert_pose(
        &mut app,
        None,
        DVec3::new(100.0, 0.0, -5.0),
        DQuat::from_rotation_y(std::f64::consts::FRAC_PI_2),
        "under an ordinary parent",
    );
}

#[test]
fn a_main_camera_in_nested_content_is_waited_for() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    let shot = Shot {
        cell: CellCoord::default(),
        translation: Vec3::new(6.0, 4.0, -2.0),
        yaw: 0.9,
    };
    write_stage(&app, dir.path(), "inner.scn.ron", false, |world, root| {
        world.entity_mut(root).insert(main_camera(shot.transform()));
    });
    let inner = app
        .world()
        .resource::<AssetServer>()
        .load::<DynamicWorld>("inner.scn.ron");
    write_stage(&app, dir.path(), "outer.scn.ron", true, |world, root| {
        world.spawn((DynamicWorldRoot(inner), ChildOf(root)));
    });

    let root = open(&mut app, "outer.scn.ron", open_stage);

    assert_pose(
        &mut app,
        Some(root),
        shot.position(),
        shot.rotation(),
        "nested",
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
    write_shot_stage(&app, dir.path(), "first.scn.ron", Some(shot));
    write_shot_stage(&app, dir.path(), "other.scn.ron", Some(shot));
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
    assert_pose(
        &mut app,
        Some(root),
        kept_position,
        kept_rotation,
        "hot reload",
    );

    open(&mut app, "other.scn.ron", open_stage_additive);
    let (_, position, rotation) = pose(&mut app);
    assert!(
        position.distance(kept_position) < 1e-3 && rotation.dot(kept_rotation).abs() > 1.0 - 1e-6,
        "an additive open moved the camera to {position}"
    );
}
