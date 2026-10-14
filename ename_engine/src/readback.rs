//! Shared GPU readback for screenshots and video: one Bevy `Screenshot` per target per frame,
//! fanned out to every request that asked for that target.
//!
//! Bevy captures a target at most once per frame and silently drops a second `Screenshot` of it,
//! so consumers never spawn their own. They queue a request in [`ReadbackSystems::Request`];
//! [`ReadbackSystems::Spawn`] then spawns one `Screenshot` per target, and the delivered image
//! comes back to each requester as a [`FrameReadBack`] carrying the tag it queued.
//!
//! Readbacks of one target in flight are counted from spawn until Bevy delivers them or the
//! `Screenshot` entity goes away undelivered (Bevy drops a second same-target `Screenshot` in a
//! frame, such as one from a debug key). A [`Limit::Bounded`] request is refused once
//! [`MAX_IN_FLIGHT`] are out. This limits how fast new GPU buffers are requested; it is an
//! admission limit, not a hard bound. A readback that never arrives and is never despawned stops
//! counting after [`RECLAIM_AFTER`], so a target is never blocked for good, but whatever Bevy
//! still holds for it is not freed by that.

use std::{sync::Arc, time::Duration};

use bevy::{
    camera::{NormalizedRenderTarget, RenderTarget},
    ecs::{lifecycle::HookContext, world::DeferredWorld},
    platform::collections::HashMap,
    prelude::*,
    render::view::screenshot::{Screenshot, ScreenshotCaptured},
    window::WindowRef,
};

/// Readbacks of one target that may be in flight before a bounded request is refused. A
/// readback takes two to three frames under pipelined rendering, so four leaves one to spare.
pub(crate) const MAX_IN_FLIGHT: u32 = 4;

/// After this long an undelivered readback no longer counts against its target. Far longer than
/// any real delivery, which takes a few frames.
const RECLAIM_AFTER: Duration = Duration::from_secs(10);

/// What to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureSource {
    /// The primary window, everything composited.
    PrimaryWindow,
    /// What this camera rendered this frame, cropped to its viewport.
    Camera(Entity),
}

/// Why a source cannot be captured this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceError {
    NotACamera(Entity),
    CameraInactive(Entity),
    UnsupportedTarget(Entity),
    NoWindow,
}

/// A source resolved for this frame.
pub(crate) struct ResolvedSource {
    pub key: NormalizedRenderTarget,
    pub target: RenderTarget,
    pub crop: Option<URect>,
}

/// Resolves `source` against this frame's cameras.
pub(crate) fn resolve_source(
    source: CaptureSource,
    cameras: &Query<(&Camera, &RenderTarget)>,
    primary_window: Option<Entity>,
) -> Result<ResolvedSource, SourceError> {
    let (target, crop) = match source {
        CaptureSource::PrimaryWindow => (RenderTarget::Window(WindowRef::Primary), None),
        CaptureSource::Camera(camera) => resolve_camera(camera, cameras.get(camera).ok())?,
    };
    let key = target
        .normalize(primary_window)
        .ok_or(SourceError::NoWindow)?;
    Ok(ResolvedSource { key, target, crop })
}

/// The target to read back for `entity`'s camera, and the crop that is its viewport.
pub(crate) fn resolve_camera(
    entity: Entity,
    camera: Option<(&Camera, &RenderTarget)>,
) -> Result<(RenderTarget, Option<URect>), SourceError> {
    let Some((camera, target)) = camera else {
        return Err(SourceError::NotACamera(entity));
    };
    if !camera.is_active {
        return Err(SourceError::CameraInactive(entity));
    }
    let target = match target {
        RenderTarget::Window(_) | RenderTarget::Image(_) => target.clone(),
        RenderTarget::TextureView(_) | RenderTarget::None { .. } => {
            return Err(SourceError::UnsupportedTarget(entity));
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

/// What a requester knew when it asked, handed back with the image. The crop travels with the
/// readback so a late image is cropped with the rect it was rendered with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReadbackTag {
    pub crop: Option<URect>,
    pub seq: u64,
    /// `Time<Real>` elapsed when requested.
    pub requested_at: Duration,
}

/// Whether a request may be refused to bound the readbacks in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Limit {
    Bounded,
    Unbounded,
}

/// One target's requests this frame.
struct QueuedReadback {
    key: NormalizedRenderTarget,
    target: RenderTarget,
    members: Vec<(Entity, ReadbackTag)>,
}

#[derive(Resource, Default)]
pub(crate) struct ReadbackQueue {
    queued: Vec<QueuedReadback>,
    /// `Screenshot` entities spawned and not yet delivered or despawned, by target, with the
    /// real time they were spawned.
    outstanding: HashMap<NormalizedRenderTarget, Vec<(Entity, Duration)>>,
}

impl ReadbackQueue {
    /// Queues a readback of `source` for `entity`. Returns false if a bounded request was refused.
    pub(crate) fn request(
        &mut self,
        source: &ResolvedSource,
        entity: Entity,
        tag: ReadbackTag,
        limit: Limit,
    ) -> bool {
        if let Some(queued) = self.queued.iter_mut().find(|q| q.key == source.key) {
            // Joining a readback already queued this frame costs nothing extra.
            queued.members.push((entity, tag));
            return true;
        }
        if limit == Limit::Bounded
            && self.outstanding(&source.key, tag.requested_at) >= MAX_IN_FLIGHT
        {
            return false;
        }
        self.queued.push(QueuedReadback {
            key: source.key.clone(),
            target: source.target.clone(),
            members: vec![(entity, tag)],
        });
        true
    }

    /// Readbacks of `key` that still count at real time `now`.
    pub(crate) fn outstanding(&self, key: &NormalizedRenderTarget, now: Duration) -> u32 {
        self.outstanding.get(key).map_or(0, |spawned| {
            spawned
                .iter()
                .filter(|(_, at)| now.saturating_sub(*at) < RECLAIM_AFTER)
                .count() as u32
        })
    }

    /// Gives back `screenshot`'s slot. Idempotent: delivery and despawn both call it.
    fn release(&mut self, key: &NormalizedRenderTarget, screenshot: Entity) {
        if let Some(spawned) = self.outstanding.get_mut(key) {
            spawned.retain(|(entity, _)| *entity != screenshot);
            // The key of an image target holds a strong handle; keeping it would leak the image.
            if spawned.is_empty() {
                self.outstanding.remove(key);
            }
        }
    }
}

/// The requests one Bevy `Screenshot` entity delivers to.
#[derive(Component)]
#[component(on_remove = release_on_remove)]
pub(crate) struct ReadbackFanOut {
    key: NormalizedRenderTarget,
    pub members: Vec<(Entity, ReadbackTag)>,
}

/// Triggered on a requester when its readback arrives.
#[derive(EntityEvent, Clone, Debug)]
pub(crate) struct FrameReadBack {
    pub entity: Entity,
    pub image: Arc<Image>,
    pub tag: ReadbackTag,
}

/// Where readbacks are asked for, then spawned. Both in `Last`.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ReadbackSystems {
    Request,
    Spawn,
}

/// Spawns this frame's queued readbacks. Added by every capture plugin that needs it.
pub(crate) struct ReadbackPlugin;

impl Plugin for ReadbackPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ReadbackQueue>()
            .configure_sets(
                Last,
                (ReadbackSystems::Request, ReadbackSystems::Spawn).chain(),
            )
            .add_systems(Last, spawn_readbacks.in_set(ReadbackSystems::Spawn));
    }
}

/// Adds [`ReadbackPlugin`] unless another capture plugin already has.
pub(crate) fn add_readback_plugin(app: &mut App) {
    if !app.is_plugin_added::<ReadbackPlugin>() {
        app.add_plugins(ReadbackPlugin);
    }
}

fn spawn_readbacks(
    mut commands: Commands,
    mut queue: ResMut<ReadbackQueue>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    let queue = &mut *queue;
    for QueuedReadback {
        key,
        target,
        members,
    } in queue.queued.drain(..)
    {
        let spawned = queue.outstanding.entry(key.clone()).or_default();
        // Forget readbacks long past reclaiming, so the list cannot grow without bound.
        spawned.retain(|(_, at)| now.saturating_sub(*at) < RECLAIM_AFTER);
        let screenshot = commands
            .spawn((Screenshot(target), ReadbackFanOut { key, members }))
            .observe(fan_out_readback)
            .id();
        spawned.push((screenshot, now));
    }
}

/// A `Screenshot` despawned without delivering, such as one Bevy dropped as a same-frame
/// duplicate, gives its slot back.
fn release_on_remove(mut world: DeferredWorld, context: HookContext) {
    let Some(key) = world
        .get::<ReadbackFanOut>(context.entity)
        .map(|fan_out| fan_out.key.clone())
    else {
        return;
    };
    if let Some(mut queue) = world.get_resource_mut::<ReadbackQueue>() {
        queue.release(&key, context.entity);
    }
}

/// Returns the target's slot and hands the image to every requester. Bevy despawns the
/// `Screenshot` entity itself once delivered; we never despawn one, because a late delivery
/// inserts into it.
fn fan_out_readback(
    mut captured: On<ScreenshotCaptured>,
    fan_outs: Query<&ReadbackFanOut>,
    mut queue: ResMut<ReadbackQueue>,
    mut commands: Commands,
) {
    let Ok(fan_out) = fan_outs.get(captured.entity) else {
        return;
    };
    queue.release(&fan_out.key, captured.entity);
    // Taken, not cloned: this observer is the image's only consumer.
    let image = Arc::new(std::mem::take(&mut captured.event_mut().image));
    for &(entity, tag) in &fan_out.members {
        commands.trigger(FrameReadBack {
            entity,
            image: image.clone(),
            tag,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        camera::{ManualTextureViewHandle, Viewport},
        window::PrimaryWindow,
    };

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
    fn resolve_camera_into_a_window_crops_to_its_viewport() {
        let target = RenderTarget::Window(WindowRef::Primary);
        let camera = camera(true, Some((UVec2::new(200, 50), UVec2::new(600, 400))));
        let (_, crop) = resolve_camera(entity(), Some((&camera, &target))).unwrap();
        assert_eq!(
            crop,
            Some(URect::from_corners(
                UVec2::new(200, 50),
                UVec2::new(800, 450)
            ))
        );
    }

    #[test]
    fn resolve_camera_rejects_what_it_cannot_capture() {
        let window = RenderTarget::Window(WindowRef::Primary);
        assert_eq!(
            resolve_camera(entity(), None).unwrap_err(),
            SourceError::NotACamera(entity())
        );
        assert_eq!(
            resolve_camera(entity(), Some((&camera(false, None), &window))).unwrap_err(),
            SourceError::CameraInactive(entity())
        );
        for target in [
            RenderTarget::TextureView(ManualTextureViewHandle(0)),
            RenderTarget::None { size: UVec2::ONE },
        ] {
            assert_eq!(
                resolve_camera(entity(), Some((&camera(true, None), &target))).unwrap_err(),
                SourceError::UnsupportedTarget(entity())
            );
        }
    }

    fn window_source(app: &mut App) -> ResolvedSource {
        let window = app
            .world_mut()
            .query_filtered::<Entity, With<PrimaryWindow>>()
            .single(app.world())
            .unwrap();
        let target = RenderTarget::Window(WindowRef::Primary);
        ResolvedSource {
            key: target.normalize(Some(window)).unwrap(),
            target,
            crop: None,
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, ReadbackPlugin));
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        app
    }

    fn tag(seq: u64) -> ReadbackTag {
        ReadbackTag {
            crop: None,
            seq,
            requested_at: Duration::ZERO,
        }
    }

    /// Real time now, which is what readbacks are stamped with when spawned.
    fn now(app: &App) -> Duration {
        app.world().resource::<Time<Real>>().elapsed()
    }

    #[test]
    fn bounded_requests_are_refused_until_a_readback_is_delivered() {
        let mut app = app();
        let source = window_source(&mut app);
        let requester = app.world_mut().spawn_empty().id();
        let tag = |app: &App, seq| ReadbackTag {
            requested_at: now(app),
            ..tag(seq)
        };
        // Every readback is spawned at or after this.
        let first_spawn = now(&app);
        for seq in 0..MAX_IN_FLIGHT as u64 {
            let tag = tag(&app, seq);
            let mut queue = app.world_mut().resource_mut::<ReadbackQueue>();
            assert!(queue.request(&source, requester, tag, Limit::Bounded));
            app.update();
        }
        let (refused, joined, extra) = (tag(&app, 9), tag(&app, 10), tag(&app, 11));
        let mut queue = app.world_mut().resource_mut::<ReadbackQueue>();
        assert!(!queue.request(&source, requester, refused, Limit::Bounded));
        // A screenshot is never refused, and a bounded request may then join it for free.
        assert!(queue.request(&source, requester, joined, Limit::Unbounded));
        assert!(queue.request(&source, requester, extra, Limit::Bounded));
        app.update();
        let outstanding = |app: &App, at| {
            app.world()
                .resource::<ReadbackQueue>()
                .outstanding(&source.key, at)
        };
        assert_eq!(outstanding(&app, now(&app)), MAX_IN_FLIGHT + 1);
        // All still count just short of the reclaim time.
        assert_eq!(
            outstanding(&app, first_spawn + RECLAIM_AFTER - Duration::from_millis(1)),
            MAX_IN_FLIGHT + 1
        );

        // Only an actual delivery gives a slot back.
        let screenshot = app
            .world_mut()
            .query_filtered::<Entity, With<ReadbackFanOut>>()
            .iter(app.world())
            .next()
            .unwrap();
        app.world_mut().trigger(ScreenshotCaptured {
            entity: screenshot,
            image: Image::default(),
        });
        assert_eq!(outstanding(&app, now(&app)), MAX_IN_FLIGHT);
        // So does a `Screenshot` despawned undelivered, as Bevy does with a same-frame duplicate.
        let undelivered = app
            .world_mut()
            .query_filtered::<Entity, With<ReadbackFanOut>>()
            .iter(app.world())
            .find(|entity| *entity != screenshot)
            .unwrap();
        app.world_mut().despawn(undelivered);
        assert_eq!(outstanding(&app, now(&app)), MAX_IN_FLIGHT - 1);
        // And one that never arrives stops counting once the reclaim time has passed.
        assert_eq!(outstanding(&app, now(&app) + RECLAIM_AFTER), 0);

        // Once every slot is back, the target's entry (and any image handle in it) is gone.
        let rest: Vec<Entity> = app
            .world_mut()
            .query_filtered::<Entity, With<ReadbackFanOut>>()
            .iter(app.world())
            .filter(|entity| *entity != screenshot && *entity != undelivered)
            .collect();
        for entity in rest {
            app.world_mut().despawn(entity);
        }
        let queue = app.world().resource::<ReadbackQueue>();
        assert!(!queue.outstanding.contains_key(&source.key));
    }

    #[test]
    fn each_member_gets_the_image_with_its_own_tag() {
        #[derive(Resource, Default)]
        struct Seen(Vec<(Entity, ReadbackTag)>);

        let mut app = app();
        app.init_resource::<Seen>().add_observer(
            |frame: On<FrameReadBack>, mut seen: ResMut<Seen>| {
                seen.0.push((frame.entity, frame.tag));
            },
        );
        let source = window_source(&mut app);
        let a = app.world_mut().spawn_empty().id();
        let b = app.world_mut().spawn_empty().id();
        let crop = Some(URect::new(1, 2, 3, 4));
        let tag_b = ReadbackTag { crop, ..tag(5) };
        let mut queue = app.world_mut().resource_mut::<ReadbackQueue>();
        queue.request(&source, a, tag(1), Limit::Unbounded);
        queue.request(&source, b, tag_b, Limit::Bounded);
        app.update();

        let screenshots: Vec<Entity> = app
            .world_mut()
            .query_filtered::<Entity, With<Screenshot>>()
            .iter(app.world())
            .collect();
        assert_eq!(screenshots.len(), 1);
        app.world_mut().trigger(ScreenshotCaptured {
            entity: screenshots[0],
            image: Image::default(),
        });
        app.world_mut().flush();
        assert_eq!(app.world().resource::<Seen>().0, [(a, tag(1)), (b, tag_b)]);
    }
}
