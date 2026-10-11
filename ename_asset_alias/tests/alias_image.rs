// The whole file is Bevy: the `alias://` source only exists under the `bevy` feature.
#![cfg(feature = "bevy")]

//! Images by alias. An alias has no file extension, and Bevy's `ImageLoader` reads the image format
//! from the asset path's extension unless its settings name one, so the reader's synthesized meta
//! must name the format of the file the alias resolves to.

mod bevy_support;

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{AssetApp, AssetPlugin, AssetServer, Assets, Handle},
    image::{CompressedImageFormats, Image, ImageLoader},
    math::UVec2,
};
use bevy_support::{assert_loaded, run_until_settled};
use ename_asset_alias::{AliasSourcePlugin, ContentIndex, ContentIndexCell};

/// Relative to the workspace root, which `BEVY_ASSET_ROOT` pins for `cargo test`.
const FIXTURE_ROOT: &str = "ename_asset_alias/tests/fixtures";

fn image_app() -> App {
    let mut app = App::new();
    app.add_plugins(AliasSourcePlugin::default().with_asset_root(FIXTURE_ROOT))
        .add_plugins(TaskPoolPlugin::default())
        .add_plugins(AssetPlugin {
            file_path: FIXTURE_ROOT.to_owned(),
            ..Default::default()
        })
        .init_asset::<Image>()
        .register_asset_loader(ImageLoader::new(CompressedImageFormats::NONE));

    let mut index = ContentIndex::default();
    index.insert("core::pixel", "files/pixel.png").unwrap();
    app.world()
        .resource::<ContentIndexCell>()
        .0
        .set_blocking(index)
        .expect("the index cell was already filled");
    app
}

#[test]
fn an_image_without_a_meta_loads_by_alias() {
    let mut app = image_app();

    let handle: Handle<Image> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::pixel");

    assert_loaded(run_until_settled(&mut app, &handle));
    let image = app
        .world()
        .resource::<Assets<Image>>()
        .get(&handle)
        .expect("loaded");
    assert_eq!(image.size(), UVec2::ONE);
}
