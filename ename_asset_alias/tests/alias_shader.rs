// The whole file is Bevy: the `alias://` source only exists under the `bevy` feature.
#![cfg(feature = "bevy")]

//! Shaders by alias. Bevy's `ShaderLoader` reads the shader language off the asset path's extension
//! and panics on an alias, so the reader's synthesized meta must name `AliasShaderLoader` instead.

mod bevy_support;

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{AssetApp, AssetPlugin, AssetServer, Assets, Handle},
    shader::{Shader, ShaderLoader, Source},
};
use bevy_support::{assert_loaded, run_until_settled};
use ename_asset_alias::{AliasSourcePlugin, ContentIndex, ContentIndexCell};

/// Relative to the workspace root, which `BEVY_ASSET_ROOT` pins for `cargo test`.
const FIXTURE_ROOT: &str = "ename_asset_alias/tests/fixtures";

/// Bevy's own `ShaderLoader` is registered too, as it is in a real app, so the test proves the
/// alias loader wins for an alias rather than being the only candidate.
fn shader_app() -> App {
    let mut app = App::new();
    app.add_plugins(AliasSourcePlugin::default().with_asset_root(FIXTURE_ROOT))
        .add_plugins(TaskPoolPlugin::default())
        .add_plugins(AssetPlugin {
            file_path: FIXTURE_ROOT.to_owned(),
            ..Default::default()
        })
        .init_asset::<Shader>()
        .init_asset_loader::<ShaderLoader>();

    let mut index = ContentIndex::default();
    index.insert("core::flat", "files/flat.wgsl").unwrap();
    index.insert("core::lit", "files/lit.wesl").unwrap();
    app.world()
        .resource::<ContentIndexCell>()
        .0
        .set_blocking(index)
        .expect("the index cell was already filled");
    app
}

#[test]
fn a_wgsl_shader_without_a_meta_loads_by_alias() {
    let mut app = shader_app();

    let handle: Handle<Shader> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::flat");

    assert_loaded(run_until_settled(&mut app, &handle));
    let shaders = app.world().resource::<Assets<Shader>>();
    let shader = shaders.get(&handle).expect("loaded");
    assert!(matches!(shader.source, Source::Wgsl(_)));
    assert_eq!(shader.path, "alias://core::flat");
}

#[test]
fn a_wesl_shader_by_alias_loads_its_module_imports() {
    let mut app = shader_app();

    let handle: Handle<Shader> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::lit");

    assert_loaded(run_until_settled(&mut app, &handle));
    let shaders = app.world().resource::<Assets<Shader>>();
    let shader = shaders.get(&handle).expect("loaded");
    assert!(matches!(shader.source, Source::Wesl(_)));
    assert_eq!(shader.path, "alias://core::lit");
    // The imported module is loaded by file path, and kept alive by the importing shader.
    let [dependency] = shader.file_dependencies.as_slice() else {
        panic!(
            "expected one file dependency, got {:?}",
            shader.file_dependencies
        );
    };
    let dependency = dependency.clone();
    assert_eq!(
        dependency.path().map(ToString::to_string).as_deref(),
        Some("files/tint.wesl")
    );
    assert_loaded(run_until_settled(&mut app, &dependency));
}
