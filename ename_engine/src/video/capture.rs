//! The render-world side of recording: once per recorded frame, after every camera has rendered
//! and before present, convert the source to NV12 on the GPU, copy it into one buffer of a
//! per-recording ring, map it, and hand the mapped buffer to the writer thread.
//!
//! An image target (the editor's Game View) is read where the camera drew it. A window is not
//! readable, because Bevy configures its swapchain as a render attachment only, so a recorded
//! window renders into a persistent texture of ours (through `ViewTargetAttachments`, the same
//! override Bevy's screenshots use) that is blitted to the swapchain afterwards.
//!
//! # Accounting
//!
//! Every request ends in exactly one of `delivered` (the map callback handed the frame to the
//! writer) or one [`DropReason`]. A ring slot is reserved in `PrepareViews` and freed on every
//! path that does not reach the writer; a frame that does reach it is freed when the writer
//! drops it ([`MappedFrame`] unmaps and frees on drop), so slots cannot leak.
//!
//! Map callbacks may run on any thread that polls the device, in any order, and in newer wgpu
//! even inline in `map_async`, so no lock is held across `map_async` and the writer orders frames
//! by sequence number itself.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    time::Duration,
};

use bevy::{
    camera::NormalizedRenderTarget,
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
        render_asset::RenderAssets,
        render_resource::{
            BindGroup, BindGroupEntry, BindGroupLayout, BindGroupLayoutEntry, BindingResource,
            BindingType, Buffer, BufferDescriptor, BufferUsages, ColorTargetState, ColorWrites,
            Extent3d, LoadOp, MapMode, MultisampleState, Operations, PipelineCompilationOptions,
            PipelineLayoutDescriptor, PollType, PrimitiveState, RawFragmentState,
            RawRenderPipelineDescriptor, RawVertexState, RenderPassColorAttachment,
            RenderPassDescriptor, RenderPipeline, ShaderModuleDescriptor, ShaderSource,
            ShaderStages, StoreOp, Texture, TextureDescriptor, TextureDimension, TextureFormat,
            TextureSampleType, TextureUsages, TextureView, TextureViewDescriptor,
            TextureViewDimension, TextureViewId,
        },
        renderer::{RenderDevice, RenderGraph, RenderGraphSystems, RenderQueue},
        texture::{GpuImage, OutputColorAttachment},
        view::{
            ExtractedWindows, ViewTargetAttachments, prepare_view_attachments, prepare_view_targets,
        },
    },
};

use super::{
    VideoError,
    nv12::{Nv12Pipelines, buffer_len, frame_len, params},
};

/// Buffers per recording. A GPU-bound game keeps two or three frames queued, a map completes a
/// frame after that, and the writer holds the previous frame in time mode, so four ran out;
/// eight cost about 40 MB at 2560x1382.
pub(crate) const RING: usize = 8;

/// A window redirect unused for this many render frames is released.
const REDIRECT_IDLE_FRAMES: u64 = 120;

/// Why a requested frame did not reach the writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DropReason {
    /// Every ring buffer was still in flight or held by the writer.
    NoSlot,
    /// No swapchain, window or image this frame, or nothing drew the target.
    NoSource,
    /// A screenshot redirected the target in the same frame.
    Conflict,
    /// The frame did not fit the target, which was resized.
    Mismatch,
    /// The source cannot be recorded at all (format or usage); the recording fails.
    Unsupported,
    /// The recording's buffers were gone by the time the frame was encoded.
    Unused,
    /// The GPU reported an error mapping the buffer.
    MapError,
    /// The writer had already stopped taking frames.
    SendFailed,
}

impl DropReason {
    const ALL: [Self; 8] = [
        Self::NoSlot,
        Self::NoSource,
        Self::Conflict,
        Self::Mismatch,
        Self::Unsupported,
        Self::Unused,
        Self::MapError,
        Self::SendFailed,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::NoSlot => "no_slot",
            Self::NoSource => "no_source",
            Self::Conflict => "conflict",
            Self::Mismatch => "mismatch",
            Self::Unsupported => "unsupported",
            Self::Unused => "unused",
            Self::MapError => "map_error",
            Self::SendFailed => "send_failed",
        }
    }
}

/// Which slots of a recording's ring are free.
pub(crate) struct SlotStates([AtomicBool; RING]);

impl Default for SlotStates {
    fn default() -> Self {
        Self(std::array::from_fn(|_| AtomicBool::new(true)))
    }
}

impl SlotStates {
    fn reserve(&self) -> Option<usize> {
        self.0.iter().position(|free| {
            free.compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        })
    }

    fn free(&self, slot: usize) {
        self.0[slot].store(true, Ordering::Release);
    }

    #[cfg(test)]
    fn free_count(&self) -> usize {
        self.0
            .iter()
            .filter(|free| free.load(Ordering::Acquire))
            .count()
    }
}

/// One recording's state shared by the main world, the render world, map callbacks and the
/// writer thread.
pub(crate) struct CaptureShared {
    id: u64,
    /// Taken on close. Callbacks clone it under the lock and send after releasing it.
    sender: Mutex<Option<Sender<Ready>>>,
    slots: Arc<SlotStates>,
    pub requested: AtomicU64,
    pub delivered: AtomicU64,
    /// Requests that did not reach the writer, by [`DropReason`].
    dropped: [AtomicU64; DropReason::ALL.len()],
    /// A map failed. wgpu keeps a failed buffer marked as mapped on our side, and unmapping it
    /// is a validation error on the device's, so the render world replaces the ring instead.
    ring_failed: AtomicBool,
    /// A problem that ends the recording, found on the render side.
    pub error: OnceLock<VideoError>,
}

impl CaptureShared {
    /// A new recording's state, and the receiving end for its writer.
    pub(crate) fn new() -> (Arc<Self>, Receiver<Ready>) {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let (sender, receiver) = channel();
        let shared = Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            sender: Mutex::new(Some(sender)),
            slots: default(),
            requested: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
            dropped: default(),
            ring_failed: AtomicBool::new(false),
            error: OnceLock::new(),
        };
        (Arc::new(shared), receiver)
    }

    /// Stops taking frames. The writer sees its channel close once every in-flight frame has
    /// been delivered or dropped.
    pub(crate) fn close(&self) {
        self.lock_sender().take();
    }

    /// Requests that did not reach the writer, for any reason.
    pub(crate) fn failed(&self) -> u64 {
        self.dropped
            .iter()
            .map(|count| count.load(Ordering::Acquire))
            .sum()
    }

    /// Each reason a request did not reach the writer, with its count.
    pub(crate) fn drops(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        DropReason::ALL.into_iter().map(|reason| {
            (
                reason.name(),
                self.dropped[reason as usize].load(Ordering::Acquire),
            )
        })
    }

    /// Requests not yet delivered or dropped.
    pub(crate) fn in_flight(&self) -> u64 {
        let requested = self.requested.load(Ordering::Acquire);
        let ended = self.delivered.load(Ordering::Acquire) + self.failed();
        requested.saturating_sub(ended)
    }

    fn lock_sender(&self) -> std::sync::MutexGuard<'_, Option<Sender<Ready>>> {
        self.sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn sender(&self) -> Option<Sender<Ready>> {
        self.lock_sender().clone()
    }

    pub(crate) fn fail(&self, reason: DropReason) {
        self.dropped[reason as usize].fetch_add(1, Ordering::AcqRel);
    }

    /// Hands a frame to the writer, counting it as delivered or, if the writer is gone, dropped.
    /// A frame that cannot be sent is dropped here, which frees its slot.
    fn deliver(&self, sender: Option<Sender<Ready>>, ready: Ready) {
        match sender.map(|sender| sender.send(ready)) {
            Some(Ok(())) => {
                self.delivered.fetch_add(1, Ordering::AcqRel);
            }
            _ => self.fail(DropReason::SendFailed),
        }
    }

    /// Delivers a frame of bytes as if a map had completed, for tests without a GPU.
    #[cfg(test)]
    pub(crate) fn deliver_bytes(&self, job: &CaptureJob, bytes: Vec<u8>) {
        let ready = Ready {
            seq: job.seq,
            stamp: job.stamp,
            size: job.rect.size(),
            frame: FrameBytes::Owned(bytes),
        };
        self.deliver(self.sender(), ready);
    }
}

impl std::fmt::Debug for CaptureShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureShared")
            .field("id", &self.id)
            .finish()
    }
}

/// One NV12 frame for the writer.
pub(crate) struct Ready {
    pub seq: u64,
    /// Real time since the recording's first request.
    pub stamp: Duration,
    /// Encoded size; even in both dimensions.
    pub size: UVec2,
    pub frame: FrameBytes,
}

pub(crate) enum FrameBytes {
    Mapped(MappedFrame),
    #[cfg(test)]
    Owned(Vec<u8>),
}

impl FrameBytes {
    /// Calls `f` with the frame's [`frame_len`] bytes. A mapped view lives only inside this call.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        match self {
            Self::Mapped(mapped) => {
                let view = mapped.buffer.slice(..mapped.padded).get_mapped_range();
                f(&view[..mapped.len])
            }
            #[cfg(test)]
            Self::Owned(bytes) => f(bytes),
        }
    }
}

/// A mapped ring buffer. Dropping it unmaps the buffer and frees the slot, on every path.
pub(crate) struct MappedFrame {
    buffer: Buffer,
    padded: u64,
    len: usize,
    slots: Arc<SlotStates>,
    slot: usize,
}

impl Drop for MappedFrame {
    fn drop(&mut self) {
        self.buffer.unmap();
        self.slots.free(self.slot);
    }
}

/// Where a capture reads from.
#[derive(Clone, Debug)]
pub(crate) struct CaptureJob {
    pub shared: Arc<CaptureShared>,
    pub key: NormalizedRenderTarget,
    /// Encoded rect in target pixels: the crop clamped and rounded down to even sizes.
    pub rect: URect,
    pub seq: u64,
    pub stamp: Duration,
}

/// This frame's captures, filled in `Last` by `request_frames`, read by the extract.
#[derive(Resource, Default)]
pub(crate) struct PendingCaptures {
    pub jobs: Vec<CaptureJob>,
    /// A recording is starting or running: build the pipelines now, not on the first frame.
    pub warm: bool,
}

/// Maps `buffer` (ring slot `slot` of `job`'s recording) and, once mapped, hands it to the
/// writer. Every outcome ends the request exactly once and frees the slot unless the writer has
/// it.
fn map_slot(job: CaptureJob, slot: usize, buffer: Buffer) {
    let shared = job.shared.clone();
    // Cloned before mapping and never held across `map_async`: the callback may run inline.
    let sender = shared.sender();
    let padded = buffer_len(job.rect.size());
    let mapped_buffer = buffer.clone();
    buffer
        .slice(..padded)
        .map_async(MapMode::Read, move |result| {
            let slots = shared.slots.clone();
            if result.is_err() {
                // The buffer cannot be mapped again (see `ring_failed`); the ring is replaced
                // before the slot is reserved again.
                shared.ring_failed.store(true, Ordering::Release);
                slots.free(slot);
                shared.fail(DropReason::MapError);
                return;
            }
            let ready = Ready {
                seq: job.seq,
                stamp: job.stamp,
                size: job.rect.size(),
                frame: FrameBytes::Mapped(MappedFrame {
                    buffer: mapped_buffer,
                    padded,
                    len: frame_len(job.rect.size()) as usize,
                    slots,
                    slot,
                }),
            };
            shared.deliver(sender, ready);
        });
}

/// Adds the render-world systems, when there is a render world.
pub(crate) fn build_render(app: &mut App) {
    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    render_app
        .init_resource::<ExtractedCaptures>()
        .init_resource::<CaptureGpu>()
        .init_resource::<PreparedCaptures>()
        .add_systems(ExtractSchedule, extract_captures)
        .add_systems(
            Render,
            prepare_capture_targets
                .in_set(RenderSystems::PrepareViews)
                .after(prepare_view_attachments)
                .before(prepare_view_targets),
        )
        // Exclusive, so it keeps its place after `Submit` even under weak set ordering.
        .add_systems(
            RenderGraph,
            capture_frames.in_set(RenderGraphSystems::Finish),
        );
}

#[derive(Resource, Default)]
struct ExtractedCaptures {
    jobs: Vec<CaptureJob>,
    warm: bool,
}

fn extract_captures(
    mut extracted: ResMut<ExtractedCaptures>,
    pending: Extract<Res<PendingCaptures>>,
) {
    extracted.jobs.clone_from(&pending.jobs);
    extracted.warm = pending.warm;
}

/// GPU objects of one recording's ring.
struct RingBuffers {
    size: UVec2,
    nv12: Buffer,
    params: [Buffer; 2],
    /// Bind groups for the last source view, rebuilt when it changes.
    bind_groups: Option<(TextureViewId, [BindGroup; 2])>,
    slots: [Buffer; RING],
    shared: Arc<CaptureShared>,
    /// The window the recording reads, whose redirect is kept while the recording lives.
    window: Option<Entity>,
}

impl RingBuffers {
    fn new(device: &RenderDevice, shared: &Arc<CaptureShared>, size: UVec2) -> Self {
        Self {
            size,
            nv12: Nv12Pipelines::output_buffer(device, size),
            params: Nv12Pipelines::params_buffers(device),
            bind_groups: None,
            slots: std::array::from_fn(|_| {
                device.create_buffer(&BufferDescriptor {
                    label: Some("video_ring"),
                    size: buffer_len(size),
                    usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            }),
            shared: shared.clone(),
            window: None,
        }
    }
}

/// The persistent texture a recorded window renders into instead of its swapchain.
struct Redirect {
    size: UVec2,
    format: TextureFormat,
    _texture: Texture,
    /// The window's sRGB format: what cameras render through and the blit samples.
    srgb_view: TextureView,
    /// The same texels as raw bytes, for the NV12 shader.
    raw_view: TextureView,
    blit_bind_group: Option<BindGroup>,
    /// The render frame it was last used in.
    last_used: u64,
}

impl Redirect {
    fn new(device: &RenderDevice, size: UVec2, format: TextureFormat) -> Self {
        let raw = format.remove_srgb_suffix();
        let texture = device.create_texture(&TextureDescriptor {
            label: Some("video_window_redirect"),
            size: Extent3d {
                width: size.x,
                height: size.y,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[raw],
        });
        Self {
            size,
            format,
            srgb_view: texture.create_view(&TextureViewDescriptor::default()),
            raw_view: texture.create_view(&TextureViewDescriptor {
                format: Some(raw),
                ..default()
            }),
            _texture: texture,
            blit_bind_group: None,
            last_used: 0,
        }
    }
}

/// GPU objects that live across frames.
#[derive(Resource, Default)]
struct CaptureGpu {
    pipelines: Option<CapturePipelines>,
    rings: HashMap<u64, RingBuffers>,
    redirects: HashMap<Entity, Redirect>,
    /// Render frames seen, to age out idle redirects.
    frame: u64,
}

/// A capture that has a slot and a source this frame.
struct PreparedCapture {
    job: CaptureJob,
    slot: usize,
    source: TextureView,
    srgb_source: bool,
    /// The output attachment the capture expects: anything else means a screenshot redirected
    /// the target after us, and the frame is skipped.
    expected: TextureViewId,
}

#[derive(Resource, Default)]
struct PreparedCaptures {
    captures: Vec<PreparedCapture>,
    /// Windows redirected to our texture this frame, which must be blitted back.
    redirected: Vec<(NormalizedRenderTarget, Entity)>,
}

fn is_supported(format: TextureFormat) -> bool {
    matches!(
        format,
        TextureFormat::Rgba8UnormSrgb | TextureFormat::Bgra8UnormSrgb
    )
}

/// What a capture reads: the view, whether it is sRGB, and the attachment it expects.
type Source = (TextureView, bool, TextureViewId);

/// Reserves a slot and a source for each job, and redirects recorded windows to our texture.
fn prepare_capture_targets(
    mut extracted: ResMut<ExtractedCaptures>,
    mut gpu: ResMut<CaptureGpu>,
    mut prepared: ResMut<PreparedCaptures>,
    mut attachments: ResMut<ViewTargetAttachments>,
    windows: Res<ExtractedWindows>,
    images: Res<RenderAssets<GpuImage>>,
    device: Res<RenderDevice>,
) {
    prepared.captures.clear();
    prepared.redirected.clear();
    let gpu = &mut *gpu;
    gpu.frame += 1;
    let frame = gpu.frame;
    // A recording that has ended holds the last reference to its ring.
    gpu.rings
        .retain(|_, ring| Arc::strong_count(&ring.shared) > 1);
    // A live recording keeps its window's redirect however rarely it captures; others go once
    // idle, so a slow output rate does not reallocate the texture every tick.
    let recorded: Vec<Entity> = gpu.rings.values().filter_map(|ring| ring.window).collect();
    gpu.redirects.retain(|window, redirect| {
        windows.contains_key(window)
            && (recorded.contains(window) || frame - redirect.last_used < REDIRECT_IDLE_FRAMES)
    });
    let jobs = std::mem::take(&mut extracted.jobs);
    if (extracted.warm || !jobs.is_empty()) && gpu.pipelines.is_none() {
        gpu.pipelines = Some(CapturePipelines::new(&device));
    }

    for job in jobs {
        let shared = job.shared.clone();
        if shared.error.get().is_some() {
            shared.fail(DropReason::Unused);
            continue;
        }
        let Some(slot) = shared.slots.reserve() else {
            shared.fail(DropReason::NoSlot);
            continue;
        };
        // After reserving: a callback marks the failure before freeing its slot, and reserving
        // acquires that slot, so a just-failed buffer is always seen here and never reused.
        let size = job.rect.size();
        let ring = gpu
            .rings
            .entry(shared.id)
            .or_insert_with(|| RingBuffers::new(&device, &shared, size));
        if ring.size != size || shared.ring_failed.swap(false, Ordering::AcqRel) {
            *ring = RingBuffers::new(&device, &shared, size);
        }
        ring.window = match &job.key {
            NormalizedRenderTarget::Window(window) => Some(window.entity()),
            _ => None,
        };
        let source = match &job.key {
            NormalizedRenderTarget::Window(window) => {
                let window = window.entity();
                let source = prepare_window(
                    &job,
                    window,
                    &windows,
                    &mut gpu.redirects,
                    &mut attachments,
                    &device,
                    frame,
                );
                if source.is_ok() && !prepared.redirected.iter().any(|(_, w)| *w == window) {
                    prepared.redirected.push((job.key.clone(), window));
                }
                source
            }
            NormalizedRenderTarget::Image(image) => {
                prepare_image(&job, &image.handle, &images, &attachments)
            }
            _ => Err(DropReason::NoSource),
        };
        match source {
            Ok((source, srgb_source, expected)) => prepared.captures.push(PreparedCapture {
                job,
                slot,
                source,
                srgb_source,
                expected,
            }),
            Err(reason) => {
                shared.slots.free(slot);
                shared.fail(reason);
            }
        }
    }
    warm_blit(gpu, &prepared.redirected, &device);
}

/// Builds the blit pipeline for each redirected window's format here rather than in the pass.
fn warm_blit(
    gpu: &mut CaptureGpu,
    redirected: &[(NormalizedRenderTarget, Entity)],
    device: &RenderDevice,
) {
    let CaptureGpu {
        pipelines,
        redirects,
        ..
    } = gpu;
    let Some(pipelines) = pipelines.as_mut() else {
        return;
    };
    for (_, window) in redirected {
        if let Some(redirect) = redirects.get(window) {
            pipelines.blit(device, redirect.format);
        }
    }
}

/// The window's redirect texture as the capture source, installing the redirect if the
/// window's attachment is still its own swapchain.
fn prepare_window(
    job: &CaptureJob,
    window: Entity,
    windows: &ExtractedWindows,
    redirects: &mut HashMap<Entity, Redirect>,
    attachments: &mut ViewTargetAttachments,
    device: &RenderDevice,
    frame: u64,
) -> Result<Source, DropReason> {
    let extracted = windows.get(&window).ok_or(DropReason::NoSource)?;
    let swapchain = extracted
        .swap_chain_texture_view
        .as_ref()
        .ok_or(DropReason::NoSource)?;
    let format = extracted
        .swap_chain_texture_view_format
        .ok_or(DropReason::NoSource)?;
    if !is_supported(format) {
        let _ = job.shared.error.set(VideoError::UnsupportedFormat(format));
        return Err(DropReason::Unsupported);
    }
    let size = UVec2::new(extracted.physical_width, extracted.physical_height);
    if job.rect.max.cmpgt(size).any() {
        return Err(DropReason::Mismatch);
    }
    let redirect = redirects
        .entry(window)
        .or_insert_with(|| Redirect::new(device, size, format));
    if redirect.size != size || redirect.format != format {
        *redirect = Redirect::new(device, size, format);
    }
    redirect.last_used = frame;
    let ours = redirect.srgb_view.id();
    let current = attachments
        .get(&job.key)
        .map(|attachment| attachment.view.id());
    if current == Some(swapchain.id()) {
        // A fresh attachment each frame: its first-use flag drives the first camera's clear.
        attachments.insert(
            job.key.clone(),
            OutputColorAttachment::new(redirect.srgb_view.clone(), format),
        );
    } else if current.is_none() {
        // No camera draws this window this frame.
        return Err(DropReason::NoSource);
    } else if current != Some(ours) {
        return Err(DropReason::Conflict);
    }
    Ok((redirect.raw_view.clone(), false, ours))
}

/// The image the camera draws into, if nothing has redirected it this frame.
fn prepare_image(
    job: &CaptureJob,
    handle: &Handle<Image>,
    images: &RenderAssets<GpuImage>,
    attachments: &ViewTargetAttachments,
) -> Result<Source, DropReason> {
    let image = images.get(handle).ok_or(DropReason::NoSource)?;
    // What the camera renders through and the shader loads through: the view, not the texture.
    let format = image.view_format();
    if !is_supported(format) {
        let _ = job.shared.error.set(VideoError::UnsupportedFormat(format));
        return Err(DropReason::Unsupported);
    }
    // wgpu validates the view's usage, which defaults to the texture's.
    let usage = image
        .texture_view_descriptor
        .as_ref()
        .and_then(|descriptor| descriptor.usage)
        .unwrap_or(image.texture_descriptor.usage);
    if !usage.contains(TextureUsages::TEXTURE_BINDING) {
        let _ = job.shared.error.set(VideoError::UnreadableTarget(usage));
        return Err(DropReason::Unsupported);
    }
    let size = UVec2::new(
        image.texture_descriptor.size.width,
        image.texture_descriptor.size.height,
    );
    if job.rect.max.cmpgt(size).any() {
        return Err(DropReason::Mismatch);
    }
    let native = image.texture_view.id();
    // Absent when no camera draws the image this frame; another view when a screenshot
    // redirected it, in which case it is blitted back only after our pass.
    match attachments
        .get(&job.key)
        .map(|attachment| attachment.view.id())
    {
        Some(current) if current == native => Ok((image.texture_view.clone(), true, native)),
        Some(_) => Err(DropReason::Conflict),
        None => Err(DropReason::NoSource),
    }
}

/// Pipelines, built when a recording starts.
struct CapturePipelines {
    nv12: Nv12Pipelines,
    blit_layout: BindGroupLayout,
    blit: HashMap<TextureFormat, RenderPipeline>,
}

const BLIT_SHADER: &str = include_str!("blit.wgsl");

impl CapturePipelines {
    fn new(device: &RenderDevice) -> Self {
        Self {
            nv12: Nv12Pipelines::new(device),
            blit_layout: device.create_bind_group_layout(
                "video_blit",
                &[BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        sample_type: TextureSampleType::Float { filterable: false },
                        view_dimension: TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                }],
            ),
            blit: HashMap::new(),
        }
    }

    fn blit(&mut self, device: &RenderDevice, format: TextureFormat) -> RenderPipeline {
        let layout = &self.blit_layout;
        self.blit
            .entry(format)
            .or_insert_with(|| {
                let module = device
                    .wgpu_device()
                    .create_shader_module(ShaderModuleDescriptor {
                        label: Some("video_blit"),
                        source: ShaderSource::Wgsl(BLIT_SHADER.into()),
                    });
                let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
                    label: Some("video_blit"),
                    bind_group_layouts: &[Some(layout)],
                    immediate_size: 0,
                });
                device.create_render_pipeline(&RawRenderPipelineDescriptor {
                    label: Some("video_blit"),
                    layout: Some(&pipeline_layout),
                    vertex: RawVertexState {
                        module: &module,
                        entry_point: Some("vertex"),
                        compilation_options: PipelineCompilationOptions::default(),
                        buffers: &[],
                    },
                    primitive: PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: MultisampleState::default(),
                    fragment: Some(RawFragmentState {
                        module: &module,
                        entry_point: Some("fragment"),
                        compilation_options: PipelineCompilationOptions::default(),
                        targets: &[Some(ColorTargetState {
                            format,
                            blend: None,
                            write_mask: ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
            .clone()
    }
}

/// Blits redirected windows back to their swapchains, converts each capture to NV12 into its
/// ring slot, submits, maps, and polls so callbacks fire without waiting for the next submit.
fn capture_frames(world: &mut World) {
    let Some(mut prepared) = world.get_resource_mut::<PreparedCaptures>() else {
        return;
    };
    let captures = std::mem::take(&mut prepared.captures);
    let redirected = std::mem::take(&mut prepared.redirected);
    if captures.is_empty() && redirected.is_empty() {
        return;
    }
    let device = world.resource::<RenderDevice>().clone();
    world.resource_scope(|world, mut gpu: Mut<CaptureGpu>| {
        let attachments = world.resource::<ViewTargetAttachments>();
        let windows = world.resource::<ExtractedWindows>();
        let queue = world.resource::<RenderQueue>();
        let current = |key: &NormalizedRenderTarget| {
            attachments.get(key).map(|attachment| attachment.view.id())
        };
        let CaptureGpu {
            pipelines,
            rings,
            redirects,
            ..
        } = &mut *gpu;
        let Some(pipelines) = pipelines.as_mut() else {
            for capture in captures {
                capture.job.shared.slots.free(capture.slot);
                capture.job.shared.fail(DropReason::Unused);
            }
            return;
        };

        let mut encoder = device.create_command_encoder(&default());
        for (key, window) in &redirected {
            let (Some(redirect), Some(extracted)) =
                (redirects.get_mut(window), windows.get(window))
            else {
                continue;
            };
            let Some(swapchain) = extracted.swap_chain_texture_view.as_ref() else {
                continue;
            };
            // A screenshot that redirected this window after us blits it back itself.
            if current(key) != Some(redirect.srgb_view.id()) {
                continue;
            }
            let pipeline = pipelines.blit(&device, redirect.format);
            let bind_group = redirect.blit_bind_group.get_or_insert_with(|| {
                device.create_bind_group(
                    "video_blit",
                    &pipelines.blit_layout,
                    &[BindGroupEntry {
                        binding: 0,
                        resource: BindingResource::TextureView(&redirect.srgb_view),
                    }],
                )
            });
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("video_blit"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: swapchain,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Load,
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &**bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        let mut mapped = Vec::new();
        for capture in captures {
            let shared = &capture.job.shared;
            let Some(ring) = rings.get_mut(&shared.id) else {
                shared.slots.free(capture.slot);
                shared.fail(DropReason::Unused);
                continue;
            };
            if current(&capture.job.key) != Some(capture.expected) {
                shared.slots.free(capture.slot);
                shared.fail(DropReason::Conflict);
                continue;
            }
            for (buffer, bytes) in ring
                .params
                .iter()
                .zip(params(capture.job.rect, capture.srgb_source))
            {
                queue.write_buffer(buffer, 0, &bytes);
            }
            let view = capture.source.id();
            if ring.bind_groups.as_ref().map(|(id, _)| *id) != Some(view) {
                let groups =
                    pipelines
                        .nv12
                        .bind_groups(&device, &capture.source, &ring.nv12, &ring.params);
                ring.bind_groups = Some((view, groups));
            }
            let (_, bind_groups) = ring.bind_groups.as_ref().unwrap();
            pipelines
                .nv12
                .encode(&mut encoder, bind_groups, capture.job.rect.size());
            let slot_buffer = ring.slots[capture.slot].clone();
            encoder.copy_buffer_to_buffer(&ring.nv12, 0, &slot_buffer, 0, buffer_len(ring.size));
            mapped.push((capture, slot_buffer));
        }
        queue.submit([encoder.finish()]);

        for (capture, buffer) in mapped {
            map_slot(capture.job, capture.slot, buffer);
        }
        let _ = device.poll(PollType::Poll);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::nv12::tests::gpu;

    #[test]
    fn slots_are_reserved_until_freed() {
        let slots = SlotStates::default();
        let reserved: Vec<usize> = std::iter::from_fn(|| slots.reserve()).collect();
        assert_eq!(reserved, (0..RING).collect::<Vec<_>>());
        assert_eq!(slots.reserve(), None);
        slots.free(3);
        assert_eq!(slots.reserve(), Some(3));
        assert_eq!(slots.free_count(), 0);
    }

    #[test]
    fn every_request_ends_once_and_drops_are_counted_by_reason() {
        let (shared, receiver) = CaptureShared::new();
        shared.requested.fetch_add(4, Ordering::AcqRel);
        let job = |seq| CaptureJob {
            shared: shared.clone(),
            key: NormalizedRenderTarget::None {
                width: 2,
                height: 2,
            },
            rect: URect::new(0, 0, 2, 2),
            seq,
            stamp: Duration::ZERO,
        };
        shared.deliver_bytes(&job(0), vec![0; 6]);
        shared.fail(DropReason::NoSlot);
        shared.fail(DropReason::Conflict);
        assert_eq!(shared.in_flight(), 1);
        drop(receiver);
        shared.deliver_bytes(&job(1), vec![0; 6]);
        assert_eq!(shared.in_flight(), 0);
        assert_eq!(shared.failed(), 3);
        let drops: Vec<_> = shared.drops().filter(|(_, count)| *count > 0).collect();
        assert_eq!(drops, [("no_slot", 1), ("conflict", 1), ("send_failed", 1)]);
    }

    fn drops_of(shared: &CaptureShared, reason: &str) -> u64 {
        shared.drops().find(|(name, _)| *name == reason).unwrap().1
    }

    /// Ring buffers on a real GPU: a delivered frame frees its slot and unmaps when the writer
    /// drops it; a recording closed while a map is in flight still delivers it, and a writer
    /// gone by then frees it; a failed map frees its slot and marks the ring for replacement;
    /// a frame held from a replaced ring still frees its slot. Skipped without an adapter.
    #[test]
    fn mapped_slots_are_released_on_every_path() {
        let Some((device, _queue)) = gpu() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let wait = || {
            device.poll(PollType::wait_indefinitely()).unwrap();
        };
        let job = |shared: &Arc<CaptureShared>, seq| CaptureJob {
            shared: shared.clone(),
            key: NormalizedRenderTarget::None {
                width: 4,
                height: 2,
            },
            rect: URect::new(0, 0, 4, 2),
            seq,
            stamp: Duration::ZERO,
        };
        let (shared, receiver) = CaptureShared::new();
        let ring = RingBuffers::new(&device, &shared, UVec2::new(4, 2));

        // Delivered: the writer's drop unmaps and frees, and the buffer maps again.
        let slot = shared.slots.reserve().unwrap();
        map_slot(job(&shared, 0), slot, ring.slots[slot].clone());
        wait();
        let ready = receiver.try_recv().unwrap();
        assert_eq!(ready.frame.with(|bytes| bytes.len()), 12);
        assert_eq!(shared.slots.free_count(), RING - 1);
        drop(ready);
        assert_eq!(shared.slots.free_count(), RING);
        map_slot(job(&shared, 1), slot, ring.slots[slot].clone());
        wait();
        drop(receiver.try_recv().unwrap());

        // Closed while the map is in flight: the frame still reaches the writer, which then
        // sees its channel close.
        let slot = shared.slots.reserve().unwrap();
        map_slot(job(&shared, 2), slot, ring.slots[slot].clone());
        shared.close();
        wait();
        drop(receiver.try_recv().unwrap());
        assert!(matches!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
        assert_eq!(shared.slots.free_count(), RING);

        // The writer gone while a map is in flight: the send fails and the slot comes back.
        let (shared, receiver) = CaptureShared::new();
        let ring = RingBuffers::new(&device, &shared, UVec2::new(4, 2));
        let slot = shared.slots.reserve().unwrap();
        map_slot(job(&shared, 0), slot, ring.slots[slot].clone());
        drop(receiver);
        wait();
        assert_eq!(shared.slots.free_count(), RING);
        assert_eq!(drops_of(&shared, "send_failed"), 1);

        // A failed map (the buffer is destroyed first) frees the slot, counts the error, and
        // marks the ring for replacement rather than touching the buffer again.
        let slot = shared.slots.reserve().unwrap();
        let doomed = ring.slots[slot].clone();
        map_slot(job(&shared, 1), slot, doomed.clone());
        doomed.destroy();
        wait();
        assert_eq!(shared.slots.free_count(), RING);
        assert_eq!(drops_of(&shared, "map_error"), 1);
        assert!(shared.ring_failed.load(Ordering::Acquire));
    }

    /// A frame held by the writer from a ring that is then replaced keeps its own buffer, and
    /// frees its slot when dropped; the new ring's buffer for that slot is untouched.
    #[test]
    fn a_frame_from_a_replaced_ring_frees_its_slot() {
        let Some((device, _queue)) = gpu() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (shared, receiver) = CaptureShared::new();
        let old = RingBuffers::new(&device, &shared, UVec2::new(4, 2));
        let held_slot = shared.slots.reserve().unwrap();
        let job = |seq| CaptureJob {
            shared: shared.clone(),
            key: NormalizedRenderTarget::None {
                width: 4,
                height: 2,
            },
            rect: URect::new(0, 0, 4, 2),
            seq,
            stamp: Duration::ZERO,
        };
        map_slot(job(0), held_slot, old.slots[held_slot].clone());
        device.poll(PollType::wait_indefinitely()).unwrap();
        let held = receiver.try_recv().unwrap();

        let new = RingBuffers::new(&device, &shared, UVec2::new(4, 2));
        drop(old);
        // The held slot stays taken, so the new ring cannot hand its buffer out yet.
        let others: Vec<usize> = std::iter::from_fn(|| shared.slots.reserve()).collect();
        assert!(!others.contains(&held_slot));
        for &slot in &others {
            shared.slots.free(slot);
        }
        drop(held);
        let slot = shared.slots.reserve().unwrap();
        assert_eq!(slot, held_slot);
        map_slot(job(1), slot, new.slots[slot].clone());
        device.poll(PollType::wait_indefinitely()).unwrap();
        drop(receiver.try_recv().unwrap());
        assert_eq!(shared.slots.free_count(), RING);
    }
}
