use bevy::prelude::*;
use big_space::plugin::BigSpaceDefaultPlugins;
use big_space::prelude::{BigSpaceCameraController, CellCoord, FloatingOrigin, Grid};
use ename_engine::bigspace::{BigSpacePlugin, FrozenOrigin, GridFollowCamera, set_origin_frozen};

fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(bevy::input::InputPlugin)
        .add_plugins(BigSpaceDefaultPlugins)
        .add_plugins(BigSpacePlugin);
    app
}

#[test]
fn a_grid_follow_camera_with_no_grid_present_stays_parentless_and_gains_no_cell_coord() {
    let mut app = test_app();
    let camera = app
        .world_mut()
        .spawn((GridFollowCamera, Transform::default()))
        .id();

    app.update();
    app.update();

    let entity = app.world().entity(camera);
    assert!(entity.contains::<BigSpaceCameraController>());
    assert!(!entity.contains::<CellCoord>());
    assert!(!entity.contains::<bevy::prelude::ChildOf>());
}

#[test]
fn a_grid_follow_camera_attaches_to_a_grid_that_appears_preserving_world_position() {
    let mut app = test_app();
    let camera = app
        .world_mut()
        .spawn((GridFollowCamera, Transform::from_xyz(3.0, 1.0, -2.0)))
        .id();
    app.update();

    app.world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default());
    app.update();
    app.update();

    let entity = app.world().entity(camera);
    assert!(entity.contains::<CellCoord>());
    assert!(entity.contains::<FloatingOrigin>());
    assert!(entity.contains::<bevy::prelude::ChildOf>());

    let transform = entity.get::<Transform>().unwrap();
    assert!(transform.translation.distance(Vec3::new(3.0, 1.0, -2.0)) < 0.01);
}

#[test]
fn a_grid_follow_camera_detaches_when_its_grid_component_is_removed_preserving_position() {
    let mut app = test_app();
    let grid = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let camera = app
        .world_mut()
        .spawn((GridFollowCamera, Transform::from_xyz(5.0, 0.0, 0.0)))
        .id();
    app.update();
    app.update();
    assert!(app.world().entity(camera).contains::<CellCoord>());

    // Remove just the `Grid` component rather than despawning the entity: despawning would
    // cascade-despawn the still-parented camera before `sync_grid_attachment` gets a chance to
    // react at all -- exactly the hazard Task 2's stage-unload rescue exists to avoid on the
    // real despawn path. This test isolates `sync_grid_attachment`'s own reaction to "my grid
    // stopped being a grid".
    app.world_mut().entity_mut(grid).remove::<Grid>();
    app.update();
    app.update();

    let entity = app.world().entity(camera);
    assert!(!entity.contains::<CellCoord>());
    assert!(!entity.contains::<FloatingOrigin>());
    let transform = entity.get::<Transform>().unwrap();
    assert!(transform.translation.distance(Vec3::new(5.0, 0.0, 0.0)) < 0.01);
}

#[test]
fn freezing_the_origin_moves_floating_origin_to_a_stationary_anchor() {
    let mut app = test_app();
    app.world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default());
    let camera = app
        .world_mut()
        .spawn((GridFollowCamera, Transform::default()))
        .id();
    app.update();
    app.update();
    assert!(app.world().entity(camera).contains::<FloatingOrigin>());

    set_origin_frozen(app.world_mut(), camera, true);

    assert!(!app.world().entity(camera).contains::<FloatingOrigin>());
    let FrozenOrigin(anchor) = *app.world().entity(camera).get::<FrozenOrigin>().unwrap();
    assert!(app.world().entity(anchor).contains::<FloatingOrigin>());

    set_origin_frozen(app.world_mut(), camera, false);

    assert!(app.world().entity(camera).contains::<FloatingOrigin>());
    assert!(!app.world().entity(camera).contains::<FrozenOrigin>());
    assert!(
        app.world().get_entity(anchor).is_err(),
        "anchor should be despawned"
    );
}
