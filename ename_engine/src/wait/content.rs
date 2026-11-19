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
