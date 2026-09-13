//! The format registry is a closed, extension-keyed lookup -- these tests exercise it with a
//! fake format so they don't need a real asset pipeline.

use bevy::prelude::*;
use ename_engine::scene::{SceneAppExt, SceneFormat, SceneFormatError, SceneFormats};

struct FakeFormat(&'static str);

impl SceneFormat for FakeFormat {
    fn extension(&self) -> &str {
        self.0
    }

    fn spawn_root(
        &self,
        commands: &mut Commands,
        _asset_server: &AssetServer,
        _path: &str,
    ) -> Entity {
        commands.spawn_empty().id()
    }

    fn serialize(&self, _world: &World, _entities: &[Entity]) -> Result<Vec<u8>, SceneFormatError> {
        Ok(Vec::new())
    }
}

#[test]
fn resolves_a_format_by_path_suffix() {
    let mut app = App::new();
    app.register_scene_format(FakeFormat("scn.ron"));

    let formats = app.world().resource::<SceneFormats>();
    assert!(formats.for_path("scenes/demo.scn.ron").is_some());
    assert!(formats.for_path("scenes/demo.other").is_none());
}

#[test]
fn the_longest_matching_extension_wins() {
    let mut app = App::new();
    app.register_scene_format(FakeFormat("ron"));
    app.register_scene_format(FakeFormat("scn.ron"));

    let formats = app.world().resource::<SceneFormats>();
    let format = formats
        .for_path("scenes/demo.scn.ron")
        .expect("a format matches");
    assert_eq!(format.extension(), "scn.ron");
}

#[test]
fn default_format_is_the_only_registered_one() {
    let mut app = App::new();
    app.register_scene_format(FakeFormat("scn.ron"));

    let formats = app.world().resource::<SceneFormats>();
    assert_eq!(
        formats.default_format().map(SceneFormat::extension),
        Some("scn.ron")
    );
}
