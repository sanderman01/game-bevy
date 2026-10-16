//! `wait.until` conditions on reflected state: a component, a resource, or an absolute position.
//!
//! Values are serialized with the type registry to the same JSON `world_get_components` returns,
//! so the `value` an agent compares with has the shape it already reads, and every type goes
//! through one comparison. Paths use `world_mutate_component`'s syntax.

use std::any::TypeId;

use bevy::{
    ecs::reflect::ReflectComponent,
    prelude::*,
    reflect::{
        GetPath, ParsedPath, PartialReflect, TypeRegistration, TypeRegistry,
        serde::TypedReflectSerializer,
    },
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::until::Check;
use crate::{
    bigspace::grid::{PositionError, absolute_position},
    command::{CommandError, EntityTarget},
};

/// Reads a field that is present, `null` included, as `Some`; `#[serde(default)]` leaves a
/// missing one `None`. Plain `Option` would read `null` as missing too.
fn present<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// How to compare.
#[derive(Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Op {
    /// JSON equality (numbers within 1e-6, relative).
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// The length of an array of numbers, such as a `Vec3`.
    LengthLt,
    LengthGt,
    /// Differs from the value when the wait started. No `value`.
    Changed,
    /// The component or resource is present. No `path`, no `value`.
    Exists,
    /// The component or resource is not present. No `path`, no `value`.
    Absent,
}

impl Op {
    /// As the caller wrote it.
    fn name(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Lt => "lt",
            Self::Le => "le",
            Self::Gt => "gt",
            Self::Ge => "ge",
            Self::LengthLt => "length_lt",
            Self::LengthGt => "length_gt",
            Self::Changed => "changed",
            Self::Exists => "exists",
            Self::Absent => "absent",
        }
    }
}

/// `{"component": {...}}`.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub(super) struct ComponentCondition {
    /// The entity id from an earlier result. Give this or `name`.
    #[serde(default)]
    entity: Option<u64>,
    /// The exact `Name` of the entity. Give this or `entity`.
    #[serde(default)]
    name: Option<String>,
    /// Full type path, or a short one that is unique, such as "LinearVelocity".
    component: String,
    /// Field path inside it, such as ".0.y" or "scale.x". Empty or left out compares the whole
    /// value.
    #[serde(default)]
    path: Option<String>,
    op: Op,
    /// What to compare with, shaped as `world_get_components` returns it. `null` is a value: an
    /// `Option` field that is `None` reads as `null`.
    #[serde(default, deserialize_with = "present")]
    value: Option<Value>,
}

/// `{"resource": {...}}`.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub(super) struct ResourceCondition {
    /// Full type path, or a short one that is unique.
    resource: String,
    /// Field path inside it. Left out compares the whole value.
    #[serde(default)]
    path: Option<String>,
    op: Op,
    #[serde(default, deserialize_with = "present")]
    value: Option<Value>,
}

/// `{"position": {...}}`: absolute metres, Y up.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub(super) struct PositionCondition {
    #[serde(default)]
    entity: Option<u64>,
    #[serde(default)]
    name: Option<String>,
    axis: Axis,
    /// One of "lt", "le", "gt", "ge".
    op: Op,
    /// Metres.
    value: f64,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
enum Axis {
    X,
    Y,
    Z,
}

/// Where a predicate reads its value.
enum Source {
    Component(EntityTarget),
    Resource,
}

/// A component or resource predicate being evaluated.
pub(super) struct Predicate {
    source: Source,
    type_id: TypeId,
    type_path: &'static str,
    path: Option<ParsedPath>,
    op: Op,
    value: Option<Value>,
    /// The value when the wait started, for `changed`.
    baseline: Option<Value>,
}

impl Predicate {
    pub(super) fn component(
        condition: &ComponentCondition,
        world: &mut World,
    ) -> Result<Self, CommandError> {
        let target = EntityTarget {
            entity: condition.entity,
            name: condition.name.clone(),
        };
        target.resolve(world)?;
        Self::new(
            Source::Component(target),
            &condition.component,
            condition.path.as_deref(),
            condition.op,
            condition.value.clone(),
            world,
        )
    }

    pub(super) fn resource(
        condition: &ResourceCondition,
        world: &mut World,
    ) -> Result<Self, CommandError> {
        Self::new(
            Source::Resource,
            &condition.resource,
            condition.path.as_deref(),
            condition.op,
            condition.value.clone(),
            world,
        )
    }

    fn new(
        source: Source,
        type_name: &str,
        path: Option<&str>,
        op: Op,
        value: Option<Value>,
        world: &mut World,
    ) -> Result<Self, CommandError> {
        let registry = world.resource::<AppTypeRegistry>().clone();
        let registry = registry.read();
        let registration = registration(&registry, type_name)?;
        let type_path = registration.type_info().type_path();
        if registration.data::<ReflectComponent>().is_none() {
            let what = match source {
                Source::Component(_) => "a component",
                Source::Resource => "a resource",
            };
            return Err(CommandError::invalid(
                "not_registered",
                format!("`{type_path}` is registered, but not as {what}"),
            ));
        }
        let path = path.filter(|path| !path.is_empty());
        // Field 0 of `Transform` is `translation`, so `#0` names it too.
        let first = path.map(|path| {
            path.trim_start_matches('.')
                .split(['.', '['])
                .next()
                .unwrap_or_default()
        });
        let touches_translation = type_path.ends_with("::Transform")
            && match first {
                Some(first) => first == "translation" || first == "#0",
                // The whole value includes the translation; presence and change do not compare it.
                None => !matches!(op, Op::Exists | Op::Absent | Op::Changed),
            };
        if touches_translation || type_path.ends_with("::CellCoord") {
            return Err(CommandError::invalid(
                "invalid_params",
                "under big_space a `Transform.translation` or `CellCoord` alone is not a world \
                 position",
            )
            .with_hint("Use the `position` condition, which compares absolute metres."));
        }
        match (op, path, &value) {
            (Op::Exists | Op::Absent, Some(_), _) | (Op::Exists | Op::Absent, _, Some(_)) => {
                return Err(CommandError::invalid(
                    "invalid_params",
                    "`exists` and `absent` take no `path` and no `value`",
                ));
            }
            (Op::Changed, _, Some(_)) => {
                return Err(CommandError::invalid(
                    "invalid_params",
                    "`changed` compares with the value at the start and takes no `value`",
                ));
            }
            (Op::Exists | Op::Absent | Op::Changed, _, None) => {}
            (_, _, None) => {
                return Err(CommandError::invalid(
                    "invalid_params",
                    format!("`{}` needs a `value`", op.name()),
                ));
            }
            _ => {}
        }
        let parsed = path
            .map(|path| {
                ParsedPath::parse(path).map_err(|err| {
                    CommandError::invalid("bad_path", format!("`{path}` is not a path: {err}"))
                })
            })
            .transpose()?;
        let type_id = registration.type_id();
        drop(registry);
        let mut predicate = Self {
            source,
            type_id,
            type_path,
            path: parsed,
            op,
            value,
            baseline: None,
        };
        // Reads once now: a bad path fails the call, and `changed` gets its baseline.
        let current = predicate.read(world)?;
        if let Some(current) = current {
            predicate.compare(&current)?;
            predicate.baseline = Some(current);
        } else if !matches!(op, Op::Exists | Op::Absent) {
            return Err(predicate.absent());
        }
        Ok(predicate)
    }

    pub(super) fn check(&mut self, world: &mut World) -> Result<Check, CommandError> {
        let current = self.read(world)?;
        let Some(current) = current else {
            return match self.op {
                Op::Absent => Ok(Check {
                    met: Some(json!({ "present": false })),
                    last: json!({ "present": false }),
                }),
                Op::Exists => Ok(Check {
                    met: None,
                    last: json!({ "present": false }),
                }),
                _ => Err(self.absent()),
            };
        };
        let met = self.compare(&current)?;
        let report = match self.op {
            Op::Exists | Op::Absent => json!({ "present": true }),
            _ => json!({ "value": current }),
        };
        Ok(Check {
            met: met.then(|| report.clone()),
            last: report,
        })
    }

    /// Whether `current` satisfies the op.
    fn compare(&self, current: &Value) -> Result<bool, CommandError> {
        let number = |value: &Value| value.as_f64();
        let wanted = self.value.as_ref();
        Ok(match self.op {
            Op::Exists => true,
            Op::Absent => false,
            Op::Changed => self
                .baseline
                .as_ref()
                .is_some_and(|baseline| !json_eq(baseline, current)),
            Op::Eq => wanted.is_some_and(|wanted| json_eq(wanted, current)),
            Op::Ne => wanted.is_some_and(|wanted| !json_eq(wanted, current)),
            Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                let (Some(a), Some(b)) = (number(current), wanted.and_then(number)) else {
                    return Err(self.not_comparable(current, "numbers"));
                };
                compare(self.op, a, b)
            }
            Op::LengthLt | Op::LengthGt => {
                let (Some(length), Some(b)) = (length(current), wanted.and_then(number)) else {
                    return Err(self.not_comparable(current, "an array of numbers and a number"));
                };
                if self.op == Op::LengthLt {
                    length < b
                } else {
                    length > b
                }
            }
        })
    }

    /// The value now, `None` when the component or resource is absent.
    fn read(&self, world: &mut World) -> Result<Option<Value>, CommandError> {
        let registry = world.resource::<AppTypeRegistry>().clone();
        let registry = registry.read();
        let Some(registration) = registry.get(self.type_id) else {
            return Ok(None);
        };
        let Some(reflect_component) = registration.data::<ReflectComponent>() else {
            return Ok(None);
        };
        let holder = match &self.source {
            Source::Component(target) => target.resolve(world).map_err(entity_gone)?,
            Source::Resource => {
                let Some(entity) = world
                    .components()
                    .get_id(self.type_id)
                    .and_then(|id| world.resource_entities().get(id))
                else {
                    return Ok(None);
                };
                entity
            }
        };
        let Ok(entity) = world.get_entity(holder) else {
            return Ok(None);
        };
        let Some(reflected) = reflect_component.reflect(entity) else {
            return Ok(None);
        };
        let value: &dyn PartialReflect = match &self.path {
            None => reflected.as_partial_reflect(),
            Some(path) => reflected.reflect_path(path).map_err(|err| {
                CommandError::failed(
                    "bad_path",
                    format!("`{}` has no such path: {err}", self.type_path),
                )
            })?,
        };
        serde_json::to_value(TypedReflectSerializer::new(value, &registry))
            .map(Some)
            .map_err(|err| {
                CommandError::failed(
                    "not_comparable",
                    format!(
                        "the value of `{}` does not serialize: {err}",
                        self.type_path
                    ),
                )
            })
    }

    fn absent(&self) -> CommandError {
        CommandError::failed(
            "component_absent",
            format!("`{}` is not present", self.type_path),
        )
        .with_hint("`exists` and `absent` wait for it to appear or go.")
    }

    fn not_comparable(&self, current: &Value, needs: &str) -> CommandError {
        CommandError::invalid(
            "not_comparable",
            format!(
                "`{}` needs {needs}; `{}` is {current}",
                self.op.name(),
                self.type_path
            ),
        )
    }
}

/// A position predicate being evaluated.
pub(super) struct PositionCheck {
    target: EntityTarget,
    axis: Axis,
    op: Op,
    value: f64,
}

impl PositionCheck {
    pub(super) fn new(
        condition: &PositionCondition,
        world: &mut World,
    ) -> Result<Self, CommandError> {
        if !matches!(condition.op, Op::Lt | Op::Le | Op::Gt | Op::Ge) {
            return Err(CommandError::invalid(
                "invalid_params",
                "`position` takes `lt`, `le`, `gt` or `ge`",
            ));
        }
        let check = Self {
            target: EntityTarget {
                entity: condition.entity,
                name: condition.name.clone(),
            },
            axis: condition.axis,
            op: condition.op,
            value: condition.value,
        };
        let entity = check.target.resolve(world)?;
        absolute_position(world, entity).map_err(position_error)?;
        Ok(check)
    }

    pub(super) fn check(&mut self, world: &mut World) -> Result<Check, CommandError> {
        let entity = self.target.resolve(world).map_err(entity_gone)?;
        let position = absolute_position(world, entity)
            .map_err(position_error)?
            .position;
        let along = match self.axis {
            Axis::X => position.x,
            Axis::Y => position.y,
            Axis::Z => position.z,
        };
        Ok(Check {
            met: compare(self.op, along, self.value)
                .then(|| json!({ "position": position.to_array() })),
            last: json!(along),
        })
    }
}

fn compare(op: Op, a: f64, b: f64) -> bool {
    match op {
        Op::Lt => a < b,
        Op::Le => a <= b,
        Op::Gt => a > b,
        Op::Ge => a >= b,
        _ => false,
    }
}

fn length(value: &Value) -> Option<f64> {
    let items = value.as_array()?;
    let mut sum = 0.0;
    for item in items {
        sum += item.as_f64()?.powi(2);
    }
    Some(sum.sqrt())
}

/// JSON equality, with numbers equal within a relative 1e-6, because an `f32` serialized as JSON
/// is never exactly the decimal an agent writes.
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(a), Some(b)) => (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0),
            _ => a == b,
        },
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| json_eq(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| json_eq(a, b)))
        }
        _ => a == b,
    }
}

/// A full type path, or a short one only one type has.
fn registration<'r>(
    registry: &'r TypeRegistry,
    name: &str,
) -> Result<&'r TypeRegistration, CommandError> {
    if let Some(registration) = registry.get_with_type_path(name) {
        return Ok(registration);
    }
    if registry.is_ambiguous(name) {
        return Err(CommandError::invalid(
            "not_registered",
            format!("the short name `{name}` belongs to several types; give the full type path"),
        )
        .with_hint("`registry_schema` with `contains` lists the full paths."));
    }
    registry.get_with_short_type_path(name).ok_or_else(|| {
        CommandError::invalid(
            "not_registered",
            format!(
                "`{name}` is not in the type registry, so nothing can read or write it (an \
                 unregistered type)"
            ),
        )
        .with_hint("`registry_schema` with `contains` finds the registered name.")
    })
}

/// A target that resolved when the wait started and no longer does.
fn entity_gone(err: CommandError) -> CommandError {
    if err.kind == "entity_not_found" {
        CommandError::failed(
            "entity_gone",
            format!("the entity went away during the wait: {}", err.message),
        )
    } else {
        err
    }
}

fn position_error(err: PositionError) -> CommandError {
    match err {
        PositionError::NoEntity(_) => CommandError::failed("entity_gone", err.to_string()),
        PositionError::NotOnGrid(_) => CommandError::failed("not_on_grid", err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use bevy::time::TimePlugin;
    use serde_json::json;

    use super::*;
    use crate::bigspace::{CellCoord, Grid};
    use crate::{
        command::{
            Caller, CommandPlugin, CommandScope, CommandWorldExt, JobId, JobStatus, Jobs, Reply,
        },
        time::TimeControlPlugin,
        wait::WaitPlugin,
    };

    #[derive(Component, Reflect, Default)]
    #[reflect(Component)]
    struct Speed {
        value: f32,
        direction: Vec3,
        limit: Option<f32>,
    }

    #[derive(Resource, Reflect, Default)]
    #[reflect(Resource)]
    struct Mode(String);

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((TimePlugin, TimeControlPlugin, CommandPlugin, WaitPlugin))
            .register_type::<Speed>()
            .register_type::<Mode>()
            .register_type::<Transform>()
            .register_type::<CellCoord>()
            .init_resource::<Mode>();
        app.world_mut().spawn((
            Name::new("Crate"),
            Speed {
                value: 1.0,
                direction: Vec3::new(3.0, 4.0, 0.0),
                limit: None,
            },
        ));
        app
    }

    fn call(app: &mut App, condition: Value) -> Result<JobId, CommandError> {
        match app.world_mut().call_command(
            "wait.until",
            json!({ "condition": condition }),
            Caller {
                scope: CommandScope::Development,
                cwd: None,
            },
        )? {
            Reply::Job(job) => Ok(job),
            Reply::Done(value) => panic!("not a job: {value}"),
        }
    }

    fn status(app: &App, job: &JobId) -> JobStatus {
        app.world()
            .resource::<Jobs>()
            .status(job, CommandScope::Development)
            .unwrap()
    }

    fn speed(app: &mut App) -> Mut<'_, Speed> {
        let entity = EntityTarget::by_name("Crate")
            .resolve(app.world_mut())
            .unwrap();
        app.world_mut().get_mut::<Speed>(entity).unwrap()
    }

    /// Whether `condition` holds against the current state after one frame.
    fn holds(app: &mut App, condition: Value) -> bool {
        let job = call(app, condition).unwrap();
        app.update();
        let status = status(app, &job);
        assert_ne!(status.state, "failed", "{status:?}");
        let held = status.state == "succeeded";
        if !held {
            app.world_mut()
                .cancel_job(&job, CommandScope::Development)
                .unwrap();
        }
        held
    }

    fn component(path: &str, op: &str, value: Value) -> Value {
        json!({ "component": {
            "name": "Crate", "component": "Speed", "path": path, "op": op, "value": value,
        } })
    }

    #[test]
    fn each_op_compares_the_serialized_value() {
        let mut app = app();
        for (path, op, value, expected) in [
            ("value", "eq", json!(1.0), true),
            ("value", "ne", json!(1.0), false),
            ("value", "lt", json!(2), true),
            ("value", "le", json!(1), true),
            ("value", "gt", json!(1), false),
            ("value", "ge", json!(1), true),
            ("direction", "length_lt", json!(5.1), true),
            ("direction", "length_gt", json!(5.1), false),
            ("direction", "eq", json!([3.0, 4.0, 0.0]), true),
            (
                "",
                "eq",
                json!({ "value": 1.0, "direction": [3.0, 4.0, 0.0], "limit": null }),
                true,
            ),
            ("limit", "eq", Value::Null, true),
            ("limit", "ne", Value::Null, false),
        ] {
            assert_eq!(
                holds(&mut app, component(path, op, value.clone())),
                expected,
                "{path} {op} {value}"
            );
        }
        assert!(holds(
            &mut app,
            json!({ "resource": { "resource": "Mode", "path": ".0", "op": "eq", "value": "" } })
        ));
    }

    #[test]
    fn changed_compares_with_the_value_at_the_start() {
        let mut app = app();
        let job = call(
            &mut app,
            json!({ "component": { "name": "Crate", "component": "Speed", "path": "value", "op": "changed" } }),
        )
        .unwrap();
        app.update();
        assert_eq!(status(&app, &job).state, "running");
        speed(&mut app).value = 2.0;
        app.update();
        let status = status(&app, &job);
        assert_eq!(status.result.unwrap()["met"], json!({ "value": 2.0 }));
    }

    #[test]
    fn exists_and_absent_follow_the_component() {
        let mut app = app();
        let condition =
            |op: &str| json!({ "component": { "name": "Crate", "component": "Speed", "op": op } });
        assert!(holds(&mut app, condition("exists")));
        let job = call(&mut app, condition("absent")).unwrap();
        app.update();
        assert_eq!(status(&app, &job).state, "running");
        let entity = EntityTarget::by_name("Crate")
            .resolve(app.world_mut())
            .unwrap();
        app.world_mut().entity_mut(entity).remove::<Speed>();
        app.update();
        assert_eq!(status(&app, &job).state, "succeeded");
        let err = call(&mut app, component("value", "eq", json!(1))).unwrap_err();
        assert_eq!(err.kind, "component_absent");
    }

    #[test]
    fn refusals_name_what_is_wrong() {
        let mut app = app();
        let refused = |app: &mut App, condition: Value| call(app, condition).unwrap_err().kind;
        assert_eq!(
            refused(
                &mut app,
                json!({ "component": { "name": "Crate", "component": "Nope", "op": "exists" } })
            ),
            "not_registered"
        );
        assert_eq!(
            refused(&mut app, component("nowhere", "eq", json!(1))),
            "bad_path"
        );
        assert_eq!(
            refused(&mut app, component("value", "length_lt", json!(1))),
            "not_comparable"
        );
        assert_eq!(
            refused(
                &mut app,
                json!({ "component": { "name": "Crate", "component": "Speed", "op": "eq" } })
            ),
            "invalid_params",
            "a missing value"
        );
        let transform = |path: &str, op: &str| {
            json!({ "component": {
                "name": "Crate", "component": "bevy_transform::components::transform::Transform",
                "path": path, "op": op, "value": 0
            } })
        };
        for path in ["translation.y", ".translation", "#0", ".#0.y", ""] {
            assert_eq!(
                refused(&mut app, transform(path, "lt")),
                "invalid_params",
                "{path}"
            );
        }
        let err = call(
            &mut app,
            json!({ "component": { "name": "Crate", "component": "CellCoord", "op": "exists" } }),
        )
        .unwrap_err();
        assert_eq!(err.kind, "invalid_params");
        assert!(err.hint.unwrap().contains("position"));
    }

    #[test]
    fn a_whole_transform_may_be_watched_for_presence_and_change() {
        let mut app = app();
        let entity = EntityTarget::by_name("Crate")
            .resolve(app.world_mut())
            .unwrap();
        app.world_mut()
            .entity_mut(entity)
            .insert(Transform::default());
        assert!(holds(
            &mut app,
            json!({ "component": { "name": "Crate", "component": "Transform", "op": "exists" } })
        ));
        let job = call(
            &mut app,
            json!({ "component": { "name": "Crate", "component": "Transform", "op": "changed" } }),
        )
        .unwrap();
        app.update();
        app.world_mut()
            .get_mut::<Transform>(entity)
            .unwrap()
            .scale
            .x = 2.0;
        app.update();
        assert_eq!(status(&app, &job).state, "succeeded");
    }

    mod first {
        use bevy::prelude::*;

        #[derive(Component, Reflect, Default)]
        #[reflect(Component)]
        pub struct Twin;
    }

    mod second {
        use bevy::prelude::*;

        #[derive(Component, Reflect, Default)]
        #[reflect(Component)]
        pub struct Twin;
    }

    #[test]
    fn an_ambiguous_short_name_asks_for_the_full_path() {
        let mut app = app();
        app.register_type::<first::Twin>()
            .register_type::<second::Twin>();
        let err = call(
            &mut app,
            json!({ "component": { "name": "Crate", "component": "Twin", "op": "exists" } }),
        )
        .unwrap_err();
        assert_eq!(err.kind, "not_registered");
        assert!(err.message.contains("several types"), "{err}");
        let full = std::any::type_name::<first::Twin>();
        assert!(holds(
            &mut app,
            json!({ "component": { "name": "Crate", "component": full, "op": "absent" } })
        ));
    }

    #[test]
    fn a_position_condition_compares_absolute_metres() {
        let mut app = app();
        let grid = app.world_mut().spawn(Grid::new(100.0, 0.0)).id();
        let body = app
            .world_mut()
            .spawn((
                Name::new("Body"),
                ChildOf(grid),
                CellCoord::new(0, 2, 0),
                Transform::from_xyz(0.0, 5.0, 0.0),
            ))
            .id();
        let below = |value: f64| json!({ "position": { "name": "Body", "axis": "y", "op": "lt", "value": value } });
        // Cell 2 of 100 m plus 5 m: 205 m, not the 5 m the transform alone says.
        assert!(!holds(&mut app, below(200.0)));
        let job = call(&mut app, below(200.0)).unwrap();
        app.update();
        app.world_mut()
            .entity_mut(body)
            .insert(CellCoord::new(0, 1, 0));
        app.update();
        let status = status(&app, &job);
        assert_eq!(
            status.result.unwrap()["met"]["position"],
            json!([0.0, 105.0, 0.0])
        );
        let err = call(
            &mut app,
            json!({ "position": { "name": "Body", "axis": "y", "op": "eq", "value": 1.0 } }),
        )
        .unwrap_err();
        assert_eq!(err.kind, "invalid_params");
        let err = call(
            &mut app,
            json!({ "position": { "name": "Crate", "axis": "y", "op": "lt", "value": 1.0 } }),
        )
        .unwrap_err();
        assert_eq!(err.kind, "not_on_grid");
    }

    #[test]
    fn a_despawn_mid_wait_is_entity_gone() {
        let mut app = app();
        let job = call(&mut app, component("value", "gt", json!(5))).unwrap();
        app.update();
        let entity = EntityTarget::by_name("Crate")
            .resolve(app.world_mut())
            .unwrap();
        app.world_mut().despawn(entity);
        app.update();
        assert_eq!(status(&app, &job).error.unwrap().kind, "entity_gone");
    }
}
