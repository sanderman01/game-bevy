//! The Game View tab: the stage's own camera, rendered to an offscreen image that egui paints
//! into the tab.
//!
//! Scene View draws straight into the window behind egui, and Game View cannot do the same. The
//! egui camera composites the window by *loading* whatever is already in its view target rather
//! than clearing it, and Bevy keys those targets by `(target, texture usages, format, MSAA)` in
//! `prepare_view_targets`. A stage camera that carries `Hdr` -- or anything else that moves it to
//! a different key -- renders into a texture the egui camera never reads, and the egui camera's
//! own blit then overwrites the window with it. An image target of the editor's own takes the
//! stage camera out of that composite entirely, so what a stage sets on its camera stops being
//! the editor's business.

use bevy::{
    camera::RenderTarget,
    prelude::*,
    render::render_resource::{Extent3d, TextureFormat},
    window::PrimaryWindow,
};
use bevy_egui::{
    EguiPostUpdateSet, EguiTextureHandle, EguiUserTextures, EguiZoomFactor, PrimaryEguiContext,
};
use ename_engine::camera::MainCamera;

use crate::panels::{ActiveViewport, UiState};

/// What the offscreen image is sized to before the tab has ever been laid out. Never seen: the
/// first paint comes after the first [`resize_game_view`].
const INITIAL_SIZE: UVec2 = UVec2::splat(64);

/// Renders the stage's [`MainCamera`] into an image and paints it in the Game View tab.
///
/// Registers [`GameViewTarget`], the image it owns, and the systems that keep that image's size
/// and the camera's target in step with the tab.
pub(crate) struct GameViewPlugin;

impl Plugin for GameViewPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, create_game_view_target)
            // The tab rect this reads is only known once the egui pass has produced it.
            .add_systems(
                PostUpdate,
                resize_game_view.after(EguiPostUpdateSet::EndPass),
            );
    }
}

/// The image the stage camera renders into, and the egui texture that paints it.
#[derive(Resource)]
pub(crate) struct GameViewTarget {
    image: Handle<Image>,
    texture: egui::TextureId,
    /// Size of `image`, in physical pixels.
    size: UVec2,
    /// Whether the loaded stage has a [`MainCamera`] at all. False leaves `image` holding the
    /// last stage's final frame, which would read as live.
    camera_present: bool,
}

fn create_game_view_target(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut user_textures: ResMut<EguiUserTextures>,
) {
    // sRGB, so the sampler hands egui's shader linear values -- which is what it converts *from*
    // when it multiplies a user texture by the vertex colour.
    let mut image = Image::new_target_texture(
        INITIAL_SIZE.x,
        INITIAL_SIZE.y,
        TextureFormat::Rgba8UnormSrgb,
        None,
    );
    // No CPU side at all, and not only to save a buffer the size of the tab. Extraction *moves*
    // an image's data into the render world, and then refuses any later version of an image that
    // once had data and no longer does, reading it as already extracted. So an image born with
    // data reaches the GPU once and every resize after that is silently dropped, while an image
    // born without data resizes for the rest of its life.
    image.data = None;
    // `new_target_texture` asks for the old contents to be blitted into the new texture on every
    // resize, which needs `COPY_SRC` it does not also ask for -- a validation error that takes the
    // app down the first time the tab changes size. The camera redraws the whole target anyway.
    image.copy_on_resize = false;
    let image = images.add(image);
    let texture = user_textures.add_image(EguiTextureHandle::Strong(image.clone()));
    commands.insert_resource(GameViewTarget {
        image,
        texture,
        size: INITIAL_SIZE,
        camera_present: true,
    });
}

/// Points the stage camera at [`GameViewTarget`]'s image, sizes that image to the tab, and
/// deactivates the camera while the tab is not the visible one.
///
/// `MainCamera` may not exist at all (no stage loaded, or a loaded stage that defines no camera),
/// which is why the camera is optional rather than a plain `Single`.
fn resize_game_view(
    ui_state: Res<UiState>,
    window: Single<&Window, With<PrimaryWindow>>,
    zoom_factor: Single<&EguiZoomFactor, With<PrimaryEguiContext>>,
    camera: Option<Single<(&mut Camera, &mut RenderTarget), With<MainCamera>>>,
    mut target: ResMut<GameViewTarget>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(camera) = camera else {
        if target.camera_present {
            target.camera_present = false;
        }
        return;
    };
    let (mut camera, mut render_target) = camera.into_inner();

    if !target.camera_present {
        target.camera_present = true;
    }

    let active = ui_state.active_viewport == ActiveViewport::Game;
    if camera.is_active != active {
        camera.is_active = active;
    }
    if !active {
        return;
    }

    if render_target.as_image() != Some(&target.image) {
        *render_target = RenderTarget::Image(target.image.clone().into());
    }
    // A viewport carves a rect out of the target; the image *is* the tab, so there is nothing to
    // carve. A stage authored with one, or a save made by an older editor, would crop the view.
    if camera.viewport.is_some() {
        camera.viewport = None;
    }

    let size = ui_state.viewport_rect.size() * window.scale_factor() * zoom_factor.zoom_factor;
    let size = UVec2::new(size.x as u32, size.y as u32).max(UVec2::ONE);
    if size != target.size
        && let Some(mut image) = images.get_mut(&target.image)
    {
        image.resize(Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        });
        target.size = size;
    }
}

/// Paints the Game View tab. Called from the dock's tab viewer, which owns the `Ui`.
pub(crate) fn ui(ui: &mut egui::Ui, world: &World) {
    let Some(target) = world.get_resource::<GameViewTarget>() else {
        return;
    };

    if !target.camera_present {
        ui.centered_and_justified(|ui| ui.label("The loaded stage has no Main Camera."));
        return;
    }

    // Painted rather than added as a widget: the image is already sized to exactly this rect, and
    // it is the same rect `resize_game_view` measures, so a widget's own layout could only
    // disagree with it.
    ui.painter().image(
        target.texture,
        ui.clip_rect(),
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}
