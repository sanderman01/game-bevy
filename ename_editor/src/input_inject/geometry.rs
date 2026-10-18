//! Where each coordinate space of `input.send` sits in the window. Every conversion between
//! egui points, physical window pixels and logical pixels lives here.

use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{EguiZoomFactor, PrimaryEguiContext};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    camera::EditorCamera,
    game_view::GameViewTarget,
    panels::{ActiveViewport, UiState},
};

/// The pixel space a step's point is in. Each is the space of a `screenshot.capture` source's
/// full-size PNG, origin top-left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Space {
    /// Physical window pixels.
    #[default]
    Window,
    /// The Scene View's pixels.
    EditorCamera,
    /// The Game View's pixels.
    GameCamera,
}

impl Space {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::EditorCamera => "editor_camera",
            Self::GameCamera => "game_camera",
        }
    }
}

/// Where a space's image is drawn: `min` and `size` in physical window pixels, `image` the size of
/// its PNG. A point maps as `min + p * size / image`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geometry {
    min: Vec2,
    size: Vec2,
    image: UVec2,
}

impl Geometry {
    pub(crate) fn image(&self) -> UVec2 {
        self.image
    }

    pub(crate) fn contains(&self, point: Vec2) -> bool {
        point.cmpge(Vec2::ZERO).all() && point.cmplt(self.image.as_vec2()).all()
    }

    /// Whether a physical window pixel falls on this space's image.
    pub(crate) fn covers(&self, window_point: Vec2) -> bool {
        window_point.cmpge(self.min).all() && window_point.cmplt(self.min + self.size).all()
    }

    /// The physical window pixel of `point` in this space.
    pub(crate) fn to_window(self, point: Vec2) -> Vec2 {
        self.min + point * self.size / self.image.as_vec2()
    }
}

/// Why a space cannot be resolved now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GeometryError {
    NoWindow,
    /// The view of that space is not shown.
    Inactive(Space),
}

/// The primary window: its entity, physical size and scale factor.
pub(crate) fn primary_window(world: &World) -> Option<(Entity, UVec2, f32)> {
    let mut query = world.try_query_filtered::<(Entity, &Window), With<PrimaryWindow>>()?;
    let (entity, window) = query.single(world).ok()?;
    Some((entity, window.physical_size(), window.scale_factor()))
}

pub(crate) fn geometry(world: &World, space: Space) -> Result<Geometry, GeometryError> {
    let (_, window_size, scale_factor) = primary_window(world).ok_or(GeometryError::NoWindow)?;
    let inactive = GeometryError::Inactive(space);
    match space {
        Space::Window => Ok(Geometry {
            min: Vec2::ZERO,
            size: window_size.as_vec2(),
            image: window_size,
        }),
        // The camera's own viewport: exactly the region its screenshot crops.
        Space::EditorCamera => {
            let ui_state = world.get_resource::<UiState>().ok_or(inactive)?;
            if ui_state.active_viewport != ActiveViewport::Scene {
                return Err(inactive);
            }
            let mut cameras = world
                .try_query_filtered::<&Camera, With<EditorCamera>>()
                .ok_or(inactive)?;
            let camera = cameras.single(world).map_err(|_| inactive)?;
            let viewport = camera
                .viewport
                .as_ref()
                .filter(|_| camera.is_active)
                .ok_or(inactive)?;
            Ok(Geometry {
                min: viewport.physical_position.as_vec2(),
                size: viewport.physical_size.as_vec2(),
                image: viewport.physical_size,
            })
        }
        // The tab's rect, as `game_view.rs` sizes the image from it.
        Space::GameCamera => {
            let ui_state = world.get_resource::<UiState>().ok_or(inactive)?;
            if ui_state.active_viewport != ActiveViewport::Game {
                return Err(inactive);
            }
            let image = world
                .get_resource::<GameViewTarget>()
                .and_then(GameViewTarget::image_size)
                .ok_or(inactive)?;
            let points_to_physical = scale_factor * egui_zoom(world);
            let rect = ui_state.viewport_rect;
            Ok(Geometry {
                min: Vec2::new(rect.min.x, rect.min.y) * points_to_physical,
                size: Vec2::new(rect.width(), rect.height()) * points_to_physical,
                image,
            })
        }
    }
}

/// The primary egui context's zoom, 1 without egui.
fn egui_zoom(world: &World) -> f32 {
    world
        .try_query_filtered::<&EguiZoomFactor, With<PrimaryEguiContext>>()
        .and_then(|mut query| query.single(world).ok().map(|zoom| zoom.zoom_factor))
        .unwrap_or(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_image_pixels_into_the_rect_with_scale_and_offset() {
        // An image of 400x200 drawn into a 800x300 rect at (100, 50).
        let geometry = Geometry {
            min: Vec2::new(100.0, 50.0),
            size: Vec2::new(800.0, 300.0),
            image: UVec2::new(400, 200),
        };
        assert_eq!(geometry.to_window(Vec2::ZERO), Vec2::new(100.0, 50.0));
        assert_eq!(
            geometry.to_window(Vec2::new(200.0, 100.0)),
            Vec2::new(500.0, 200.0)
        );
        assert!(geometry.contains(Vec2::new(399.0, 199.0)));
        assert!(!geometry.contains(Vec2::new(400.0, 10.0)));
        assert!(geometry.covers(Vec2::new(899.0, 349.0)));
        assert!(!geometry.covers(Vec2::new(99.0, 60.0)));
    }
}
