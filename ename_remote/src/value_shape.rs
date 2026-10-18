//! How a type is written over BRP: a one-line hint and an example the game accepts.
//!
//! The upstream JSON schema describes reflection structure, not what serde reads, so it is wrong
//! for every newtype and every glam type. Here the example comes from the real serializer and is
//! deserialized again before it is shown, so it cannot disagree with the wire.

use std::any::TypeId;

use bevy::{
    prelude::*,
    reflect::{
        PartialReflect, ReflectDeserialize, ReflectFromReflect, TypeInfo, TypeRegistration,
        TypeRegistry,
        array::DynamicArray,
        enums::{DynamicEnum, DynamicVariant, VariantInfo},
        list::DynamicList,
        map::DynamicMap,
        serde::{SerializationData, TypedReflectDeserializer, TypedReflectSerializer},
        set::DynamicSet,
        std_traits::ReflectDefault,
        structs::DynamicStruct,
        tuple::DynamicTuple,
        tuple_struct::DynamicTupleStruct,
    },
};
use serde::de::DeserializeSeed;
use serde_json::Value;

/// A default bigger than this, serialized, gives way to a synthesized example.
const MAX_DEFAULT_BYTES: usize = 1024;
/// How deep synthesis recurses before giving up on a type.
const MAX_DEPTH: usize = 6;

/// One type's wire shape.
pub(crate) struct Shape {
    pub hint: String,
    pub example: Option<Value>,
    /// "default" or "synthesized".
    pub example_source: Option<&'static str>,
}

pub(crate) fn describe(registration: &TypeRegistration, registry: &TypeRegistry) -> Shape {
    let (example, example_source) = match example(registration, registry) {
        Some((value, source)) => (Some(value), Some(source)),
        None => (None, None),
    };
    let mut hint = hint(registration, registry, example.as_ref());
    if example.is_none() {
        hint.push_str(" (no verified example)");
    }
    Shape {
        hint,
        example,
        example_source,
    }
}

/// The type's `ReflectDefault` value, else one built from its `TypeInfo`, whichever first
/// survives a round trip through the game's own deserializer.
pub(crate) fn example(
    registration: &TypeRegistration,
    registry: &TypeRegistry,
) -> Option<(Value, &'static str)> {
    let default = registration
        .data::<ReflectDefault>()
        .map(|default| default.default())
        .and_then(|value| verified(registration, registry, value.as_partial_reflect()))
        .filter(|value| value.to_string().len() <= MAX_DEFAULT_BYTES);
    if let Some(value) = default {
        return Some((value, "default"));
    }
    // Left dynamic: its custom-serde nodes are concrete defaults already, and the rest
    // serializes structurally, which is how the game reads a type without `ReflectDeserialize`.
    let built = synthesize(registration, registry, 0, true)?;
    verified(registration, registry, built.as_ref()).map(|value| (value, "synthesized"))
}

/// Serializes `value`, and returns the JSON only if the game reads it back into the type.
fn verified(
    registration: &TypeRegistration,
    registry: &TypeRegistry,
    value: &dyn PartialReflect,
) -> Option<Value> {
    let json = serde_json::to_value(TypedReflectSerializer::new(value, registry)).ok()?;
    let read = TypedReflectDeserializer::new(registration, registry)
        .deserialize(&json)
        .ok()?;
    materialize(registration, read.as_ref()).ok()?;
    Some(json)
}

/// A deserialized value as the concrete type, the way inserting it would build it. Inserting
/// one that cannot be built panics the game, so writes check with this first.
pub(crate) fn materialize(
    registration: &TypeRegistration,
    value: &dyn PartialReflect,
) -> Result<Box<dyn Reflect>, String> {
    if let Some(from) = registration.data::<ReflectFromReflect>()
        && let Some(built) = from.from_reflect(value)
    {
        return Ok(built);
    }
    if let Some(default) = registration.data::<ReflectDefault>() {
        let mut built = default.default();
        built.try_apply(value).map_err(|err| err.to_string())?;
        return Ok(built);
    }
    Err(format!(
        "`{}` cannot be built from this value: give every field",
        registration.type_info().type_path()
    ))
}

/// A value of the type: its `ReflectDefault` where it has one (always for opaque and custom-serde
/// types, whose structure means nothing on the wire), else a dynamic built field by field.
fn synthesize(
    registration: &TypeRegistration,
    registry: &TypeRegistry,
    depth: usize,
    skip_default: bool,
) -> Option<Box<dyn PartialReflect>> {
    if depth > MAX_DEPTH {
        return None;
    }
    let type_info = registration.type_info();
    let opaque = matches!(type_info, TypeInfo::Opaque(_));
    let custom = registration.data::<ReflectDeserialize>().is_some();
    if (!skip_default || opaque || custom)
        && let Some(default) = registration.data::<ReflectDefault>()
    {
        return Some(default.default().into_partial_reflect());
    }
    if opaque || custom {
        return None;
    }
    let field = |type_id: TypeId| {
        let registration = registry.get(type_id)?;
        synthesize(registration, registry, depth + 1, false)
    };
    let represented = Some(type_info);
    Some(match type_info {
        TypeInfo::Struct(info) => {
            let mut value = DynamicStruct::default();
            for named in info.iter() {
                value.insert_boxed(named.name(), field(named.type_id())?);
            }
            value.set_represented_type(represented);
            Box::new(value)
        }
        TypeInfo::TupleStruct(info) => {
            let mut value = DynamicTupleStruct::default();
            for unnamed in info.iter() {
                value.insert_boxed(field(unnamed.type_id())?);
            }
            value.set_represented_type(represented);
            Box::new(value)
        }
        TypeInfo::Tuple(info) => {
            let mut value = DynamicTuple::default();
            for unnamed in info.iter() {
                value.insert_boxed(field(unnamed.type_id())?);
            }
            value.set_represented_type(represented);
            Box::new(value)
        }
        TypeInfo::Array(info) => {
            let items: Option<Vec<_>> = (0..info.capacity())
                .map(|_| field(info.item_ty().id()))
                .collect();
            let mut value = DynamicArray::new(items?.into_boxed_slice());
            value.set_represented_type(represented);
            Box::new(value)
        }
        TypeInfo::List(_) => {
            let mut value = DynamicList::default();
            value.set_represented_type(represented);
            Box::new(value)
        }
        TypeInfo::Map(_) => {
            let mut value = DynamicMap::default();
            value.set_represented_type(represented);
            Box::new(value)
        }
        TypeInfo::Set(_) => {
            let mut value = DynamicSet::default();
            value.set_represented_type(represented);
            Box::new(value)
        }
        TypeInfo::Enum(info) => {
            let unit = info
                .iter()
                .find(|variant| matches!(variant, VariantInfo::Unit(_)));
            let variant = unit.or_else(|| info.iter().next())?;
            let payload = match variant {
                VariantInfo::Unit(_) => DynamicVariant::Unit,
                VariantInfo::Tuple(tuple) => {
                    let mut payload = DynamicTuple::default();
                    for unnamed in tuple.iter() {
                        payload.insert_boxed(field(unnamed.type_id())?);
                    }
                    DynamicVariant::Tuple(payload)
                }
                VariantInfo::Struct(fields) => {
                    let mut payload = DynamicStruct::default();
                    for named in fields.iter() {
                        payload.insert_boxed(named.name(), field(named.type_id())?);
                    }
                    DynamicVariant::Struct(payload)
                }
            };
            // The serializer names the variant by its index, so it must be the real one.
            let index = info.index_of(variant.name())?;
            let mut value = DynamicEnum::new_with_index(index, variant.name(), payload);
            value.set_represented_type(represented);
            Box::new(value)
        }
        TypeInfo::Opaque(_) => return None,
    })
}

/// The one-line hint. The first matching rule wins; the named special cases come first because
/// generic rows would hide them.
pub(crate) fn hint(
    registration: &TypeRegistration,
    registry: &TypeRegistry,
    example: Option<&Value>,
) -> String {
    let info = registration.type_info();
    let path = info.type_path();
    if registration.type_id() == TypeId::of::<Entity>() {
        return "entity id (u64) from world_query; changes every run".to_owned();
    }
    if path.starts_with("bevy_asset::handle::Handle<") {
        return "asset handle: not writable over BRP".to_owned();
    }
    if let TypeInfo::Enum(info) = info
        && is_option(
            info.type_path_table().module_path(),
            info.type_path_table().ident(),
        )
    {
        let inner = match info.variant("Some") {
            Some(VariantInfo::Tuple(some)) => some
                .field_at(0)
                .map_or("value", |field| field.type_path_table().short_path()),
            _ => "value",
        };
        return format!("{inner} or null");
    }
    // What the game reads is what matters, so serde's own deserialize decides this row.
    if registration.data::<ReflectDeserialize>().is_some() || matches!(info, TypeInfo::Opaque(_)) {
        let kind = example.map_or_else(|| "custom serde".to_owned(), json_kind);
        return if matches!(info, TypeInfo::Opaque(_)) {
            kind
        } else {
            format!("custom serde: {kind}")
        };
    }
    let short = |type_id: TypeId| {
        registry
            .get(type_id)
            .map_or("value", |r| r.type_info().type_path_table().short_path())
    };
    match info {
        TypeInfo::Struct(info) if info.field_len() == 0 => "unit struct: write {}".to_owned(),
        TypeInfo::Struct(info) => {
            let fields: Vec<&str> = info.iter().map(|field| field.name()).collect();
            let optional = if registration.data::<ReflectDefault>().is_some() {
                " (any may be left out)"
            } else {
                ""
            };
            format!("object with fields {}{optional}", fields.join(", "))
        }
        TypeInfo::TupleStruct(info)
            if info.field_len() == 1 && registration.data::<SerializationData>().is_none() =>
        {
            let inner = short(info.field_at(0).map_or(TypeId::of::<()>(), |f| f.type_id()));
            format!("newtype over {inner}: write the {inner} value itself, not [value]")
        }
        TypeInfo::TupleStruct(info) => format!("array of {} items", info.field_len()),
        TypeInfo::Tuple(info) => format!("array of {} items", info.field_len()),
        TypeInfo::Array(info) => {
            format!(
                "array of {} {}",
                info.capacity(),
                short(info.item_ty().id())
            )
        }
        TypeInfo::List(info) => format!("array of {}", short(info.item_ty().id())),
        TypeInfo::Set(info) => format!("array of unique {}", short(info.value_ty().id())),
        TypeInfo::Map(info) => format!(
            "object of {} to {}",
            short(info.key_ty().id()),
            short(info.value_ty().id())
        ),
        TypeInfo::Enum(info) => {
            let unit_only = info.iter().all(|v| matches!(v, VariantInfo::Unit(_)));
            if unit_only {
                let names: Vec<String> = info.iter().map(|v| format!("\"{}\"", v.name())).collect();
                return format!("one of {} (bare string)", names.join(" | "));
            }
            let forms: Vec<String> = info
                .iter()
                .map(|variant| match variant {
                    VariantInfo::Unit(v) => format!("\"{}\"", v.name()),
                    VariantInfo::Tuple(v) if v.field_len() == 1 => {
                        format!("{{\"{}\": value}}", v.name())
                    }
                    VariantInfo::Tuple(v) => format!("{{\"{}\": [values]}}", v.name()),
                    VariantInfo::Struct(v) => format!("{{\"{}\": {{fields}}}}", v.name()),
                })
                .collect();
            format!("one of {}", forms.join(", "))
        }
        TypeInfo::Opaque(_) => "custom serde".to_owned(),
    }
}

fn is_option(module: Option<&str>, ident: Option<&str>) -> bool {
    module == Some("core::option") && ident == Some("Option")
}

/// The JSON kind of a value, for a hint: `array of 3 numbers`, `a string`.
fn json_kind(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(_) => "true or false".to_owned(),
        Value::Number(_) => "a number".to_owned(),
        Value::String(_) => "a string".to_owned(),
        Value::Array(items) if items.iter().all(Value::is_number) => {
            format!("array of {} numbers", items.len())
        }
        Value::Array(items) => format!("array of {} items", items.len()),
        Value::Object(fields) => {
            let names: Vec<&str> = fields.keys().map(String::as_str).collect();
            format!("object with fields {}", names.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use serde_json::json;

    use super::*;

    #[derive(Component, Reflect, Default)]
    #[reflect(Component, Default)]
    struct Marker;

    #[derive(Component, Reflect, Default)]
    #[reflect(Component, Default)]
    struct Torque(Vec3);

    #[derive(Reflect, Default)]
    enum Mode {
        #[default]
        Off,
        On,
    }

    #[derive(Reflect)]
    enum Shot {
        Still,
        Burst(u32),
    }

    /// No `Default`, and a nested custom-serde field.
    #[derive(Component, Reflect)]
    #[reflect(Component)]
    struct Aim {
        at: Vec3,
        steady: bool,
    }

    /// Serializes with serde, deserializes by reflection: the default's own form is not read back.
    #[derive(Reflect, Default, Serialize)]
    #[reflect(Serialize, Default)]
    struct Renamed {
        #[serde(rename = "m")]
        value: f32,
    }

    fn registry() -> TypeRegistry {
        let mut registry = TypeRegistry::new();
        registry.register::<Marker>();
        registry.register::<Torque>();
        registry.register::<Mode>();
        registry.register::<Shot>();
        registry.register::<Aim>();
        registry.register::<Renamed>();
        registry.register::<Option<f32>>();
        registry.register::<Entity>();
        registry.register::<Quat>();
        registry
    }

    fn shape<T: 'static>(registry: &TypeRegistry) -> Shape {
        describe(registry.get(TypeId::of::<T>()).unwrap(), registry)
    }

    #[test]
    fn hints_name_the_wire_form() {
        let registry = registry();
        for (hint, expected) in [
            (shape::<Marker>(&registry).hint, "unit struct: write {}"),
            (
                shape::<Torque>(&registry).hint,
                "newtype over Vec3: write the Vec3 value itself, not [value]",
            ),
            (
                shape::<Quat>(&registry).hint,
                "custom serde: array of 4 numbers",
            ),
            (
                shape::<Mode>(&registry).hint,
                r#"one of "Off" | "On" (bare string)"#,
            ),
            (
                shape::<Shot>(&registry).hint,
                r#"one of "Still", {"Burst": value}"#,
            ),
            (shape::<Option<f32>>(&registry).hint, "f32 or null"),
            (
                shape::<Entity>(&registry).hint,
                "entity id (u64) from world_query; changes every run",
            ),
        ] {
            assert!(hint.starts_with(expected), "{hint}");
        }
        assert_eq!(shape::<Marker>(&registry).example, Some(json!({})));
        assert_eq!(
            shape::<Torque>(&registry).example,
            Some(json!([0.0, 0.0, 0.0]))
        );
    }

    #[test]
    fn a_struct_without_default_gets_a_synthesized_example_with_glam_arrays() {
        let registry = registry();
        let aim = shape::<Aim>(&registry);
        assert_eq!(aim.example_source, Some("synthesized"));
        assert_eq!(
            aim.example,
            Some(json!({ "at": [0.0, 0.0, 0.0], "steady": false }))
        );
    }

    #[test]
    fn a_default_the_game_cannot_read_back_is_skipped() {
        let registry = registry();
        let renamed = shape::<Renamed>(&registry);
        assert_eq!(renamed.example_source, Some("synthesized"));
        assert_eq!(renamed.example, Some(json!({ "value": 0.0 })));
    }
}
