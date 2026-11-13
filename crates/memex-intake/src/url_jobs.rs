//! Agent-owned URL jobs, behind the same authenticated intake boundary.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod podcast;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityMode {
    #[default]
    Off,
    Configured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationPolicy {
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationMode {
    Inbox,
    SelectedGroup,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UrlEnqueue {
    pub schema_version: u32,
    pub request_key: String,
    pub url: String,
    pub include_comments: bool,
    pub quality_check: QualityMode,
    #[serde(default)]
    pub generation_policy: Option<GenerationPolicy>,
    #[serde(default)]
    pub destination_mode: Option<DestinationMode>,
    #[serde(default)]
    pub target_group_id: Option<String>,
    #[serde(default)]
    pub remove_from_group_ids: Vec<String>,
    /// Untrusted display hints. The worker independently fetches the fixed RSS.
    #[serde(default)]
    pub source_hints: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobId {
    schema_version: u32,
    job_id: String,
}

pub enum UrlJobCommand {
    Groups,
    ActiveGroup,
    Enqueue(UrlEnqueue),
    Status(String),
    Retry(String),
    Cancel(String),
}

#[derive(Debug, Clone, Copy)]
pub enum UrlJobError {
    InvalidRequest,
    PodcastDuplicateConflict,
    IdentityConflict,
    NotFound,
    NotRetryable,
    NotCancellable,
    Unavailable,
    Unknown,
}

impl UrlJobError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::PodcastDuplicateConflict => "podcast_duplicate_conflict",
            Self::IdentityConflict => "request_identity_conflict",
            Self::NotFound => "resource_not_found",
            Self::NotRetryable => "job_not_retryable",
            Self::NotCancellable => "job_not_cancellable",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
    pub fn status(self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::PodcastDuplicateConflict => 409,
            Self::NotFound => 404,
            Self::IdentityConflict | Self::NotRetryable | Self::NotCancellable => 409,
            Self::Unavailable => 503,
            Self::Unknown => 500,
        }
    }
}

/// Intake supplies a server-derived registration, never a client-selected owner.
pub trait UrlJobBackend: Send + Sync {
    /// Backend-specific guarantees; an installed generic backend must not
    /// advertise policies it cannot persist and enforce after restart.
    fn features(&self) -> &'static [&'static str] { &[] }
    fn execute(&self, registration: &str, command: UrlJobCommand) -> Result<Value, UrlJobError>;
}

pub(crate) fn parse(path: &str, value: Value) -> Result<UrlJobCommand, UrlJobError> {
    if path == "/api/v1/groups" || path == "/api/v1/active-group" {
        return if value == serde_json::json!({"schema_version": 1}) {
            Ok(if path == "/api/v1/groups" { UrlJobCommand::Groups } else { UrlJobCommand::ActiveGroup })
        } else {
            Err(UrlJobError::InvalidRequest)
        };
    }
    if path == "/api/v1/url-jobs/enqueue" {
        let request: UrlEnqueue =
            serde_json::from_value(value).map_err(|_| UrlJobError::InvalidRequest)?;
        if request.schema_version != 1 {
            return Err(UrlJobError::InvalidRequest);
        }
        return Ok(UrlJobCommand::Enqueue(request));
    }
    let request: JobId = serde_json::from_value(value).map_err(|_| UrlJobError::InvalidRequest)?;
    if request.schema_version != 1 || request.job_id.len() > 128 {
        return Err(UrlJobError::InvalidRequest);
    }
    match path {
        "/api/v1/url-jobs/status" => Ok(UrlJobCommand::Status(request.job_id)),
        "/api/v1/url-jobs/retry" => Ok(UrlJobCommand::Retry(request.job_id)),
        "/api/v1/url-jobs/cancel" => Ok(UrlJobCommand::Cancel(request.job_id)),
        _ => Err(UrlJobError::InvalidRequest),
    }
}
