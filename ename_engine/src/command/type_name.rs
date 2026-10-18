//! Naming a component or resource type the way an agent writes it: a full type path, a path
//! suffix (`grid::Grid`) or a short name (`Transform`).
//!
//! Exact full path wins, then exact short name (case-sensitive, then case-insensitive), then
//! suffix. Several matches in the first tier that has any is an error listing them, the same rule
//! [`EntityTarget`](super::EntityTarget) applies to names. Results always carry the full path.

use std::any::TypeId;

use bevy::{
    ecs::{component::ComponentId, reflect::ReflectComponent},
    prelude::*,
    reflect::TypeRegistry,
};

use super::CommandError;

/// Which kind of type a name is looked up among.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeSet {
    Components,
    Resources,
}

/// A type a name resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedType {
    /// The registration's type path, or `ComponentInfo::name` for a type with no registration.
    pub path: String,
    pub type_id: Option<TypeId>,
    /// `None` for a registered type no entity or resource has used yet.
    pub component_id: Option<ComponentId>,
    /// Registered with `ReflectComponent`, so its value can be read and written.
    pub reflected: bool,
}

impl ResolvedType {
    /// Fails with `not_reflected` for a type reflection cannot read or write.
    pub fn require_reflected(self) -> Result<Self, CommandError> {
        if self.reflected {
            return Ok(self);
        }
        Err(CommandError::invalid(
            "not_reflected",
            format!(
                "`{}` does not derive `Reflect` with `#[reflect(Component)]`, so its value cannot \
                 be read or written",
                self.path
            ),
        )
        .with_hint("It can still be listed and removed."))
    }
}

struct Candidate {
    resolved: ResolvedType,
    short: String,
}

/// Resolves `name` among `set`. With `entity`, that entity's own components are tried first, so
/// a short name another crate also uses still means the one the entity has.
pub fn resolve_type(
    world: &World,
    set: TypeSet,
    entity: Option<Entity>,
    name: &str,
) -> Result<ResolvedType, CommandError> {
    let registry = world.resource::<AppTypeRegistry>().read();
    if let Some(entity) = entity
        && let Ok(entity_ref) = world.get_entity(entity)
    {
        let own: Vec<Candidate> = entity_ref
            .archetype()
            .components()
            .iter()
            .filter_map(|&id| candidate_for_id(world, &registry, id))
            .collect();
        if let Some(found) = pick(&own, name)? {
            return Ok(found);
        }
    }
    let all = candidates(world, &registry, set);
    pick(&all, name)?.ok_or_else(|| unknown(&all, name, set))
}

/// The first tier with a match decides.
fn pick(candidates: &[Candidate], name: &str) -> Result<Option<ResolvedType>, CommandError> {
    let suffix = format!("::{name}");
    let tiers: [&dyn Fn(&Candidate) -> bool; 4] = [
        &|c| c.resolved.path == name,
        &|c| c.short == name,
        &|c| c.short.eq_ignore_ascii_case(name),
        &|c| c.resolved.path.ends_with(&suffix),
    ];
    for tier in tiers {
        let mut found: Vec<&Candidate> = candidates.iter().filter(|c| tier(c)).collect();
        found.sort_by(|a, b| a.resolved.path.cmp(&b.resolved.path));
        found.dedup_by(|a, b| a.resolved.path == b.resolved.path);
        match found[..] {
            [] => continue,
            [one] => return Ok(Some(one.resolved.clone())),
            _ => {
                let paths: Vec<&str> = found.iter().map(|c| c.resolved.path.as_str()).collect();
                return Err(CommandError::invalid(
                    "ambiguous_type",
                    format!("`{name}` names {} types: {}", paths.len(), paths.join(", ")),
                )
                .with_hint("Give the full type path, or a longer suffix such as `module::Type`."));
            }
        }
    }
    Ok(None)
}

fn unknown(candidates: &[Candidate], name: &str, set: TypeSet) -> CommandError {
    let needle = name.to_lowercase();
    let mut close: Vec<&str> = candidates
        .iter()
        .filter(|c| c.resolved.path.to_lowercase().contains(&needle))
        .map(|c| c.resolved.path.as_str())
        .collect();
    close.sort_by_key(|path| (path.len(), *path));
    close.dedup();
    close.truncate(5);
    let what = match set {
        TypeSet::Components => "component",
        TypeSet::Resources => "resource",
    };
    let error = CommandError::invalid(
        "unknown_type",
        format!("no {what} type in this game is called `{name}`"),
    );
    if close.is_empty() {
        error.with_hint("`registry_schema` with `contains` searches the registered types.")
    } else {
        error.with_hint(format!("Did you mean: {}?", close.join(", ")))
    }
}

/// Every type of `set` the registry or the world knows, one per type.
fn candidates(world: &World, registry: &TypeRegistry, set: TypeSet) -> Vec<Candidate> {
    let components = world.components();
    let mut out: Vec<Candidate> = registry
        .iter()
        .filter(|registration| {
            let resource = registration.data::<ReflectResource>().is_some();
            match set {
                TypeSet::Components => {
                    registration.data::<ReflectComponent>().is_some() && !resource
                }
                TypeSet::Resources => resource,
            }
        })
        .map(|registration| Candidate {
            resolved: ResolvedType {
                path: registration.type_info().type_path().to_owned(),
                type_id: Some(registration.type_id()),
                component_id: components.get_id(registration.type_id()),
                reflected: true,
            },
            short: registration
                .type_info()
                .type_path_table()
                .short_path()
                .to_owned(),
        })
        .collect();
    if set == TypeSet::Components {
        // Components reflection cannot see still have a name, and can still be removed.
        out.extend(components.iter_registered().filter_map(|info| {
            let registered = info.type_id().and_then(|id| registry.get(id));
            // Resources are components on an entity of their own; they are not offered here.
            if registered.is_some_and(|r| r.data::<ReflectComponent>().is_some())
                || world.resource_entities().get(info.id()).is_some()
            {
                return None;
            }
            candidate_for_id(world, registry, info.id())
        }));
    }
    out
}

fn candidate_for_id(world: &World, registry: &TypeRegistry, id: ComponentId) -> Option<Candidate> {
    let info = world.components().get_info(id)?;
    let registration = info.type_id().and_then(|type_id| registry.get(type_id));
    Some(match registration {
        Some(registration) => Candidate {
            resolved: ResolvedType {
                path: registration.type_info().type_path().to_owned(),
                type_id: Some(registration.type_id()),
                component_id: Some(id),
                reflected: registration.data::<ReflectComponent>().is_some(),
            },
            short: registration
                .type_info()
                .type_path_table()
                .short_path()
                .to_owned(),
        },
        None => Candidate {
            resolved: ResolvedType {
                path: info.name().to_string(),
                type_id: info.type_id(),
                component_id: Some(id),
                reflected: false,
            },
            short: info.name().shortname().to_string(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    mod first {
        use bevy::prelude::*;

        #[derive(Component, Reflect, Default)]
        #[reflect(Component)]
        pub struct Pair;
    }

    mod second {
        use bevy::prelude::*;

        #[derive(Component, Reflect, Default)]
        #[reflect(Component)]
        pub struct Pair;
    }

    #[derive(Component, Reflect, Default)]
    #[reflect(Component)]
    struct Thrust(f32);

    #[derive(Component)]
    struct Hidden;

    #[derive(Resource, Reflect, Default)]
    #[reflect(Resource)]
    struct Budget(u32);

    fn world() -> World {
        let mut world = World::new();
        world.init_resource::<AppTypeRegistry>();
        {
            let mut registry = world.resource::<AppTypeRegistry>().write();
            registry.register::<Thrust>();
            registry.register::<first::Pair>();
            registry.register::<second::Pair>();
            registry.register::<Budget>();
        }
        world.init_resource::<Budget>();
        world
    }

    fn path(world: &World, entity: Option<Entity>, name: &str) -> Result<String, String> {
        resolve_type(world, TypeSet::Components, entity, name)
            .map(|found| found.path)
            .map_err(|err| err.kind.into_owned())
    }

    #[test]
    fn full_path_short_name_case_and_suffix() {
        let world = world();
        let full = std::any::type_name::<Thrust>().to_owned();
        assert_eq!(path(&world, None, &full), Ok(full.clone()));
        assert_eq!(path(&world, None, "Thrust"), Ok(full.clone()));
        assert_eq!(path(&world, None, "thrust"), Ok(full.clone()));
        assert_eq!(
            path(&world, None, "first::Pair"),
            Ok(std::any::type_name::<first::Pair>().into())
        );
        assert_eq!(path(&world, None, "Pair"), Err("ambiguous_type".into()));
        let err = resolve_type(&world, TypeSet::Components, None, "Thrus").unwrap_err();
        assert_eq!(err.kind, "unknown_type");
        // A resource is not a component, though it registers `ReflectComponent` too.
        assert_eq!(path(&world, None, "Budget"), Err("unknown_type".into()));
        assert!(resolve_type(&world, TypeSet::Resources, None, "Budget").is_ok());
    }

    #[test]
    fn the_entitys_own_component_wins_and_unreflected_ones_resolve() {
        let mut world = world();
        let entity = world.spawn((second::Pair, Hidden)).id();
        assert_eq!(
            path(&world, Some(entity), "Pair"),
            Ok(std::any::type_name::<second::Pair>().into())
        );
        let hidden = resolve_type(&world, TypeSet::Components, Some(entity), "Hidden").unwrap();
        assert!(!hidden.reflected);
        assert_eq!(
            hidden.require_reflected().unwrap_err().kind,
            "not_reflected"
        );
        // Known but not on this entity: resolves, so the caller can report it absent.
        assert!(resolve_type(&world, TypeSet::Components, Some(entity), "Thrust").is_ok());
    }
}
