//! The Bevy side: installing a backend, and polling storage futures from systems.

use crate::{MemoryStorage, StorageBackend, StorageError, UserStorage};
use bevy::{
    prelude::*,
    tasks::{ConditionalSend, ConditionalSendFuture, IoTaskPool, Task, futures::check_ready},
};
use std::path::{Path, PathBuf};

/// The directory under the persistent data directory that the filesystem backend owns.
pub const STORAGE_DIR: &str = "storage";

/// Installs the app's user storage as the [`UserStorage`] resource, and logs once it is ready or
/// why it is not.
///
/// [`StoragePlugin::local`] is the default: the filesystem backend in [`STORAGE_DIR`] under the
/// persistent data directory, or the in-memory backend where there is none (the browser), so
/// game code never branches. [`StoragePlugin::with_backend`] installs any other backend, such as
/// a cloud one built with the platform user it needs.
pub struct StoragePlugin {
    backend: Backend,
}

enum Backend {
    Local(Option<PathBuf>),
    Custom(UserStorage),
}

impl StoragePlugin {
    /// The filesystem backend under `persistent_data_path`, or memory when it is `None`. The store
    /// opens when the plugin builds.
    pub fn local(persistent_data_path: Option<&Path>) -> Self {
        Self {
            backend: Backend::Local(persistent_data_path.map(Path::to_path_buf)),
        }
    }

    pub fn with_backend(backend: impl StorageBackend) -> Self {
        Self {
            backend: Backend::Custom(UserStorage::new(backend)),
        }
    }
}

impl Plugin for StoragePlugin {
    fn build(&self, app: &mut App) {
        let storage = match &self.backend {
            Backend::Custom(storage) => storage.clone(),
            Backend::Local(path) => local_storage(path.as_deref()),
        };
        app.insert_resource(storage)
            .add_systems(Startup, report_when_ready);
    }
}

fn local_storage(persistent_data_path: Option<&Path>) -> UserStorage {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = persistent_data_path {
        let root = path.join(STORAGE_DIR);
        info!("user storage: {}", root.display());
        return UserStorage::new(crate::FsStorage::open(root));
    }
    #[cfg(target_arch = "wasm32")]
    let _ = persistent_data_path;
    info!("user storage: in memory, lost on exit (no persistent data directory)");
    UserStorage::new(MemoryStorage::new())
}

fn report_when_ready(storage: Res<UserStorage>) {
    let storage = storage.clone();
    IoTaskPool::get()
        .spawn(async move {
            match storage.ready().await {
                Ok(()) => info!("user storage: {}", storage.status()),
                Err(error @ StorageError::Locked(_)) => {
                    warn!("user storage is unavailable: {error}");
                }
                Err(error) => error!("user storage is unavailable: {error}"),
            }
        })
        .detach();
}

/// A storage future running on the io task pool, for a system to poll each frame without
/// blocking it. Works the same on native and wasm.
///
/// ```ignore
/// let task = StorageTask::spawn(storage.list(&saves, Some(&meta)));
/// // later, in a system:
/// if let Some(result) = task.poll() { /* ... */ }
/// ```
pub struct StorageTask<T>(Option<Task<T>>);

impl<T: ConditionalSend + 'static> StorageTask<T> {
    pub fn spawn(future: impl ConditionalSendFuture<Output = T> + 'static) -> Self {
        Self(Some(IoTaskPool::get().spawn(future)))
    }

    /// The output the first time it is polled after the future finished; `None` before that, and
    /// after, since a task must not be polled once it is done.
    pub fn poll(&mut self) -> Option<T> {
        let output = check_ready(self.0.as_mut()?)?;
        self.0 = None;
        Some(output)
    }

    /// Until [`StorageTask::poll`] has returned the output.
    pub fn is_pending(&self) -> bool {
        self.0.is_some()
    }
}
