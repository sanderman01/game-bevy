//! `ApplicationPlugin` puts the directories it was given into the world and creates them.

use bevy::prelude::*;
use ename_engine::application::{
    ApplicationPaths, ApplicationPlugin, OrganisationName, Paths, ProjectName,
};

const ORG: OrganisationName = OrganisationName::from_static("ename");
const PROJECT: ProjectName = ProjectName::from_static("application-paths-test");

fn app_with(paths: ApplicationPaths) -> App {
    let mut app = App::new();
    app.add_plugins(ApplicationPlugin { paths });
    app
}

#[test]
fn resource_holds_the_given_names_and_paths() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let cache = root.path().join("cache");
    let app = app_with(ApplicationPaths::new(
        ORG,
        PROJECT,
        Some(Paths::from_paths(data.clone(), cache.clone())),
    ));

    let paths = app.world().resource::<ApplicationPaths>();
    assert_eq!(paths.organisation(), &ORG);
    assert_eq!(paths.project(), &PROJECT);
    assert_eq!(paths.persistent_data_path(), Some(data.as_path()));
    assert_eq!(paths.cache_path(), Some(cache.as_path()));
    assert!(
        data.is_dir(),
        "the plugin creates the persistent data directory"
    );
    assert!(cache.is_dir(), "the plugin creates the cache directory");
}

#[test]
fn creation_failure_still_inserts_the_resource() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::write(&data, "").unwrap();
    let cache = root.path().join("cache");
    let app = app_with(ApplicationPaths::new(
        ORG,
        PROJECT,
        Some(Paths::from_paths(data.clone(), cache.clone())),
    ));

    let paths = app.world().resource::<ApplicationPaths>();
    assert_eq!(paths.persistent_data_path(), Some(data.as_path()));
    assert!(data.is_file());
    assert!(
        cache.is_dir(),
        "one failure does not stop the other directory"
    );
}
