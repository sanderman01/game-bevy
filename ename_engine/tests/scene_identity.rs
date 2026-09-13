//! `SceneId`/`SceneMembership` are plain reflected components: this only proves they compile,
//! derive what they need to, and compare the way callers expect.

use bevy::prelude::*;
use ename_engine::scene::{SceneId, SceneMembership, SceneName};
use uuid::Uuid;

#[test]
fn scene_id_equality_is_by_value() {
    let id = Uuid::new_v4();
    assert_eq!(SceneId(id), SceneId(id));
    assert_ne!(SceneId(id), SceneId(Uuid::new_v4()));
}

#[test]
fn scene_membership_wraps_a_scene_id() {
    let id = SceneId(Uuid::new_v4());
    let membership = SceneMembership(id);
    assert_eq!(membership.0, id);
}

#[test]
fn identity_components_register_for_reflection() {
    let mut app = App::new();
    app.register_type::<SceneId>()
        .register_type::<SceneMembership>()
        .register_type::<SceneName>();

    let registry = app.world().resource::<AppTypeRegistry>().read();
    assert!(registry.get(std::any::TypeId::of::<SceneId>()).is_some());
    assert!(
        registry
            .get(std::any::TypeId::of::<SceneMembership>())
            .is_some()
    );
    assert!(registry.get(std::any::TypeId::of::<SceneName>()).is_some());
}
