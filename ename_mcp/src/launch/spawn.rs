//! The two ways the launcher starts a process, each with a Unix and a Windows arm.

use std::{path::Path, process::Stdio};

use anyhow::Context as _;
use process_wrap::tokio::{ChildWrapper, CommandWrap};
use tokio::process::Command;

use super::state::SUPERVISOR_LOG;

/// Copies this executable into the instance dir and starts `supervise <dir> <generation>` from
/// the copy, detached: its own session on Unix, no console and a new process group on Windows.
/// That cuts it from the caller's terminal, process group and lifetime. Returns its pid.
///
/// The copy is what keeps `cargo build -p ename_mcp` working while a game runs: Windows will not
/// replace a running `.exe`, and the supervisor lives as long as the game.
pub fn supervisor(dir: &Path, generation: u64) -> anyhow::Result<u32> {
    let exe = super::current_exe()?;
    let copy = dir.join(format!(
        "supervise-{generation}{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(&exe, &copy)
        .with_context(|| format!("cannot copy {} to {}", exe.display(), copy.display()))?;
    let log = std::fs::File::options()
        .create(true)
        .append(true)
        .open(dir.join(SUPERVISOR_LOG))?;

    let mut command = Command::new(&copy);
    command
        .arg("supervise")
        .arg(dir)
        .arg(generation.to_string())
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut command = CommandWrap::from(command);
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessSession);
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        command.wrap(process_wrap::tokio::CreationFlags(
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
        ));
    }
    let child = command.spawn().context("cannot start the supervisor")?;
    // Dropping the handle neither waits nor kills; tokio reaps the process when it exits.
    child
        .id()
        .context("the supervisor exited before its pid was read")
}

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
