use bevy::prelude::*;
use big_space::plugin::BigSpaceDefaultPlugins;
use big_space::prelude::{BigSpaceCameraController, CellCoord, FloatingOrigin, Grid};
use ename_engine::bigspace::{
    BigSpacePlugin, FloatingOriginCandidate, FrozenOrigin, GridFollowCamera, set_origin_frozen,
};

fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(bevy::input::InputPlugin)
        .add_plugins(BigSpaceDefaultPlugins)
        .add_plugins(BigSpacePlugin);
    app
}

/// Spawns a root `BigSpace` and a candidate of the given priority already parented to it, skipping
/// `GridFollowCamera`'s attach step so a test can set up several candidates in one known cell.
fn spawn_candidate(app: &mut App, root: Entity, priority: i32) -> Entity {
    app.world_mut()
        .spawn((
            FloatingOriginCandidate(priority),
            CellCoord::default(),
            Transform::default(),
            ChildOf(root),
        ))
        .id()
}

fn holds_origin(app: &App, entity: Entity) -> bool {
    app.world().entity(entity).contains::<FloatingOrigin>()
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
    // Use a position that crosses cell boundaries (cell (25,0,0) at default 2000 cell_edge_length)
    // so the test can't pass by coincidence.
    let camera = app
        .world_mut()
        .spawn((GridFollowCamera, Transform::from_xyz(50_000.0, 0.0, 0.0)))
        .id();
    app.update();
    app.update();
    assert!(app.world().entity(camera).contains::<CellCoord>());

    // Remove just the `Grid` component rather than despawning the entity: despawning would
    // cascade-despawn the still-parented camera before the component hook gets a chance to
    // react at all -- exactly the hazard Task 2's stage-unload rescue exists to avoid on the
    // real despawn path. This test isolates the `on_remove` hook's reaction to Grid removal.
    app.world_mut().entity_mut(grid).remove::<Grid>();
    // No app.update() needed: the hook runs synchronously during remove()

    let entity = app.world().entity(camera);
    assert!(!entity.contains::<CellCoord>());
    assert!(!entity.contains::<FloatingOrigin>());
    let transform = entity.get::<Transform>().unwrap();
    assert!(
        transform
            .translation
            .distance(Vec3::new(50_000.0, 0.0, 0.0))
            < 0.01,
        "position should be preserved at 50000.0, got {}",
        transform.translation.x
    );
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

#[test]
fn the_highest_priority_candidate_under_a_root_holds_the_origin() {
    let mut app = test_app();
    let root = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let low = spawn_candidate(&mut app, root, 0);
    let high = spawn_candidate(&mut app, root, 100);
    app.update();

    assert!(holds_origin(&app, high));
    assert!(!holds_origin(&app, low));
}

#[test]
fn raising_a_losers_priority_moves_the_origin_to_it() {
    let mut app = test_app();
    let root = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let editor = spawn_candidate(&mut app, root, 100);
    let game = spawn_candidate(&mut app, root, 0);
    app.update();
    assert!(holds_origin(&app, editor));

    // What entering play-in-editor will do: the editor camera drops below the stage's own
    // candidate rather than the game camera being raised, but the election sees the same thing.
    *app.world_mut()
        .get_mut::<FloatingOriginCandidate>(editor)
        .unwrap() = FloatingOriginCandidate(-100);
    app.update();

    assert!(holds_origin(&app, game));
    assert!(!holds_origin(&app, editor));
}

#[test]
fn despawning_the_holder_hands_the_origin_to_the_remaining_candidate() {
    let mut app = test_app();
    let root = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let winner = spawn_candidate(&mut app, root, 100);
    let runner_up = spawn_candidate(&mut app, root, 0);
    app.update();
    assert!(holds_origin(&app, winner));

    app.world_mut().entity_mut(winner).despawn();
    app.update();

    assert!(holds_origin(&app, runner_up));
}

#[test]
fn each_root_elects_its_own_origin_independently() {
    let mut app = test_app();
    let first = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let second = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let in_first = spawn_candidate(&mut app, first, 0);
    let in_second = spawn_candidate(&mut app, second, 0);
    app.update();

    assert!(holds_origin(&app, in_first));
    assert!(holds_origin(&app, in_second));
}

#[test]
fn a_candidate_that_is_not_grid_attached_never_holds_the_origin() {
    let mut app = test_app();
    app.world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default());
    // No `CellCoord` and no parent: eligible on paper, but not in any `BigSpace` hierarchy, which
    // is the same walk `find_floating_origin` does.
    let loose = app
        .world_mut()
        .spawn((FloatingOriginCandidate(1000), Transform::default()))
        .id();
    app.update();
    app.update();

    assert!(!holds_origin(&app, loose));
}
