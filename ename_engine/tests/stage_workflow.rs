//! The four menu-facing stage operations: unload-then-load semantics for `new_stage`/`open_stage`,
//! keep-others-loaded for `open_stage_additive`, and source-tracking for `save_stage`.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::AssetPlugin,
    prelude::*,
    world_serialization::{DynamicWorldBuilder, WorldSerializationPlugin},
};
use big_space::plugin::BigSpaceDefaultPlugins;
use big_space::prelude::CellCoord;
use ename_engine::bigspace::{BigSpacePlugin, GridFollowCamera};
use ename_engine::stage::{
    AssetRoot, DynamicWorldFormat, SaveStageError, StageAppExt, StageId, StageMember, StageOpened,
    StagePlugin, StageSource, new_stage, open_stage, open_stage_additive, save_stage, stage_of,
    write_stage_file,
};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// A temporary stage directory, removed when dropped. Its path is canonical, so it matches what
/// a file watcher reports (on macOS the temp dir sits behind the `/var` -> `/private/var` link).
struct StageDir {
    path: PathBuf,
    _dir: tempfile::TempDir,
}

impl AsRef<Path> for StageDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for StageDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

fn temp_stage_dir(test_name: &str) -> StageDir {
    let dir = tempfile::Builder::new()
        .prefix(&format!("ename_engine_stage_workflow_{test_name}_"))
        .tempdir()
        .expect("create temp stage dir");
    StageDir {
        path: dir.path().canonicalize().expect("canonical temp stage dir"),
        _dir: dir,
    }
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
fn new_stage_rescues_a_grid_follow_camera_instead_of_despawning_it_with_the_old_stage() {
    let dir = temp_stage_dir("rescue_grid_follow_camera");
    let mut app = test_app(&dir);
    // `BigSpaceDefaultPlugins`'s camera controller reads `Time` and keyboard input, neither of
    // which `test_app`'s minimal plugin set otherwise provides.
    app.add_plugins(bevy::time::TimePlugin)
        .add_plugins(bevy::input::InputPlugin)
        .add_plugins(BigSpaceDefaultPlugins)
        .add_plugins(BigSpacePlugin);

    // The old stage's own root doubles as a `Grid`, the way the editor's future always-present
    // `GridFollowCamera` attaches to whichever stage happens to be loaded.
    let old_root = new_stage(app.world_mut());
    app.world_mut()
        .entity_mut(old_root)
        .insert(big_space::bundles::BigSpaceRootBundle::default());

    let camera = app
        .world_mut()
        .spawn((GridFollowCamera, Transform::from_xyz(1.0, 2.0, 3.0)))
        .id();
    app.update();
    app.update();
    assert!(
        app.world().entity(camera).contains::<CellCoord>(),
        "camera must be attached to the old stage's grid before the unload this test exercises"
    );

    new_stage(app.world_mut());

    assert!(
        app.world().get_entity(camera).is_ok(),
        "camera should not have been despawned along with the stage it was attached to"
    );
    let entity = app.world().entity(camera);
    assert!(
        !entity.contains::<CellCoord>(),
        "camera must end up detached, not left dangling with a now-despawned parent"
    );
    let transform = entity.get::<Transform>().unwrap();
    assert!(
        transform.translation.distance(Vec3::new(1.0, 2.0, 3.0)) < 0.01,
        "camera must be restored to its true position, got {}",
        transform.translation
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

#[test]
fn open_stage_recovers_from_a_malformed_file_with_no_single_root() {
    let dir = temp_stage_dir("malformed_multi_root");
    let mut world = World::new();
    let a = world.spawn(Name::new("orphan_a")).id();
    let b = world.spawn(Name::new("orphan_b")).id();
    let mut type_registry = bevy::reflect::TypeRegistry::default();
    type_registry.register::<Name>();
    let dynamic_world = DynamicWorldBuilder::from_world(&world, &type_registry)
        .extract_entities([a, b].into_iter())
        .build();
    let ron = dynamic_world.serialize(&type_registry).expect("serializes");
    std::fs::write(dir.join("broken.scn.ron"), ron).expect("writes fixture");

    let mut app = test_app(&dir);
    open_stage(app.world_mut(), "broken.scn.ron");

    // The point of this test is that this loop does not panic.
    for _ in 0..50 {
        app.update();
    }

    let mut roots = app.world_mut().query::<&StageId>();
    assert_eq!(
        roots.iter(app.world()).count(),
        0,
        "a malformed stage file (two top-level entities, no StageId) must not leave any \
         StageId-tagged entity behind"
    );
}

#[test]
#[cfg(unix)]
fn write_stage_file_derives_a_relative_source_through_a_symlinked_asset_root() {
    let real_dir = temp_stage_dir("symlink_real");
    let link_parent = temp_stage_dir("symlink_link");
    let link_dir = link_parent.join("link");
    std::os::unix::fs::symlink(&real_dir, &link_dir).expect("create symlink");

    let mut app = test_app(&real_dir);
    app.world_mut().insert_resource(AssetRoot(link_dir.clone()));

    let id = StageId(Uuid::new_v4());
    app.world_mut().spawn((id, StageMember(id)));

    let save_path = link_dir.join("through_symlink.scn.ron");
    write_stage_file(&save_path.to_string_lossy(), app.world_mut(), id).expect("save succeeds");

    let mut roots = app.world_mut().query::<(&StageId, &StageSource)>();
    let (_, source) = roots
        .iter(app.world())
        .find(|(found, _)| **found == id)
        .expect("stage root exists");
    assert_eq!(
        source.0, "through_symlink.scn.ron",
        "StageSource must be asset-relative even when the save path was reached through a \
         symlinked asset root, got {:?}",
        source.0
    );

    let _ = std::fs::remove_file(&link_dir);
}

/// Every [`StageOpened`] seen, as `(id, replaced_others)`.
#[derive(Resource, Default)]
struct Opened(Vec<(StageId, bool)>);

fn record_opened(opened: On<StageOpened>, mut seen: ResMut<Opened>) {
    seen.0.push((opened.id, opened.replaced_others));
}

fn opened(app: &App) -> Vec<(StageId, bool)> {
    app.world().resource::<Opened>().0.clone()
}

#[test]
fn stage_opened_tells_an_exclusive_open_from_an_additive_one_and_skips_hot_reloads() {
    let dir = temp_stage_dir("stage_opened");
    let first = StageId(Uuid::new_v4());
    let second = StageId(Uuid::new_v4());
    write_fixture_stage(&dir, "first.scn.ron", first);
    write_fixture_stage(&dir, "second.scn.ron", second);
    let mut app = test_app(&dir);
    app.init_resource::<Opened>().add_observer(record_opened);

    let container = open_stage(app.world_mut(), "first.scn.ron");
    wait_until(&mut app, |world| !world.resource::<Opened>().0.is_empty());
    assert_eq!(opened(&app), [(first, true)]);

    // What a file watcher does on an edit: the asset changes and Bevy respawns the instance.
    for _ in 0..4 {
        app.update();
    }
    let root_of = |app: &mut App, id: StageId| {
        let mut roots = app.world_mut().query::<(Entity, &StageId)>();
        roots
            .iter(app.world())
            .find(|(_, found)| **found == id)
            .map(|(entity, _)| entity)
    };
    let before = root_of(&mut app, first);
    let handle = app
        .world()
        .get::<bevy::world_serialization::DynamicWorldRoot>(container)
        .unwrap()
        .0
        .clone();
    app.world_mut()
        .resource_mut::<Assets<bevy::world_serialization::DynamicWorld>>()
        .get_mut(&handle)
        .unwrap()
        .into_inner();
    for _ in 0..10 {
        app.update();
    }
    let after = root_of(&mut app, first);
    assert!(
        after.is_some() && after != before,
        "the stage was not respawned"
    );
    assert_eq!(opened(&app), [(first, true)], "a hot reload is not an open");

    open_stage_additive(app.world_mut(), "second.scn.ron");
    wait_until(&mut app, |world| world.resource::<Opened>().0.len() == 2);
    assert_eq!(opened(&app)[1], (second, false));

    let fresh = new_stage(app.world_mut());
    let fresh_id = *app.world().get::<StageId>(fresh).unwrap();
    assert_eq!(opened(&app)[2], (fresh_id, true));
}

/// Writes a stage whose root holds a child named `label`, and, with `nested`, a child loading
/// that stage file as nested content.
fn write_labelled_stage(
    app: &App,
    dir: &std::path::Path,
    name: &str,
    id: StageId,
    label: &str,
    nested: Option<&str>,
) {
    let mut world = World::new();
    let root = world.spawn(id).id();
    world.spawn((Name::new(label.to_owned()), ChildOf(root)));
    if let Some(nested) = nested {
        let handle = app
            .world()
            .resource::<AssetServer>()
            .load::<bevy::world_serialization::DynamicWorld>(nested.to_owned());
        world.spawn((
            bevy::world_serialization::DynamicWorldRoot(handle),
            ChildOf(root),
        ));
    }
    let registry = app.world().resource::<AppTypeRegistry>().read();
    let dynamic_world = DynamicWorldBuilder::from_world(&world, &registry)
        .extract_entities(world.iter_entities().map(|entity| entity.id()))
        .build();
    std::fs::write(dir.join(name), dynamic_world.serialize(&registry).unwrap()).unwrap();
}

/// The names in the world when each [`StageOpened`] was triggered.
#[derive(Resource, Default)]
struct NamesAtOpen(Vec<Vec<String>>);

fn record_names_at_open(_: On<StageOpened>, names: Query<&Name>, mut seen: ResMut<NamesAtOpen>) {
    let mut names: Vec<String> = names.iter().map(|name| name.as_str().to_owned()).collect();
    names.sort();
    seen.0.push(names);
}

/// Updates, sleeping a little between frames so a file watcher can deliver, until `condition`
/// holds or ten seconds pass.
fn wait_with_watcher(app: &mut App, mut condition: impl FnMut(&mut World) -> bool) {
    let start = std::time::Instant::now();
    while !condition(app.world_mut()) {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "condition did not become true within ten seconds"
        );
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn names(world: &mut World) -> Vec<String> {
    let mut names: Vec<String> = world
        .query::<&Name>()
        .iter(world)
        .map(|name| name.as_str().to_owned())
        .collect();
    names.sort();
    names
}

/// The open does not reload a file the asset server holds: with watching on, as in the editor,
/// that copy already follows the file, so a reopen after an edit announces the edit.
#[test]
fn reopening_an_edited_file_with_watching_on_announces_the_edit() {
    let dir = temp_stage_dir("reopen_watched");
    let mut app = App::new();
    app.add_plugins(TaskPoolPlugin::default())
        .add_plugins(AssetPlugin {
            file_path: dir.to_string_lossy().into_owned(),
            watch_for_changes_override: Some(true),
            ..Default::default()
        })
        .add_plugins(WorldSerializationPlugin)
        .add_plugins(StagePlugin)
        .insert_resource(AssetRoot(dir.to_path_buf()))
        .init_resource::<NamesAtOpen>()
        .add_observer(record_names_at_open);
    let watching = app
        .world()
        .resource::<AssetServer>()
        .get_source(None::<&'static str>)
        .unwrap()
        .event_receiver()
        .is_some();
    if !watching {
        eprintln!("skipped: no file watcher (Bevy built without `file_watcher`)");
        return;
    }
    let id = StageId(Uuid::new_v4());
    write_labelled_stage(&app, &dir, "changing.scn.ron", id, "before", None);
    open_stage(app.world_mut(), "changing.scn.ron");
    wait_with_watcher(&mut app, |world| {
        world.resource::<NamesAtOpen>().0.len() == 1
    });

    write_labelled_stage(&app, &dir, "changing.scn.ron", id, "after", None);
    // The watcher's hot reload, which is no open.
    wait_with_watcher(&mut app, |world| names(world) == ["after", "changing"]);
    assert_eq!(app.world().resource::<NamesAtOpen>().0.len(), 1);

    open_stage(app.world_mut(), "changing.scn.ron");
    wait_with_watcher(&mut app, |world| {
        world.resource::<NamesAtOpen>().0.len() == 2
    });
    for _ in 0..10 {
        app.update();
    }

    let seen = &app.world().resource::<NamesAtOpen>().0;
    assert_eq!(seen.len(), 2, "one announcement per open");
    assert_eq!(seen[1], ["after", "changing"]);
}

#[test]
fn a_stage_is_announced_once_its_nested_content_has_spawned() {
    let dir = temp_stage_dir("nested_content");
    let mut app = test_app(&dir);
    app.init_resource::<NamesAtOpen>()
        .add_observer(record_names_at_open);
    write_labelled_stage(
        &app,
        &dir,
        "inner.scn.ron",
        StageId(Uuid::new_v4()),
        "inner",
        None,
    );
    write_labelled_stage(
        &app,
        &dir,
        "outer.scn.ron",
        StageId(Uuid::new_v4()),
        "outer",
        Some("inner.scn.ron"),
    );

    open_stage(app.world_mut(), "outer.scn.ron");
    wait_until(&mut app, |world| {
        world.resource::<NamesAtOpen>().0.len() == 1
    });

    let seen = &app.world().resource::<NamesAtOpen>().0;
    assert!(
        seen[0].iter().any(|name| name == "inner"),
        "announced before the nested content spawned: {:?}",
        seen[0]
    );
}

#[test]
fn a_nested_handle_with_no_load_behind_it_does_not_hold_up_the_open() {
    let dir = temp_stage_dir("placeholder_nested");
    let mut app = test_app(&dir);
    app.init_resource::<NamesAtOpen>()
        .add_observer(record_names_at_open);
    let mut world = World::new();
    let root = world.spawn(StageId(Uuid::new_v4())).id();
    world.spawn((
        bevy::world_serialization::DynamicWorldRoot(Handle::default()),
        ChildOf(root),
    ));
    let registry = app.world().resource::<AppTypeRegistry>().read();
    let dynamic_world = DynamicWorldBuilder::from_world(&world, &registry)
        .extract_entities(world.iter_entities().map(|entity| entity.id()))
        .build();
    std::fs::write(
        dir.join("placeholder.scn.ron"),
        dynamic_world.serialize(&registry).unwrap(),
    )
    .unwrap();
    drop(registry);

    open_stage(app.world_mut(), "placeholder.scn.ron");
    wait_until(&mut app, |world| {
        world.resource::<NamesAtOpen>().0.len() == 1
    });
}

#[test]
fn an_empty_stage_file_is_discarded_instead_of_staying_loading() {
    let dir = temp_stage_dir("empty_file");
    let mut app = test_app(&dir);
    let empty = bevy::world_serialization::DynamicWorld::default();
    let ron = {
        let registry = app.world().resource::<AppTypeRegistry>().read();
        empty.serialize(&registry).unwrap()
    };
    std::fs::write(dir.join("empty.scn.ron"), ron).unwrap();

    let container = open_stage(app.world_mut(), "empty.scn.ron");
    wait_until(&mut app, |world| world.get_entity(container).is_err());

    let mut loading = app
        .world_mut()
        .query_filtered::<(), With<ename_engine::stage::StageLoading>>();
    assert_eq!(loading.iter(app.world()).count(), 0);
}
