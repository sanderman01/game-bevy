//! What the texture loader does to a default texture: a mip chain where it has none, then block
//! compression where the GPU decodes BC, and the result's form in the disk cache.

use bevy::{
    asset::RenderAssetUsages,
    image::{Image, ImageSampler},
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use texpresso::{Algorithm, Format, Params};

/// The formats a default texture leaves the loader in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Output {
    Rgba8 {
        srgb: bool,
    },
    /// Opaque colour or data.
    Bc1 {
        srgb: bool,
    },
    /// Colour with alpha.
    Bc3 {
        srgb: bool,
    },
    /// A tangent-space normal map: X and Y kept, Z rebuilt by the shader.
    Bc5,
}

impl Output {
    fn texture_format(self) -> TextureFormat {
        match self {
            Self::Rgba8 { srgb: false } => TextureFormat::Rgba8Unorm,
            Self::Rgba8 { srgb: true } => TextureFormat::Rgba8UnormSrgb,
            Self::Bc1 { srgb: false } => TextureFormat::Bc1RgbaUnorm,
            Self::Bc1 { srgb: true } => TextureFormat::Bc1RgbaUnormSrgb,
            Self::Bc3 { srgb: false } => TextureFormat::Bc3RgbaUnorm,
            Self::Bc3 { srgb: true } => TextureFormat::Bc3RgbaUnormSrgb,
            Self::Bc5 => TextureFormat::Bc5RgUnorm,
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Rgba8 { srgb } => u8::from(srgb),
            Self::Bc1 { srgb } => 2 + u8::from(srgb),
            Self::Bc3 { srgb } => 4 + u8::from(srgb),
            Self::Bc5 => 6,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 | 1 => Self::Rgba8 { srgb: code == 1 },
            2 | 3 => Self::Bc1 { srgb: code == 3 },
            4 | 5 => Self::Bc3 { srgb: code == 5 },
            6 => Self::Bc5,
            _ => return None,
        })
    }
}

/// A default texture after processing: every mip level, level 0 first, in one format.
pub(super) struct Processed {
    pub output: Output,
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub data: Vec<u8>,
}

/// Whether the loader can take this decoded image: a plain 2D RGBA8 texture. Anything else, such
/// as an array, a cube map or a format already compressed, is left as loaded.
pub(super) fn processable(image: &Image) -> bool {
    let descriptor = &image.texture_descriptor;
    matches!(
        descriptor.format,
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb
    ) && descriptor.dimension == TextureDimension::D2
        && descriptor.size.depth_or_array_layers == 1
        && image.texture_view_descriptor.is_none()
        && image.data.is_some()
}

/// Mips where the image has none, then BC where `bc` (the GPU decodes it) and the size allows.
pub(super) fn process(image: &Image, bc: bool) -> Processed {
    let srgb = image.texture_descriptor.format == TextureFormat::Rgba8UnormSrgb;
    let (width, height) = (image.width(), image.height());
    let data = image.data.as_deref().unwrap_or_default();
    let levels = if image.texture_descriptor.mip_level_count > 1 {
        split_levels(
            data,
            width,
            height,
            image.texture_descriptor.mip_level_count,
        )
    } else {
        mip_chain(data, width, height, srgb)
    };
    let mip_levels = levels.len() as u32;
    // BC encodes 4 x 4 blocks; the GPU needs the top level whole blocks.
    let output = if !bc || width % 4 != 0 || height % 4 != 0 {
        Output::Rgba8 { srgb }
    } else if !srgb && is_normal_map(&levels[0].texels) {
        Output::Bc5
    } else if levels[0]
        .texels
        .as_chunks::<4>()
        .0
        .iter()
        .any(|t| t[3] < 250)
    {
        Output::Bc3 { srgb }
    } else {
        Output::Bc1 { srgb }
    };
    let data = match output {
        Output::Rgba8 { .. } => levels.into_iter().flat_map(|level| level.texels).collect(),
        Output::Bc1 { .. } => compress(&levels, Format::Bc1),
        Output::Bc3 { .. } => compress(&levels, Format::Bc3),
        Output::Bc5 => compress(&levels, Format::Bc5),
    };
    Processed {
        output,
        width,
        height,
        mip_levels,
        data,
    }
}

impl Processed {
    /// The image to hand out, sampled and kept as the load's settings say.
    pub fn into_image(self, sampler: ImageSampler, usage: RenderAssetUsages) -> Image {
        let mut image = Image::new_uninit(
            Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            self.output.texture_format(),
            usage,
        );
        image.texture_descriptor.mip_level_count = self.mip_levels;
        image.data = Some(self.data);
        image.sampler = sampler;
        image
    }

    /// Format code, width, height, mip count, then the data.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(13 + self.data.len());
        bytes.push(self.output.code());
        for value in [self.width, self.height, self.mip_levels] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&self.data);
        bytes
    }

    pub fn from_bytes(mut bytes: Vec<u8>) -> Option<Self> {
        let output = Output::from_code(*bytes.first()?)?;
        let word = |i: usize| -> Option<u32> {
            Some(u32::from_le_bytes(
                bytes.get(1 + 4 * i..5 + 4 * i)?.try_into().ok()?,
            ))
        };
        let (width, height, mip_levels) = (word(0)?, word(1)?, word(2)?);
        bytes.drain(..13);
        Some(Self {
            output,
            width,
            height,
            mip_levels,
            data: bytes,
        })
    }
}

/// One mip level, RGBA8.
struct Level {
    width: u32,
    height: u32,
    texels: Vec<u8>,
}

fn split_levels(data: &[u8], width: u32, height: u32, count: u32) -> Vec<Level> {
    let mut levels = Vec::with_capacity(count as usize);
    let mut offset = 0;
    for i in 0..count {
        let (w, h) = ((width >> i).max(1), (height >> i).max(1));
        let len = (w * h * 4) as usize;
        let Some(texels) = data.get(offset..offset + len) else {
            break;
        };
        levels.push(Level {
            width: w,
            height: h,
            texels: texels.to_vec(),
        });
        offset += len;
    }
    levels
}

/// Every level down to 1 x 1, each texel the mean of the 2 x 2 above it; colour averaged in linear
/// light for an sRGB image, alpha as it is. An odd edge repeats its last texel.
fn mip_chain(data: &[u8], width: u32, height: u32, srgb: bool) -> Vec<Level> {
    let decode: Vec<f32> = (0..=255u8)
        .map(|c| {
            let c = f32::from(c) / 255.0;
            if srgb {
                if c <= 0.04045 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            } else {
                c
            }
        })
        .collect();
    let encode = |c: f32| -> u8 {
        let c = c.clamp(0.0, 1.0);
        let c = if !srgb {
            c
        } else if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (c * 255.0).round() as u8
    };
    let mut levels = vec![Level {
        width,
        height,
        texels: data.to_vec(),
    }];
    let mut linear: Vec<[f32; 4]> = data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|t| {
            [
                decode[t[0] as usize],
                decode[t[1] as usize],
                decode[t[2] as usize],
                f32::from(t[3]) / 255.0,
            ]
        })
        .collect();
    let (mut w, mut h) = (width, height);
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = Vec::with_capacity((nw * nh) as usize);
        for y in 0..nh {
            for x in 0..nw {
                let mut sum = [0.0f32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (sx, sy) = ((2 * x + dx).min(w - 1), (2 * y + dy).min(h - 1));
                    let texel = linear[(sy * w + sx) as usize];
                    for (s, t) in sum.iter_mut().zip(texel) {
                        *s += t;
                    }
                }
                next.push(sum.map(|s| s / 4.0));
            }
        }
        let texels = next
            .iter()
            .flat_map(|t| {
                [
                    encode(t[0]),
                    encode(t[1]),
                    encode(t[2]),
                    (t[3].clamp(0.0, 1.0) * 255.0).round() as u8,
                ]
            })
            .collect();
        levels.push(Level {
            width: nw,
            height: nh,
            texels,
        });
        linear = next;
        (w, h) = (nw, nh);
    }
    levels
}

/// Whether linear RGBA8 texels read as a tangent-space normal map: nearly every sampled texel a
/// unit vector pointing out of the surface.
fn is_normal_map(texels: &[u8]) -> bool {
    let samples: Vec<&[u8; 4]> = texels.as_chunks::<4>().0.iter().step_by(61).collect();
    let normal = samples
        .iter()
        .filter(|t| {
            let [x, y, z] = [t[0], t[1], t[2]].map(|c| f32::from(c) / 127.5 - 1.0);
            z > 0.0 && ((x * x + y * y + z * z).sqrt() - 1.0).abs() < 0.12
        })
        .count();
    !samples.is_empty() && normal * 100 >= samples.len() * 97
}

/// Every level block-compressed, level 0 first. Range fit: cluster fit is several times slower
/// for little gain at the distances most textures are seen.
fn compress(levels: &[Level], format: Format) -> Vec<u8> {
    let params = Params {
        algorithm: Algorithm::RangeFit,
        ..Default::default()
    };
    let mut out = Vec::new();
    for level in levels {
        let (w, h) = (level.width as usize, level.height as usize);
        let mut blocks = vec![0; format.compressed_size(w, h)];
        format.compress(&level.texels, w, h, params, &mut blocks);
        out.extend(blocks);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, texel: [u8; 4], format: TextureFormat) -> Image {
        let mut image = Image::new_uninit(
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            format,
            RenderAssetUsages::default(),
        );
        image.data = Some(texel.repeat((width * height) as usize));
        image
    }

    #[test]
    fn a_full_chain_is_made_down_to_one_texel() {
        let processed = process(
            &image(8, 4, [200, 100, 50, 255], TextureFormat::Rgba8UnormSrgb),
            false,
        );
        assert_eq!(processed.mip_levels, 4);
        assert_eq!(processed.output, Output::Rgba8 { srgb: true });
        assert_eq!(processed.data.len(), (32 + 8 + 2 + 1) * 4);
        // A flat colour stays that colour all the way down.
        assert!(
            processed
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .all(|t| *t == [200, 100, 50, 255])
        );
    }

    #[test]
    fn formats_follow_the_content() {
        let opaque = process(
            &image(8, 8, [90, 90, 90, 255], TextureFormat::Rgba8UnormSrgb),
            true,
        );
        assert_eq!(opaque.output, Output::Bc1 { srgb: true });
        let cut_out = process(
            &image(8, 8, [90, 90, 90, 0], TextureFormat::Rgba8UnormSrgb),
            true,
        );
        assert_eq!(cut_out.output, Output::Bc3 { srgb: true });
        let normal = process(
            &image(8, 8, [128, 128, 255, 255], TextureFormat::Rgba8Unorm),
            true,
        );
        assert_eq!(normal.output, Output::Bc5);
        let data = process(
            &image(8, 8, [0, 200, 30, 255], TextureFormat::Rgba8Unorm),
            true,
        );
        assert_eq!(data.output, Output::Bc1 { srgb: false });
        // Not whole blocks: mips only.
        let odd = process(
            &image(6, 6, [90, 90, 90, 255], TextureFormat::Rgba8UnormSrgb),
            true,
        );
        assert_eq!(odd.output, Output::Rgba8 { srgb: true });
    }

    #[test]
    fn processed_textures_round_trip_through_cache_bytes() {
        let processed = process(
            &image(8, 8, [90, 90, 90, 255], TextureFormat::Rgba8UnormSrgb),
            true,
        );
        let read = Processed::from_bytes(processed.to_bytes()).unwrap();
        assert_eq!(read.output, processed.output);
        assert_eq!((read.width, read.height, read.mip_levels), (8, 8, 4));
        assert_eq!(read.data, processed.data);
        // BC1: 8 bytes a 4 x 4 block, a small level still one block.
        assert_eq!(read.data.len(), 8 * (4 + 1 + 1 + 1));
    }
}
