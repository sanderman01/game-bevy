//! Confines the editor's scene camera to the Scene View tab, activating it only while that tab
//! is the one the dock shows.
//!
//! The game's camera is not here: it renders offscreen, see `game_view.rs`.

use bevy::{app::TransformGizmoRenderStep, camera::Viewport, prelude::*, window::PrimaryWindow};
use bevy_egui::{EguiPostUpdateSet, EguiZoomFactor, PrimaryEguiContext};

use crate::{
    camera::EditorCamera,
    panels::{ActiveViewport, UiState},
};

pub(crate) struct ViewportPlugin;

impl Plugin for ViewportPlugin {
    fn build(&self, app: &mut App) {
        // The dock rect is only known once the Egui pass has run, and the gizmo's overlay
        // camera copies the viewport during the render step, so a resize reaches the handles in
        // the same frame.
        app.add_systems(
            PostUpdate,
            set_scene_camera_viewport
                .after(EguiPostUpdateSet::EndPass)
                .before(TransformGizmoRenderStep),
        );
    }
}

/// The dock rect in physical pixels, or `None` when it would fall outside the window.
///
/// A viewport reaching past the window edge is rejected rather than clamped: it happens
/// transiently while the window is resizing, and a clamped viewport for one frame reads as a
/// jump.
fn viewport_for(rect: egui::Rect, scale_factor: f32, window_size: UVec2) -> Option<Viewport> {
    let viewport_pos = rect.left_top().to_vec2() * scale_factor;
    let viewport_size = rect.size() * scale_factor;

    let physical_position = UVec2::new(viewport_pos.x as u32, viewport_pos.y as u32);
    let physical_size = UVec2::new(viewport_size.x as u32, viewport_size.y as u32);

    let far_corner = physical_position + physical_size;
    if far_corner.x > window_size.x || far_corner.y > window_size.y {
        return None;
    }

    Some(Viewport {
        physical_position,
        physical_size,
        depth: 0.0..1.0,
    })
}

/// The scene camera always exists, so this is a plain `Single`.
fn set_scene_camera_viewport(
    ui_state: Res<UiState>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut cam: Single<&mut Camera, With<EditorCamera>>,
    zoom_factor: Single<&EguiZoomFactor, With<PrimaryEguiContext>>,
) {
    cam.is_active = ui_state.active_viewport == ActiveViewport::Scene;
    if !cam.is_active {
        return;
    }
    let scale_factor = window.scale_factor() * zoom_factor.zoom_factor;
    if let Some(viewport) =
        viewport_for(ui_state.viewport_rect, scale_factor, window.physical_size())
    {
        cam.viewport = Some(viewport);
    }
}
