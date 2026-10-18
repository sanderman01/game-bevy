//! Key names and a US-layout table from physical keys to the logical keys egui reads.

use std::sync::LazyLock;

use bevy::{
    input::keyboard::{Key, NativeKey, NativeKeyCode},
    platform::collections::HashMap,
    prelude::*,
    reflect::{
        FromReflect, TypeInfo, Typed,
        enums::{DynamicEnum, DynamicVariant, VariantInfo},
    },
};

/// Every unit variant of [`KeyCode`], by name, from its reflected type info.
static KEY_NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let TypeInfo::Enum(info) = KeyCode::type_info() else {
        return Vec::new();
    };
    info.iter()
        .filter_map(|variant| match variant {
            VariantInfo::Unit(unit) => Some(unit.name()),
            _ => None,
        })
        .collect()
});

/// A `KeyCode` from its variant name (`F10`, `KeyW`, `ShiftLeft`). Only unit variants parse.
pub(crate) fn parse_key(name: &str) -> Option<KeyCode> {
    if !KEY_NAMES.contains(&name) {
        return None;
    }
    KeyCode::from_reflect(&DynamicEnum::new(name, DynamicVariant::Unit))
}

/// Up to five key names close to `name`, for an `unknown_key` hint.
pub(crate) fn close_key_names(name: &str) -> Vec<&'static str> {
    let lower = name.to_lowercase();
    let mut scored: Vec<(usize, &'static str)> = KEY_NAMES
        .iter()
        .map(|candidate| (distance(&lower, &candidate.to_lowercase()), *candidate))
        .collect();
    scored.sort();
    scored.into_iter().take(5).map(|(_, name)| name).collect()
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let current = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(current + 1);
            prev = current;
        }
    }
    row[b.len()]
}

/// Unshifted characters of a US keyboard.
const CHARACTERS: [(KeyCode, char); 47] = [
    (KeyCode::KeyA, 'a'),
    (KeyCode::KeyB, 'b'),
    (KeyCode::KeyC, 'c'),
    (KeyCode::KeyD, 'd'),
    (KeyCode::KeyE, 'e'),
    (KeyCode::KeyF, 'f'),
    (KeyCode::KeyG, 'g'),
    (KeyCode::KeyH, 'h'),
    (KeyCode::KeyI, 'i'),
    (KeyCode::KeyJ, 'j'),
    (KeyCode::KeyK, 'k'),
    (KeyCode::KeyL, 'l'),
    (KeyCode::KeyM, 'm'),
    (KeyCode::KeyN, 'n'),
    (KeyCode::KeyO, 'o'),
    (KeyCode::KeyP, 'p'),
    (KeyCode::KeyQ, 'q'),
    (KeyCode::KeyR, 'r'),
    (KeyCode::KeyS, 's'),
    (KeyCode::KeyT, 't'),
    (KeyCode::KeyU, 'u'),
    (KeyCode::KeyV, 'v'),
    (KeyCode::KeyW, 'w'),
    (KeyCode::KeyX, 'x'),
    (KeyCode::KeyY, 'y'),
    (KeyCode::KeyZ, 'z'),
    (KeyCode::Digit0, '0'),
    (KeyCode::Digit1, '1'),
    (KeyCode::Digit2, '2'),
    (KeyCode::Digit3, '3'),
    (KeyCode::Digit4, '4'),
    (KeyCode::Digit5, '5'),
    (KeyCode::Digit6, '6'),
    (KeyCode::Digit7, '7'),
    (KeyCode::Digit8, '8'),
    (KeyCode::Digit9, '9'),
    (KeyCode::Minus, '-'),
    (KeyCode::Equal, '='),
    (KeyCode::BracketLeft, '['),
    (KeyCode::BracketRight, ']'),
    (KeyCode::Backslash, '\\'),
    (KeyCode::Semicolon, ';'),
    (KeyCode::Quote, '\''),
    (KeyCode::Backquote, '`'),
    (KeyCode::Comma, ','),
    (KeyCode::Period, '.'),
    (KeyCode::Slash, '/'),
];

static BY_CHAR: LazyLock<HashMap<char, KeyCode>> = LazyLock::new(|| {
    CHARACTERS
        .iter()
        .flat_map(|&(code, c)| [(c, code), (c.to_ascii_uppercase(), code)])
        .chain([(' ', KeyCode::Space)])
        .collect()
});

/// The physical key that types `c` on a US layout, or `Unidentified` for one it has none for.
pub(crate) fn key_for_char(c: char) -> KeyCode {
    match c {
        '\n' => KeyCode::Enter,
        '\t' => KeyCode::Tab,
        _ => BY_CHAR
            .get(&c)
            .copied()
            .unwrap_or(KeyCode::Unidentified(NativeKeyCode::Unidentified)),
    }
}

/// The logical key a `text` step sends for `c`: Enter and Tab for newline and tab, so game code
/// sees those keys, and the character itself otherwise.
pub(crate) fn logical_char(c: char) -> Key {
    match c {
        '\n' => Key::Enter,
        '\t' => Key::Tab,
        _ => Key::Character(c.to_string().into()),
    }
}

/// What a US layout makes of a physical key with no modifiers held.
pub(crate) fn logical_key(code: KeyCode) -> Key {
    if let Some((_, c)) = CHARACTERS.iter().find(|(key, _)| *key == code) {
        return Key::Character(c.to_string().into());
    }
    match code {
        KeyCode::Space => Key::Space,
        KeyCode::Enter | KeyCode::NumpadEnter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Escape => Key::Escape,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::ArrowUp => Key::ArrowUp,
        KeyCode::ArrowDown => Key::ArrowDown,
        KeyCode::ArrowLeft => Key::ArrowLeft,
        KeyCode::ArrowRight => Key::ArrowRight,
        KeyCode::ShiftLeft | KeyCode::ShiftRight => Key::Shift,
        KeyCode::ControlLeft | KeyCode::ControlRight => Key::Control,
        KeyCode::AltLeft | KeyCode::AltRight => Key::Alt,
        KeyCode::SuperLeft | KeyCode::SuperRight => Key::Super,
        KeyCode::F1 => Key::F1,
        KeyCode::F2 => Key::F2,
        KeyCode::F3 => Key::F3,
        KeyCode::F4 => Key::F4,
        KeyCode::F5 => Key::F5,
        KeyCode::F6 => Key::F6,
        KeyCode::F7 => Key::F7,
        KeyCode::F8 => Key::F8,
        KeyCode::F9 => Key::F9,
        KeyCode::F10 => Key::F10,
        KeyCode::F11 => Key::F11,
        KeyCode::F12 => Key::F12,
        _ => Key::Unidentified(NativeKey::Unidentified),
    }
}

/// The key a modifier name in a `key` step presses.
pub(crate) fn modifier_key(name: Modifier) -> KeyCode {
    match name {
        Modifier::Shift => KeyCode::ShiftLeft,
        Modifier::Ctrl => KeyCode::ControlLeft,
        Modifier::Alt => KeyCode::AltLeft,
        Modifier::Super => KeyCode::SuperLeft,
    }
}

/// A modifier a `key` step holds around its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Modifier {
    Shift,
    Ctrl,
    Alt,
    Super,
}
