//! JSON-RPC client for the game's Bevy Remote Protocol server.
//!
//! One `POST` per call. BRP handlers run as systems in the game's main schedule, so calls are
//! already serialized against the game thread and this client needs no locking of its own.

use anyhow::{Context as _, bail};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// Where the game listens when `RemoteHttpPlugin` is left at its defaults.
pub const DEFAULT_URL: &str = "http://127.0.0.1:15702";

/// A connection to one game process.
#[derive(Clone)]
pub struct BrpClient {
    http: reqwest::Client,
    url: String,
    next_id: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl BrpClient {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            url: url.into(),
            next_id: std::sync::Arc::default(),
        }
    }

    /// A client whose every request gives up after `timeout`, for probes.
    pub fn with_timeout(url: impl Into<String>, timeout: std::time::Duration) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .unwrap_or_default(),
            url: url.into(),
            next_id: std::sync::Arc::default(),
        }
    }

    /// Calls `method` and deserializes the `result` field.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> anyhow::Result<T> {
        let value = self.call_raw(method, params).await?;
        serde_json::from_value(value)
            .with_context(|| format!("BRP method `{method}` returned an unexpected result shape"))
    }

    /// Calls `method` and returns the raw `result` field.
    ///
    /// A transport failure is reported as "the game is not running", because that is what it
    /// means in practice and it is the answer the agent needs.
    pub async fn call_raw(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });

        let response = self
            .http
            .post(&self.url)
            .json(&request)
            .send()
            .await
            .with_context(|| {
                format!(
                    "cannot reach the game's remote server at {}. The game is not running, or \
                     runs on another port: game_status says which, and game_start (or `cargo \
                     game start`) starts it.",
                    self.url
                )
            })?;

        let body: serde_json::Map<String, Value> = response
            .json()
            .await
            .context("the game's remote server returned a body that is not JSON-RPC")?;

        // `contains_key` and not `Option<Value>`: several BRP methods answer with a literal
        // `null` result, which serde would otherwise render indistinguishable from no result.
        if let Some(result) = body.get("result") {
            return Ok(result.clone());
        }
        match body.get("error").cloned().map(serde_json::from_value) {
            Some(Ok(ResponseError {
                code,
                message,
                data,
            })) => Err(BrpError {
                method: method.to_owned(),
                code,
                message,
                data,
            }
            .into()),
            _ => bail!("BRP `{method}` returned neither a result nor an error"),
        }
    }
}

#[derive(Deserialize)]
struct ResponseError {
    code: i32,
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

/// JSON-RPC's code for a method the server does not have.
pub const METHOD_NOT_FOUND: i32 = -32601;

/// BRP's code for an id that names no entity in the world.
pub const ENTITY_NOT_FOUND: i32 = -23401;

/// An error the game's remote server returned, kept typed rather than flattened to a string so
/// that a caller can match on `code`: an id that names nothing is a different answer from a
/// call that went wrong, and the two deserve different wording.
#[derive(Debug)]
pub struct BrpError {
    pub method: String,
    pub code: i32,
    pub message: String,
    /// The error's structured detail, when the method sends one.
    pub data: Option<Value>,
}

impl std::fmt::Display for BrpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            method,
            code,
            message,
            data,
        } = self;
        write!(f, "BRP `{method}` failed ({code}): {message}")?;
        // The game's stable kind and its advice, which a caller formatting this would otherwise
        // lose.
        let field = |name: &str| data.as_ref()?.get(name)?.as_str();
        if let Some(kind) = field("kind") {
            write!(f, " ({kind})")?;
        }
        if let Some(hint) = field("hint") {
            write!(f, ". {hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for BrpError {}

/// The game this MCP session talks to: `ENAME_BRP_URL` when set, else the instance its owner
/// selected last (`selected/<owner>`, written by a successful start or restart), else 15702.
///
/// Each tool call takes one [`snapshot`](Self::snapshot) and uses it for every request it
/// makes, so a call never spans two games. A selection change applies to later calls only.
pub struct Endpoint {
    pinned: Option<String>,
    selection: Option<(crate::launch::Project, Option<String>)>,
    current: std::sync::Mutex<Selected>,
}

struct Selected {
    /// The stamps of the selection file and the selected instance's state when they were last
    /// read; `None` before the first read.
    stamp: Option<SelectionStamp>,
    /// When the selected instance's liveness was last checked.
    checked: Option<std::time::Instant>,
    client: BrpClient,
}

type SelectionStamp = (Option<crate::launch::Stamp>, Option<crate::launch::Stamp>);

/// A game can die without its state file changing (a supervisor killed with it), so liveness is
/// checked again at least this often.
const RECHECK: std::time::Duration = std::time::Duration::from_secs(2);

impl Endpoint {
    /// Always `client`.
    #[cfg(test)]
    pub fn fixed(client: BrpClient) -> Self {
        Self {
            pinned: Some(client.url.clone()),
            selection: None,
            current: std::sync::Mutex::new(Selected {
                stamp: None,
                checked: None,
                client,
            }),
        }
    }

    /// `pinned` is `ENAME_BRP_URL`; `project` and `owner` locate the selection file.
    pub fn new(
        pinned: Option<String>,
        project: Option<crate::launch::Project>,
        owner: Option<String>,
    ) -> Self {
        let url = pinned.clone().unwrap_or_else(|| DEFAULT_URL.to_owned());
        Self {
            pinned,
            selection: project.map(|project| (project, owner)),
            current: std::sync::Mutex::new(Selected {
                stamp: None,
                checked: None,
                client: BrpClient::new(url),
            }),
        }
    }

    /// The URL `ENAME_BRP_URL` pins, if any.
    pub fn pinned(&self) -> Option<&str> {
        self.pinned.as_deref()
    }

    /// The client for one tool call. The selection is read again when the selection file or the
    /// selected instance's state changed, or [`RECHECK`] passed; a selected instance that is no
    /// longer live falls back to 15702.
    pub fn snapshot(&self) -> BrpClient {
        let mut current = self.current.lock().unwrap();
        if self.pinned.is_none()
            && let Some((project, owner)) = &self.selection
        {
            let stamp = crate::launch::selection_stamp(project, owner.as_deref());
            let due = current
                .checked
                .is_none_or(|checked| checked.elapsed() >= RECHECK);
            if current.stamp != Some(stamp) || due {
                current.stamp = Some(stamp);
                current.checked = Some(std::time::Instant::now());
                let url = crate::launch::read_selected(project, owner.as_deref()).map_or_else(
                    || DEFAULT_URL.to_owned(),
                    |(_, port)| crate::launch::port_url(port),
                );
                if url != current.client.url {
                    tracing::info!(%url, "following the selected game instance");
                    current.client = BrpClient::new(url);
                }
            }
        }
        current.client.clone()
    }
}
