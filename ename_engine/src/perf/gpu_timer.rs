//! The render-world GPU timer: each camera's schedule wrapped in timestamps and CPU wall time,
//! the frame bracketed by two more timestamps, and the results sent to the main world tagged
//! with the [`PerfFrame`](super::PerfFrame) they were extracted from.
//!
//! Bevy's per-pass spans carry no view and reach the main world frames late without a frame id,
//! so they cannot be split per camera. This timer records the camera with its timestamps
//! instead. Timestamps are empty compute passes with `timestamp_writes`, which need only
//! `TIMESTAMP_QUERY`; `write_timestamp` would also need `TIMESTAMP_QUERY_INSIDE_ENCODERS`.
//!
//! Root `RenderGraph` work outside `RenderGraphSystems::Render` may fall outside the frame
//! bracket, and work submitted outside the graph (uncovered window clears, screenshot and
//! readback copies, direct `queue.submit` calls) is not in the total.

use std::{
    borrow::Cow,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc::Sender,
    },
    time::Instant,
};

use bevy::{
    ecs::{entity::EntityHashMap, schedule::ScheduleLabel},
    prelude::*,
    render::{
        Extract, ExtractSchedule, GpuResourceAppExt, Render, RenderApp, RenderSystems,
        camera::ExtractedCamera,
        renderer::{
            CurrentView, PendingCommandBuffers, RenderDevice, RenderGraph, RenderGraphSystems,
            RenderQueue,
        },
        sync_world::{MainEntity, RenderEntity},
    },
};
use wgpu::{
    Buffer, BufferDescriptor, BufferUsages, CommandBuffer, CommandEncoder,
    CommandEncoderDescriptor, ComputePassDescriptor, ComputePassTimestampWrites, Features, MapMode,
    QuerySet, QuerySetDescriptor, QueryType,
};

use super::{PerfDemand, PerfFrame, PerfViewLabel};
use crate::camera::MainCameraFilter;

/// Views timed per frame; later ones get CPU time only and count in
/// [`GpuFrame::untimed_views`].
const MAX_TIMED_VIEWS: usize = 64;
/// Frame begin and end, then a begin and end per view.
const QUERIES: u32 = 2 + 2 * MAX_TIMED_VIEWS as u32;
/// Frames whose timestamps may be in flight at once. With none free a frame is skipped.
const MAX_SLOTS: usize = 4;
const TIMESTAMP_SIZE: u64 = 8;

/// Label of the view the `MainCameraFilter` camera gets, so results compare across stages.
const MAIN_CAMERA_LABEL: &str = "main_camera";

/// One view's time in one frame. The camera entity is the identity; the label only groups.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewTime {
    pub camera: MainEntity,
    pub label: Arc<str>,
    pub ms: f64,
}

/// Whether a render frame recorded GPU timestamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GpuTiming {
    /// A [`GpuFrame`] for this frame follows, or a [`RenderRecord::GpuLost`].
    Recorded,
    /// Every frame slot was still in flight; this frame has no GPU times.
    Skipped,
    /// The device has no `TIMESTAMP_QUERY`.
    Unavailable,
}

/// CPU side of one render frame, sent at its end.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CpuFrame {
    pub frame: u64,
    /// Wall time from the first to the last `RenderSystems` set. Includes waiting for the
    /// swapchain and present, so it is not CPU work.
    pub render_wall_ms: f64,
    /// CPU wall time of each camera's schedule, in the order they ran.
    pub views: Vec<ViewTime>,
    pub gpu: GpuTiming,
}

/// GPU side of one render frame, sent once its timestamps are read back.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GpuFrame {
    pub frame: u64,
    /// From the end of `RenderGraphSystems::Begin` to the end of `RenderGraphSystems::Render`.
    pub total_ms: f64,
    pub views: Vec<ViewTime>,
    /// Views past [`MAX_TIMED_VIEWS`], which got no timestamps.
    pub untimed_views: usize,
}

/// What the render world sends the main world.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RenderRecord {
    Cpu(CpuFrame),
    Gpu(GpuFrame),
    /// The frame's timestamps failed to map; it counts as skipped.
    GpuLost(u64),
}

/// Which timestamps the device supports, written once the render device exists. Both worlds
/// hold it.
#[derive(Resource, Clone, Default)]
pub(crate) struct TimestampSupport(Arc<AtomicU8>);

impl TimestampSupport {
    const KNOWN: u8 = 1;
    const VIEWS: u8 = 2;
    const PASSES: u8 = 4;

    fn store(&self, features: Features) {
        let mut bits = Self::KNOWN;
        if features.contains(Features::TIMESTAMP_QUERY) {
            bits |= Self::VIEWS;
        }
        // What Bevy's pass spans need for GPU times.
        if features.contains(
            Features::TIMESTAMP_QUERY
                | Features::TIMESTAMP_QUERY_INSIDE_ENCODERS
                | Features::TIMESTAMP_QUERY_INSIDE_PASSES,
        ) {
            bits |= Self::PASSES;
        }
        self.0.store(bits, Ordering::Relaxed);
    }

    /// `None` until the render device exists.
    #[expect(dead_code, reason = "read by perf.sample")]
    pub(crate) fn get(&self) -> Option<Timestamps> {
        let bits = self.0.load(Ordering::Relaxed);
        (bits & Self::KNOWN != 0).then_some(Timestamps {
            views: bits & Self::VIEWS != 0,
            passes: bits & Self::PASSES != 0,
        })
    }
}

#[cfg_attr(not(test), expect(dead_code, reason = "read by perf.sample"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Timestamps {
    /// This timer's per-view and frame times.
    pub views: bool,
    /// GPU times of Bevy's pass spans.
    pub passes: bool,
}

/// The `PerfFrame` being sampled, or `None` while nothing demands timing.
#[derive(Resource, Default)]
struct PerfSampling(Option<u64>);

/// Each sampled camera's label, by render entity, extracted every sampled frame.
#[derive(Resource, Default)]
struct ViewLabels(EntityHashMap<ViewLabel>);

#[derive(Clone)]
struct ViewLabel {
    camera: MainEntity,
    label: Arc<str>,
}

/// The cameras wrapped this frame, by render entity.
#[derive(Resource, Default)]
struct TimedViews(EntityHashMap<TimedView>);

#[derive(Clone)]
struct TimedView {
    /// The camera's own schedule, which [`PerfViewSchedule`] runs.
    inner: bevy::ecs::schedule::InternedScheduleLabel,
    camera: MainEntity,
    label: Arc<str>,
}

/// Runs in place of a sampled camera's schedule and runs that schedule between two timestamps.
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
struct PerfViewSchedule;

/// The render frame being recorded.
#[derive(Resource)]
struct FrameRecording {
    start: Instant,
    views: Vec<ViewTime>,
    gpu: GpuTiming,
}

impl Default for FrameRecording {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            views: Vec::new(),
            gpu: GpuTiming::Unavailable,
        }
    }
}

#[derive(Resource)]
struct RecordSender(Sender<RenderRecord>);

/// Mapping state of a slot's read buffer, set by the map callback.
const MAP_PENDING: u8 = 0;
const MAP_DONE: u8 = 1;
const MAP_FAILED: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState {
    Free,
    Recording,
    /// Submitted; the read buffer maps once the GPU is done with it.
    Mapping,
}

/// One frame's query set and its buffers. Only a `Free` slot is reused.
struct Slot {
    state: SlotState,
    query_set: QuerySet,
    resolve: Buffer,
    read: Buffer,
    mapped: Arc<AtomicU8>,
    frame: u64,
    /// Timed views in recording order; view `i` has queries `2 + 2i` and `3 + 2i`.
    views: Vec<ViewLabel>,
    untimed_views: usize,
}

impl Slot {
    fn new(device: &wgpu::Device) -> Self {
        let size = u64::from(QUERIES) * TIMESTAMP_SIZE;
        Self {
            state: SlotState::Free,
            query_set: device.create_query_set(&QuerySetDescriptor {
                label: Some("perf_timestamps"),
                ty: QueryType::Timestamp,
                count: QUERIES,
            }),
            resolve: device.create_buffer(&BufferDescriptor {
                label: Some("perf_timestamps_resolve"),
                size,
                usage: BufferUsages::QUERY_RESOLVE | BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            read: device.create_buffer(&BufferDescriptor {
                label: Some("perf_timestamps_read"),
                size,
                usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            mapped: Arc::new(AtomicU8::new(MAP_PENDING)),
            frame: 0,
            views: Vec::new(),
            untimed_views: 0,
        }
    }

    fn queries_used(&self) -> u32 {
        2 + 2 * self.views.len() as u32
    }

    /// Reads a mapped slot's timestamps into a frame and frees the slot.
    fn read_back(&mut self, period_ns: f32) -> GpuFrame {
        let used = self.queries_used() as usize;
        let ticks: Vec<u64> = {
            let data = self.read.get_mapped_range(..);
            data[..used * TIMESTAMP_SIZE as usize]
                .as_chunks::<8>()
                .0
                .iter()
                .map(|bytes| u64::from_le_bytes(*bytes))
                .collect()
        };
        self.read.unmap();
        self.state = SlotState::Free;
        let ms = |begin: usize, end: usize| {
            ticks[end].saturating_sub(ticks[begin]) as f64 * f64::from(period_ns) / 1e6
        };
        GpuFrame {
            frame: self.frame,
            total_ms: ms(0, 1),
            views: self
                .views
                .drain(..)
                .enumerate()
                .map(|(i, view)| ViewTime {
                    camera: view.camera,
                    label: view.label,
                    ms: ms(2 + 2 * i, 3 + 2 * i),
                })
                .collect(),
            untimed_views: self.untimed_views,
        }
    }
}

/// The slot pool. Created at `RenderStartup`, so a recovered device gets a fresh one.
#[derive(Resource)]
struct GpuTimer {
    supported: bool,
    period_ns: f32,
    slots: Vec<Slot>,
    /// The slot recording this frame.
    recording: Option<usize>,
}

impl FromWorld for GpuTimer {
    fn from_world(world: &mut World) -> Self {
        let features = world.resource::<RenderDevice>().features();
        world.resource::<TimestampSupport>().store(features);
        Self {
            supported: features.contains(Features::TIMESTAMP_QUERY),
            period_ns: world.resource::<RenderQueue>().get_timestamp_period(),
            slots: Vec::new(),
            recording: None,
        }
    }
}

impl GpuTimer {
    fn recording_slot(&mut self) -> Option<&mut Slot> {
        self.slots.get_mut(self.recording?)
    }
}

pub(super) fn build(app: &mut App, sender: Sender<RenderRecord>, support: TimestampSupport) {
    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    render_app
        .insert_resource(RecordSender(sender))
        .insert_resource(support)
        .init_resource::<PerfSampling>()
        .init_resource::<ViewLabels>()
        .init_resource::<TimedViews>()
        .init_resource::<FrameRecording>()
        .init_gpu_resource::<GpuTimer>()
        .add_systems(ExtractSchedule, extract_sampling)
        .add_systems(
            Render,
            (
                start_render_frame.before(RenderSystems::ExtractCommands),
                send_cpu_frame.after(RenderSystems::PostCleanup),
            )
                .run_if(sampling),
        )
        .configure_sets(
            RenderGraph,
            (
                GpuTimerSystems::FrameBegin
                    .after(RenderGraphSystems::Begin)
                    .before(RenderGraphSystems::Render),
                // Where Bevy resolves its own diagnostics.
                GpuTimerSystems::FrameEnd
                    .after(RenderGraphSystems::Render)
                    .before(RenderGraphSystems::Submit),
            ),
        )
        .add_systems(
            RenderGraph,
            (
                // Inside `Begin`, so it needs no ordering: every camera schedule runs in `Render`.
                wrap_cameras
                    .in_set(RenderGraphSystems::Begin)
                    .run_if(sampling),
                begin_gpu_frame
                    .in_set(GpuTimerSystems::FrameBegin)
                    .run_if(timer_active),
                end_gpu_frame
                    .in_set(GpuTimerSystems::FrameEnd)
                    .run_if(sampling),
            ),
        )
        .add_systems(PerfViewSchedule, run_timed_view);
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
enum GpuTimerSystems {
    /// Writes the frame's begin timestamp, after every `Begin` system.
    FrameBegin,
    /// Writes the frame's end timestamp and resolves the queries.
    FrameEnd,
}

fn sampling(sampling: Res<PerfSampling>) -> bool {
    sampling.0.is_some()
}

/// Also while slots still map after sampling stopped, so their frames are sent before they drop.
fn timer_active(sampling: Res<PerfSampling>, timer: Res<GpuTimer>) -> bool {
    sampling.0.is_some() || !timer.slots.is_empty()
}

type CameraLabelData = (
    Entity,
    RenderEntity,
    &'static Camera,
    Option<&'static PerfViewLabel>,
    Option<&'static Name>,
);

fn extract_sampling(
    mut sampling: ResMut<PerfSampling>,
    mut labels: ResMut<ViewLabels>,
    demand: Extract<Res<PerfDemand>>,
    frame: Extract<Res<PerfFrame>>,
    cameras: Extract<Query<CameraLabelData>>,
    main_camera: Extract<Query<Entity, MainCameraFilter>>,
) {
    let now = demand.on().then_some(frame.0);
    if sampling.0 != now {
        sampling.0 = now;
    }
    if now.is_none() {
        if !labels.0.is_empty() {
            labels.0.clear();
        }
        return;
    }
    let main_camera = main_camera.single().ok();
    let mut previous = std::mem::take(&mut labels.0);
    for (entity, render_entity, camera, perf_label, name) in &cameras {
        if !camera.is_active {
            continue;
        }
        let label: Cow<str> = if let Some(PerfViewLabel(label)) = perf_label {
            Cow::Borrowed(label)
        } else if main_camera == Some(entity) {
            Cow::Borrowed(MAIN_CAMERA_LABEL)
        } else if let Some(name) = name {
            Cow::Borrowed(name.as_str())
        } else {
            Cow::Owned(format!("camera {}", camera.order))
        };
        // Reuses last frame's string, so a steady camera set allocates nothing.
        let label = match previous.remove(&render_entity) {
            Some(old) if *old.label == *label => old.label,
            _ => Arc::from(label.as_ref()),
        };
        labels.0.insert(
            render_entity,
            ViewLabel {
                camera: MainEntity::from(entity),
                label,
            },
        );
    }
}

fn start_render_frame(mut recording: ResMut<FrameRecording>, timer: Res<GpuTimer>) {
    recording.start = Instant::now();
    recording.views.clear();
    recording.gpu = if timer.supported {
        GpuTiming::Skipped
    } else {
        GpuTiming::Unavailable
    };
}

fn send_cpu_frame(
    mut recording: ResMut<FrameRecording>,
    sampling: Res<PerfSampling>,
    sender: Res<RecordSender>,
) {
    let Some(frame) = sampling.0 else {
        return;
    };
    // The main world drops its receiver only at exit.
    let _ = sender.0.send(RenderRecord::Cpu(CpuFrame {
        frame,
        render_wall_ms: recording.start.elapsed().as_secs_f64() * 1000.0,
        views: std::mem::take(&mut recording.views),
        gpu: recording.gpu,
    }));
}

/// Points every labelled camera at [`PerfViewSchedule`]. Extraction rewrites `schedule` every
/// frame, so nothing stays wrapped once sampling stops.
fn wrap_cameras(
    mut cameras: Query<(Entity, &mut ExtractedCamera)>,
    labels: Res<ViewLabels>,
    mut timed: ResMut<TimedViews>,
) {
    timed.0.clear();
    let wrapper = PerfViewSchedule.intern();
    for (entity, mut camera) in &mut cameras {
        let Some(label) = labels.0.get(&entity) else {
            continue;
        };
        if camera.schedule == wrapper {
            continue;
        }
        timed.0.insert(
            entity,
            TimedView {
                inner: camera.schedule,
                camera: label.camera,
                label: label.label.clone(),
            },
        );
        camera.schedule = wrapper;
    }
}

/// Sends the frames of slots that finished mapping, then starts recording this frame in a free
/// slot. Once sampling stops it only drains, and drops the slots when all are free.
fn begin_gpu_frame(
    mut timer: ResMut<GpuTimer>,
    mut recording: ResMut<FrameRecording>,
    sampling: Res<PerfSampling>,
    device: Res<RenderDevice>,
    sender: Res<RecordSender>,
    mut pending: ResMut<PendingCommandBuffers>,
) {
    let period_ns = timer.period_ns;
    for slot in &mut timer.slots {
        if slot.state != SlotState::Mapping {
            continue;
        }
        let record = match slot.mapped.load(Ordering::Acquire) {
            MAP_DONE => RenderRecord::Gpu(slot.read_back(period_ns)),
            MAP_FAILED => {
                slot.state = SlotState::Free;
                slot.views.clear();
                RenderRecord::GpuLost(slot.frame)
            }
            _ => continue,
        };
        let _ = sender.0.send(record);
    }

    let Some(frame) = sampling.0 else {
        if timer.slots.iter().all(|slot| slot.state == SlotState::Free) {
            timer.slots.clear();
        }
        return;
    };
    if !timer.supported {
        return;
    }
    let index = match timer
        .slots
        .iter()
        .position(|slot| slot.state == SlotState::Free)
    {
        Some(index) => index,
        None if timer.slots.len() < MAX_SLOTS => {
            timer.slots.push(Slot::new(device.wgpu_device()));
            timer.slots.len() - 1
        }
        None => return,
    };
    let slot = &mut timer.slots[index];
    slot.state = SlotState::Recording;
    slot.frame = frame;
    slot.views.clear();
    slot.untimed_views = 0;
    slot.mapped.store(MAP_PENDING, Ordering::Release);
    pending.push([timestamp_buffer(&device, &slot.query_set, 0)]);
    timer.recording = Some(index);
    recording.gpu = GpuTiming::Recorded;
}

/// Writes the end timestamp, resolves the queries into the read buffer and maps it once the
/// command buffer is submitted.
fn end_gpu_frame(
    mut timer: ResMut<GpuTimer>,
    device: Res<RenderDevice>,
    mut pending: ResMut<PendingCommandBuffers>,
) {
    let Some(slot) = timer.recording_slot() else {
        return;
    };
    let mut encoder = timestamp_encoder(&device, &slot.query_set, 1);
    let used = slot.queries_used();
    encoder.resolve_query_set(&slot.query_set, 0..used, &slot.resolve, 0);
    encoder.copy_buffer_to_buffer(
        &slot.resolve,
        0,
        &slot.read,
        0,
        u64::from(used) * TIMESTAMP_SIZE,
    );
    let mapped = slot.mapped.clone();
    encoder.map_buffer_on_submit(&slot.read, MapMode::Read, .., move |result| {
        let state = if result.is_ok() { MAP_DONE } else { MAP_FAILED };
        mapped.store(state, Ordering::Release);
    });
    slot.state = SlotState::Mapping;
    pending.push([encoder.finish()]);
    timer.recording = None;
}

/// The one system of [`PerfViewSchedule`]: runs the current camera's own schedule between two
/// timestamps and measures its CPU wall time.
fn run_timed_view(world: &mut World) {
    let Some(view) = world.get_resource::<CurrentView>().map(|view| view.0) else {
        return;
    };
    let Some(timed) = world.resource::<TimedViews>().0.get(&view).cloned() else {
        debug_assert!(
            false,
            "camera {view} runs the perf wrapper without a timed view"
        );
        return;
    };
    let timed_index = world.resource_scope(|world, mut timer: Mut<GpuTimer>| {
        let slot = timer.recording_slot()?;
        if slot.views.len() >= MAX_TIMED_VIEWS {
            slot.untimed_views += 1;
            return None;
        }
        let index = slot.queries_used();
        slot.views.push(ViewLabel {
            camera: timed.camera,
            label: timed.label.clone(),
        });
        let buffer = timestamp_buffer(world.resource(), &slot.query_set, index);
        world.resource_mut::<PendingCommandBuffers>().push([buffer]);
        Some(index)
    });

    let start = Instant::now();
    world.run_schedule(timed.inner);
    let ms = start.elapsed().as_secs_f64() * 1000.0;

    if let Some(index) = timed_index {
        world.resource_scope(|world, mut timer: Mut<GpuTimer>| {
            let Some(slot) = timer.recording_slot() else {
                return;
            };
            let buffer = timestamp_buffer(world.resource(), &slot.query_set, index + 1);
            world.resource_mut::<PendingCommandBuffers>().push([buffer]);
        });
    }
    world.resource_mut::<FrameRecording>().views.push(ViewTime {
        camera: timed.camera,
        label: timed.label,
        ms,
    });
}

/// An encoder holding one empty compute pass that writes timestamp `index` at its start.
fn timestamp_encoder(device: &RenderDevice, query_set: &QuerySet, index: u32) -> CommandEncoder {
    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
        label: Some("perf_timestamp"),
    });
    encoder.begin_compute_pass(&ComputePassDescriptor {
        label: Some("perf_timestamp"),
        timestamp_writes: Some(ComputePassTimestampWrites {
            query_set,
            beginning_of_pass_write_index: Some(index),
            end_of_pass_write_index: None,
        }),
    });
    encoder
}

/// A finished buffer, not an encoder: `PendingCommandBuffers::take` puts encoders after
/// buffers, which would move the timestamp behind the work it brackets.
fn timestamp_buffer(device: &RenderDevice, query_set: &QuerySet, index: u32) -> CommandBuffer {
    timestamp_encoder(device, query_set, index).finish()
}
