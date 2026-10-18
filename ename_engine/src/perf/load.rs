//! Machine load around a sample window: other processes' CPU time through `sysinfo`, and their GPU
//! time from the OS's own counters (Linux DRM fdinfo; `None` on Windows and macOS for now).

#[cfg(target_os = "linux")]
pub(crate) mod drm;

use std::{
    collections::{HashMap, HashSet},
    thread,
    time::Duration,
};

use serde::Serialize;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

#[cfg(target_os = "linux")]
use drm::GpuSnapshot;

/// Processes listed in `top`, busiest first.
const TOP_COUNT: usize = 3;
/// Cores a process must use on average to be listed in `top`.
const TOP_MIN_CORES: f64 = 0.25;
/// Other processes' cores above which the machine counts as busy.
const BUSY_CORES: f64 = 1.0;
/// Other processes' GPU time, as a fraction of the window, above which the machine counts as busy.
const BUSY_GPU_FRACTION: f64 = 0.05;

/// No per-process GPU source on this OS yet, so there is never a snapshot.
#[cfg(not(target_os = "linux"))]
#[derive(Clone, Debug)]
enum GpuSnapshot {}

#[cfg(not(target_os = "linux"))]
impl GpuSnapshot {
    fn take(_own_pid: u32) -> Option<Self> {
        None
    }

    fn diff(_before: &Self, after: &Self, _window: Duration) -> GpuUse {
        match *after {}
    }
}

/// Other processes' GPU use over a window.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GpuUse {
    /// Engine time per pid, in ms.
    pub per_pid: HashMap<u32, f64>,
    /// Pids holding a DRM device whose driver reports no usage.
    pub unmeasured: HashSet<u32>,
}

/// The processes and counters at one moment. Compare two with [`LoadSnapshot::diff`].
#[derive(Clone, Debug)]
pub(crate) struct LoadSnapshot {
    loadavg_1m: Option<f64>,
    cpus: usize,
    /// Every process except this one, by pid.
    processes: HashMap<u32, ProcessCpu>,
    gpu: Option<GpuSnapshot>,
}

#[derive(Clone, Debug)]
struct ProcessCpu {
    name: String,
    /// Accumulated CPU time, in ms.
    cpu_ms: u64,
    /// Started by this process, directly or not, such as `video.record`'s ffmpeg.
    child: bool,
}

impl LoadSnapshot {
    /// Lists every process and reads its counters. Blocking file IO: run it on a task, never on
    /// the main thread during a window.
    pub fn take() -> Self {
        let own_pid = std::process::id();
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().without_tasks(),
        );
        let parents: HashMap<u32, u32> = system
            .processes()
            .iter()
            .filter_map(|(pid, process)| Some((pid.as_u32(), process.parent()?.as_u32())))
            .collect();
        let processes = system
            .processes()
            .iter()
            .filter(|(pid, process)| pid.as_u32() != own_pid && process.thread_kind().is_none())
            .map(|(pid, process)| {
                let pid = pid.as_u32();
                let cpu = ProcessCpu {
                    name: process.name().to_string_lossy().into_owned(),
                    cpu_ms: process.accumulated_cpu_time(),
                    child: descends_from(pid, own_pid, &parents),
                };
                (pid, cpu)
            })
            .collect();
        Self {
            loadavg_1m: load_average_1m(),
            cpus: thread::available_parallelism().map_or(1, usize::from),
            processes,
            gpu: GpuSnapshot::take(own_pid),
        }
    }

    /// What other processes used between `before` and `after`, over a sample window of `window`.
    pub fn diff(before: &Self, after: &Self, window: Duration) -> Load {
        let window_ms = (window.as_secs_f64() * 1000.0).max(f64::EPSILON);
        let mut busy: Vec<BusyProcess> = after
            .processes
            .iter()
            .map(|(&pid, process)| {
                // A process missing before started during the window, so all its time counts.
                let start = before.processes.get(&pid).map_or(0, |p| p.cpu_ms);
                let cores = process.cpu_ms.saturating_sub(start) as f64 / window_ms;
                BusyProcess {
                    name: process.name.clone(),
                    pid,
                    cores,
                    child: process.child,
                }
            })
            .collect();
        let other_cpu_cores: f64 = busy.iter().map(|p| p.cores).sum();
        busy.retain(|p| p.cores > TOP_MIN_CORES);
        busy.sort_by(|a, b| b.cores.total_cmp(&a.cores));
        busy.truncate(TOP_COUNT);
        for process in &mut busy {
            process.cores = round2(process.cores);
        }

        let gpu = match (&before.gpu, &after.gpu) {
            (Some(before), Some(after)) => Some(GpuSnapshot::diff(before, after, window)),
            _ => None,
        };
        let gpu_others_ms = gpu.as_ref().map(|gpu| gpu.per_pid.values().sum::<f64>());
        let gpu_top = gpu.map_or_else(Vec::new, |gpu| after.gpu_processes(gpu));

        let machine_busy = other_cpu_cores > BUSY_CORES
            || gpu_others_ms.is_some_and(|ms| ms > BUSY_GPU_FRACTION * window_ms);
        Load {
            loadavg_1m: after.loadavg_1m.map(round2),
            cpus: after.cpus,
            other_cpu_cores: round2(other_cpu_cores),
            top: busy,
            gpu_others_ms: gpu_others_ms.map(round2),
            gpu_top,
            machine_busy,
        }
    }

    /// Processes that used the GPU, busiest first; unmeasured ones last, without a number.
    fn gpu_processes(&self, gpu: GpuUse) -> Vec<GpuProcess> {
        let describe = |pid: u32, gpu_ms: Option<f64>| {
            let process = self.processes.get(&pid);
            GpuProcess {
                name: process.map_or_else(String::new, |p| p.name.clone()),
                pid,
                gpu_ms,
                child: process.is_some_and(|p| p.child),
            }
        };
        let mut measured: Vec<(u32, f64)> = gpu
            .per_pid
            .into_iter()
            .filter(|&(_, ms)| ms > 0.0)
            .collect();
        measured.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        measured.truncate(TOP_COUNT);
        let mut unmeasured: Vec<u32> = gpu.unmeasured.into_iter().collect();
        unmeasured.sort_unstable();
        measured
            .into_iter()
            .map(|(pid, ms)| describe(pid, Some(round2(ms))))
            .chain(unmeasured.into_iter().map(|pid| describe(pid, None)))
            .collect()
    }
}

/// Other processes' use of the machine over a sample window. A part the platform cannot read is
/// `None`, never 0.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Load {
    pub loadavg_1m: Option<f64>,
    pub cpus: usize,
    /// Cores used by other processes on average.
    pub other_cpu_cores: f64,
    pub top: Vec<BusyProcess>,
    /// Engine time of other processes' GPU clients, in ms; `None` where the OS has no source.
    pub gpu_others_ms: Option<f64>,
    pub gpu_top: Vec<GpuProcess>,
    /// Other processes used over 1 core or over 5 % of the window in GPU time: `machine_busy`.
    #[serde(skip)]
    pub machine_busy: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct BusyProcess {
    pub name: String,
    pub pid: u32,
    pub cores: f64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub child: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct GpuProcess {
    pub name: String,
    pub pid: u32,
    /// `None` when its driver reports no usage.
    pub gpu_ms: Option<f64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub child: bool,
}

fn descends_from(mut pid: u32, ancestor: u32, parents: &HashMap<u32, u32>) -> bool {
    // Bounded, in case a pid was reused into a cycle between the two listings.
    for _ in 0..parents.len() {
        let Some(&parent) = parents.get(&pid) else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        pid = parent;
    }
    false
}

/// Windows has no load average; `sysinfo` reports zeros there.
#[cfg(windows)]
fn load_average_1m() -> Option<f64> {
    None
}

#[cfg(not(windows))]
fn load_average_1m() -> Option<f64> {
    Some(System::load_average().one)
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
