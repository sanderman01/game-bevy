//! Screenshots: capture the primary window, or what one camera rendered, to a PNG file.
//!
//! Spawn a [`ScreenshotRequest`] on a fresh entity. The entity is the request: when the capture
//! finishes, either way, [`ScreenshotFinished`] is triggered on it and it is despawned, unless it
//! also has [`RetainScreenshotOutcome`].
//!
//! ```ignore
//! fn screenshot_game_camera(mut commands: Commands, camera: Single<Entity, With<MainCamera>>) {
//!     commands
//!         .spawn(ScreenshotRequest {
//!             source: ScreenshotSource::Camera(*camera),
//!             path: "shots/game.png".into(),
//!         })
//!         .observe(|finished: On<ScreenshotFinished>| info!("{:?}", finished.result));
//! }
//! ```
//!
//! # What a camera capture contains
//!
//! A camera that renders into an image is read back from that image. A camera that renders into
//! a window is a crop, to its viewport, of the final composited window, so anything drawn on top
//! inside that rect (gizmos, UI, other cameras) is in the picture.
//!
//! The camera must be active in the frame the capture starts: an inactive camera's target holds
//! whatever was last drawn there, which would read as live.
//!
//! # Timing
//!
//! Requests start in `Last`, after everything that sets a camera's `is_active` and `viewport`
//! for the frame, so the crop is the viewport the frame was rendered with. The GPU readback
//! arrives a frame or two later; encoding and writing then run on the async compute pool.
//! Requests on the same target in the same frame share one readback, because Bevy captures a
//! target at most once per frame and silently drops the rest.
//!
//! A readback that never arrives fails the request after [`CAPTURE_TIMEOUT`] of real time. Its
//! Bevy `Screenshot` entity is left for Bevy to clear: a late delivery inserts into it, which
//! would fail had it been despawned. A target whose GPU image is missing never delivers, so that
//! case leaves one inert entity behind.

use std::{
    io::Cursor,
    path::{Path, PathBuf},
    time::Duration,
};

use bevy::{
    camera::{NormalizedRenderTarget, RenderTarget},
    prelude::*,
    render::{
        render_resource::TextureFormat,
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    tasks::{AsyncComputeTaskPool, Task, futures::check_ready},
    window::{PrimaryWindow, WindowRef},
};
use image::ImageFormat;

/// How long a request waits for its readback before failing with [`ScreenshotError::TimedOut`].
pub const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a request with [`RetainScreenshotOutcome`] stays alive after it finishes.
pub const OUTCOME_TTL: Duration = Duration::from_secs(30);

/// What to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenshotSource {
    /// The primary window, everything composited.
    PrimaryWindow,
    /// What this camera rendered this frame, cropped to its viewport.
    Camera(Entity),
}

/// A capture to take. See the module docs.
#[derive(Component, Clone, Debug)]
pub struct ScreenshotRequest {
    pub source: ScreenshotSource,
    /// Must end in `.png`, any case. A relative path resolves against the process's working
    /// directory. Missing parent directories are created and an existing file is replaced.
    pub path: PathBuf,
}

/// Keeps a finished request alive for [`OUTCOME_TTL`] with a [`ScreenshotOutcome`], for callers
/// that poll rather than observe.
#[derive(Component, Default)]
pub struct RetainScreenshotOutcome;

/// A PNG that was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedScreenshot {
    pub path: PathBuf,
    /// Physical pixels.
    pub size: UVec2,
}

#[derive(thiserror::Error, Clone, Debug, PartialEq, Eq)]
pub enum ScreenshotError {
    #[error("`{0}` is not a .png path")]
    NotPng(PathBuf),
    #[error("entity {0} is not a camera")]
    NotACamera(Entity),
    #[error("camera {0} is not active this frame, so its target holds a stale image")]
    CameraInactive(Entity),
    #[error("camera {0} renders to a texture view or to nothing, which this API does not capture")]
    UnsupportedTarget(Entity),
    #[error("there is no primary window to capture")]
    NoWindow,
    #[error("the captured texture is {0:?}; only Rgba8UnormSrgb and Bgra8UnormSrgb are supported")]
    UnsupportedFormat(TextureFormat),
    #[error("the viewport {crop:?} has no area inside the {size} image")]
    EmptyCrop { crop: URect, size: UVec2 },
    #[error("the frame was not read back within {0:?}; is the window minimized?")]
    TimedOut(Duration),
    #[error("could not convert the captured texture: {0}")]
    Convert(String),
    #[error("could not write `{path}`: {message}")]
    Write { path: PathBuf, message: String },
}

/// Triggered once on the request entity when its capture finishes, either way.
#[derive(EntityEvent, Clone, Debug)]
pub struct ScreenshotFinished {
    pub entity: Entity,
    pub result: Result<SavedScreenshot, ScreenshotError>,
}

/// The finished result, kept on a request with [`RetainScreenshotOutcome`].
#[derive(Component, Clone, Debug)]
pub struct ScreenshotOutcome(pub Result<SavedScreenshot, ScreenshotError>);

/// Captures [`ScreenshotRequest`]s and writes them to disk.
pub struct ScreenshotPlugin;

impl Plugin for ScreenshotPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (finish_captures, expire_captures))
            // `Last`: a camera's `is_active` and `viewport` are settled by then, including the
            // editor's, which are set in `PostUpdate`.
            .add_systems(Last, start_captures);
    }
}

/// Where a request is. One component, so the stages cannot overlap; a request without one has
/// not started. Deadlines are `Time<Real>` elapsed time.
#[derive(Component)]
enum CaptureStage {
    Waiting {
        crop: Option<URect>,
        deadline: Duration,
    },
    Writing(Task<Result<SavedScreenshot, ScreenshotError>>),
    /// Terminal. Only a request with [`RetainScreenshotOutcome`] stays around in it.
    Done {
        expires: Duration,
    },
}

/// The requests one Bevy `Screenshot` entity delivers to.
#[derive(Component)]
struct ScreenshotFanOut(Vec<Entity>);

fn start_captures(
    mut commands: Commands,
    requests: Query<
        (Entity, &ScreenshotRequest, Has<RetainScreenshotOutcome>),
        Without<CaptureStage>,
    >,
    cameras: Query<(&Camera, &RenderTarget)>,
    primary_window: Query<Entity, With<PrimaryWindow>>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    let primary_window = primary_window.single().ok();
    let mut groups: Vec<(NormalizedRenderTarget, RenderTarget, Vec<Entity>)> = Vec::new();

    for (entity, request, retain) in &requests {
        let resolved = check_png_path(&request.path)
            .and_then(|()| match request.source {
                ScreenshotSource::PrimaryWindow => {
                    Ok((RenderTarget::Window(WindowRef::Primary), None))
                }
                ScreenshotSource::Camera(camera) => {
                    resolve_camera(camera, cameras.get(camera).ok())
                }
            })
            .and_then(|(target, crop)| match target.normalize(primary_window) {
                Some(normalized) => Ok((normalized, target, crop)),
                None => Err(ScreenshotError::NoWindow),
            });
        let (normalized, target, crop) = match resolved {
            Ok(resolved) => resolved,
            Err(err) => {
                commands.entity(entity).insert(CaptureStage::Done {
                    expires: now + OUTCOME_TTL,
                });
                report(&mut commands, entity, retain, Err(err));
                continue;
            }
        };

        commands.entity(entity).insert(CaptureStage::Waiting {
            crop,
            deadline: now + CAPTURE_TIMEOUT,
        });
        match groups.iter_mut().find(|(key, ..)| *key == normalized) {
            Some((.., members)) => members.push(entity),
            None => groups.push((normalized, target, vec![entity])),
        }
    }

    for (_, target, members) in groups {
        commands
            .spawn((Screenshot(target), ScreenshotFanOut(members)))
            .observe(deliver_capture);
    }
}

/// Hands one readback to every request still waiting on it. Checks the deadline itself, so a
/// late readback fails the same way whether this or [`expire_captures`] runs first.
fn deliver_capture(
    captured: On<ScreenshotCaptured>,
    fan_outs: Query<&ScreenshotFanOut>,
    mut requests: Query<(
        &ScreenshotRequest,
        &mut CaptureStage,
        Has<RetainScreenshotOutcome>,
    )>,
    time: Res<Time<Real>>,
    mut commands: Commands,
) {
    let Ok(ScreenshotFanOut(members)) = fan_outs.get(captured.entity) else {
        return;
    };
    let now = time.elapsed();
    for &entity in members {
        // A request the game despawned, or one that has already timed out, takes nothing.
        let Ok((request, mut stage, retain)) = requests.get_mut(entity) else {
            continue;
        };
        let CaptureStage::Waiting { crop, deadline } = *stage else {
            continue;
        };
        if now >= deadline {
            *stage = CaptureStage::Done {
                expires: now + OUTCOME_TTL,
            };
            report(&mut commands, entity, retain, Err(timed_out()));
            continue;
        }
        let image = captured.image.clone();
        let path = request.path.clone();
        *stage = CaptureStage::Writing(
            AsyncComputeTaskPool::get().spawn(async move { write_png(image, crop, &path) }),
        );
    }
}

fn finish_captures(
    mut commands: Commands,
    mut requests: Query<(Entity, &mut CaptureStage, Has<RetainScreenshotOutcome>)>,
    time: Res<Time<Real>>,
) {
    for (entity, mut stage, retain) in &mut requests {
        let CaptureStage::Writing(task) = &mut *stage else {
            continue;
        };
        let Some(result) = check_ready(task) else {
            continue;
        };
        *stage = CaptureStage::Done {
            expires: time.elapsed() + OUTCOME_TTL,
        };
        report(&mut commands, entity, retain, result);
    }
}

fn expire_captures(
    mut commands: Commands,
    mut requests: Query<(Entity, &mut CaptureStage, Has<RetainScreenshotOutcome>)>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    for (entity, mut stage, retain) in &mut requests {
        match *stage {
            CaptureStage::Waiting { deadline, .. } if now >= deadline => {
                *stage = CaptureStage::Done {
                    expires: now + OUTCOME_TTL,
                };
                report(&mut commands, entity, retain, Err(timed_out()));
            }
            CaptureStage::Done { expires } if now >= expires => {
                commands.entity(entity).despawn();
            }
            _ => {}
        }
    }
}

/// Publishes a finished request. The caller has already moved it to [`CaptureStage::Done`],
/// which is what keeps this from running twice for one request.
fn report(
    commands: &mut Commands,
    entity: Entity,
    retain: bool,
    result: Result<SavedScreenshot, ScreenshotError>,
) {
    if retain {
        commands
            .entity(entity)
            .insert(ScreenshotOutcome(result.clone()));
    }
    commands.trigger(ScreenshotFinished { entity, result });
    if !retain {
        commands.entity(entity).despawn();
    }
}

fn timed_out() -> ScreenshotError {
    ScreenshotError::TimedOut(CAPTURE_TIMEOUT)
}

fn check_png_path(path: &Path) -> Result<(), ScreenshotError> {
    match path.extension() {
        Some(extension) if extension.eq_ignore_ascii_case("png") => Ok(()),
        _ => Err(ScreenshotError::NotPng(path.to_path_buf())),
    }
}

/// The target to read back for `entity`'s camera, and the crop that is its viewport.
fn resolve_camera(
    entity: Entity,
    camera: Option<(&Camera, &RenderTarget)>,
) -> Result<(RenderTarget, Option<URect>), ScreenshotError> {
    let Some((camera, target)) = camera else {
        return Err(ScreenshotError::NotACamera(entity));
    };
    if !camera.is_active {
        return Err(ScreenshotError::CameraInactive(entity));
    }
    let target = match target {
        RenderTarget::Window(_) | RenderTarget::Image(_) => target.clone(),
        RenderTarget::TextureView(_) | RenderTarget::None { .. } => {
            return Err(ScreenshotError::UnsupportedTarget(entity));
        }
    };
    let crop = camera.viewport.as_ref().map(|viewport| {
        URect::from_corners(
            viewport.physical_position,
            viewport.physical_position + viewport.physical_size,
        )
    });
    Ok((target, crop))
}

/// Converts, crops and writes one readback. Runs on the async compute pool.
fn write_png(
    image: Image,
    crop: Option<URect>,
    path: &Path,
) -> Result<SavedScreenshot, ScreenshotError> {
    let format = image.texture_descriptor.format;
    if !matches!(
        format,
        TextureFormat::Rgba8UnormSrgb | TextureFormat::Bgra8UnormSrgb
    ) {
        return Err(ScreenshotError::UnsupportedFormat(format));
    }
    let mut image = image
        .try_into_dynamic()
        .map_err(|err| ScreenshotError::Convert(err.to_string()))?;

    if let Some(crop) = crop {
        let size = UVec2::new(image.width(), image.height());
        // Bevy clamps a viewport to its target the same way, so this is the rect it drew.
        let area = crop.intersect(URect::from_corners(UVec2::ZERO, size));
        if area.is_empty() {
            return Err(ScreenshotError::EmptyCrop { crop, size });
        }
        image = image.crop_imm(area.min.x, area.min.y, area.width(), area.height());
    }
    // Alpha holds brightness on an HDR target and nothing useful on a window, as in Bevy's own
    // `save_to_disk`.
    let image = image.to_rgb8();

    let write_error = |message: String| ScreenshotError::Write {
        path: path.to_path_buf(),
        message,
    };
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|err| write_error(err.to_string()))?;

    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(directory).map_err(|err| write_error(err.to_string()))?;
    // A uniquely named file in the same directory, renamed into place: a reader never sees a
    // partial PNG, and two captures to one path cannot write into the same temporary file.
    let mut file =
        tempfile::NamedTempFile::new_in(directory).map_err(|err| write_error(err.to_string()))?;
    std::io::Write::write_all(&mut file, png.get_ref())
        .map_err(|err| write_error(err.to_string()))?;
    file.persist(path)
        .map_err(|err| write_error(err.error.to_string()))?;

    Ok(SavedScreenshot {
        path: path.to_path_buf(),
        size: UVec2::new(image.width(), image.height()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        asset::RenderAssetUsages,
        camera::{ManualTextureViewHandle, Viewport},
        render::render_resource::{Extent3d, TextureDimension},
    };
    use image::GenericImageView as _;

    /// Pixel `(x, y)` of a test image, as RGBA. Distinct per pixel so a crop or a channel swap
    /// shows up.
    fn pixel(x: u32, y: u32) -> [u8; 4] {
        [x as u8 * 40, y as u8 * 40, 200, 255]
    }

    fn test_image(width: u32, height: u32, format: TextureFormat) -> Image {
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let [r, g, b, a] = pixel(x, y);
                match format {
                    TextureFormat::Bgra8UnormSrgb => data.extend([b, g, r, a]),
                    _ => data.extend([r, g, b, a]),
                }
            }
        }
        Image::new(
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            data,
            format,
            RenderAssetUsages::default(),
        )
    }

    fn camera(active: bool, viewport: Option<(UVec2, UVec2)>) -> Camera {
        Camera {
            is_active: active,
            viewport: viewport.map(|(physical_position, physical_size)| Viewport {
                physical_position,
                physical_size,
                ..default()
            }),
            ..default()
        }
    }

    fn entity() -> Entity {
        Entity::from_raw_u32(7).unwrap()
    }

    #[test]
    fn path_must_end_in_png() {
        assert!(check_png_path(Path::new("a/b.png")).is_ok());
        assert!(check_png_path(Path::new("a/b.PNG")).is_ok());
        for bad in ["a/b.jpg", "a/b", "a/png"] {
            assert_eq!(
                check_png_path(Path::new(bad)),
                Err(ScreenshotError::NotPng(bad.into()))
            );
        }
    }

    #[test]
    fn resolve_camera_into_an_image_uses_the_image_and_its_viewport() {
        let image = Handle::<Image>::default();
        let target = RenderTarget::Image(image.clone().into());
        let camera = camera(true, Some((UVec2::new(2, 3), UVec2::new(10, 20))));
        let (resolved, crop) = resolve_camera(entity(), Some((&camera, &target))).unwrap();
        assert_eq!(resolved.as_image(), Some(&image));
        assert_eq!(
            crop,
            Some(URect::from_corners(UVec2::new(2, 3), UVec2::new(12, 23)))
        );
    }

    #[test]
    fn resolve_camera_into_a_window_without_viewport_has_no_crop() {
        let target = RenderTarget::Window(WindowRef::Primary);
        let (resolved, crop) =
            resolve_camera(entity(), Some((&camera(true, None), &target))).unwrap();
        assert!(matches!(resolved, RenderTarget::Window(WindowRef::Primary)));
        assert_eq!(crop, None);
    }

    #[test]
    fn resolve_camera_rejects_what_it_cannot_capture() {
        let window = RenderTarget::Window(WindowRef::Primary);
        assert_eq!(
            resolve_camera(entity(), None).unwrap_err(),
            ScreenshotError::NotACamera(entity())
        );
        assert_eq!(
            resolve_camera(entity(), Some((&camera(false, None), &window))).unwrap_err(),
            ScreenshotError::CameraInactive(entity())
        );
        for target in [
            RenderTarget::TextureView(ManualTextureViewHandle(0)),
            RenderTarget::None { size: UVec2::ONE },
        ] {
            assert_eq!(
                resolve_camera(entity(), Some((&camera(true, None), &target))).unwrap_err(),
                ScreenshotError::UnsupportedTarget(entity())
            );
        }
    }

    #[test]
    fn write_png_crops_and_drops_alpha() {
        let dir = tempfile::tempdir().unwrap();
        for format in [TextureFormat::Rgba8UnormSrgb, TextureFormat::Bgra8UnormSrgb] {
            let path = dir.path().join(format!("{format:?}.png"));
            let crop = URect::from_corners(UVec2::new(1, 1), UVec2::new(3, 3));
            let saved = write_png(test_image(4, 4, format), Some(crop), &path).unwrap();
            assert_eq!(saved.size, UVec2::new(2, 2));

            let written = image::open(&path).unwrap();
            assert_eq!(written.color(), image::ColorType::Rgb8, "{format:?}");
            let [r, g, b, _] = pixel(2, 1);
            assert_eq!(written.to_rgb8().get_pixel(1, 0).0, [r, g, b], "{format:?}");
        }
    }

    #[test]
    fn write_png_handles_odd_width() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("odd.png");
        let saved =
            write_png(test_image(3, 2, TextureFormat::Rgba8UnormSrgb), None, &path).unwrap();
        assert_eq!(saved.size, UVec2::new(3, 2));
        assert_eq!(image::open(&path).unwrap().dimensions(), (3, 2));
    }

    #[test]
    fn write_png_rejects_other_formats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.png");
        let image = test_image(2, 2, TextureFormat::Rgba8Unorm);
        assert_eq!(
            write_png(image, None, &path),
            Err(ScreenshotError::UnsupportedFormat(
                TextureFormat::Rgba8Unorm
            ))
        );
        assert!(!path.exists());
    }

    #[test]
    fn write_png_clamps_and_rejects_empty_crops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.png");
        let image = || test_image(4, 4, TextureFormat::Rgba8UnormSrgb);

        let past_edge = URect::from_corners(UVec2::new(2, 2), UVec2::new(10, 10));
        let saved = write_png(image(), Some(past_edge), &path).unwrap();
        assert_eq!(saved.size, UVec2::new(2, 2));

        let outside = URect::from_corners(UVec2::new(5, 5), UVec2::new(10, 10));
        assert_eq!(
            write_png(image(), Some(outside), &path),
            Err(ScreenshotError::EmptyCrop {
                crop: outside,
                size: UVec2::new(4, 4)
            })
        );
    }

    #[test]
    fn write_png_creates_parents_and_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/shot.png");
        write_png(test_image(4, 4, TextureFormat::Rgba8UnormSrgb), None, &path).unwrap();
        write_png(test_image(2, 3, TextureFormat::Rgba8UnormSrgb), None, &path).unwrap();
        assert_eq!(image::open(&path).unwrap().dimensions(), (2, 3));
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn write_png_leaves_no_temp_file_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should go makes the final rename fail.
        let path = dir.path().join("taken.png");
        std::fs::create_dir(&path).unwrap();
        let result = write_png(test_image(2, 2, TextureFormat::Rgba8UnormSrgb), None, &path);
        assert!(matches!(result, Err(ScreenshotError::Write { .. })));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    /// Every [`ScreenshotFinished`] seen, in order.
    #[derive(Resource, Default)]
    struct Finished(Vec<(Entity, Result<SavedScreenshot, ScreenshotError>)>);

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, ScreenshotPlugin))
            .init_resource::<Finished>()
            .add_observer(
                |finished: On<ScreenshotFinished>, mut seen: ResMut<Finished>| {
                    seen.0.push((finished.entity, finished.result.clone()));
                },
            );
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        app
    }

    fn request(app: &mut App, source: ScreenshotSource, path: &Path, retain: bool) -> Entity {
        let mut entity = app.world_mut().spawn(ScreenshotRequest {
            source,
            path: path.to_path_buf(),
        });
        if retain {
            entity.insert(RetainScreenshotOutcome);
        }
        entity.id()
    }

    fn screenshots(app: &mut App) -> Vec<(Entity, Vec<Entity>)> {
        app.world_mut()
            .query_filtered::<(Entity, &ScreenshotFanOut), With<Screenshot>>()
            .iter(app.world())
            .map(|(entity, fan_out)| (entity, fan_out.0.clone()))
            .collect()
    }

    fn set_deadline(app: &mut App, entity: Entity, deadline: Duration) {
        let mut stage = app.world_mut().get_mut::<CaptureStage>(entity).unwrap();
        let CaptureStage::Waiting { deadline: d, .. } = &mut *stage else {
            panic!("not waiting");
        };
        *d = deadline;
    }

    fn deliver(app: &mut App, screenshot: Entity, image: Image) {
        app.world_mut().trigger(ScreenshotCaptured {
            entity: screenshot,
            image,
        });
    }

    fn finished(app: &App) -> &[(Entity, Result<SavedScreenshot, ScreenshotError>)] {
        &app.world().resource::<Finished>().0
    }

    #[test]
    fn a_bad_path_finishes_once_and_is_not_retried() {
        let mut app = app();
        let entity = request(
            &mut app,
            ScreenshotSource::PrimaryWindow,
            Path::new("x.jpg"),
            true,
        );
        app.update();
        app.update();
        assert_eq!(
            finished(&app),
            [(entity, Err(ScreenshotError::NotPng("x.jpg".into())))]
        );
        assert!(screenshots(&mut app).is_empty());
        assert!(app.world().get::<ScreenshotOutcome>(entity).is_some());
    }

    #[test]
    fn an_inactive_camera_finishes_once_and_the_request_is_despawned() {
        let mut app = app();
        let camera = app
            .world_mut()
            .spawn((
                camera(false, None),
                RenderTarget::Window(WindowRef::Primary),
            ))
            .id();
        let entity = request(
            &mut app,
            ScreenshotSource::Camera(camera),
            Path::new("x.png"),
            false,
        );
        app.update();
        app.update();
        assert_eq!(
            finished(&app),
            [(entity, Err(ScreenshotError::CameraInactive(camera)))]
        );
        assert!(app.world().get_entity(entity).is_err());
    }

    #[test]
    fn requests_on_one_target_share_one_screenshot() {
        let mut app = app();
        let camera = app
            .world_mut()
            .spawn((camera(true, None), RenderTarget::Window(WindowRef::Primary)))
            .id();
        let first = request(
            &mut app,
            ScreenshotSource::PrimaryWindow,
            Path::new("a.png"),
            false,
        );
        let second = request(
            &mut app,
            ScreenshotSource::Camera(camera),
            Path::new("b.png"),
            false,
        );
        app.update();
        let shots = screenshots(&mut app);
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].1, [first, second]);
    }

    #[test]
    fn a_delivered_capture_is_written_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        let mut app = app();
        let entity = request(&mut app, ScreenshotSource::PrimaryWindow, &path, true);
        app.update();
        let (screenshot, _) = screenshots(&mut app)[0];
        deliver(
            &mut app,
            screenshot,
            test_image(4, 2, TextureFormat::Bgra8UnormSrgb),
        );

        for _ in 0..200 {
            app.update();
            if !finished(&app).is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let saved = SavedScreenshot {
            path: path.clone(),
            size: UVec2::new(4, 2),
        };
        assert_eq!(finished(&app), [(entity, Ok(saved.clone()))]);
        assert_eq!(
            app.world().get::<ScreenshotOutcome>(entity).unwrap().0,
            Ok(saved)
        );
        assert!(path.exists());
    }

    #[test]
    fn a_late_capture_times_out_once_whichever_runs_first() {
        for deliver_first in [true, false] {
            let mut app = app();
            let entity = request(
                &mut app,
                ScreenshotSource::PrimaryWindow,
                Path::new("x.png"),
                true,
            );
            app.update();
            let (screenshot, _) = screenshots(&mut app)[0];
            set_deadline(&mut app, entity, Duration::ZERO);

            let image = test_image(2, 2, TextureFormat::Rgba8UnormSrgb);
            if deliver_first {
                deliver(&mut app, screenshot, image);
                app.update();
            } else {
                app.update();
                deliver(&mut app, screenshot, image);
            }
            app.update();
            assert_eq!(
                finished(&app),
                [(entity, Err(timed_out()))],
                "deliver_first: {deliver_first}"
            );
        }
    }

    #[test]
    fn a_retained_outcome_expires() {
        let mut app = app();
        let entity = request(
            &mut app,
            ScreenshotSource::PrimaryWindow,
            Path::new("x.jpg"),
            true,
        );
        app.update();
        assert!(app.world().get_entity(entity).is_ok());
        *app.world_mut().get_mut::<CaptureStage>(entity).unwrap() = CaptureStage::Done {
            expires: Duration::ZERO,
        };
        app.update();
        assert!(app.world().get_entity(entity).is_err());
    }
}
