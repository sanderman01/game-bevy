//! Reading RON that may carry fields the loader does not know, and reporting them.
//!
//! Content written for a newer version of a game must still load, and a misspelt field must still
//! be visible. A loader reads with [`ron_from_bytes_lenient`], which skips unknown fields instead of
//! failing and lists them, then hands the list to [`UnknownFieldSink::record`]. The sink lands in
//! [`ContentReport::unknown_fields`], which `content.report` shows.

use bevy::{
    ecs::{
        resource::Resource,
        system::{Res, ResMut},
    },
    log::warn,
};
use ename_asset_package::ContentReport;
use serde::de::DeserializeOwned;
use std::sync::{Arc, Mutex, PoisonError};

/// Deserializes `bytes` as RON, skipping unknown fields. Returns the value and the path of every
/// field it skipped, such as `parts.0.colour`.
///
/// The target type must not use `deny_unknown_fields`, or it fails as before.
pub fn ron_from_bytes_lenient<T: DeserializeOwned>(
    bytes: &[u8],
    options: &ron::Options,
) -> Result<(T, Vec<String>), ron::error::SpannedError> {
    let mut deserializer = ron::Deserializer::from_bytes_with_options(bytes, options)?;
    let mut unknown = Vec::new();
    let value = serde_ignored::deserialize(&mut deserializer, |path| {
        unknown.push(path.to_string());
    })
    .map_err(|err| deserializer.span_error(err))?;
    deserializer
        .end()
        .map_err(|err| deserializer.span_error(err))?;
    Ok((value, unknown))
}

/// Where loaders record the unknown fields they skipped. A loader takes a clone in its
/// `FromWorld`, since a loader runs on a task with no `World`.
#[derive(Resource, Clone, Default)]
pub struct UnknownFieldSink(Arc<Mutex<Vec<Recorded>>>);

/// One load: the asset path and the unknown fields it held.
type Recorded = (String, Vec<String>);

impl UnknownFieldSink {
    /// Records what one load of `asset` skipped, replacing what an earlier load recorded. Call it
    /// after every load, with an empty list too, so a fixed file clears its warning on reload.
    pub fn record(&self, asset: impl Into<String>, fields: Vec<String>) {
        let asset = asset.into();
        if !fields.is_empty() {
            warn!("{asset}: unknown fields {}", fields.join(", "));
        }
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((asset, fields));
    }
}

/// Moves what loaders recorded into the report, once it exists.
pub(crate) fn collect_unknown_fields(
    sink: Res<UnknownFieldSink>,
    report: Option<ResMut<ContentReport>>,
) {
    let Some(mut report) = report else {
        return;
    };
    let recorded = std::mem::take(&mut *sink.0.lock().unwrap_or_else(PoisonError::into_inner));
    for (asset, fields) in recorded {
        report.record_unknown_fields(asset, fields);
    }
}

#[cfg(test)]
mod tests {
    use super::ron_from_bytes_lenient;
    use serde::Deserialize;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Part {
        name: String,
    }

    #[derive(Deserialize, Debug, PartialEq)]
    struct Ship {
        name: String,
        #[serde(default)]
        parts: Vec<Part>,
    }

    #[test]
    fn unknown_fields_are_skipped_and_listed() {
        let text = br#"(name: "a", colour: "red", parts: [(name: "mast", height: 3)])"#;
        let (ship, unknown): (Ship, _) =
            ron_from_bytes_lenient(text, &ron::Options::default()).unwrap();
        assert_eq!(ship.name, "a");
        assert_eq!(
            ship.parts,
            [Part {
                name: "mast".into()
            }]
        );
        assert_eq!(unknown, ["colour", "parts.0.height"]);
    }

    #[test]
    fn a_file_with_only_known_fields_lists_nothing() {
        let (_, unknown): (Ship, _) =
            ron_from_bytes_lenient(br#"(name: "a")"#, &ron::Options::default()).unwrap();
        assert!(unknown.is_empty());
    }

    #[test]
    fn a_syntax_error_still_fails() {
        assert!(
            ron_from_bytes_lenient::<Ship>(br#"(name: "a",, )"#, &ron::Options::default()).is_err()
        );
    }
}
