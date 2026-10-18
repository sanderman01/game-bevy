//! The `asset.*` commands over a headless app: a RON asset type with one valid and one malformed
//! file, a loader that adds a labeled `Mesh`, and an alias index inserted by hand.

use bevy::{
    asset::{
        AssetLoader, AssetPlugin, AsyncReadExt, LoadContext, LoadState, RenderAssetUsages,
        io::Reader,
    },
    mesh::{Mesh, PrimitiveTopology},
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use ename_asset_content::{AssetContentPlugin, ContentIndex};
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
            let image = Image::new_fill(
                Extent3d {
                    width: 2,
                    height: 2,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                &[255, 0, 0, 255],
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::default(),
            );
            load_context.add_labeled_asset("Image0", image);
        }
        Ok(note)
    }

    fn extensions(&self) -> &[&str] {
        &["note", "meshy"]
    }
}

/// Loads successfully and depends on `bad.note`, which does not.
#[derive(Asset, TypePath)]
struct Parent {
    #[dependency]
    child: Handle<Note>,
}

#[derive(TypePath)]
struct ParentLoader;

impl AssetLoader for ParentLoader {
    type Asset = Parent;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        _reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<Parent, std::io::Error> {
        Ok(Parent {
            child: load_context.load("bad.note"),
        })
    }

    fn extensions(&self) -> &[&str] {
        &["parent"]
    }
}

fn test_app() -> App {
    test_app_at(std::path::Path::new(FIXTURES))
}

fn test_app_at(root: &std::path::Path) -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin {
            file_path: root.to_string_lossy().into_owned(),
            ..default()
        },
        CommandPlugin,
        AssetInfoPlugin,
    ))
    .init_asset::<Note>()
    .init_asset::<Parent>()
    .init_asset::<Mesh>()
    .init_asset::<Image>()
    .register_asset_reflect::<Mesh>()
    .register_asset_loader(NoteLoader)
    .register_asset_loader(ParentLoader);
    let mut index = ContentIndex::default();
    for (alias, path) in [
        ("test::good", "good.note"),
        ("test::bad", "bad.note"),
        ("test::shape", "shape.meshy"),
        ("test::unused", "unused.note"),
        ("test::parent", "p.parent"),
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
        json!({"aliases": 5, "not_requested": 2, "loading": 0, "loaded": 2, "failed": 1}),
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
    let outside = &listed["failures_outside_index"];
    assert_eq!(outside["count"], 1, "{listed:#}");
    assert_eq!(outside["items"][0]["path"], "missing.note");

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
fn a_failed_label_is_listed_under_its_alias_kept_or_dropped() {
    let mut app = test_app();
    let server = app.world().resource::<AssetServer>().clone();
    // The usual way a glTF fails: through a label, with no handle on the file itself.
    let missing: Handle<Mesh> = server.load("shape.meshy#Missing");
    update_until(&mut app, |app| settled(app, &missing));
    app.update();
    let failed_label = |app: &mut App| {
        let listed = call(app, "asset.list", json!({"state": "failed"}));
        let shape = asset(&listed, "test::shape").clone();
        let handle = shape["handles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|handle| handle["path"] == "shape.meshy#Missing")
            .cloned()
            .unwrap_or_else(|| panic!("no labeled handle in {shape:#}"));
        assert!(handle["error"].is_string(), "{handle:#}");
        handle
    };
    assert!(failed_label(&mut app).get("dropped").is_none());
    drop(missing);
    for _ in 0..10 {
        app.update();
    }
    assert_eq!(failed_label(&mut app)["dropped"], true);
}

#[test]
fn an_unknown_alias_failure_is_outside_the_index() {
    let mut app = test_app();
    let server = app.world().resource::<AssetServer>().clone();
    // A typo: `alias://` paths of aliases the index does not hold belong to no entry.
    let _ = server.load::<Note>("alias://test::typo");
    let _ = server.load::<Note>("missing.note");
    for _ in 0..20 {
        app.update();
    }
    let listed = call(&mut app, "asset.list", json!({"limit": 1}));
    let outside = &listed["failures_outside_index"];
    assert_eq!(outside["count"], 2, "{listed:#}");
    assert_eq!(outside["truncated"], true);
    let all = call(&mut app, "asset.list", json!({}));
    let paths: Vec<&str> = all["failures_outside_index"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|failure| failure["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"alias://test::typo"), "{paths:?}");
}

#[test]
fn a_recovered_failure_is_history_once_its_handle_drops() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("flip.note"), "(text: ").unwrap();
    let mut app = test_app_at(root.path());
    app.world_mut()
        .resource_mut::<ContentIndex>()
        .insert("test::flip", "flip.note")
        .unwrap();
    let server = app.world().resource::<AssetServer>().clone();
    let flip: Handle<Note> = server.load("flip.note");
    update_until(&mut app, |app| settled(app, &flip));
    std::fs::write(root.path().join("flip.note"), "(text: \"fixed\")").unwrap();
    server.reload("flip.note");
    update_until(&mut app, |app| {
        matches!(
            app.world().resource::<AssetServer>().get_load_state(&flip),
            Some(LoadState::Loaded)
        )
    });
    app.update();
    let listed = call(&mut app, "asset.list", json!({"contains": "flip"}));
    let entry = asset(&listed, "test::flip");
    assert_eq!(entry["state"], "loaded", "{entry:#}");
    assert!(
        entry["handles"][0]["previous_error"].is_string(),
        "{entry:#}"
    );

    drop(flip);
    for _ in 0..10 {
        app.update();
    }
    let listed = call(
        &mut app,
        "asset.list",
        json!({"state": "any", "contains": "flip"}),
    );
    let entry = asset(&listed, "test::flip");
    assert_eq!(entry["state"], "not_requested", "{entry:#}");
    assert_eq!(entry["handles"], json!([]));
}

#[test]
fn a_loaded_asset_with_a_failed_dependency_counts_as_failed() {
    let mut app = test_app();
    let server = app.world().resource::<AssetServer>().clone();
    let parent: Handle<Parent> = server.load("p.parent");
    update_until(&mut app, |app| {
        matches!(
            app.world()
                .resource::<AssetServer>()
                .get_recursive_dependency_load_state(&parent),
            Some(bevy::asset::RecursiveDependencyLoadState::Failed(_))
        )
    });
    let listed = call(&mut app, "asset.list", json!({"state": "failed"}));
    let entry = asset(&listed, "test::parent");
    let handle = &entry["handles"][0];
    assert_eq!(handle["state"], "loaded", "{entry:#}");
    assert_eq!(handle["dependencies"], "failed");
    let error = handle["dependency_error"].as_str().unwrap();
    assert!(error.contains("bad.note"), "{error}");
}

#[test]
fn aliases_sharing_a_file_share_its_labeled_assets() {
    let mut app = test_app();
    app.world_mut()
        .resource_mut::<ContentIndex>()
        .insert("test::shape_again", "shape.meshy")
        .unwrap();
    let server = app.world().resource::<AssetServer>().clone();
    let mesh: Handle<Mesh> = server.load("shape.meshy#Mesh0");
    update_until(&mut app, |app| settled(app, &mesh));
    let listed = call(&mut app, "asset.list", json!({"contains": "shape"}));
    for alias in ["test::shape", "test::shape_again"] {
        assert_eq!(asset(&listed, alias)["labeled"]["loaded"], 1, "{listed:#}");
    }
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

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/asset_info");

/// The real package scan over `content/` (one package) and a search path that does not exist.
fn scanning_app() -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        AssetContentPlugin::default()
            .with_asset_root(FIXTURES)
            .with_search_paths(["content", "missing"])
            .with_load_order_file(format!("{FIXTURES}/no_load_order.toml")),
        AssetPlugin {
            file_path: FIXTURES.into(),
            ..default()
        },
        CommandPlugin,
        AssetInfoPlugin,
    ));
    app
}

#[test]
fn content_report_goes_from_running_to_done() {
    let mut app = scanning_app();
    let first = call(&mut app, "content.report", json!({}));
    assert_eq!(first["scan"], "running", "{first:#}");
    assert_eq!(first["packages"]["count"], 0);

    update_until(&mut app, |app| {
        app.world().contains_resource::<ContentIndex>()
    });
    let report = call(&mut app, "content.report", json!({}));
    assert_eq!(report["scan"], "done", "{report:#}");
    assert_eq!(report["asset_root"], FIXTURES);
    assert_eq!(
        report["search_paths"],
        json!([
            {"path": "content", "exists": true, "packages": 1},
            {"path": "missing", "exists": false, "packages": 0}
        ])
    );
    assert_eq!(report["load_order_file"]["exists"], false);
    assert_eq!(
        report["packages"],
        json!({"count": 1, "truncated": false, "items": [
            {"id": "fixture", "version": "1.2.0", "root": "content/fixture", "aliases": 1}
        ]})
    );
    assert_eq!(report["aliases"], 1);
}

#[test]
fn content_resolve_maps_both_ways_and_names_the_package() {
    let mut app = scanning_app();
    update_until(&mut app, |app| {
        app.world().contains_resource::<ContentIndex>()
    });
    let expected = json!({"entries": [
        {"alias": "fixture::thing", "path": "content/fixture/thing.note", "package": "fixture"}
    ], "truncated": false});
    assert_eq!(
        call(
            &mut app,
            "content.resolve",
            json!({"alias": "fixture::thing"})
        ),
        expected
    );
    assert_eq!(
        call(
            &mut app,
            "content.resolve",
            json!({"path": "content/fixture/thing.note"})
        ),
        expected
    );
    assert_eq!(
        call(&mut app, "content.resolve", json!({"contains": "THING"})),
        expected
    );
    let caller = Caller {
        scope: CommandScope::Safe,
        cwd: None,
    };
    let err = app
        .world_mut()
        .call_command("content.resolve", json!({}), caller)
        .err()
        .unwrap();
    assert_eq!(err.kind, "invalid_params");
}

#[test]
fn content_commands_without_a_scan_say_so() {
    let mut app = test_app();
    app.update();
    let caller = Caller {
        scope: CommandScope::Safe,
        cwd: None,
    };
    let err = app
        .world_mut()
        .call_command("content.report", json!({}), caller)
        .err()
        .unwrap();
    assert_eq!(err.kind, "no_content_scan");
}

#[test]
fn asset_inspect_summarizes_meshes_images_and_errors() {
    let mut app = test_app();
    let server = app.world().resource::<AssetServer>().clone();
    let mesh: Handle<Mesh> = server.load("shape.meshy#Mesh0");
    let image: Handle<Image> = server.load("shape.meshy#Image0");
    let bad: Handle<Note> = server.load("bad.note");
    update_until(&mut app, |app| {
        settled(app, &mesh) && settled(app, &image) && settled(app, &bad)
    });

    // An alias with a label reaches the physical path's handle through the index.
    let inspected = call(
        &mut app,
        "asset.inspect",
        json!({"asset": "test::shape#Mesh0"}),
    );
    let handle = &inspected["handles"][0];
    assert_eq!(handle["path"], "shape.meshy#Mesh0", "{inspected:#}");
    assert_eq!(handle["state"], "loaded");
    let summary = &handle["summary"];
    assert_eq!(summary["vertices"], 3, "{summary:#}");
    assert_eq!(
        summary["aabb"],
        json!({"min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]})
    );
    let color = summary["attributes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|attribute| attribute["name"] == "Vertex_Color")
        .unwrap();
    assert_eq!(color["min"][3], 0.0, "{color:#}");
    assert_eq!(color["max"][3], 0.5);

    let inspected = call(
        &mut app,
        "asset.inspect",
        json!({"asset": "shape.meshy#Image0"}),
    );
    let summary = &inspected["handles"][0]["summary"];
    assert_eq!(summary["format"], "Rgba8UnormSrgb", "{summary:#}");
    assert_eq!(summary["size"], json!([2, 2]));

    let inspected = call(&mut app, "asset.inspect", json!({"asset": "test::bad"}));
    let handle = &inspected["handles"][0];
    assert_eq!(handle["state"], "failed");
    let error = handle["errors"][0].as_str().unwrap();
    assert!(error.contains("Expected comma"), "{handle:#}");

    let caller = Caller {
        scope: CommandScope::Safe,
        cwd: None,
    };
    let err = app
        .world_mut()
        .call_command("asset.inspect", json!({"asset": "test::unused"}), caller)
        .err()
        .unwrap();
    assert_eq!(err.kind, "not_loaded");
}

#[test]
fn asset_inspect_refuses_malformed_paths_and_reads_dropped_failures_from_the_log() {
    let mut app = test_app();
    let server = app.world().resource::<AssetServer>().clone();
    let bad: Handle<Note> = server.load("bad.note");
    update_until(&mut app, |app| settled(app, &bad));
    drop(bad);
    for _ in 0..10 {
        app.update();
    }
    let inspected = call(&mut app, "asset.inspect", json!({"asset": "test::bad"}));
    let handle = &inspected["handles"][0];
    assert_eq!(handle["dropped"], true, "{inspected:#}");
    assert_eq!(handle["state"], "failed");

    for malformed in ["alias://://x", "://x"] {
        let caller = Caller {
            scope: CommandScope::Safe,
            cwd: None,
        };
        let err = app
            .world_mut()
            .call_command("asset.inspect", json!({"asset": malformed}), caller)
            .err()
            .unwrap_or_else(|| panic!("{malformed} was accepted"));
        assert_eq!(err.kind, "invalid_params", "{malformed}: {err}");
    }
}
