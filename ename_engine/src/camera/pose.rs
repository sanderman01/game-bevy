//! A camera pose an agent or a user can read and type: absolute position plus yaw and pitch in
//! degrees, no roll.
//!
//! Yaw 0 looks along -Z and yaw 90 along +X; pitch is positive up. That is the mapping the
//! editor's orbit uses (`yaw = atan2(f.x, -f.z)`, `pitch = asin(f.y)`), so there is one
//! convention. Yaw is normalized to (-180, 180].

use bevy::math::{DQuat, DVec3, EulerRot};
use big_space::prelude::BigSpaceCameraController;

/// Below this horizontal length the forward vector is treated as straight up or down, and the
/// heading comes from the up vector instead.
const POLE_EPSILON: f64 = 1e-6;

/// Position in absolute metres (Y up, the root grid's frame), yaw and pitch in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraPose {
    pub position: DVec3,
    pub yaw: f64,
    pub pitch: f64,
}

impl CameraPose {
    /// The pose of a camera at `position` with `rotation`. Roll is dropped; a pose looking
    /// straight up or down keeps the heading its up vector gives.
    pub fn from_rotation(position: DVec3, rotation: DQuat) -> Self {
        let (yaw, pitch) = yaw_pitch(rotation);
        Self {
            position,
            yaw,
            pitch,
        }
    }
}

/// Roll-free rotation for `yaw` and `pitch` in degrees.
pub fn rotation_from_yaw_pitch(yaw: f64, pitch: f64) -> DQuat {
    DQuat::from_euler(EulerRot::YXZ, -yaw.to_radians(), pitch.to_radians(), 0.0)
}

/// Yaw and pitch in degrees of the view direction of `rotation`.
pub fn yaw_pitch(rotation: DQuat) -> (f64, f64) {
    let forward = rotation * DVec3::NEG_Z;
    let pitch = forward.y.clamp(-1.0, 1.0).asin();
    let horizontal = if forward.x.hypot(forward.z) < POLE_EPSILON {
        // Heading from the up vector: forward = cos(p) h + sin(p) Y, up = -sin(p) h + cos(p) Y.
        let up = rotation * DVec3::Y;
        forward * pitch.cos() - up * pitch.sin()
    } else {
        forward
    };
    let yaw = horizontal.x.atan2(-horizontal.z).to_degrees();
    (normalize_yaw(yaw), pitch.to_degrees())
}

/// The direction from `from` to `to` as yaw and pitch in degrees, or `None` when they coincide.
pub fn yaw_pitch_towards(from: DVec3, to: DVec3) -> Option<(f64, f64)> {
    let direction = (to - from).try_normalize()?;
    let yaw = direction.x.atan2(-direction.z).to_degrees();
    let pitch = direction.y.clamp(-1.0, 1.0).asin().to_degrees();
    Some((normalize_yaw(yaw), pitch))
}

/// The roll of `rotation` in degrees, what [`CameraPose::from_rotation`] drops.
pub fn roll_degrees(rotation: DQuat) -> f64 {
    let (yaw, pitch) = yaw_pitch(rotation);
    let roll_free = rotation_from_yaw_pitch(yaw, pitch);
    let (roll, _, _) = (roll_free.inverse() * rotation).to_euler(EulerRot::ZYX);
    roll.to_degrees()
}

/// `yaw` in degrees, wrapped to (-180, 180].
fn normalize_yaw(yaw: f64) -> f64 {
    let wrapped = yaw.rem_euclid(360.0);
    if wrapped > 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }
}

/// Zeroes the velocity big_space's controller keeps between frames, keeping its settings.
///
/// big_space has no setter for the private velocity, so a rebuilt value is the only way to clear
/// it. Leftover velocity keeps moving a camera after a pose is written, and while the world is
/// paused its decay factor is 0, so it never stops.
pub fn reset_controller_motion(controller: &mut BigSpaceCameraController) {
    let (translation, rotation) = controller.velocity();
    if translation == DVec3::ZERO && rotation == DQuat::IDENTITY {
        return;
    }
    let mut fresh = BigSpaceCameraController::default();
    fresh.smoothness = controller.smoothness;
    fresh.rotational_smoothness = controller.rotational_smoothness;
    fresh.speed = controller.speed;
    fresh.speed_yaw = controller.speed_yaw;
    fresh.speed_pitch = controller.speed_pitch;
    fresh.speed_roll = controller.speed_roll;
    fresh.speed_bounds = controller.speed_bounds;
    fresh.slow_near_objects = controller.slow_near_objects;
    fresh.require_visibility = controller.require_visibility;
    *controller = fresh;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(a: f64, b: f64, what: &str) {
        assert!((a - b).abs() < 1e-6, "{what}: {a} != {b}");
    }

    #[test]
    fn yaw_and_pitch_round_trip_through_a_quaternion() {
        for (yaw, pitch) in [
            (0.0, 0.0),
            (90.0, 0.0),
            (-90.0, -25.0),
            (0.0, -3.0),
            (180.0, 10.0),
            (-135.5, 60.0),
            (45.0, -89.0),
        ] {
            let (back_yaw, back_pitch) = yaw_pitch(rotation_from_yaw_pitch(yaw, pitch));
            assert_close(back_yaw, yaw, "yaw");
            assert_close(back_pitch, pitch, "pitch");
        }
    }

    #[test]
    fn the_convention_matches_the_compass() {
        let ahead = |yaw| rotation_from_yaw_pitch(yaw, 0.0) * DVec3::NEG_Z;
        assert!((ahead(0.0) - DVec3::NEG_Z).length() < 1e-9);
        assert!((ahead(90.0) - DVec3::X).length() < 1e-9);
        assert!((ahead(-90.0) - DVec3::NEG_X).length() < 1e-9);
        let up = rotation_from_yaw_pitch(0.0, 30.0) * DVec3::NEG_Z;
        assert!(up.y > 0.0);
    }

    #[test]
    // The quaternion as a `Transform` copied from the game prints it.
    #[allow(clippy::approx_constant)]
    fn straight_down_keeps_its_heading_and_has_no_nan() {
        let down = DQuat::from_xyzw(-0.707107, 0.0, 0.0, 0.707107).normalize();
        let (yaw, pitch) = yaw_pitch(down);
        assert_close(pitch, -90.0, "pitch");
        assert_close(yaw, 0.0, "yaw");

        let heading_east_down = rotation_from_yaw_pitch(90.0, -90.0);
        let (yaw, pitch) = yaw_pitch(heading_east_down);
        assert_close(yaw, 90.0, "yaw");
        assert_close(pitch, -90.0, "pitch");
        let (yaw, pitch) = yaw_pitch(rotation_from_yaw_pitch(-30.0, 90.0));
        assert_close(yaw, -30.0, "yaw");
        assert_close(pitch, 90.0, "pitch");
    }
}
