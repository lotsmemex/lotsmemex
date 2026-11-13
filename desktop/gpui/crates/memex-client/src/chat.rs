//! `item_chat` (capability `item_chat_v1`): one question about one item, answered
//! by the agent's chat model. The request carries the conversation so far; the
//! agent keeps nothing. Limits are the agent's: at most 40 messages, 16 KiB per
//! message, 64 KiB in total, the last one from the user. Older turns are dropped
//! here, never sent and rejected.

use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const ITEM_CHAT_FEATURE: &str = "item_chat_v1";
pub const MAX_CHAT_MESSAGES: usize = 40;
pub const MAX_CHAT_MESSAGE_BYTES: usize = 16 * 1024;
pub const MAX_CHAT_TOTAL_BYTES: usize = 64 * 1024;
/// A reply may take up to ~120 s; `item_chat` gets its own deadline instead of the
/// 5 s used for library reads.
pub const ITEM_CHAT_TIMEOUT: Duration = Duration::from_secs(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: ChatRole,
    pub content: String,
}

impl ChatTurn {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: ChatRole::User, content: content.into() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: ChatRole::Assistant, content: content.into() }
    }
}

/// One send. `request_id` is new per send and reused only to retry a send whose
/// outcome is unknown, with exactly the same messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemChatRequest {
    pub item_id: String,
    pub request_id: String,
    pub messages: Vec<ChatTurn>,
}

/// `item_chat {request_id, reply, model, finish}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ItemChatReply {
    pub request_id: String,
    pub reply: String,
    #[serde(default)]
    pub model: Option<String>,
    /// `stop | length` (kept as a string: a newer agent may add values).
    #[serde(default)]
    pub finish: String,
}

impl ItemChatReply {
    /// The model stopped at its length limit; the reply is cut off.
    pub fn truncated(&self) -> bool {
        self.finish == "length"
    }
}

/// Cuts `s` to at most `max` bytes on a character boundary.
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The messages to send for `history` (oldest first, ending with the new user
/// message): earlier messages longer than 16 KiB are cut, then the oldest are
/// dropped until at most 40 remain within 64 KiB, and the result starts with a
/// user message. `None` when the new message alone breaks a limit.
pub fn trim_history(history: &[ChatTurn]) -> Option<Vec<ChatTurn>> {
    let (last, earlier) = history.split_last()?;
    if last.role != ChatRole::User || last.content.trim().is_empty() || last.content.len() > MAX_CHAT_MESSAGE_BYTES {
        return None;
    }
    let mut turns: Vec<ChatTurn> = earlier.iter().map(|t| ChatTurn { role: t.role, content: cut(&t.content, MAX_CHAT_MESSAGE_BYTES).to_string() }).collect();
    turns.push(last.clone());
    let mut total: usize = turns.iter().map(|t| t.content.len()).sum();
    let mut start = 0;
    while turns.len() - start > MAX_CHAT_MESSAGES || total > MAX_CHAT_TOTAL_BYTES || turns[start].role != ChatRole::User {
        total -= turns[start].content.len();
        start += 1;
    }
    Some(turns.split_off(start))
}

/// Whether `messages` is within the agent's limits (checked before sending).
pub fn valid_messages(messages: &[ChatTurn]) -> bool {
    !messages.is_empty()
        && messages.len() <= MAX_CHAT_MESSAGES
        && messages.last().is_some_and(|m| m.role == ChatRole::User)
        && messages.iter().all(|m| m.content.len() <= MAX_CHAT_MESSAGE_BYTES)
        && messages.iter().map(|m| m.content.len()).sum::<usize>() <= MAX_CHAT_TOTAL_BYTES
}

/// `item_chat_history_v1`: the agent stores each answered `item_chat` exchange
/// (question and reply) per item; the read-only `conversation_read {item_id,
/// max_sensitivity, cursor?}` pages them oldest first (≤ 48 KiB / 200 turns a page;
/// `cursor` is the decimal `seq` of the previous page's last turn, negative for
/// turns restored before the others). Unknown fields are ignored.
pub const ITEM_CHAT_HISTORY_FEATURE: &str = "item_chat_history_v1";
/// `origin` of turns the owner restored from a screenshot (`conversation_import`).
pub const RESTORED_FROM_USER_SCREENSHOT: &str = "restored_from_user_screenshot";
/// Most pages read for one item (200 turns each); a longer history is cut at the end.
pub const MAX_CONVERSATION_PAGES: usize = 50;

/// One stored turn as the agent returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTurn {
    pub seq: i64,
    pub role: ChatRole,
    pub content: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub created_at_ms: i64,
    #[serde(default)]
    pub model: Option<String>,
    /// `None` for turns asked through `item_chat`; a newer agent may add values.
    #[serde(default)]
    pub origin: Option<String>,
    /// The agent cut this single turn to fit a page.
    #[serde(default)]
    pub truncated: bool,
}

impl StoredTurn {
    pub fn restored(&self) -> bool {
        self.origin.as_deref() == Some(RESTORED_FROM_USER_SCREENSHOT)
    }
    pub fn chat_turn(&self) -> ChatTurn {
        ChatTurn { role: self.role, content: self.content.clone() }
    }
}

/// One `conversation_read` page; `version` is `v1:<count>:<max seq>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationPage {
    pub item_id: String,
    #[serde(default)]
    pub turns: Vec<StoredTurn>,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// `request_id`: 16–128 characters of `[A-Za-z0-9_-]`, new for every send.
pub fn new_chat_request_id() -> String {
    format!("chat-{}", crate::biblio::new_request_id())
}
