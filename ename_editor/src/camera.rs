//! The editor's own scene camera: an always-present `EditorCamera` with Unreal-style flight
//! controls. Unlike the game's own cameras (engine-driven, bound by whatever layer loads them),
//! this one is entirely the editor's: it owns both the input bindings and how they're applied.

use bevy::{
    input::mouse::{MouseMotion, MouseWheel},
    prelude::*,
};
use ename_engine::bigspace::{
    BigSpaceCameraController, BigSpaceCameraInput, CellCoord, GridCameraSystems, GridFollowCamera,
};

/// Tags the editor's own scene-view camera. Distinct from `ename_engine::camera::MainCamera`,
/// which tags whatever camera a loaded stage defines for the Game View -- that one may not exist
/// at all; this one always does.
#[derive(Component, Debug, Default, Clone, Copy)]
pub(crate) struct EditorCamera;

/// Free-flight-only velocity state, carried between frames while `EditorCamera` has no
/// `CellCoord` (no Grid to fly relative to, so big_space's own `camera_controller` won't touch
/// it). `BigSpaceCameraController`'s own velocity fields are private to big_space; this is
/// `EditorCamera`'s equivalent, used only by [`apply_free_flight`]. Speed/smoothness/bounds
/// configuration is deliberately *not* duplicated here -- both flight paths read that from the
/// `BigSpaceCameraController` that [`GridFollowCamera`] keeps present on the same entity at all
/// times.
#[derive(Component, Debug, Default, Clone, Copy)]
pub(crate) struct FreeFlightState {
    vel_translation: Vec3,
    vel_rotation: Quat,
}

/// Frame-local flight input, in the same aircraft axes as `BigSpaceCameraInput`. Captured by
/// [`write_editor_camera_intent`], consumed by whichever of [`apply_grid_flight`] /
/// [`apply_free_flight`] matches `EditorCamera`'s current attachment state, and cleared by
/// whichever one runs.
#[derive(Resource, Debug, Default, Clone)]
struct EditorCameraIntent {
    forward: f64,
    up: f64,
    right: f64,
    pitch: f64,
    yaw: f64,
    boost: bool,
}

impl EditorCameraIntent {
    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// The button that puts the fly camera in control. The only place this binding is written down;
/// [`fly_camera_active`], [`write_editor_camera_intent`], and [`adjust_fly_speed`] all read it
/// from here.
const FLY_CAMERA_BUTTON: MouseButton = MouseButton::Right;

/// Linear step applied to `BigSpaceCameraController::speed` per plain scroll tick.
const SPEED_SCROLL_STEP: f64 = 5.0;
/// Multiplicative factor applied per scroll tick instead, while Shift is held.
const SPEED_SCROLL_MULTIPLIER: f64 = 2.0;

/// Spawns `EditorCamera` and drives it from Unreal-style bindings.
pub(crate) struct EditorCameraPlugin;

impl Plugin for EditorCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EditorCameraIntent>()
            .add_systems(
                Startup,
                (spawn_editor_camera, disable_default_camera_inputs),
            )
            .add_systems(
                PostUpdate,
                (
                    write_editor_camera_intent,
                    adjust_fly_speed,
                    apply_grid_flight.in_set(GridCameraSystems::Apply),
                    apply_free_flight,
                )
                    .chain(),
            );
    }
}

/// True while the fly camera is claiming the keyboard. The camera and the gizmo shortcuts both
/// want W and E; the camera wins while this is held, and this is the single place that rule is
/// written down.
pub(crate) fn fly_camera_active(mouse: Res<ButtonInput<MouseButton>>) -> bool {
    mouse.pressed(FLY_CAMERA_BUTTON)
}

fn spawn_editor_camera(mut commands: Commands) {
    commands.spawn((
        EditorCamera,
        GridFollowCamera,
        FreeFlightState::default(),
        Camera3d::default(),
        Transform::from_xyz(0.0, 2.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
        Name::new("Editor Camera"),
    ));
}

/// big_space's own WASD/Space/Ctrl/Q-E-roll bindings (`default_camera_inputs`) are never wanted
/// here -- this crate supplies its own Unreal-style ones instead. `BigSpaceCameraInput::reset`
/// preserves `defaults_disabled` across every reset, so setting this once at startup is enough;
/// it must not be left to [`apply_grid_flight`] alone, which only touches the resource while a
/// grid is attached and would otherwise leave big_space's defaults live the rest of the time.
fn disable_default_camera_inputs(mut input: ResMut<BigSpaceCameraInput>) {
    input.defaults_disabled = true;
}

/// WASD move, Q/E down/up, mouse look -- Unreal's viewport camera bindings. No roll.
fn write_editor_camera_intent(
    keyboard: Res<ButtonInput<KeyCode>>,
    mouse_button: Res<ButtonInput<MouseButton>>,
    mut mouse_move: MessageReader<MouseMotion>,
    mut intent: ResMut<EditorCameraIntent>,
) {
    if !mouse_button.pressed(FLY_CAMERA_BUTTON) {
        // Drop the motion accumulated while not flying, or the first frame of the next drag
        // gets all of it at once.
        mouse_move.clear();
        return;
    }

    keyboard
        .pressed(KeyCode::KeyW)
        .then(|| intent.forward -= 1.0);
    keyboard
        .pressed(KeyCode::KeyS)
        .then(|| intent.forward += 1.0);
    keyboard.pressed(KeyCode::KeyA).then(|| intent.right -= 1.0);
    keyboard.pressed(KeyCode::KeyD).then(|| intent.right += 1.0);
    keyboard.pressed(KeyCode::KeyE).then(|| intent.up += 1.0);
    keyboard.pressed(KeyCode::KeyQ).then(|| intent.up -= 1.0);
    keyboard
        .pressed(KeyCode::ShiftLeft)
        .then(|| intent.boost = true);

    if let Some(total) = mouse_move.read().map(|e| e.delta).reduce(|sum, i| sum + i) {
        intent.pitch += total.y as f64 * -0.1;
        intent.yaw += total.x as f64 * -0.1;
    }
}

/// Scroll wheel while flying adjusts `BigSpaceCameraController::speed`: plain ticks step it
/// linearly, Shift+tick scales it multiplicatively (`speed *= 2` per tick up, `/= 2` per tick
/// down).
fn adjust_fly_speed(
    mouse_button: Res<ButtonInput<MouseButton>>,
    keyboard: Res<ButtonInput<KeyCode>>,
    mut wheel: MessageReader<MouseWheel>,
    mut cams: Query<&mut BigSpaceCameraController, With<EditorCamera>>,
) {
    if !mouse_button.pressed(FLY_CAMERA_BUTTON) {
        wheel.clear();
        return;
    }
    let ticks: f64 = wheel.read().map(|event| event.y as f64).sum();
    if ticks == 0.0 {
        return;
    }
    let Ok(mut controller) = cams.single_mut() else {
        return;
    };
    let [min, max] = controller.speed_bounds;
    controller.speed = if keyboard.pressed(KeyCode::ShiftLeft) {
        controller.speed * SPEED_SCROLL_MULTIPLIER.powf(ticks)
    } else {
        controller.speed + ticks * SPEED_SCROLL_STEP
    }
    .clamp(min, max);
}

/// Grid-attached path: hands the intent to big_space's own `camera_controller`, the same way the
/// deleted `ename_engine::input::FlyCameraPlugin` did.
fn apply_grid_flight(
    mut intent: ResMut<EditorCameraIntent>,
    mut input: ResMut<BigSpaceCameraInput>,
    cams: Query<(), (With<EditorCamera>, With<CellCoord>)>,
) {
    if cams.is_empty() {
        return;
    }
    input.forward = intent.forward;
    input.up = intent.up;
    input.right = intent.right;
    input.pitch = intent.pitch;
    input.yaw = intent.yaw;
    input.boost = intent.boost;
    intent.clear();
}

/// Free path: there is no Grid to fly relative to, so big_space's `camera_controller` (which
/// hard-requires `CellCoord`) never touches this entity. This integrates `Transform` directly
/// instead, mirroring `camera_controller`'s own smoothing and speed formulas in plain `f32`, with
/// no cell bookkeeping. There is deliberately no nearest-object slowdown: `nearest_object` is
/// private to big_space and only ever set by its `nearest_objects_in_grid` system, which
/// hard-requires `CellCoord` and so never runs on a `CellCoord`-less entity -- meaning free mode
/// can only ever be in `camera_controller`'s `nearest_object: None` branch, where its speed
/// formula reduces to `controller.speed * (controller.speed + boost)`. The formula below
/// reproduces exactly that branch, not merely an approximation of it: at any `controller.speed`
/// other than `1.0`, a formula merely linear in `controller.speed` (as an earlier draft had) gives
/// a real speed discontinuity crossing in and out of a Grid. Runs unconditionally after
/// [`apply_grid_flight`] in the same `.chain()`, and is itself responsible for clearing
/// `EditorCameraIntent` whenever the grid path didn't (its query is empty exactly when the grid
/// path's was not, since a camera is never both attached and unattached in the same frame).
#[allow(clippy::type_complexity)]
fn apply_free_flight(
    time: Res<Time>,
    mut intent: ResMut<EditorCameraIntent>,
    mut cams: Query<
        (
            &mut Transform,
            &BigSpaceCameraController,
            &mut FreeFlightState,
        ),
        (With<EditorCamera>, Without<CellCoord>),
    >,
) {
    let Ok((mut transform, controller, mut state)) = cams.single_mut() else {
        return;
    };

    let speed = (controller.speed * (controller.speed + intent.boost as u32 as f64))
        .clamp(controller.speed_bounds[0], controller.speed_bounds[1]);
    let dt = time.delta_secs_f64().min(0.1);
    let lerp_translation = 1.0 - controller.smoothness.clamp(0.0, 0.999).powf(dt * 60.0);
    let lerp_rotation = 1.0
        - controller
            .rotational_smoothness
            .clamp(0.0, 0.999)
            .powf(dt * 60.0);

    let target_translation = transform.rotation
        * (Vec3::new(intent.right as f32, intent.up as f32, intent.forward as f32)
            * speed as f32
            * dt as f32);
    state.vel_translation = state
        .vel_translation
        .lerp(target_translation, lerp_translation as f32);

    let target_rotation = Quat::from_euler(
        EulerRot::XYZ,
        (intent.pitch * dt * controller.speed_pitch) as f32,
        (intent.yaw * dt * controller.speed_yaw) as f32,
        0.0,
    );
    state.vel_rotation = state
        .vel_rotation
        .slerp(target_rotation, lerp_rotation as f32);

    transform.translation += state.vel_translation;
    transform.rotation *= state.vel_rotation;
    intent.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::time::TimeUpdateStrategy;
    use ename_engine::bigspace::{BigSpaceDefaultPlugins, BigSpacePlugin, CellCoord};
    use std::time::Duration;

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(bevy::input::InputPlugin)
            .add_plugins(BigSpaceDefaultPlugins)
            .add_plugins(BigSpacePlugin)
            .add_plugins(EditorCameraPlugin)
            // Real-time headless `App::update()` calls run with a near-zero `Time::delta`,
            // which the flight integrator's smoothing formulas correctly read as "barely move
            // this frame" -- that's accurate integration, not a bug, but it makes a real-time
            // test assert on noise. Force a fixed 60fps step so the math in the plan's own
            // `apply_free_flight`/big_space's `camera_controller` sees the frame time actual
            // play would give it.
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                1.0 / 60.0,
            )));
        app
    }

    #[test]
    fn spawns_exactly_one_editor_camera_with_no_parent() {
        let mut app = test_app();
        app.update();

        let mut query = app.world_mut().query::<(&EditorCamera, Option<&ChildOf>)>();
        let (_, parent) = query.single(app.world()).unwrap();
        assert!(parent.is_none());
    }

    #[test]
    fn holding_w_with_right_mouse_moves_the_free_flight_camera_forward() {
        let mut app = test_app();
        app.update();
        let camera = app
            .world_mut()
            .query_filtered::<Entity, With<EditorCamera>>()
            .single(app.world())
            .unwrap();
        let start = app.world().get::<Transform>(camera).unwrap().translation;

        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Right);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        for _ in 0..30 {
            app.update();
        }

        let moved = app.world().get::<Transform>(camera).unwrap().translation;
        assert!(
            moved.distance(start) > 0.01,
            "expected the camera to move forward, stayed at {moved:?}"
        );
        assert!(!app.world().entity(camera).contains::<CellCoord>());
    }

    #[test]
    fn free_flight_speed_matches_grid_paths_quadratic_speed_formula_after_scrolling() {
        // big_space's own `camera_controller` (the grid-attached path) computes
        // `speed = match (nearest_object, slow_near_objects) { (Some(n), true) => n.1.abs(), _ =>
        // controller.speed } * (controller.speed + boost)`. `nearest_object` is private to
        // big_space and only ever set by a system that hard-requires `CellCoord`, so a free-flight
        // (`CellCoord`-less) entity is always in the `_ => controller.speed` branch, which reduces
        // to `controller.speed * (controller.speed + boost)` -- quadratic in `controller.speed`,
        // not linear. At the default `speed == 1.0` a linear formula and this quadratic one agree
        // (1*1 == 1+0 == 1), which is why `holding_w_...` above doesn't catch a regression here;
        // this test scrolls to a non-default speed first so the two formulas disagree.
        let mut app = test_app();
        app.update();
        let camera = app
            .world_mut()
            .query_filtered::<Entity, With<EditorCamera>>()
            .single(app.world())
            .unwrap();
        let start = app.world().get::<Transform>(camera).unwrap().translation;

        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Right);
        // One plain (non-shift) scroll tick: default speed 1.0 -> 1.0 + SPEED_SCROLL_STEP == 6.0,
        // the same value the scroll-wheel test above also lands on after its first tick.
        app.world_mut()
            .write_message(bevy::input::mouse::MouseWheel {
                unit: bevy::input::mouse::MouseScrollUnit::Line,
                x: 0.0,
                y: 1.0,
                window: Entity::PLACEHOLDER,
                phase: bevy::input::touch::TouchPhase::Moved,
            });
        app.update();
        let controller_speed = app
            .world()
            .get::<BigSpaceCameraController>(camera)
            .unwrap()
            .speed;
        assert_eq!(
            controller_speed, 6.0,
            "scroll tick didn't land on the expected speed"
        );

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        const FRAMES: i32 = 60;
        for _ in 0..FRAMES {
            app.update();
        }
        let moved = app.world().get::<Transform>(camera).unwrap().translation;
        let actual_distance = moved.distance(start) as f64;

        // Closed-form distance for a constant target velocity fed through the same exponential
        // smoothing `apply_free_flight` itself uses (W and the right mouse button stay held for
        // every one of the `FRAMES` frames above, and no mouse motion is fed, so both the target
        // velocity and the smoothing factor are the same constant every frame): with `alpha` the
        // per-frame lerp factor and `decay = 1 - alpha`, velocity after `i` frames from rest is
        // `target * (1 - decay^i)`, and the summed displacement over `n` frames is
        // `target * (n - decay * (1 - decay^n) / alpha)`. `target` here plugs in `speed` computed
        // independently via `camera_controller`'s own quadratic formula -- exactly what the "Fix"
        // changed `apply_free_flight` to compute -- so this assertion fails under the old, merely
        // linear formula (which predicts roughly 1/6th the distance asserted here).
        let dt = 1.0 / 60.0_f64;
        let smoothness = 0.85_f64; // BigSpaceCameraController::default().smoothness
        let alpha = 1.0 - smoothness.powf(dt * 60.0);
        let decay = 1.0 - alpha;
        let speed = controller_speed * (controller_speed + 0.0); // no boost held
        let target_per_frame = speed * dt;
        let predicted_distance =
            target_per_frame * (f64::from(FRAMES) - decay * (1.0 - decay.powi(FRAMES)) / alpha);

        let relative_error = (actual_distance - predicted_distance).abs() / predicted_distance;
        assert!(
            relative_error < 0.01,
            "expected distance close to the quadratic-speed prediction {predicted_distance:.3}, \
             got {actual_distance:.3} ({relative_error:.4} relative error)"
        );
    }

    #[test]
    fn scroll_wheel_while_flying_adjusts_speed_linearly_and_shift_scroll_multiplies() {
        let mut app = test_app();
        app.update();
        let camera = app
            .world_mut()
            .query_filtered::<Entity, With<EditorCamera>>()
            .single(app.world())
            .unwrap();

        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Right);
        let starting_speed = app
            .world()
            .get::<BigSpaceCameraController>(camera)
            .unwrap()
            .speed;

        app.world_mut()
            .write_message(bevy::input::mouse::MouseWheel {
                unit: bevy::input::mouse::MouseScrollUnit::Line,
                x: 0.0,
                y: 1.0,
                window: Entity::PLACEHOLDER,
                phase: bevy::input::touch::TouchPhase::Moved,
            });
        app.update();
        let after_linear = app
            .world()
            .get::<BigSpaceCameraController>(camera)
            .unwrap()
            .speed;
        assert!(after_linear > starting_speed);

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::ShiftLeft);
        app.world_mut()
            .write_message(bevy::input::mouse::MouseWheel {
                unit: bevy::input::mouse::MouseScrollUnit::Line,
                x: 0.0,
                y: 1.0,
                window: Entity::PLACEHOLDER,
                phase: bevy::input::touch::TouchPhase::Moved,
            });
        app.update();
        let after_multiplied = app
            .world()
            .get::<BigSpaceCameraController>(camera)
            .unwrap()
            .speed;
        assert!(after_multiplied > after_linear * 1.9);
    }
}
