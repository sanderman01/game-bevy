// The whole file is Bevy: the `alias://` source only exists under the `bevy` feature.
#![cfg(feature = "bevy")]

//! Live reload through `alias://`, against a real filesystem and a real file watcher.
//!
//! Each test builds its own tree in a temp dir and edits it while frames run. Routing itself is
//! covered deterministically by the unit tests in `src/watcher.rs`; these prove the wiring.
//!
//! Needs Bevy's `file_watcher` feature, which the workspace aliases (`cargo dt`) turn on. Without
//! it there is no watcher, and each test says so and returns.

mod bevy_support;

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{AssetEvent, AssetPlugin, AssetServer, Assets, Handle, io::AssetSourceId},
    ecs::{message::MessageReader, resource::Resource, system::ResMut},
    prelude::Update,
    reflect::TypePath,
};
use bevy_support::{
    Shout, Text, TextLoader, assert_loaded, register_test_assets, run_until_settled, text,
};
use ename_asset_alias::{ALIAS_SOURCE, AliasPlugins};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};
use tempfile::TempDir;

/// Long enough for the watcher's 300 ms debounce plus a slow machine.
const RELOAD_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to watch for a reload that must not happen.
const QUIET_WINDOW: Duration = Duration::from_millis(1500);

/// `Modified` events seen for `Text`, so a test can tell a reload from no reload.
#[derive(Resource, Default)]
struct TextModified(usize);

fn count_text_modified(
    mut events: MessageReader<AssetEvent<Text>>,
    mut seen: ResMut<TextModified>,
) {
    seen.0 += events
        .read()
        .filter(|event| matches!(event, AssetEvent::Modified { .. }))
        .count();
}

/// A temp asset root where every `.txt` and `.shout` is `t::{stem}`.
fn tree(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(
        dir.path().join("_alias_rules.toml"),
        "alias = \"t::{stem}\"\ninclude = [\"*.txt\", \"*.shout\"]\n",
    )
    .unwrap();
    for (name, contents) in files {
        fs::write(dir.path().join(name), contents).unwrap();
    }
    dir
}

/// A headless app over `root`, with watching forced to `watch`. `AliasPlugins` goes in before
/// `AssetPlugin`, which builds registered sources once.
fn test_app(root: &Path, watch: bool) -> App {
    let root = root.to_str().expect("utf-8 temp path").to_owned();
    let mut app = App::new();
    app.add_plugins(AliasPlugins::default().with_asset_root(root.clone()))
        .add_plugins(TaskPoolPlugin::default())
        .add_plugins(AssetPlugin {
            file_path: root,
            watch_for_changes_override: Some(watch),
            ..Default::default()
        })
        .init_resource::<TextModified>()
        .add_systems(Update, count_text_modified);
    register_test_assets(&mut app);
    app
}

/// Whether Bevy can watch at all, judged by the default source. Where it can, the alias source
/// must have a watcher too.
fn has_watcher(app: &App) -> bool {
    let server = app.world().resource::<AssetServer>();
    let watching = |id| {
        server
            .get_source(id)
            .expect("source registered")
            .event_receiver()
            .is_some()
    };
    if !watching(AssetSourceId::Default) {
        eprintln!("skipped: no file watcher (Bevy built without `file_watcher`)");
        return false;
    }
    assert!(
        watching(AssetSourceId::from(ALIAS_SOURCE)),
        "the default source watches but the alias source does not"
    );
    true
}

fn load_text(app: &mut App, alias: &'static str) -> Handle<Text> {
    let handle = app.world().resource::<AssetServer>().load(alias);
    assert_loaded(run_until_settled(app, &handle));
    handle
}

/// Runs frames until `done` holds, panicking after [`RELOAD_TIMEOUT`].
fn run_until(app: &mut App, what: &str, mut done: impl FnMut(&App) -> bool) {
    let start = Instant::now();
    while start.elapsed() < RELOAD_TIMEOUT {
        app.update();
        if done(app) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

/// Runs frames for [`QUIET_WINDOW`].
fn run_for_quiet_window(app: &mut App) {
    let start = Instant::now();
    while start.elapsed() < QUIET_WINDOW {
        app.update();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn modified(app: &App) -> usize {
    app.world().resource::<TextModified>().0
}

#[test]
fn editing_an_aliased_file_reloads_it() {
    let dir = tree(&[("a.txt", "one")]);
    let mut app = test_app(dir.path(), true);
    if !has_watcher(&app) {
        return;
    }
    let handle = load_text(&mut app, "alias://t::a");
    assert_eq!(text(&app, &handle), "one");

    fs::write(dir.path().join("a.txt"), "two").unwrap();
    run_until(&mut app, "the edit", |app| text(app, &handle) == "two");
}

/// Editors that write a temp file and rename it over the target produce a rename, not a write.
#[test]
fn an_atomic_save_reloads() {
    let dir = tree(&[("a.txt", "one")]);
    let mut app = test_app(dir.path(), true);
    if !has_watcher(&app) {
        return;
    }
    let handle = load_text(&mut app, "alias://t::a");

    let temp = dir.path().join(".a.txt.swp");
    fs::write(&temp, "two").unwrap();
    fs::rename(&temp, dir.path().join("a.txt")).unwrap();
    run_until(&mut app, "the atomic save", |app| {
        text(app, &handle) == "two"
    });
}

/// A failed reload keeps the last good asset; the next good save replaces it.
#[test]
fn a_broken_edit_keeps_the_old_asset_until_a_good_one() {
    let dir = tree(&[("a.txt", "one")]);
    let mut app = test_app(dir.path(), true);
    if !has_watcher(&app) {
        return;
    }
    let handle = load_text(&mut app, "alias://t::a");

    // Not UTF-8, so `TextLoader` fails.
    fs::write(dir.path().join("a.txt"), [0xff, 0xfe, 0xfd]).unwrap();
    run_for_quiet_window(&mut app);
    assert_eq!(text(&app, &handle), "one");

    fs::write(dir.path().join("a.txt"), "two").unwrap();
    run_until(&mut app, "recovery", |app| text(app, &handle) == "two");
}

/// Deleting keeps the last good asset; recreating the file reloads it.
#[test]
fn delete_then_recreate_reloads() {
    let dir = tree(&[("a.txt", "one")]);
    let mut app = test_app(dir.path(), true);
    if !has_watcher(&app) {
        return;
    }
    let handle = load_text(&mut app, "alias://t::a");

    fs::remove_file(dir.path().join("a.txt")).unwrap();
    run_for_quiet_window(&mut app);
    assert_eq!(text(&app, &handle), "one");

    fs::write(dir.path().join("a.txt"), "two").unwrap();
    run_until(&mut app, "the recreated file", |app| {
        text(app, &handle) == "two"
    });
}

/// Adding a `.meta` reloads with it, and removing it reloads with the synthesized default.
#[test]
fn adding_and_removing_a_meta_reloads() {
    let dir = tree(&[("a.txt", "one")]);
    let mut app = test_app(dir.path(), true);
    if !has_watcher(&app) {
        return;
    }
    let _handle = load_text(&mut app, "alias://t::a");
    let before = modified(&app);

    let meta = dir.path().join("a.txt.meta");
    fs::write(
        &meta,
        format!(
            "(meta_format_version: \"1.0\", asset: Load(loader: \"{}\", settings: ()))",
            TextLoader::type_path()
        ),
    )
    .unwrap();
    run_until(&mut app, "the added meta", |app| modified(app) > before);

    let before = modified(&app);
    fs::remove_file(&meta).unwrap();
    run_until(&mut app, "the removed meta", |app| modified(app) > before);
}

/// A labelled sub-asset reloads even when nothing holds the root asset.
#[test]
fn a_labelled_only_handle_reloads() {
    let dir = tree(&[("s.shout", "one")]);
    let mut app = test_app(dir.path(), true);
    if !has_watcher(&app) {
        return;
    }
    let handle: Handle<Shout> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://t::s#Loud");
    assert_loaded(run_until_settled(&mut app, &handle));

    fs::write(dir.path().join("s.shout"), "two").unwrap();
    run_until(&mut app, "the labelled asset", |app| {
        app.world()
            .resource::<Assets<Shout>>()
            .get(&handle)
            .is_some_and(|shout| shout.0 == "TWO")
    });
}

/// With watching off, an edit changes nothing.
#[test]
fn nothing_reloads_with_watching_off() {
    let dir = tree(&[("a.txt", "one")]);
    let mut app = test_app(dir.path(), false);
    let handle = load_text(&mut app, "alias://t::a");

    fs::write(dir.path().join("a.txt"), "two").unwrap();
    run_for_quiet_window(&mut app);
    assert_eq!(text(&app, &handle), "one");
    assert_eq!(modified(&app), 0);
}
