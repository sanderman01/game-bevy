//! `game.components.*` and `game.resources.*`: reading and writing values the way an agent writes
//! them.
//!
//! Entities by `name` or `entity`, types by short name or path, values coerced when a mistake has
//! one fix, and failures that name the field, its shape and an example. Upstream's `world.*`
//! methods stay as they are for raw clients; only `world.mutate_components` is replaced, by
//! [`guarded_mutate_components`], because upstream panics the game on a despawned entity and on an
//! immutable component.

use bevy::{
    ecs::{component::ComponentId, reflect::ReflectComponent},
    prelude::*,
    reflect::{GetPath, PartialReflect, TypeRegistry, serde::TypedReflectSerializer},
    remote::{
        BrpError, BrpResult,
        builtin_methods::{parse_some, process_remote_mutate_components_request},
    },
};
use ename_engine::{
    bigspace::grid::{is_position_half, root_grid, set_absolute_position},
    command::{CommandError, EntityTarget, ResolvedType, TypeSet, resolve_type},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    coerce::{self, Coercion, ValueError},
    command::brp_error,
    value_shape,
};

pub const LIST_METHOD: &str = "game.components.list";
pub const GET_METHOD: &str = "game.components.get";
pub const INSERT_METHOD: &str = "game.components.insert";
pub const MUTATE_METHOD: &str = "game.components.mutate";
pub const REMOVE_METHOD: &str = "game.components.remove";
pub const RESOURCES_GET_METHOD: &str = "game.resources.get";
pub const RESOURCES_MUTATE_METHOD: &str = "game.resources.mutate";
pub const SPAWN_METHOD: &str = "game.entities.spawn";
pub const BUILTIN_MUTATE_METHOD: &str = bevy::remote::builtin_methods::BRP_MUTATE_COMPONENTS_METHOD;

/// A value bigger than this, serialized, is replaced by its field names.
const DEFAULT_MAX_BYTES: usize = 8000;

#[derive(Serialize)]
struct Identity {
    entity: u64,
    name: Option<String>,
}

fn identity(world: &World, entity: Entity) -> Identity {
    Identity {
        entity: entity.to_bits(),
        name: world.get::<Name>(entity).map(|n| n.as_str().to_owned()),
    }
}

fn params<T: for<'de> Deserialize<'de>>(
    params: Option<Value>,
    world: &World,
) -> Result<T, BrpError> {
    serde_json::from_value(params.unwrap_or(Value::Null)).map_err(|err| {
        brp_error(
            CommandError::invalid("invalid_params", format!("invalid params: {err}")),
            world,
        )
    })
}

fn target(target: &EntityTarget, world: &mut World) -> Result<Entity, BrpError> {
    target.resolve(world).map_err(|err| brp_error(err, world))
}

fn resolve(
    world: &World,
    set: TypeSet,
    entity: Option<Entity>,
    name: &str,
) -> Result<ResolvedType, BrpError> {
    resolve_type(world, set, entity, name).map_err(|err| brp_error(err, world))
}

fn reflected(resolved: ResolvedType, world: &World) -> Result<ResolvedType, BrpError> {
    resolved
        .require_reflected()
        .map_err(|err| brp_error(err, world))
}

/// A value the type does not accept, with where, what shape and an example in `data`.
fn value_error(err: &ValueError, owner: &str, world: &World) -> BrpError {
    let short = owner.rsplit("::").next().unwrap_or(owner);
    let mut error = brp_error(
        CommandError::invalid("value_shape", err.sentence(owner)).with_hint(format!(
            "`registry_schema` with `{short}` shows how to write it."
        )),
        world,
    );
    if let (Some(Value::Object(data)), Value::Object(extra)) = (&mut error.data, err.data()) {
        data.extend(extra);
    }
    error
}

fn failed(kind: &'static str, message: String, world: &World) -> BrpError {
    brp_error(CommandError::failed(kind, message), world)
}

/// Does the entity have this component right now?
fn has(world: &World, entity: Entity, resolved: &ResolvedType) -> bool {
    resolved
        .component_id
        .is_some_and(|id| world.get_entity(entity).is_ok_and(|e| e.contains_id(id)))
}

// -------------------------------------------------------------------------------------------
// list

#[derive(Deserialize)]
struct ListParams {
    entities: Vec<EntityTarget>,
}

#[derive(Serialize)]
struct EntityComponents {
    #[serde(flatten)]
    identity: Identity,
    /// Reflected: readable and writable.
    components: Vec<String>,
    /// Present but invisible to reflection: can only be removed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    not_reflected: Vec<String>,
}

pub(crate) fn list(In(raw): In<Option<Value>>, world: &mut World) -> BrpResult {
    let ListParams { entities } = params(raw, world)?;
    let registry = world.resource::<AppTypeRegistry>().clone();
    let mut answer = Vec::new();
    for selector in &entities {
        let entity = target(selector, world)?;
        let registry = registry.read();
        let mut components = Vec::new();
        let mut not_reflected = Vec::new();
        for &id in world.entity(entity).archetype().components().iter() {
            let Some(info) = world.components().get_info(id) else {
                continue;
            };
            match info.type_id().and_then(|type_id| registry.get(type_id)) {
                Some(r) if r.data::<ReflectComponent>().is_some() => {
                    components.push(r.type_info().type_path().to_owned());
                }
                Some(r) => not_reflected.push(r.type_info().type_path().to_owned()),
                None => not_reflected.push(info.name().to_string()),
            }
        }
        components.sort();
        not_reflected.sort();
        answer.push(EntityComponents {
            identity: identity(world, entity),
            components,
            not_reflected,
        });
    }
    crate::to_value(json!({ "entities": answer }))
}

// -------------------------------------------------------------------------------------------
// get

#[derive(Deserialize)]
struct GetParams {
    #[serde(flatten)]
    target: EntityTarget,
    /// `Type` or `Type.field.path`.
    components: Vec<String>,
    #[serde(default)]
    max_bytes: Option<usize>,
}

#[derive(Serialize, Default)]
struct Values {
    components: Map<String, Value>,
    #[serde(skip_serializing_if = "Map::is_empty")]
    unreadable: Map<String, Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    absent: Vec<String>,
    /// Asked name to full path, for every name that was not already the full path.
    #[serde(skip_serializing_if = "Map::is_empty")]
    resolved: Map<String, Value>,
}

/// `Type.field` into the type and the reflect path. Type paths contain no `.` outside `<...>`.
fn split_field(asked: &str) -> (&str, Option<&str>) {
    let mut depth = 0i32;
    for (i, c) in asked.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            '.' if depth == 0 => return (&asked[..i], Some(&asked[i + 1..])),
            _ => {}
        }
    }
    (asked, None)
}

fn unreadable(err: &CommandError) -> Value {
    json!({ "kind": err.kind, "message": err.message, "hint": err.hint })
}

/// Reads the asked values from `holder` into `values`.
fn read_into(
    world: &World,
    holder: Entity,
    set: TypeSet,
    asked: &[String],
    max_bytes: usize,
    values: &mut Values,
) {
    let registry = world.resource::<AppTypeRegistry>().read();
    let entity_hint = (set == TypeSet::Components).then_some(holder);
    for asked in asked {
        let (type_name, field) = split_field(asked);
        let resolved = match resolve_type(world, set, entity_hint, type_name)
            .and_then(ResolvedType::require_reflected)
        {
            Ok(resolved) => resolved,
            Err(err) => {
                values.unreadable.insert(asked.clone(), unreadable(&err));
                continue;
            }
        };
        let key = match field {
            Some(field) => format!("{}.{field}", resolved.path),
            None => resolved.path.clone(),
        };
        if key != *asked {
            values
                .resolved
                .insert(asked.clone(), Value::from(key.clone()));
        }
        if !has(world, holder, &resolved) {
            values.absent.push(asked.clone());
            continue;
        }
        match read_one(world, holder, &resolved, field, &registry) {
            Ok(value) => {
                values.components.insert(key, trim(value, max_bytes));
            }
            Err(err) => {
                values.unreadable.insert(key, unreadable(&err));
            }
        }
    }
}

fn read_one(
    world: &World,
    holder: Entity,
    resolved: &ResolvedType,
    field: Option<&str>,
    registry: &TypeRegistry,
) -> Result<Value, CommandError> {
    let reflect_component = resolved
        .type_id
        .and_then(|id| registry.get(id))
        .and_then(|r| r.data::<ReflectComponent>())
        .ok_or_else(|| CommandError::failed("not_reflected", "no reflection data"))?;
    let reflected = reflect_component
        .reflect(world.entity(holder))
        .ok_or_else(|| CommandError::failed("component_absent", "not present"))?;
    let value: &dyn PartialReflect = match field {
        None => reflected.as_partial_reflect(),
        Some(field) => reflected.reflect_path(field).map_err(|err| {
            CommandError::invalid(
                "bad_path",
                format!("`{}` has no such path: {err}", resolved.path),
            )
        })?,
    };
    serde_json::to_value(TypedReflectSerializer::new(value, registry)).map_err(|err| {
        CommandError::failed("not_serializable", err.to_string())
            .with_hint("Usually an asset handle, which reflection cannot serialize.")
    })
}

/// A value over `max_bytes` becomes its size and its top-level field names, to read by path.
fn trim(value: Value, max_bytes: usize) -> Value {
    let size = value.to_string().len();
    if size <= max_bytes {
        return value;
    }
    let fields: Vec<String> = match &value {
        Value::Object(object) => object.keys().cloned().collect(),
        Value::Array(items) => (0..items.len()).map(|i| format!("[{i}]")).collect(),
        _ => Vec::new(),
    };
    json!({ "omitted_bytes": size, "fields": fields })
}

pub(crate) fn get(In(raw): In<Option<Value>>, world: &mut World) -> BrpResult {
    let GetParams {
        target: selector,
        components,
        max_bytes,
    } = params(raw, world)?;
    let entity = target(&selector, world)?;
    let mut values = Values::default();
    read_into(
        world,
        entity,
        TypeSet::Components,
        &components,
        max_bytes.unwrap_or(DEFAULT_MAX_BYTES),
        &mut values,
    );
    let mut answer = serde_json::to_value(values).map_err(BrpError::internal)?;
    if let (Value::Object(answer), Ok(Value::Object(id))) =
        (&mut answer, serde_json::to_value(identity(world, entity)))
    {
        answer.extend(id);
    }
    Ok(answer)
}

// -------------------------------------------------------------------------------------------
// insert and spawn

#[derive(Deserialize)]
struct InsertParams {
    #[serde(flatten)]
    target: EntityTarget,
    components: Map<String, Value>,
    #[serde(default)]
    strict: bool,
}

/// Component values read and built, ready to insert. Built before anything is inserted, so a
/// bad value changes nothing.
struct Built {
    path: String,
    value: Box<dyn Reflect>,
}

fn build(
    world: &World,
    entity: Option<Entity>,
    components: &Map<String, Value>,
    strict: bool,
    coerced: &mut Vec<Coercion>,
) -> Result<Vec<Built>, BrpError> {
    let registry = world.resource::<AppTypeRegistry>().read();
    let mut built = Vec::new();
    for (name, value) in components {
        let resolved = reflected(resolve(world, TypeSet::Components, entity, name)?, world)?;
        let registration = resolved
            .type_id
            .and_then(|id| registry.get(id))
            .expect("a reflected type has a registration");
        let short = registration.type_info().type_path_table().short_path();
        let (read, fixes) = coerce::deserialize(registration, value, &registry, strict, "")
            .map_err(|err| value_error(&err, &resolved.path, world))?;
        coerced.extend(fixes.into_iter().map(|mut fix| {
            fix.at = if fix.at.is_empty() {
                short.to_owned()
            } else {
                format!("{short}.{}", fix.at)
            };
            fix
        }));
        let not_built = |at: String, message: String| {
            value_error(
                &ValueError {
                    at,
                    message,
                    expected_type: resolved.path.clone(),
                    expected: value_shape::hint(registration, &registry, None),
                    example: value_shape::example(registration, &registry).map(|(v, _)| v),
                },
                &resolved.path,
                world,
            )
        };
        let value = value_shape::materialize(registration, read.as_ref())
            .map_err(|message| not_built(String::new(), message))?;
        // Building falls back to a default for a field it cannot build, which would drop part
        // of the value without a word. What was read must survive into what was built.
        let sent = serde_json::to_value(TypedReflectSerializer::new(read.as_ref(), &registry));
        let kept = serde_json::to_value(TypedReflectSerializer::new(
            value.as_partial_reflect(),
            &registry,
        ));
        if let (Ok(sent), Ok(kept)) = (sent, kept)
            && let Some(at) = dropped(&sent, &kept, "")
        {
            return Err(not_built(
                at,
                "this part could not be built and would be replaced by its default; give every \
                 field"
                    .to_owned(),
            ));
        }
        built.push(Built {
            path: resolved.path,
            value,
        });
    }
    Ok(built)
}

/// The first path where `kept` lost or changed something `sent` has. A field `kept` does not
/// serialize at all is skipped by serde and cannot be checked.
fn dropped(sent: &Value, kept: &Value, at: &str) -> Option<String> {
    let join = |key: &str| {
        if at.is_empty() {
            key.to_owned()
        } else {
            format!("{at}.{key}")
        }
    };
    match (sent, kept) {
        (Value::Object(sent), Value::Object(kept)) => sent
            .iter()
            .find_map(|(key, item)| dropped(item, kept.get(key)?, &join(key))),
        (Value::Array(sent), Value::Array(kept)) => {
            if sent.len() != kept.len() {
                return Some(at.to_owned());
            }
            sent.iter()
                .zip(kept)
                .enumerate()
                .find_map(|(i, (a, b))| dropped(a, b, &format!("{at}[{i}]")))
        }
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64()?, b.as_f64()?);
            ((a - b).abs() > 1e-6 * a.abs().max(b.abs()).max(1.0)).then(|| at.to_owned())
        }
        (a, b) => (a != b).then(|| at.to_owned()),
    }
}

fn insert_built(world: &mut World, entity: Entity, built: Vec<Built>) -> Vec<String> {
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    let mut entity_mut = world.entity_mut(entity);
    built
        .into_iter()
        .map(|Built { path, value }| {
            let reflect_component = registry
                .get(value.reflect_type_info().type_id())
                .and_then(|r| r.data::<ReflectComponent>())
                .expect("built from a reflected registration");
            reflect_component.insert(&mut entity_mut, value.as_partial_reflect(), &registry);
            path
        })
        .collect()
}

pub(crate) fn insert(In(raw): In<Option<Value>>, world: &mut World) -> BrpResult {
    let InsertParams {
        target: selector,
        components,
        strict,
    } = params(raw, world)?;
    let entity = target(&selector, world)?;
    let mut coerced = Vec::new();
    let built = build(world, Some(entity), &components, strict, &mut coerced)?;
    let inserted = insert_built(world, entity, built);
    crate::to_value(json!({
        "entity": entity.to_bits(),
        "name": identity(world, entity).name,
        "inserted": inserted,
        "coerced": coerced,
    }))
}

#[derive(Deserialize)]
struct SpawnParams {
    name: String,
    #[serde(default)]
    components: Map<String, Value>,
    #[serde(default)]
    parent: Option<EntityTarget>,
    /// Absolute metres. Without `parent`, the entity hangs under the root grid.
    #[serde(default)]
    position: Option<[f64; 3]>,
    #[serde(default)]
    strict: bool,
}

pub(crate) fn spawn(In(raw): In<Option<Value>>, world: &mut World) -> BrpResult {
    let SpawnParams {
        name,
        components,
        parent,
        position,
        strict,
    } = params(raw, world)?;
    let mut coerced = Vec::new();
    let built = build(world, None, &components, strict, &mut coerced)?;
    let parent = match (&parent, position) {
        (Some(selector), _) => Some(target(selector, world)?),
        (None, Some(_)) => Some(
            root_grid(world)
                .map_err(|message| brp_error(CommandError::failed("no_grid", message), world))?,
        ),
        (None, None) => None,
    };
    let entity = world.spawn(Name::new(name.clone())).id();
    insert_built(world, entity, built);
    if let Some(parent) = parent {
        world.entity_mut(entity).insert(ChildOf(parent));
    }
    let placed = match position {
        Some(position) => Some(
            set_absolute_position(world, entity, position.into())
                .map_err(|err| failed("not_on_grid", err.to_string(), world))?
                .position
                .to_array(),
        ),
        None => None,
    };
    crate::to_value(json!({
        "entity": entity.to_bits(),
        "name": name,
        "position": placed,
        "coerced": coerced,
    }))
}

// -------------------------------------------------------------------------------------------
// mutate

#[derive(Deserialize)]
struct MutateParams {
    #[serde(flatten)]
    target: EntityTarget,
    component: String,
    #[serde(default)]
    path: String,
    value: Value,
    #[serde(default)]
    strict: bool,
}

/// Sets `path` in the reflected value held by `holder`. An immutable component with an empty path
/// is replaced whole; with a path it is refused, since reflection would panic writing it.
fn mutate_on(
    world: &mut World,
    holder: Entity,
    resolved: &ResolvedType,
    path: &str,
    value: &Value,
    strict: bool,
) -> Result<Vec<Coercion>, BrpError> {
    let mutable = resolved
        .component_id
        .and_then(|id| world.components().get_info(id))
        .is_none_or(|info| info.mutable());
    if !mutable {
        if !path.is_empty() {
            return Err(brp_error(
                CommandError::invalid(
                    "immutable_component",
                    format!(
                        "`{}` is immutable: its fields cannot be set one by one",
                        resolved.path
                    ),
                )
                .with_hint(
                    "Write the whole component with `path` empty, or with world_insert_component.",
                ),
                world,
            ));
        }
        let mut coerced = Vec::new();
        let one = Map::from_iter([(resolved.path.clone(), value.clone())]);
        let built = build(world, Some(holder), &one, strict, &mut coerced)?;
        insert_built(world, holder, built);
        return Ok(coerced);
    }
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    let registration = resolved
        .type_id
        .and_then(|id| registry.get(id))
        .expect("a reflected type has a registration");
    let short = registration.type_info().type_path_table().short_path();
    let report = |mut fixes: Vec<Coercion>| {
        for fix in &mut fixes {
            fix.at = [short, &fix.at]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(".");
        }
        fixes
    };
    let reflect_component = registration
        .data::<ReflectComponent>()
        .expect("a reflected type has ReflectComponent");
    let field_type = {
        let entity = world.entity(holder);
        let current = reflect_component.reflect(entity).ok_or_else(|| {
            failed(
                "component_absent",
                format!("`{}` is not present", resolved.path),
                world,
            )
        })?;
        let field = current.reflect_path(path).map_err(|err| {
            brp_error(
                CommandError::invalid(
                    "bad_path",
                    format!("`{}` has no path `{path}`: {err}", resolved.path),
                ),
                world,
            )
        })?;
        field
            .get_represented_type_info()
            .map(|info| info.type_id())
            .and_then(|id| registry.get(id))
            .ok_or_else(|| {
                failed(
                    "not_reflected",
                    format!("the field type at `{path}` is not registered"),
                    world,
                )
            })?
    };
    let base = path.trim_start_matches('.');
    let (read, fixes) = coerce::deserialize(field_type, value, &registry, strict, base)
        .map_err(|err| value_error(&err, &resolved.path, world))?;
    let mut current = reflect_component
        .reflect_mut(world.entity_mut(holder))
        .expect("checked present above");
    current
        .reflect_path_mut(path)
        .map_err(|err| BrpError::internal(err.to_string()))?
        .try_apply(read.as_ref())
        .map_err(|err| {
            failed(
                "apply_failed",
                format!(
                    "`{}` did not take the value at `{path}`: {err}",
                    resolved.path
                ),
                world,
            )
        })?;
    Ok(report(fixes))
}

pub(crate) fn mutate(In(raw): In<Option<Value>>, world: &mut World) -> BrpResult {
    let MutateParams {
        target: selector,
        component,
        path,
        value,
        strict,
    } = params(raw, world)?;
    let entity = target(&selector, world)?;
    let resolved = reflected(
        resolve(world, TypeSet::Components, Some(entity), &component)?,
        world,
    )?;
    if let Some(type_id) = resolved.type_id
        && is_position_half(type_id, &path)
    {
        return Err(brp_error(
            CommandError::invalid(
                "position_write",
                "position is a grid cell plus an offset from a moving origin, so writing either \
                 half alone lands somewhere else",
            )
            .with_hint("Use world_set_position with absolute metres."),
            world,
        ));
    }
    if !has(world, entity, &resolved) {
        return Err(failed(
            "component_absent",
            format!("the entity has no `{}`", resolved.path),
            world,
        ));
    }
    let coerced = mutate_on(world, entity, &resolved, &path, &value, strict)?;
    crate::to_value(json!({
        "entity": entity.to_bits(),
        "name": identity(world, entity).name,
        "component": resolved.path,
        "coerced": coerced,
    }))
}

// -------------------------------------------------------------------------------------------
// remove

#[derive(Deserialize)]
struct RemoveParams {
    #[serde(flatten)]
    target: EntityTarget,
    components: Vec<String>,
}

pub(crate) fn remove(In(raw): In<Option<Value>>, world: &mut World) -> BrpResult {
    let RemoveParams {
        target: selector,
        components,
    } = params(raw, world)?;
    let entity = target(&selector, world)?;
    let mut ids: Vec<(String, ComponentId)> = Vec::new();
    let mut absent = Vec::new();
    // Resolved first, so an unknown name removes nothing.
    for name in &components {
        let resolved = resolve(world, TypeSet::Components, Some(entity), name)?;
        match resolved
            .component_id
            .filter(|_| has(world, entity, &resolved))
        {
            Some(id) => ids.push((resolved.path, id)),
            None => absent.push(name.clone()),
        }
    }
    let mut entity_mut = world.entity_mut(entity);
    for (_, id) in &ids {
        entity_mut.remove_by_id(*id);
    }
    crate::to_value(json!({
        "entity": entity.to_bits(),
        "name": identity(world, entity).name,
        "removed": ids.into_iter().map(|(path, _)| path).collect::<Vec<_>>(),
        "absent": absent,
    }))
}

// -------------------------------------------------------------------------------------------
// resources: a resource is a component on an entity of its own

#[derive(Deserialize)]
struct ResourcesGetParams {
    resources: Vec<String>,
}

#[derive(Deserialize)]
struct ResourceMutateParams {
    resource: String,
    #[serde(default)]
    path: String,
    value: Value,
    #[serde(default)]
    strict: bool,
}

fn resource_entity(world: &World, resolved: &ResolvedType) -> Option<Entity> {
    resolved
        .component_id
        .and_then(|id| world.resource_entities().get(id))
}

pub(crate) fn resources_get(In(raw): In<Option<Value>>, world: &mut World) -> BrpResult {
    let ResourcesGetParams { resources } = params(raw, world)?;
    let mut values = Values::default();
    for asked in &resources {
        let (type_name, _) = split_field(asked);
        let holder = resolve_type(world, TypeSet::Resources, None, type_name)
            .ok()
            .and_then(|resolved| resource_entity(world, &resolved));
        match holder {
            Some(holder) => read_into(
                world,
                holder,
                TypeSet::Resources,
                std::slice::from_ref(asked),
                DEFAULT_MAX_BYTES,
                &mut values,
            ),
            None => match resolve_type(world, TypeSet::Resources, None, type_name) {
                Ok(_) => values.absent.push(asked.clone()),
                Err(err) => {
                    values.unreadable.insert(asked.clone(), unreadable(&err));
                }
            },
        }
    }
    let mut answer = serde_json::to_value(values).map_err(BrpError::internal)?;
    if let Value::Object(answer) = &mut answer
        && let Some(found) = answer.remove("components")
    {
        answer.insert("resources".into(), found);
    }
    Ok(answer)
}

pub(crate) fn resources_mutate(In(raw): In<Option<Value>>, world: &mut World) -> BrpResult {
    let ResourceMutateParams {
        resource,
        path,
        value,
        strict,
    } = params(raw, world)?;
    let resolved = reflected(resolve(world, TypeSet::Resources, None, &resource)?, world)?;
    let Some(holder) = resource_entity(world, &resolved) else {
        return Err(failed(
            "resource_absent",
            format!("`{}` is not inserted in this world", resolved.path),
            world,
        ));
    };
    let coerced = mutate_on(world, holder, &resolved, &path, &value, strict)?;
    crate::to_value(json!({ "resource": resolved.path, "coerced": coerced }))
}

// -------------------------------------------------------------------------------------------
// the built-in, guarded

#[derive(Deserialize)]
struct BuiltinTarget {
    entity: Entity,
    component: String,
}

/// Upstream `world.mutate_components`, minus its two panics: a despawned entity
/// (`World::entity_mut` unchecked, `bevy_remote` 0.19.1 `builtin_methods.rs:1194`) and an
/// immutable component (`ReflectComponent::reflect_mut`). Raw clients keep upstream's contract.
pub(crate) fn guarded_mutate_components(
    In(raw): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let BuiltinTarget { entity, component } = parse_some(raw.clone())?;
    if world.get_entity(entity).is_err() {
        return Err(BrpError::entity_not_found(entity));
    }
    let immutable = {
        let registry = world.resource::<AppTypeRegistry>().read();
        registry
            .get_with_type_path(&component)
            .and_then(|r| world.components().get_id(r.type_id()))
            .and_then(|id| world.components().get_info(id))
            .is_some_and(|info| !info.mutable())
    };
    if immutable {
        return Err(BrpError::component_error(format!(
            "`{component}` is immutable and cannot be mutated through reflection; insert it whole"
        )));
    }
    process_remote_mutate_components_request(In(raw), world)
}
