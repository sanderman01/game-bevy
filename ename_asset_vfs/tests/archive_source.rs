//! Archives under Bevy's `Default` source with no alias crate in the graph: plain-path loads read
//! members, and the source's writer refuses anything inside a mount.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{
        Asset, AssetApp, AssetLoader, AssetPlugin, AssetServer, Assets, Handle, LoadContext,
        LoadState,
        io::{AssetSourceId, Reader},
    },
    reflect::TypePath,
    tasks::{block_on, futures_lite::AsyncReadExt},
};
use ename_asset_vfs::{ArchiveMounts, ArchiveSourcePlugin, Origin};
use std::path::Path;

const MAX_FRAMES: usize = 2_000;

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

/// A headless app whose asset root is `root`, holding `archive` mounted as `pkg`.
fn app_over(root: &Path) -> App {
    let root = root.to_string_lossy().into_owned();
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        ArchiveSourcePlugin::default().with_asset_root(root.clone()),
        AssetPlugin {
            file_path: root,
            ..Default::default()
        },
    ))
    .init_asset::<Text>()
    .register_asset_loader(TextLoader);
    app
}

fn root_with(archive: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let extension = Path::new(archive).extension().unwrap();
    let target = root.path().join("pkg").with_extension(extension);
    std::fs::copy(Path::new("tests/fixtures/archives").join(archive), target).unwrap();
    std::fs::write(root.path().join("loose.txt"), "loose").unwrap();
    root
}

fn text(app: &mut App, handle: &Handle<Text>) -> String {
    for _ in 0..MAX_FRAMES {
        app.update();
        let server = app.world().resource::<AssetServer>();
        match server.load_state(handle) {
            LoadState::Loaded => break,
            LoadState::Failed(err) => panic!("load failed: {err}"),
            _ => std::thread::sleep(std::time::Duration::from_millis(1)),
        }
    }
    let assets = app.world().resource::<Assets<Text>>();
    assets.get(handle).expect("loaded").0.clone()
}

#[test]
fn plain_paths_load_archive_members_and_loose_files() {
    for archive in ["basic.zip", "basic.tar"] {
        let root = root_with(archive);
        let mut app = app_over(root.path());
        let server = app.world().resource::<AssetServer>().clone();
        let a = server.load::<Text>("pkg/a.txt");
        let c = server.load::<Text>("pkg/sub/deep/c.txt");
        let loose = server.load::<Text>("loose.txt");
        assert_eq!(text(&mut app, &a), "first", "{archive}");
        assert!(text(&mut app, &c).starts_with("third"), "{archive}");
        assert_eq!(text(&mut app, &loose), "loose", "{archive}");
    }
}

#[test]
fn the_default_source_lists_a_mount_as_a_directory() {
    let root = root_with("basic.zip");
    let app = app_over(root.path());
    let server = app.world().resource::<AssetServer>();
    let reader = server.get_source(AssetSourceId::Default).unwrap().reader();
    block_on(async {
        use bevy::tasks::futures_lite::StreamExt;
        let top: Vec<_> = reader
            .read_directory(Path::new(""))
            .await
            .unwrap()
            .collect()
            .await;
        assert_eq!(top.len(), 2, "{top:?}");
        assert!(top.contains(&"pkg".into()) && top.contains(&"loose.txt".into()));
        assert!(reader.is_directory(Path::new("pkg")).await.unwrap());
        assert!(reader.is_directory(Path::new("pkg/sub")).await.unwrap());
        assert!(!reader.is_directory(Path::new("pkg/a.txt")).await.unwrap());
        let meta = reader
            .read_meta_bytes(Path::new("pkg/b.ron"))
            .await
            .unwrap();
        assert!(String::from_utf8(meta).unwrap().contains("RonLoader"));
        assert!(matches!(
            reader.read_meta_bytes(Path::new("pkg/a.txt")).await,
            Err(bevy::asset::io::AssetReaderError::NotFound(_))
        ));
    });
}

#[test]
fn the_default_writer_refuses_paths_inside_a_mount() {
    let root = root_with("basic.zip");
    let app = app_over(root.path());
    let server = app.world().resource::<AssetServer>();
    let writer = server
        .get_source(AssetSourceId::Default)
        .unwrap()
        .writer()
        .unwrap();
    block_on(async {
        for path in ["pkg/a.txt", "pkg/new.txt", "pkg"] {
            assert!(
                writer.write_bytes(Path::new(path), b"x").await.is_err(),
                "{path}"
            );
        }
        assert!(
            writer
                .write_meta_bytes(Path::new("pkg/a.txt"), b"x")
                .await
                .is_err()
        );
        assert!(
            writer
                .rename(Path::new("loose.txt"), Path::new("pkg/loose.txt"))
                .await
                .is_err()
        );
        assert!(
            writer
                .rename(Path::new("pkg/a.txt"), Path::new("out.txt"))
                .await
                .is_err()
        );
        writer
            .write_bytes(Path::new("new/loose.txt"), b"fine")
            .await
            .unwrap();
    });
    assert!(!root.path().join("pkg").exists());
    assert_eq!(
        std::fs::read(root.path().join("new/loose.txt")).unwrap(),
        b"fine"
    );
    let mounts = &app.world().resource::<ArchiveMounts>().0;
    assert!(matches!(
        mounts.origin(Path::new("pkg/a.txt")),
        Origin::Member { .. }
    ));
}
