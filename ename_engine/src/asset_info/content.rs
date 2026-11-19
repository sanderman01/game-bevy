//! `content.report` (the package scan as the running game sees it), `content.resolve` (alias to
//! path, path to alias) and `content.list` (the catalog entries of one content kind).

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use ename_asset_content::{ContentCatalog, ContentIndex, ContentReport, ContentScanSettings};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Items, limit, list::asset_path_string, not_scanned};
use crate::command::{Call, CommandError};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ReportParams {
    /// Most items in each list, 1 to 500. Default 50.
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Serialize, JsonSchema)]
pub(super) struct Report {
    /// `running` until the scan has finished, then `done`.
    scan: &'static str,
    /// The asset root the scan read, made absolute the way Bevy's file reader makes it.
    asset_root: String,
    /// `BEVY_ASSET_ROOT=<value>` when that variable is set; it moves the asset root.
    #[serde(skip_serializing_if = "Option::is_none")]
    asset_root_env: Option<String>,
    search_paths: Vec<SearchPath>,
    /// `null` when the game has no load order file.
    load_order_file: Option<LoadOrderFile>,
    /// The lists are empty while the scan runs.
    packages: Items<PackageItem>,
    disabled: Items<DisabledItem>,
    /// One line per alias two packages both claimed.
    contests: Items<String>,
    problems: Items<ProblemItem>,
    /// Aliases in the index.
    aliases: usize,
    /// `loading` until every content descriptor has loaded or failed, then `ready`.
    catalog: &'static str,
    /// Each registered content kind and its entries; empty while the catalog loads.
    kinds: Vec<KindItem>,
    /// Fields a loader skipped because it did not know them, per asset.
    unknown_fields: Items<UnknownFieldsItem>,
}

#[derive(Serialize, JsonSchema)]
struct KindItem {
    kind: String,
    /// Files ending in `.<extension>` are of this kind.
    extension: String,
    entries: Items<CatalogItem>,
}

#[derive(Serialize, JsonSchema)]
struct CatalogItem {
    /// The alias, which `alias://<id>` loads.
    id: String,
    package: String,
}

#[derive(Serialize, JsonSchema)]
struct UnknownFieldsItem {
    /// The asset path the loader read.
    asset: String,
    /// Field paths such as `parts.0.colour`.
    fields: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
struct SearchPath {
    path: String,
    /// Whether `<asset_root>/<path>` is a directory on disk.
    exists: bool,
    /// Packages found under it.
    packages: usize,
}

#[derive(Serialize, JsonSchema)]
struct LoadOrderFile {
    path: String,
    exists: bool,
}

#[derive(Serialize, JsonSchema)]
struct PackageItem {
    id: String,
    version: String,
    /// Under the asset root, with `/`.
    root: String,
    aliases: usize,
}

#[derive(Serialize, JsonSchema)]
struct DisabledItem {
    id: String,
    reason: String,
}

#[derive(Serialize, JsonSchema)]
struct ProblemItem {
    kind: String,
    path: String,
    detail: String,
}

pub(super) fn report(
    In(call): In<Call<ReportParams>>,
    world: &World,
) -> Result<Report, CommandError> {
    let limit = limit(call.params.limit)?;
    let settings = scan_settings(world)?;
    // `BEVY_ASSET_ROOT=.` gives `<cwd>/./assets`; drop the `.`.
    let asset_root: PathBuf = crate::asset_base_path()
        .join(settings.asset_root())
        .components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .collect();
    let report = world.get_resource::<ContentReport>();
    let packages = report.map_or(&[][..], |report| report.packages.as_slice());
    let search_paths = settings
        .search_paths()
        .iter()
        .map(|path| SearchPath {
            path: path.clone(),
            exists: asset_root.join(path).is_dir(),
            packages: packages
                .iter()
                .filter(|package| package.root.starts_with(path))
                .count(),
        })
        .collect();
    let empty = ContentReport::default();
    let report_or_empty = report.unwrap_or(&empty);
    let catalog = world.get_resource::<ContentCatalog>();
    Ok(Report {
        scan: if report.is_some() { "done" } else { "running" },
        asset_root: asset_root.display().to_string(),
        asset_root_env: std::env::var("BEVY_ASSET_ROOT")
            .ok()
            .map(|value| format!("BEVY_ASSET_ROOT={value}")),
        search_paths,
        load_order_file: settings.load_order_file().map(|path| LoadOrderFile {
            path: path.display().to_string(),
            exists: path.is_file(),
        }),
        packages: Items::of(
            report_or_empty.packages.iter().map(|package| PackageItem {
                id: package.id.clone(),
                version: package.version.to_string(),
                root: asset_path_string(&package.root),
                aliases: package.aliases,
            }),
            limit,
        ),
        disabled: Items::of(
            report_or_empty
                .disabled
                .iter()
                .map(|disabled| DisabledItem {
                    id: disabled.id.clone(),
                    reason: disabled.reason.to_string(),
                }),
            limit,
        ),
        contests: Items::of(
            report_or_empty.contests.iter().map(ToString::to_string),
            limit,
        ),
        problems: Items::of(
            report_or_empty.problems.iter().map(|problem| ProblemItem {
                kind: problem.kind.to_string(),
                path: asset_path_string(&problem.path),
                detail: problem.detail.clone(),
            }),
            limit,
        ),
        aliases: world
            .get_resource::<ContentIndex>()
            .map_or(0, ContentIndex::len),
        catalog: if catalog.is_some() {
            "ready"
        } else {
            "loading"
        },
        kinds: catalog.map_or_else(Vec::new, |catalog| {
            catalog
                .kinds()
                .iter()
                .map(|(kind, extension)| KindItem {
                    kind: kind.to_owned(),
                    extension: extension.to_owned(),
                    entries: Items::of(
                        catalog
                            .entries(kind)
                            .map(|entry| CatalogItem {
                                id: entry.id.clone(),
                                package: entry.package.clone(),
                            })
                            .collect::<Vec<_>>()
                            .into_iter(),
                        limit,
                    ),
                })
                .collect()
        }),
        unknown_fields: Items::of(
            report_or_empty
                .unknown_fields
                .iter()
                .map(|unknown| UnknownFieldsItem {
                    asset: unknown.asset.clone(),
                    fields: unknown.fields.clone(),
                }),
            limit,
        ),
    })
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListParams {
    /// A registered content kind, such as `map`.
    kind: String,
    /// Most entries returned, 1 to 500. Default 50.
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Serialize, JsonSchema)]
pub(super) struct Listed {
    kind: String,
    extension: String,
    /// Sorted by id.
    entries: Items<ListedEntry>,
}

#[derive(Serialize, JsonSchema)]
struct ListedEntry {
    /// The alias, which `alias://<id>` loads.
    id: String,
    package: String,
    /// The file that won the alias, under the asset root, with `/`.
    path: Option<String>,
}

pub(super) fn list(In(call): In<Call<ListParams>>, world: &World) -> Result<Listed, CommandError> {
    let limit = limit(call.params.limit)?;
    scan_settings(world)?;
    let Some(catalog) = world.get_resource::<ContentCatalog>() else {
        return Err(CommandError::failed(
            "content_catalog_loading",
            "the content catalog is not ready: the scan runs or descriptors still load",
        )
        .with_hint("`content.report` says `catalog: ready` once it is"));
    };
    let kind = call.params.kind;
    let Some((_, extension)) = catalog.kinds().iter().find(|(known, _)| *known == kind) else {
        let known: Vec<&str> = catalog.kinds().iter().map(|(kind, _)| kind).collect();
        return Err(CommandError::invalid(
            "unknown_kind",
            format!(
                "no content kind `{kind}`; registered: {}",
                if known.is_empty() {
                    "none".to_owned()
                } else {
                    known.join(", ")
                }
            ),
        ));
    };
    let index = world.get_resource::<ContentIndex>();
    let entries: Vec<ListedEntry> = catalog
        .entries(&kind)
        .map(|entry| ListedEntry {
            id: entry.id.clone(),
            package: entry.package.clone(),
            path: index
                .and_then(|index| index.resolve(&entry.id))
                .map(asset_path_string),
        })
        .collect();
    Ok(Listed {
        extension: extension.to_owned(),
        kind,
        entries: Items::of(entries.into_iter(), limit),
    })
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ResolveParams {
    /// An alias such as `examples::airship`: returns the path it resolves to.
    #[serde(default)]
    alias: Option<String>,
    /// A path under the asset root: returns the aliases claiming it.
    #[serde(default)]
    path: Option<String>,
    /// A substring of aliases or paths, ignoring case.
    #[serde(default)]
    contains: Option<String>,
    /// Most entries returned, 1 to 500. Default 50.
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Serialize, JsonSchema)]
pub(super) struct Resolved {
    /// Sorted by alias.
    entries: Vec<ResolvedEntry>,
    truncated: bool,
}

#[derive(Serialize, JsonSchema)]
struct ResolvedEntry {
    alias: String,
    /// Under the asset root, with `/`.
    path: String,
    /// The package whose root holds the path; `null` if none does.
    package: Option<String>,
}

pub(super) fn resolve(
    In(call): In<Call<ResolveParams>>,
    world: &World,
) -> Result<Resolved, CommandError> {
    let ResolveParams {
        alias,
        path,
        contains,
        limit: requested,
    } = call.params;
    let limit = limit(requested)?;
    let given = [alias.is_some(), path.is_some(), contains.is_some()];
    if given.iter().filter(|given| **given).count() != 1 {
        return Err(CommandError::invalid(
            "invalid_params",
            "give exactly one of `alias`, `path` and `contains`",
        ));
    }
    scan_settings(world)?;
    let Some(index) = world.get_resource::<ContentIndex>() else {
        return Err(not_scanned());
    };
    let path = path.map(|path| asset_path_string(Path::new(&path)));
    let contains = contains.map(|text| text.to_lowercase());
    let mut entries: Vec<(String, String)> = match &alias {
        // One lookup, not a scan of the whole index.
        Some(alias) => index
            .resolve(alias)
            .map(|path| (alias.clone(), asset_path_string(path)))
            .into_iter()
            .collect(),
        None => index
            .iter()
            .map(|(alias, path)| (alias, asset_path_string(path)))
            .filter(|(entry_alias, entry_path)| {
                path.as_deref().is_none_or(|path| path == entry_path)
                    && contains.as_deref().is_none_or(|text| {
                        entry_alias.to_lowercase().contains(text)
                            || entry_path.to_lowercase().contains(text)
                    })
            })
            .collect(),
    };
    entries.sort();
    let truncated = entries.len() > limit;
    let packages: Vec<(PathBuf, String)> = world
        .get_resource::<ContentReport>()
        .map(|report| {
            report
                .packages
                .iter()
                .map(|package| (package.root.clone(), package.id.clone()))
                .collect()
        })
        .unwrap_or_default();
    Ok(Resolved {
        entries: entries
            .into_iter()
            .take(limit)
            .map(|(alias, path)| ResolvedEntry {
                // The deepest root wins, should one package root sit inside another.
                package: packages
                    .iter()
                    .filter(|(root, _)| Path::new(&path).starts_with(root))
                    .max_by_key(|(root, _)| root.components().count())
                    .map(|(_, id)| id.clone()),
                alias: alias.to_owned(),
                path,
            })
            .collect(),
        truncated,
    })
}

fn scan_settings(world: &World) -> Result<&ContentScanSettings, CommandError> {
    world.get_resource::<ContentScanSettings>().ok_or_else(|| {
        CommandError::failed(
            "no_content_scan",
            "this game has no package scan (AssetContentPlugin is not added)",
        )
    })
}
