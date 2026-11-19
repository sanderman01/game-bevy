//! The content catalog in an `App`: kinds registered, descriptors loaded, the state reached.
//!
//! The fixture package holds a good note with an unknown field, a note that does not parse, a memo
//! (a kind with a loader of its own) with an unknown field, and a file of no kind.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{
        Asset, AssetApp, AssetLoader, AssetPlugin, AssetServer, Assets, LoadContext, io::Reader,
    },
    ecs::world::{FromWorld, World},
    reflect::TypePath,
    state::{app::StatesPlugin, state::State},
};
use ename_asset_content::{
    AssetContentPlugin, ContentAppExt, ContentCatalog, ContentCatalogState, ContentKinds,
    ContentReport, ProblemKind, RonContentError, UnknownFieldSink, read_ron_lenient,
};
use serde::Deserialize;
use std::time::Duration;

/// Relative to the workspace root, which `BEVY_ASSET_ROOT` pins in `.cargo/config.toml`.
const FIXTURE_ROOT: &str = "ename_asset_content/tests/fixtures";

const MAX_FRAMES: usize = 2_000;

/// Read by `RonContentLoader`.
#[derive(Asset, TypePath, Debug, Deserialize)]
struct Note {
    name: String,
}

/// Read by a loader of its own, through `read_ron_lenient`.
#[derive(Asset, TypePath, Debug, Deserialize)]
struct Memo {
    text: String,
}

#[derive(TypePath)]
struct MemoLoader {
    unknown_fields: UnknownFieldSink,
}

impl FromWorld for MemoLoader {
    fn from_world(world: &mut World) -> Self {
        Self {
            unknown_fields: world.resource::<UnknownFieldSink>().clone(),
        }
    }
}

impl AssetLoader for MemoLoader {
    type Asset = Memo;
    type Settings = ();
    type Error = RonContentError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<Memo, RonContentError> {
        read_ron_lenient(
            reader,
            load_context,
            &self.unknown_fields,
            &ron::Options::default(),
        )
        .await
    }

    fn extensions(&self) -> &[&str] {
        &["memo.ron"]
    }
}

/// The plugins every test app has, over `asset_root` with the package search path `catalog`.
fn base_app(asset_root: &str) -> App {
    let mut app = App::new();
    app.add_plugins(
        AssetContentPlugin::default()
            .with_asset_root(asset_root)
            .with_search_paths(["catalog"]),
    )
    .add_plugins((
        TaskPoolPlugin::default(),
        AssetPlugin {
            file_path: asset_root.to_owned(),
            watch_for_changes_override: Some(false),
            ..Default::default()
        },
    ));
    app
}

/// `App::run` calls `finish` and `cleanup`; a test that drives `update` itself must, or no
/// plugin's `finish` runs and the catalog state is never initialised.
fn finish(mut app: App) -> App {
    app.finish();
    app.cleanup();
    app
}

/// Notes through the RON content loader and memos through a loader of their own. `states` adds
/// `StatesPlugin`.
fn app_over(asset_root: &str, states: bool) -> App {
    let mut app = base_app(asset_root);
    if states {
        app.add_plugins(StatesPlugin);
    }
    app.init_asset::<Note>()
        .init_asset::<Memo>()
        .init_asset_loader::<MemoLoader>()
        .register_ron_content_kind::<Note>("note", "note.ron")
        .register_content_kind::<Memo>("memo", "memo.ron");
    finish(app)
}

fn run_until(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    for _ in 0..MAX_FRAMES {
        app.update();
        if done(app) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("{what} did not happen within {MAX_FRAMES} frames");
}

fn ready(app: &App) -> bool {
    app.world()
        .get_resource::<State<ContentCatalogState>>()
        .is_some_and(|state| *state.get() == ContentCatalogState::Ready)
}

fn unloadable(app: &App) -> Vec<String> {
    app.world()
        .resource::<ContentReport>()
        .problems
        .iter()
        .filter(|p| p.kind == ProblemKind::UnloadableContent)
        .map(|p| p.detail.clone())
        .collect()
}

fn unknown_fields(app: &App) -> Vec<(String, Vec<String>)> {
    app.world()
        .resource::<ContentReport>()
        .unknown_fields()
        .iter()
        .map(|entry| (entry.asset.clone(), entry.fields.clone()))
        .collect()
}

#[test]
fn the_catalog_is_ready_once_every_descriptor_loaded_or_failed() {
    let mut app = app_over(FIXTURE_ROOT, true);
    app.update();
    assert!(!app.world().contains_resource::<ContentCatalog>());
    run_until(&mut app, "the ready state", ready);

    let catalog = app.world().resource::<ContentCatalog>();
    let notes: Vec<&str> = catalog.entries("note").map(|e| e.id.as_str()).collect();
    assert_eq!(notes, ["notes::good"], "the broken one is left out");
    let memos: Vec<&str> = catalog.entries("memo").map(|e| e.id.as_str()).collect();
    assert_eq!(memos, ["notes::hello"]);

    let names: Vec<&str> = app
        .world()
        .resource::<Assets<Note>>()
        .iter()
        .map(|(_, note)| note.name.as_str())
        .collect();
    assert_eq!(names, ["good"], "the descriptor stays loaded");
    let texts: Vec<&str> = app
        .world()
        .resource::<Assets<Memo>>()
        .iter()
        .map(|(_, memo)| memo.text.as_str())
        .collect();
    assert_eq!(texts, ["hi"]);

    let failed = unloadable(&app);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].contains("notes::broken"));
}

/// The RON content loader and a loader of its own both report what they skipped.
#[test]
fn unknown_fields_land_in_the_report() {
    let mut app = app_over(FIXTURE_ROOT, true);
    run_until(&mut app, "the ready state", ready);
    app.update();

    assert_eq!(
        unknown_fields(&app),
        [
            ("alias://notes::good".to_owned(), vec!["colour".to_owned()]),
            ("alias://notes::hello".to_owned(), vec!["mood".to_owned()]),
        ]
    );
}

/// The drift test a game writes: its registered kinds equal the ones its `Cargo.toml` declares for
/// `ename_check`, whatever the order of either.
#[test]
fn registered_kinds_compare_with_the_declared_table() {
    let app = app_over(FIXTURE_ROOT, false);
    let manifest: toml::Table =
        "[workspace.metadata.ename.content_kinds]\nmemo = \"memo.ron\"\nnote = \"note.ron\""
            .parse()
            .unwrap();
    assert_eq!(
        app.registered_content_kinds(),
        ContentKinds::from_workspace_manifest(&manifest).unwrap()
    );
}

/// Without `StatesPlugin` there is no state, and the resource is the signal.
#[test]
fn without_states_the_catalog_resource_still_appears() {
    let mut app = app_over(FIXTURE_ROOT, false);
    run_until(&mut app, "the catalog resource", |app| {
        app.world().contains_resource::<ContentCatalog>()
    });
    assert!(
        app.world()
            .get_resource::<State<ContentCatalogState>>()
            .is_none()
    );
}

/// A kind no package has a file of is ready at once, with no entries.
#[test]
fn a_kind_with_no_descriptors_is_ready() {
    let mut app = base_app(FIXTURE_ROOT);
    app.add_plugins(StatesPlugin)
        .init_asset::<Note>()
        .register_ron_content_kind::<Note>("level", "level.ron");
    let mut app = finish(app);
    run_until(&mut app, "the ready state", ready);
    assert_eq!(
        app.world()
            .resource::<ContentCatalog>()
            .entries("level")
            .count(),
        0
    );
}

/// After `Ready`, a descriptor that fails to reload is reported and its unknown fields are cleared;
/// a fixed file clears the report line again. The catalog does not change.
#[test]
fn a_reload_failure_after_ready_is_reported_and_cleared() {
    let root = tempfile::tempdir().unwrap();
    let write = |path: &str, text: &str| {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "catalog/notes/package-info.toml",
        "[package]\nid = \"notes\"\nversion = \"1.0.0\"\nauthors = []\ntitle = \"\"\ndescription = \"\"\n",
    );
    write("catalog/notes/good.note.ron", "(name: \"good\", colour: 1)");
    let asset_root = root.path().to_str().unwrap().to_owned();
    let mut app = app_over(&asset_root, true);
    run_until(&mut app, "the ready state", ready);
    run_until(&mut app, "the unknown field", |app| {
        !unknown_fields(app).is_empty()
    });
    assert!(unloadable(&app).is_empty());

    let server = app.world().resource::<AssetServer>().clone();
    write("catalog/notes/good.note.ron", "(name: ");
    server.reload("alias://notes::good");
    run_until(&mut app, "the reload failure", |app| {
        !unloadable(app).is_empty()
    });
    assert!(unknown_fields(&app).is_empty(), "a failed read clears it");
    assert!(
        app.world()
            .resource::<ContentCatalog>()
            .get("note", "notes::good")
            .is_some()
    );

    write("catalog/notes/good.note.ron", "(name: \"good\")");
    server.reload("alias://notes::good");
    run_until(&mut app, "the report line to clear", |app| {
        unloadable(app).is_empty()
    });
}
