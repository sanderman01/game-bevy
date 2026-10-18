//! Argument parsing for the `ename_xtask` binary.
//!
//! No argument-parsing crate: a few flat subcommands with positional arguments and a flag or two
//! do not need one, and a hand-rolled parser takes a plain iterator of `String`, which a test can build
//! without touching `std::env`.

use std::fmt::Display;
use std::path::PathBuf;

/// One invocation of `cargo xtask <name> ...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `ename_check` -- non-zero exit if the content tree has a problem or a contested alias.
    Check,
    /// `ename_fix` -- writes missing `.alias` files and assigns missing guids.
    Fix,
    /// `ename_content_build` -- stub. Will bake `content-index.ron` for a shipping build.
    ContentBuild,
    /// `ename_content_list` -- dumps the resolved alias index.
    List,
    /// `ename_mv <from> <to>` -- moves an asset and its `.meta`, `.alias` and `.license` sidecars.
    Mv { from: PathBuf, to: PathBuf },
    /// `ename_dep_src <crate>` -- prints the source directory of each locked version of a crate.
    DepSrc { name: String },
    /// `ename_image_info <asset>...` -- size, format, channel ranges and alpha of each image.
    ImageInfo { assets: Vec<String> },
    /// `ename_gltf_info <asset> [--mesh <substring>] [--images]` -- what is inside a glTF file.
    GltfInfo {
        asset: String,
        mesh: Option<String>,
        images: bool,
    },
}

/// Why the command line could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    NoCommand,
    Unknown(String),
    MvNeedsTwoPaths,
    DepSrcNeedsName,
    ImageInfoNeedsAsset,
    GltfInfoNeedsOneAsset,
    MeshNeedsSubstring,
    UnknownFlag(String),
}

impl Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCommand => write!(
                f,
                "usage: cargo xtask <ename_check|ename_fix|ename_content_build|ename_content_list|ename_mv <from> <to>|ename_dep_src <crate>|ename_image_info <asset>...|ename_gltf_info <asset> [--mesh <substring>] [--images]>"
            ),
            Self::Unknown(name) => write!(f, "unknown subcommand `{name}`"),
            Self::MvNeedsTwoPaths => write!(f, "ename_mv needs a <from> and a <to> path"),
            Self::DepSrcNeedsName => write!(f, "ename_dep_src needs a <crate> name"),
            Self::ImageInfoNeedsAsset => write!(
                f,
                "ename_image_info needs at least one <asset>: a path, an asset path or an alias"
            ),
            Self::GltfInfoNeedsOneAsset => write!(
                f,
                "ename_gltf_info needs exactly one <asset>: a path, an asset path or an alias"
            ),
            Self::MeshNeedsSubstring => write!(f, "--mesh needs a <substring>"),
            Self::UnknownFlag(flag) => write!(f, "unknown flag `{flag}`"),
        }
    }
}

/// Parses argv, without the program name, into a [`Command`].
pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Command, CliError> {
    let name = args.next().ok_or(CliError::NoCommand)?;
    match name.as_str() {
        "ename_check" => Ok(Command::Check),
        "ename_fix" => Ok(Command::Fix),
        "ename_content_build" => Ok(Command::ContentBuild),
        "ename_content_list" => Ok(Command::List),
        "ename_mv" => {
            let from = args.next().ok_or(CliError::MvNeedsTwoPaths)?;
            let to = args.next().ok_or(CliError::MvNeedsTwoPaths)?;
            Ok(Command::Mv {
                from: from.into(),
                to: to.into(),
            })
        }
        "ename_dep_src" => {
            let name = args
                .next()
                .filter(|name| !name.is_empty())
                .ok_or(CliError::DepSrcNeedsName)?;
            Ok(Command::DepSrc { name })
        }
        "ename_image_info" => {
            let assets: Vec<String> = args.filter(|asset| !asset.is_empty()).collect();
            if assets.is_empty() {
                return Err(CliError::ImageInfoNeedsAsset);
            }
            Ok(Command::ImageInfo { assets })
        }
        "ename_gltf_info" => {
            let mut assets = Vec::new();
            let mut mesh = None;
            let mut images = false;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--mesh" => {
                        let substring = args
                            .next()
                            .filter(|s| !s.is_empty())
                            .ok_or(CliError::MeshNeedsSubstring)?;
                        mesh = Some(substring);
                    }
                    "--images" => images = true,
                    flag if flag.starts_with("--") => {
                        return Err(CliError::UnknownFlag(flag.to_owned()));
                    }
                    _ => assets.push(arg),
                }
            }
            let [asset] =
                <[String; 1]>::try_from(assets).map_err(|_| CliError::GltfInfoNeedsOneAsset)?;
            Ok(Command::GltfInfo {
                asset,
                mesh,
                images,
            })
        }
        other => Err(CliError::Unknown(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::{CliError, Command, parse};

    fn args(words: &[&str]) -> impl Iterator<Item = String> {
        words
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn each_flat_subcommand_parses() {
        assert_eq!(parse(args(&["ename_check"])), Ok(Command::Check));
        assert_eq!(parse(args(&["ename_fix"])), Ok(Command::Fix));
        assert_eq!(
            parse(args(&["ename_content_build"])),
            Ok(Command::ContentBuild)
        );
        assert_eq!(parse(args(&["ename_content_list"])), Ok(Command::List));
    }

    #[test]
    fn mv_takes_two_positional_paths() {
        assert_eq!(
            parse(args(&["ename_mv", "a/b.png", "a/c.png"])),
            Ok(Command::Mv {
                from: "a/b.png".into(),
                to: "a/c.png".into()
            })
        );
    }

    #[test]
    fn mv_without_both_paths_is_an_error() {
        assert_eq!(
            parse(args(&["ename_mv", "a/b.png"])),
            Err(CliError::MvNeedsTwoPaths)
        );
        assert_eq!(parse(args(&["ename_mv"])), Err(CliError::MvNeedsTwoPaths));
    }

    #[test]
    fn dep_src_takes_one_crate_name() {
        assert_eq!(
            parse(args(&["ename_dep_src", "big_space"])),
            Ok(Command::DepSrc {
                name: "big_space".into()
            })
        );
        assert_eq!(
            parse(args(&["ename_dep_src"])),
            Err(CliError::DepSrcNeedsName)
        );
        assert_eq!(
            parse(args(&["ename_dep_src", ""])),
            Err(CliError::DepSrcNeedsName)
        );
    }

    #[test]
    fn image_info_takes_one_or_more_assets() {
        assert_eq!(
            parse(args(&["ename_image_info", "a.png", "core::b"])),
            Ok(Command::ImageInfo {
                assets: vec!["a.png".into(), "core::b".into()]
            })
        );
        assert_eq!(
            parse(args(&["ename_image_info"])),
            Err(CliError::ImageInfoNeedsAsset)
        );
    }

    #[test]
    fn gltf_info_takes_one_asset_and_flags_in_any_order() {
        assert_eq!(
            parse(args(&[
                "ename_gltf_info",
                "--images",
                "core::a",
                "--mesh",
                "Tree"
            ])),
            Ok(Command::GltfInfo {
                asset: "core::a".into(),
                mesh: Some("Tree".into()),
                images: true
            })
        );
        assert_eq!(
            parse(args(&["ename_gltf_info", "a.glb"])),
            Ok(Command::GltfInfo {
                asset: "a.glb".into(),
                mesh: None,
                images: false
            })
        );
        assert_eq!(
            parse(args(&["ename_gltf_info", "a.glb", "b.glb"])),
            Err(CliError::GltfInfoNeedsOneAsset)
        );
        assert_eq!(
            parse(args(&["ename_gltf_info", "a.glb", "--mesh"])),
            Err(CliError::MeshNeedsSubstring)
        );
        assert_eq!(
            parse(args(&["ename_gltf_info", "a.glb", "--json"])),
            Err(CliError::UnknownFlag("--json".into()))
        );
    }

    #[test]
    fn no_subcommand_is_an_error() {
        assert_eq!(parse(args(&[])), Err(CliError::NoCommand));
    }

    #[test]
    fn an_unrecognised_name_is_an_error() {
        assert_eq!(
            parse(args(&["content"])),
            Err(CliError::Unknown("content".into()))
        );
    }
}
