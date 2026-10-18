//! `debug.list`, `debug.get`, `debug.set` and `debug.reset`. Params are flat (a category, a list
//! of names, a name-to-value map), so a console line maps onto them directly.

use std::collections::BTreeMap;

use bevy::prelude::*;
use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::registry::{
    DebugSwitchError, DebugSwitchRegistry, DebugSwitchTarget, DebugSwitchValue,
    DebugSwitchWorldExt, Entry, check_value,
};
use crate::command::{Call, CommandAppExt, CommandError, CommandScope, CommandSpec};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListParams {
    /// Only the debug switches in this category. Omitted: all of them.
    #[serde(default)]
    category: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetParams {
    /// Debug switch names, such as `overlay.physics`. Returned in this order.
    names: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetParams {
    /// Debug switch name to value: `true`/`false` for a bool, a snake_case variant name for a
    /// mode. Every entry is checked before any is written.
    values: BTreeMap<String, DebugSwitchValue>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResetParams {
    /// Debug switches to restore to their launch values. Omitted: all of them.
    #[serde(default)]
    names: Option<Vec<String>>,
}

#[derive(Serialize, JsonSchema)]
struct Listed {
    /// Which chord of each switch is live in this build: "editor" or "release".
    target: &'static str,
    /// Every category, sorted.
    categories: Vec<&'static str>,
    switches: Vec<SwitchInfo>,
}

#[derive(Serialize, JsonSchema)]
struct Switches {
    switches: Vec<SwitchInfo>,
}

#[derive(Serialize, JsonSchema)]
struct Changed {
    /// Only the switches whose value changed.
    changed: Vec<Change>,
    switches: Vec<SwitchInfo>,
}

#[derive(Serialize, JsonSchema)]
struct Change {
    name: &'static str,
    from: DebugSwitchValue,
    to: DebugSwitchValue,
}

#[derive(Serialize, JsonSchema)]
struct SwitchInfo {
    name: &'static str,
    category: &'static str,
    /// `null` when the switch does not apply now (no editor camera yet, say).
    value: Option<DebugSwitchValue>,
    allowed: Vec<DebugSwitchValue>,
    /// What `debug.reset` restores. `null` until the switch has applied once.
    launch_value: Option<DebugSwitchValue>,
    keys: KeysInfo,
    description: &'static str,
    /// Type path of the resource behind it; `null` for a switch over callbacks.
    resource: Option<&'static str>,
    /// Field path inside the resource; `null` when the whole resource is the value.
    path: Option<&'static str>,
}

#[derive(Serialize, JsonSchema)]
struct KeysInfo {
    /// Chord in editor builds, such as `Ctrl+F4`.
    editor: Option<String>,
    /// Chord in release builds.
    release: Option<String>,
}

pub(super) fn register(app: &mut App) {
    let registry = app.world().resource::<DebugSwitchRegistry>();
    let names: Vec<String> = registry.names().map(str::to_owned).collect();
    let categories: Vec<String> = registry
        .categories()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let (get_names, set_names, reset_names) = (names.clone(), names.clone(), names);
    app.register_command(
        CommandSpec::new(
            "debug.list",
            "List debug switches (all, or one category) with their values, allowed values and \
             keys.",
        )
        .scope(CommandScope::Safe)
        .read_only()
        .adjust_params_schema(move |schema| restrict(schema, "category", &categories)),
        list,
    )
    .register_command(
        CommandSpec::new("debug.get", "Read named debug switches.")
            .scope(CommandScope::Safe)
            .read_only()
            .adjust_params_schema(move |schema| restrict_items(schema, "names", &get_names)),
        get,
    )
    .register_command(
        CommandSpec::new(
            "debug.set",
            "Set debug switches by name; every value is checked before any is written.",
        )
        .scope(CommandScope::Safe)
        .adjust_params_schema(move |schema| restrict_keys(schema, "values", &set_names)),
        set,
    )
    .register_command(
        CommandSpec::new(
            "debug.reset",
            "Restore debug switches (all, or the named ones) to their launch values.",
        )
        .scope(CommandScope::Safe)
        .adjust_params_schema(move |schema| restrict_items(schema, "names", &reset_names)),
        reset,
    );
}

fn property<'a>(
    schema: &'a mut Schema,
    name: &str,
) -> Option<&'a mut serde_json::Map<String, Value>> {
    schema
        .get_mut("properties")
        .and_then(|properties| properties.get_mut(name))
        .and_then(Value::as_object_mut)
}

/// With nothing registered the schema is left alone: an empty enum would forbid every value.
fn restrict(schema: &mut Schema, name: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    if let Some(property) = property(schema, name) {
        let mut allowed: Vec<Value> = values.iter().cloned().map(Value::from).collect();
        // An `Option` property also takes an explicit `null`.
        if property
            .get("type")
            .and_then(Value::as_array)
            .is_some_and(|types| types.contains(&Value::from("null")))
        {
            allowed.push(Value::Null);
        }
        property.insert("enum".into(), Value::from(allowed));
    }
}

fn restrict_items(schema: &mut Schema, name: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    if let Some(property) = property(schema, name) {
        property.insert(
            "items".into(),
            serde_json::json!({"type": "string", "enum": values}),
        );
    }
}

fn restrict_keys(schema: &mut Schema, name: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    if let Some(property) = property(schema, name) {
        property.insert("propertyNames".into(), serde_json::json!({"enum": values}));
    }
}

fn info(world: &World, entry: &Entry) -> SwitchInfo {
    let registry = world.resource::<DebugSwitchRegistry>();
    SwitchInfo {
        name: entry.name,
        category: entry.category,
        value: world.debug_switch(entry.name).ok().flatten(),
        allowed: entry.kind.allowed(),
        launch_value: registry.launch_value(entry.name).cloned(),
        keys: KeysInfo {
            editor: entry.keys.editor.map(|chord| chord.to_string()),
            release: entry.keys.release.map(|chord| chord.to_string()),
        },
        description: entry.description,
        resource: entry.resource,
        path: entry.path,
    }
}

fn infos<'a>(world: &World, names: impl IntoIterator<Item = &'a str>) -> Vec<SwitchInfo> {
    let registry = world.resource::<DebugSwitchRegistry>();
    names
        .into_iter()
        .filter_map(|name| registry.get(name))
        .map(|entry| info(world, entry))
        .collect()
}

fn list(In(call): In<Call<ListParams>>, world: &mut World) -> Result<Listed, CommandError> {
    let registry = world.resource::<DebugSwitchRegistry>();
    let categories = registry.categories();
    if let Some(category) = &call.params.category
        && !categories.contains(&category.as_str())
    {
        return Err(CommandError::invalid(
            "unknown_category",
            format!("no debug switch has the category `{category}`"),
        )
        .with_hint(format!("Categories: {}.", categories.join(", "))));
    }
    let switches = registry
        .iter()
        .filter(|entry| {
            call.params
                .category
                .as_deref()
                .is_none_or(|category| entry.category == category)
        })
        .map(|entry| info(world, entry))
        .collect();
    let target = match world.resource::<DebugSwitchTarget>() {
        DebugSwitchTarget::Editor => "editor",
        DebugSwitchTarget::Release => "release",
    };
    Ok(Listed {
        target,
        categories,
        switches,
    })
}

fn get(In(call): In<Call<GetParams>>, world: &mut World) -> Result<Switches, CommandError> {
    if call.params.names.is_empty() {
        return Err(CommandError::invalid("no_names", "`names` is empty"));
    }
    for name in &call.params.names {
        known(world, name)?;
    }
    Ok(Switches {
        switches: infos(world, call.params.names.iter().map(String::as_str)),
    })
}

fn set(In(call): In<Call<SetParams>>, world: &mut World) -> Result<Changed, CommandError> {
    if call.params.values.is_empty() {
        return Err(CommandError::invalid("no_values", "`values` is empty"));
    }
    apply(world, call.params.values.into_iter().collect())
}

fn reset(In(call): In<Call<ResetParams>>, world: &mut World) -> Result<Changed, CommandError> {
    let names: Vec<String> = match call.params.names {
        Some(names) if names.is_empty() => {
            return Err(CommandError::invalid("no_names", "`names` is empty"));
        }
        Some(names) => names,
        None => {
            let registry = world.resource::<DebugSwitchRegistry>();
            // Switches that never applied, or do not apply now, are skipped; only named ones fail.
            registry
                .names()
                .filter(|name| {
                    registry.launch_value(name).is_some()
                        && matches!(world.debug_switch(name), Ok(Some(_)))
                })
                .map(str::to_owned)
                .collect()
        }
    };
    let mut values = Vec::with_capacity(names.len());
    for name in names {
        known(world, &name)?;
        let Some(launch) = world
            .resource::<DebugSwitchRegistry>()
            .launch_value(&name)
            .cloned()
        else {
            return Err(unavailable(&name));
        };
        values.push((name, launch));
    }
    apply(world, values)
}

/// Checks every entry, then writes them in order.
fn apply(
    world: &mut World,
    values: Vec<(String, DebugSwitchValue)>,
) -> Result<Changed, CommandError> {
    for (name, value) in &values {
        let entry = known(world, name)?;
        check_value(entry, value).map_err(command_error)?;
        if world.debug_switch(name).map_err(command_error)?.is_none() {
            return Err(unavailable(name));
        }
    }
    let mut changed = Vec::new();
    for (name, value) in &values {
        if let Some(from) = world
            .set_debug_switch(name, value.clone(), "command")
            .map_err(command_error)?
        {
            let name = world
                .resource::<DebugSwitchRegistry>()
                .get(name)
                .map_or("", |entry| entry.name);
            changed.push(Change {
                name,
                from,
                to: value.clone(),
            });
        }
    }
    Ok(Changed {
        changed,
        switches: infos(world, values.iter().map(|(name, _)| name.as_str())),
    })
}

fn known<'w>(world: &'w World, name: &str) -> Result<&'w Entry, CommandError> {
    let registry = world.resource::<DebugSwitchRegistry>();
    registry
        .get(name)
        .ok_or_else(|| command_error(DebugSwitchError::Unknown(name.to_owned())))
        .map_err(|err| {
            let closest = registry.closest(name);
            let hint = if closest.is_empty() {
                "`debug.list` lists every debug switch.".to_owned()
            } else {
                format!(
                    "Did you mean {}? `debug.list` lists every debug switch.",
                    closest
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            err.with_hint(hint)
        })
}

fn unavailable(name: &str) -> CommandError {
    command_error(DebugSwitchError::Unavailable(name.to_owned()))
}

fn command_error(err: DebugSwitchError) -> CommandError {
    let kind = match &err {
        DebugSwitchError::Unknown(_) => "unknown_switch",
        DebugSwitchError::InvalidValue { .. } => "invalid_value",
        DebugSwitchError::Unavailable(_) => {
            return CommandError::failed("switch_unavailable", err.to_string()).with_hint(
                "It does not apply now, for example before the editor camera has a grid. \
                 `debug.list` shows `value: null` for such switches.",
            );
        }
    };
    CommandError::invalid(kind, err.to_string())
}
