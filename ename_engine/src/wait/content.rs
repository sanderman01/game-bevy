//! The `content_catalog` settle probe: the package scan, or the content descriptors it found,
//! still loading. Idle in a game without `AssetContentPlugin`.

use bevy::prelude::*;
use ename_asset_content::{ContentCatalog, ContentScanSettings};

use super::Activity;

pub(super) fn content_catalog_probe(
    settings: Option<Res<ContentScanSettings>>,
    catalog: Option<Res<ContentCatalog>>,
) -> Activity {
    let loading = settings.is_some() && catalog.is_none();
    Activity::idle().pending("catalog_loading", usize::from(loading))
}

#[cfg(test)]
mod tests {
    use bevy::{app::TaskPoolPlugin, asset::AssetPlugin, ecs::system::RunSystemOnce};
    use ename_asset_content::AssetContentPlugin;

    use super::*;

    fn probe(app: &mut App) -> Activity {
        app.world_mut()
            .run_system_once(content_catalog_probe)
            .unwrap()
    }

    #[test]
    fn busy_until_the_catalog_exists_and_idle_without_a_scan() {
        let mut app = App::new();
        assert!(
            probe(&mut app).is_idle(),
            "no package scan, nothing to wait for"
        );

        app.add_plugins((
            AssetContentPlugin::default(),
            TaskPoolPlugin::default(),
            AssetPlugin {
                watch_for_changes_override: Some(false),
                ..default()
            },
        ));
        assert_eq!(
            probe(&mut app).counters().collect::<Vec<_>>(),
            [("catalog_loading", 1)]
        );
        for _ in 0..2_000 {
            app.update();
            if app.world().contains_resource::<ContentCatalog>() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(probe(&mut app).is_idle());
    }
}
