//! A pose the camera driver can be pointed at.

use bevy::{
    ecs::{component::Component, reflect::ReflectComponent},
    reflect::Reflect,
};

use crate::camera::driver::{Blend, DEFAULT_CHANNEL_MASK, DEFAULT_PRIORITY};

/// Attach to any Entity with a transform. Represents a virtual camera within the game scene.
/// The actual camera with the [`CameraDriver`](crate::camera::CameraDriver) can assume the
/// position and orientation of (or smoothly translate to) whichever virtual camera is currently
/// live.
#[derive(Debug, Component, Reflect)]
#[reflect(Component)]
pub struct VirtualCamera {
    pub enabled: bool,
    pub channel_mask: u32,
    pub priority: u32,
    pub blend: Option<Blend>,
}

impl Default for VirtualCamera {
    fn default() -> Self {
        Self {
            enabled: true,
            priority: DEFAULT_PRIORITY,
            channel_mask: DEFAULT_CHANNEL_MASK,
            blend: None,
        }
    }
}
