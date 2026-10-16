//! Facts about processes, the same way on Linux, macOS and Windows, through `sysinfo`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

/// One process, by pid and start time. A recorded process counts as alive only while a process
/// with that pid has that start time, so a recycled pid never matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRef {
    pub pid: u32,
    /// Seconds since the Unix epoch.
    pub start_time: u64,
}

/// What the launcher needs to know about a live process.
#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub start_time: u64,
    pub exe: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub parent: Option<u32>,
}

impl ProcessRef {
    /// The live process `pid`, or `None` when there is none.
    pub fn of(pid: u32) -> Option<Self> {
        Some(Self {
            pid,
            start_time: info(pid)?.start_time,
        })
    }

    /// This process.
    pub fn current() -> Option<Self> {
        Self::of(std::process::id())
    }

    pub fn is_alive(&self) -> bool {
        info(self.pid).is_some_and(|info| info.start_time == self.start_time)
    }

    /// Kills this process, after checking again that it is still the same one and that its
    /// executable passes `exe_ok`. Returns whether a kill was sent. SIGKILL on Unix,
    /// `TerminateProcess` on Windows; its children are not reached.
    pub fn kill_checked(&self, exe_ok: impl Fn(&Path) -> bool) -> anyhow::Result<bool> {
        let mut system = System::new();
        refresh(&mut system, self.pid);
        let Some(process) = system.process(Pid::from_u32(self.pid)) else {
            return Ok(false);
        };
        if process.start_time() != self.start_time || is_zombie(process.status()) {
            return Ok(false);
        }
        let exe = process.exe().map(strip_deleted);
        if !exe.as_deref().is_some_and(&exe_ok) {
            anyhow::bail!(
                "process {} is not the game binary ({}); not killing it",
                self.pid,
                exe.as_deref()
                    .map_or("unknown executable".into(), |e| e.display().to_string())
            );
        }
        Ok(process.kill())
    }
}

/// The live process `pid`, or `None` when there is none or it is a zombie.
pub fn info(pid: u32) -> Option<ProcessInfo> {
    let mut system = System::new();
    refresh(&mut system, pid);
    let process = system.process(Pid::from_u32(pid))?;
    if is_zombie(process.status()) {
        return None;
    }
    Some(ProcessInfo {
        start_time: process.start_time(),
        exe: process.exe().map(strip_deleted),
        cwd: process.cwd().map(Path::to_path_buf),
        parent: process.parent().map(Pid::as_u32),
    })
}

/// This process's parent.
pub fn parent_of_current() -> Option<ProcessRef> {
    ProcessRef::of(info(std::process::id())?.parent?)
}

pub fn boot_time() -> u64 {
    System::boot_time()
}

fn refresh(system: &mut System, pid: u32) {
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cwd(UpdateKind::OnlyIfNotSet),
    );
}

fn is_zombie(status: ProcessStatus) -> bool {
    matches!(status, ProcessStatus::Zombie | ProcessStatus::Dead)
}

/// Linux reports a replaced executable as `<path> (deleted)`.
fn strip_deleted(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_suffix(" (deleted)") {
        Some(stripped) => PathBuf::from(stripped),
        None => path.to_path_buf(),
    }
}
