//! `game.registry.describe`: how to write a type, as one hint line and an example the game
//! accepts.
//!
//! Replaces reading `registry.schema`, which describes reflection structure rather than the wire
//! form and costs the whole 775 KB registry per call. The upstream schema is still there per type
//! with `full_schema`.

use bevy::{
    ecs::reflect::ReflectComponent,
    prelude::*,
    reflect::{TypeInfo, TypeRegistration, TypeRegistry, enums::VariantInfo},
    remote::{
        BrpResult,
        builtin_methods::parse,
        schemas::{SchemaTypesMetadata, json_schema::export_type},
    },
};
use ename_engine::command::{TypeSet, resolve_type};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::value_shape;

pub const DESCRIBE_METHOD: &str = "game.registry.describe";

const DEFAULT_LIMIT: usize = 25;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct DescribeParams {
    /// Full paths, suffixes or short names. A short name several types share returns them all.
    types: Vec<String>,
    /// Case-insensitive substring of the type path.
    contains: Option<String>,
    with_crates: Vec<String>,
    without_crates: Vec<String>,
    components_only: bool,
    limit: Option<usize>,
    full_schema: bool,
}

#[derive(Serialize)]
struct TypeDescription {
    type_path: String,
    short: String,
    kind: &'static str,
    component: bool,
    resource: bool,
    shape: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    example: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    example_source: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: Option<serde_json::Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    variants: Option<Vec<&'static str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema: Option<Value>,
}

#[derive(Serialize)]
struct DescribeResponse {
    types: Vec<TypeDescription>,
    truncated: usize,
    /// Asked-for names that match no registered type.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unregistered: Vec<String>,
    /// Asked-for names that are components reflection cannot read or write.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    not_reflected: Vec<String>,
}

pub(crate) fn describe(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: DescribeParams = params.map(parse).transpose()?.unwrap_or_default();
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();

    let mut unregistered = Vec::new();
    let mut not_reflected = Vec::new();
    let mut matched: Vec<&TypeRegistration> = if params.types.is_empty() {
        registry.iter().collect()
    } else {
        let mut found = Vec::new();
        for name in &params.types {
            let named = named(&registry, name);
            if !named.is_empty() {
                found.extend(named);
                continue;
            }
            match resolve_type(world, TypeSet::Components, None, name) {
                Ok(resolved) if !resolved.reflected => not_reflected.push(resolved.path),
                _ => unregistered.push(name.clone()),
            }
        }
        found
    };

    let needle = params.contains.as_ref().map(|c| c.to_lowercase());
    matched.retain(|registration| {
        let table = registration.type_info().type_path_table();
        let krate = table.crate_name().unwrap_or_default();
        needle
            .as_ref()
            .is_none_or(|needle| table.path().to_lowercase().contains(needle))
            && (params.with_crates.is_empty() || params.with_crates.iter().any(|c| c == krate))
            && !params.without_crates.iter().any(|c| c == krate)
            && (!params.components_only || is_component(registration))
    });
    matched.sort_by_key(|registration| registration.type_info().type_path());
    matched.dedup_by_key(|registration| registration.type_id());

    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    let truncated = matched.len().saturating_sub(limit);
    matched.truncate(limit);

    let fallback = SchemaTypesMetadata::default();
    let metadata = world
        .get_resource::<SchemaTypesMetadata>()
        .unwrap_or(&fallback);
    let types = matched
        .into_iter()
        .map(|registration| {
            let schema = params.full_schema.then(|| {
                serde_json::to_value(export_type(registration, metadata, world.components()).1)
                    .unwrap_or_default()
            });
            describe_one(registration, &registry, schema)
        })
        .collect();
    crate::to_value(DescribeResponse {
        types,
        truncated,
        unregistered,
        not_reflected,
    })
}

/// Every registered type `name` names: its full path, else its short name, else a path suffix.
fn named<'r>(registry: &'r TypeRegistry, name: &str) -> Vec<&'r TypeRegistration> {
    if let Some(registration) = registry.get_with_type_path(name) {
        return vec![registration];
    }
    let suffix = format!("::{name}");
    let short: Vec<_> = registry
        .iter()
        .filter(|r| r.type_info().type_path_table().short_path() == name)
        .collect();
    if !short.is_empty() {
        return short;
    }
    let lower: Vec<_> = registry
        .iter()
        .filter(|r| {
            r.type_info()
                .type_path_table()
                .short_path()
                .eq_ignore_ascii_case(name)
        })
        .collect();
    if !lower.is_empty() {
        return lower;
    }
    registry
        .iter()
        .filter(|r| r.type_info().type_path().ends_with(&suffix))
        .collect()
}

fn is_component(registration: &TypeRegistration) -> bool {
    registration.data::<ReflectComponent>().is_some()
        && registration.data::<ReflectResource>().is_none()
}

fn describe_one(
    registration: &TypeRegistration,
    registry: &TypeRegistry,
    schema: Option<Value>,
) -> TypeDescription {
    let info = registration.type_info();
    let shape = value_shape::describe(registration, registry);
    let (kind, fields, variants) = match info {
        TypeInfo::Struct(info) => (
            "struct",
            Some(
                info.iter()
                    .map(|field| (field.name().to_owned(), Value::from(field.type_path())))
                    .collect(),
            ),
            None,
        ),
        TypeInfo::TupleStruct(_) => ("tuple_struct", None, None),
        TypeInfo::Tuple(_) => ("tuple", None, None),
        TypeInfo::List(_) => ("list", None, None),
        TypeInfo::Array(_) => ("array", None, None),
        TypeInfo::Map(_) => ("map", None, None),
        TypeInfo::Set(_) => ("set", None, None),
        TypeInfo::Enum(info) => (
            "enum",
            None,
            Some(info.iter().map(VariantInfo::name).collect()),
        ),
        TypeInfo::Opaque(_) => ("opaque", None, None),
    };
    TypeDescription {
        type_path: info.type_path().to_owned(),
        short: info.type_path_table().short_path().to_owned(),
        kind,
        component: is_component(registration),
        resource: registration.data::<ReflectResource>().is_some(),
        shape: shape.hint,
        example: shape.example,
        example_source: shape.example_source,
        fields,
        variants,
        schema,
    }
}
