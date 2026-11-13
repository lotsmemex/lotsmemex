//! One explicitly authorized, bounded text request. Not a queue or readiness test.
//! The host owns disclosure authorization, settings revalidation and durable receipts.
use crate::ApiCredential;
use memex_models::{Purpose, ReasoningEffort, Snapshot};
use reqwest::{header, redirect::Policy, Client};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use zeroize::Zeroizing;

const DEADLINE: Duration = Duration::from_secs(120);
const MAX_RESPONSE: usize = 1024 * 1024;
/// Non-streaming requests stay below this; a streamed long output may use up to
/// `MAX_STREAM_COMPLETION_TOKENS`.
const MAX_COMPLETION_TOKENS: u32 = 16_000;
pub const MAX_STREAM_COMPLETION_TOKENS: u32 = 32_000;

/// Deadlines of one streamed request: `idle` bounds the wait for the response head and every
/// gap between received bytes; `total` caps the whole request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLimits {
    pub idle: Duration,
    pub total: Duration,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(60),
            total: Duration::from_secs(15 * 60),
        }
    }
}

/// No Debug/Serialize: prompt and source are never diagnostic metadata.
pub struct TextRequest<'a> {
    pub purpose: Purpose,
    pub system: &'a str,
    pub input: &'a str,
    pub max_completion_tokens: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationError {
    InvalidRequest,
    CredentialScopeMismatch,
    /// After dispatch even a timeout/error may have consumed upstream tokens.
    OutcomeUnknown,
    Rejected,
    InvalidResponse,
    Incomplete,
    ModelMismatch,
}
impl std::fmt::Display for GenerationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidRequest => "invalid_generation_request",
            Self::CredentialScopeMismatch => "provider_credential_scope_mismatch",
            Self::OutcomeUnknown => "generation_outcome_unknown",
            Self::Rejected => "generation_rejected_no_retry",
            Self::InvalidResponse => "invalid_generation_response_no_retry",
            Self::Incomplete => "generation_incomplete_no_retry",
            Self::ModelMismatch => "generation_model_mismatch_no_retry",
        })
    }
}
impl std::error::Error for GenerationError {}

#[derive(Debug, Deserialize, Serialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptDetails>,
    #[serde(default)]
    pub completion_tokens_details: Option<CompletionDetails>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct PromptDetails {
    #[serde(default)]
    pub cached_tokens: Option<u64>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct CompletionDetails {
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
}

/// Text is an intentional result, never Debug/log output. Model is a provider
/// assertion, not proof of immutable weights; effort records the sent preference.
#[derive(Serialize)]
pub struct TextResult {
    pub settings_revision: u64,
    pub credential_generation: u64,
    pub requested_model: String,
    pub reported_model: String,
    pub requested_effort: Option<ReasoningEffort>,
    pub usage: Option<Usage>,
    pub text: String,
}

#[derive(Deserialize)]
struct Response {
    model: String,
    choices: Vec<Choice>,
    usage: Option<Usage>,
}
#[derive(Deserialize)]
struct Choice {
    index: u32,
    finish_reason: String,
    message: Message,
}
#[derive(Deserialize)]
struct Message {
    role: String,
    content: Option<String>,
    refusal: Option<String>,
    tool_calls: Option<serde_json::Value>,
}

/// Bounds shared by single-turn and chat requests. The input may carry a whole bounded source
/// document as read-only context; the request body bound covers JSON escaping of that input.
pub const MAX_SYSTEM_BYTES: usize = 16 * 1024;
/// `chat` carries a bounded source document (up to 160 KiB) plus metadata and minutes as
/// its system context.
pub const MAX_CHAT_SYSTEM_BYTES: usize = 200 * 1024;
pub const MAX_INPUT_BYTES: usize = 256 * 1024;
const MAX_BODY_BYTES: usize = 640 * 1024;
pub const MAX_CHAT_MESSAGES: usize = 41;

/// How a successful choice ended. `Length` is only accepted by `chat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Finish {
    Stop,
    Length,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
}

/// One prior conversation turn. No Debug/Serialize: content is never diagnostic metadata.
pub struct ChatTurn<'a> {
    pub role: ChatRole,
    pub content: &'a str,
}

/// Multi-turn, non-streaming request; the last turn must be the user's.
pub struct ChatRequest<'a> {
    pub purpose: Purpose,
    pub system: &'a str,
    pub turns: &'a [ChatTurn<'a>],
    pub max_completion_tokens: u32,
}

pub async fn complete(
    snapshot: &Snapshot,
    credential: &ApiCredential,
    input: TextRequest<'_>,
) -> Result<TextResult, GenerationError> {
    if !snapshot.configuration.selections.iter().any(|s|
        s.purpose == input.purpose && s.reasoning_effort.is_some()) {
        return Err(GenerationError::InvalidRequest);
    }
    complete_with_optional_effort(snapshot, credential, input).await
}

/// Explicit opt-in for routes whose provider does not accept reasoning_effort.
/// An absent preference omits the field; it never sends the value "none" instead.
pub async fn complete_with_optional_effort(
    snapshot: &Snapshot,
    credential: &ApiCredential,
    input: TextRequest<'_>,
) -> Result<TextResult, GenerationError> {
    if input.input.trim().is_empty() || input.input.len() > MAX_INPUT_BYTES {
        return Err(GenerationError::InvalidRequest);
    }
    let turn = [ChatTurn {
        role: ChatRole::User,
        content: input.input,
    }];
    send(
        snapshot,
        credential,
        ChatRequest {
            purpose: input.purpose,
            system: input.system,
            turns: &turn,
            max_completion_tokens: input.max_completion_tokens,
        },
        false,
        None,
        true,
    )
    .await
    .map(|(result, _)| result)
}

/// One single-turn request answered as an OpenAI chat-completions SSE stream
/// (`choices[].delta.content`, `finish_reason`, optional usage chunk, `data: [DONE]`).
/// A `length` finish returns the partial text with `Finish::Length` (never silently accepted
/// as complete by callers). A stream broken after dispatch, an idle gap longer than
/// `limits.idle` or the total cap is `OutcomeUnknown`; nothing is retried.
pub async fn complete_stream(
    snapshot: &Snapshot,
    credential: &ApiCredential,
    input: TextRequest<'_>,
    limits: StreamLimits,
) -> Result<(TextResult, Finish), GenerationError> {
    if input.input.trim().is_empty() || input.input.len() > MAX_INPUT_BYTES {
        return Err(GenerationError::InvalidRequest);
    }
    let turn = [ChatTurn {
        role: ChatRole::User,
        content: input.input,
    }];
    send(
        snapshot,
        credential,
        ChatRequest {
            purpose: input.purpose,
            system: input.system,
            turns: &turn,
            max_completion_tokens: input.max_completion_tokens,
        },
        false,
        Some(limits),
        false,
    )
    .await
}

/// Like `complete`, but over a bounded conversation, and a `length` finish returns the partial
/// text instead of failing.
pub async fn chat(
    snapshot: &Snapshot,
    credential: &ApiCredential,
    input: ChatRequest<'_>,
) -> Result<(TextResult, Finish), GenerationError> {
    send(snapshot, credential, input, true, None, false).await
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}
#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    delta: Option<StreamDelta>,
}
#[derive(Deserialize)]
struct StreamDelta {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    refusal: Option<String>,
    #[serde(default)]
    tool_calls: Option<serde_json::Value>,
}

/// Incremental SSE decoder for one chat-completions stream. Bounded: the pending line buffer
/// and the accumulated text are both limited to `MAX_RESPONSE`.
struct SseAccumulator {
    model_id: String,
    pending: Vec<u8>,
    data: Vec<u8>,
    text: Zeroizing<String>,
    finish: Option<String>,
    usage: Option<Usage>,
    done: bool,
}

impl SseAccumulator {
    fn new(model_id: &str) -> Self {
        Self {
            model_id: model_id.to_owned(),
            pending: Vec::new(),
            data: Vec::new(),
            text: Zeroizing::new(String::new()),
            finish: None,
            usage: None,
            done: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<(), GenerationError> {
        if self.done {
            return Ok(());
        }
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.line(&line)?;
            if self.done {
                return Ok(());
            }
        }
        if self.pending.len() > MAX_RESPONSE {
            return Err(GenerationError::InvalidResponse);
        }
        Ok(())
    }

    fn line(&mut self, line: &[u8]) -> Result<(), GenerationError> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(b":") {
            return Ok(()); // comment / keep-alive
        }
        if let Some(value) = line.strip_prefix(b"data:") {
            let value = value.strip_prefix(b" ").unwrap_or(value);
            if !self.data.is_empty() {
                self.data.push(b'\n');
            }
            self.data.extend_from_slice(value);
            if self.data.len() > MAX_RESPONSE {
                return Err(GenerationError::InvalidResponse);
            }
        }
        // `event:`, `id:`, `retry:` carry nothing for chat completions.
        Ok(())
    }

    fn dispatch(&mut self) -> Result<(), GenerationError> {
        use GenerationError::*;
        if self.data.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.data);
        if data.as_slice() == b"[DONE]" {
            self.done = true;
            return Ok(());
        }
        let chunk: StreamChunk = serde_json::from_slice(&data).map_err(|_| InvalidResponse)?;
        if chunk
            .model
            .as_deref()
            .is_some_and(|model| !model.is_empty() && model != self.model_id)
        {
            return Err(ModelMismatch);
        }
        if chunk.usage.is_some() {
            self.usage = chunk.usage;
        }
        for choice in chunk.choices {
            if choice.index != 0 {
                return Err(InvalidResponse);
            }
            if let Some(delta) = choice.delta {
                if delta.refusal.is_some()
                    || delta.tool_calls.is_some()
                    || delta
                        .role
                        .as_deref()
                        .is_some_and(|role| role != "assistant")
                {
                    return Err(InvalidResponse);
                }
                if let Some(content) = delta.content {
                    if content.len() > MAX_RESPONSE - self.text.len() {
                        return Err(InvalidResponse);
                    }
                    self.text.push_str(&content);
                }
            }
            if let Some(reason) = choice.finish_reason {
                self.finish = Some(reason);
            }
        }
        Ok(())
    }

    /// At end of stream: a finish reason is required (a stream cut before it is unknown).
    fn end(mut self) -> Result<(String, Finish, Option<Usage>), GenerationError> {
        use GenerationError::*;
        if !self.done {
            self.dispatch()?;
        }
        let finish = match self.finish.as_deref() {
            None => return Err(OutcomeUnknown),
            Some("stop") => Finish::Stop,
            Some("length") => Finish::Length,
            Some(_) => return Err(Incomplete),
        };
        if self.text.trim().is_empty() {
            return Err(InvalidResponse);
        }
        Ok((std::mem::take(&mut *self.text), finish, self.usage))
    }
}

async fn send(
    snapshot: &Snapshot,
    credential: &ApiCredential,
    input: ChatRequest<'_>,
    allow_length: bool,
    stream: Option<StreamLimits>,
    allow_unspecified_effort: bool,
) -> Result<(TextResult, Finish), GenerationError> {
    use GenerationError::*;
    let total: usize = input.turns.iter().map(|turn| turn.content.len()).sum();
    if snapshot.schema_version != 1
        || snapshot.configuration.validate().is_err()
        || input.system.trim().is_empty()
        || input.system.len()
            > if allow_length {
                MAX_CHAT_SYSTEM_BYTES
            } else {
                MAX_SYSTEM_BYTES
            }
        || input.turns.is_empty()
        || input.turns.len() > MAX_CHAT_MESSAGES
        || input.turns.last().map(|turn| turn.role) != Some(ChatRole::User)
        || input
            .turns
            .iter()
            .any(|turn| turn.content.trim().is_empty())
        || total > MAX_INPUT_BYTES
        || !(1..=if stream.is_some() {
            MAX_STREAM_COMPLETION_TOKENS
        } else {
            MAX_COMPLETION_TOKENS
        })
            .contains(&input.max_completion_tokens)
    {
        return Err(InvalidRequest);
    }
    let (provider, model) = snapshot
        .configuration
        .target(input.purpose)
        .ok_or(InvalidRequest)?;
    let effort = snapshot
        .configuration
        .selections
        .iter()
        .find(|s| s.purpose == input.purpose)
        .and_then(|s| s.reasoning_effort);
    if effort.is_none() && !allow_unspecified_effort {
        return Err(InvalidRequest);
    }
    if credential.provider_id != provider.id || credential.api_base != provider.api_base {
        return Err(CredentialScopeMismatch);
    }
    let mut messages = vec![serde_json::json!({"role":"system", "content":input.system})];
    messages.extend(input.turns.iter().map(|turn| {
        serde_json::json!({
            "role": match turn.role {
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
            },
            "content": turn.content,
        })
    }));
    let mut payload = serde_json::json!({
            "model": model.model_id,
            "messages": messages,
            "max_completion_tokens": input.max_completion_tokens,
            "stream": stream.is_some(),
            "n": 1
        });
    if let Some(effort) = effort {
        payload["reasoning_effort"] = serde_json::to_value(effort).map_err(|_| InvalidRequest)?;
    }
    let body = Zeroizing::new(serde_json::to_vec(&payload).map_err(|_| InvalidRequest)?);
    drop(payload);
    drop(messages);
    if body.len() > MAX_BODY_BYTES {
        return Err(InvalidRequest);
    }
    let endpoint = url::Url::parse(&provider.api_base)
        .and_then(|base| base.join("chat/completions"))
        .map_err(|_| InvalidRequest)?;
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .referer(false)
        .timeout(stream.map_or(DEADLINE, |limits| limits.total))
        .build()
        .map_err(|_| InvalidRequest)?;
    let bearer = Zeroizing::new(format!("Bearer {}", credential.value.as_str()));
    let mut auth = header::HeaderValue::from_str(&bearer).map_err(|_| InvalidRequest)?;
    auth.set_sensitive(true);
    if let Some(limits) = stream {
        let model_id = model.model_id.clone();
        let result = tokio::time::timeout(limits.total, async {
            let idle = |future| tokio::time::timeout(limits.idle, future);
            let mut response = idle(
                client
                    .post(endpoint)
                    .header(header::AUTHORIZATION, auth)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "text/event-stream")
                    .body(body.to_vec())
                    .send(),
            )
            .await
            .map_err(|_| OutcomeUnknown)?
            .map_err(|_| OutcomeUnknown)?;
            if !response.status().is_success() {
                return Err(Rejected);
            }
            let mut sse = SseAccumulator::new(&model_id);
            loop {
                let chunk = tokio::time::timeout(limits.idle, response.chunk())
                    .await
                    .map_err(|_| OutcomeUnknown)?
                    .map_err(|_| OutcomeUnknown)?;
                match chunk {
                    Some(bytes) => sse.push(&bytes)?,
                    None => break,
                }
                if sse.done {
                    break;
                }
            }
            sse.end()
        })
        .await
        .map_err(|_| OutcomeUnknown)??;
        let (text, finish, usage) = result;
        return Ok((
            TextResult {
                settings_revision: snapshot.revision,
                credential_generation: credential.generation,
                requested_model: model.model_id.clone(),
                reported_model: model.model_id.clone(),
                requested_effort: effort,
                usage,
                text,
            },
            finish,
        ));
    }
    let result = tokio::time::timeout(DEADLINE, async {
        let mut response = client
            .post(endpoint)
            .header(header::AUTHORIZATION, auth)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.to_vec())
            .send()
            .await
            .map_err(|_| OutcomeUnknown)?;
        if !response.status().is_success() {
            return Err(Rejected);
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE as u64)
        {
            return Err(InvalidResponse);
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(|_| OutcomeUnknown)? {
            if chunk.len() > MAX_RESPONSE - bytes.len() {
                return Err(InvalidResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        let result: Response = serde_json::from_slice(&bytes).map_err(|_| InvalidResponse)?;
        if result.model != model.model_id {
            return Err(ModelMismatch);
        }
        if result.choices.len() != 1 {
            return Err(InvalidResponse);
        }
        let choice = result.choices.into_iter().next().ok_or(InvalidResponse)?;
        let finish = match choice.finish_reason.as_str() {
            "stop" => Finish::Stop,
            "length" if allow_length => Finish::Length,
            _ => return Err(Incomplete),
        };
        if choice.index != 0
            || choice.message.role != "assistant"
            || choice.message.refusal.is_some()
            || choice.message.tool_calls.is_some()
        {
            return Err(InvalidResponse);
        }
        let text = choice
            .message
            .content
            .filter(|s| !s.trim().is_empty())
            .ok_or(InvalidResponse)?;
        Ok((
            TextResult {
                settings_revision: snapshot.revision,
                credential_generation: credential.generation,
                requested_model: model.model_id.clone(),
                reported_model: result.model,
                requested_effort: effort,
                usage: result.usage,
                text,
            },
            finish,
        ))
    })
    .await
    .map_err(|_| OutcomeUnknown)??;
    Ok(result)
}
