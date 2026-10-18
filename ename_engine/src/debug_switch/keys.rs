//! The one system that turns key chords into debug switch changes.

use bevy::prelude::*;

use super::registry::{DebugSwitchRegistry, DebugSwitchTarget, DebugSwitchWorldExt, KeyChord};

/// Toggles or cycles the debug switch bound to each chord pressed this frame in the live target.
pub(super) fn apply_debug_keys(world: &mut World) {
    let chords: Vec<KeyChord> = {
        let keys = world.resource::<ButtonInput<KeyCode>>();
        let held = |a, b| keys.pressed(a) || keys.pressed(b);
        if held(KeyCode::SuperLeft, KeyCode::SuperRight) {
            return;
        }
        let shift = held(KeyCode::ShiftLeft, KeyCode::ShiftRight);
        let ctrl = held(KeyCode::ControlLeft, KeyCode::ControlRight);
        let alt = held(KeyCode::AltLeft, KeyCode::AltRight);
        keys.get_just_pressed()
            .map(|&key| KeyChord {
                key,
                shift,
                ctrl,
                alt,
            })
            .collect()
    };
    if chords.is_empty() {
        return;
    }
    let target = *world.resource::<DebugSwitchTarget>();
    for chord in chords {
        let Some((name, next)) = ({
            let registry = world.resource::<DebugSwitchRegistry>();
            registry
                .by_chord(target, &chord)
                .and_then(|name| registry.get(name))
                .map(|entry| (entry.name, entry.kind.clone()))
        })
        .map(|(name, kind)| {
            let next = world
                .debug_switch(name)
                .ok()
                .flatten()
                .map(|current| kind.next(&current));
            (name, next)
        }) else {
            continue;
        };
        let Some(next) = next else {
            warn!("{chord}: debug switch {name} is not available now");
            continue;
        };
        if let Err(err) = world.set_debug_switch(name, next, &chord.to_string()) {
            warn!("{chord}: {err}");
        }
    }
}
