//! The editor's own scene camera: an always-present `EditorCamera` with Unreal-style flight
//! controls. Unlike the game's own cameras (engine-driven, bound by whatever layer loads them),
//! this one is entirely the editor's: it owns both the input bindings and how they're applied.

use bevy::{
    camera::primitives::Aabb,
    input::mouse::{MouseMotion, MouseWheel},
    prelude::*,
    transform::TransformSystems,
};
use ename_engine::bigspace::{
    BigSpaceCameraController, BigSpaceCameraInput, CellCoord, GridCameraSystems, GridFollowCamera,
};

use crate::panels::{ActiveViewport, UiState};

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

/// The camera's current orbit pivot: the point orbiting rotates around, and how far the camera
/// sits from it. Set explicitly by F-focus; read (and its `distance` adjusted) while orbiting.
/// Initialized at spawn to a point some distance ahead of the camera, so orbiting works even
/// before anything has ever been focused.
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct OrbitFocus {
    point: Vec3,
    distance: f32,
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

/// `EditorCamera`'s spawn-time `OrbitFocus` sits this far in front of it, so orbiting has
/// something to pivot around before anything has ever been F-focused.
const DEFAULT_FOCUS_DISTANCE: f32 = 10.0;
/// Fallback radius used to size an F-focus dolly when the target has no `Aabb`.
const DEFAULT_FOCUS_RADIUS: f32 = 2.0;
/// Fallback FOV (radians) used to size an F-focus dolly when `EditorCamera` has no `Projection`,
/// or a non-perspective one. Matches `PerspectiveProjection::default()`'s own 45 degrees.
const DEFAULT_FOCUS_FOV: f32 = core::f32::consts::FRAC_PI_4;
/// Headroom multiplier applied to an F-focus target's radius, so the dolly doesn't frame it
/// edge-to-edge.
const FOCUS_PADDING: f32 = 1.5;
/// Floor on F-focus distance, so a tiny/zero-size target doesn't put the camera on top of it.
const MIN_FOCUS_DISTANCE: f32 = 0.5;

/// Orbit's own per-pixel mouse sensitivity, in degrees. Unlike the fly paths' `-0.1` (a
/// velocity-like accumulator that only becomes an angle once multiplied by `dt * speed_{pitch,
/// yaw}` downstream, so its bare magnitude carries no unit on its own), orbit's `(yaw, pitch)` is
/// an absolute angle updated directly frame to frame with no further scaling -- so the conversion
/// to radians has to happen where this is used, and the sign is independently derived there (see
/// [`apply_orbit`]) rather than copied from the fly paths' constant.
const ORBIT_MOUSE_SENSITIVITY_DEG_PER_PIXEL: f32 = 0.1;
/// Shared pitch clamp, within this many radians of level: in [`apply_orbit`], so orbit can't flip
/// over the top or bottom; in [`correct_camera_roll`], so it never rebuilds a `look_to` direction
/// parallel to world-up (degenerate). Clamping the pitch *scalar* -- not a cartesian component of
/// the forward vector, which `Dir3::new`'s renormalization would silently undo -- is what makes
/// this an actual bound on the resulting angle.
const MAX_CAMERA_PITCH: f32 = 89.0 * core::f32::consts::PI / 180.0;
/// Floor on `OrbitFocus::distance`, so scrolling in can't pull the camera through the pivot.
const MIN_ORBIT_DISTANCE: f32 = 0.1;

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
                    apply_orbit,
                    apply_grid_flight.in_set(GridCameraSystems::Apply),
                    apply_free_flight,
                    correct_camera_roll,
                )
                    .chain()
                    .before(TransformSystems::Propagate),
            )
            .add_systems(
                PostUpdate,
                apply_focus_on_f_key
                    .in_set(crate::EditorSystems::ApplySelection)
                    .before(TransformSystems::Propagate),
            );
    }
}

/// True while the fly camera is claiming the keyboard. The camera and the gizmo shortcuts both
/// want W and E; the camera wins while this is held, and this is the single place that rule is
/// written down.
pub(crate) fn fly_camera_active(mouse: Res<ButtonInput<MouseButton>>) -> bool {
    mouse.pressed(FLY_CAMERA_BUTTON)
}

/// True while orbit is claiming the fly button: RMB *and* either Alt held. Orbit and plain fly
/// are mutually exclusive -- see [`write_editor_camera_intent`]'s guard and [`apply_orbit`].
fn orbit_active(mouse: &ButtonInput<MouseButton>, keyboard: &ButtonInput<KeyCode>) -> bool {
    mouse.pressed(FLY_CAMERA_BUTTON)
        && (keyboard.pressed(KeyCode::AltLeft) || keyboard.pressed(KeyCode::AltRight))
}

fn spawn_editor_camera(mut commands: Commands) {
    let spawn_transform = Transform::from_xyz(0.0, 2.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y);
    commands.spawn((
        EditorCamera,
        GridFollowCamera,
        FreeFlightState::default(),
        OrbitFocus {
            point: spawn_transform.translation + spawn_transform.forward() * DEFAULT_FOCUS_DISTANCE,
            distance: DEFAULT_FOCUS_DISTANCE,
        },
        Camera3d::default(),
        spawn_transform,
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
    if !mouse_button.pressed(FLY_CAMERA_BUTTON) || orbit_active(&mouse_button, &keyboard) {
        // Drop the motion accumulated while not flying (or while orbiting has claimed the mouse
        // instead -- see `apply_orbit`), or the first frame of the next drag gets all of it at
        // once.
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
/// down). Stands down while orbiting -- scrolling then adjusts `OrbitFocus::distance` instead (see
/// [`apply_orbit`]), and must not also drift the persistent fly speed.
fn adjust_fly_speed(
    mouse_button: Res<ButtonInput<MouseButton>>,
    keyboard: Res<ButtonInput<KeyCode>>,
    mut wheel: MessageReader<MouseWheel>,
    mut cams: Query<&mut BigSpaceCameraController, With<EditorCamera>>,
) {
    if !mouse_button.pressed(FLY_CAMERA_BUTTON) || orbit_active(&mouse_button, &keyboard) {
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

/// Orbits `EditorCamera` around its `OrbitFocus` while RMB+Alt are held: drag to rotate around the
/// pivot, scroll to change distance from it. Mutually exclusive with the fly paths --
/// `write_editor_camera_intent` bails out and produces no intent whenever [`orbit_active`] is
/// true, so WASD is a no-op during orbit (the fly paths just apply a no-op intent that frame) and
/// this owns mouse-look input entirely while active, via its own `MessageReader`s so clearing the
/// fly path's readers doesn't consume events this system would otherwise see.
///
/// `orbiting` holds `(yaw, pitch)` in radians while an orbit drag is in progress, `None` when it
/// isn't. The first frame RMB+Alt are both held, it initializes from the camera's *current* facing
/// (the inverse of the spherical mapping below), so orbit continues smoothly from wherever the
/// camera already points instead of snapping; it resets to `None` the frame orbit stops, so the
/// next orbit-start re-initializes cleanly.
fn apply_orbit(
    mouse_button: Res<ButtonInput<MouseButton>>,
    keyboard: Res<ButtonInput<KeyCode>>,
    mut mouse_move: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    mut orbiting: Local<Option<(f32, f32)>>,
    mut cams: Query<(&mut Transform, &mut OrbitFocus), With<EditorCamera>>,
) {
    if !orbit_active(&mouse_button, &keyboard) {
        *orbiting = None;
        mouse_move.clear();
        wheel.clear();
        return;
    }

    let Ok((mut transform, mut focus)) = cams.single_mut() else {
        mouse_move.clear();
        wheel.clear();
        return;
    };

    let (yaw, pitch) = orbiting.get_or_insert_with(|| {
        let forward = transform.forward();
        (
            forward.x.atan2(-forward.z),
            forward.y.clamp(-1.0, 1.0).asin(),
        )
    });

    if let Some(total) = mouse_move.read().map(|e| e.delta).reduce(|sum, i| sum + i) {
        // Opposite sign from `write_editor_camera_intent`'s `-0.1`: a fly-yaw rotation of angle
        // `theta` about world-Y leaves `forward.x == -sin(theta)`, but the spherical convention
        // below has `forward.x == sin(yaw)`, i.e. `yaw == -theta` -- so a *positive* per-pixel
        // factor here reproduces the same on-screen turning direction fly-look's negative one
        // does. Pitch keeps the fly paths' sign: both this convention's `forward.y == sin(pitch)`
        // and a fly-pitch rotation's `forward.y == sin(theta)` agree without a flip.
        *yaw += total.x * ORBIT_MOUSE_SENSITIVITY_DEG_PER_PIXEL.to_radians();
        *pitch += total.y * -ORBIT_MOUSE_SENSITIVITY_DEG_PER_PIXEL.to_radians();
    }
    *pitch = pitch.clamp(-MAX_CAMERA_PITCH, MAX_CAMERA_PITCH);

    let ticks: f64 = wheel.read().map(|event| event.y as f64).sum();
    if ticks != 0.0 {
        let distance = focus.distance as f64;
        focus.distance = if keyboard.pressed(KeyCode::ShiftLeft) {
            distance * SPEED_SCROLL_MULTIPLIER.powf(ticks)
        } else {
            distance + ticks * SPEED_SCROLL_STEP
        }
        .max(MIN_ORBIT_DISTANCE as f64) as f32;
    }

    let forward = Vec3::new(
        yaw.sin() * pitch.cos(),
        pitch.sin(),
        -(yaw.cos() * pitch.cos()),
    );
    transform.translation = focus.point - forward * focus.distance;
    transform.look_to(forward, Vec3::Y);
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

/// Neutralizes roll drift. Both flight paths accumulate rotation incrementally
/// (`transform.rotation *= step`, or big_space's equivalent inside `camera_controller` for the
/// grid-attached path, which this crate doesn't own and can't fix directly) -- the classic
/// FPS-camera bug where mixed pitch+yaw drifts roll over time, because each frame's yaw is applied
/// in the camera's *current*, possibly already-pitched, local frame rather than around true
/// world-Y. Rebuilding the rotation from the resulting forward vector each frame, with world-Y as
/// up, is roll-free by construction and needs no bookkeeping across frames. Runs last in the
/// chain, after both flight paths and after orbit (whose `look_to` output this is a harmless
/// no-op for, since orbit is already roll-free) -- the single place roll gets neutralized,
/// regardless of source.
fn correct_camera_roll(mut cams: Query<&mut Transform, With<EditorCamera>>) {
    let Ok(mut transform) = cams.single_mut() else {
        return;
    };
    let forward = transform.forward().as_vec3();
    // Decompose into the same yaw/pitch spherical convention `apply_orbit` uses, clamp the pitch
    // *scalar*, then rebuild the forward vector from the clamped angles. Clamping a cartesian
    // component of an already-unit vector and renormalizing (an earlier version of this function
    // did exactly that) doesn't work: renormalization rescales the whole vector back up, undoing
    // the clamp and leaving the angle to world-up virtually unchanged. Clamping the angle itself
    // is the only way to guarantee a real minimum horizontal magnitude, so `look_to` never
    // receives a direction parallel to world-up (which would be degenerate).
    let yaw = forward.x.atan2(-forward.z);
    let pitch = forward
        .y
        .clamp(-1.0, 1.0)
        .asin()
        .clamp(-MAX_CAMERA_PITCH, MAX_CAMERA_PITCH);
    let corrected_forward = Vec3::new(
        yaw.sin() * pitch.cos(),
        pitch.sin(),
        -(yaw.cos() * pitch.cos()),
    );
    let Ok(forward) = Dir3::new(corrected_forward) else {
        return;
    };
    transform.look_to(forward, Vec3::Y);
}

/// Radius used to size an F-focus dolly: the target's `Aabb` half-extents scaled by its world
/// scale, vector length -- [`DEFAULT_FOCUS_RADIUS`] when it has no `Aabb` at all.
fn focus_radius(aabb: Option<&Aabb>, world_scale: Vec3) -> f32 {
    match aabb {
        Some(aabb) => (aabb.half_extents * Vec3A::from(world_scale)).length(),
        None => DEFAULT_FOCUS_RADIUS,
    }
}

/// Distance at which a `fov`-radians perspective camera frames a sphere of `radius`, with
/// [`FOCUS_PADDING`] headroom, floored at [`MIN_FOCUS_DISTANCE`].
fn focus_distance(radius: f32, fov: f32) -> f32 {
    (radius / (fov / 2.0).tan() * FOCUS_PADDING).max(MIN_FOCUS_DISTANCE)
}

/// The pure geometry behind F-focus, split out from [`apply_focus_on_f_key`] so it can be unit
/// tested without a `UiState`: dollies `transform` to frame a target at `target_position` with the
/// given `radius`/`fov`, keeping the camera's current facing (Unreal's F-key reframes without
/// reorienting), and points `focus` at the target so a following orbit drag pivots around what was
/// just focused.
fn apply_focus(
    transform: &mut Transform,
    focus: &mut OrbitFocus,
    target_position: Vec3,
    radius: f32,
    fov: f32,
) {
    let distance = focus_distance(radius, fov);
    focus.point = target_position;
    focus.distance = distance;
    let current_forward = transform.forward().as_vec3();
    transform.translation = target_position - current_forward * distance;
}

/// Unreal-style F-key focus. Gated the same way `gizmo::gizmo_keyboard_shortcuts` is -- only while
/// the pointer is over Scene View and it's the active tab -- and only while exactly one entity is
/// selected.
///
/// `UiState` is `Option`al here (unlike the gizmo's own `Res<UiState>`) purely so this system
/// doesn't panic in this module's tests, which build a minimal `App` without `PanelsPlugin`;
/// `PanelsPlugin` always inserts it in the real editor, so `None` never happens outside tests.
fn apply_focus_on_f_key(
    keyboard: Res<ButtonInput<KeyCode>>,
    ui_state: Option<Res<UiState>>,
    targets: Query<(&GlobalTransform, Option<&Aabb>)>,
    mut cams: Query<(&mut Transform, &mut OrbitFocus, Option<&Projection>), With<EditorCamera>>,
) {
    if !keyboard.just_pressed(KeyCode::KeyF) {
        return;
    }
    let Some(ui_state) = ui_state else {
        return;
    };
    if ui_state.active_viewport != ActiveViewport::Scene || !ui_state.pointer_in_viewport {
        return;
    }
    let &[target] = ui_state.selected_entities.as_slice() else {
        return;
    };
    let Ok((target_transform, aabb)) = targets.get(target) else {
        return;
    };
    let Ok((mut transform, mut focus, projection)) = cams.single_mut() else {
        return;
    };

    let radius = focus_radius(aabb, target_transform.scale());
    let fov = match projection {
        Some(Projection::Perspective(perspective)) => perspective.fov,
        _ => DEFAULT_FOCUS_FOV,
    };
    apply_focus(
        &mut transform,
        &mut focus,
        target_transform.translation(),
        radius,
        fov,
    );
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

    #[test]
    fn correct_camera_roll_holds_roll_at_zero_through_mixed_pitch_and_yaw_drag() {
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
        for _ in 0..120 {
            app.world_mut().write_message(MouseMotion {
                delta: Vec2::new(3.0, 1.5),
            });
            app.update();
        }

        // A roll-free camera's `right()` is always horizontal (perpendicular to world-up), no
        // matter its pitch or yaw. Roll drift would tilt it out of the horizontal plane.
        let right = app.world().get::<Transform>(camera).unwrap().right();
        assert!(
            right.y.abs() < 1e-4,
            "expected a horizontal right vector (no roll), got right.y = {}",
            right.y
        );
    }

    #[test]
    fn correct_camera_roll_clamps_pitch_away_from_the_poles() {
        let mut app = test_app();
        app.update();
        let camera = app
            .world_mut()
            .query_filtered::<Entity, With<EditorCamera>>()
            .single(app.world())
            .unwrap();

        // Point the camera almost straight up -- forward.y very close to 1, the degenerate case
        // the pitch clamp exists to avoid. `Quat::from_rotation_arc` sets `forward()` directly,
        // with no "up" reference to go degenerate on, unlike `look_to` would here.
        let near_pole_forward = Vec3::new(0.001, 0.9999995, 0.0).normalize();
        {
            let mut transform = app.world_mut().get_mut::<Transform>(camera).unwrap();
            transform.rotation = Quat::from_rotation_arc(Vec3::NEG_Z, near_pole_forward);
        }
        assert!(
            app.world().get::<Transform>(camera).unwrap().forward().y > 0.999,
            "test setup didn't actually place the camera near the pole"
        );

        app.update();

        let forward_y = app.world().get::<Transform>(camera).unwrap().forward().y;
        let max_sin = MAX_CAMERA_PITCH.sin();
        assert!(
            forward_y <= max_sin + 1e-4,
            "expected pitch clamped to within {MAX_CAMERA_PITCH} rad of level (forward.y <= \
             {max_sin}), got forward.y = {forward_y}"
        );
    }

    #[test]
    fn orbit_keeps_constant_distance_from_focus_point_while_dragging() {
        let mut app = test_app();
        app.update();
        let camera = app
            .world_mut()
            .query_filtered::<Entity, With<EditorCamera>>()
            .single(app.world())
            .unwrap();
        let focus = *app.world().get::<OrbitFocus>(camera).unwrap();

        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Right);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::AltLeft);
        for _ in 0..30 {
            app.world_mut().write_message(MouseMotion {
                delta: Vec2::new(4.0, -2.0),
            });
            app.update();
        }

        let translation = app.world().get::<Transform>(camera).unwrap().translation;
        let distance = translation.distance(focus.point);
        assert!(
            (distance - focus.distance).abs() < 1e-3,
            "expected distance to stay {}, got {distance}",
            focus.distance
        );
        // No scroll happened, so `OrbitFocus::distance` itself must be unchanged too.
        let after = app.world().get::<OrbitFocus>(camera).unwrap();
        assert!((after.distance - focus.distance).abs() < 1e-5);
    }

    #[test]
    fn scrolling_while_orbiting_changes_distance_but_not_fly_speed() {
        let mut app = test_app();
        app.update();
        let camera = app
            .world_mut()
            .query_filtered::<Entity, With<EditorCamera>>()
            .single(app.world())
            .unwrap();
        let starting_speed = app
            .world()
            .get::<BigSpaceCameraController>(camera)
            .unwrap()
            .speed;
        let starting_distance = app.world().get::<OrbitFocus>(camera).unwrap().distance;

        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Right);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::AltLeft);
        app.world_mut()
            .write_message(bevy::input::mouse::MouseWheel {
                unit: bevy::input::mouse::MouseScrollUnit::Line,
                x: 0.0,
                y: 1.0,
                window: Entity::PLACEHOLDER,
                phase: bevy::input::touch::TouchPhase::Moved,
            });
        app.update();

        let distance = app.world().get::<OrbitFocus>(camera).unwrap().distance;
        let speed = app
            .world()
            .get::<BigSpaceCameraController>(camera)
            .unwrap()
            .speed;

        assert!(
            (distance - starting_distance).abs() > 1e-3,
            "expected scrolling during orbit to change OrbitFocus::distance, stayed at {distance}"
        );
        assert_eq!(
            speed, starting_speed,
            "scrolling during orbit must not also drift BigSpaceCameraController::speed"
        );
    }

    #[test]
    fn orbit_and_fly_are_mutually_exclusive_holding_w_applies_no_thrust() {
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
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::AltLeft);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        app.update();
        let after_first_frame = app.world().get::<Transform>(camera).unwrap().translation;

        // No mouse motion from here, so orbit's own (yaw, pitch) is unchanged too -- if W's
        // "forward" thrust were leaking through, the camera would keep moving every frame despite
        // that.
        for _ in 0..30 {
            app.update();
        }
        let after_many_frames = app.world().get::<Transform>(camera).unwrap().translation;

        assert!(
            after_first_frame.distance(after_many_frames) < 1e-4,
            "expected W to be a no-op during orbit, camera moved from {after_first_frame:?} to \
             {after_many_frames:?}"
        );
    }

    #[test]
    fn focus_frames_target_using_its_aabb_and_camera_fov_while_preserving_facing() {
        let aabb = Aabb {
            center: Vec3A::ZERO,
            half_extents: Vec3A::new(1.0, 1.0, 1.0),
        };
        let radius = focus_radius(Some(&aabb), Vec3::ONE);
        assert!(
            (radius - 3.0_f32.sqrt()).abs() < 1e-5,
            "radius should be the length of the (1,1,1) half-extents vector, got {radius}"
        );

        let fov = core::f32::consts::FRAC_PI_2; // 90 degrees: tan(fov/2) == 1
        let expected_distance = radius / (fov / 2.0).tan() * FOCUS_PADDING;
        let distance = focus_distance(radius, fov);
        assert!((distance - expected_distance).abs() < 1e-5);

        let mut transform = Transform::from_xyz(0.0, 0.0, 10.0).looking_at(Vec3::ZERO, Vec3::Y);
        let forward_before = transform.forward().as_vec3();
        let mut focus = OrbitFocus {
            point: Vec3::ZERO,
            distance: 999.0,
        };
        let target_position = Vec3::new(5.0, 0.0, 0.0);

        apply_focus(&mut transform, &mut focus, target_position, radius, fov);

        assert_eq!(focus.point, target_position);
        assert!((focus.distance - expected_distance).abs() < 1e-5);
        assert!(
            transform.forward().as_vec3().distance(forward_before) < 1e-5,
            "F-focus must preserve the camera's current facing"
        );
        let expected_translation = target_position - forward_before * expected_distance;
        assert!(
            transform.translation.distance(expected_translation) < 1e-4,
            "expected translation {expected_translation:?}, got {:?}",
            transform.translation
        );
    }

    #[test]
    fn focus_falls_back_to_default_radius_without_an_aabb() {
        assert_eq!(focus_radius(None, Vec3::ONE), DEFAULT_FOCUS_RADIUS);
    }

    #[test]
    fn orbit_turns_the_same_direction_plain_fly_look_does() {
        // `apply_orbit`'s spherical yaw sign is derived, not copied verbatim from
        // `write_editor_camera_intent`'s `-0.1` (see the comment in `apply_orbit`) -- this locks
        // in that a rightward drag rotates the view the same way whichever path is running.
        fn forward_x_after_rightward_drag(hold_alt: bool) -> f32 {
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
            if hold_alt {
                app.world_mut()
                    .resource_mut::<ButtonInput<KeyCode>>()
                    .press(KeyCode::AltLeft);
            }
            for _ in 0..10 {
                app.world_mut().write_message(MouseMotion {
                    delta: Vec2::new(10.0, 0.0),
                });
                app.update();
            }
            app.world().get::<Transform>(camera).unwrap().forward().x
        }

        let start_x = {
            let mut app = test_app();
            app.update();
            let camera = app
                .world_mut()
                .query_filtered::<Entity, With<EditorCamera>>()
                .single(app.world())
                .unwrap();
            app.world().get::<Transform>(camera).unwrap().forward().x
        };

        let fly_delta = forward_x_after_rightward_drag(false) - start_x;
        let orbit_delta = forward_x_after_rightward_drag(true) - start_x;

        assert!(
            fly_delta.abs() > 1e-3,
            "fly-look didn't turn at all: {fly_delta}"
        );
        assert!(
            orbit_delta.abs() > 1e-3,
            "orbit didn't turn at all: {orbit_delta}"
        );
        assert_eq!(
            fly_delta.signum(),
            orbit_delta.signum(),
            "fly-look and orbit turned opposite ways for the same rightward drag: fly {fly_delta}, \
             orbit {orbit_delta}"
        );
    }
}
