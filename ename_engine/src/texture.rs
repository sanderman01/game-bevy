//! Texture sampling defaults. Textures are mipmapped and sampled trilinearly with anisotropic
//! filtering unless they say otherwise: `EnginePlugins` makes [`default_sampler`] the sampler of
//! every image left at `ImageSampler::Default` and of every glTF texture. See the "Textures"
//! section of `docs/conventions.md`.

use bevy::image::{ImageFilterMode, ImageSamplerDescriptor};

/// The anisotropy textures get by default. 16 is the most any GPU offers; its cost is small next to
/// the blur at grazing angles it removes.
pub const DEFAULT_ANISOTROPY: u16 = 16;

/// Trilinear and anisotropic, clamped at the edges as Bevy's own default is. A glTF texture keeps
/// its file's wrap modes; anisotropy makes Bevy ignore the file's filters, which it must, since
/// wgpu requires linear filtering for it.
pub fn default_sampler() -> ImageSamplerDescriptor {
    ImageSamplerDescriptor {
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: DEFAULT_ANISOTROPY,
        ..ImageSamplerDescriptor::linear()
    }
}
