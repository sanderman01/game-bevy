use bevy::{ecs::resource::Resource, platform::collections::HashMap};

/// Maps a package-namespaced asset alias such as `core::airship` to a path under the asset root.
///
/// Plain `ResMut` on purpose: an earlier version wrapped an `RwLock` in an `Arc` and mutated
/// through `Res`, which let the scheduler run readers alongside a writer. Bevy's borrow rules do
/// that job correctly and visibly.
#[derive(Resource, Debug, Default)]
pub struct AssetRegistry {
    alias_to_path: HashMap<String, String>,
}

impl AssetRegistry {
    /// Registers `alias`, replacing any path already registered under it.
    pub fn register_asset(&mut self, alias: &str, path: &str) {
        self.alias_to_path
            .insert(alias.to_string(), path.to_string());
    }

    pub fn unregister_asset(&mut self, alias: &str) {
        self.alias_to_path.remove(alias);
    }

    pub fn get_path(&self, alias: &str) -> Option<&str> {
        self.alias_to_path.get(alias).map(String::as_str)
    }
}
