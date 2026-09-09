//! `airship.glb.alias` -- the per-asset file this project owns.
//!
//! Bevy owns `airship.glb.meta` and writes it through its own serializer, which reconstructs the
//! file from `AssetMeta` and drops every field it does not recognise. Anything of ours in there is
//! one run of somebody else's tool away from being deleted, so we keep a file nobody else claims.
//! Neither side ever writes the other's. See `scratch/content-addressing-design.md`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// The extension of the sidecar, without the dot. Claimed by nobody else, unlike `.toml`.
pub const ALIAS_EXTENSION: &str = "alias";

/// Who chose an asset's alias, and therefore whether tooling may ever change it.
///
/// A written-down alias is otherwise indistinguishable from a typed one, which leaves tooling
/// choosing between never rewriting -- making folder rules useless the moment they are applied --
/// and always rewriting, which silently destroys hand-chosen names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AliasOrigin {
    /// A human typed it. Tooling never rewrites it, not when the folder rule changes and not when
    /// the file moves. This is the default: an unflagged file was written by hand.
    #[default]
    Authored,
    /// Tooling derived it from the folder rule and wrote it down so it would stop moving. Tooling
    /// may rewrite it when the rule or the path changes.
    Derived,
}

/// The contents of one `.alias` file.
///
/// Every field is optional because the file is written incrementally: tooling assigns the guid,
/// a folder rule usually supplies the alias, and `include` appears only where it disagrees with
/// that rule. `deny_unknown_fields` is load-bearing -- a misspelt key that silently took the
/// default is precisely the failure the manifest format had.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AliasFile {
    /// Stable identity across a move or rename. Nothing at runtime addresses an asset by it;
    /// tooling uses it to notice a file moved and to rewrite the aliases derived from its old
    /// path. `Option` because only `xtask content fix` assigns one, in phase 4.
    pub guid: Option<Uuid>,
    /// Overrides whatever the folder rule would have derived.
    pub alias: Option<String>,
    #[serde(default)]
    pub alias_origin: AliasOrigin,
    /// Overrides the folder rule's `include` patterns for this one file. `None` means "whatever
    /// the rule says".
    pub include: Option<bool>,
    /// Reserved for per-platform and per-build-profile import settings, unused until a second
    /// target exists. Typed rather than skipped so the shape is validated today and adding the
    /// machinery later is not a format break.
    #[serde(default)]
    pub profiles: BTreeMap<String, toml::Table>,
}

/// The asset a `.alias` sidecar belongs to, given the sidecar's file name.
///
/// The full file name is used, `airship.glb.alias` and not `airship.alias`, which is Bevy's
/// convention for `.meta` and which stops `abc.png` and `abc.jpg` fighting over one sidecar.
pub fn alias_sidecar_target(file_name: &str) -> Option<&str> {
    let target = file_name.strip_suffix(".alias")?;
    (!target.is_empty()).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::{AliasFile, AliasOrigin, alias_sidecar_target};
    use uuid::Uuid;

    fn parse(text: &str) -> Result<AliasFile, toml::de::Error> {
        toml::from_str(text)
    }

    #[test]
    fn a_full_file_parses_every_field() {
        let file = parse(
            r#"
            guid = "018f2c00-0000-7000-8000-000000000000"
            alias = "core::airship"
            alias_origin = "authored"
            include = true

            [profiles]
            "#,
        )
        .expect("parses");

        assert_eq!(
            file.guid,
            Some(Uuid::parse_str("018f2c00-0000-7000-8000-000000000000").unwrap())
        );
        assert_eq!(file.alias.as_deref(), Some("core::airship"));
        assert_eq!(file.alias_origin, AliasOrigin::Authored);
        assert_eq!(file.include, Some(true));
        assert!(file.profiles.is_empty());
    }

    /// The bulk case: a file written by tooling for an asset a folder rule already names.
    #[test]
    fn a_derived_file_parses() {
        let file = parse(
            r#"
            guid = "018f2c00-0000-7000-8000-000000000001"
            alias = "core::props/barrel"
            alias_origin = "derived"
            "#,
        )
        .expect("parses");

        assert_eq!(file.alias_origin, AliasOrigin::Derived);
        assert_eq!(
            file.include, None,
            "inclusion falls back to the folder rule"
        );
    }

    /// A file with no flag was written by a human. Tooling must never rewrite that alias, so the
    /// safe default is the one that forbids rewriting.
    #[test]
    fn alias_origin_defaults_to_authored() {
        let file = parse(r#"alias = "core::airship""#).expect("parses");
        assert_eq!(file.alias_origin, AliasOrigin::Authored);
    }

    /// The failure this whole design exists to stop: a misspelt key that silently does nothing.
    #[test]
    fn an_unknown_field_is_rejected() {
        let err = parse(
            r#"
            alias = "core::airship"
            alias_orign = "derived"
            "#,
        )
        .expect_err("a misspelt key must not be ignored");
        assert!(
            err.to_string().contains("alias_orign"),
            "the error must name the offending key, got: {err}"
        );
    }

    #[test]
    fn a_malformed_guid_is_rejected() {
        assert!(parse(r#"guid = "not-a-uuid""#).is_err());
    }

    /// `profiles` carries no machinery yet, but it is typed rather than skipped, so a file whose
    /// shape could never work fails now instead of on the day profiles are implemented.
    #[test]
    fn profiles_must_be_a_table_of_tables() {
        let file = parse(
            r#"
            [profiles.web]
            include = false
            "#,
        )
        .expect("a table of tables parses");
        assert_eq!(file.profiles.len(), 1);
        assert!(file.profiles.contains_key("web"));

        assert!(
            parse("profiles = 3").is_err(),
            "a scalar `profiles` must not parse"
        );
    }

    #[test]
    fn a_sidecar_names_the_asset_it_sits_beside() {
        assert_eq!(
            alias_sidecar_target("airship.glb.alias"),
            Some("airship.glb")
        );
        assert_eq!(alias_sidecar_target("barrel.png.alias"), Some("barrel.png"));
        assert_eq!(alias_sidecar_target("airship.glb"), None);
        assert_eq!(alias_sidecar_target(".alias"), None);
    }
}
