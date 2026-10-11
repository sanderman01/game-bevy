//! Files the alias walk skips because a caller's own format owns them.

use std::path::Path;

/// Sidecar extensions skipped unless a caller says otherwise. `license` is the REUSE convention:
/// `oak_bark.png.license` holds the licensing of `oak_bark.png`.
pub const DEFAULT_IGNORED_SIDECAR_EXTENSIONS: &[&str] = &["license"];

/// Files the walk must never turn into an asset, on top of `_alias_rules.toml`, `*.alias` and
/// `*.meta`, which it always skips.
///
/// Without these, a permissive folder rule sweeps them up as content: `{stem}` of
/// `oak_bark.png.license` is `oak_bark`, which collides with the asset it describes. Every name and extension
/// is matched case-insensitively, because macOS and Windows preserve whatever case a file was
/// saved in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredFiles {
    /// Exact file names, such as `manifest.toml`.
    pub file_names: Vec<String>,
    /// Extensions, without the dot, of sidecars that describe the file next to them.
    pub sidecar_extensions: Vec<String>,
}

impl Default for IgnoredFiles {
    fn default() -> Self {
        Self {
            file_names: Vec::new(),
            sidecar_extensions: DEFAULT_IGNORED_SIDECAR_EXTENSIONS
                .iter()
                .map(|e| (*e).to_owned())
                .collect(),
        }
    }
}

impl IgnoredFiles {
    /// True if `file_name` is a name or carries a sidecar extension listed here.
    pub fn matches(&self, file_name: &str) -> bool {
        let extension = Path::new(file_name).extension().and_then(|e| e.to_str());
        self.file_names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(file_name))
            || extension.is_some_and(|extension| {
                self.sidecar_extensions
                    .iter()
                    .any(|ignored| ignored.eq_ignore_ascii_case(extension))
            })
    }
}
