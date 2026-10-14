//! GPU conversion of a captured texture to NV12, the format ffmpeg's encoders take directly.
//!
//! The shader is standalone WGSL built with wgpu directly, not Bevy's pipeline cache, so it has
//! no shader imports to port when Bevy changes its shader tooling.

use std::borrow::Cow;

use bevy::{
    prelude::*,
    render::{
        render_resource::{
            BindGroupEntry, BindGroupLayout, BindGroupLayoutEntry, BindingResource, BindingType,
            Buffer, BufferBindingType, BufferDescriptor, BufferInitDescriptor, BufferUsages,
            CommandEncoder, ComputePassDescriptor, ComputePipeline, PipelineCompilationOptions,
            PipelineLayoutDescriptor, RawComputePipelineDescriptor, ShaderModuleDescriptor,
            ShaderSource, ShaderStages, TextureSampleType, TextureView, TextureViewDimension,
        },
        renderer::RenderDevice,
    },
};

const SHADER: &str = include_str!("nv12.wgsl");
const WORKGROUP: u32 = 256;
/// wgpu's guaranteed limit on workgroups per dimension.
const MAX_GROUPS: u32 = 65_535;

/// Bytes of one NV12 frame of `size`, which must be even in both dimensions.
pub(crate) fn frame_len(size: UVec2) -> u64 {
    u64::from(size.x) * u64::from(size.y) * 3 / 2
}

/// [`frame_len`] rounded up to whole words, the size of the GPU buffers.
pub(crate) fn buffer_len(size: UVec2) -> u64 {
    frame_len(size).next_multiple_of(4)
}

/// The two compute pipelines and their layout.
pub(crate) struct Nv12Pipelines {
    layout: BindGroupLayout,
    luma: ComputePipeline,
    chroma: ComputePipeline,
}

impl Nv12Pipelines {
    pub(crate) fn new(device: &RenderDevice) -> Self {
        let module = device
            .wgpu_device()
            .create_shader_module(ShaderModuleDescriptor {
                label: Some("video_nv12"),
                source: ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
            });
        let layout = device.create_bind_group_layout(
            "video_nv12",
            &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::COMPUTE,
                    ty: BindingType::Texture {
                        sample_type: TextureSampleType::Float { filterable: false },
                        view_dimension: TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::COMPUTE,
                    ty: BindingType::Buffer {
                        ty: BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 2,
                    visibility: ShaderStages::COMPUTE,
                    ty: BindingType::Buffer {
                        ty: BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        );
        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("video_nv12"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry_point| {
            device.create_compute_pipeline(&RawComputePipelineDescriptor {
                label: Some(entry_point),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry_point),
                compilation_options: PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        Self {
            luma: pipeline("luma_main"),
            chroma: pipeline("chroma_main"),
            layout,
        }
    }

    /// The buffer [`Self::encode`] writes into: `STORAGE | COPY_SRC`, [`buffer_len`] bytes.
    pub(crate) fn output_buffer(device: &RenderDevice, size: UVec2) -> Buffer {
        device.create_buffer(&BufferDescriptor {
            label: Some("video_nv12_output"),
            size: buffer_len(size),
            usage: BufferUsages::STORAGE | BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        })
    }

    /// Records the conversion of `rect` of `source` into `output`. `srgb_view` says whether
    /// `source` is an sRGB view, which decodes on load and so needs re-encoding.
    pub(crate) fn encode(
        &self,
        device: &RenderDevice,
        encoder: &mut CommandEncoder,
        source: &TextureView,
        srgb_view: bool,
        rect: URect,
        output: &Buffer,
    ) {
        let size = rect.size();
        let luma_words = size.x * size.y / 4;
        let chroma_words = (size.x / 2 * (size.y / 2)).div_ceil(2);
        let groups = |words: u32| {
            let groups = words.div_ceil(WORKGROUP).max(1);
            let x = groups.min(MAX_GROUPS);
            (x, groups.div_ceil(x))
        };
        for (pipeline, words) in [(&self.luma, luma_words), (&self.chroma, chroma_words)] {
            let (x, y) = groups(words);
            let params: [u32; 8] = [
                rect.min.x,
                rect.min.y,
                size.x,
                size.y,
                x,
                u32::from(srgb_view),
                luma_words,
                0,
            ];
            let params = device.create_buffer_with_data(&BufferInitDescriptor {
                label: Some("video_nv12_params"),
                contents: &params.map(u32::to_le_bytes).concat(),
                usage: BufferUsages::UNIFORM,
            });
            let bind_group = device.create_bind_group(
                "video_nv12",
                &self.layout,
                &[
                    BindGroupEntry {
                        binding: 0,
                        resource: BindingResource::TextureView(source),
                    },
                    BindGroupEntry {
                        binding: 1,
                        resource: output.as_entire_binding(),
                    },
                    BindGroupEntry {
                        binding: 2,
                        resource: params.as_entire_binding(),
                    },
                ],
            );
            let mut pass = encoder.begin_compute_pass(&ComputePassDescriptor {
                label: Some("video_nv12"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &*bind_group, &[]);
            pass.dispatch_workgroups(x, y, 1);
        }
    }
}

/// The shader's maths on the CPU, for tests. `pixels` is RGBA8 holding sRGB-encoded values,
/// `width` pixels per row.
#[cfg(test)]
pub(crate) fn reference(pixels: &[u8], width: u32, rect: URect) -> Vec<u8> {
    let size = rect.size();
    let pixel = |x: u32, y: u32| -> Vec3 {
        let i = (((rect.min.y + y) * width + rect.min.x + x) * 4) as usize;
        Vec3::new(
            f32::from(pixels[i]),
            f32::from(pixels[i + 1]),
            f32::from(pixels[i + 2]),
        ) / 255.0
    };
    let luma = |c: Vec3| c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
    let byte = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    let mut out = Vec::with_capacity(frame_len(size) as usize);
    for y in 0..size.y {
        for x in 0..size.x {
            out.push(byte(16.0 + 219.0 * luma(pixel(x, y))));
        }
    }
    for cy in 0..size.y / 2 {
        for cx in 0..size.x / 2 {
            let x = 2 * cx;
            let left = x.saturating_sub(1);
            let mut sum = Vec3::ZERO;
            for y in [2 * cy, 2 * cy + 1] {
                sum += pixel(left, y) + 2.0 * pixel(x, y) + pixel(x + 1, y);
            }
            let rgb = sum / 8.0;
            let y = luma(rgb);
            out.push(byte(128.0 + 224.0 * (rgb.z - y) / 1.8556));
            out.push(byte(128.0 + 224.0 * (rgb.x - y) / 1.5748));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_pad_to_words() {
        assert_eq!(frame_len(UVec2::new(2, 2)), 6);
        assert_eq!(buffer_len(UVec2::new(2, 2)), 8);
        assert_eq!(buffer_len(UVec2::new(2560, 1382)), 2560 * 1382 * 3 / 2);
    }

    #[test]
    fn reference_matches_the_bt709_tv_range_extremes() {
        // White, black, red, blue in a 2x2 block of a 4x2 image (the right half is ignored).
        let pixels = [
            [255, 255, 255, 255],
            [0, 0, 0, 255],
            [9, 9, 9, 255],
            [9, 9, 9, 255],
            [255, 0, 0, 255],
            [0, 0, 255, 255],
            [9, 9, 9, 255],
            [9, 9, 9, 255],
        ]
        .concat();
        let out = reference(&pixels, 4, URect::new(0, 0, 2, 2));
        // Luma: white 235, black 16, red 16 + 219 * 0.2126 = 62.6, blue 16 + 219 * 0.0722 = 31.8.
        assert_eq!(&out[..4], &[235, 16, 63, 32]);
        // Left-sited chroma at the image edge: columns (0, 0, 1) weighted (1, 2, 1).
        let rgb = (Vec3::new(1.0, 1.0, 1.0) * 3.0 + Vec3::ZERO // row 0: white x3, black x1
            + Vec3::new(1.0, 0.0, 0.0) * 3.0 + Vec3::new(0.0, 0.0, 1.0))
            / 8.0;
        let y = rgb.dot(Vec3::new(0.2126, 0.7152, 0.0722));
        let cb = (128.0 + 224.0 * (rgb.z - y) / 1.8556).round() as u8;
        let cr = (128.0 + 224.0 * (rgb.x - y) / 1.5748).round() as u8;
        assert_eq!(&out[4..], &[cb, cr]);
    }

    /// A small RGBA image with distinct pixels, sRGB-encoded.
    fn pattern(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                data.extend([
                    (x * 37 + y * 11) as u8,
                    (x * 91 + 40) as u8,
                    (y * 53 + x * 7 + 3) as u8,
                    255,
                ]);
            }
        }
        data
    }

    /// Runs the shader on a real GPU and compares with [`reference`]. Skipped without an adapter.
    #[test]
    fn the_shader_matches_the_reference_on_a_gpu() {
        use bevy::render::render_resource::{
            Extent3d, MapMode, PollType, TexelCopyBufferLayout, TexelCopyTextureInfo,
            TextureAspect, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
            TextureViewDescriptor,
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = bevy::tasks::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        ) else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (device, queue) =
            bevy::tasks::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .unwrap();
        let device = RenderDevice::from(device);
        let pipelines = Nv12Pipelines::new(&device);

        // An odd-sized texture, so the crop's edges and a chroma row that ends mid-word are both
        // exercised: 6x4 at (1, 1) of a 9x7 image.
        let (width, height) = (9, 7);
        let rect = URect::new(1, 1, 7, 5);
        let rgba = pattern(width, height);
        let bgra: Vec<u8> = rgba
            .chunks(4)
            .flat_map(|p| [p[2], p[1], p[0], p[3]])
            .collect();
        let expected = reference(&rgba, width, rect);

        for (format, data, raw) in [
            (TextureFormat::Rgba8UnormSrgb, &rgba, false),
            (TextureFormat::Bgra8UnormSrgb, &bgra, false),
            (TextureFormat::Rgba8UnormSrgb, &rgba, true),
        ] {
            let raw_format = format.remove_srgb_suffix();
            let texture = device.create_texture(&TextureDescriptor {
                label: None,
                size: Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format,
                usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
                view_formats: &[raw_format],
            });
            queue.write_texture(
                TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: Default::default(),
                    aspect: TextureAspect::All,
                },
                data,
                TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: None,
                },
                Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            let view = texture.create_view(&TextureViewDescriptor {
                format: raw.then_some(raw_format),
                ..Default::default()
            });
            let output = Nv12Pipelines::output_buffer(&device, rect.size());
            let staging = device.create_buffer(&BufferDescriptor {
                label: None,
                size: buffer_len(rect.size()),
                usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            pipelines.encode(&device, &mut encoder, &view, !raw, rect, &output);
            encoder.copy_buffer_to_buffer(&output, 0, &staging, 0, buffer_len(rect.size()));
            queue.submit([encoder.finish()]);
            staging
                .slice(..)
                .map_async(MapMode::Read, |result| result.unwrap());
            device.poll(PollType::wait_indefinitely()).unwrap();
            let actual = staging.slice(..).get_mapped_range()[..expected.len()].to_vec();
            staging.unmap();

            // sRGB decode and re-encode, and GPU rounding, may each move a value by one.
            for (i, (a, e)) in actual.iter().zip(&expected).enumerate() {
                assert!(
                    a.abs_diff(*e) <= 1,
                    "{format:?} raw={raw}: byte {i} is {a}, expected {e}"
                );
            }
        }
    }

    /// Encodes an NV12 frame losslessly with the real ffmpeg, decodes it back to RGB, and
    /// returns the mean absolute error against `rgba` per column. `None` without ffmpeg.
    fn round_trip_column_error(rgba: &[u8], size: UVec2, nv12: &[u8]) -> Option<Vec<f32>> {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let (input, video, output) = (
            dir.path().join("in.nv12"),
            dir.path().join("out.mkv"),
            dir.path().join("out.rgb"),
        );
        std::fs::write(&input, nv12).unwrap();
        let dims = format!("{}x{}", size.x, size.y);
        let run = |args: &[&str]| {
            Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(args)
                .status()
                .map(|status| status.success())
        };
        let Ok(true) = run(&[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "nv12",
            "-s",
            &dims,
            "-i",
            input.to_str()?,
            "-vf",
            "setparams=color_primaries=bt709:color_trc=iec61966-2-1:colorspace=bt709:range=tv",
            "-c:v",
            "libx264",
            "-qp",
            "0",
            video.to_str()?,
        ]) else {
            return None;
        };
        // An interpolating decode with chroma at the left position H.264 declares. swscale's
        // default would duplicate each chroma sample, and treats 4:2:0 as centre-sited when the
        // position is unset, so both are pinned here.
        let decoded_ok = run(&[
            "-i",
            video.to_str()?,
            "-vf",
            "scale=flags=bilinear+full_chroma_int+accurate_rnd:in_color_matrix=bt709:\
             in_range=tv:in_h_chr_pos=0:in_v_chr_pos=128,format=rgb24",
            "-f",
            "rawvideo",
            output.to_str()?,
        ]);
        if decoded_ok.ok() != Some(true) {
            return None;
        }
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=chroma_location",
                "-of",
                "csv=p=0",
            ])
            .arg(&video)
            .output()
            .ok()?;
        let location = String::from_utf8_lossy(&probe.stdout).trim().to_owned();
        assert!(
            location == "left" || location == "unspecified",
            "chroma location {location}"
        );
        let decoded = std::fs::read(&output).unwrap();
        let mut errors = vec![0.0; size.x as usize];
        for y in 0..size.y as usize {
            for x in 0..size.x as usize {
                for c in 0..3 {
                    let source = rgba[(y * size.x as usize + x) * 4 + c];
                    let back = decoded[(y * size.x as usize + x) * 3 + c];
                    errors[x] += f32::from(source.abs_diff(back)) / (3.0 * size.y as f32);
                }
            }
        }
        Some(errors)
    }

    /// Chroma sited where decoders expect it survives a round trip through ffmpeg in place: on a
    /// horizontal red-to-blue ramp, left-sited chroma comes back closer to the source than
    /// centre-sited chroma, which shifts by half a pixel.
    #[test]
    fn left_sited_chroma_round_trips_through_ffmpeg() {
        let _guard = super::super::encoder::tests::lock_processes();
        let size = UVec2::new(32, 8);
        let mut rgba = Vec::new();
        for _ in 0..size.y {
            for x in 0..size.x {
                let t = (x * 255 / (size.x - 1)) as u8;
                rgba.extend([t, 40, 255 - t, 255]);
            }
        }
        let left_sited = reference(&rgba, size.x, URect::from_corners(UVec2::ZERO, size));
        // The same frame with centre-sited chroma (a plain 2x2 average), for comparison.
        let mut centred = left_sited.clone();
        let luma_len = (size.x * size.y) as usize;
        for cy in 0..size.y / 2 {
            for cx in 0..size.x / 2 {
                let mut rgb = Vec3::ZERO;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let i = (((2 * cy + dy) * size.x + 2 * cx + dx) * 4) as usize;
                    rgb += Vec3::new(
                        f32::from(rgba[i]),
                        f32::from(rgba[i + 1]),
                        f32::from(rgba[i + 2]),
                    );
                }
                let rgb = rgb / (4.0 * 255.0);
                let y = rgb.dot(Vec3::new(0.2126, 0.7152, 0.0722));
                let at = luma_len + ((cy * size.x / 2 + cx) * 2) as usize;
                centred[at] = (128.0 + 224.0 * (rgb.z - y) / 1.8556).round() as u8;
                centred[at + 1] = (128.0 + 224.0 * (rgb.x - y) / 1.5748).round() as u8;
            }
        }
        let Some(left) = round_trip_column_error(&rgba, size, &left_sited) else {
            eprintln!("no ffmpeg with libx264; skipping");
            return;
        };
        let centre = round_trip_column_error(&rgba, size, &centred).unwrap();
        // The outer columns are clamped differently by each siting; compare the interior.
        let total = |errors: &[f32]| errors[2..errors.len() - 2].iter().sum::<f32>();
        eprintln!("left-sited {left:?}\ncentre-sited {centre:?}");
        // Left-sited chroma comes back to within rounding; centre-sited is off by about 3 values
        // per channel on every pixel of the ramp.
        let columns = (left.len() - 4) as f32;
        assert!(total(&left) / columns < 1.0, "left-sited error {left:?}");
        assert!(
            total(&centre) > 4.0 * total(&left),
            "centre-sited error {centre:?}"
        );
    }
}
