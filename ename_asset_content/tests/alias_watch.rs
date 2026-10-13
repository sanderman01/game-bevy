//! Live reload across packages: only the file that won an alias reloads it.
//!
//! Copies the `base` and `mods` fixtures into a temp dir, where `loud` overrides
//! `core::greeting`, and edits both files while frames run. Needs Bevy's `file_watcher` feature
//! (on under `cargo dt`); without it the test says so and returns.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{
        Asset, AssetApp, AssetLoader, AssetPlugin, AssetServer, Assets, Handle, LoadContext,
        LoadState, io::Reader,
    },
    reflect::TypePath,
    tasks::futures_lite::AsyncReadExt,
};
use ename_asset_content::AssetContentPlugin;
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
const TIMEOUT: Duration = Duration::from_secs(10);
const QUIET_WINDOW: Duration = Duration::from_millis(1500);

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

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn greeting(app: &App, handle: &Handle<Greeting>) -> String {
    app.world()
        .resource::<Assets<Greeting>>()
        .get(handle)
        .expect("loaded")
        .0
        .clone()
}

#[test]
fn only_the_winning_file_reloads_an_overridden_alias() {
    let dir = tempfile::tempdir().unwrap();
    for package_root in ["base", "mods"] {
        copy_tree(
            &Path::new(FIXTURES).join(package_root),
            &dir.path().join(package_root),
        );
    }
    let root = dir.path().to_str().unwrap().to_owned();

    let mut app = App::new();
    app.add_plugins(
        AssetContentPlugin::default()
            .with_asset_root(root.clone())
            .with_search_paths(["base", "mods"]),
    )
    .add_plugins(TaskPoolPlugin::default())
    .add_plugins(AssetPlugin {
        file_path: root,
        watch_for_changes_override: Some(true),
        ..Default::default()
    })
    .init_asset::<Greeting>()
    .init_asset_loader::<GreetingLoader>();

    let server = app.world().resource::<AssetServer>();
    if server
        .get_source(None::<&'static str>)
        .unwrap()
        .event_receiver()
        .is_none()
    {
        eprintln!("skipped: no file watcher (Bevy built without `file_watcher`)");
        return;
    }
    let handle: Handle<Greeting> = server.load("alias://core::greeting");

    let start = Instant::now();
    loop {
        app.update();
        match app
            .world()
            .resource::<AssetServer>()
            .load_state(handle.id())
        {
            LoadState::Loaded => break,
            LoadState::Failed(error) => panic!("load failed: {error}"),
            _ => {}
        }
        assert!(start.elapsed() < TIMEOUT, "never loaded");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(greeting(&app, &handle), "HELLO FROM LOUD");

    // The shadowed file: no reload.
    fs::write(dir.path().join("base/core/greeting.txt"), "edited core").unwrap();
    let start = Instant::now();
    while start.elapsed() < QUIET_WINDOW {
        app.update();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(greeting(&app, &handle), "HELLO FROM LOUD");

    // The winner: reloads.
    fs::write(dir.path().join("mods/loud/greeting.txt"), "EDITED LOUD").unwrap();
    let start = Instant::now();
    while greeting(&app, &handle) != "EDITED LOUD" {
        assert!(
            start.elapsed() < TIMEOUT,
            "the winning file's edit never reloaded"
        );
        app.update();
        std::thread::sleep(Duration::from_millis(5));
    }
}
