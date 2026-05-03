use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use bevy::{
    asset::{Asset, AssetServer, Handle},
    ecs::resource::Resource,
    platform::collections::HashMap,
};

struct AssetRegistryData {
    mappings: RwLock<Mappings>,
}

struct Mappings {
    pub alias_to_path: HashMap<String, String>,
}
impl Mappings {
    fn new() -> Self {
        Self {
            alias_to_path: HashMap::<String, String>::new(),
        }
    }
}

#[derive(Resource, Clone)]
pub struct AssetRegistry {
    data: Arc<AssetRegistryData>,
}

impl AssetRegistry {
    pub fn new() -> Self {
        Self {
            data: Arc::new(AssetRegistryData {
                mappings: RwLock::new(Mappings::new()),
            }),
        }
    }

    fn read_mappings(&self) -> RwLockReadGuard<'_, Mappings> {
        self.data
            .mappings
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn write_mappings(&self) -> RwLockWriteGuard<'_, Mappings> {
        self.data
            .mappings
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn register_asset(&self, alias: &str, path: &str) {
        self.write_mappings()
            .alias_to_path
            .insert(alias.to_string(), path.to_string());
    }

    pub fn unregister_asset(&self, alias: &str) {
        self.write_mappings().alias_to_path.remove(alias);
    }

    pub fn get_path(&self, alias: &str) -> Option<String> {
        self.read_mappings()
            .alias_to_path
            .get(alias)
            .map(|s| s.to_owned())
    }

    pub fn load<A>(&self, alias: &str, server: &AssetServer) -> Option<Handle<A>>
    where
        A: Asset,
    {
        self.read_mappings()
            .alias_to_path
            .get(alias)
            .map(|p| server.load::<A>(p))
    }
}
