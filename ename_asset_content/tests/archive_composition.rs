//! A target that turns an archive format on in `ename_asset_vfs` directly and registers
//! `ArchiveSourcePlugin` itself, with no archive feature on this crate: the content report still
//! carries mount problems, and the write guard still refuses writes into a mount.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::AssetPlugin,
};
use ename_asset_content::{AssetContentPlugin, ContentReport, ProblemKind, check_writable};
use ename_asset_vfs::ArchiveSourcePlugin;
use std::{path::Path, time::Duration};

const ARCHIVES: &str = "../ename_asset_vfs/tests/fixtures/archives";

#[test]
fn archives_registered_by_the_target_are_guarded_and_reported() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("mods")).unwrap();
    for (target, archive) in [("good.zip", "basic.zip"), ("bad.zip", "bad/truncated.zip")] {
        std::fs::copy(
            Path::new(ARCHIVES).join(archive),
            root.path().join("mods").join(target),
        )
        .unwrap();
    }
    let root_str = root.path().to_string_lossy().into_owned();
    let mut app = App::new();
    app.add_plugins(ArchiveSourcePlugin::default().with_asset_root(root_str.clone()))
        .add_plugins(
            AssetContentPlugin::default()
                .with_asset_root(root_str.clone())
                .with_search_paths(["mods"]),
        )
        .add_plugins(TaskPoolPlugin::default())
        .add_plugins(AssetPlugin {
            file_path: root_str,
            ..Default::default()
        });

    let report = (0..2_000)
        .find_map(|_| {
            app.update();
            std::thread::sleep(Duration::from_millis(1));
            app.world().get_resource::<ContentReport>().cloned()
        })
        .expect("the scan finished");
    assert_eq!(report.packages.len(), 1, "{report:?}");
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.kind == ProblemKind::Mount),
        "{:?}",
        report.problems
    );

    let world = app.world();
    assert!(check_writable(world, &root.path().join("mods/good/new.scn.ron")).is_err());
    assert!(check_writable(world, &root.path().join("mods/good.zip")).is_err());
    assert!(check_writable(world, &root.path().join("mods/new.scn.ron")).is_ok());
}
