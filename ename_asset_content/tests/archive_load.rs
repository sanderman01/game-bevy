//! Loading from archived packages end to end: plain paths and aliases, labels, a real `.meta`
//! from inside the archive and a synthesized one, for both `.zip` and `.tar`.
//!
//! Each test copies a committed fixture archive from `ename_asset_vfs` into a temporary asset root
//! as `mods/basic.<ext>`. Loads are issued before the first frame, so they also cover reads that
//! start before the scan has filled the index.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{
        Asset, AssetApp, AssetLoader, AssetPlugin, AssetServer, Assets, Handle, LoadContext,
        LoadState, io::Reader,
    },
    reflect::TypePath,
    tasks::futures_lite::AsyncReadExt,
};
use ename_asset_content::{AssetContentPlugin, ContentReport, ProblemKind};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};

const MAX_FRAMES: usize = 2_000;
const ARCHIVES: &str = "../ename_asset_vfs/tests/fixtures/archives";

#[derive(Asset, TypePath, Debug)]
struct Text(String);

/// Loads a text file, and its upper-cased copy under the label `Upper`.
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
        load_context: &mut LoadContext<'_>,
    ) -> Result<Text, Self::Error> {
        let mut text = String::new();
        reader.read_to_string(&mut text).await?;
        let text = text.trim().to_owned();
        load_context.add_labeled_asset("Upper", Text(text.to_uppercase()));
        Ok(Text(text))
    }

    fn extensions(&self) -> &[&str] {
        &["txt"]
    }
}

#[derive(Default, Serialize, Deserialize)]
struct RonSettings {
    tag: String,
}

/// Prefixes the file with its settings' tag, which only the fixture's `.meta` sets.
#[derive(Default, TypePath)]
struct RonLoader;

impl AssetLoader for RonLoader {
    type Asset = Text;
    type Settings = RonSettings;
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        settings: &RonSettings,
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Text, Self::Error> {
        let mut text = String::new();
        reader.read_to_string(&mut text).await?;
        Ok(Text(format!("{}:{}", settings.tag, text.trim())))
    }

    fn extensions(&self) -> &[&str] {
        &["ron"]
    }
}

fn root_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for (target, archive) in files {
        let target = root.path().join(target);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(Path::new(ARCHIVES).join(archive), target).unwrap();
    }
    root
}

fn app_over(root: &Path) -> App {
    let root = root.to_string_lossy().into_owned();
    let mut app = App::new();
    app.add_plugins(
        AssetContentPlugin::default()
            .with_asset_root(root.clone())
            .with_search_paths(["mods"]),
    )
    .add_plugins(TaskPoolPlugin::default())
    .add_plugins(AssetPlugin {
        file_path: root,
        ..Default::default()
    })
    .init_asset::<Text>()
    .register_asset_loader(TextLoader)
    .register_asset_loader(RonLoader);
    app
}

fn text(app: &mut App, handle: &Handle<Text>) -> String {
    for _ in 0..MAX_FRAMES {
        app.update();
        match app.world().resource::<AssetServer>().load_state(handle) {
            LoadState::Loaded => break,
            LoadState::Failed(err) => panic!("{:?} failed: {err}", handle.path()),
            _ => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    let assets = app.world().resource::<Assets<Text>>();
    assets.get(handle).expect("loaded").0.clone()
}

fn report(app: &mut App) -> ContentReport {
    for _ in 0..MAX_FRAMES {
        app.update();
        if let Some(report) = app.world().get_resource::<ContentReport>() {
            return report.clone();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the scan never finished");
}

#[test]
fn archived_assets_load_by_path_and_by_alias() {
    for archive in ["basic.zip", "basic.tar"] {
        let extension = Path::new(archive).extension().unwrap().to_str().unwrap();
        let root = root_with(&[(&format!("mods/basic.{extension}"), archive)]);
        let mut app = app_over(root.path());
        let server = app.world().resource::<AssetServer>().clone();
        let cases = [
            ("mods/basic/a.txt", "first"),
            ("alias://basic::renamed", "first"),
            // No `.meta` in the archive: the alias source synthesizes one, which is what makes a
            // labelled alias loadable.
            ("alias://basic::renamed#Upper", "FIRST"),
            ("mods/basic/sub/deep/c.txt#Upper", "THIRD"),
            // The archive's own `.meta` names the loader and sets its tag.
            ("mods/basic/b.ron", "from-meta:\"second\""),
            ("alias://basic::b", "from-meta:\"second\""),
        ];
        let handles: Vec<Handle<Text>> = cases.iter().map(|(path, _)| server.load(*path)).collect();
        for ((path, expected), handle) in cases.iter().zip(&handles) {
            let got = text(&mut app, handle);
            assert!(got.starts_with(expected), "{archive} {path}: {got}");
        }
        assert!(report(&mut app).problems.is_empty(), "{archive}");
    }
}

#[test]
fn a_corrupt_archive_is_a_mount_problem_in_the_report() {
    let root = root_with(&[
        ("mods/bad.zip", "bad/truncated.zip"),
        ("mods/basic.zip", "basic.zip"),
    ]);
    let mut app = app_over(root.path());
    let report = report(&mut app);
    let mount: Vec<_> = report
        .problems
        .iter()
        .filter(|problem| problem.kind == ProblemKind::Mount)
        .collect();
    assert_eq!(mount.len(), 1, "{:?}", report.problems);
    assert_eq!(mount[0].path, Path::new("mods/bad"));
    assert!(mount[0].detail.contains("bad.zip"), "{}", mount[0].detail);
    // The package's own failure is reported too, as an unreadable manifest, not a silent miss.
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.kind == ProblemKind::UnparseableManifest)
    );
    assert_eq!(report.packages.len(), 1);
}
