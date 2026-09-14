//! `StageId`/`StageMember` are plain reflected components: this only proves they compile,
//! derive what they need to, and compare the way callers expect.

use bevy::prelude::*;
use ename_engine::stage::{StageId, StageMember};
use uuid::Uuid;

#[test]
fn stage_id_equality_is_by_value() {
    let id = Uuid::new_v4();
    assert_eq!(StageId(id), StageId(id));
    assert_ne!(StageId(id), StageId(Uuid::new_v4()));
}

#[test]
fn stage_membership_wraps_a_stage_id() {
    let id = StageId(Uuid::new_v4());
    let membership = StageMember(id);
    assert_eq!(membership.0, id);
}

#[test]
fn identity_components_register_for_reflection() {
    let mut app = App::new();
    app.register_type::<StageId>()
        .register_type::<StageMember>();

    let registry = app.world().resource::<AppTypeRegistry>().read();
    assert!(registry.get(std::any::TypeId::of::<StageId>()).is_some());
    assert!(
        registry
            .get(std::any::TypeId::of::<StageMember>())
            .is_some()
    );
}
