//! The one error type every command returns.

use std::borrow::Cow;

use schemars::JsonSchema;
use serde::Serialize;

/// Whose fault a failure is. A BRP client maps it to a JSON-RPC code; a console colours it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// The call was wrong: bad params, an unknown name, a scope it may not use.
    Invalid,
    /// The call was fine and the work failed.
    Failed,
}

/// A command's failure, as every caller sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema, thiserror::Error)]
#[error("{message}")]
pub struct CommandError {
    /// Stable snake_case name. Clients branch on this, never on the message.
    pub kind: Cow<'static, str>,
    pub class: ErrorClass,
    pub message: String,
    /// What to do about it, written by the crate that knows. Command names in it are written
    /// dotted and in backticks, so a client can rewrite them into its own names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl CommandError {
    pub fn invalid(kind: impl Into<Cow<'static, str>>, message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Invalid, kind, message)
    }

    pub fn failed(kind: impl Into<Cow<'static, str>>, message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Failed, kind, message)
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    fn new(
        class: ErrorClass,
        kind: impl Into<Cow<'static, str>>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            class,
            message: message.into(),
            hint: None,
        }
    }
}
