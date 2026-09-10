// The whole file is Bevy: the `alias://` source only exists under the `bevy` feature, and the
// bevy-free build still has to compile its test targets.
#![cfg(feature = "bevy")]

//! Integration tests for the `alias://` asset source, over a real `App`.
//!
//! The index is filled by the test rather than by a scan. `AliasSourcePlugin` is added alone, with
//! no `AliasScanPlugin`, which is exactly how `ename_asset_content` uses it: the reader's
//! behaviour has nothing to do with where an index came from, and testing it this way means a
//! failure in the walk can never be mistaken for a failure in the reader. `alias_scan.rs` covers
//! the other half, where the crate fills its own index.
//!
//! Headless: `TaskPoolPlugin` supplies the pools and `AssetPlugin` the asset system. No window,
//! no renderer.

mod bevy_support;

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{AssetPlugin, AssetServer, Handle, LoadState},
};
use bevy_support::{
    Shout, Text, assert_loaded, register_test_assets, run_until_settled, shout, text,
};
use ename_asset_alias::{AliasSourcePlugin, ContentIndex, ContentIndexCell};
use std::time::Duration;

/// Where the fixtures live, relative to the workspace root. `BEVY_ASSET_ROOT` is pinned to the
/// workspace root in `.cargo/config.toml`, and Cargo applies `[env]` to `cargo test`, so this
/// resolves the same way from any working directory.
const FIXTURE_ROOT: &str = "ename_asset_alias/tests/fixtures";

/// Builds a headless `App` over the fixture tree.
///
/// `AliasSourcePlugin` goes in first on purpose: it registers the `alias://` source into
/// `AssetSourceBuilders`, and `AssetPlugin` is what builds that resource into live sources.
fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(AliasSourcePlugin::default().with_asset_root(FIXTURE_ROOT))
        .add_plugins(TaskPoolPlugin::default())
        .add_plugins(AssetPlugin {
            file_path: FIXTURE_ROOT.to_owned(),
            ..Default::default()
        });
    register_test_assets(&mut app);
    app
}

/// Fills the index the reader is waiting on. Panics if it was already filled.
fn fill_index(app: &App, index: ContentIndex) {
    app.world()
        .resource::<ContentIndexCell>()
        .0
        .set_blocking(index)
        .expect("the index cell was already filled");
}

// --- tests ----------------------------------------------------------------------------------

#[test]
fn resolves_an_alias_to_a_real_file() {
    let mut app = test_app();
    let mut index = ContentIndex::default();
    index.insert("core::thing", "files/plain.txt").unwrap();
    fill_index(&app, index);

    let handle: Handle<Text> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::thing");

    assert_loaded(run_until_settled(&mut app, &handle));
    assert_eq!(text(&app, &handle), "plain");
}

/// The headline property of the phase. The handle is requested before the index exists at all, so
/// nothing could have resolved it yet. The reader awaits the cell inside `bevy_asset`, which is
/// what lets `LoaderState` and the gate in `ename_game` be deleted in Task 7.
#[test]
fn a_handle_requested_before_the_index_exists_still_loads() {
    let mut app = test_app();

    // No `app.update()` has run, and the cell is empty.
    let handle: Handle<Text> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::thing");

    // Let the load start and block on the empty cell.
    for _ in 0..5 {
        app.update();
        std::thread::sleep(Duration::from_millis(1));
    }

    let mut index = ContentIndex::default();
    index.insert("core::thing", "files/plain.txt").unwrap();
    fill_index(&app, index);

    assert_loaded(run_until_settled(&mut app, &handle));
    assert_eq!(text(&app, &handle), "plain");
}

/// A handle is keyed on the alias, never on what it resolved to. Which file answered is invisible
/// to the caller, which is what makes an override safe.
#[test]
fn the_handle_is_keyed_on_the_alias_not_the_resolved_path() {
    let mut app = test_app();
    let mut index = ContentIndex::default();
    index
        .insert("core::thing", "files/replacement.txt")
        .unwrap();
    fill_index(&app, index);

    let handle: Handle<Text> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::thing");

    let path = handle.path().expect("a path-backed handle").clone();
    assert_eq!(path.source().as_str(), Some("alias"));
    assert_eq!(path.path().to_str(), Some("core::thing"));

    assert_loaded(run_until_settled(&mut app, &handle));
    assert_eq!(text(&app, &handle), "replacement");
}

/// An alias carries no file extension, and a labelled `AssetPath` makes `bevy_asset` skip the
/// by-asset-type loader lookup entirely (`AssetLoaders::find`), so without help every
/// `alias://core::thing#Label` load would fail with `MissingAssetLoader`. The reader closes that
/// by answering `read_meta` with the default meta of the loader for the *resolved* file's
/// extension whenever the asset has no `.meta` of its own.
#[test]
fn a_labelled_alias_resolves_a_loader_through_the_synthesized_meta() {
    let mut app = test_app();
    let mut index = ContentIndex::default();
    index.insert("core::noisy", "files/noisy.shout").unwrap();
    fill_index(&app, index);

    let handle: Handle<Shout> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::noisy#Loud");

    assert_loaded(run_until_settled(&mut app, &handle));
    assert_eq!(shout(&app, &handle), "QUIETLY");
}

/// A real `.meta` beside the resolved file wins over the synthesized one, so an author keeps
/// control of loader settings. `meta/plain_with_meta.txt.meta` names `TextLoader` explicitly.
#[test]
fn a_real_meta_beside_the_resolved_file_is_used() {
    let mut app = test_app();
    let mut index = ContentIndex::default();
    index
        .insert("core::metad", "meta/plain_with_meta.txt")
        .unwrap();
    fill_index(&app, index);

    let handle: Handle<Text> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::metad");

    assert_loaded(run_until_settled(&mut app, &handle));
    assert_eq!(text(&app, &handle), "has a meta");
}

#[test]
fn an_unknown_alias_fails_to_load() {
    let mut app = test_app();
    fill_index(&app, ContentIndex::default());

    let handle: Handle<Text> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::nothing_here");

    assert!(matches!(
        run_until_settled(&mut app, &handle),
        LoadState::Failed(_)
    ));
}

/// An alias in the index whose file is missing must fail the load, not the process.
#[test]
fn an_alias_pointing_at_a_missing_file_fails_to_load() {
    let mut app = test_app();
    let mut index = ContentIndex::default();
    index.insert("core::gone", "files/not_here.txt").unwrap();
    fill_index(&app, index);

    let handle: Handle<Text> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::gone");

    assert!(matches!(
        run_until_settled(&mut app, &handle),
        LoadState::Failed(_)
    ));
}
