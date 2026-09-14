//! The four menu-facing stage operations: unload-then-load semantics for `new_stage`/`open_stage`,
//! keep-others-loaded for `open_stage_additive`, and source-tracking for `save_stage`.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::AssetPlugin,
    prelude::*,
    world_serialization::{DynamicWorldBuilder, WorldSerializationPlugin},
};
use ename_engine::stage::{
    AssetRoot, DynamicWorldFormat, SaveStageError, StageAppExt, StageId, StageMember, StagePlugin,
    new_stage, open_stage, open_stage_additive, save_stage, stage_of,
};
use std::path::PathBuf;
use uuid::Uuid;

fn temp_stage_dir(test_name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ename_engine_stage_workflow_{test_name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp stage dir");
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
        // Overrides StagePlugin's production default, which points at the real `assets/` -- this
        // app's `AssetPlugin` above reads from `asset_root` instead, so saves must too.
        .insert_resource(AssetRoot(asset_root.to_owned()))
        .register_stage_format(DynamicWorldFormat);
    app
}

/// Writes a minimal `.scn.ron` stage file directly, so a test can load it without going through
/// `save_stage` first.
fn write_fixture_stage(dir: &std::path::Path, name: &str, id: StageId) {
    let mut world = World::new();
    let root = world.spawn(id).id();

    let mut type_registry = bevy::reflect::TypeRegistry::default();
    type_registry.register::<StageId>();

    let dynamic_world = DynamicWorldBuilder::from_world(&world, &type_registry)
        .extract_entity(root)
        .build();
    let ron = dynamic_world.serialize(&type_registry).expect("serializes");
    std::fs::write(dir.join(name), ron).expect("writes fixture");
}

/// Runs `app.update()` until `condition` holds or 200 frames pass (loading is async).
fn wait_until(app: &mut App, mut condition: impl FnMut(&mut World) -> bool) {
    for _ in 0..200 {
        if condition(app.world_mut()) {
            return;
        }
        app.update();
    }
    panic!("condition did not become true within 200 frames");
}

#[test]
fn new_stage_unloads_everything_and_spawns_an_empty_tagged_root() {
    let dir = temp_stage_dir("new_stage");
    let mut app = test_app(&dir);
    let old_id = StageId(Uuid::new_v4());
    app.world_mut().spawn((old_id, StageMember(old_id)));

    let root = new_stage(app.world_mut());

    assert!(
        app.world().get::<StageMember>(root).is_some(),
        "the new root must itself be a stage member, so save_stage can find it"
    );
    let mut old = app.world_mut().query::<&StageMember>();
    assert_eq!(
        old.iter(app.world()).filter(|m| m.0 == old_id).count(),
        0,
        "the previously loaded stage's entities must be gone"
    );
}

#[test]
fn open_stage_unloads_every_other_stage_before_loading_the_target() {
    let dir = temp_stage_dir("open_exclusive");
    let old_id = StageId(Uuid::new_v4());
    let new_id = StageId(Uuid::new_v4());
    write_fixture_stage(&dir, "target.scn.ron", new_id);

    let mut app = test_app(&dir);
    app.world_mut().spawn((old_id, StageMember(old_id)));

    open_stage(app.world_mut(), "target.scn.ron");
    wait_until(&mut app, |world| {
        let mut roots = world.query::<&StageId>();
        roots.iter(world).any(|id| *id == new_id)
    });

    let mut members = app.world_mut().query::<&StageMember>();
    assert_eq!(
        members.iter(app.world()).filter(|m| m.0 == old_id).count(),
        0,
        "the previously loaded stage must be unloaded"
    );
}

#[test]
fn open_stage_additive_keeps_the_other_stage_loaded() {
    let dir = temp_stage_dir("open_additive");
    let kept_id = StageId(Uuid::new_v4());
    let new_id = StageId(Uuid::new_v4());
    write_fixture_stage(&dir, "target.scn.ron", new_id);

    let mut app = test_app(&dir);
    app.world_mut().spawn((kept_id, StageMember(kept_id)));

    open_stage_additive(app.world_mut(), "target.scn.ron");
    wait_until(&mut app, |world| {
        let mut roots = world.query::<&StageId>();
        roots.iter(world).any(|id| *id == new_id)
    });

    let mut members = app.world_mut().query::<&StageMember>();
    assert_eq!(
        members.iter(app.world()).filter(|m| m.0 == kept_id).count(),
        1,
        "the already-loaded stage must survive an additive open of a different stage"
    );
}

#[test]
fn open_stage_additive_reloads_a_target_that_is_already_open() {
    let dir = temp_stage_dir("open_additive_reload");
    let id = StageId(Uuid::new_v4());
    write_fixture_stage(&dir, "target.scn.ron", id);

    let mut app = test_app(&dir);
    open_stage_additive(app.world_mut(), "target.scn.ron");
    wait_until(&mut app, |world| {
        let mut roots = world.query::<&StageId>();
        roots.iter(world).any(|found| *found == id)
    });
    let first_root = {
        let mut roots = app.world_mut().query::<(Entity, &StageId)>();
        roots
            .iter(app.world())
            .find(|(_, found)| **found == id)
            .map(|(entity, _)| entity)
            .expect("first load produced a root")
    };

    open_stage_additive(app.world_mut(), "target.scn.ron");
    // Wait for the *replacement* root specifically, not merely for `first_root` to disappear --
    // the despawn above is synchronous, so `first_root` is already gone before the first
    // `app.update()` even runs, well before the second load has finished.
    wait_until(&mut app, |world| {
        let mut roots = world.query::<(Entity, &StageId)>();
        roots
            .iter(world)
            .any(|(entity, found)| *found == id && entity != first_root)
    });

    let mut roots = app.world_mut().query::<&StageId>();
    let matching: Vec<_> = roots
        .iter(app.world())
        .filter(|found| **found == id)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "reloading the same target must not leave two copies of it loaded"
    );
}

#[test]
fn save_stage_writes_back_to_the_path_it_was_opened_from() {
    let dir = temp_stage_dir("save_after_open");
    let id = StageId(Uuid::new_v4());
    write_fixture_stage(&dir, "target.scn.ron", id);

    let mut app = test_app(&dir);
    open_stage(app.world_mut(), "target.scn.ron");
    wait_until(&mut app, |world| {
        let mut roots = world.query::<&StageId>();
        roots.iter(world).any(|found| *found == id)
    });

    // Mutate the loaded stage so the save is provably real, not a no-op against an
    // already-correct file.
    let root = {
        let mut roots = app.world_mut().query::<(Entity, &StageId)>();
        roots
            .iter(app.world())
            .find(|(_, found)| **found == id)
            .map(|(entity, _)| entity)
            .expect("stage loaded")
    };
    app.world_mut().entity_mut(root).insert(Name::new("Edited"));

    save_stage(app.world_mut(), id).expect("save succeeds once a source is recorded");

    let saved = std::fs::read_to_string(dir.join("target.scn.ron")).expect("file was written");
    assert!(
        saved.contains("Edited"),
        "save_stage must write back to the exact path the stage was opened from: {saved}"
    );
}

#[test]
fn save_stage_fails_with_no_source_for_a_brand_new_stage() {
    let dir = temp_stage_dir("save_new");
    let mut app = test_app(&dir);
    let root = new_stage(app.world_mut());
    let id = *app.world().get::<StageId>(root).unwrap();

    let err = save_stage(app.world_mut(), id).expect_err("a never-saved stage has no source");
    assert!(matches!(err, SaveStageError::NoSource));
}

#[test]
fn stage_of_agrees_only_when_every_selected_entity_shares_a_stage() {
    let dir = temp_stage_dir("stage_of");
    let mut app = test_app(&dir);
    let id_a = StageId(Uuid::new_v4());
    let id_b = StageId(Uuid::new_v4());
    let a1 = app.world_mut().spawn(StageMember(id_a)).id();
    let a2 = app.world_mut().spawn(StageMember(id_a)).id();
    let b1 = app.world_mut().spawn(StageMember(id_b)).id();
    let unrelated = app.world_mut().spawn_empty().id();

    assert_eq!(stage_of(app.world(), &[a1, a2]), Some(id_a));
    assert_eq!(stage_of(app.world(), &[a1, b1]), None);
    assert_eq!(stage_of(app.world(), &[unrelated]), None);
    assert_eq!(stage_of(app.world(), &[]), None);
}
