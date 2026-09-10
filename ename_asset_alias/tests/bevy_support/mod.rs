//! The bits every `App`-level test in this crate needs: two trivial asset types, and a way to run
//! frames until a handle settles.
//!
//! Trivial on purpose. `alias_reader.rs` and `alias_scan.rs` are about addressing, not about
//! loading, so neither wants a renderer or a real asset format in the way.

#![allow(dead_code)]

use bevy::{
    app::App,
    asset::{
        Asset, AssetApp, AssetLoader, AssetServer, Assets, Handle, LoadContext, LoadState,
        io::Reader,
    },
    reflect::TypePath,
    tasks::futures_lite::AsyncReadExt,
};
use std::time::Duration;

/// Frames to run before giving up on a load. Generous: the read is async and a loaded CI machine
/// can take a while to get round to it.
pub const MAX_FRAMES: usize = 2_000;

#[derive(Asset, TypePath, Debug)]
pub struct Text(pub String);

#[derive(Default, TypePath)]
pub struct TextLoader;

impl AssetLoader for TextLoader {
    type Asset = Text;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Text, Self::Error> {
        let mut text = String::new();
        reader.read_to_string(&mut text).await?;
        Ok(Text(text.trim().to_owned()))
    }

    fn extensions(&self) -> &[&str] {
        &["txt"]
    }
}

/// A second asset type over a second extension, and the only one that produces a labelled asset.
/// A labelled `AssetPath` is what makes `bevy_asset` skip the by-asset-type loader lookup, so it
/// is the case the synthesized `.meta` exists for.
#[derive(Asset, TypePath, Debug)]
pub struct Shout(pub String);

#[derive(Default, TypePath)]
pub struct ShoutLoader;

impl AssetLoader for ShoutLoader {
    type Asset = Shout;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<Shout, Self::Error> {
        let mut text = String::new();
        reader.read_to_string(&mut text).await?;
        let text = text.trim().to_uppercase();
        load_context.add_labeled_asset("Loud".to_owned(), Shout(text.clone()));
        Ok(Shout(text))
    }

    fn extensions(&self) -> &[&str] {
        &["shout"]
    }
}

/// Registers both asset types and their loaders.
pub fn register_test_assets(app: &mut App) -> &mut App {
    app.init_asset::<Text>()
        .init_asset_loader::<TextLoader>()
        .init_asset::<Shout>()
        .init_asset_loader::<ShoutLoader>()
}

/// Runs frames until `handle` settles, then returns its final state.
///
/// Sleeps a millisecond per frame so a single-core runner does not spin the main thread hard
/// enough to starve the io pool the read is running on.
pub fn run_until_settled<A: Asset>(app: &mut App, handle: &Handle<A>) -> LoadState {
    for _ in 0..MAX_FRAMES {
        app.update();
        let state = app
            .world()
            .resource::<AssetServer>()
            .load_state(handle.id());
        match state {
            LoadState::Loaded | LoadState::Failed(_) => return state,
            _ => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    panic!("handle never settled within {MAX_FRAMES} frames");
}

/// `LoadState` is not `PartialEq`, so the assertion is a `matches!` that still reports what it
/// actually got.
#[track_caller]
pub fn assert_loaded(state: LoadState) {
    assert!(
        matches!(state, LoadState::Loaded),
        "expected the handle to load, got {state:?}"
    );
}

pub fn text(app: &App, handle: &Handle<Text>) -> String {
    app.world()
        .resource::<Assets<Text>>()
        .get(handle)
        .expect("asset is loaded")
        .0
        .clone()
}

pub fn shout(app: &App, handle: &Handle<Shout>) -> String {
    app.world()
        .resource::<Assets<Shout>>()
        .get(handle)
        .expect("asset is loaded")
        .0
        .clone()
}
