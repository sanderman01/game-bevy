//! Shaders by alias.
//!
//! Bevy's `ShaderLoader` picks the shader language from the asset path's extension and panics when
//! there is none, which is every `alias://` path, and it has no setting to name the language
//! instead. [`AliasShaderLoader`] is the same loader with that setting: the reader's synthesized
//! `.meta` names it for any file with an extension in [`SHADER_EXTENSIONS`] and fills in
//! [`AliasShaderSettings::extension`] from the resolved file.
//!
//! WESL imports are module paths, which resolve against the default asset source, so a shader
//! cannot import another by alias.

use bevy::{
    asset::{AssetLoader, AssetPath, LoadContext, io::Reader},
    log::warn,
    reflect::TypePath,
    shader::{Shader, ShaderDefVal, ShaderImport},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// File extensions, without the dot, whose synthesized `.meta` names [`AliasShaderLoader`].
pub const SHADER_EXTENSIONS: &[&str] = &["wgsl", "wesl", "spv"];

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
    /// The shader defs for this shader. WESL only, as with Bevy's `ShaderLoader`.
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
        if extension != "wesl" && !settings.shader_defs.is_empty() {
            warn!("Shader defs are only supported for wesl; ignoring them for {path}");
        }
        let mut shader = match extension {
            "spv" => Shader::from_spirv(bytes, load_context.path().path().to_string_lossy()),
            "wgsl" => Shader::from_wgsl(String::from_utf8(bytes)?, path),
            _ => {
                let mut shader = Shader::from_wesl(String::from_utf8(bytes)?, path);
                shader.shader_defs = settings.shader_defs.clone();
                shader
            }
        };

        // As Bevy's loader does: a WESL import names a module or an item in one, so only the
        // candidates that exist as files are loaded, and kept alive by the importing shader.
        let candidates: Vec<AssetPath<'static>> = shader
            .imports
            .iter()
            .filter_map(|import| match import {
                ShaderImport::AssetPath(path) => {
                    Some(format!("{}.wesl", path.trim_start_matches('/')).into())
                }
                ShaderImport::Custom(_) => None,
            })
            .collect();
        for candidate in candidates {
            if load_context
                .read_asset_bytes(candidate.clone())
                .await
                .is_ok()
            {
                shader.file_dependencies.push(load_context.load(candidate));
            }
        }
        Ok(shader)
    }

    fn extensions(&self) -> &[&str] {
        &[]
    }
}
