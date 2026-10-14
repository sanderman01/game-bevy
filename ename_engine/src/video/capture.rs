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
//! writer) or `failed`. A ring slot is reserved in `PrepareViews` and freed on every path that
//! does not reach the writer; a frame that does reach it is freed when the writer drops it
//! ([`MappedFrame`] unmaps and frees on drop), so slots cannot leak.
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
            BindGroupEntry, BindGroupLayout, BindGroupLayoutEntry, BindingResource, BindingType,
            Buffer, BufferDescriptor, BufferUsages, ColorTargetState, ColorWrites, Extent3d,
            LoadOp, MapMode, MultisampleState, Operations, PipelineCompilationOptions,
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
    nv12::{Nv12Pipelines, buffer_len, frame_len},
};

/// Buffers per recording. One is usually held by the writer as the previous frame, the rest
/// cover the frame or two a map takes.
pub(crate) const RING: usize = 4;

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
    pub failed: AtomicU64,
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
            failed: AtomicU64::new(0),
            error: OnceLock::new(),
        };
        (Arc::new(shared), receiver)
    }

    /// Stops taking frames. The writer sees its channel close once every in-flight frame has
    /// been delivered or dropped.
    pub(crate) fn close(&self) {
        self.lock_sender().take();
    }

    /// Requests not yet delivered or failed.
    pub(crate) fn in_flight(&self) -> u64 {
        let requested = self.requested.load(Ordering::Acquire);
        let ended = self.delivered.load(Ordering::Acquire) + self.failed.load(Ordering::Acquire);
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

    fn fail(&self) {
        self.failed.fetch_add(1, Ordering::AcqRel);
    }

    /// Hands a frame to the writer, counting it as delivered or, if the writer is gone, failed.
    /// A frame that cannot be sent is dropped here, which frees its slot.
    fn deliver(&self, sender: Option<Sender<Ready>>, ready: Ready) {
        match sender.map(|sender| sender.send(ready)) {
            Some(Ok(())) => self.delivered.fetch_add(1, Ordering::AcqRel),
            _ => self.failed.fetch_add(1, Ordering::AcqRel),
        };
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

    /// Fails a request as the render side would, for tests.
    #[cfg(test)]
    pub(crate) fn fail_for_test(&self) {
        self.fail();
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

impl std::fmt::Debug for CaptureShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureShared")
            .field("id", &self.id)
            .finish()
    }
}

/// This frame's captures, filled in `Last` by `request_frames`, read by the extract.
#[derive(Resource, Default)]
pub(crate) struct PendingCaptures(pub Vec<CaptureJob>);

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
struct ExtractedCaptures(Vec<CaptureJob>);

fn extract_captures(
    mut extracted: ResMut<ExtractedCaptures>,
    pending: Extract<Res<PendingCaptures>>,
) {
    extracted.0.clone_from(&pending.0);
}

/// GPU buffers of one recording's ring.
struct RingBuffers {
    size: UVec2,
    nv12: Buffer,
    slots: [Buffer; RING],
    shared: Arc<CaptureShared>,
}

/// GPU objects that live across frames: each recording's ring, each recorded window's redirect.
#[derive(Resource, Default)]
struct CaptureGpu {
    rings: HashMap<u64, RingBuffers>,
    redirects: WindowRedirects,
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
}

#[derive(Default)]
struct WindowRedirects(HashMap<Entity, Redirect>);

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
    let CaptureGpu { rings, redirects } = &mut *gpu;
    // A recording that has ended holds the last reference to its ring.
    rings.retain(|_, ring| Arc::strong_count(&ring.shared) > 1);

    for job in std::mem::take(&mut extracted.0) {
        let shared = job.shared.clone();
        if shared.error.get().is_some() {
            shared.fail();
            continue;
        }
        let size = job.rect.size();
        let ring = rings
            .entry(shared.id)
            .or_insert_with(|| ring(&device, &shared, size));
        if ring.size != size {
            *ring = self::ring(&device, &shared, size);
        }
        let Some(slot) = shared.slots.reserve() else {
            shared.fail();
            continue;
        };
        let source = match &job.key {
            NormalizedRenderTarget::Window(window) => {
                let window = window.entity();
                prepare_window(&job, window, &windows, redirects, &mut attachments, &device)
                    .inspect(|_| {
                        if !prepared.redirected.iter().any(|(_, w)| *w == window) {
                            prepared.redirected.push((job.key.clone(), window));
                        }
                    })
            }
            NormalizedRenderTarget::Image(image) => {
                prepare_image(&job, &image.handle, &images, &attachments)
            }
            _ => None,
        };
        match source {
            Some((source, srgb_source, expected)) => prepared.captures.push(PreparedCapture {
                job,
                slot,
                source,
                srgb_source,
                expected,
            }),
            None => {
                shared.slots.free(slot);
                shared.fail();
            }
        }
    }
}

fn ring(device: &RenderDevice, shared: &Arc<CaptureShared>, size: UVec2) -> RingBuffers {
    RingBuffers {
        size,
        nv12: Nv12Pipelines::output_buffer(device, size),
        slots: std::array::from_fn(|_| {
            device.create_buffer(&BufferDescriptor {
                label: Some("video_ring"),
                size: buffer_len(size),
                usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        }),
        shared: shared.clone(),
    }
}

/// The window's redirect texture as the capture source, installing the redirect if the
/// window's attachment is still its own swapchain.
fn prepare_window(
    job: &CaptureJob,
    window: Entity,
    windows: &ExtractedWindows,
    redirects: &mut WindowRedirects,
    attachments: &mut ViewTargetAttachments,
    device: &RenderDevice,
) -> Option<(TextureView, bool, TextureViewId)> {
    let extracted = windows.get(&window)?;
    let swapchain = extracted.swap_chain_texture_view.as_ref()?;
    let format = extracted.swap_chain_texture_view_format?;
    if !is_supported(format) {
        let _ = job.shared.error.set(VideoError::UnsupportedFormat(format));
        return None;
    }
    let size = UVec2::new(extracted.physical_width, extracted.physical_height);
    if job.rect.max.cmpgt(size).any() {
        return None;
    }
    let redirect = redirects
        .0
        .entry(window)
        .or_insert_with(|| redirect(device, size, format));
    if redirect.size != size || redirect.format != format {
        *redirect = self::redirect(device, size, format);
    }
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
    } else if current != Some(ours) {
        // Another redirect (a screenshot), or no camera draws this window this frame.
        return None;
    }
    Some((redirect.raw_view.clone(), false, ours))
}

fn redirect(device: &RenderDevice, size: UVec2, format: TextureFormat) -> Redirect {
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
    Redirect {
        size,
        format,
        srgb_view: texture.create_view(&TextureViewDescriptor::default()),
        raw_view: texture.create_view(&TextureViewDescriptor {
            format: Some(raw),
            ..default()
        }),
        _texture: texture,
    }
}

/// The image the camera draws into, if nothing has redirected it this frame.
fn prepare_image(
    job: &CaptureJob,
    handle: &Handle<Image>,
    images: &RenderAssets<GpuImage>,
    attachments: &ViewTargetAttachments,
) -> Option<(TextureView, bool, TextureViewId)> {
    let image = images.get(handle)?;
    let format = image.texture_descriptor.format;
    if !is_supported(format) {
        let _ = job.shared.error.set(VideoError::UnsupportedFormat(format));
        return None;
    }
    let size = UVec2::new(
        image.texture_descriptor.size.width,
        image.texture_descriptor.size.height,
    );
    if job.rect.max.cmpgt(size).any() {
        return None;
    }
    let native = image.texture_view.id();
    // Absent when no camera draws the image this frame; another view when a screenshot
    // redirected it, in which case it is blitted back only after our pass.
    if attachments
        .get(&job.key)
        .map(|attachment| attachment.view.id())
        != Some(native)
    {
        return None;
    }
    Some((image.texture_view.clone(), true, native))
}

/// Pipelines, created on first use.
#[derive(Resource)]
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

    fn blit(&mut self, device: &RenderDevice, format: TextureFormat) -> &RenderPipeline {
        let layout = &self.blit_layout;
        self.blit.entry(format).or_insert_with(|| {
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
    if !world.contains_resource::<CapturePipelines>() {
        world.insert_resource(CapturePipelines::new(&device));
    }
    world.resource_scope(|world, mut pipelines: Mut<CapturePipelines>| {
        let attachments = world.resource::<ViewTargetAttachments>();
        let windows = world.resource::<ExtractedWindows>();
        let CaptureGpu { rings, redirects } = world.resource::<CaptureGpu>();
        let current = |key: &NormalizedRenderTarget| {
            attachments.get(key).map(|attachment| attachment.view.id())
        };

        let mut encoder = device.create_command_encoder(&default());
        for (key, window) in &redirected {
            let (Some(redirect), Some(extracted)) = (redirects.0.get(window), windows.get(window))
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
            let pipeline = pipelines.blit(&device, redirect.format).clone();
            let bind_group = device.create_bind_group(
                "video_blit",
                &pipelines.blit_layout,
                &[BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(&redirect.srgb_view),
                }],
            );
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
            pass.set_bind_group(0, &*bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        let mut mapped = Vec::new();
        for capture in captures {
            let shared = &capture.job.shared;
            let Some(ring) = rings.get(&shared.id) else {
                shared.slots.free(capture.slot);
                shared.fail();
                continue;
            };
            if current(&capture.job.key) != Some(capture.expected) {
                shared.slots.free(capture.slot);
                shared.fail();
                continue;
            }
            pipelines.nv12.encode(
                &device,
                &mut encoder,
                &capture.source,
                capture.srgb_source,
                capture.job.rect,
                &ring.nv12,
            );
            let slot_buffer = ring.slots[capture.slot].clone();
            encoder.copy_buffer_to_buffer(&ring.nv12, 0, &slot_buffer, 0, buffer_len(ring.size));
            mapped.push((capture, slot_buffer));
        }
        world.resource::<RenderQueue>().submit([encoder.finish()]);

        for (capture, buffer) in mapped {
            let job = capture.job;
            let shared = job.shared.clone();
            // Cloned before mapping and never held across `map_async`: the callback may run
            // inline in it.
            let sender = shared.sender();
            let padded = buffer_len(job.rect.size());
            let mapped_buffer = buffer.clone();
            let slots = shared.slots.clone();
            buffer
                .slice(..padded)
                .map_async(MapMode::Read, move |result| {
                    if result.is_err() {
                        slots.free(capture.slot);
                        shared.fail();
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
                            slot: capture.slot,
                        }),
                    };
                    shared.deliver(sender, ready);
                });
        }
        let _ = device.poll(PollType::Poll);
    });
}
