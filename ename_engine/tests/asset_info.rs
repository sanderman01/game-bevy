//! The `asset.*` commands over a headless app: a RON asset type with one valid and one malformed
//! file, a loader that adds a labeled `Mesh`, and an alias index inserted by hand.

use bevy::{
    asset::{
        AssetLoader, AssetPlugin, AsyncReadExt, LoadContext, LoadState, RenderAssetUsages,
        io::Reader,
    },
    mesh::{Mesh, PrimitiveTopology},
    prelude::*,
};
use ename_asset_alias::ContentIndex;
use ename_engine::{
    asset_info::AssetInfoPlugin,
    command::{Caller, CommandPlugin, CommandScope, CommandWorldExt, Reply},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Asset, TypePath, Deserialize)]
struct Note {
    #[expect(dead_code, reason = "only parsed, to make the RON fail")]
    text: String,
}

#[derive(Debug, thiserror::Error)]
enum NoteError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Ron(#[from] ron::error::SpannedError),
}

/// `.note`: a RON `Note`. `.meshy`: the same, plus a labeled `Mesh0` with vertex colours whose
/// alpha runs 0 to 0.5.
#[derive(TypePath)]
struct NoteLoader;

impl AssetLoader for NoteLoader {
    type Asset = Note;
    type Settings = ();
    type Error = NoteError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<Note, NoteError> {
        let mut text = String::new();
        reader.read_to_string(&mut text).await?;
        let note: Note = ron::from_str(&text)?;
        if load_context.path().path().extension() == Some("meshy".as_ref()) {
            let mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::default(),
            )
            .with_inserted_attribute(
                Mesh::ATTRIBUTE_POSITION,
                vec![[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            )
            .with_inserted_attribute(
                Mesh::ATTRIBUTE_COLOR,
                vec![
                    [1.0_f32, 1.0, 1.0, 0.0],
                    [1.0, 1.0, 1.0, 0.25],
                    [1.0, 1.0, 1.0, 0.5],
                ],
            );
            load_context.add_labeled_asset("Mesh0", mesh);
        }
        Ok(note)
    }

    fn extensions(&self) -> &[&str] {
        &["note", "meshy"]
    }
}

fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin {
            file_path: concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/asset_info").into(),
            ..default()
        },
        CommandPlugin,
        AssetInfoPlugin,
    ))
    .init_asset::<Note>()
    .init_asset::<Mesh>()
    .register_asset_reflect::<Mesh>()
    .register_asset_loader(NoteLoader);
    let mut index = ContentIndex::default();
    for (alias, path) in [
        ("test::good", "good.note"),
        ("test::bad", "bad.note"),
        ("test::shape", "shape.meshy"),
        ("test::unused", "unused.note"),
    ] {
        index.insert(alias, path).unwrap();
    }
    app.insert_resource(index);
    app
}

fn update_until(app: &mut App, mut done: impl FnMut(&App) -> bool) {
    for _ in 0..500 {
        app.update();
        if done(app) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("did not happen within 500 frames");
}

fn settled(app: &App, id: impl Into<bevy::asset::UntypedAssetId>) -> bool {
    matches!(
        app.world().resource::<AssetServer>().get_load_state(id),
        Some(LoadState::Loaded | LoadState::Failed(_))
    )
}

fn call(app: &mut App, name: &str, params: Value) -> Value {
    let caller = Caller {
        scope: CommandScope::Safe,
        cwd: None,
    };
    match app.world_mut().call_command(name, params, caller) {
        Ok(Reply::Done(value)) => value,
        Ok(Reply::Job(_)) => panic!("{name} started a job"),
        Err(err) => panic!("{name} failed: {err}"),
    }
}

fn asset<'a>(listed: &'a Value, alias: &str) -> &'a Value {
    listed["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|asset| asset["alias"] == alias)
        .unwrap_or_else(|| panic!("{alias} not listed in {listed:#}"))
}

#[test]
fn asset_list_reports_loaded_failed_labeled_and_unrequested_assets() {
    let mut app = test_app();
    let server = app.world().resource::<AssetServer>().clone();
    let good: Handle<Note> = server.load("good.note");
    let bad: Handle<Note> = server.load("bad.note");
    // Only the labeled mesh is kept: the root `Note` of `shape.meshy` drops once loaded.
    let mesh: Handle<Mesh> = server.load("shape.meshy#Mesh0");
    // Not indexed, and its handle dropped at once: only the failure log remembers it.
    let _ = server.load::<Note>("missing.note");
    update_until(&mut app, |app| {
        settled(app, &good) && settled(app, &bad) && settled(app, &mesh)
    });
    // Let the failure messages reach the log.
    for _ in 0..10 {
        app.update();
    }

    let listed = call(&mut app, "asset.list", json!({"state": "any"}));
    assert_eq!(
        listed["counts"],
        json!({"aliases": 4, "not_requested": 1, "loading": 0, "loaded": 2, "failed": 1}),
        "{listed:#}"
    );

    let good = asset(&listed, "test::good");
    assert_eq!(good["state"], "loaded");
    assert_eq!(good["path"], "good.note");
    assert_eq!(good["handles"][0]["via"], "path");
    // `Note` is not in the type registry, so its type is left out.
    assert!(good["handles"][0].get("type").is_none(), "{good:#}");

    let bad = asset(&listed, "test::bad");
    assert_eq!(bad["state"], "failed");
    let error = bad["handles"][0]["error"].as_str().unwrap();
    assert!(error.contains("bad.note"), "{error}");

    let shape = asset(&listed, "test::shape");
    assert_eq!(shape["state"], "loaded", "{shape:#}");
    assert_eq!(
        shape["labeled"],
        json!({"count": 1, "loading": 0, "loaded": 1, "failed": 0})
    );

    assert_eq!(asset(&listed, "test::unused")["state"], "not_requested");
    let outside = listed["failures_outside_index"].as_array().unwrap();
    assert_eq!(outside.len(), 1, "{listed:#}");
    assert_eq!(outside[0]["path"], "missing.note");

    // The default lists only requested assets; `contains` and `limit` narrow further.
    let requested = call(&mut app, "asset.list", json!({}));
    assert_eq!(requested["assets"].as_array().unwrap().len(), 3);
    let failed = call(&mut app, "asset.list", json!({"state": "failed"}));
    assert_eq!(failed["assets"][0]["alias"], "test::bad");
    let narrowed = call(&mut app, "asset.list", json!({"contains": "SHAPE"}));
    assert_eq!(narrowed["assets"].as_array().unwrap().len(), 1);
    let limited = call(&mut app, "asset.list", json!({"limit": 1}));
    assert_eq!(limited["assets"].as_array().unwrap().len(), 1);
    assert_eq!(limited["truncated"], true);
}

#[test]
fn a_failed_indexed_asset_whose_handle_dropped_is_still_listed() {
    let mut app = test_app();
    let server = app.world().resource::<AssetServer>().clone();
    let bad: Handle<Note> = server.load("bad.note");
    update_until(&mut app, |app| settled(app, &bad));
    drop(bad);
    for _ in 0..10 {
        app.update();
    }
    let listed = call(&mut app, "asset.list", json!({"state": "failed"}));
    let bad = asset(&listed, "test::bad");
    assert_eq!(bad["handles"][0]["dropped"], true, "{bad:#}");
    assert!(bad["handles"][0]["error"].is_string());
}

#[test]
fn asset_list_refuses_a_limit_over_500() {
    let mut app = test_app();
    app.update();
    let caller = Caller {
        scope: CommandScope::Safe,
        cwd: None,
    };
    let err = app
        .world_mut()
        .call_command("asset.list", json!({"limit": 501}), caller)
        .err()
        .unwrap();
    assert_eq!(err.kind, "invalid_params");
}
