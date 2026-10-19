//! The `assets` settle probe: meshes, materials and world roots the asset server is still loading.
//!
//! Only handles the asset server tracks count, and only while `LoadState::Loading`: an
//! `Assets::add`ed asset is never loading, and a failed load is not pending work. A crate with
//! its own material type reports it in its own probe. Runs on real frames, so it drains while the
//! world is paused.

use bevy::{
    asset::{LoadState, RecursiveDependencyLoadState, UntypedAssetId, VisitAssetDependencies},
    platform::collections::HashSet,
    prelude::*,
    world_serialization::{DynamicWorldRoot, WorldAssetRoot, WorldInstance, WorldInstanceSpawner},
};

use super::Activity;
use crate::stage::ContentState;

pub(super) fn assets_probe(
    server: Res<AssetServer>,
    meshes: Query<&Mesh3d>,
    materials: Query<&MeshMaterial3d<StandardMaterial>>,
    material_assets: Option<Res<Assets<StandardMaterial>>>,
    world_roots: Query<(
        Option<&WorldAssetRoot>,
        Option<&DynamicWorldRoot>,
        Option<&WorldInstance>,
    )>,
    spawner: Option<Res<WorldInstanceSpawner>>,
    content: Option<ContentState>,
) -> Activity {
    let loading =
        |id: UntypedAssetId| matches!(server.get_load_state(id), Some(LoadState::Loading));
    let meshes_loading = meshes
        .iter()
        .map(|mesh| mesh.0.id())
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|&id| loading(id.untyped()))
        .count();
    let materials_loading = materials
        .iter()
        .map(|material| material.0.id())
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|&id| {
            loading(id.untyped())
                || matches!(
                    server.get_recursive_dependency_load_state(id),
                    Some(RecursiveDependencyLoadState::Loading)
                )
                || material_assets
                    .as_ref()
                    .and_then(|assets| assets.get(id))
                    .is_some_and(|material| {
                        // A material made in code has no load state of its own, so its
                        // textures are checked one by one.
                        let mut any = false;
                        material.visit_dependencies(&mut |dependency| any |= loading(dependency));
                        any
                    })
        })
        .count();
    // `world_instance_spawner` inserts `WorldInstance` when it queues the spawn, not when the
    // instance exists, so a root counts until the spawner says its instance is ready.
    let world_roots_loading = world_roots
        .iter()
        .filter(|&(asset, dynamic, instance)| {
            // The stages' rule: a failed load, or a handle with neither an asset nor a load
            // behind it, never spawns.
            let to_spawn = content
                .as_ref()
                .is_some_and(|content| content.still_to_spawn(dynamic, asset));
            let ready = instance.is_some_and(|instance| {
                spawner
                    .as_ref()
                    .is_some_and(|spawner| spawner.instance_is_ready(**instance))
            });
            (asset.is_some() || dynamic.is_some()) && to_spawn && !ready
        })
        .count();
    Activity::idle()
        .pending("meshes_loading", meshes_loading)
        .pending("materials_loading", materials_loading)
        .pending("world_roots_loading", world_roots_loading)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use bevy::{
        asset::{AssetLoader, LoadContext, RenderAssetUsages, io::Reader},
        mesh::PrimitiveTopology,
        tasks::futures_lite::future::yield_now,
        world_serialization::{WorldAsset, WorldSerializationPlugin},
    };

    use super::*;
    use crate::wait::{SettleSample, SettleWaiter, WaitPlugin};

    /// Loads nothing until the test opens the gate, so "still loading" is deterministic.
    #[derive(TypePath)]
    struct GatedMeshLoader(Arc<AtomicBool>);

    impl AssetLoader for GatedMeshLoader {
        type Asset = Mesh;
        type Settings = ();
        type Error = std::io::Error;

        async fn load(
            &self,
            _: &mut dyn Reader,
            _: &(),
            _: &mut LoadContext<'_>,
        ) -> Result<Mesh, Self::Error> {
            while !self.0.load(Ordering::Acquire) {
                yield_now().await;
            }
            Ok(Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::default(),
            ))
        }

        fn extensions(&self) -> &[&str] {
            &["gatedmesh"]
        }
    }

    #[derive(TypePath)]
    struct GatedWorldLoader(Arc<AtomicBool>);

    impl AssetLoader for GatedWorldLoader {
        type Asset = WorldAsset;
        type Settings = ();
        type Error = std::io::Error;

        async fn load(
            &self,
            _: &mut dyn Reader,
            _: &(),
            _: &mut LoadContext<'_>,
        ) -> Result<WorldAsset, Self::Error> {
            while !self.0.load(Ordering::Acquire) {
                yield_now().await;
            }
            Ok(WorldAsset::new(World::new()))
        }

        fn extensions(&self) -> &[&str] {
            &["gatedworld"]
        }
    }

    #[derive(TypePath)]
    struct GatedImageLoader(Arc<AtomicBool>);

    impl AssetLoader for GatedImageLoader {
        type Asset = Image;
        type Settings = ();
        type Error = std::io::Error;

        async fn load(
            &self,
            _: &mut dyn Reader,
            _: &(),
            _: &mut LoadContext<'_>,
        ) -> Result<Image, Self::Error> {
            while !self.0.load(Ordering::Acquire) {
                yield_now().await;
            }
            Ok(Image::default())
        }

        fn extensions(&self) -> &[&str] {
            &["gatedimage"]
        }
    }

    struct Fixture {
        app: App,
        gate: Arc<AtomicBool>,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        for file in ["a.gatedmesh", "b.gatedworld", "c.gatedimage"] {
            std::fs::write(dir.path().join(file), b"x").unwrap();
        }
        let gate = Arc::new(AtomicBool::new(false));
        let mut app = App::new();
        app.add_plugins((
            TaskPoolPlugin::default(),
            AssetPlugin {
                file_path: dir.path().to_string_lossy().into_owned(),
                ..default()
            },
            WorldSerializationPlugin,
            WaitPlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<StandardMaterial>()
        .register_asset_loader(GatedMeshLoader(gate.clone()))
        .register_asset_loader(GatedWorldLoader(gate.clone()))
        .init_asset::<Image>()
        .register_asset_loader(GatedImageLoader(gate.clone()));
        app.world_mut().spawn(SettleWaiter);
        Fixture {
            app,
            gate,
            _dir: dir,
        }
    }

    fn counter(app: &App, name: &str) -> u32 {
        app.world()
            .resource::<SettleSample>()
            .busy
            .get("assets")
            .and_then(|activity| activity.counters().find(|(n, _)| *n == name))
            .map_or(0, |(_, count)| count)
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

    #[test]
    fn a_mesh_still_loading_counts_and_an_added_one_does_not() {
        let Fixture {
            mut app,
            gate,
            _dir,
        } = fixture();
        let loading: Handle<Mesh> = app.world().resource::<AssetServer>().load("a.gatedmesh");
        let added = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::default(),
            ));
        app.world_mut().spawn(Mesh3d(added));
        app.update();
        assert_eq!(counter(&app, "meshes_loading"), 0);

        // Two entities sharing the handle count once.
        app.world_mut().spawn(Mesh3d(loading.clone()));
        app.world_mut().spawn(Mesh3d(loading));
        app.update();
        assert_eq!(counter(&app, "meshes_loading"), 1);

        gate.store(true, Ordering::Release);
        update_until(&mut app, |app| counter(app, "meshes_loading") == 0);
    }

    #[test]
    fn a_world_root_counts_until_its_instance_is_ready() {
        let Fixture {
            mut app,
            gate,
            _dir,
        } = fixture();
        let handle: Handle<WorldAsset> = app.world().resource::<AssetServer>().load("b.gatedworld");
        let root = app.world_mut().spawn(WorldAssetRoot(handle)).id();
        for _ in 0..3 {
            app.update();
        }
        assert!(app.world().get::<WorldInstance>(root).is_some());
        assert_eq!(counter(&app, "world_roots_loading"), 1);

        gate.store(true, Ordering::Release);
        update_until(&mut app, |app| counter(app, "world_roots_loading") == 0);
    }

    #[test]
    fn a_world_root_with_no_asset_or_load_behind_it_does_not_count() {
        let Fixture { mut app, _dir, .. } = fixture();
        app.world_mut().spawn(DynamicWorldRoot(Handle::default()));
        let both: Handle<WorldAsset> = app.world().resource::<AssetServer>().load("b.gatedworld");
        app.world_mut()
            .spawn((WorldAssetRoot(both), DynamicWorldRoot(Handle::default())));
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(counter(&app, "world_roots_loading"), 0);
    }

    #[test]
    fn a_code_made_material_counts_while_its_texture_loads() {
        let Fixture {
            mut app,
            gate,
            _dir,
        } = fixture();
        let texture: Handle<Image> = app.world().resource::<AssetServer>().load("c.gatedimage");
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color_texture: Some(texture),
                ..default()
            });
        app.world_mut().spawn(MeshMaterial3d(material));
        app.update();
        assert_eq!(counter(&app, "materials_loading"), 1);

        gate.store(true, Ordering::Release);
        update_until(&mut app, |app| counter(app, "materials_loading") == 0);
    }
}
