//! Linux GPU use per process from the kernel's DRM usage stats in `/proc/<pid>/fdinfo`
//! (`drm-usage-stats`). Stats belong to a DRM client, which several fds and processes can share.

use std::{
    collections::{HashMap, hash_map::Entry},
    fs,
    path::Path,
    time::Duration,
};

use super::GpuUse;

/// One DRM fd's fdinfo, parsed.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DrmFdinfo {
    pub pdev: String,
    /// `None` when the driver reports no usage stats.
    pub client_id: Option<u64>,
    /// `drm-engine-<name>` (amdgpu, i915, ...), ns of engine time.
    pub engines_ns: HashMap<String, u64>,
    /// `drm-cycles-<name>` with `drm-total-cycles-<name>` (xe): busy and total cycles.
    pub cycles: HashMap<String, (u64, u64)>,
}

impl DrmFdinfo {
    fn measured(&self) -> bool {
        self.client_id.is_some() && !(self.engines_ns.is_empty() && self.cycles.is_empty())
    }
}

/// `None` when the text is not a DRM fd's fdinfo.
pub(crate) fn parse_fdinfo(text: &str) -> Option<DrmFdinfo> {
    let mut is_drm = false;
    let mut info = DrmFdinfo::default();
    let mut busy_cycles = HashMap::new();
    let mut total_cycles = HashMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let number = || value.split_whitespace().next()?.parse::<u64>().ok();
        if key == "drm-driver" {
            is_drm = true;
        } else if key == "drm-pdev" {
            info.pdev = value.to_owned();
        } else if key == "drm-client-id" {
            info.client_id = number();
        } else if let Some(engine) = key.strip_prefix("drm-engine-") {
            if !engine.starts_with("capacity-")
                && let Some(ns) = number()
            {
                info.engines_ns.insert(engine.to_owned(), ns);
            }
        } else if let Some(engine) = key.strip_prefix("drm-total-cycles-") {
            total_cycles.extend(number().map(|n| (engine.to_owned(), n)));
        } else if let Some(engine) = key.strip_prefix("drm-cycles-") {
            busy_cycles.extend(number().map(|n| (engine.to_owned(), n)));
        }
    }
    info.cycles = busy_cycles
        .into_iter()
        .filter_map(|(engine, busy)| Some((engine.clone(), (busy, *total_cycles.get(&engine)?))))
        .collect();
    is_drm.then_some(info)
}

/// Every other process's DRM clients at one moment, each counted once.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GpuSnapshot {
    /// By (`drm-pdev`, `drm-client-id`).
    clients: HashMap<(String, u64), Client>,
    /// Processes holding a DRM fd with no usage stats and no measured client.
    unmeasured: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq)]
struct Client {
    /// The lowest pid seen holding it.
    pid: u32,
    info: DrmFdinfo,
}

impl GpuSnapshot {
    /// Reads every process's DRM fds. Processes we may not inspect are skipped.
    pub(super) fn take(own_pid: u32) -> Option<Self> {
        Some(Self::from_fds(read_fds(), own_pid))
    }

    /// Dedupes clients across fds and processes, and drops the ones `own_pid` holds.
    pub(crate) fn from_fds(fds: impl IntoIterator<Item = (u32, DrmFdinfo)>, own_pid: u32) -> Self {
        let mut fds: Vec<(u32, DrmFdinfo)> = fds.into_iter().collect();
        fds.sort_by_key(|(pid, _)| *pid);
        let own: Vec<(String, u64)> = fds
            .iter()
            .filter(|(pid, _)| *pid == own_pid)
            .filter_map(|(_, info)| Some((info.pdev.clone(), info.client_id?)))
            .collect();
        let mut snapshot = Self::default();
        for (pid, info) in fds {
            if pid == own_pid {
                continue;
            }
            if !info.measured() {
                snapshot.unmeasured.push(pid);
                continue;
            }
            let Some(client_id) = info.client_id else {
                continue;
            };
            let key = (info.pdev.clone(), client_id);
            if own.contains(&key) {
                continue;
            }
            if let Entry::Vacant(entry) = snapshot.clients.entry(key) {
                entry.insert(Client { pid, info });
            }
        }
        snapshot
            .unmeasured
            .retain(|pid| !snapshot.clients.values().any(|client| client.pid == *pid));
        snapshot.unmeasured.dedup();
        snapshot
    }

    /// Engine time per pid between the two snapshots. A client new in `after` counts its engine
    /// time from zero; a counter that went down counts 0; cycles need both snapshots.
    pub(crate) fn diff(before: &Self, after: &Self, window: Duration) -> GpuUse {
        let window_ms = window.as_secs_f64() * 1000.0;
        let mut usage = GpuUse {
            unmeasured: after.unmeasured.iter().copied().collect(),
            ..GpuUse::default()
        };
        for (key, client) in &after.clients {
            let previous = before.clients.get(key).map(|c| &c.info);
            let engine_ns: u64 = client
                .info
                .engines_ns
                .iter()
                .map(|(engine, &ns)| {
                    let start = previous.and_then(|p| p.engines_ns.get(engine)).copied();
                    ns.saturating_sub(start.unwrap_or(0))
                })
                .sum();
            let cycles_ms: f64 = client
                .info
                .cycles
                .iter()
                .filter_map(|(engine, &(busy, total))| {
                    let &(busy_start, total_start) = previous?.cycles.get(engine)?;
                    let busy = busy.checked_sub(busy_start)?;
                    let total = total.checked_sub(total_start).filter(|&t| t > 0)?;
                    Some((busy as f64 / total as f64).min(1.0) * window_ms)
                })
                .sum();
            *usage.per_pid.entry(client.pid).or_default() += engine_ns as f64 / 1e6 + cycles_ms;
        }
        usage
    }
}

/// Every process's fds that point at a DRM device, with their fdinfo.
fn read_fds() -> Vec<(u32, DrmFdinfo)> {
    let Ok(processes) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut fds = Vec::new();
    for process in processes.flatten() {
        let Some(pid) = process.file_name().to_str().and_then(|s| s.parse().ok()) else {
            continue;
        };
        let Ok(entries) = fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        for entry in entries.flatten() {
            let is_drm = fs::read_link(entry.path())
                .is_ok_and(|target| target.starts_with(Path::new("/dev/dri")));
            if !is_drm {
                continue;
            }
            let fdinfo = process.path().join("fdinfo").join(entry.file_name());
            if let Some(info) = fs::read_to_string(fdinfo)
                .ok()
                .as_deref()
                .and_then(parse_fdinfo)
            {
                fds.push((pid, info));
            }
        }
    }
    fds
}
