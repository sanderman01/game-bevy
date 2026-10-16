//! Who holds an instance's ports, and which pair a new instance gets.

use std::{
    net::{Ipv4Addr, TcpListener},
    time::Duration,
};

use rmcp::schemars;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{FIRST_EXTRA_PORT, Project, process, state::Instance};
use crate::brp::BrpClient;

/// How long a probe waits for a listener or an answer.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// What answers on a port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    Free,
    /// A game with BRP: its OS pid and run id.
    Game {
        os_pid: u32,
        pid: String,
    },
    /// Something that is not a BRP server, or a game too old to report its OS pid.
    Other,
}

/// The process holding a port: a game this launcher did not start.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct Holder {
    pub os_pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Unix seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
}

impl Holder {
    pub fn of(os_pid: u32) -> Self {
        let info = process::info(os_pid);
        Self {
            os_pid,
            exe: info
                .as_ref()
                .and_then(|info| info.exe.as_ref())
                .map(|exe| exe.display().to_string()),
            cwd: info
                .as_ref()
                .and_then(|info| info.cwd.as_ref())
                .map(|cwd| cwd.display().to_string()),
            started_at: info.map(|info| info.start_time),
        }
    }

    pub fn describe(&self) -> String {
        match &self.exe {
            Some(exe) => format!("os pid {} ({exe})", self.os_pid),
            None => format!("os pid {}", self.os_pid),
        }
    }
}

pub fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// A BRP client for `port` that gives up after [`PROBE_TIMEOUT`].
pub fn client(port: u16) -> BrpClient {
    BrpClient::with_timeout(url(port), PROBE_TIMEOUT)
}

/// Asks `port` and `port + 1` who holds them.
pub async fn probe(port: u16) -> Probe {
    #[derive(Deserialize)]
    struct Pid {
        pid: String,
        os_pid: u32,
    }
    let main_open = is_open(port).await;
    if !main_open {
        return if is_open(port + 1).await {
            Probe::Other
        } else {
            Probe::Free
        };
    }
    match client(port).call::<Pid>("game.pid.get", json!({})).await {
        Ok(answer) => Probe::Game {
            os_pid: answer.os_pid,
            pid: answer.pid,
        },
        Err(_) => Probe::Other,
    }
}

async fn is_open(port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            PROBE_TIMEOUT,
            tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
        )
        .await,
        Ok(Ok(_))
    )
}

/// The first pair from [`FIRST_EXTRA_PORT`] that no other instance records and nothing listens
/// on.
pub fn free_pair(project: &Project, instance: &str) -> anyhow::Result<u16> {
    let taken: Vec<u16> = Instance::all(project)
        .into_iter()
        .filter(|other| other.name != instance)
        .filter_map(|other| other.read().ok().flatten())
        .filter(|state| state.is_live())
        .flat_map(|state| [state.port, state.render_port])
        .collect();
    (0..200)
        .map(|index| FIRST_EXTRA_PORT + index * 2)
        .find(|&port| {
            !taken.contains(&port)
                && !taken.contains(&(port + 1))
                && can_bind(port)
                && can_bind(port + 1)
        })
        .ok_or_else(|| anyhow::anyhow!("no free port pair from {FIRST_EXTRA_PORT}"))
}

fn can_bind(port: u16) -> bool {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
}
