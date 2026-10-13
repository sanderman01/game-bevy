//! Sky example: a data asset (`*.sky.ron`) that sets the clear color, applied on load and again
//! on every edit while the game runs.

use bevy::{
    asset::{AssetLoader, LoadContext, io::Reader},
    prelude::*,
};
use serde::Deserialize;
use thiserror::Error;

/// The sky settings the example loads.
const SKY: &str = "alias://examples::backdrop";

/// Loads [`SkySettings`] on startup and applies them to [`ClearColor`] whenever they load or change.
pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<SkySettings>()
            .init_asset_loader::<SkySettingsLoader>()
            .add_systems(Startup, load_sky)
            .add_systems(Update, apply_sky);
    }
}

/// How the sky looks.
#[derive(Asset, TypePath, Deserialize, Debug, Clone, Copy)]
pub struct SkySettings {
    /// sRGB, each channel in `0.0..=1.0`.
    pub clear_color: (f32, f32, f32),
}

#[derive(Resource)]
struct Sky(Handle<SkySettings>);

fn load_sky(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(Sky(asset_server.load(SKY)));
}

fn apply_sky(
    mut events: MessageReader<AssetEvent<SkySettings>>,
    sky: Res<Sky>,
    settings: Res<Assets<SkySettings>>,
    mut clear_color: ResMut<ClearColor>,
) {
    let changed = events.read().any(|event| match event {
        AssetEvent::LoadedWithDependencies { id } | AssetEvent::Modified { id } => {
            *id == sky.0.id()
        }
        _ => false,
    });
    let Some(settings) = settings.get(&sky.0).filter(|_| changed) else {
        return;
    };
    let (r, g, b) = settings.clear_color;
    clear_color.0 = Color::srgb(r, g, b);
    info!("Applied sky settings: {settings:?}");
}

#[derive(Default, TypePath)]
struct SkySettingsLoader;

#[derive(Debug, Error)]
enum SkySettingsError {
    #[error("could not read sky settings: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse sky settings: {0}")]
    Ron(#[from] ron::de::SpannedError),
}

impl AssetLoader for SkySettingsLoader {
    type Asset = SkySettings;
    type Settings = ();
    type Error = SkySettingsError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<SkySettings, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(ron::de::from_bytes(&bytes)?)
    }

    fn extensions(&self) -> &[&str] {
        &["sky.ron"]
    }
}
