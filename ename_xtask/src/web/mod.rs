//! `ename_web_build` and `ename_web_serve`: the browser build of a game binary, as one directory a
//! static web server can serve as it is. See `docs/web.md`.
//!
//! The output holds `index.html`, the wasm module and its JS glue, and `assets/`: the asset tree as
//! loose files plus the baked package discovery.

mod bake;
mod serve;

pub use bake::{bake, discover};
pub use serve::serve;

use ename_asset_package::BAKED_DISCOVERY_FILE;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// The wasm target, and the target dir the build uses so it never waits on `target/`'s lock.
pub const TARGET: &str = "wasm32-unknown-unknown";
pub const TARGET_DIR: &str = "target/web";
/// The workspace profiles the build uses, without and with `--release`.
pub const DEV_PROFILE: &str = "web-dev";
pub const RELEASE_PROFILE: &str = "web-release";
/// Where the output goes by default, relative to the workspace root.
pub const DEFAULT_OUT: &str = "target/web/dist";
/// The base name of the module and its glue inside the output.
const MODULE_NAME: &str = "game";
/// The `wasm-bindgen-cli-support` this tool links; the game's `wasm-bindgen` must be the same.
const WASM_BINDGEN_VERSION: &str = "0.2.127";

/// What `ename_web_build` builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebBuild {
    /// `None` reads `[workspace.metadata.ename.web] package` from the workspace `Cargo.toml`.
    pub package: Option<String>,
    pub release: bool,
    /// WebGL2 instead of WebGPU: no compute shaders, but more browsers.
    pub webgl2: bool,
    pub out: PathBuf,
}

/// Builds the browser version of the game in the workspace at the current directory.
pub fn web_build(
    options: &WebBuild,
    asset_root: &Path,
    search_paths: &[String],
) -> Result<(), String> {
    let manifest = read_toml(Path::new("Cargo.toml"))?;
    let package = match &options.package {
        Some(package) => package.clone(),
        None => default_package(&manifest)?,
    };
    check_wasm_bindgen_version(&read_toml(Path::new("Cargo.lock"))?)?;

    // Before the slow part, so a tree the browser cannot read fails at once.
    let assets_out = options.out.join("assets");
    remove_dir_if_present(&options.out)?;
    let packages = bake(
        asset_root,
        search_paths,
        &assets_out.join(BAKED_DISCOVERY_FILE),
    )?;
    println!("baked {packages} packages");

    let wasm = cargo_build(&package, options)?;
    println!("generating JS glue for {}", wasm.display());
    wasm_bindgen_cli_support::Bindgen::new()
        .input_path(&wasm)
        .out_name(MODULE_NAME)
        .web(true)
        .map_err(|err| err.to_string())?
        .typescript(false)
        .generate(&options.out)
        .map_err(|err| format!("wasm-bindgen: {err}"))?;

    let html = include_str!("index.html")
        .replace("{{TITLE}}", &package)
        .replace("{{NAME}}", MODULE_NAME)
        .replace(
            "{{NEEDS_WEBGPU}}",
            if options.webgl2 { "false" } else { "true" },
        );
    write(&options.out.join("index.html"), html.as_bytes())?;

    let files = mirror_assets(asset_root, &assets_out)?;
    println!("linked {files} asset files");
    println!(
        "done: {} (serve it with `cargo xtask ename_web_serve`)",
        options.out.display()
    );
    Ok(())
}

fn read_toml(path: &Path) -> Result<toml::Table, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    text.parse()
        .map_err(|err| format!("{}: {err}", path.display()))
}

fn default_package(manifest: &toml::Table) -> Result<String, String> {
    manifest
        .get("workspace")
        .and_then(|v| v.get("metadata"))
        .and_then(|v| v.get("ename"))
        .and_then(|v| v.get("web"))
        .and_then(|v| v.get("package"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            "no package: pass -p <package>, or set `[workspace.metadata.ename.web] package`"
                .to_owned()
        })
}

/// The glue only fits a module built against the same `wasm-bindgen`.
fn check_wasm_bindgen_version(lock: &toml::Table) -> Result<(), String> {
    let versions: Vec<&str> = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|package| package.get("name").and_then(toml::Value::as_str) == Some("wasm-bindgen"))
        .filter_map(|package| package.get("version").and_then(toml::Value::as_str))
        .collect();
    if versions
        .iter()
        .all(|version| *version == WASM_BINDGEN_VERSION)
    {
        Ok(())
    } else {
        Err(format!(
            "Cargo.lock has wasm-bindgen {versions:?} but this tool generates glue for \
             {WASM_BINDGEN_VERSION}: bump `wasm-bindgen-cli-support` in the engine workspace"
        ))
    }
}

/// Runs the wasm build and returns the module it wrote.
fn cargo_build(package: &str, options: &WebBuild) -> Result<PathBuf, String> {
    // `cargo run` sets `CARGO` to the cargo that launched us.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.args([
        "build",
        "-p",
        package,
        "--target",
        TARGET,
        "--target-dir",
        TARGET_DIR,
        // Without `dev`: it turns on Bevy's file watcher, which does not exist on wasm.
        "--no-default-features",
    ]);
    if !options.webgl2 {
        command.args(["--features", "webgpu"]);
    }
    let profile = if options.release {
        RELEASE_PROFILE
    } else {
        DEV_PROFILE
    };
    command.args(["--profile", profile]);
    println!("building {package} for {TARGET}");
    let status = command
        .status()
        .map_err(|err| format!("could not run cargo: {err}"))?;
    if !status.success() {
        return Err(format!("cargo build failed: {status}"));
    }

    let dir = Path::new(TARGET_DIR).join(TARGET).join(profile);
    // A bin keeps its name; `-` becomes `_` only for libraries, but accept either.
    [package.to_owned(), package.replace('-', "_")]
        .iter()
        .map(|name| dir.join(format!("{name}.wasm")))
        .find(|path| path.is_file())
        .ok_or_else(|| format!("no {package}.wasm in {}", dir.display()))
}

/// Hard-links (or copies, across filesystems) every servable file under `from` into `to`.
/// Dot-files and archives are left out: the browser cannot read archives (`bake` refuses them).
fn mirror_assets(from: &Path, to: &Path) -> Result<usize, String> {
    let mut count = 0;
    let mut stack = vec![PathBuf::new()];
    while let Some(relative) = stack.pop() {
        let dir = from.join(&relative);
        let entries = std::fs::read_dir(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|err| format!("{}: {err}", dir.display()))?;
            let name = entry.file_name();
            let name_text = name.to_string_lossy();
            if name_text.starts_with('.') || is_archive(&name_text) {
                continue;
            }
            let source = entry.path();
            let relative = relative.join(&name);
            // `metadata` follows symlinks: an asset tree is often one.
            let metadata =
                std::fs::metadata(&source).map_err(|err| format!("{}: {err}", source.display()))?;
            if metadata.is_dir() {
                stack.push(relative);
                continue;
            }
            let target = to.join(&relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| format!("{}: {err}", parent.display()))?;
            }
            // Link the file a symlink names, not the symlink, whose relative target would dangle.
            let source = std::fs::canonicalize(&source)
                .map_err(|err| format!("{}: {err}", source.display()))?;
            if std::fs::hard_link(&source, &target).is_err() {
                std::fs::copy(&source, &target).map_err(|err| {
                    format!("{} -> {}: {err}", source.display(), target.display())
                })?;
            }
            count += 1;
        }
    }
    Ok(count)
}

fn is_archive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".zip") || lower.ends_with(".tar")
}

fn remove_dir_if_present(dir: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("{}: {err}", dir.display())),
    }
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|err| format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_package_comes_from_workspace_metadata() {
        let manifest: toml::Table = "[workspace.metadata.ename.web]\npackage = \"game_bin\""
            .parse()
            .unwrap();
        assert_eq!(default_package(&manifest).unwrap(), "game_bin");
        assert!(default_package(&toml::Table::new()).is_err());
    }

    #[test]
    fn a_different_wasm_bindgen_is_refused() {
        let lock = |version: &str| -> toml::Table {
            format!("[[package]]\nname = \"wasm-bindgen\"\nversion = \"{version}\"")
                .parse()
                .unwrap()
        };
        assert!(check_wasm_bindgen_version(&lock(WASM_BINDGEN_VERSION)).is_ok());
        assert!(check_wasm_bindgen_version(&lock("0.2.1")).is_err());
    }

    #[test]
    fn mirroring_skips_dot_files_and_archives() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        std::fs::create_dir_all(from.join("pkg/sub")).unwrap();
        std::fs::write(from.join("pkg/sub/a.png"), "a").unwrap();
        std::fs::write(from.join("pkg/.hidden"), "").unwrap();
        std::fs::write(from.join("pkg.zip"), "").unwrap();
        let to = root.path().join("to");
        assert_eq!(mirror_assets(&from, &to).unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(to.join("pkg/sub/a.png")).unwrap(),
            "a"
        );
        assert!(!to.join("pkg.zip").exists());
    }
}
