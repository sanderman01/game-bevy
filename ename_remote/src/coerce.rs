//! Reading a value an agent wrote: strictly first, then once more after fixing the mistakes that
//! have one unambiguous fix, reporting every fix.
//!
//! A value that parses as sent is used as sent, so a fix never changes the meaning of a valid
//! value, and the walk only descends into the parts that fail on their own. When nothing fixes
//! it, the error names the deepest failing field, its shape and an example.

use std::any::TypeId;

use bevy::reflect::{
    PartialReflect, ReflectDeserialize, TypeInfo, TypeRegistration, TypeRegistry,
    enums::VariantInfo,
    serde::{SerializationData, TypedReflectDeserializer},
};
use serde::{Serialize, de::DeserializeSeed};
use serde_json::{Map, Value};

use crate::value_shape;

/// One fix applied to a value.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub(crate) struct Coercion {
    /// Field path inside the value, as `world_mutate_component` writes paths.
    pub at: String,
    pub rule: &'static str,
    pub from: Value,
    pub to: Value,
}

/// A value the type does not accept, located.
#[derive(Debug)]
pub(crate) struct ValueError {
    /// The deepest failing field; empty for the value as a whole.
    pub at: String,
    pub message: String,
    pub expected_type: String,
    /// The shape hint of `expected_type`.
    pub expected: String,
    pub example: Option<Value>,
}

impl ValueError {
    /// "`Owner` is invalid at `field`: why (value_shape). Expected: shape. Example: value"
    pub(crate) fn sentence(&self, owner: &str) -> String {
        let at = if self.at.is_empty() {
            String::new()
        } else {
            format!(" at `{}`", self.at)
        };
        let mut sentence = format!(
            "`{owner}` is invalid{at}: {}. Expected: {}",
            self.message, self.expected
        );
        if let Some(example) = &self.example {
            sentence.push_str(&format!(". Example: {example}"));
        }
        sentence
    }

    pub(crate) fn data(&self) -> Value {
        serde_json::json!({
            "at": self.at,
            "expected_type": self.expected_type,
            "expected": self.expected,
            "example": self.example,
        })
    }
}

/// A value as the type reads it, and the fixes it needed.
pub(crate) type Read = (Box<dyn PartialReflect>, Vec<Coercion>);

/// Deserializes `value` as the type of `registration`. `base` is the field path the value sits
/// at, for the reports. Without `strict`, a value that fails is normalized once and tried again.
pub(crate) fn deserialize(
    registration: &TypeRegistration,
    value: &Value,
    registry: &TypeRegistry,
    strict: bool,
    base: &str,
) -> Result<Read, Box<ValueError>> {
    let read = match parse(registration, value, registry) {
        Ok(read) => Ok((read, Vec::new())),
        Err(first) => {
            let mut coerced = Vec::new();
            let fixed = (!strict)
                .then(|| normalize(registration, value, registry, base, &mut coerced))
                .filter(|_| !coerced.is_empty())
                .and_then(|fixed| parse(registration, &fixed, registry).ok());
            match fixed {
                Some(read) => Ok((read, coerced)),
                None => Err(locate(registration, value, registry, base).unwrap_or((
                    base.to_owned(),
                    registration,
                    first,
                ))),
            }
        }
    };
    let (at, failing, message) = match read {
        Ok((read, coerced)) => match unbuildable_item(read.as_ref(), registry, base) {
            None => return Ok((read, coerced)),
            Some((at, item)) => (
                at,
                item,
                format!(
                    "a new `{}` item needs every field",
                    item.type_info().type_path_table().short_path()
                ),
            ),
        },
        Err(located) => located,
    };
    let shape = value_shape::describe(failing, registry);
    Err(Box::new(ValueError {
        at,
        message,
        expected_type: failing.type_info().type_path().to_owned(),
        expected: shape.hint,
        example: shape.example,
    }))
}

fn parse(
    registration: &TypeRegistration,
    value: &Value,
    registry: &TypeRegistry,
) -> Result<Box<dyn PartialReflect>, String> {
    TypedReflectDeserializer::new(registration, registry)
        .deserialize(value)
        .map_err(|err| err.to_string())
}

/// The first list, map or set item in `value` that cannot be built as its concrete type, with its
/// path and type. Deserializing accepts a struct with fields missing, and applying such an item
/// to a collection panics the game (`List::push` needs a concrete value).
pub(crate) fn unbuildable_item<'r>(
    value: &dyn PartialReflect,
    registry: &'r TypeRegistry,
    at: &str,
) -> Option<(String, &'r TypeRegistration)> {
    use bevy::reflect::{ReflectFromReflect, ReflectRef};
    let item_types = value.get_represented_type_info().map(|info| match info {
        TypeInfo::List(info) => (None, Some(info.item_ty().id())),
        TypeInfo::Set(info) => (None, Some(info.value_ty().id())),
        TypeInfo::Map(info) => (Some(info.key_ty().id()), Some(info.value_ty().id())),
        _ => (None, None),
    });
    let check = |item: &dyn PartialReflect, type_id: Option<TypeId>, at: String| {
        let registration = registry.get(type_id?)?;
        let from = registration.data::<ReflectFromReflect>()?;
        if from.from_reflect(item).is_none() {
            return Some((at, registration));
        }
        unbuildable_item(item, registry, &at)
    };
    let (key_type, item_type) = item_types.unwrap_or((None, None));
    match value.reflect_ref() {
        ReflectRef::Struct(value) => (0..value.field_len()).find_map(|i| {
            let name = value.name_at(i)?;
            unbuildable_item(value.field_at(i)?, registry, &join(at, name))
        }),
        ReflectRef::TupleStruct(value) => value
            .iter_fields()
            .enumerate()
            .find_map(|(i, field)| unbuildable_item(field, registry, &join(at, &i.to_string()))),
        ReflectRef::Tuple(value) => value
            .iter_fields()
            .enumerate()
            .find_map(|(i, field)| unbuildable_item(field, registry, &join(at, &i.to_string()))),
        ReflectRef::Enum(value) => value.iter_fields().enumerate().find_map(|(i, field)| {
            let name = field.name().map_or_else(|| i.to_string(), str::to_owned);
            unbuildable_item(field.value(), registry, &join(at, &name))
        }),
        ReflectRef::Array(value) => value
            .iter()
            .enumerate()
            .find_map(|(i, item)| unbuildable_item(item, registry, &index(at, i))),
        ReflectRef::List(value) => value
            .iter()
            .enumerate()
            .find_map(|(i, item)| check(item, item_type, index(at, i))),
        ReflectRef::Set(value) => value
            .iter()
            .enumerate()
            .find_map(|(i, item)| check(item, item_type, index(at, i))),
        ReflectRef::Map(value) => value.iter().enumerate().find_map(|(i, (key, item))| {
            check(key, key_type, index(at, i)).or_else(|| check(item, item_type, index(at, i)))
        }),
        _ => None,
    }
}

fn join(base: &str, field: &str) -> String {
    if base.is_empty() {
        field.to_owned()
    } else {
        format!("{base}.{field}")
    }
}

fn index(base: &str, index: usize) -> String {
    format!("{base}[{index}]")
}

/// Whether the game reads the type through serde rather than its reflected structure.
fn custom(registration: &TypeRegistration) -> bool {
    registration.data::<ReflectDeserialize>().is_some()
}

fn is_newtype(registration: &TypeRegistration) -> bool {
    matches!(registration.type_info(), TypeInfo::TupleStruct(info) if info.field_len() == 1)
        && registration.data::<SerializationData>().is_none()
        && !custom(registration)
}

fn is_option(info: &TypeInfo) -> bool {
    let table = info.type_path_table();
    table.module_path() == Some("core::option") && table.ident() == Some("Option")
}

const NUMBERS: [fn() -> TypeId; 14] = [
    TypeId::of::<f32>,
    TypeId::of::<f64>,
    TypeId::of::<i8>,
    TypeId::of::<i16>,
    TypeId::of::<i32>,
    TypeId::of::<i64>,
    TypeId::of::<i128>,
    TypeId::of::<isize>,
    TypeId::of::<u8>,
    TypeId::of::<u16>,
    TypeId::of::<u32>,
    TypeId::of::<u64>,
    TypeId::of::<u128>,
    TypeId::of::<usize>,
];

/// `"3.0"` as a JSON number, for a numeric target. Integers only when the string is whole.
fn numeric(text: &str, float: bool) -> Option<Value> {
    let parsed: f64 = text.trim().parse().ok()?;
    if !parsed.is_finite() {
        return None;
    }
    if float {
        return serde_json::Number::from_f64(parsed).map(Value::Number);
    }
    if parsed.fract() != 0.0 {
        return None;
    }
    let text = text.trim().trim_end_matches(".0");
    text.parse::<i64>()
        .ok()
        .map(Value::from)
        .or_else(|| text.parse::<u64>().ok().map(Value::from))
}

/// Variant names compared without case and underscores: `field_only` is `FieldOnly`.
fn variant_named<'a>(names: impl Iterator<Item = &'a str>, wanted: &str) -> Option<&'a str> {
    let key = |name: &str| name.replace('_', "").to_lowercase();
    let wanted = key(wanted);
    let mut found = names.filter(|name| key(name) == wanted);
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// `value` with every unambiguous fix applied below the first point where it fails.
fn normalize(
    registration: &TypeRegistration,
    value: &Value,
    registry: &TypeRegistry,
    at: &str,
    coerced: &mut Vec<Coercion>,
) -> Value {
    if parse(registration, value, registry).is_ok() {
        return value.clone();
    }
    let fix = |rule: &'static str, to: Value, coerced: &mut Vec<Coercion>| {
        coerced.push(Coercion {
            at: at.to_owned(),
            rule,
            from: value.clone(),
            to: to.clone(),
        });
        to
    };
    let info = registration.type_info();
    let type_id = registration.type_id();
    let child =
        |type_id: TypeId, value: &Value, at: &str, coerced: &mut Vec<Coercion>| match registry
            .get(type_id)
        {
            Some(registration) => normalize(registration, value, registry, at, coerced),
            None => value.clone(),
        };

    if let Value::String(text) = value
        && NUMBERS.iter().any(|id| id() == type_id)
    {
        let float = type_id == TypeId::of::<f32>() || type_id == TypeId::of::<f64>();
        return match numeric(text, float) {
            Some(number) => fix("numeric_string", number, coerced),
            None => value.clone(),
        };
    }

    if custom(registration) {
        // Read by serde: walk by the example's shape, not the reflected structure.
        let fields: Vec<&str> = match info {
            TypeInfo::Struct(info) => info.iter().map(|field| field.name()).collect(),
            _ => Vec::new(),
        };
        let mut value = value.clone();
        if info.type_path().starts_with("glam::")
            && let Value::Object(object) = &value
            && !fields.is_empty()
            && object.len() == fields.len()
            && fields.iter().all(|name| object.contains_key(*name))
        {
            let array: Vec<Value> = fields.iter().map(|name| object[*name].clone()).collect();
            value = fix("object_to_array", Value::Array(array), coerced);
        }
        if let Value::Array(items) = &mut value
            && let Some((Value::Array(example), _)) = value_shape::example(registration, registry)
            && example.len() == items.len()
        {
            for (i, (item, shape)) in items.iter_mut().zip(&example).enumerate() {
                if let (Value::String(text), Value::Number(_)) = (&*item, shape)
                    && let Some(number) = numeric(text, shape.is_f64())
                {
                    coerced.push(Coercion {
                        at: index(at, i),
                        rule: "numeric_string",
                        from: item.clone(),
                        to: number.clone(),
                    });
                    *item = number;
                }
            }
        }
        return value;
    }

    match (info, value) {
        (TypeInfo::Struct(info), Value::Null) if info.field_len() == 0 => {
            fix("null_unit_struct", Value::Object(Map::new()), coerced)
        }
        (TypeInfo::Struct(info), Value::Object(object)) => Value::Object(
            object
                .iter()
                .map(|(key, field_value)| {
                    let fixed = match info.field(key) {
                        Some(field) => child(field.type_id(), field_value, &join(at, key), coerced),
                        None => field_value.clone(),
                    };
                    (key.clone(), fixed)
                })
                .collect(),
        ),
        (TypeInfo::TupleStruct(info), _) if is_newtype(registration) => {
            let inner = info.field_at(0).map(|field| field.type_id());
            let Some(inner) = inner else {
                return value.clone();
            };
            if let Value::Array(items) = value
                && let [only] = &items[..]
            {
                let unwrapped = fix("newtype_unwrap", only.clone(), coerced);
                return child(inner, &unwrapped, at, coerced);
            }
            child(inner, value, at, coerced)
        }
        (TypeInfo::TupleStruct(info), Value::Array(items)) => Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(i, item)| match info.field_at(i) {
                    Some(field) => child(field.type_id(), item, &join(at, &i.to_string()), coerced),
                    None => item.clone(),
                })
                .collect(),
        ),
        (TypeInfo::Tuple(info), Value::Array(items)) => Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(i, item)| match info.field_at(i) {
                    Some(field) => child(field.type_id(), item, &join(at, &i.to_string()), coerced),
                    None => item.clone(),
                })
                .collect(),
        ),
        (TypeInfo::List(_) | TypeInfo::Array(_) | TypeInfo::Set(_), Value::Array(items)) => {
            let item_type = match info {
                TypeInfo::List(info) => info.item_ty().id(),
                TypeInfo::Array(info) => info.item_ty().id(),
                TypeInfo::Set(info) => info.value_ty().id(),
                _ => unreachable!(),
            };
            Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| child(item_type, item, &index(at, i), coerced))
                    .collect(),
            )
        }
        (TypeInfo::Map(info), Value::Object(object)) => Value::Object(
            object
                .iter()
                .map(|(key, item)| {
                    let fixed = child(info.value_ty().id(), item, &join(at, key), coerced);
                    (key.clone(), fixed)
                })
                .collect(),
        ),
        (TypeInfo::Enum(info), _) if is_option(registration.type_info()) => {
            match (info.variant("Some"), value) {
                (_, Value::Null) => value.clone(),
                (Some(VariantInfo::Tuple(some)), _) => match some.field_at(0) {
                    Some(field) => child(field.type_id(), value, at, coerced),
                    None => value.clone(),
                },
                _ => value.clone(),
            }
        }
        (TypeInfo::Enum(info), Value::String(text)) => {
            let names = || info.iter().map(VariantInfo::name);
            let unquoted = serde_json::from_str::<String>(text).ok();
            let mut current = text.clone();
            if let Some(inner) = unquoted {
                current = inner.clone();
                fix("quoted_string", Value::String(inner), coerced);
            }
            if !names().any(|name| name == current)
                && let Some(name) = variant_named(names(), &current)
            {
                coerced.push(Coercion {
                    at: at.to_owned(),
                    rule: "variant_case",
                    from: Value::String(current.clone()),
                    to: Value::String(name.to_owned()),
                });
                current = name.to_owned();
            }
            Value::String(current)
        }
        (TypeInfo::Enum(info), Value::Object(object)) if object.len() == 1 => {
            let (key, payload) = object.iter().next().expect("one entry");
            let names = || info.iter().map(VariantInfo::name);
            let mut name = key.clone();
            if !names().any(|candidate| candidate == name)
                && let Some(found) = variant_named(names(), &name)
            {
                coerced.push(Coercion {
                    at: at.to_owned(),
                    rule: "variant_case",
                    from: Value::String(name.clone()),
                    to: Value::String(found.to_owned()),
                });
                name = found.to_owned();
            }
            let payload = match info.variant(&name) {
                Some(VariantInfo::Tuple(tuple)) if tuple.field_len() == 1 => {
                    match tuple.field_at(0) {
                        Some(field) => child(field.type_id(), payload, &join(at, &name), coerced),
                        None => payload.clone(),
                    }
                }
                Some(VariantInfo::Struct(fields)) => match payload {
                    Value::Object(object) => Value::Object(
                        object
                            .iter()
                            .map(|(key, item)| {
                                let fixed = match fields.field(key) {
                                    Some(field) => {
                                        child(field.type_id(), item, &join(at, key), coerced)
                                    }
                                    None => item.clone(),
                                };
                                (key.clone(), fixed)
                            })
                            .collect(),
                    ),
                    _ => payload.clone(),
                },
                _ => payload.clone(),
            };
            Value::Object(Map::from_iter([(name, payload)]))
        }
        _ => value.clone(),
    }
}

/// The deepest field that fails on its own: its path, its type and why.
fn locate<'r>(
    registration: &'r TypeRegistration,
    value: &Value,
    registry: &'r TypeRegistry,
    at: &str,
) -> Option<(String, &'r TypeRegistration, String)> {
    let err = parse(registration, value, registry).err()?;
    let child = |type_id: TypeId, value: &Value, at: String| {
        registry
            .get(type_id)
            .and_then(|registration| locate(registration, value, registry, &at))
    };
    let deeper = if custom(registration) {
        None
    } else {
        match (registration.type_info(), value) {
            (TypeInfo::Struct(info), Value::Object(object)) => object
                .iter()
                .find_map(|(key, v)| child(info.field(key)?.type_id(), v, join(at, key))),
            (TypeInfo::TupleStruct(info), _) if is_newtype(registration) => {
                child(info.field_at(0)?.type_id(), value, at.to_owned())
            }
            (TypeInfo::TupleStruct(info), Value::Array(items)) => {
                items.iter().enumerate().find_map(|(i, item)| {
                    child(info.field_at(i)?.type_id(), item, join(at, &i.to_string()))
                })
            }
            (TypeInfo::List(info), Value::Array(items)) => items
                .iter()
                .enumerate()
                .find_map(|(i, item)| child(info.item_ty().id(), item, index(at, i))),
            (TypeInfo::Array(info), Value::Array(items)) => items
                .iter()
                .enumerate()
                .find_map(|(i, item)| child(info.item_ty().id(), item, index(at, i))),
            (TypeInfo::Enum(info), _) if is_option(registration.type_info()) => {
                match info.variant("Some") {
                    Some(VariantInfo::Tuple(some)) if !value.is_null() => {
                        child(some.field_at(0)?.type_id(), value, at.to_owned())
                    }
                    _ => None,
                }
            }
            (TypeInfo::Enum(info), Value::Object(object)) if object.len() == 1 => {
                let (name, payload) = object.iter().next()?;
                match info.variant(name)? {
                    VariantInfo::Tuple(tuple) if tuple.field_len() == 1 => {
                        child(tuple.field_at(0)?.type_id(), payload, join(at, name))
                    }
                    VariantInfo::Struct(fields) => {
                        payload.as_object()?.iter().find_map(|(key, item)| {
                            child(fields.field(key)?.type_id(), item, join(at, key))
                        })
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    };
    deeper.or_else(|| Some((at.to_owned(), registration, err)))
}

#[cfg(test)]
mod tests {
    use bevy::prelude::*;
    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[derive(Reflect, Default)]
    struct Unit;

    #[derive(Reflect, Default)]
    struct Spin(Vec3);

    #[derive(Reflect, Default)]
    struct Rows(Vec<Vec<f32>>);

    #[derive(Reflect, Default)]
    enum Fill {
        #[default]
        Hidden,
        FieldOnly,
    }

    #[derive(Reflect, Default)]
    enum Clash {
        #[default]
        FooBar,
        Foobar,
    }

    /// Read by serde as `[a, b]`, but not a glam type.
    #[derive(Reflect, Default, Serialize, Deserialize, Clone, Copy)]
    #[reflect(Serialize, Deserialize, Default)]
    #[serde(from = "[f32; 2]", into = "[f32; 2]")]
    struct Span {
        a: f32,
        b: f32,
    }

    impl From<[f32; 2]> for Span {
        fn from([a, b]: [f32; 2]) -> Self {
            Self { a, b }
        }
    }

    impl From<Span> for [f32; 2] {
        fn from(span: Span) -> Self {
            [span.a, span.b]
        }
    }

    /// No `Default`: a partial one cannot be built.
    #[derive(Reflect)]
    struct Mark {
        a: f32,
        b: f32,
    }

    #[derive(Reflect, Default)]
    struct Light {
        bias: f32,
        fill: Fill,
        clash: Clash,
        unit: Unit,
        spin: Spin,
        rows: Rows,
        turn: Quat,
        span: Span,
        marks: Vec<Mark>,
    }

    fn registry() -> TypeRegistry {
        let mut registry = TypeRegistry::new();
        registry.register::<Light>();
        registry
    }

    fn read(registry: &TypeRegistry, value: Value) -> Result<Vec<Coercion>, Box<ValueError>> {
        let registration = registry.get(TypeId::of::<Light>()).unwrap();
        let mut full = json!({
            "bias": 0.0, "fill": "Hidden", "clash": "FooBar", "unit": {}, "spin": [0.0, 0.0, 0.0],
            "rows": [], "turn": [0.0, 0.0, 0.0, 1.0], "span": [0.0, 0.0], "marks": [],
        });
        for (key, field) in value.as_object().unwrap() {
            full[key] = field.clone();
        }
        deserialize(registration, &full, registry, false, "").map(|(_, coerced)| coerced)
    }

    fn rules(coerced: &[Coercion]) -> Vec<(&str, &str)> {
        coerced.iter().map(|c| (c.at.as_str(), c.rule)).collect()
    }

    #[test]
    fn each_rule_fixes_its_mistake() {
        let registry = registry();
        for (value, expected) in [
            (json!({ "unit": null }), vec![("unit", "null_unit_struct")]),
            (json!({ "bias": "3.0" }), vec![("bias", "numeric_string")]),
            (
                json!({ "fill": "\"FieldOnly\"" }),
                vec![("fill", "quoted_string")],
            ),
            (
                json!({ "fill": "field_only" }),
                vec![("fill", "variant_case")],
            ),
            (
                json!({ "spin": [[0, 1, 0]] }),
                vec![("spin", "newtype_unwrap")],
            ),
            (
                json!({ "turn": { "x": 0, "y": 0, "z": 0, "w": 1 } }),
                vec![("turn", "object_to_array")],
            ),
        ] {
            let coerced = read(&registry, value.clone()).unwrap();
            assert_eq!(rules(&coerced), expected, "{value}");
        }
        let coerced = read(
            &registry,
            json!({ "turn": { "x": 1, "y": 2, "z": 3, "w": 4 } }),
        )
        .unwrap();
        assert_eq!(coerced[0].to, json!([1, 2, 3, 4]));
    }

    #[test]
    fn a_valid_value_is_never_rewritten() {
        let registry = registry();
        assert_eq!(read(&registry, json!({})).unwrap(), Vec::new());
        // `[[]]` is a valid `Rows`; only the failing sibling is fixed.
        let coerced = read(&registry, json!({ "rows": [[]], "bias": "3" })).unwrap();
        assert_eq!(rules(&coerced), [("bias", "numeric_string")]);
    }

    #[test]
    fn ambiguous_or_unsafe_fixes_are_not_made() {
        let registry = registry();
        let err = read(&registry, json!({ "clash": "foo_bar" })).unwrap_err();
        assert_eq!(err.at, "clash");
        let err = read(&registry, json!({ "span": { "a": 1, "b": 2 } })).unwrap_err();
        assert_eq!(err.at, "span");
        // A list item missing a field would panic the game when pushed.
        let err = read(&registry, json!({ "marks": [{ "a": 1.0 }] })).unwrap_err();
        assert_eq!(err.at, "marks[0]");
    }

    #[test]
    fn an_error_names_the_deepest_field_its_shape_and_an_example() {
        let registry = registry();
        let err = read(&registry, json!({ "bias": "abc" })).unwrap_err();
        assert_eq!(err.at, "bias");
        assert_eq!(err.expected, "a number");
        assert_eq!(err.example, Some(json!(0.0)));
        assert!(
            err.sentence("Light")
                .starts_with("`Light` is invalid at `bias`: "),
            "{}",
            err.sentence("Light")
        );
    }
}
