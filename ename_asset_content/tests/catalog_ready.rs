//! The content catalog in an `App`: kinds registered, descriptors loaded, the state reached.
//!
//! The fixture package holds one good descriptor with an unknown field, one that does not parse,
//! and a file of no kind.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{Asset, AssetApp, AssetLoader, AssetPlugin, Assets, LoadContext, io::Reader},
    ecs::world::{FromWorld, World},
    reflect::TypePath,
    state::{app::StatesPlugin, state::State},
};
use ename_asset_content::{
    AssetContentPlugin, ContentAppExt, ContentCatalog, ContentCatalogState, ContentReport,
    ProblemKind, UnknownFieldSink, ron_from_bytes_lenient,
};
use serde::Deserialize;
use std::time::Duration;

/// Relative to the workspace root, which `BEVY_ASSET_ROOT` pins in `.cargo/config.toml`.
const FIXTURE_ROOT: &str = "ename_asset_content/tests/fixtures";

const MAX_FRAMES: usize = 2_000;

#[derive(Asset, TypePath, Debug, Deserialize)]
struct Note {
    name: String,
}

#[derive(TypePath)]
struct NoteLoader {
    unknown_fields: UnknownFieldSink,
}

impl FromWorld for NoteLoader {
    fn from_world(world: &mut World) -> Self {
        Self {
            unknown_fields: world.resource::<UnknownFieldSink>().clone(),
        }
    }
}

impl AssetLoader for NoteLoader {
    type Asset = Note;
    type Settings = ();
    type Error = Box<dyn std::error::Error + Send + Sync>;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<Note, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let (note, unknown) = ron_from_bytes_lenient(&bytes, &ron::Options::default())?;
        self.unknown_fields
            .record(load_context.path().to_string(), unknown);
        Ok(note)
    }

    fn extensions(&self) -> &[&str] {
        &["note.ron"]
    }
}

fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(
        AssetContentPlugin::default()
            .with_asset_root(FIXTURE_ROOT)
            .with_search_paths(["catalog"]),
    )
    .add_plugins((
        TaskPoolPlugin::default(),
        AssetPlugin {
            file_path: FIXTURE_ROOT.to_owned(),
            ..Default::default()
        },
        StatesPlugin,
    ))
    .init_asset::<Note>()
    .init_asset_loader::<NoteLoader>()
    .register_content_kind::<Note>("note", "note.ron");
    // `App::run` does this; a test that drives `update` itself must, or no plugin's `finish`
    // runs and the catalog state is never initialised.
    app.finish();
    app.cleanup();
    app
}

fn run_until_ready(app: &mut App) {
    for _ in 0..MAX_FRAMES {
        app.update();
        let ready = app
            .world()
            .get_resource::<State<ContentCatalogState>>()
            .is_some_and(|state| *state.get() == ContentCatalogState::Ready);
        if ready {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the content catalog was never ready within {MAX_FRAMES} frames");
}

#[test]
fn the_catalog_is_ready_once_every_descriptor_loaded_or_failed() {
    let mut app = test_app();
    app.update();
    assert!(!app.world().contains_resource::<ContentCatalog>());
    run_until_ready(&mut app);

    let catalog = app.world().resource::<ContentCatalog>();
    let ids: Vec<&str> = catalog.entries("note").map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["notes::good"], "the broken one is left out");
    let names: Vec<&str> = app
        .world()
        .resource::<Assets<Note>>()
        .iter()
        .map(|(_, note)| note.name.as_str())
        .collect();
    assert_eq!(names, ["good"], "the descriptor stays loaded");

    let report = app.world().resource::<ContentReport>();
    let unloadable: Vec<_> = report
        .problems
        .iter()
        .filter(|p| p.kind == ProblemKind::UnloadableContent)
        .collect();
    assert_eq!(unloadable.len(), 1, "{:?}", report.problems);
    assert!(unloadable[0].detail.contains("notes::broken"));
}

#[test]
fn unknown_fields_land_in_the_report() {
    let mut app = test_app();
    run_until_ready(&mut app);
    app.update();

    let report = app.world().resource::<ContentReport>();
    assert_eq!(
        report.unknown_fields.len(),
        1,
        "{:?}",
        report.unknown_fields
    );
    assert_eq!(report.unknown_fields[0].asset, "alias://notes::good");
    assert_eq!(report.unknown_fields[0].fields, ["colour"]);
}
