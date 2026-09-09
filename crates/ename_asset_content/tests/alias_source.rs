//! End-to-end tests: a real scan of a real package tree, resolved through the real asset source.
//!
//! `ename_asset_alias` already tests the reader against a hand-filled index and
//! `ename_asset_package` already tests discovery on its own. What is only testable here is the two
//! of them wired together in an `App`.
//!
//! Headless: `TaskPoolPlugin` supplies the pools the scan runs on and `AssetPlugin` the asset
//! system. No window, no renderer.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{
        Asset, AssetApp, AssetLoader, AssetPlugin, AssetServer, Assets, Handle, LoadContext,
        LoadState, io::Reader,
    },
    reflect::TypePath,
    tasks::futures_lite::AsyncReadExt,
};
use ename_asset_alias::ContentIndex;
use ename_asset_content::{AssetContentPlugin, ContentReport, ProblemKind};
use std::path::Path;
use std::time::Duration;

/// Relative to the workspace root, which `BEVY_ASSET_ROOT` pins in `.cargo/config.toml`.
const FIXTURE_ROOT: &str = "crates/ename_asset_content/tests/fixtures";

/// Frames to run before giving up on a load. Generous: the scan and the read are both async, and
/// a loaded CI machine can take a while to get round to them.
const MAX_FRAMES: usize = 2_000;

// --- a trivial asset type, so the tests need no renderer ------------------------------------

#[derive(Asset, TypePath, Debug)]
struct Greeting(String);

#[derive(Default, TypePath)]
struct GreetingLoader;

impl AssetLoader for GreetingLoader {
    type Asset = Greeting;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Greeting, Self::Error> {
        let mut text = String::new();
        reader.read_to_string(&mut text).await?;
        Ok(Greeting(text.trim().to_owned()))
    }

    fn extensions(&self) -> &[&str] {
        &["txt"]
    }
}

// --- harness --------------------------------------------------------------------------------

/// Builds a headless `App` over the fixture tree.
///
/// `AssetContentPlugin` goes in first on purpose: it registers the `alias://` source into
/// `AssetSourceBuilders`, and `AssetPlugin` is what builds that resource into live sources. In the
/// real game `EnginePlugins` guarantees this with `add_before::<AssetPlugin>`.
fn test_app(search_paths: &[&str]) -> App {
    let mut app = App::new();
    app.add_plugins(
        AssetContentPlugin::default()
            .with_asset_root(FIXTURE_ROOT)
            .with_search_paths(search_paths.iter().copied()),
    )
    .add_plugins(TaskPoolPlugin::default())
    .add_plugins(AssetPlugin {
        file_path: FIXTURE_ROOT.to_owned(),
        ..Default::default()
    })
    .init_asset::<Greeting>()
    .init_asset_loader::<GreetingLoader>();
    app
}

/// Runs frames until `handle` settles, then returns its final state.
///
/// Sleeps a millisecond per frame so a single-core runner does not spin the main thread hard
/// enough to starve the io pool the scan is running on.
fn run_until_settled(app: &mut App, handle: &Handle<Greeting>) -> LoadState {
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

/// Runs frames until the scan has been mirrored into the `World`.
///
/// Separate from `run_until_settled` because the two land in an unspecified order: a handle can
/// finish loading in the same frame the mirror system first sees the index.
fn run_until_mirrored(app: &mut App) {
    for _ in 0..MAX_FRAMES {
        if app.world().get_resource::<ContentReport>().is_some() {
            return;
        }
        app.update();
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the scan was never mirrored into resources within {MAX_FRAMES} frames");
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

fn greeting_text(app: &App, handle: &Handle<Greeting>) -> String {
    app.world()
        .resource::<Assets<Greeting>>()
        .get(handle)
        .expect("asset is loaded")
        .0
        .clone()
}

// --- tests ----------------------------------------------------------------------------------

/// The headline property of the phase. The handle is requested before a single frame has run, so
/// `start_content_scan` has not even been called yet. The reader awaits the index inside
/// `bevy_asset`, which is what lets `LoaderState` and the gate in `ename_game` be deleted in
/// Task 7.
#[test]
fn a_handle_requested_before_the_first_frame_still_loads() {
    let mut app = test_app(&["base", "mods"]);

    let handle: Handle<Greeting> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::greeting");

    assert_loaded(run_until_settled(&mut app, &handle));
}

/// A mod replacing a base-game asset is the reason the system exists. `mods/loud` claims
/// `core::greeting` in a `.alias` file beside its own `greeting.txt`; there is no list of
/// overrides anywhere. The alias is unchanged and only what it resolves to moves.
#[test]
fn a_later_package_overrides_an_earlier_alias() {
    let mut app = test_app(&["base", "mods"]);
    let handle: Handle<Greeting> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::greeting");

    assert_loaded(run_until_settled(&mut app, &handle));
    assert_eq!(
        greeting_text(&app, &handle),
        "HELLO FROM LOUD",
        "`mods` is passed after `base`, so loud loads later and its replace wins"
    );
}

/// The control for the test above: the same alias with only the base search path. Without this,
/// "loud wins" could just as well mean "loud was the only file there was".
#[test]
fn without_the_overriding_package_the_base_file_wins() {
    let mut app = test_app(&["base"]);
    let handle: Handle<Greeting> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::greeting");

    assert_loaded(run_until_settled(&mut app, &handle));
    assert_eq!(greeting_text(&app, &handle), "hello from core");
}

/// The base game names its assets with one `_rules.toml` line and no per-asset file at all. That
/// is the property that makes the system scale past a few dozen assets.
#[test]
fn a_folder_rule_names_an_asset_with_no_sidecar() {
    let mut app = test_app(&["base"]);
    let handle: Handle<Greeting> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::farewell");

    assert_loaded(run_until_settled(&mut app, &handle));
    assert_eq!(greeting_text(&app, &handle), "goodbye from core");
}

#[test]
fn an_unknown_alias_fails_to_load() {
    let mut app = test_app(&["base", "mods"]);
    let handle: Handle<Greeting> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::nothing_here");

    assert!(matches!(
        run_until_settled(&mut app, &handle),
        LoadState::Failed(_)
    ));
}

/// A missing search path must not take the game down, and must not stop the packages that were
/// found from registering.
#[test]
fn a_missing_search_path_is_survivable() {
    let mut app = test_app(&["base", "does_not_exist", "mods"]);
    let handle: Handle<Greeting> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://core::greeting");

    assert_loaded(run_until_settled(&mut app, &handle));
}

/// The editor and the log need to answer "what content is loaded, and what is wrong with it"
/// without going near the addressing path.
#[test]
fn the_finished_scan_is_mirrored_into_resources() {
    let mut app = test_app(&["base", "mods"]);
    run_until_mirrored(&mut app);

    let report = app.world().resource::<ContentReport>();
    assert_eq!(
        report
            .packages
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["core", "loud"],
        "packages are listed in load order"
    );
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.kind == ProblemKind::OrphanAliasFile),
        "the .alias file naming a missing asset must be reported, got {:?}",
        report.problems
    );

    let index = app.world().resource::<ContentIndex>();
    assert_eq!(
        index.resolve("core::greeting"),
        Some(Path::new("mods/loud/greeting.txt")),
        "the mirrored index shows the winner, not the base game's file"
    );
}
