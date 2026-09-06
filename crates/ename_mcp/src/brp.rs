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
                    "cannot reach the game's remote server at {}. Is the game running, and was \
                     it built with `--features agent`?",
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
            Some(Ok(ResponseError { code, message })) => {
                bail!("BRP `{method}` failed ({code}): {message}")
            }
            _ => bail!("BRP `{method}` returned neither a result nor an error"),
        }
    }
}

#[derive(Deserialize)]
struct ResponseError {
    code: i32,
    message: String,
}
