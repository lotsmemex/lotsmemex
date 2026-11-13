//! Chat state: availability, the suggestion toggle, per-item conversations.
//!
//! With an agent that advertises `item_chat_v1` (and a chat route) the composer
//! sends the item's conversation through `item_chat` and shows the model's reply.
//! Conversations live in memory per item for this session; nothing is persisted.
//! Without the capability the composer is disabled with the reason. The synthetic
//! fixture without `item_chat_v1` keeps the labelled sample transcripts and never
//! invents a reply. Errors are reported as they are: an unknown outcome can be
//! retried with the same request id, nothing else reuses one.
//!
//! With `item_chat_history_v1` the agent keeps answered turns per item: the first
//! time an item's conversation is shown its stored turns are read once
//! (`conversation_read`, off the UI thread) and become the context of the next
//! question. They are context only, never sent as new turns. A late answer for an
//! item that was opened again, cleared or reloaded is dropped by its load token.

use memex_client::chat::{new_chat_request_id, trim_history, StoredTurn};
use memex_client::{tx, ChatTurn, Citation, Client, ClientError, ItemChatReply, ItemChatRequest};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// No usable conversation interface; the reason is a key shown via `relabel`.
    Unavailable(String),
    /// Synthetic development data without `item_chat_v1`; transcripts are samples.
    Sample,
    /// `item_chat` is advertised and a chat route exists.
    Ready,
}

/// Reasons the composer is disabled (keys).
pub const NO_CHAT_SUPPORT: &str = "当前代理不支持 AI 对话";
pub const NO_CHAT_MODEL: &str = "还没有配置对话模型";

/// What the agent's capabilities allow. Blocking; call off the UI thread.
pub fn probe(client: &Client) -> Availability {
    match client.capabilities() {
        Ok(c) if c.chat_ready() => Availability::Ready,
        Ok(c) if c.item_chat => Availability::Unavailable(NO_CHAT_MODEL.into()),
        Ok(_) if client.mode().is_fixture() => Availability::Sample,
        Ok(_) => Availability::Unavailable(NO_CHAT_SUPPORT.into()),
        Err(e) => Availability::Unavailable(e.message()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub speaker: Speaker,
    pub text: String,
    pub citations: Vec<Citation>,
    pub sample: bool,
    /// A question that got no answer; kept on screen, left out of later requests.
    pub unanswered: bool,
    /// The model stopped at its length limit.
    pub truncated: bool,
    /// A stored turn the owner restored from a screenshot (「从截图恢复」).
    pub restored: bool,
    /// A stored turn the agent cut to fit a history page.
    pub cut: bool,
    /// The `item_chat` exchange this turn belongs to (stored or asked here).
    pub request_id: Option<String>,
}

impl Message {
    fn new(speaker: Speaker, text: impl Into<String>) -> Self {
        Self { speaker, text: text.into(), citations: vec![], sample: false, unanswered: false, truncated: false, restored: false, cut: false, request_id: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatError {
    /// `no_chat_provider`: no chat model configured; offer settings.
    NoProvider,
    /// The send may or may not have reached the model; retry reuses its id.
    Unknown,
    /// Anything else, as a short reason in the interface language.
    Failed(String),
}

/// The item's stored conversation (`item_chat_history_v1` only).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum History {
    /// Not asked for (or the agent does not keep conversations).
    #[default]
    NotLoaded,
    /// `conversation_read` is on its way; the token identifies this load.
    Loading(u64),
    /// Read; `empty` when the agent had no turns for the item.
    Loaded { empty: bool },
    /// The read failed (a short reason); 重试 loads again.
    Failed(String),
}

#[derive(Debug, Clone, Default)]
struct Conversation {
    history: History,
    messages: Vec<Message>,
    /// The last request; kept after an unknown outcome so a retry is identical.
    request: Option<ItemChatRequest>,
    waiting: bool,
    error: Option<ChatError>,
}

/// Result of a send attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Send {
    /// Send this to the agent (off the UI thread).
    Request(ItemChatRequest),
    /// Recorded locally (sample mode); nothing to send.
    Local,
    /// Nothing happened.
    Nothing,
}

#[derive(Debug, Clone)]
pub struct ChatModel {
    availability: Availability,
    pub suggestions_enabled: bool,
    item_id: Option<String>,
    revision_id: Option<String>,
    conversations: HashMap<String, Conversation>,
    /// Markdown of the open item when it may carry a sample transcript, so the
    /// sample appears if sample mode is only known after the item opened.
    sample_source: Option<String>,
    /// The agent advertises `item_chat_history_v1`.
    history_supported: bool,
    next_history_token: u64,
}

/// Prefix of the sample answer; the view swaps it after a language switch.
pub const SAMPLE_LABEL: &str = "（示例对话，非模型生成）";
const SAMPLE_ANSWER: &str = "本文把方法分为基于抽取、基于生成和基于结构化建模三类：抽取保留事实片段，生成改写更连贯，结构化建模按段落层次压缩。";
/// Stored as the Chinese key and shown through `relabel`.
pub const NO_MODEL_REPLY: &str = "开发模式：尚未连接模型，不会生成回复。";

pub const SAMPLE_SUGGESTIONS: [&str; 3] = ["总结作者的核心论点", "与我库中的类似文章比较", "列出可以追问的研究空白"];

impl ChatModel {
    pub fn new(availability: Availability) -> Self {
        Self { availability, suggestions_enabled: true, item_id: None, revision_id: None, conversations: HashMap::new(), sample_source: None, history_supported: false, next_history_token: 0 }
    }

    pub fn availability(&self) -> &Availability {
        &self.availability
    }

    pub fn set_availability(&mut self, availability: Availability) {
        self.availability = availability;
        if let (Some((item, revision)), Some(markdown)) = (self.current(), self.sample_source.clone()) {
            self.open(&item, &revision, &markdown, true);
        }
    }

    /// Whether stored conversations are read back (from the capability probe).
    pub fn set_history_supported(&mut self, supported: bool) {
        self.history_supported = supported;
    }

    /// The open item's stored-conversation state.
    pub fn history(&self) -> History {
        self.conversation().map(|c| c.history.clone()).unwrap_or_default()
    }

    /// The load to start for the open item, if its stored turns were never read:
    /// (item id, token). Marks it loading; call once per answer.
    pub fn history_to_load(&mut self) -> Option<(String, u64)> {
        if !self.history_supported || self.availability != Availability::Ready {
            return None;
        }
        let item = self.item_id.clone()?;
        let c = self.conversations.entry(item.clone()).or_default();
        if c.history != History::NotLoaded {
            return None;
        }
        self.next_history_token += 1;
        c.history = History::Loading(self.next_history_token);
        Some((item, self.next_history_token))
    }

    /// After a failed read: read the open item's stored turns again.
    pub fn retry_history(&mut self) -> Option<(String, u64)> {
        let c = self.conversations.get_mut(self.item_id.as_ref()?)?;
        // Not while a reply is on its way: its exchange may be stored meanwhile.
        if !matches!(c.history, History::Failed(_)) || c.waiting {
            return None;
        }
        c.history = History::NotLoaded;
        self.history_to_load()
    }

    /// The stored turns of `item_id` (dropped unless `token` is the load awaited).
    /// They go before anything asked meanwhile, as context, not as new questions;
    /// an exchange already shown here (same request id) is not shown twice.
    pub fn finish_history(&mut self, item_id: &str, token: u64, result: Result<Vec<StoredTurn>, ClientError>) {
        let Some(c) = self.conversations.get_mut(item_id) else { return };
        if c.history != History::Loading(token) {
            return;
        }
        match result {
            Ok(turns) => {
                c.history = History::Loaded { empty: turns.is_empty() };
                let shown: std::collections::HashSet<String> = c.messages.iter().filter_map(|m| m.request_id.clone()).collect();
                let stored = turns.into_iter().filter(|t| !shown.contains(&t.request_id)).map(|t| {
                    let speaker = match t.role {
                        memex_client::ChatRole::User => Speaker::User,
                        memex_client::ChatRole::Assistant => Speaker::Assistant,
                    };
                    Message { restored: t.restored(), cut: t.truncated, request_id: Some(t.request_id.clone()), ..Message::new(speaker, t.content) }
                });
                let mut messages: Vec<Message> = stored.collect();
                messages.append(&mut c.messages);
                c.messages = messages;
            }
            Err(e) => c.history = History::Failed(e.message()),
        }
    }

    fn conversation(&self) -> Option<&Conversation> {
        self.conversations.get(self.item_id.as_ref()?)
    }

    pub fn can_send(&self) -> bool {
        match self.availability {
            Availability::Ready => self.item_id.is_some() && !self.is_waiting() && !matches!(self.history(), History::Loading(_)),
            Availability::Sample => self.item_id.is_some(),
            Availability::Unavailable(_) => false,
        }
    }

    /// Whether the center area splits into reader above and conversation below.
    pub fn has_conversation(&self) -> bool {
        self.conversation().is_some_and(|c| !c.messages.is_empty())
    }

    pub fn transcript(&self) -> &[Message] {
        self.conversation().map(|c| c.messages.as_slice()).unwrap_or_default()
    }

    /// A reply for the open item is on its way.
    pub fn is_waiting(&self) -> bool {
        self.conversation().is_some_and(|c| c.waiting)
    }

    pub fn error(&self) -> Option<&ChatError> {
        self.conversation().and_then(|c| c.error.as_ref())
    }

    /// Suggestions only in the labelled sample mode (no other source exists yet).
    /// Chinese keys; the view shows them translated.
    pub fn suggestions(&self) -> &'static [&'static str] {
        if self.suggestions_enabled && self.availability == Availability::Sample && self.can_send() { &SAMPLE_SUGGESTIONS } else { &[] }
    }

    /// Switches to `item_id`, keeping each item's conversation for the session.
    /// In sample mode, items whose markdown contains the sample anchors get a
    /// labelled sample transcript with real byte citations.
    pub fn open(&mut self, item_id: &str, revision_id: &str, markdown: &str, sample_transcript: bool) {
        self.item_id = Some(item_id.into());
        self.revision_id = Some(revision_id.into());
        self.sample_source = sample_transcript.then(|| markdown.to_string());
        let sample = self.availability == Availability::Sample && sample_transcript;
        let conversation = self.conversations.entry(item_id.into()).or_default();
        if sample && conversation.messages.is_empty() {
            let cite = |label: &str, needle: &str| -> Option<Citation> {
                let start = markdown.find(needle)?;
                Some(Citation { label: label.into(), item_id: item_id.into(), revision_id: revision_id.into(), start_byte: start, end_byte: start + needle.len() })
            };
            conversation.messages.push(Message { sample: true, ..Message::new(Speaker::User, "这篇文章的三种压缩方法有什么区别？") });
            conversation.messages.push(Message {
                // The label is UI; the sample answer itself is synthetic content, like the fixture text.
                citations: [cite("§2.1", "这是第 5 段合成示例正文"), cite("§3.4", "这是第 13 段合成示例正文")].into_iter().flatten().collect(),
                sample: true,
                ..Message::new(Speaker::Assistant, format!("{}{SAMPLE_ANSWER}", tx(SAMPLE_LABEL)))
            });
        }
    }

    /// The item and revision the conversation is bound to.
    pub fn current(&self) -> Option<(String, String)> {
        Some((self.item_id.clone()?, self.revision_id.clone()?))
    }

    /// Questions asked in the open item's conversation (the pane header count).
    pub fn question_count(&self) -> usize {
        self.transcript().iter().filter(|m| m.speaker == Speaker::User).count()
    }

    /// Forgets the open item's in-memory conversation; refused while a reply is
    /// on its way (its answer would have nowhere to go).
    pub fn clear(&mut self) -> bool {
        if self.is_waiting() {
            return false;
        }
        let Some(item) = self.item_id.clone() else { return false };
        self.sample_source = None;
        let removed = self.conversations.remove(&item);
        // Only this view forgets: the agent's stored turns are not read back again.
        if removed.as_ref().is_some_and(|c| c.history != History::NotLoaded) {
            self.conversations.insert(item, Conversation { history: History::Loaded { empty: true }, ..Default::default() });
        }
        removed.is_some_and(|c| !c.messages.is_empty())
    }

    pub fn close(&mut self) {
        self.sample_source = None;
        self.item_id = None;
        self.revision_id = None;
    }

    /// Records the user's message and, when the agent can answer, returns the
    /// request to send (a new request id every time).
    pub fn send(&mut self, text: &str) -> Send {
        let text = text.trim();
        if text.is_empty() || !self.can_send() {
            return Send::Nothing;
        }
        let Some(item_id) = self.item_id.clone() else { return Send::Nothing };
        let ready = self.availability == Availability::Ready;
        let c = self.conversations.entry(item_id.clone()).or_default();
        if !ready {
            c.messages.push(Message { sample: true, ..Message::new(Speaker::User, text) });
            c.messages.push(Message { sample: true, ..Message::new(Speaker::System, NO_MODEL_REPLY) });
            return Send::Local;
        }
        // A question whose outcome stayed unknown is left behind, unanswered.
        if c.error.take() == Some(ChatError::Unknown) {
            if let Some(m) = c.messages.iter_mut().rev().find(|m| m.speaker == Speaker::User) {
                m.unanswered = true;
            }
        }
        c.request = None;
        c.messages.push(Message::new(Speaker::User, text));
        let history: Vec<ChatTurn> = c
            .messages
            .iter()
            .filter(|m| !m.sample && !m.unanswered)
            .filter_map(|m| match m.speaker {
                Speaker::User => Some(ChatTurn::user(m.text.clone())),
                Speaker::Assistant => Some(ChatTurn::assistant(m.text.clone())),
                Speaker::System => None,
            })
            .collect();
        let Some(messages) = trim_history(&history) else {
            if let Some(m) = c.messages.last_mut() {
                m.unanswered = true;
            }
            c.error = Some(ChatError::Failed(tx("问题过长，未发送").into()));
            return Send::Nothing;
        };
        let request = ItemChatRequest { item_id, request_id: new_chat_request_id(), messages };
        if let Some(m) = c.messages.last_mut() {
            m.request_id = Some(request.request_id.clone());
        }
        c.request = Some(request.clone());
        c.waiting = true;
        Send::Request(request)
    }

    /// After an unknown outcome only: the same request again, same id.
    pub fn retry(&mut self) -> Option<ItemChatRequest> {
        let c = self.conversations.get_mut(self.item_id.as_ref()?)?;
        if c.error != Some(ChatError::Unknown) || c.waiting {
            return None;
        }
        let request = c.request.clone()?;
        c.error = None;
        c.waiting = true;
        Some(request)
    }

    /// The agent's answer to `request` (dropped unless it is the one awaited).
    pub fn finish(&mut self, request: &ItemChatRequest, result: Result<ItemChatReply, ClientError>) {
        let Some(c) = self.conversations.get_mut(&request.item_id) else { return };
        if !c.waiting || c.request.as_ref().map(|r| &r.request_id) != Some(&request.request_id) {
            return;
        }
        c.waiting = false;
        let unanswered = |c: &mut Conversation| {
            c.request = None;
            if let Some(m) = c.messages.iter_mut().rev().find(|m| m.speaker == Speaker::User) {
                m.unanswered = true;
            }
        };
        match result {
            Ok(reply) => {
                c.request = None;
                c.error = None;
                c.messages.push(Message { truncated: reply.truncated(), request_id: Some(request.request_id.clone()), ..Message::new(Speaker::Assistant, reply.reply) });
            }
            Err(ClientError::Daemon(code)) if code == "no_chat_provider" => {
                unanswered(c);
                c.error = Some(ChatError::NoProvider);
            }
            // Sent, but the answer did not come back: it may exist. Retry the same request.
            Err(ClientError::Daemon(code)) if code == "provider_outcome_unknown" => c.error = Some(ChatError::Unknown),
            Err(ClientError::Timeout | ClientError::InvalidFrame | ClientError::TooLarge) => c.error = Some(ChatError::Unknown),
            Err(ClientError::Unsupported) => {
                unanswered(c);
                c.error = Some(ChatError::Failed(tx(NO_CHAT_SUPPORT).into()));
            }
            Err(e) => {
                unanswered(c);
                c.error = Some(ChatError::Failed(e.message()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_client::{fixture::{FixtureBackend, FixtureControls, CHAT_HISTORY_FAIL_ITEM, CHAT_HISTORY_ITEM, CHAT_UNKNOWN_ITEM, SHOWCASE_ITEM}, Backend, BackendMode, Command, Response};
    use std::sync::{Arc, Mutex};

    fn reply(r: &ItemChatRequest, text: &str) -> Result<ItemChatReply, ClientError> {
        Ok(ItemChatReply { request_id: r.request_id.clone(), reply: text.into(), model: None, finish: "stop".into() })
    }

    #[test]
    fn clear_forgets_only_the_open_item_and_waits_for_a_pending_reply() {
        let mut chat = ChatModel::new(Availability::Ready);
        chat.open("item_a", "rev_1", "", false);
        let Send::Request(r) = chat.send("第一个问题") else { panic!() };
        assert_eq!(chat.question_count(), 1);
        assert!(!chat.clear(), "a reply is on its way");
        chat.finish(&r, reply(&r, "答"));
        chat.open("item_b", "rev_2", "", false);
        let Send::Request(rb) = chat.send("另一篇") else { panic!() };
        chat.finish(&rb, reply(&rb, "答"));
        chat.open("item_a", "rev_1", "", false);
        assert!(chat.clear());
        assert!(!chat.has_conversation() && chat.question_count() == 0);
        chat.open("item_b", "rev_2", "", false);
        assert!(chat.has_conversation());
    }

    #[test]
    fn unavailable_agent_offers_nothing() {
        let mut chat = ChatModel::new(Availability::Unavailable(NO_CHAT_SUPPORT.into()));
        chat.open("item_a", "rev_1", "这是第 5 段合成示例正文", true);
        assert!(!chat.can_send() && !chat.has_conversation());
        assert!(chat.suggestions().is_empty());
        assert_eq!(chat.send("你好"), Send::Nothing);
    }

    #[test]
    fn sample_mode_labels_samples_and_never_invents_replies() {
        let md = "前言\n\n这是第 5 段合成示例正文……\n\n这是第 13 段合成示例正文……";
        let mut chat = ChatModel::new(Availability::Sample);
        chat.open("item_a", "rev_1", md, true);
        assert!(chat.has_conversation());
        let answer = &chat.transcript()[1];
        assert!(answer.sample && answer.text.contains("示例"));
        assert_eq!(&md[answer.citations[0].start_byte..answer.citations[0].end_byte], "这是第 5 段合成示例正文");
        assert_eq!(chat.send("再说说第三种"), Send::Local);
        assert_eq!(chat.transcript().last().unwrap().speaker, Speaker::System);
        chat.suggestions_enabled = false;
        assert!(chat.suggestions().is_empty());
        chat.open("item_b", "rev_9", md, false);
        assert!(!chat.has_conversation());
    }

    #[test]
    fn conversation_per_item_with_pending_reply_and_history_sent_back() {
        let mut chat = ChatModel::new(Availability::Ready);
        chat.open("item_a", "rev_1", "", true);
        assert!(!chat.has_conversation(), "no samples with a real chat");
        let Send::Request(first) = chat.send("第一问") else { panic!() };
        assert!(chat.is_waiting() && !chat.can_send() && chat.has_conversation());
        assert_eq!(chat.send("插队"), Send::Nothing, "one send at a time");
        chat.finish(&first, reply(&first, "第一答"));
        assert!(!chat.is_waiting());
        let Send::Request(second) = chat.send("第二问") else { panic!() };
        assert_ne!(second.request_id, first.request_id);
        assert_eq!(second.messages, [ChatTurn::user("第一问"), ChatTurn::assistant("第一答"), ChatTurn::user("第二问")]);
        // Another item has its own (empty) conversation; a late reply still lands in item_a.
        chat.open("item_b", "rev_2", "", false);
        assert!(!chat.has_conversation() && chat.can_send());
        chat.finish(&second, reply(&second, "第二答"));
        chat.open("item_a", "rev_1", "", false);
        assert_eq!(chat.transcript().len(), 4);
        assert_eq!(chat.transcript()[3].text, "第二答");
    }

    #[test]
    fn request_id_is_reused_only_to_retry_an_unknown_outcome() {
        let mut chat = ChatModel::new(Availability::Ready);
        chat.open("item_a", "rev_1", "", false);
        // A definite failure: no retry, the question is left out of the next request.
        let Send::Request(r1) = chat.send("问一") else { panic!() };
        chat.finish(&r1, Err(ClientError::Daemon("provider_failure".into())));
        assert!(matches!(chat.error(), Some(ChatError::Failed(_))));
        assert_eq!(chat.retry(), None);
        let Send::Request(r2) = chat.send("问二") else { panic!() };
        assert_ne!(r2.request_id, r1.request_id);
        assert_eq!(r2.messages, [ChatTurn::user("问二")]);
        // Unknown outcome: retry sends the identical request.
        chat.finish(&r2, Err(ClientError::Daemon("provider_outcome_unknown".into())));
        assert_eq!(chat.error(), Some(&ChatError::Unknown));
        let again = chat.retry().unwrap();
        assert_eq!(again, r2);
        assert_eq!(chat.retry(), None, "not twice while waiting");
        chat.finish(&again, Err(ClientError::Timeout));
        assert_eq!(chat.error(), Some(&ChatError::Unknown), "a lost reply is unknown too");
        // Asking something else instead abandons the unknown one with a new id.
        let Send::Request(r3) = chat.send("问三") else { panic!() };
        assert_ne!(r3.request_id, r2.request_id);
        assert_eq!(r3.messages, [ChatTurn::user("问三")]);
        chat.finish(&r3, Err(ClientError::Daemon("no_chat_provider".into())));
        assert_eq!(chat.error(), Some(&ChatError::NoProvider));
        assert_eq!(chat.retry(), None);
    }

    /// Records every command name; optionally hides `item_chat_v1`.
    struct Recording(FixtureBackend, Arc<Mutex<Vec<String>>>);
    impl Backend for Recording {
        fn send(&self, c: &Command) -> Result<Response, ClientError> {
            let value = serde_json::to_value(c).unwrap();
            self.1.lock().unwrap().push(value["command"].as_str().unwrap().to_string());
            self.0.send(c)
        }
        fn mode(&self) -> BackendMode {
            BackendMode::Agent { socket: "/nonexistent".into() }
        }
    }

    #[test]
    fn without_item_chat_v1_the_composer_is_disabled_and_nothing_is_sent() {
        let fixture = FixtureBackend::new(30);
        fixture.set_controls(FixtureControls { no_item_chat: true, instant_chat: true, ..Default::default() });
        let sent = Arc::new(Mutex::new(vec![]));
        let client = Client::new(Recording(fixture.clone(), sent.clone()));
        let availability = probe(&client);
        assert_eq!(availability, Availability::Unavailable(NO_CHAT_SUPPORT.into()));
        let mut chat = ChatModel::new(availability);
        chat.open(SHOWCASE_ITEM, "rev", "", false);
        assert!(!chat.can_send());
        assert_eq!(chat.send("你好"), Send::Nothing);
        assert_eq!(*sent.lock().unwrap(), ["capabilities"]);

        // With the capability: a real exchange, then an unknown outcome retried with the same id.
        fixture.set_controls(FixtureControls { instant_chat: true, ..Default::default() });
        let client = Client::new(Recording(fixture, sent.clone()));
        let mut chat = ChatModel::new(probe(&client));
        assert_eq!(chat.availability(), &Availability::Ready);
        chat.open(SHOWCASE_ITEM, "rev", "", false);
        let Send::Request(r) = chat.send("这篇讲了什么？") else { panic!() };
        let result = client.item_chat(&r);
        chat.finish(&r, result);
        assert!(chat.transcript()[1].text.contains("合成回复"));
        chat.open(CHAT_UNKNOWN_ITEM, "rev", "", false);
        let Send::Request(r) = chat.send("问") else { panic!() };
        let result = client.item_chat(&r);
        chat.finish(&r, result);
        assert_eq!(chat.error(), Some(&ChatError::Unknown));
        let again = chat.retry().unwrap();
        let result = client.item_chat(&again);
        chat.finish(&again, result);
        assert_eq!(chat.error(), None);
        assert_eq!(chat.transcript().len(), 2);
        let log = sent.lock().unwrap();
        assert_eq!(log.iter().filter(|c| *c == "item_chat").count(), 3);
    }

    /// Opens `item` and runs its history load (if any) against `client`.
    fn open_and_load(chat: &mut ChatModel, client: &Client, item: &str) {
        chat.open(item, "rev", "", false);
        if let Some((id, token)) = chat.history_to_load() {
            let result = client.conversation_read(&id);
            chat.finish_history(&id, token, result);
        }
    }

    fn recording(controls: FixtureControls) -> (FixtureBackend, Client, Arc<Mutex<Vec<String>>>) {
        let fixture = FixtureBackend::new(30);
        fixture.set_controls(FixtureControls { instant_chat: true, ..controls });
        let sent = Arc::new(Mutex::new(vec![]));
        let client = Client::new(Recording(fixture.clone(), sent.clone()));
        (fixture, client, sent)
    }

    #[test]
    fn stored_conversation_is_read_only_with_item_chat_history_v1() {
        // An agent that does not keep conversations: nothing is read, nothing is shown.
        let (_, client, sent) = recording(FixtureControls { no_chat_history: true, ..Default::default() });
        let mut chat = ChatModel::new(probe(&client));
        chat.set_history_supported(client.capabilities().unwrap().item_chat_history);
        open_and_load(&mut chat, &client, CHAT_HISTORY_ITEM);
        assert_eq!(chat.history(), History::NotLoaded);
        assert!(!chat.has_conversation() && chat.can_send());
        assert!(!sent.lock().unwrap().iter().any(|c| c == "conversation_read"));
        assert_eq!(client.conversation_read(CHAT_HISTORY_ITEM), Err(ClientError::Unsupported));

        // With it: read once per item, shown in order, then never again.
        let (_, client, sent) = recording(FixtureControls::default());
        let mut chat = ChatModel::new(probe(&client));
        chat.set_history_supported(client.capabilities().unwrap().item_chat_history);
        chat.open(CHAT_HISTORY_ITEM, "rev", "", false);
        let (id, token) = chat.history_to_load().unwrap();
        assert!(matches!(chat.history(), History::Loading(_)) && !chat.can_send(), "no send while loading");
        let result = client.conversation_read(&id);
        chat.finish_history(&id, token, result);
        assert_eq!(chat.history(), History::Loaded { empty: false });
        assert_eq!(chat.question_count(), 2);
        assert_eq!(chat.transcript()[1].speaker, Speaker::Assistant);
        chat.open(SHOWCASE_ITEM, "rev", "", false);
        chat.open(CHAT_HISTORY_ITEM, "rev", "", false);
        assert_eq!(chat.history_to_load(), None, "read once per session");
        // One load: its two pages (the fixture pages 3 turns at a time), never a second load.
        assert_eq!(sent.lock().unwrap().iter().filter(|c| *c == "conversation_read").count(), 2);

        // An item without stored turns is honestly empty; a failed read can be retried.
        open_and_load(&mut chat, &client, SHOWCASE_ITEM);
        assert_eq!(chat.history(), History::Loaded { empty: true });
        open_and_load(&mut chat, &client, CHAT_HISTORY_FAIL_ITEM);
        assert!(matches!(chat.history(), History::Failed(_)) && chat.can_send());
        assert!(chat.retry_history().is_some());
    }

    #[test]
    fn a_follow_up_carries_the_stored_turns_as_context_never_as_new_turns() {
        let (_, client, _) = recording(FixtureControls::default());
        let mut chat = ChatModel::new(probe(&client));
        chat.set_history_supported(true);
        open_and_load(&mut chat, &client, CHAT_HISTORY_ITEM);
        let stored: Vec<ChatTurn> = client.conversation_read(CHAT_HISTORY_ITEM).unwrap().iter().map(|t| t.chat_turn()).collect();
        let Send::Request(r) = chat.send("继续：还有哪些局限？") else { panic!() };
        assert_eq!(r.messages[..4], stored[..], "the stored turns go first, unchanged");
        assert_eq!(r.messages.len(), 5);
        assert_eq!(r.messages[4], ChatTurn::user("继续：还有哪些局限？"));
        let result = client.item_chat(&r);
        chat.finish(&r, result);
        assert!(chat.transcript().last().unwrap().text.contains("本次对话共 3 个提问"));
        // The agent stored the one new exchange only, not the history again.
        let after: Vec<ChatTurn> = client.conversation_read(CHAT_HISTORY_ITEM).unwrap().iter().map(|t| t.chat_turn()).collect();
        assert_eq!(after.len(), 6);
        assert_eq!(after[..4], stored[..]);
        assert_eq!(after[4], ChatTurn::user("继续：还有哪些局限？"));
    }

    #[test]
    fn two_items_never_mix_and_a_stale_load_is_dropped() {
        let (_, client, _) = recording(FixtureControls::default());
        let mut chat = ChatModel::new(probe(&client));
        chat.set_history_supported(true);
        // Start item A's load, switch to B before it answers, ask on B.
        chat.open(CHAT_HISTORY_ITEM, "rev", "", false);
        let (a, token_a) = chat.history_to_load().unwrap();
        open_and_load(&mut chat, &client, SHOWCASE_ITEM);
        let Send::Request(rb) = chat.send("B 的问题") else { panic!() };
        assert_eq!(rb.messages, [ChatTurn::user("B 的问题")], "nothing of A in B's request");
        // A's answer lands in A only.
        let result = client.conversation_read(&a);
        chat.finish_history(&a, token_a, result.clone());
        assert_eq!(chat.question_count(), 1, "B still shows its own question only");
        // A replayed or late answer for an old token is ignored.
        chat.finish_history(&a, token_a, Ok(vec![stored_turn(9, "重复", "chat-late-0000000001", None)]));
        let reply_b = client.item_chat(&rb);
        chat.finish(&rb, reply_b);
        chat.open(CHAT_HISTORY_ITEM, "rev", "", false);
        assert_eq!(chat.question_count(), 2);
        assert!(chat.transcript().iter().all(|m| !m.text.contains("B 的问题") && m.text != "重复"));
        // Clearing A forgets the view; the stored turns are not read back into it.
        assert!(chat.clear());
        assert_eq!(chat.history_to_load(), None);
        assert!(!chat.has_conversation());
        // B's stored turns are B's.
        let b = client.conversation_read(SHOWCASE_ITEM).unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].chat_turn(), ChatTurn::user("B 的问题"));
    }

    fn stored_turn(seq: i64, content: &str, request_id: &str, origin: Option<&str>) -> StoredTurn {
        StoredTurn {
            seq,
            role: if seq % 2 == 0 { memex_client::ChatRole::Assistant } else { memex_client::ChatRole::User },
            content: content.into(),
            request_id: request_id.into(),
            created_at_ms: 0,
            model: None,
            origin: origin.map(str::to_string),
            truncated: false,
        }
    }

    #[test]
    fn restored_turns_are_labelled_and_a_late_history_never_repeats_an_exchange() {
        let (_, client, _) = recording(FixtureControls::default());
        let mut chat = ChatModel::new(probe(&client));
        chat.set_history_supported(true);
        open_and_load(&mut chat, &client, CHAT_HISTORY_ITEM);
        let t = chat.transcript();
        assert!(t[0].restored && t[1].restored, "the screenshot exchange is marked");
        assert!(!t[2].restored && !t[3].restored);

        // The read fails, the owner asks anyway, then 重试 brings the stored history,
        // which by now also holds the exchange just asked: it is shown once.
        chat.open("item_x", "rev", "", false);
        let (x, token) = chat.history_to_load().unwrap();
        chat.finish_history(&x, token, Err(ClientError::Daemon("internal_error".into())));
        let Send::Request(r) = chat.send("新问题") else { panic!() };
        assert_eq!(chat.retry_history(), None, "not while the reply is on its way");
        chat.finish(&r, reply(&r, "新回答"));
        let (x, token) = chat.retry_history().unwrap();
        let stored = vec![
            stored_turn(-1, "截图里的问题", "restored:x:00", Some(memex_client::chat::RESTORED_FROM_USER_SCREENSHOT)),
            stored_turn(1, "新问题", &r.request_id, None),
            stored_turn(2, "新回答", &r.request_id, None),
        ];
        chat.finish_history(&x, token, Ok(stored));
        let texts: Vec<&str> = chat.transcript().iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["截图里的问题", "新问题", "新回答"]);
        assert!(chat.transcript()[0].restored);
    }

    #[test]
    fn a_request_identity_conflict_is_a_definite_failure_with_nothing_to_retry() {
        let mut chat = ChatModel::new(Availability::Ready);
        chat.open("item_a", "rev_1", "", false);
        let Send::Request(r) = chat.send("问") else { panic!() };
        chat.finish(&r, Err(ClientError::Daemon("request_identity_conflict".into())));
        let Some(ChatError::Failed(reason)) = chat.error() else { panic!("{:?}", chat.error()) };
        assert!(reason.contains("请求编号"), "{reason}");
        assert_eq!(chat.retry(), None);
        assert!(chat.transcript()[0].unanswered);
        let Send::Request(next) = chat.send("再问") else { panic!() };
        assert_ne!(next.request_id, r.request_id);
        assert_eq!(next.messages, [ChatTurn::user("再问")]);
    }
}
