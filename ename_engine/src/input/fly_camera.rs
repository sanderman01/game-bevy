//! Applies [`FlyCameraIntent`] to big_space's fly camera.

use bevy::prelude::*;
use big_space::camera::{BigSpaceCameraInput, camera_controller};

/// Frame-local movement intent for the fly camera, in big_space's aircraft axes.
///
/// The engine owns the big_space glue; whichever layer owns the bindings writes this. There is
/// deliberately no `KeyCode` anywhere in it: the editor writes it from its viewport bindings
/// today, and a spectator mode or a replay could write it instead without either side changing.
///
/// Cleared every frame after it is applied, so a writer that stops writing stops the camera.
#[derive(Resource, Debug, Clone, Default, Reflect)]
#[reflect(Resource, Default)]
pub struct FlyCameraIntent {
    /// Z-negative.
    pub forward: f64,
    /// Y-positive.
    pub up: f64,
    /// X-positive.
    pub right: f64,
    /// Positive = right wing down.
    pub roll: f64,
    /// Positive = nose up.
    pub pitch: f64,
    /// Positive = nose right.
    pub yaw: f64,
    /// Speed modifier, e.g. "sprint".
    pub boost: bool,
}

impl FlyCameraIntent {
    /// Resets every axis to zero.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Ordering points for the fly camera.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum FlyCameraSystems {
    /// Where a layer above the engine writes [`FlyCameraIntent`].
    Intent,
    /// Copies the intent into big_space's input resource and clears it. Runs after
    /// [`FlyCameraSystems::Intent`] and before big_space consumes the result.
    ///
    /// This is the only place in the workspace that names
    /// `big_space::camera::camera_controller`, so a big_space upgrade that renames or splits
    /// that system is a one-line fix here.
    Apply,
}

/// Drives big_space's fly camera from [`FlyCameraIntent`].
pub struct FlyCameraPlugin;

impl Plugin for FlyCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FlyCameraIntent>()
            .register_type::<FlyCameraIntent>()
            .configure_sets(
                PostUpdate,
                (FlyCameraSystems::Intent, FlyCameraSystems::Apply)
                    .chain()
                    .before(camera_controller),
            )
            .add_systems(
                PostUpdate,
                apply_fly_camera_intent.in_set(FlyCameraSystems::Apply),
            );
    }
}

/// big_space's own bindings are switched off with `defaults_disabled`, so this is the only
/// writer of `BigSpaceCameraInput`.
fn apply_fly_camera_intent(
    mut intent: ResMut<FlyCameraIntent>,
    mut cam: ResMut<BigSpaceCameraInput>,
) {
    cam.reset();
    cam.defaults_disabled = true;

    cam.forward = intent.forward;
    cam.up = intent.up;
    cam.right = intent.right;
    cam.roll = intent.roll;
    cam.pitch = intent.pitch;
    cam.yaw = intent.yaw;
    cam.boost = intent.boost;

    intent.clear();
}
