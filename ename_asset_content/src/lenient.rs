//! Reading RON that may carry fields the loader does not know, and reporting them.
//!
//! Content written for a newer version of a game must still load, and a misspelt field must still
//! be visible. Three ways in, from most to least done for you:
//!
//! - [`ContentAppExt::register_ron_content_kind`](crate::ContentAppExt::register_ron_content_kind)
//!   registers a kind with [`RonContentLoader`], which does everything below, and
//!   [`ContentAppExt::register_ron_asset`](crate::ContentAppExt::register_ron_asset) registers
//!   the same loader for a plain asset type that is not a kind.
//! - A loader of its own calls [`read_ron_lenient`], which reads, parses and records in one call.
//! - Code outside a loader (part parameters read at spawn) calls [`ron_from_bytes_lenient`] and
//!   reports the list itself.
//!
//! What loaders record lands in [`ContentReport::unknown_fields`], which `content.report` shows.
//!
//! `serde_ignored` sees only fields that serde's derive skips. Unknown fields inside a
//! `#[serde(flatten)]` struct or an untagged enum are swallowed by serde's buffering and never
//! reported.

use bevy::{
    asset::{Asset, AssetLoader, LoadContext, io::Reader},
    ecs::{
        resource::Resource,
        system::{Res, ResMut},
        world::World,
    },
    log::warn,
    reflect::TypePath,
};
use ename_asset_package::ContentReport;
use serde::de::DeserializeOwned;
use std::{
    marker::PhantomData,
    sync::{Arc, Mutex, PoisonError},
};

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

/// Why a lenient RON read failed.
#[derive(Debug, thiserror::Error)]
pub enum RonContentError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Ron(#[from] ron::error::SpannedError),
}

/// Reads all of `reader` as RON into `T`, skipping unknown fields, and records them for the asset
/// `load_context` is loading. Records on every outcome, an empty list when the read or the parse
/// fails, so a warning from an earlier load never outlives the file that caused it.
pub async fn read_ron_lenient<T: DeserializeOwned>(
    reader: &mut dyn Reader,
    load_context: &mut LoadContext<'_>,
    sink: &UnknownFieldSink,
    options: &ron::Options,
) -> Result<T, RonContentError> {
    let asset = load_context.path().to_string();
    let mut bytes = Vec::new();
    let parsed = match reader.read_to_end(&mut bytes).await {
        Ok(_) => ron_from_bytes_lenient(&bytes, options).map_err(RonContentError::from),
        Err(err) => Err(RonContentError::from(err)),
    };
    match parsed {
        Ok((value, unknown)) => {
            sink.record(asset, unknown);
            Ok(value)
        }
        Err(err) => {
            sink.record(asset, Vec::new());
            Err(err)
        }
    }
}

/// Where loaders record the unknown fields they skipped. A loader takes a clone of the resource in
/// its `FromWorld`, since a loader runs on a task with no `World`, and hands it to
/// [`read_ron_lenient`].
#[derive(Resource, Clone, Default)]
pub struct UnknownFieldSink(Arc<Mutex<Vec<Recorded>>>);

/// One load: the asset path and the unknown fields it held.
type Recorded = (String, Vec<String>);

impl UnknownFieldSink {
    /// Records what one load of `asset` skipped, replacing what an earlier load recorded.
    pub(crate) fn record(&self, asset: String, fields: Vec<String>) {
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

/// Loads `T` from RON with [`read_ron_lenient`] and default [`ron::Options`]. Registered by
/// [`ContentAppExt::register_ron_content_kind`](crate::ContentAppExt::register_ron_content_kind)
/// and [`ContentAppExt::register_ron_asset`](crate::ContentAppExt::register_ron_asset).
#[derive(TypePath)]
pub struct RonContentLoader<T> {
    extensions: [&'static str; 1],
    unknown_fields: UnknownFieldSink,
    _asset: PhantomData<fn() -> T>,
}

impl<T> RonContentLoader<T> {
    pub(crate) fn new(extension: &'static str, world: &mut World) -> Self {
        Self {
            extensions: [extension],
            unknown_fields: (*world.get_resource_or_init::<UnknownFieldSink>()).clone(),
            _asset: PhantomData,
        }
    }
}

impl<T: Asset + DeserializeOwned> AssetLoader for RonContentLoader<T> {
    type Asset = T;
    type Settings = ();
    type Error = RonContentError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<T, RonContentError> {
        read_ron_lenient(
            reader,
            load_context,
            &self.unknown_fields,
            &ron::Options::default(),
        )
        .await
    }

    fn extensions(&self) -> &[&str] {
        &self.extensions
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
