//! The run record: one small file per BRP port saying which run of the game holds the port, its
//! latest panic, and how it exited.
//!
//! The sidecar reads it when the game stops answering, to say how the game died, and the
//! launcher's crash report reads the panic from it. Keyed by port, because a game the user started
//! by hand has no launcher state directory, and the port is the one thing the game and the sidecar
//! both know. `ename_mcp` reads the same format without depending on this crate, so both test
//! against `ename_mcp/tests/fixtures/run_record.json`.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, TryLockError},
    time::{SystemTime, UNIX_EPOCH},
};

use bevy::prelude::*;
use ename_engine::{command::RunId, log::PanicReport};
use serde::{Deserialize, Serialize};

/// `<runtime dir>/ename/brp-<port>.json`, the temp dir standing in where there is no runtime dir
/// (macOS, Windows). `ename_mcp` computes the same path.
pub(crate) fn record_path(port: u16) -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("ename")
        .join(format!("brp-{port}.json"))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RunRecord {
    run: String,
    os_pid: u32,
    /// `std::env::current_exe`, which a liveness check compares, so a recycled pid never matches.
    exe: Option<PathBuf>,
    started_unix_ms: u64,
    /// The latest panic. A panic the process survived stays here after a clean exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    panic: Option<PanicRecord>,
    #[serde(default)]
    panics: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit: Option<ExitRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PanicRecord {
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    location: Option<String>,
    thread: String,
    at_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ExitRecord {
    /// 0 for `AppExit::Success`, else the code of `AppExit::Error`.
    code: u8,
    at_unix_ms: u64,
}

/// Writes the record at build, from a panic hook on every panic, and on `AppExit`.
///
/// Added after `BrpPortPlugin`, which exits before this builds when the port is taken, so a second
/// game never overwrites the record of the one that holds the port.
pub(crate) struct RunRecordPlugin {
    pub path: PathBuf,
}

/// The record and its file, shared with the panic hook.
#[derive(Resource, Clone)]
struct RecordFile {
    record: Arc<Mutex<RunRecord>>,
    path: Arc<Path>,
}

/// Where in `Last` the record reads `AppExit`: after everything in this crate that writes it.
#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RecordExit;

impl Plugin for RunRecordPlugin {
    fn build(&self, app: &mut App) {
        let Some(run) = app.world().get_resource::<RunId>() else {
            warn!("no run record: this app has no run id; add ename_engine's CommandPlugin");
            return;
        };
        let file = RecordFile {
            record: Arc::new(Mutex::new(RunRecord {
                run: run.as_str().to_owned(),
                os_pid: std::process::id(),
                exe: std::env::current_exe().ok(),
                started_unix_ms: now_ms(),
                panic: None,
                panics: 0,
                exit: None,
            })),
            path: self.path.clone().into(),
        };
        file.write(&file.record.lock().unwrap_or_else(|p| p.into_inner()));
        install_panic_hook(file.clone());
        app.insert_resource(file)
            .add_systems(Last, record_exit.in_set(RecordExit));
    }
}

impl RecordFile {
    /// Atomically: a reader never sees half a record.
    fn write(&self, record: &RunRecord) {
        let result = (|| {
            let dir = self.path.parent().unwrap_or(Path::new("."));
            std::fs::create_dir_all(dir)?;
            let temp = self
                .path
                .with_extension(format!("json.{}.tmp", std::process::id()));
            std::fs::write(&temp, serde_json::to_vec_pretty(record)?)?;
            std::fs::rename(&temp, &self.path)
        })();
        if let Err(err) = result {
            warn!("cannot write the run record {}: {err}", self.path.display());
        }
    }
}

/// Chained like the log buffer's: records the panic, then runs the hook it replaced.
fn install_panic_hook(file: RecordFile) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Never waits: a panic while this thread holds the lock would deadlock.
        let record = match file.record.try_lock() {
            Ok(record) => Some(record),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        };
        if let Some(mut record) = record {
            let report = PanicReport::of(info);
            record.panic = Some(PanicRecord {
                message: report.message,
                location: report.location,
                thread: report.thread,
                at_unix_ms: now_ms(),
            });
            record.panics += 1;
            file.write(&record);
        }
        previous(info);
    }));
}

fn record_exit(mut exits: MessageReader<AppExit>, file: Res<RecordFile>) {
    let Some(exit) = exits.read().last() else {
        return;
    };
    let code = match exit {
        AppExit::Success => 0,
        AppExit::Error(code) => code.get(),
    };
    let mut record = file.record.lock().unwrap_or_else(|p| p.into_inner());
    record.exit = Some(ExitRecord {
        code,
        at_unix_ms: now_ms(),
    });
    file.write(&record);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use ename_engine::command::CommandPlugin;
    use serde_json::Value;

    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../ename_mcp/tests/fixtures/run_record.json"
    );

    #[test]
    fn the_shared_fixture_round_trips() {
        let text = std::fs::read_to_string(FIXTURE).unwrap();
        let record: RunRecord = serde_json::from_str(&text).unwrap();
        assert_eq!(record.panics, 1);
        assert_eq!(
            serde_json::to_value(&record).unwrap(),
            serde_json::from_str::<Value>(&text).unwrap()
        );
    }

    fn read(path: &Path) -> RunRecord {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn written_at_build_and_on_exit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ename/brp-1.json");
        let mut app = App::new();
        app.add_plugins(CommandPlugin)
            .add_plugins(RunRecordPlugin { path: path.clone() });

        let record = read(&path);
        assert_eq!(record.run, app.world().resource::<RunId>().as_str());
        assert_eq!(record.os_pid, std::process::id());
        assert_eq!(record.exit, None);

        app.world_mut()
            .write_message(AppExit::Error(NonZero::new(3).unwrap()));
        app.update();
        assert_eq!(read(&path).exit.map(|exit| exit.code), Some(3));
    }
}
