//! The image loader: Bevy's, with default textures given mips and block compression on the way in,
//! and the result kept in the disk cache.

use bevy::{
    asset::{
        AssetLoader, LoadContext, RenderAssetUsages,
        io::{Reader, VecReader},
    },
    image::{
        CompressedImageFormats, Image, ImageLoader, ImageLoaderError, ImageLoaderSettings,
        ImageSampler,
    },
    log::{debug, warn},
    reflect::TypePath,
};
use ename_filesystem_cache::{DiskCache, KeyHasher};

use super::process::{Processed, process, processable};

/// Part of every cache key. Bump it when a dependency changes what processing makes, such as a
/// `texpresso` upgrade; edits to the processing code change the keys by themselves.
const CACHE_VERSION: u32 = 1;

/// Registered for every extension Bevy's `ImageLoader` reads, after it, so it is the one used.
/// Same settings type, so loads that set `ImageLoaderSettings` work unchanged.
#[derive(TypePath)]
pub(super) struct TextureLoader {
    pub inner: ImageLoader,
    /// The GPU decodes BC blocks.
    pub bc: bool,
    pub cache: DiskCache,
}

impl TextureLoader {
    pub fn new(formats: CompressedImageFormats, cache: DiskCache) -> Self {
        Self {
            inner: ImageLoader::new(formats),
            bc: formats.contains(CompressedImageFormats::BC),
            cache,
        }
    }
}

/// A default texture: drawn by the GPU, a single image, sampled with anisotropy. A data texture says
/// otherwise with a sampler of its own without anisotropy, or keeps its texels on the CPU only.
fn is_default_texture(settings: &ImageLoaderSettings) -> bool {
    let anisotropic = match &settings.sampler {
        ImageSampler::Default => true,
        ImageSampler::Descriptor(descriptor) => descriptor.anisotropy_clamp > 1,
    };
    anisotropic
        && settings
            .asset_usage
            .contains(RenderAssetUsages::RENDER_WORLD)
        && settings.array_layout.is_none()
        && settings.texture_format.is_none()
}

impl AssetLoader for TextureLoader {
    type Asset = Image;
    type Settings = ImageLoaderSettings;
    type Error = ImageLoaderError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        settings: &ImageLoaderSettings,
        load_context: &mut LoadContext<'_>,
    ) -> Result<Image, ImageLoaderError> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        if !is_default_texture(settings) {
            return self
                .inner
                .load(&mut VecReader::new(bytes), settings, load_context)
                .await;
        }
        let key = KeyHasher::new()
            .bytes(include_bytes!("process.rs"))
            .bytes(include_bytes!("loader.rs"))
            .value(CACHE_VERSION)
            .value(u8::from(self.bc))
            .value(u8::from(settings.is_srgb))
            .value(u8::from(settings.expand_grayscale))
            .str(&format!("{:?}", settings.format))
            .bytes(&bytes)
            .finish();
        if let Some(processed) = self.cache.get(&key).and_then(Processed::from_bytes) {
            return Ok(processed.into_image(settings.sampler.clone(), settings.asset_usage));
        }
        let image = self
            .inner
            .load(&mut VecReader::new(bytes), settings, load_context)
            .await?;
        if !processable(&image) {
            return Ok(image);
        }
        let processed = process(&image, self.bc);
        debug!(
            "{}: {:?}, {} mip levels",
            load_context.path(),
            processed.output,
            processed.mip_levels
        );
        if let Err(error) = self.cache.put(&key, &processed.to_bytes()) {
            warn!(
                "{}: storing the processed texture in the disk cache failed: {error}",
                load_context.path()
            );
        }
        Ok(processed.into_image(settings.sampler.clone(), settings.asset_usage))
    }

    fn extensions(&self) -> &[&str] {
        ImageLoader::SUPPORTED_FILE_EXTENSIONS
    }
}
