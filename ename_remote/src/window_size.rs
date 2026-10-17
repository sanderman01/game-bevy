//! `ENAME_WINDOW_SIZE`: the launcher's `window_size`, applied to the primary window.
//!
//! An agent-facing launch variable like `ENAME_BRP_PORT`, so it lives here and a shipping build
//! never reads it. The same size and dock layout give the same view sizes, so captures from two
//! launches compare.
//!
//! The size is physical pixels. `bevy_winit` creates the window from its logical size before it
//! knows the display's scale factor, so setting it once is wrong on a scaled display. The size is
//! applied again each frame it differs, for the first [`APPLY_FRAMES`] frames, and then left to
//! the user and the window manager. A window manager that refuses the size wins.

use bevy::{prelude::*, window::PrimaryWindow};

/// The variable the launcher sets from `window_size`.
pub const WINDOW_SIZE_VAR: &str = "ENAME_WINDOW_SIZE";

/// Frames during which the size is reapplied, long enough for the scale factor to arrive.
const APPLY_FRAMES: u32 = 30;

/// Applies [`WINDOW_SIZE_VAR`] to the primary window when it is set.
pub(crate) struct WindowSizePlugin;

#[derive(Resource)]
struct RequestedSize {
    size: UVec2,
    frames_left: u32,
}

impl Plugin for WindowSizePlugin {
    fn build(&self, app: &mut App) {
        let Some(value) = std::env::var_os(WINDOW_SIZE_VAR) else {
            return;
        };
        match value.to_str().and_then(parse_size) {
            Some(size) => {
                app.insert_resource(RequestedSize {
                    size,
                    frames_left: APPLY_FRAMES,
                })
                .add_systems(
                    PreUpdate,
                    apply_window_size.run_if(resource_exists::<RequestedSize>),
                );
            }
            None => error!(
                "{WINDOW_SIZE_VAR} must be WIDTHxHEIGHT in physical pixels, both above 0, not \
                 {value:?}; keeping the default window size"
            ),
        }
    }
}

/// `1280x800`, both above 0.
fn parse_size(text: &str) -> Option<UVec2> {
    let (width, height) = text.trim().split_once(['x', 'X'])?;
    let width: u32 = width.trim().parse().ok()?;
    let height: u32 = height.trim().parse().ok()?;
    (width > 0 && height > 0).then_some(UVec2::new(width, height))
}

fn apply_window_size(
    mut requested: ResMut<RequestedSize>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut commands: Commands,
) {
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    let size = requested.size;
    if window.resolution.physical_size() != size {
        window.resolution.set_physical_resolution(size.x, size.y);
    }
    requested.frames_left -= 1;
    if requested.frames_left == 0 {
        commands.remove_resource::<RequestedSize>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_width_by_height() {
        assert_eq!(parse_size("1280x800"), Some(UVec2::new(1280, 800)));
        assert_eq!(parse_size(" 1000X700 "), Some(UVec2::new(1000, 700)));
        for bad in ["0x5", "1280", "ax800", "1280x", "-1x5", ""] {
            assert_eq!(parse_size(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_primary_window_gets_the_size() {
        let mut app = App::new();
        app.insert_resource(RequestedSize {
            size: UVec2::new(1280, 800),
            frames_left: 2,
        })
        .add_systems(
            PreUpdate,
            apply_window_size.run_if(resource_exists::<RequestedSize>),
        );
        let window = app
            .world_mut()
            .spawn((Window::default(), PrimaryWindow))
            .id();
        app.update();
        let resolution = &app.world().get::<Window>(window).unwrap().resolution;
        assert_eq!(resolution.physical_size(), UVec2::new(1280, 800));
        app.update();
        assert!(!app.world().contains_resource::<RequestedSize>());
    }
}
