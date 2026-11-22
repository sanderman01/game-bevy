//! Texture defaults. A texture is mipmapped, block compressed where the GPU decodes BC, and sampled
//! trilinearly with anisotropic filtering, unless it says otherwise. See the "Textures" section of
//! `docs/conventions.md`.
//!
//! - `EnginePlugins` makes [`default_sampler`] the sampler of every image left at
//!   `ImageSampler::Default` and of every glTF texture.
//! - [`TextureLoadPlugin`] replaces Bevy's image loader with one that gives each default texture
//!   loaded from a file a mip chain where it has none, then encodes it as BC1, BC3 or BC5, and keeps
//!   the result in the disk cache, so a second load only reads it back.

mod loader;
mod process;

use bevy::{
    image::{
        CompressedImageFormatSupport, CompressedImageFormats, ImageFilterMode,
        ImageSamplerDescriptor,
    },
    prelude::*,
};
use ename_filesystem_cache::DiskCache;

use crate::application::{ApplicationPaths, Paths};

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

/// Registers the texture loader in `finish`, after Bevy's render plugin has registered its own
/// image loader and found which compressed formats the GPU takes; the later loader wins. Bevy warns
/// once that two image loaders share their extensions.
///
/// A default texture is one loaded from a file to be drawn, as a single 2D RGBA8 image, with an
/// anisotropic or default sampler. A data texture opts out by giving its load a sampler of its
/// own without anisotropy, or by keeping its texels on the CPU only. Images inside a `.glb` are
/// decoded by the glTF loader itself and pass by; models reference their textures as files.
pub struct TextureLoadPlugin;

impl Plugin for TextureLoadPlugin {
    fn build(&self, _app: &mut App) {}

    fn finish(&self, app: &mut App) {
        let formats = app
            .world()
            .get_resource::<CompressedImageFormatSupport>()
            .map_or(CompressedImageFormats::NONE, |support| support.0);
        let paths = app
            .world()
            .get_resource::<ApplicationPaths>()
            .map_or_else(Paths::default, |paths| paths.paths().clone());
        let cache = DiskCache::new(paths, "engine/textures");
        app.register_asset_loader(loader::TextureLoader::new(formats, cache));
    }
}
