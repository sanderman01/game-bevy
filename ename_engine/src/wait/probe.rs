//! Settle probes: read-only systems a crate registers to report the work it still has pending.
//!
//! Only the crate that owns the work knows what "pending" means, so it writes the probe. Probes
//! run only while something waits ([`SettleWaiter`]), so a build where nobody waits pays nothing.

use std::collections::BTreeMap;

use bevy::{
    diagnostic::FrameCount,
    ecs::system::{ReadOnlySystem, SystemId},
    prelude::*,
};
use serde::{Serialize, ser::SerializeMap};

/// Named counters of work still pending. Zero counters are dropped.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Activity(Vec<(&'static str, u32)>);

impl Activity {
    pub fn idle() -> Self {
        Self::default()
    }

    /// Adds a counter. `count` saturates at `u32::MAX`; zero adds nothing.
    pub fn pending(mut self, counter: &'static str, count: usize) -> Self {
        if count > 0 {
            self.0
                .push((counter, u32::try_from(count).unwrap_or(u32::MAX)));
        }
        self
    }

    pub fn is_idle(&self) -> bool {
        self.0.is_empty()
    }

    pub fn counters(&self) -> impl Iterator<Item = (&'static str, u32)> + '_ {
        self.0.iter().copied()
    }
}

/// Serializes as `{"counter": count}`.
impl Serialize for Activity {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (counter, count) in &self.0 {
            map.serialize_entry(counter, count)?;
        }
        map.end()
    }
}

impl std::fmt::Display for Activity {
    /// `tiles_loading 3, transitions_pending 1`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, (counter, count)) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{counter} {count}")?;
        }
        Ok(())
    }
}

/// Marks an entity that waits on the probes. While one exists, every probe runs each frame.
#[derive(Component, Default)]
pub struct SettleWaiter;

/// Every registered probe, by name.
#[derive(Resource, Default)]
pub(crate) struct SettleProbes(BTreeMap<&'static str, SystemId<(), Activity>>);

/// This frame's sample, written in [`WaitSystems::Sample`](super::WaitSystems) when anyone waits.
#[derive(Resource, Default, Debug)]
pub struct SettleSample {
    /// The `FrameCount` the sample was taken in.
    pub frame: u32,
    /// Busy probes only, by name.
    pub busy: BTreeMap<&'static str, Activity>,
}

/// Registering settle probes on an [`App`].
pub trait SettleAppExt {
    /// `name` is snake_case and unique; a duplicate panics at startup. The probe must be
    /// read-only, and should count only work that finishes: a probe that never reaches idle
    /// makes every settle time out.
    fn register_settle_probe<M, S>(&mut self, name: &'static str, probe: S) -> &mut Self
    where
        S: IntoSystem<(), Activity, M> + 'static,
        S::System: ReadOnlySystem;
}

impl SettleAppExt for App {
    fn register_settle_probe<M, S>(&mut self, name: &'static str, probe: S) -> &mut Self
    where
        S: IntoSystem<(), Activity, M> + 'static,
        S::System: ReadOnlySystem,
    {
        assert!(
            !name.is_empty()
                && name.starts_with(|c: char| c.is_ascii_lowercase())
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "settle probe name `{name}` must be lowercase snake_case"
        );
        let world = self.world_mut();
        let id = world.register_system(probe);
        let mut probes = world.get_resource_or_init::<SettleProbes>();
        assert!(
            probes.0.insert(name, id).is_none(),
            "settle probe `{name}` is registered twice"
        );
        self
    }
}

/// Runs every probe once and returns the busy ones.
pub(crate) fn run_probes(world: &mut World) -> BTreeMap<&'static str, Activity> {
    let Some(probes) = world.get_resource::<SettleProbes>() else {
        return BTreeMap::new();
    };
    let probes: Vec<_> = probes.0.iter().map(|(&name, &id)| (name, id)).collect();
    probes
        .into_iter()
        .filter_map(|(name, id)| match world.run_system(id) {
            Ok(activity) => (!activity.is_idle()).then_some((name, activity)),
            Err(err) => {
                // A probe whose params are missing (a resource not inserted) reports nothing.
                debug!("settle probe `{name}` did not run: {err}");
                None
            }
        })
        .collect()
}

pub(super) fn sample_probes(world: &mut World) {
    let busy = run_probes(world);
    let frame = world
        .get_resource::<FrameCount>()
        .map_or(0, |count| count.0);
    world.insert_resource(SettleSample { frame, busy });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wait::WaitPlugin;

    #[derive(Resource)]
    struct Pending(usize);

    fn pending_probe(pending: Res<Pending>) -> Activity {
        Activity::idle().pending("things", pending.0)
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(WaitPlugin)
            .insert_resource(Pending(3))
            .register_settle_probe("test", pending_probe);
        app
    }

    fn busy(app: &App) -> Option<Activity> {
        app.world()
            .resource::<SettleSample>()
            .busy
            .get("test")
            .cloned()
    }

    #[test]
    fn a_probe_reports_busy_until_its_work_drains() {
        let mut app = app();
        app.world_mut().spawn(SettleWaiter);
        for left in (0..3).rev() {
            app.update();
            assert_eq!(
                busy(&app),
                Some(Activity::idle().pending("things", left + 1))
            );
            app.world_mut().resource_mut::<Pending>().0 = left;
        }
        app.update();
        assert_eq!(busy(&app), None);
    }

    #[test]
    fn nothing_samples_without_a_waiter() {
        let mut app = app();
        app.update();
        assert!(busy(&app).is_none());
        app.world_mut().spawn(SettleWaiter);
        app.update();
        assert!(busy(&app).is_some());
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn a_duplicate_name_panics() {
        app().register_settle_probe("test", pending_probe);
    }

    #[test]
    fn activity_drops_zero_counters_and_serializes_as_a_map() {
        let activity = Activity::idle()
            .pending("a", 2)
            .pending("b", 0)
            .pending("c", usize::MAX);
        assert_eq!(
            serde_json::to_value(&activity).unwrap(),
            serde_json::json!({ "a": 2, "c": u32::MAX })
        );
        assert_eq!(activity.to_string(), format!("a 2, c {}", u32::MAX));
        assert!(Activity::idle().pending("x", 0).is_idle());
    }
}
