//! A stage inside an archive mount cannot be saved: the write would land in a loose directory
//! that shadows the archive.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::AssetPlugin,
    prelude::*,
    world_serialization::WorldSerializationPlugin,
};
use ename_asset_vfs::{ArchiveMounts, Mounts};
use ename_engine::stage::{
    AssetRoot, DynamicWorldFormat, SaveStageError, StageAppExt, StageId, StageMember, StagePlugin,
    write_stage_file,
};
use std::sync::Arc;
use uuid::Uuid;

#[test]
fn saving_into_an_archive_is_refused_before_anything_is_written() {
    let root = tempfile::tempdir().unwrap();
    std::fs::copy(
        "../ename_asset_vfs/tests/fixtures/archives/basic.zip",
        root.path().join("pkg.zip"),
    )
    .unwrap();
    let mut app = App::new();
    app.add_plugins(TaskPoolPlugin::default())
        .add_plugins(AssetPlugin {
            file_path: root.path().to_string_lossy().into_owned(),
            ..Default::default()
        })
        .add_plugins(WorldSerializationPlugin)
        .add_plugins(StagePlugin)
        .insert_resource(AssetRoot(root.path().to_owned()))
        .insert_resource(ArchiveMounts(Arc::new(Mounts::new(root.path()))))
        .register_stage_format(DynamicWorldFormat);
    let id = StageId(Uuid::new_v4());
    app.world_mut().spawn((id, StageMember(id)));

    let inside = root.path().join("pkg/stages/level.scn.ron");
    let result = write_stage_file(&inside.to_string_lossy(), app.world_mut(), id);
    assert!(
        matches!(result, Err(SaveStageError::ReadOnlySource(_))),
        "{result:?}"
    );
    assert!(!root.path().join("pkg").exists());

    let beside = root.path().join("level.scn.ron");
    write_stage_file(&beside.to_string_lossy(), app.world_mut(), id).unwrap();
    assert!(beside.exists());
}
