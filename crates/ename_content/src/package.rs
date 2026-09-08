use bevy::{
    ecs::{reflect::ReflectResource, resource::Resource},
    reflect::{Reflect, std_traits::ReflectDefault},
};

/// Resource containing all packages
#[derive(Default, Debug, Clone, Resource, Reflect)]
#[reflect(Default, Debug, Clone, Resource)]
pub struct Packages {
    pub items: Vec<Package>,
}

/// Used to keep track of packages.
///
/// A husk. It held a `Handle<Manifest>` until manifests stopped being Bevy assets; nothing
/// constructs one now, and the crate is deleted once `ename_game` addresses assets by alias.
#[derive(Debug, Clone, PartialEq, Reflect)]
pub struct Package {
    pub state: PackageState,
    pub id: String,
}

#[derive(Debug, Copy, Clone, PartialEq, Reflect)]
pub enum PackageState {
    Inactive,
    Active,
}
