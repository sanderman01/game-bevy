//! Shaders by alias.
//!
//! Bevy's `ShaderLoader` picks the shader language from the asset path's extension and panics when
//! there is none, which is every `alias://` path, and it has no setting to name the language
//! instead. [`AliasShaderLoader`] is the same loader with that setting: the reader's synthesized
//! `.meta` names it for any file with an extension in [`SHADER_EXTENSIONS`] and fills in
//! [`AliasShaderSettings::extension`] from the resolved file.
//!
//! Only WGSL and SPIR-V are handled. GLSL would need a direct `naga` dependency pinned to Bevy's,
//! so `.vert`, `.frag` and `.comp` files keep Bevy's loader and do not load by alias.

use bevy::{
    asset::{AssetLoader, LoadContext, io::Reader},
    log::warn,
    reflect::TypePath,
    shader::{Shader, ShaderDefVal, ShaderImport},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// File extensions, without the dot, whose synthesized `.meta` names [`AliasShaderLoader`].
pub const SHADER_EXTENSIONS: &[&str] = &["wgsl", "spv"];

/// Loads a [`Shader`] whose language comes from its settings rather than from the asset path.
///
/// Claims no extensions, so it never competes with Bevy's `ShaderLoader` for a shader loaded by
/// file path. It is only ever chosen by name, from a `.meta`.
#[derive(Default, TypePath)]
pub struct AliasShaderLoader;

/// Settings for [`AliasShaderLoader`].
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct AliasShaderSettings {
    /// The extension of the file the alias resolves to, without the dot. Picks the language.
    pub extension: String,
    /// The `#define`s for this shader. WGSL only, as with Bevy's `ShaderLoader`.
    #[serde(default)]
    pub shader_defs: Vec<ShaderDefVal>,
}

/// An error encountered while loading a shader by alias.
#[derive(Debug, Error)]
pub enum AliasShaderLoaderError {
    #[error("could not read shader: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse shader: {0}")]
    Parse(#[from] std::string::FromUtf8Error),
    #[error("unsupported shader extension {0:?}, expected one of {SHADER_EXTENSIONS:?}")]
    UnsupportedExtension(String),
}

impl AssetLoader for AliasShaderLoader {
    type Asset = Shader;
    type Settings = AliasShaderSettings;
    type Error = AliasShaderLoaderError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        settings: &AliasShaderSettings,
        load_context: &mut LoadContext<'_>,
    ) -> Result<Shader, Self::Error> {
        let extension = settings.extension.as_str();
        if !SHADER_EXTENSIONS.contains(&extension) {
            return Err(AliasShaderLoaderError::UnsupportedExtension(
                extension.to_owned(),
            ));
        }
        // Same normalisation as Bevy's loader, so imports by path compare equal across platforms.
        let path = load_context
            .path()
            .to_string()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        if extension != "wgsl" && !settings.shader_defs.is_empty() {
            warn!("Shader defs are only supported for wgsl; ignoring them for {path}");
        }
        let mut shader = match extension {
            "spv" => Shader::from_spirv(bytes, load_context.path().path().to_string_lossy()),
            _ => Shader::from_wgsl_with_defs(
                String::from_utf8(bytes)?,
                path,
                settings.shader_defs.clone(),
            ),
        };

        // Imports by asset path are loaded here, as Bevy's loader does, so they stay alive.
        for import in &shader.imports {
            if let ShaderImport::AssetPath(asset_path) = import {
                shader.file_dependencies.push(load_context.load(asset_path));
            }
        }
        Ok(shader)
    }

    fn extensions(&self) -> &[&str] {
        &[]
    }
}
