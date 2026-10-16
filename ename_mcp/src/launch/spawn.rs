//! The two ways the launcher starts a process, each with a Unix and a Windows arm.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::Context as _;
use process_wrap::tokio::{ChildWrapper, CommandWrap};
use tokio::process::Command;

use super::state::SUPERVISOR_LOG;

/// Variables `cargo run` sets for the binary it runs. A supervisor started from `cargo game`
/// would otherwise hand them to its builds, which a supervisor started by the sidecar does not.
/// The dynamic-library path is left alone: cargo prepends to the user's own value, and the
/// two cannot be told apart.
const CARGO_RUN_VARS: [&str; 6] = [
    "CARGO_PKG_",
    "CARGO_MANIFEST_",
    "CARGO_BIN_",
    "CARGO_CRATE_",
    "CARGO_PRIMARY_PACKAGE",
    "CARGO_TARGET_TMPDIR",
];

/// Copies this executable into the instance dir under a unique temporary name. Done before any
/// lock is taken: the binary is tens of megabytes.
pub fn copy_exe(dir: &Path) -> anyhow::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(dir)?;
    let exe = super::current_exe()?;
    let copy = dir.join(format!(
        "{TEMP_PREFIX}{}-{}{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(&exe, &copy)
        .with_context(|| format!("cannot copy {} to {}", exe.display(), copy.display()))?;
    Ok(copy)
}

/// The prefix of [`copy_exe`]'s temporary names.
pub const TEMP_PREFIX: &str = "supervise-tmp-";

/// Renames `copy` (from [`copy_exe`]) to `supervise-<generation>` and starts
/// `supervise <dir> <generation>` from it, detached: its own session on Unix, no console, a new
/// process group and, where allowed, outside the caller's job on Windows. That cuts it from the
/// caller's terminal, process group and lifetime. Returns its pid.
///
/// Running from a copy is what keeps `cargo build -p ename_mcp` working while a game runs:
/// Windows will not replace a running `.exe`, and the supervisor lives as long as the game.
pub fn supervisor(dir: &Path, copy: &Path, generation: u64) -> anyhow::Result<u32> {
    let exe = dir.join(format!(
        "supervise-{generation}{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::rename(copy, &exe)
        .with_context(|| format!("cannot rename {} to {}", copy.display(), exe.display()))?;
    #[cfg(windows)]
    stop_inheriting_std_handles();

    let command = || -> anyhow::Result<CommandWrap> {
        let log = std::fs::File::options()
            .create(true)
            .append(true)
            .open(dir.join(SUPERVISOR_LOG))?;
        let mut command = Command::new(&exe);
        command
            .arg("supervise")
            .arg(dir)
            .arg(generation.to_string())
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if CARGO_RUN_VARS.iter().any(|prefix| name.starts_with(prefix)) {
                command.env_remove(&key);
            }
        }
        #[cfg_attr(windows, expect(unused_mut, reason = "only Unix adds a wrapper here"))]
        let mut command = CommandWrap::from(command);
        #[cfg(unix)]
        command.wrap(process_wrap::tokio::ProcessSession);
        Ok(command)
    };

    #[cfg(unix)]
    let child = command()?.spawn();
    #[cfg(windows)]
    let child = {
        use windows::Win32::System::Threading::{
            CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
        };
        let detached = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        // Out of the caller's job, so a kill-on-close job (cargo's, a terminal's) cannot take it
        // down. A job that forbids breaking away refuses with access denied.
        let mut wrapped = command()?;
        wrapped.wrap(process_wrap::tokio::CreationFlags(
            detached | CREATE_BREAKAWAY_FROM_JOB,
        ));
        match wrapped.spawn() {
            Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                let mut wrapped = command()?;
                wrapped.wrap(process_wrap::tokio::CreationFlags(detached));
                wrapped.spawn()
            }
            other => other,
        }
    };
    let child = child.context("cannot start the supervisor")?;
    // Dropping the handle neither waits nor kills; tokio reaps the process when it exits.
    child
        .id()
        .context("the supervisor exited before its pid was read")
}

#[cfg(windows)]
const ERROR_ACCESS_DENIED: i32 = windows::Win32::Foundation::ERROR_ACCESS_DENIED.0 as i32;

/// Starts `command` in a new process group (Unix) or job object (Windows), so one kill reaches it
/// and everything it starts, and never the supervisor.
pub fn grouped(command: Command) -> std::io::Result<Box<dyn ChildWrapper>> {
    let mut command = CommandWrap::from(command);
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    {
        // The supervisor has no console, so a console child would otherwise open a window.
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;
        command
            .wrap(process_wrap::tokio::JobObject)
            .wrap(process_wrap::tokio::CreationFlags(CREATE_NO_WINDOW));
    }
    command.spawn()
}

/// Makes this process's stdin, stdout and stderr non-inheritable.
///
/// std on Windows creates every child with `bInheritHandles = TRUE`, so a child inherits every
/// inheritable handle of the parent, not only the stdio it was given. A detached supervisor
/// would keep the caller's pipes open (the Bash tool's, the MCP transport), and the reader on
/// the other end would never see end of file. The supervisor calls this too, so the game does
/// not hold its log open. The handles the children are given are separate ones, which std marks
/// inheritable itself.
#[cfg(windows)]
#[allow(unsafe_code, reason = "Win32 handle flags; no safe wrapper exists")]
pub fn stop_inheriting_std_handles() {
    use windows::Win32::{
        Foundation::{HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation},
        System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: `GetStdHandle` only reads this process's standard handle table, and
        // `SetHandleInformation` only changes the inherit flag of a handle that table holds; a
        // missing or invalid handle is an error result, not undefined behaviour. Nothing is
        // closed or dereferenced.
        let _ = unsafe {
            GetStdHandle(which).and_then(|handle| {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))
            })
        };
    }
}
