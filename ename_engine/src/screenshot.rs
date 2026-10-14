//! Screenshots: capture the primary window, or what one camera rendered, to a PNG file.
//!
//! Spawn a [`ScreenshotRequest`] on a fresh entity. The entity is the request: when the capture
//! finishes, either way, [`ScreenshotFinished`] is triggered on it and it is despawned.
//!
//! ```ignore
//! fn screenshot_game_camera(mut commands: Commands, camera: Single<Entity, With<MainCamera>>) {
//!     commands
//!         .spawn(ScreenshotRequest {
//!             source: CaptureSource::Camera(*camera),
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
//! Requests on the same target in the same frame share one readback with each other and with
//! any video recording, because Bevy captures a target at most once per frame and silently drops
//! the rest.
//!
//! A readback that never arrives fails the request after 5 s of real time. Its Bevy
//! `Screenshot` entity is left for Bevy to clear: a late delivery inserts into it, which would
//! fail had it been despawned. A target whose GPU image is missing never delivers, so that case
//! leaves one inert entity behind. So does another `Screenshot` of the same target spawned
//! elsewhere in the same frame, such as a debug key using Bevy's `save_to_disk`: Bevy keeps one
//! of the two, and if it keeps the other, these requests time out.
//!
//! A request may be despawned at any point; it then simply never reports.
//!
//! The `screenshot.capture` command runs a request as a job (see `crate::command`).

use std::{
    io::{BufWriter, Write as _},
    path::{Path, PathBuf},
    time::Duration,
};

use bevy::{
    camera::RenderTarget,
    prelude::*,
    render::render_resource::TextureFormat,
    tasks::{AsyncComputeTaskPool, Task, futures::check_ready},
    window::PrimaryWindow,
};
use image::ImageFormat;

use crate::capture::CaptureError;

mod command;

pub use crate::readback::CaptureSource;
use crate::readback::{
    FrameReadBack, ReadbackQueue, ReadbackSystems, ReadbackTag, SourceError, add_readback_plugin,
    resolve_source,
};

/// How long a request waits for its readback before failing with [`ScreenshotError::TimedOut`].
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

/// A capture to take. See the module docs.
#[derive(Component, Clone, Debug)]
pub struct ScreenshotRequest {
    pub source: CaptureSource,
    /// Must end in `.png`, any case. A relative path resolves against the process's working
    /// directory. Missing parent directories are created and an existing file is replaced.
    pub path: PathBuf,
}

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

impl CaptureError for ScreenshotError {
    fn kind(&self) -> &'static str {
        match self {
            Self::NotPng(_) => "not_png",
            Self::NotACamera(_) => "not_a_camera",
            Self::CameraInactive(_) => "camera_inactive",
            Self::UnsupportedTarget(_) => "unsupported_target",
            Self::NoWindow => "no_window",
            Self::UnsupportedFormat(_) => "unsupported_format",
            Self::EmptyCrop { .. } => "empty_crop",
            Self::TimedOut(_) => "timed_out",
            Self::Convert(_) => "convert",
            Self::Write { .. } => "write",
        }
    }

    fn is_invalid(&self) -> bool {
        matches!(self, Self::NotPng(_))
    }
}

/// Triggered once on the request entity when its capture finishes, either way.
#[derive(EntityEvent, Clone, Debug)]
pub struct ScreenshotFinished {
    pub entity: Entity,
    pub result: Result<SavedScreenshot, ScreenshotError>,
}

/// Captures [`ScreenshotRequest`]s and writes them to disk, and registers the
/// `screenshot.capture` command.
pub struct ScreenshotPlugin;

impl Plugin for ScreenshotPlugin {
    fn build(&self, app: &mut App) {
        add_readback_plugin(app);
        app.add_systems(Update, (finish_captures, expire_captures))
            // `Last`: a camera's `is_active` and `viewport` are settled by then, including the
            // editor's, which are set in `PostUpdate`.
            .add_systems(Last, start_captures.in_set(ReadbackSystems::Request))
            .add_observer(deliver_capture);
    }

    /// `finish`, so every capture source a plugin added in `build` is in the command's schema.
    fn finish(&self, app: &mut App) {
        command::register(app);
    }
}

/// Where a request is. One component, so the stages cannot overlap; a request without one has
/// not started. Deadlines are `Time<Real>` elapsed time.
#[derive(Component)]
enum CaptureStage {
    Waiting {
        deadline: Duration,
    },
    Writing(Task<Result<SavedScreenshot, ScreenshotError>>),
    /// Terminal: reported, and despawned with the next command flush.
    Done,
}

impl From<SourceError> for ScreenshotError {
    fn from(error: SourceError) -> Self {
        match error {
            SourceError::NotACamera(entity) => Self::NotACamera(entity),
            SourceError::CameraInactive(entity) => Self::CameraInactive(entity),
            SourceError::UnsupportedTarget(entity) => Self::UnsupportedTarget(entity),
            SourceError::NoWindow => Self::NoWindow,
        }
    }
}

fn start_captures(
    mut commands: Commands,
    requests: Query<(Entity, &ScreenshotRequest), Without<CaptureStage>>,
    cameras: Query<(&Camera, &RenderTarget)>,
    primary_window: Query<Entity, With<PrimaryWindow>>,
    mut queue: ResMut<ReadbackQueue>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    let primary_window = primary_window.single().ok();

    for (entity, request) in &requests {
        let resolved = check_png_path(&request.path).and_then(|()| {
            resolve_source(request.source, &cameras, primary_window).map_err(Into::into)
        });
        let source = match resolved {
            Ok(source) => source,
            Err(err) => {
                commands.entity(entity).try_insert(CaptureStage::Done);
                report(&mut commands, entity, Err(err));
                continue;
            }
        };

        commands.entity(entity).try_insert(CaptureStage::Waiting {
            deadline: now + CAPTURE_TIMEOUT,
        });
        let tag = ReadbackTag {
            crop: source.crop,
            seq: 0,
            requested_at: now,
        };
        queue.request(&source, entity, tag);
    }
}

/// Starts writing one readback, unless the request has moved on. Checks the deadline itself, so
/// a late readback fails the same way whether this or [`expire_captures`] runs first.
fn deliver_capture(
    frame: On<FrameReadBack>,
    mut requests: Query<(&ScreenshotRequest, &mut CaptureStage)>,
    time: Res<Time<Real>>,
    mut commands: Commands,
) {
    // A request the game despawned, a recording, or one that has already timed out takes nothing.
    let Ok((request, mut stage)) = requests.get_mut(frame.entity) else {
        return;
    };
    let CaptureStage::Waiting { deadline } = *stage else {
        return;
    };
    let now = time.elapsed();
    if now >= deadline {
        *stage = CaptureStage::Done;
        report(&mut commands, frame.entity, Err(timed_out()));
        return;
    }
    // Shared, so each request's task copies the pixels on its own thread, not on this one.
    let image = frame.image.clone();
    let crop = frame.tag.crop;
    let path = request.path.clone();
    *stage = CaptureStage::Writing(
        AsyncComputeTaskPool::get().spawn(async move { write_png(&image, crop, &path) }),
    );
}

fn finish_captures(mut commands: Commands, mut requests: Query<(Entity, &mut CaptureStage)>) {
    for (entity, mut stage) in &mut requests {
        let CaptureStage::Writing(task) = &mut *stage else {
            continue;
        };
        let Some(result) = check_ready(task) else {
            continue;
        };
        *stage = CaptureStage::Done;
        report(&mut commands, entity, result);
    }
}

fn expire_captures(
    mut commands: Commands,
    mut requests: Query<(Entity, &mut CaptureStage)>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    for (entity, mut stage) in &mut requests {
        match *stage {
            CaptureStage::Waiting { deadline } if now >= deadline => {
                *stage = CaptureStage::Done;
                report(&mut commands, entity, Err(timed_out()));
            }
            _ => {}
        }
    }
}

/// Publishes a finished request and despawns it. The caller has already moved it to
/// [`CaptureStage::Done`], which is what keeps this from running twice for one request. `try_`:
/// an observer of an earlier report may already have despawned this request.
fn report(
    commands: &mut Commands,
    entity: Entity,
    result: Result<SavedScreenshot, ScreenshotError>,
) {
    commands.trigger(ScreenshotFinished { entity, result });
    commands.entity(entity).try_despawn();
}

fn timed_out() -> ScreenshotError {
    ScreenshotError::TimedOut(CAPTURE_TIMEOUT)
}

/// Whether `path` names a PNG. A request is checked with this when it starts; a caller that wants
/// to refuse a bad path before spawning one can check it first.
pub fn check_png_path(path: &Path) -> Result<(), ScreenshotError> {
    match path.extension() {
        Some(extension) if extension.eq_ignore_ascii_case("png") => Ok(()),
        _ => Err(ScreenshotError::NotPng(path.to_path_buf())),
    }
}

/// Converts, crops and writes one readback. Runs on the async compute pool.
fn write_png(
    image: &Image,
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
        .clone()
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
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(directory).map_err(|err| write_error(err.to_string()))?;
    // A uniquely named file in the same directory, renamed into place: a reader never sees a
    // partial PNG, and two captures to one path cannot write into the same temporary file.
    let mut builder = tempfile::Builder::new();
    // `tempfile` makes its files owner-only, and the rename would keep that. 0o666 less the
    // umask is what creating the file directly would have given.
    #[cfg(unix)]
    builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666));
    let mut file = builder
        .tempfile_in(directory)
        .map_err(|err| write_error(err.to_string()))?;
    let mut writer = BufWriter::new(file.as_file_mut());
    image
        .write_to(&mut writer, ImageFormat::Png)
        .map_err(|err| write_error(err.to_string()))?;
    writer.flush().map_err(|err| write_error(err.to_string()))?;
    drop(writer);
    // Durable before it is visible, so a crash cannot leave a truncated file at `path`.
    file.as_file()
        .sync_all()
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
    use crate::readback::ReadbackFanOut;
    use bevy::{
        asset::RenderAssetUsages,
        camera::Viewport,
        render::{
            render_resource::{Extent3d, TextureDimension},
            view::screenshot::{Screenshot, ScreenshotCaptured},
        },
        window::WindowRef,
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
    fn write_png_crops_and_drops_alpha() {
        let dir = tempfile::tempdir().unwrap();
        for format in [TextureFormat::Rgba8UnormSrgb, TextureFormat::Bgra8UnormSrgb] {
            let path = dir.path().join(format!("{format:?}.png"));
            let crop = URect::from_corners(UVec2::new(1, 1), UVec2::new(3, 3));
            let saved = write_png(&test_image(4, 4, format), Some(crop), &path).unwrap();
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
        let saved = write_png(
            &test_image(3, 2, TextureFormat::Rgba8UnormSrgb),
            None,
            &path,
        )
        .unwrap();
        assert_eq!(saved.size, UVec2::new(3, 2));
        assert_eq!(image::open(&path).unwrap().dimensions(), (3, 2));
    }

    #[test]
    fn write_png_rejects_other_formats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.png");
        let rgba = test_image(2, 2, TextureFormat::Rgba8Unorm);
        let hdr = Image::new_fill(
            Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[0; 8],
            TextureFormat::Rgba16Float,
            RenderAssetUsages::default(),
        );
        for image in [rgba, hdr] {
            let format = image.texture_descriptor.format;
            assert_eq!(
                write_png(&image, None, &path),
                Err(ScreenshotError::UnsupportedFormat(format))
            );
        }
        assert!(!path.exists());
    }

    #[test]
    fn write_png_clamps_and_rejects_empty_crops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.png");
        let image = || test_image(4, 4, TextureFormat::Rgba8UnormSrgb);

        let past_edge = URect::from_corners(UVec2::new(2, 2), UVec2::new(10, 10));
        let saved = write_png(&image(), Some(past_edge), &path).unwrap();
        assert_eq!(saved.size, UVec2::new(2, 2));

        let outside = URect::from_corners(UVec2::new(5, 5), UVec2::new(10, 10));
        assert_eq!(
            write_png(&image(), Some(outside), &path),
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
        write_png(
            &test_image(4, 4, TextureFormat::Rgba8UnormSrgb),
            None,
            &path,
        )
        .unwrap();
        write_png(
            &test_image(2, 3, TextureFormat::Rgba8UnormSrgb),
            None,
            &path,
        )
        .unwrap();
        assert_eq!(image::open(&path).unwrap().dimensions(), (2, 3));
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    /// The same mode a plain `File::create` gets, not `tempfile`'s owner-only default.
    #[cfg(unix)]
    #[test]
    fn write_png_gives_the_file_ordinary_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        write_png(
            &test_image(2, 2, TextureFormat::Rgba8UnormSrgb),
            None,
            &path,
        )
        .unwrap();
        let plain = dir.path().join("plain");
        std::fs::File::create(&plain).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), mode(&plain));
    }

    #[test]
    fn write_png_leaves_no_temp_file_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should go makes the final rename fail.
        let path = dir.path().join("taken.png");
        std::fs::create_dir(&path).unwrap();
        let result = write_png(
            &test_image(2, 2, TextureFormat::Rgba8UnormSrgb),
            None,
            &path,
        );
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

    fn request(app: &mut App, source: CaptureSource, path: &Path) -> Entity {
        app.world_mut()
            .spawn(ScreenshotRequest {
                source,
                path: path.to_path_buf(),
            })
            .id()
    }

    fn screenshots(app: &mut App) -> Vec<(Entity, Vec<Entity>)> {
        app.world_mut()
            .query_filtered::<(Entity, &ReadbackFanOut), With<Screenshot>>()
            .iter(app.world())
            .map(|(entity, fan_out)| {
                let members = fan_out.members.iter().map(|(entity, _)| *entity).collect();
                (entity, members)
            })
            .collect()
    }

    fn set_deadline(app: &mut App, entity: Entity, deadline: Duration) {
        let mut stage = app.world_mut().get_mut::<CaptureStage>(entity).unwrap();
        let CaptureStage::Waiting { deadline: d } = &mut *stage else {
            panic!("not waiting");
        };
        *d = deadline;
    }

    /// Delivers a readback and applies the triggers the fan-out queued.
    fn deliver(app: &mut App, screenshot: Entity, image: Image) {
        app.world_mut().trigger(ScreenshotCaptured {
            entity: screenshot,
            image,
        });
        app.world_mut().flush();
    }

    fn finished(app: &App) -> &[(Entity, Result<SavedScreenshot, ScreenshotError>)] {
        &app.world().resource::<Finished>().0
    }

    #[test]
    fn a_bad_path_finishes_once_and_is_not_retried() {
        let mut app = app();
        let entity = request(&mut app, CaptureSource::PrimaryWindow, Path::new("x.jpg"));
        app.update();
        app.update();
        assert_eq!(
            finished(&app),
            [(entity, Err(ScreenshotError::NotPng("x.jpg".into())))]
        );
        assert!(screenshots(&mut app).is_empty());
        assert!(app.world().get_entity(entity).is_err());
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
        let entity = request(&mut app, CaptureSource::Camera(camera), Path::new("x.png"));
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
        let first = request(&mut app, CaptureSource::PrimaryWindow, Path::new("a.png"));
        let second = request(&mut app, CaptureSource::Camera(camera), Path::new("b.png"));
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
        let entity = request(&mut app, CaptureSource::PrimaryWindow, &path);
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
        assert_eq!(finished(&app), [(entity, Ok(saved))]);
        assert!(path.exists());
    }

    #[test]
    fn a_late_capture_times_out_once_whichever_runs_first() {
        for deliver_first in [true, false] {
            let mut app = app();
            let entity = request(&mut app, CaptureSource::PrimaryWindow, Path::new("x.png"));
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

    /// Updates until `count` requests have reported, or gives up.
    fn update_until_finished(app: &mut App, count: usize) {
        for _ in 0..400 {
            app.update();
            if finished(app).len() >= count {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("only {} of {count} requests finished", finished(app).len());
    }

    #[test]
    fn one_readback_is_cropped_per_request_and_requests_despawn() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app();
        let camera = app
            .world_mut()
            .spawn((
                camera(true, Some((UVec2::new(1, 1), UVec2::new(2, 1)))),
                RenderTarget::Window(WindowRef::Primary),
            ))
            .id();
        let whole = dir.path().join("whole.png");
        let cropped = dir.path().join("cropped.png");
        let first = request(&mut app, CaptureSource::PrimaryWindow, &whole);
        let second = request(&mut app, CaptureSource::Camera(camera), &cropped);
        app.update();
        let (screenshot, _) = screenshots(&mut app)[0];
        deliver(
            &mut app,
            screenshot,
            test_image(4, 3, TextureFormat::Bgra8UnormSrgb),
        );
        update_until_finished(&mut app, 2);

        let mut sizes: Vec<_> = finished(&app)
            .iter()
            .map(|(entity, result)| (*entity, result.as_ref().unwrap().size))
            .collect();
        sizes.sort_by_key(|(entity, _)| *entity == second);
        assert_eq!(
            sizes,
            [(first, UVec2::new(4, 3)), (second, UVec2::new(2, 1))]
        );
        let [r, g, b, _] = pixel(1, 1);
        assert_eq!(
            image::open(&cropped).unwrap().to_rgb8().get_pixel(0, 0).0,
            [r, g, b]
        );
        app.update();
        assert!(app.world().get_entity(first).is_err());
        assert!(app.world().get_entity(second).is_err());
    }

    #[test]
    fn a_request_despawned_in_flight_never_reports_and_nothing_panics() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app();
        let waiting = dir.path().join("w.png");
        let writing = dir.path().join("x.png");
        let waiting = request(&mut app, CaptureSource::PrimaryWindow, &waiting);
        let writing = request(&mut app, CaptureSource::PrimaryWindow, &writing);
        app.update();
        let (screenshot, _) = screenshots(&mut app)[0];

        app.world_mut().despawn(waiting);
        deliver(
            &mut app,
            screenshot,
            test_image(2, 2, TextureFormat::Rgba8UnormSrgb),
        );
        assert!(matches!(
            app.world().get::<CaptureStage>(writing),
            Some(CaptureStage::Writing(_))
        ));
        app.world_mut().despawn(writing);
        for _ in 0..20 {
            app.update();
        }
        assert!(finished(&app).is_empty());
    }

    #[test]
    fn a_late_capture_after_a_timeout_is_ignored() {
        let mut app = app();
        let entity = request(&mut app, CaptureSource::PrimaryWindow, Path::new("x.png"));
        app.update();
        let (screenshot, _) = screenshots(&mut app)[0];
        set_deadline(&mut app, entity, Duration::ZERO);
        app.update();
        assert!(app.world().get_entity(entity).is_err());
        deliver(
            &mut app,
            screenshot,
            test_image(2, 2, TextureFormat::Rgba8UnormSrgb),
        );
        app.update();
        assert_eq!(finished(&app), [(entity, Err(timed_out()))]);
    }

    /// A game observer reacting to one report by despawning another request that is reporting in
    /// the same frame must not turn that request's queued commands into a panic.
    #[test]
    fn an_observer_may_despawn_another_reporting_request() {
        let mut app = app();
        let first = request(&mut app, CaptureSource::PrimaryWindow, Path::new("a.jpg"));
        let second = request(&mut app, CaptureSource::PrimaryWindow, Path::new("b.jpg"));
        app.world_mut().entity_mut(first).observe(
            move |_: On<ScreenshotFinished>, mut commands: Commands| {
                commands.entity(second).try_despawn();
            },
        );
        app.world_mut().entity_mut(second).observe(
            move |_: On<ScreenshotFinished>, mut commands: Commands| {
                commands.entity(first).try_despawn();
            },
        );
        app.update();
        app.update();
        assert!(!finished(&app).is_empty());
    }
}
