//! Integration tests for the `alias://` asset source, over a real `App`.
//!
//! The index is filled by the test rather than by a scan, which is the point of the crate split:
//! the reader's behaviour has nothing to do with manifests, and testing it here means a failure
//! in `ename_asset_content` can never be mistaken for a failure in the reader.
//!
//! Headless: `TaskPoolPlugin` supplies the pools and `AssetPlugin` the asset system. No window,
//! no renderer.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{
        Asset, AssetApp, AssetLoader, AssetPlugin, AssetServer, Assets, Handle, LoadContext,
        LoadState, io::Reader,
    },
    reflect::TypePath,
    tasks::futures_lite::AsyncReadExt,
};
use ename_asset_alias::{AliasSourcePlugin, ContentIndex, ContentIndexCell};
use std::time::Duration;

/// Where the fixtures live, relative to the workspace root. `BEVY_ASSET_ROOT` is pinned to the
/// workspace root in `.cargo/config.toml`, and Cargo applies `[env]` to `cargo test`, so this
/// resolves the same way from any working directory.
const FIXTURE_ROOT: &str = "crates/ename_asset_alias/tests/fixtures";

/// Frames to run before giving up on a load. Generous: the read is async and a loaded CI machine
/// can take a while to get round to it.
const MAX_FRAMES: usize = 2_000;

// --- a trivial asset type, so the tests need no renderer ------------------------------------

#[derive(Asset, TypePath, Debug)]
struct Text(String);

#[derive(Default, TypePath)]
struct TextLoader;

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

/// A second asset type over the same extension, so a labelled load has an ambiguous asset type
/// and can only be resolved through the meta. See
/// `a_labelled_alias_resolves_a_loader_through_the_synthesized_meta`.
#[derive(Asset, TypePath, Debug)]
struct Shout(String);

#[derive(Default, TypePath)]
struct ShoutLoader;

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

// --- harness --------------------------------------------------------------------------------

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
        })
        .init_asset::<Text>()
        .init_asset_loader::<TextLoader>()
        .init_asset::<Shout>()
        .init_asset_loader::<ShoutLoader>();
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

/// Runs frames until `handle` settles, then returns its final state.
///
/// Sleeps a millisecond per frame so a single-core runner does not spin the main thread hard
/// enough to starve the io pool the read is running on.
fn run_until_settled<A: Asset>(app: &mut App, handle: &Handle<A>) -> LoadState {
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
fn assert_loaded(state: LoadState) {
    assert!(
        matches!(state, LoadState::Loaded),
        "expected the handle to load, got {state:?}"
    );
}

fn text(app: &App, handle: &Handle<Text>) -> String {
    app.world()
        .resource::<Assets<Text>>()
        .get(handle)
        .expect("asset is loaded")
        .0
        .clone()
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
    assert_eq!(
        app.world()
            .resource::<Assets<Shout>>()
            .get(&handle)
            .expect("asset is loaded")
            .0,
        "QUIETLY"
    );
}

/// A real `.meta` beside the resolved file wins over the synthesized one, so an author keeps
/// control of loader settings. `plain_with_meta.txt.meta` names `TextLoader` explicitly.
#[test]
fn a_real_meta_beside_the_resolved_file_is_used() {
    let mut app = test_app();
    let mut index = ContentIndex::default();
    index
        .insert("core::metad", "files/plain_with_meta.txt")
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
