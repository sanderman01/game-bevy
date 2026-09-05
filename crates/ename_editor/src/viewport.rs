//! Confines the game camera to the part of the window the dock is not covering.

use bevy::{app::TransformGizmoRenderStep, camera::Viewport, prelude::*, window::PrimaryWindow};
use bevy_egui::{EguiPostUpdateSet, EguiZoomFactor, PrimaryEguiContext};
use ename_engine::camera::MainCamera;

use crate::panels::UiState;

/// Keeps the game camera rendering only into the Game View tab.
pub(crate) struct ViewportPlugin;

impl Plugin for ViewportPlugin {
    fn build(&self, app: &mut App) {
        // The dock rect is only known once the Egui pass has run, and the gizmo's overlay
        // camera copies the viewport during the render step, so a resize reaches the handles in
        // the same frame.
        app.add_systems(
            PostUpdate,
            set_camera_viewport
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

fn set_camera_viewport(
    ui_state: Res<UiState>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut cam: Single<&mut Camera, With<MainCamera>>,
    zoom_factor: Single<&EguiZoomFactor, With<PrimaryEguiContext>>,
) {
    let scale_factor = window.scale_factor() * zoom_factor.zoom_factor;
    if let Some(viewport) =
        viewport_for(ui_state.viewport_rect, scale_factor, window.physical_size())
    {
        cam.viewport = Some(viewport);
    }
}
