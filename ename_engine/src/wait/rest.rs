//! The `at_rest` condition of `wait.until`: a dynamic rigid body that has stopped moving.
//!
//! Speed passes through zero at the apex of every bounce, so "at rest" must hold for
//! `hold_seconds` of virtual time, not a frame. avian's `Sleeping` short-circuits the hold, but
//! only once a physics step has run since the wait started: a body moved while asleep keeps
//! `Sleeping` until avian wakes it in its next step, and a rendered frame can contain no fixed
//! step at all.

use avian3d::prelude::{AngularVelocity, LinearVelocity, RigidBody, Sleeping};
use bevy::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::until::Check;
use crate::{
    bigspace::grid::absolute_position,
    command::{CommandError, EntityTarget},
};

const DEFAULT_SPEED_BELOW: f64 = 0.05;
const ANGULAR_SPEED_BELOW: f64 = 0.05;
const DEFAULT_HOLD_SECONDS: f64 = 0.25;

/// `{"at_rest": {...}}`.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub(super) struct AtRestCondition {
    #[serde(default)]
    entity: Option<u64>,
    #[serde(default)]
    name: Option<String>,
    /// Linear speed under which the body counts as still, m/s. Defaults to 0.05. Angular speed
    /// must also be under 0.05 rad/s.
    #[serde(default)]
    speed_below: Option<f64>,
    /// Virtual seconds it must stay still. Defaults to 0.25.
    #[serde(default)]
    hold_seconds: Option<f64>,
}

/// Physics steps run while some `wait.until` exists. Bumped in `FixedPostUpdate`, where avian
/// steps, so a change since the wait started means a step has run.
#[derive(Resource, Default)]
pub(super) struct PhysicsSteps(u64);

pub(super) fn count_physics_steps(mut steps: ResMut<PhysicsSteps>) {
    steps.0 += 1;
}

pub(super) struct AtRest {
    target: EntityTarget,
    speed_below: f64,
    hold_seconds: f64,
    steps_at_start: u64,
    /// Virtual time the body was first seen still, after a physics step.
    still_since: Option<f64>,
}

impl AtRest {
    pub(super) fn new(
        condition: &AtRestCondition,
        world: &mut World,
    ) -> Result<Self, CommandError> {
        let speed_below = condition.speed_below.unwrap_or(DEFAULT_SPEED_BELOW);
        let hold_seconds = condition.hold_seconds.unwrap_or(DEFAULT_HOLD_SECONDS);
        if !(speed_below > 0.0 && speed_below.is_finite())
            || !(hold_seconds >= 0.0 && hold_seconds.is_finite())
        {
            return Err(CommandError::invalid(
                "invalid_params",
                "`speed_below` must be above 0 and `hold_seconds` at least 0",
            ));
        }
        let target = EntityTarget {
            entity: condition.entity,
            name: condition.name.clone(),
        };
        let entity = target.resolve(world)?;
        if let Some(message) = not_a_dynamic_body(world, entity) {
            return Err(
                CommandError::invalid("not_comparable", message).with_hint(NOT_DYNAMIC_HINT)
            );
        }
        Ok(Self {
            target,
            speed_below,
            hold_seconds,
            steps_at_start: world
                .get_resource::<PhysicsSteps>()
                .map_or(0, |steps| steps.0),
            still_since: None,
        })
    }

    pub(super) fn check(
        &mut self,
        world: &mut World,
        virtual_now: f64,
    ) -> Result<Check, CommandError> {
        let entity = self.target.resolve(world).map_err(|err| {
            if err.kind == "entity_not_found" {
                CommandError::failed(
                    "entity_gone",
                    format!("the body went away: {}", err.message),
                )
            } else {
                err
            }
        })?;
        // A body made static or stripped of its velocities mid-wait would read as still.
        if let Some(message) = not_a_dynamic_body(world, entity) {
            return Err(CommandError::failed("not_comparable", message).with_hint(NOT_DYNAMIC_HINT));
        }
        let linear = world
            .get::<LinearVelocity>(entity)
            .map_or(0.0, |velocity| f64::from(velocity.0.length()));
        let angular = world
            .get::<AngularVelocity>(entity)
            .map_or(0.0, |velocity| f64::from(velocity.0.length()));
        let sleeping = world.get::<Sleeping>(entity).is_some();
        let stepped = world
            .get_resource::<PhysicsSteps>()
            .is_some_and(|steps| steps.0 != self.steps_at_start);
        let state = json!({
            "linear_speed": round(linear),
            "angular_speed": round(angular),
            "sleeping": sleeping,
        });
        let still = stepped && (linear < self.speed_below && angular < ANGULAR_SPEED_BELOW);
        if !still && !(stepped && sleeping) {
            self.still_since = None;
            return Ok(Check {
                met: None,
                last: state,
            });
        }
        let since = *self.still_since.get_or_insert(virtual_now);
        let met = (sleeping || virtual_now - since >= self.hold_seconds).then(|| {
            let mut met = state.clone();
            met["position"] = absolute_position(world, entity)
                .map_or(Value::Null, |found| json!(found.position.to_array()));
            met
        });
        Ok(Check { met, last: state })
    }
}

/// Why `entity` is not a body `at_rest` can judge, if it is not.
fn not_a_dynamic_body(world: &World, entity: Entity) -> Option<String> {
    let body = world.get::<RigidBody>(entity).copied();
    let moving = world.get::<LinearVelocity>(entity).is_some()
        && world.get::<AngularVelocity>(entity).is_some();
    if body == Some(RigidBody::Dynamic) && moving {
        return None;
    }
    let what = match body {
        Some(RigidBody::Dynamic) => "a dynamic body without its velocities".to_owned(),
        Some(body) => format!("a {body:?} rigid body"),
        None => "not a rigid body".to_owned(),
    };
    Some(format!(
        "`at_rest` needs a dynamic rigid body with velocities; this is {what}"
    ))
}

const NOT_DYNAMIC_HINT: &str = "Only `RigidBody::Dynamic` moves under physics. For a position \
                                threshold use the `position` condition.";
fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use avian3d::prelude::{Collider, PhysicsPlugins};
    use bevy::{
        asset::AssetPlugin, mesh::MeshPlugin, scene::ScenePlugin, time::TimeUpdateStrategy,
        transform::TransformPlugin,
    };
    use serde_json::json;

    use crate::{
        command::{
            Caller, CommandPlugin, CommandScope, CommandWorldExt, JobId, JobStatus, Jobs, Reply,
        },
        time::{TimeControl, TimeControlPlugin},
        wait::WaitPlugin,
    };

    use super::*;

    fn app(frame: Duration) -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            TransformPlugin,
            AssetPlugin::default(),
            MeshPlugin,
            ScenePlugin,
            PhysicsPlugins::default(),
            TimeControlPlugin,
            CommandPlugin,
            WaitPlugin,
        ))
        .insert_resource(TimeUpdateStrategy::ManualDuration(frame));
        app.finish();
        app.cleanup();
        app.world_mut()
            .spawn((RigidBody::Static, Collider::cuboid(20.0, 1.0, 20.0)));
        app.world_mut().spawn((
            Name::new("Crate"),
            RigidBody::Dynamic,
            Collider::cuboid(1.0, 1.0, 1.0),
            Transform::from_xyz(0.0, 3.0, 0.0),
        ));
        app.update();
        app
    }

    fn call(app: &mut App, params: serde_json::Value) -> JobId {
        match app
            .world_mut()
            .call_command(
                "wait.until",
                params,
                Caller {
                    scope: CommandScope::Development,
                    cwd: None,
                },
            )
            .unwrap()
        {
            Reply::Job(job) => job,
            Reply::Done(value) => panic!("not a job: {value}"),
        }
    }

    fn status(app: &App, job: &JobId) -> JobStatus {
        app.world()
            .resource::<Jobs>()
            .status(job, CommandScope::Development)
            .unwrap()
    }

    fn set_paused(app: &mut App, paused: bool) {
        app.world_mut()
            .resource_scope(|world, mut control: Mut<TimeControl>| {
                let mut time = world.resource_mut::<Time<Virtual>>();
                if paused {
                    control.pause(&mut time);
                } else {
                    control.resume(&mut time);
                }
            });
    }

    fn crate_y(app: &mut App) -> f32 {
        let entity = EntityTarget::by_name("Crate")
            .resolve(app.world_mut())
            .unwrap();
        app.world().get::<Transform>(entity).unwrap().translation.y
    }

    fn run_until_done(app: &mut App, job: &JobId) -> JobStatus {
        for _ in 0..2000 {
            app.update();
            let status = status(app, job);
            if status.state != "running" {
                return status;
            }
        }
        panic!("the body never came to rest");
    }

    #[test]
    fn a_dropped_body_comes_to_rest_and_the_world_pauses_in_that_frame() {
        let mut app = app(Duration::from_secs_f64(1.0 / 60.0));
        set_paused(&mut app, true);
        let job = call(
            &mut app,
            json!({ "condition": { "at_rest": { "name": "Crate" } }, "time": "run" }),
        );
        let status = run_until_done(&mut app, &job);
        assert_eq!(status.state, "succeeded", "{status:?}");
        let result = status.result.unwrap();
        assert_eq!(result["paused"], true);
        assert!(app.world().resource::<Time<Virtual>>().is_paused());
        // Resting on the ground: its top at 0.5, the crate's half height 0.5 above that.
        let y = crate_y(&mut app);
        assert!((y - 1.0).abs() < 0.1, "rests at {y}");
    }

    #[test]
    fn a_sleeping_body_teleported_up_is_not_at_rest_before_it_falls() {
        // Frames shorter than the fixed timestep: some frames run no physics step.
        let mut app = app(Duration::from_secs_f64(1.0 / 240.0));
        let settled = call(
            &mut app,
            json!({ "condition": { "at_rest": { "name": "Crate" } }, "timeout_seconds": 60 }),
        );
        assert_eq!(run_until_done(&mut app, &settled).state, "succeeded");
        // Let it fall asleep.
        let entity = EntityTarget::by_name("Crate")
            .resolve(app.world_mut())
            .unwrap();
        for _ in 0..2000 {
            if app.world().get::<Sleeping>(entity).is_some() {
                break;
            }
            app.update();
        }
        assert!(app.world().get::<Sleeping>(entity).is_some(), "never slept");

        set_paused(&mut app, true);
        app.world_mut()
            .get_mut::<Transform>(entity)
            .unwrap()
            .translation
            .y = 10.0;
        let job = call(
            &mut app,
            json!({ "condition": { "at_rest": { "name": "Crate" } }, "time": "run" }),
        );
        // The premise: a frame with no physics step, while the stale `Sleeping` is still there.
        let steps = app.world().resource::<PhysicsSteps>().0;
        app.update();
        assert_eq!(
            app.world().resource::<PhysicsSteps>().0,
            steps,
            "a step ran"
        );
        assert!(app.world().get::<Sleeping>(entity).is_some());
        assert_eq!(status(&app, &job).state, "running");

        let status = run_until_done(&mut app, &job);
        assert_eq!(status.state, "succeeded", "{status:?}");
        let y = crate_y(&mut app);
        assert!(y < 3.0, "reported at rest at {y}, before it fell");
    }

    #[test]
    fn a_body_made_static_mid_wait_fails_instead_of_resting() {
        let mut app = app(Duration::from_secs_f64(1.0 / 60.0));
        let job = call(
            &mut app,
            json!({ "condition": { "at_rest": { "name": "Crate" } } }),
        );
        app.update();
        let entity = EntityTarget::by_name("Crate")
            .resolve(app.world_mut())
            .unwrap();
        app.world_mut().entity_mut(entity).insert(RigidBody::Static);
        app.update();
        app.update();
        assert_eq!(status(&app, &job).error.unwrap().kind, "not_comparable");
    }
}
