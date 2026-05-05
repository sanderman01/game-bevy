//! Components and systems for working with cameras.
//! Offers [CameraDriver] to select and transition between multiple [VirtualCamera] positions,
//! orientations, and other settings.
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

use bevy::{
    app::{Plugin, PostUpdate},
    ecs::{
        component::Component,
        entity::Entity,
        query::With,
        system::{Query, Res},
    },
    math::Quat,
    reflect::Reflect,
    time::Time,
    transform::components::{GlobalTransform, Transform},
};

pub const DEFAULT_PRIORITY: u32 = 0;
pub const DEFAULT_CHANNEL_MASK: u32 = 1;
pub const DEFAULT_BLEND: Blend = Blend::Cut;
pub const DEFAULT_BLEND_DAMPING: Blend = Blend::Damping(10.0, 0.2);

/// Registers systems for controlling a [CameraDriver] to select, cut, and
/// transition between multiple [VirtualCamera] positions, orientations,
/// and other settings.
pub struct VirtualCameraPlugin;

impl Plugin for VirtualCameraPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        app.register_type::<MainCamera>()
            .register_type::<CameraDriver>()
            .register_type::<VirtualCamera>()
            .add_systems(PostUpdate, update_camera_drivers);
    }
}

/// Marker component. Attach this tag component to indicate the main camera
/// entity. Not used by camera systems. Can be used by other systems to query
///  camera transform position and orientation, or to manipulate the camera for
/// gameplay purposes.
#[derive(Debug, Component, Reflect)]
pub struct MainCamera;

/// Controls camera transform and camera settings, based on the current live
/// VirtualCamera. Attach to an entity with Camera3d or Camera2d component.
#[derive(Debug, Component, Reflect)]
pub struct CameraDriver {
    pub enabled: bool,
    pub channel_mask: u32,
    pub tracking: Tracking,
    pub default_blend: Blend,
}

impl Default for CameraDriver {
    fn default() -> Self {
        Self {
            enabled: true,
            tracking: Default::default(),
            channel_mask: DEFAULT_CHANNEL_MASK,
            default_blend: DEFAULT_BLEND,
        }
    }
}

#[derive(Debug, Default, Reflect)]
pub enum Tracking {
    // The camera driver will use only those virtual cameras that output to
    // channels that are present in the mask.
    // From these, the virtual camera with the highest priority value will be
    // selected.
    // All other VirtualCameras will be ignored.
    #[default]
    PriorityChannel,
    // The camera driver will track the specified virtual camera entity.
    Target(Entity),
}

#[derive(Debug, Reflect)]
pub enum Blend {
    Cut,
    Damping(f32, f32), // TODO Replace with proper interpolation methods and remove this one
                       //EaseInOut,  // TODO
                       //EaseIn,     // TODO
                       //EaseOut,    // TODO
                       //HardIn,     // TODO
                       //HardOut,    // TODO
                       //Linear,     // TODO
                       //Custom,     // TODO
}

/// Attach to any Entity with a transform. Represents a virtual camera within the game scene.
/// The actual camera with the [CameraDriver] can assume the position and orientation of
/// (or smoothly translate to) whichever virtual camera is currently live.
#[derive(Debug, Component, Reflect)]
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

pub fn update_camera_drivers(
    mut driver_cams: Query<(Entity, &mut CameraDriver), With<GlobalTransform>>,
    virtual_cams: Query<(Entity, &VirtualCamera), With<GlobalTransform>>,
    mut transforms: Query<&mut Transform>,
    global_transforms: Query<&GlobalTransform>,
    time: Res<Time>,
) {
    let iter = driver_cams.iter_mut();
    for (cam_driver_entity, cam_driver) in iter {
        if !cam_driver.enabled {
            continue;
        }

        let target_vcam_entity = match cam_driver.tracking {
            Tracking::Target(entity) => virtual_cams.get(entity).ok(),
            Tracking::PriorityChannel => virtual_cams
                .iter()
                .filter(|(_e, vc)| vc.enabled && vc.channel_mask & cam_driver.channel_mask != 0)
                .max_by_key(|(_e, vc)| vc.priority),
        };

        match target_vcam_entity {
            None => continue,
            Some((vcam_entity, _vcam)) => {
                // Impl. Note: Calculate translation difference in global/world
                // space, then apply the difference to the local transform.
                // This helps avoid issues (to some extent) when camera driver
                // and target virtual camera are not in the same hierarchy or at
                // different levels.
                //
                // This method also helps to compatible with big_space grids,
                // which cause the local transform to change very suddenly and
                // extremely whenever an entity gets shifted to another grid
                // cell.

                let vcam_global_transform = global_transforms.get(vcam_entity).unwrap();
                let cam_driver_global_transform = global_transforms.get(cam_driver_entity).unwrap();
                let mut cam_driver_transform = transforms.get_mut(cam_driver_entity).unwrap();

                let target_pos = vcam_global_transform.translation();
                let current_pos = cam_driver_global_transform.translation();
                let target_rot = vcam_global_transform.rotation();
                let current_rot = cam_driver_global_transform.rotation();

                let translation_offset = target_pos - current_pos;
                let rotation_offset = target_rot.mul_quat(current_rot.inverse());

                match cam_driver.default_blend {
                    Blend::Cut => {
                        cam_driver_transform.translation += translation_offset;
                        cam_driver_transform.rotation =
                            rotation_offset * cam_driver_transform.rotation;
                    }
                    // TODO Replace with proper interpolation methods
                    Blend::Damping(translation_damping, rotation_damping) => {
                        cam_driver_transform.translation +=
                            clamp01(time.delta_secs() * translation_damping) * translation_offset;
                        cam_driver_transform.rotation = (Quat::IDENTITY.slerp(
                            rotation_offset,
                            clamp01(time.delta_secs() * rotation_damping),
                        )) * cam_driver_transform.rotation;
                    } // Blend::EaseInOut => todo!(),
                      // Blend::EaseIn => todo!(),
                      // Blend::EaseOut => todo!(),
                      // Blend::HardIn => todo!(),
                      // Blend::HardOut => todo!(),
                      // Blend::Linear => todo!(),
                      // Blend::Custom => todo!(),
                }
            }
        }
    }
}

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}
