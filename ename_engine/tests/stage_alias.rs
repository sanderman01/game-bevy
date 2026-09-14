//! `save_stage_by_alias` on a brand-new alias must create both the stage file and a matching
//! `.alias` sidecar, so a follow-up scan of the same directory resolves that alias to the file
//! just written.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::AssetPlugin,
    prelude::*,
    world_serialization::WorldSerializationPlugin,
};
use ename_asset_alias::ContentIndex;
use ename_engine::stage::{
    DynamicWorldFormat, StageAppExt, StageId, StageMember, StagePlugin, save_stage_by_alias,
};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Component, Reflect, Clone, PartialEq, Debug, Default)]
#[reflect(Component)]
struct Marker(i32);

fn temp_asset_root(test_name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ename_engine_stage_alias_{test_name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp asset root");
    dir
}

fn test_app(asset_root: &std::path::Path) -> App {
    let mut app = App::new();
    app.add_plugins(TaskPoolPlugin::default())
        .add_plugins(AssetPlugin {
            file_path: asset_root.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .add_plugins(WorldSerializationPlugin)
        .add_plugins(StagePlugin)
        .register_stage_format(DynamicWorldFormat)
        .register_type::<Marker>()
        .insert_resource(ContentIndex::default());
    app
}

#[test]
fn saving_a_brand_new_alias_writes_the_stage_file_and_a_sidecar() {
    let asset_root = temp_asset_root("new_alias");
    let mut app = test_app(&asset_root);
    let id = StageId(Uuid::new_v4());
    app.world_mut().spawn((id, StageMember(id), Marker(1)));

    save_stage_by_alias(app.world_mut(), &asset_root, "core::demo_stage", id)
        .expect("save succeeds for a brand-new alias");

    let stage_path = asset_root.join("basegame/core/demo_stage.scn.ron");
    let sidecar_path = asset_root.join("basegame/core/demo_stage.scn.ron.alias");
    assert!(
        stage_path.exists(),
        "stage file must be written at the conventional path"
    );
    assert!(
        sidecar_path.exists(),
        "a .alias sidecar must be written for a brand-new stage"
    );

    let sidecar: ename_asset_alias::AliasFile =
        toml::from_str(&std::fs::read_to_string(&sidecar_path).unwrap()).expect("valid toml");
    assert_eq!(sidecar.alias.as_deref(), Some("core::demo_stage"));
}
