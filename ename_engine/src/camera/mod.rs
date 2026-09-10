//! Components and systems for working with cameras.
//! Offers [CameraDriver] to select and transition between multiple [VirtualCamera] positions,
//! orientations, and other settings.
//!
//! An engine feature: it knows nothing about keys, panels or gameplay. A layer above decides
//! which virtual camera is live; this module decides how the real camera gets there.
//!
//! To setup a camera rig:
//!
//! 0. Add [CameraDriver] to a chosen camera entity which has a Camera2D or
//!    Camera3D component. This component will control this camera entity's
//!    position and orientation, as such this entity should be free to move.
//!
//! 0. Add [VirtualCamera] to an empty Entity, or any entity other than an
//!    actual camera or child thereof.
//!
//! 0. Set the priority on each virtual cameras, manually or at runtime through
//!    some system. The CameraDriver will choose the VirtualCamera with the
//!    highest priority value.
//!    Or alternatively; set an explicit target in tracking field.
//!
//! 0. If you have multiple CameraDriver, then set the channel_mask on each
//!    CameraDriver and VirtualCamera to indicate which camera drivers are affected
//!    by which virtual cameras.

mod driver;
mod virtual_camera;

pub use driver::{
    Blend, CameraDriver, DEFAULT_BLEND, DEFAULT_BLEND_DAMPING, DEFAULT_CHANNEL_MASK,
    DEFAULT_PRIORITY, Tracking,
};
pub use virtual_camera::VirtualCamera;

use bevy::prelude::*;

/// Registers systems for controlling a [`CameraDriver`] to select, cut, and transition between
/// multiple [`VirtualCamera`] positions, orientations, and other settings.
pub struct VirtualCameraPlugin;

impl Plugin for VirtualCameraPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<MainCamera>()
            .register_type::<CameraDriver>()
            .register_type::<VirtualCamera>()
            .add_systems(PostUpdate, driver::update_camera_drivers);
    }
}

/// Marker component. Attach this tag component to indicate the main camera entity. Not used by
/// camera systems. Can be used by other systems to query camera transform position and
/// orientation, or to manipulate the camera for gameplay purposes.
///
/// The editor uses it to tell the game's 3D camera apart from its own egui and gizmo overlay
/// cameras.
#[derive(Debug, Component, Reflect)]
#[reflect(Component)]
pub struct MainCamera;
