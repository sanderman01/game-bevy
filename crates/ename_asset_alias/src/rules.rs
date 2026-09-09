//! `_alias_rules.toml` -- naming a whole folder's worth of assets with one line.
//!
//! Problem one with the old system was that every alias was hand-written in a manifest, which does
//! not scale past a few dozen assets. A folder rule is the answer: an alias template and a set of
//! include patterns, applied to every file in the directory it sits in.
//!
//! A rule inherits into subdirectories, and the nearest one wins. A deeper rule replaces its
//! parent wholesale rather than merging field by field, because merging makes "what does this file
//! get" unanswerable without reading every ancestor -- which is the class of problem the design is
//! trying to remove, not reproduce.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// The file that carries a folder rule, in the spelling everything should be written in.
///
/// The leading underscore sorts it to the top of a listing, and `alias` in the name says what the
/// rules are about, so a folder full of art does not leave a reader guessing. Neither the
/// underscore nor the extension is load-bearing -- the walk matches the whole name and parses TOML
/// regardless -- but the extension buys editor syntax highlighting for free. A leading dot is the
/// one thing the name may not have: `Vfs` listings hide dot-files, the way Bevy's own file reader
/// does, so a rule named that way would be invisible to the walk.
pub const RULES_FILE: &str = "_alias_rules.toml";

/// True for a file name that carries a folder rule.
///
/// Matched case-insensitively, the same way `.alias` and `.meta` are: macOS and Windows preserve
/// whatever case an author saved a file in, and a `_Alias_Rules.toml` treated as content instead
/// of as a rule is a silent phantom asset with a name nobody asked for. [`RULES_FILE`] is still the
/// spelling to write, and it wins where a case-sensitive filesystem carries more than one.
pub fn is_rules_file(file_name: &str) -> bool {
    file_name.eq_ignore_ascii_case(RULES_FILE)
}

/// The two placeholders an alias template may use. Anything else is a compile error on the rule.
const STEM: &str = "stem";
const PATH: &str = "path";

/// Why a rule could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RulesError {
    #[error(
        "unknown placeholder `{{{0}}}` in an alias template; expected `{{stem}}` or `{{path}}`"
    )]
    UnknownPlaceholder(String),
    #[error("unbalanced `{{` in the alias template: {0}")]
    UnbalancedBrace(String),
    #[error("invalid include pattern `{pattern}`: {message}")]
    BadPattern { pattern: String, message: String },
}

/// The contents of one `_alias_rules.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    /// The alias every included file in this folder gets, with `{stem}` and `{path}` expanded.
    /// `None` means the folder derives no aliases and only files with their own `.alias` are
    /// addressable.
    pub alias: Option<String>,
    /// Glob patterns, matched against the **file name** rather than the path. Defaults to `["*"]`:
    /// a rule that names an alias and says nothing else covers its whole folder.
    #[serde(default = "everything")]
    pub include: Vec<String>,
}

fn everything() -> Vec<String> {
    vec!["*".to_owned()]
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            alias: None,
            include: everything(),
        }
    }
}

/// A [`Rules`] with its template checked and its patterns compiled, bound to the directory it was
/// found in.
///
/// Validation happens here, once per rule, rather than in [`CompiledRules::alias_for`], which the
/// walk calls once per file. That is what lets `alias_for` be infallible and gives a bad template
/// one error naming the rule instead of one per asset.
#[derive(Debug, Clone)]
pub struct CompiledRules {
    /// Relative to the vfs root, like every other path in this crate. `{path}` is relative to it.
    dir: PathBuf,
    alias: Option<String>,
    include: Vec<glob::Pattern>,
}

impl CompiledRules {
    pub fn compile(rules: Rules, dir: impl Into<PathBuf>) -> Result<Self, RulesError> {
        if let Some(template) = &rules.alias {
            check_template(template)?;
        }
        let include = rules
            .include
            .iter()
            .map(|pattern| {
                glob::Pattern::new(pattern).map_err(|err| RulesError::BadPattern {
                    pattern: pattern.clone(),
                    message: err.to_string(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            dir: dir.into(),
            alias: rules.alias,
            include,
        })
    }

    /// Whether this rule claims a file, by name.
    pub fn includes(&self, file_name: &str) -> bool {
        self.include
            .iter()
            .any(|pattern| pattern.matches(file_name))
    }

    /// The alias this rule derives for `path`, which is relative to the vfs root.
    ///
    /// `None` when the rule carries no template, or when the path is not under the rule's
    /// directory, or when it is not valid UTF-8 -- none of which the walk can do anything about
    /// beyond skipping the file.
    pub fn alias_for(&self, path: &Path) -> Option<String> {
        let template = self.alias.as_deref()?;
        let file_name = path.file_name()?.to_str()?;
        let needs_path = template.contains("{path}");

        let mut expanded = template.replace("{stem}", stem(file_name));
        if needs_path {
            expanded = expanded.replace("{path}", &self.relative_alias_path(path)?);
        }
        Some(expanded)
    }

    /// The file's path relative to this rule's directory, `/` separated, with the last component
    /// reduced to its stem.
    fn relative_alias_path(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.dir).ok()?;
        let mut segments: Vec<&str> = Vec::new();
        for component in relative.components() {
            segments.push(component.as_os_str().to_str()?);
        }
        let (last, parents) = segments.split_last()?;
        let mut out = String::new();
        for parent in parents {
            out.push_str(parent);
            out.push('/');
        }
        out.push_str(stem(last));
        Some(out)
    }
}

/// The file name up to its **first** dot, so `airship.glb` and `terrain.height.png` both lose
/// their whole extension. Same convention Bevy uses for a full extension, and the same one the
/// alias reader uses when it synthesizes a meta.
fn stem(file_name: &str) -> &str {
    file_name.split('.').next().unwrap_or(file_name)
}

/// Rejects any placeholder that is not `{stem}` or `{path}`, and any unclosed `{`.
fn check_template(template: &str) -> Result<(), RulesError> {
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let close = after
            .find('}')
            .ok_or_else(|| RulesError::UnbalancedBrace(template.to_owned()))?;
        let name = &after[..close];
        if name != STEM && name != PATH {
            return Err(RulesError::UnknownPlaceholder(name.to_owned()));
        }
        rest = &after[close + 1..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CompiledRules, Rules, RulesError, is_rules_file};
    use std::path::Path;

    fn parse(text: &str) -> Result<Rules, toml::de::Error> {
        toml::from_str(text)
    }

    fn compiled(text: &str, dir: &str) -> CompiledRules {
        CompiledRules::compile(parse(text).expect("parses"), dir).expect("compiles")
    }

    #[test]
    fn a_rules_file_parses_both_fields() {
        let rules = parse(
            r#"
            alias = "core::props/{stem}"
            include = ["*.png", "*.glb"]
            "#,
        )
        .expect("parses");
        assert_eq!(rules.alias.as_deref(), Some("core::props/{stem}"));
        assert_eq!(rules.include, ["*.png", "*.glb"]);
    }

    /// A rule that names an alias and nothing else covers its whole folder. That is the common
    /// case, so it is the default rather than something every rule has to spell out.
    #[test]
    fn include_defaults_to_everything() {
        let rules = parse(r#"alias = "core::{stem}""#).expect("parses");
        assert_eq!(rules.include, ["*"]);
    }

    #[test]
    fn an_unknown_field_is_rejected() {
        let err = parse(
            r#"
            alias = "core::{stem}"
            includes = ["*.png"]
            "#,
        )
        .expect_err("a misspelt key must not be ignored");
        assert!(
            err.to_string().contains("includes"),
            "the error must name the offending key, got: {err}"
        );
    }

    /// The stem stops at the *first* dot, so `airship.glb` and `terrain.height.png` both lose
    /// their whole extension. That matches the full-extension convention the alias reader already
    /// uses when it synthesizes a meta.
    #[test]
    fn stem_stops_at_the_first_dot() {
        let rules = compiled(r#"alias = "core::{stem}""#, "base/core");
        assert_eq!(
            rules
                .alias_for(Path::new("base/core/airship.glb"))
                .as_deref(),
            Some("core::airship")
        );
        assert_eq!(
            rules
                .alias_for(Path::new("base/core/terrain.height.png"))
                .as_deref(),
            Some("core::terrain")
        );
    }

    /// `{stem}` alone collapses a nested tree onto one name per file, so `{path}` exists to keep
    /// the subdirectory. A rule at `base/core/props` sees `crates/barrel` for a file two levels
    /// down.
    #[test]
    fn the_path_placeholder_keeps_the_subdirectory() {
        let rules = compiled(r#"alias = "core::{path}""#, "base/core/props");
        assert_eq!(
            rules
                .alias_for(Path::new("base/core/props/crates/barrel.png"))
                .as_deref(),
            Some("core::crates/barrel")
        );
        assert_eq!(
            rules
                .alias_for(Path::new("base/core/props/barrel.png"))
                .as_deref(),
            Some("core::barrel")
        );
    }

    /// Whether `{path}` needs expanding is decided from the *template*, before `{stem}` is
    /// spliced in. A file name that happens to contain the literal text `{path}` must not be
    /// mistaken for the template using the placeholder.
    #[test]
    fn a_stem_containing_literal_braces_is_not_rescanned_for_path() {
        let rules = compiled(r#"alias = "core::{stem}""#, "base/core");
        assert_eq!(
            rules
                .alias_for(Path::new("base/core/weird{path}rest.png"))
                .as_deref(),
            Some("core::weird{path}rest")
        );
    }

    /// `{path}` is relative to the rule's own directory, so a path outside it has nothing to be
    /// relative to.
    #[test]
    fn alias_for_is_none_when_the_path_is_outside_the_rules_directory() {
        let rules = compiled(r#"alias = "core::{path}""#, "base/core");
        assert_eq!(rules.alias_for(Path::new("base/other/airship.glb")), None);
    }

    /// A file name that is not valid UTF-8 cannot appear in an alias, which is a `String`.
    #[cfg(unix)]
    #[test]
    fn alias_for_is_none_for_a_path_that_is_not_valid_utf8() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let rules = compiled(r#"alias = "core::{stem}""#, "base/core");
        let name = OsStr::from_bytes(b"fo\x80o.png");
        let path = Path::new("base/core").join(name);
        assert_eq!(rules.alias_for(&path), None);
    }

    /// A silently unexpanded placeholder would produce a few hundred live aliases with a brace in
    /// them. It fails once, at the rule.
    #[test]
    fn an_unknown_placeholder_is_rejected() {
        let rules = parse(r#"alias = "core::{stemm}""#).expect("parses");
        // `matches!` rather than `assert_eq!`: `CompiledRules` holds compiled glob patterns and is
        // deliberately not `PartialEq`, so the `Ok` half of the result cannot be compared.
        assert!(matches!(
            CompiledRules::compile(rules, "base/core"),
            Err(RulesError::UnknownPlaceholder(name)) if name == "stemm"
        ));
    }

    #[test]
    fn an_unbalanced_brace_is_rejected() {
        let rules = parse(r#"alias = "core::{stem""#).expect("parses");
        assert!(matches!(
            CompiledRules::compile(rules, "base/core"),
            Err(RulesError::UnbalancedBrace(_))
        ));
    }

    #[test]
    fn include_patterns_match_the_file_name() {
        let rules = compiled(
            r#"
            alias = "core::{stem}"
            include = ["*.png", "*.glb"]
            "#,
            "base/core",
        );
        assert!(rules.includes("airship.glb"));
        assert!(rules.includes("barrel.png"));
        assert!(!rules.includes("notes.txt"));
    }

    #[test]
    fn a_bad_glob_pattern_is_rejected() {
        let rules = parse(r#"include = ["["]"#).expect("parses");
        assert!(matches!(
            CompiledRules::compile(rules, "base/core"),
            Err(RulesError::BadPattern { .. })
        ));
    }

    /// A rule may exist only to widen or narrow `include`. With no template it derives nothing,
    /// and only files carrying their own `.alias` get an alias in that folder.
    #[test]
    fn a_rule_with_no_template_derives_nothing() {
        let rules = compiled(r#"include = ["*.glb"]"#, "base/core");
        assert_eq!(rules.alias_for(Path::new("base/core/airship.glb")), None);
    }

    /// macOS and Windows preserve whatever case a file was saved in. A `_Alias_Rules.toml` read as
    /// content rather than as a rule would quietly become an asset named after itself.
    #[test]
    fn the_rules_file_is_matched_case_insensitively() {
        assert!(is_rules_file("_alias_rules.toml"));
        assert!(is_rules_file("_Alias_Rules.toml"));
        assert!(is_rules_file("_ALIAS_RULES.TOML"));
        assert!(
            !is_rules_file("alias_rules.toml"),
            "the leading underscore is part of the name"
        );
        assert!(!is_rules_file("_alias_rules.tom"));
        assert!(
            !is_rules_file("_rules.toml"),
            "the old name is not a rule file, so an unmigrated one shows up as a stray asset \
             rather than silently half-working"
        );
    }
}
