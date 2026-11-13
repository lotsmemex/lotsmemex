//! 格式整理 (`reader_content_format_v1`) of the item or chapter side on screen,
//! and 批量格式整理 of a folder (`reader_content_format_batch_v1`). Preview
//! (local, no model) → one explicit confirmation → a durable batch → per-target
//! results → 查看修改 (old/new side by side plus the proof list) / 退回旧版.
//!
//! Request ids are stored before anything is sent; an unknown outcome keeps
//! them and only 查询原任务 replays the same id. Nothing is retried on its own:
//! not a request, a restore or a resume. A refused whole-batch read shows no
//! partial counts and no cached row; known succeeded targets stay reviewable
//! and restorable through their own (still authorized) requests.
use super::*;
use super::chapter_translate::usage_label;
use memex_client::book::RevisionTarget;
use memex_client::format::{outcome_unknown, whole_batch_denied, FormatBatch, FormatQuote, FormatResult, FormatSelection, ResultsPage, RestoreReceipt, TargetHandle, TargetState, RESULTS_PAGE};
use memex_client::ClientError;
use gpui_kit::component::checkbox::Checkbox;

/// Shared wording with the phone.
pub(super) const DENIED: &str = "无法读取整批：其中有条目已不可访问";
const UNKNOWN: &str = "结果未知：不确定格式整理是否完成，正文没有显示未确认的修改。可以查询原任务，不会自动重试。";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) enum FlowState {
    #[default]
    Idle,
    /// Reading the local preview (scope, route and budget).
    Previewing,
    Confirm { quote: FormatQuote, consent: bool },
    /// The preview found nothing to send (e.g. already formatted).
    Nothing(String),
    /// The preview or request was refused: nothing was admitted.
    Refused(String),
    /// Sent or followed; `batch` holds the latest status.
    Following,
    /// The request's (or batch's) outcome is unknown: only 查询原任务.
    Unknown,
    /// The whole-batch read was refused.
    Denied,
    /// The preview met `library_loading`: never resent by itself (it may write a
    /// quote); 重试 previews again.
    Busy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RestoreState {
    Sending,
    Done(RestoreReceipt),
    Unknown,
    Failed(String),
}

/// One 格式整理 flow: the target on screen, or one folder.
#[derive(Debug, Clone, Default)]
pub(super) struct Flow {
    /// `item|target` or `group|id`: the persistence key.
    pub key: String,
    pub selection: Option<FormatSelection>,
    /// 本条正文 / 本章原文 · title / folder name.
    pub scope: String,
    pub state: FlowState,
    pub ticket: u64,
    pub batch: Option<FormatBatch>,
    /// The page of results on screen (dropped when a whole-batch read is refused).
    pub results: Option<ResultsPage>,
    /// Known succeeded targets that were just re-authorized (after a refusal).
    pub verified: Vec<TargetHandle>,
    pub verifying: bool,
}

#[derive(Debug, Clone)]
pub(super) struct Review {
    pub handle: TargetHandle,
    pub page: Option<memex_client::format::ChangesPage>,
    /// Change id → (before, before total, after, after total).
    pub texts: HashMap<String, (String, usize, String, usize)>,
    pub error: Option<String>,
}

#[derive(Default)]
pub(super) struct Fmt {
    pub supported: bool,
    pub batch_supported: bool,
    pub single: Flow,
    pub batch: Option<Flow>,
    pub review: Option<Review>,
    pub restores: HashMap<String, RestoreState>,
    /// Item titles for batch rows (from the library).
    pub titles: HashMap<String, String>,
    /// Walks and tests: previews started and `format_request` sends.
    pub previews: usize,
    pub requests: usize,
    /// The last 退回旧版 sent: its target and request id (walks replay it exactly).
    pub last_restore: Option<(TargetHandle, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending {
    request_id: String,
    quote_id: String,
    batch_id: Option<String>,
    /// The revision the selection was read at (single targets).
    revision_id: Option<String>,
}

impl Pending {
    fn to_json(&self) -> String {
        serde_json::json!({ "request_id": self.request_id, "quote_id": self.quote_id, "batch_id": self.batch_id, "revision_id": self.revision_id }).to_string()
    }
    fn from_json(s: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        let opt = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        Some(Self { request_id: opt("request_id")?, quote_id: opt("quote_id")?, batch_id: opt("batch_id"), revision_id: opt("revision_id") })
    }
}

/// What a failed send or read leads to. Nothing here ever retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AfterError {
    /// The outcome is unknown: keep the stored id; only 查询原任务 replays it.
    Unknown,
    /// The whole batch can't be read: no partial rows or counts.
    Denied,
    /// Definitive refusal: nothing was admitted. A sent request's id is dropped.
    Refused { forget_request: bool, message: String },
}

pub(super) fn after_error(e: &ClientError, sent: bool) -> AfterError {
    if outcome_unknown(e) { AfterError::Unknown }
    else if whole_batch_denied(e) { AfterError::Denied }
    else { AfterError::Refused { forget_request: sent, message: e.message() } }
}

fn target_key(t: &RevisionTarget) -> String {
    match t {
        RevisionTarget::Body {} => "body".into(),
        RevisionTarget::Chapter { side, chapter } => format!("{side}:{chapter}"),
    }
}

pub(super) fn reason_label(code: &str) -> String {
    match code {
        "already_formatted" => tx("已整理过，无需再整理").into(),
        "chapter_required" => tx("书籍需要逐章整理").into(),
        "unsupported_body" => tx("这类正文暂不支持整理").into(),
        "restricted" => tx("受限条目不会发给模型").into(),
        "budget_exceeded" => tx("超出单条整理预算").into(),
        "busy" => tx("条目正在处理其他任务").into(),
        "stale_revision" => tx("条目已更新，没有整理").into(),
        "scope_changed" => tx("已不在所选范围内").into(),
        other => ClientError::Daemon(other.into()).message(),
    }
}

fn kind_label(kind: &str) -> String {
    match kind {
        "whitespace" => tx("空白").into(),
        "paragraph_boundary" => tx("分段").into(),
        "garbage_delete" => tx("删除无关内容").into(),
        "typo" => tx("错字").into(),
        "extraction_garble" => tx("提取乱码").into(),
        "punctuation" => tx("标点").into(),
        other => other.into(),
    }
}

fn change_reason_label(code: &str) -> String {
    match code {
        "navigation" => tx("导航").into(),
        "advertisement" => tx("广告").into(),
        "saved_control" => tx("网页控件").into(),
        "account_widget" => tx("账户组件").into(),
        "paragraph_layout" => tx("段落版式").into(),
        "short_correction" => tx("短校正").into(),
        "source_garble" => tx("来源乱码").into(),
        "punctuation" => tx("标点").into(),
        other => other.into(),
    }
}

impl Reader {
    /// The single target on screen: an ordinary item's body, or one chapter of
    /// the side shown (原文 or 译文). `Err(hint)` when the button is off.
    pub(super) fn fmt_target(&self) -> Result<(FormatSelection, String, String), Option<&'static str>> {
        if self.loading || !self.fmt.supported { return Err(None); }
        let detail = self.model.detail().ok_or(None)?;
        let Some(book) = self.model.book() else {
            let item = detail.canonical.item_id.clone();
            let sel = FormatSelection::Item { item_id: item.clone(), revision_id: detail.canonical.revision_id.clone(), target: RevisionTarget::Body {} };
            return Ok((sel, format!("{item}|body"), tx("本条正文").into()));
        };
        if self.chapter_loading { return Err(None); }
        let (side, index) = match self.model.mode() {
            Mode::Bilingual => return Err(Some("切到原文或译文后整理本章")),
            Mode::Original => ("original", book.entries.get(book.at).and_then(|e| e.original)),
            Mode::Translation => ("translation", book.entries.get(book.at).and_then(|e| e.translation)),
        };
        let chapter = index.and_then(|i| book.doc.edition(side)?.chapters.get(i)).ok_or(Some("这一侧没有这一章，不能整理"))?;
        let target = RevisionTarget::Chapter { side: side.into(), chapter: chapter.key.clone() };
        let key = format!("{}|{}", book.doc.item_id, target_key(&target));
        let scope = txf(if side == "original" { "本章原文 · {0}" } else { "本章译文 · {0}" }, &[&chapter.title.trim()]);
        Ok((FormatSelection::Item { item_id: book.doc.item_id.clone(), revision_id: book.doc.revision_id.clone(), target }, key, scope))
    }

    fn fmt_pending(&self, key: &str) -> Option<Pending> {
        self.repair_ids.get(&format!("format|{key}")).and_then(|v| Pending::from_json(v))
    }

    fn fmt_set_pending(&mut self, key: &str, p: Option<&Pending>) {
        match p {
            Some(p) => { self.repair_ids.insert(format!("format|{key}"), p.to_json()); }
            None => { self.repair_ids.remove(&format!("format|{key}")); }
        }
        self.save_repair_ids();
    }

    fn fmt_store_handle(&mut self, h: &TargetHandle) {
        self.repair_ids.insert(format!("format-target|{}|{}", h.batch_id, h.target_id), serde_json::to_string(h).unwrap_or_default());
        self.save_repair_ids();
    }

    fn fmt_forget_handle(&mut self, h: &TargetHandle) {
        self.repair_ids.remove(&format!("format-target|{}|{}", h.batch_id, h.target_id));
        self.save_repair_ids();
    }

    fn fmt_handles(&self, batch_id: &str) -> Vec<TargetHandle> {
        let prefix = format!("format-target|{batch_id}|");
        let mut out: Vec<TargetHandle> = self.repair_ids.iter().filter(|(k, _)| k.starts_with(&prefix)).filter_map(|(_, v)| serde_json::from_str(v).ok()).collect();
        out.sort_by(|a, b| a.target_id.cmp(&b.target_id));
        out
    }

    fn flow_mut(&mut self, group: bool) -> Option<&mut Flow> {
        if group { self.fmt.batch.as_mut() } else { Some(&mut self.fmt.single) }
    }

    fn flow(&self, group: bool) -> Option<&Flow> {
        if group { self.fmt.batch.as_ref() } else { Some(&self.fmt.single) }
    }

    /// After the item, chapter, side or capability changed: what the line shows.
    /// A stored request of this target is followed by reading (never resent);
    /// an old settled result is shown only while it is still the current text.
    pub(super) fn refresh_format(&mut self, cx: &mut Context<Self>) {
        let Ok((selection, key, scope)) = self.fmt_target() else {
            if !self.fmt.single.key.is_empty() { self.fmt.single = Flow { ticket: self.fmt.single.ticket + 1, ..Default::default() }; }
            return;
        };
        if self.fmt.single.key == key {
            // Same target: a new revision of it (e.g. after this result) keeps the flow.
            self.fmt.single.selection = Some(selection);
            self.fmt.single.scope = scope;
            return;
        }
        self.fmt.single = Flow { key: key.clone(), selection: Some(selection.clone()), scope, ticket: self.fmt.single.ticket + 1, ..Default::default() };
        let FormatSelection::Item { revision_id, .. } = &selection else { return };
        let Some(p) = self.fmt_pending(&key) else { return };
        let settled_elsewhere = p.revision_id.as_deref().is_some_and(|r| r != revision_id)
            && !self.fmt_handles(p.batch_id.as_deref().unwrap_or("-")).iter().any(|h| h.result_revision_id == *revision_id);
        match p.batch_id {
            // An old result of an earlier text is not this text's state.
            Some(_) if settled_elsewhere => {}
            Some(batch) => self.fmt_follow(false, None, Some(batch), cx),
            None => self.fmt.single.state = FlowState::Unknown,
        }
    }

    /// 格式整理 / 再试一次: a fresh local preview, then the confirmation.
    pub fn ask_format(&mut self, cx: &mut Context<Self>) {
        if self.editing.visible { return; }
        let Ok((selection, key, scope)) = self.fmt_target() else { return };
        if self.fmt.single.key != key { self.fmt.single = Flow { key, scope, ticket: self.fmt.single.ticket + 1, ..Default::default() }; }
        self.fmt.single.selection = Some(selection);
        self.fmt_preview(false, cx);
    }

    /// 批量格式整理 of a folder (from the sidebar): the panel replaces the text.
    pub fn open_format_batch(&mut self, group_id: &str, name: &str, include_descendants: bool, titles: HashMap<String, String>, cx: &mut Context<Self>) {
        if !self.fmt.batch_supported { return; }
        self.fmt.titles.extend(titles);
        let key = format!("group|{group_id}");
        let ticket = self.fmt.batch.as_ref().map_or(0, |b| b.ticket + 1);
        self.fmt.batch = Some(Flow { key: key.clone(), scope: name.into(), ticket,
            selection: Some(FormatSelection::Group { group_id: group_id.into(), include_descendants }), ..Default::default() });
        self.compare = None;
        self.fmt.review = None;
        // A batch of this folder already sent is shown (by reading), not resent.
        match self.fmt_pending(&key) {
            Some(Pending { batch_id: Some(batch), .. }) => self.fmt_follow(true, None, Some(batch), cx),
            Some(_) => { if let Some(f) = self.fmt.batch.as_mut() { f.state = FlowState::Unknown; } cx.notify(); }
            None => self.fmt_preview(true, cx),
        }
    }

    pub fn close_format_batch(&mut self, cx: &mut Context<Self>) {
        self.fmt.batch = None;
        if self.fmt.review.is_some() { self.compare = None; self.fmt.review = None; }
        cx.notify();
    }

    /// 含子文件夹: a different selection, so a new preview.
    pub fn set_format_descendants(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(f) = self.fmt.batch.as_mut() else { return };
        if !matches!(f.state, FlowState::Confirm { .. } | FlowState::Nothing(_) | FlowState::Refused(_) | FlowState::Busy | FlowState::Idle) { return; }
        let Some(FormatSelection::Group { include_descendants, .. }) = f.selection.as_mut() else { return };
        *include_descendants = on;
        self.fmt_preview(true, cx);
    }

    /// Starts a new batch of the folder after a settled one (new preview, new id).
    pub fn restart_format_batch(&mut self, cx: &mut Context<Self>) {
        if let Some(f) = self.fmt.batch.as_mut() {
            f.batch = None;
            f.results = None;
            f.verified.clear();
        }
        self.fmt_preview(true, cx);
    }

    fn fmt_preview(&mut self, group: bool, cx: &mut Context<Self>) {
        let Some(f) = self.flow_mut(group) else { return };
        let Some(selection) = f.selection.clone() else { return };
        if matches!(f.state, FlowState::Previewing) { return; }
        f.ticket += 1;
        let ticket = f.ticket;
        f.state = FlowState::Previewing;
        self.fmt.previews += 1;
        cx.notify();
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { client.format_preview(&selection, Duration::from_millis(250)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(f) = this.flow_mut(group).filter(|f| f.ticket == ticket) else { return };
                f.state = match result {
                    Ok(p) => match (p.state.as_str(), p.quote) {
                        ("ready", Some(q)) if q.counts.eligible > 0 => FlowState::Confirm { quote: q, consent: false },
                        ("ready", Some(q)) => {
                            let reasons: Vec<String> = q.counts.skipped_by_reason.iter().map(|(r, n)| if group { format!("{} {n}", reason_label(r)) } else { reason_label(r) }).collect();
                            FlowState::Nothing(if reasons.is_empty() { tx("没有需要整理的内容").into() } else { reasons.join(" · ") })
                        }
                        _ => FlowState::Refused(ClientError::Daemon(p.error_code.unwrap_or_else(|| "content_format_unavailable".into())).message()),
                    },
                    Err(e) if e.is_busy() => FlowState::Busy,
                    Err(e) => FlowState::Refused(e.message()),
                };
                cx.notify();
            }).ok();
        }).detach();
    }

    pub fn cancel_format(&mut self, group: bool, cx: &mut Context<Self>) {
        if let Some(f) = self.flow_mut(group) {
            if matches!(f.state, FlowState::Confirm { .. }) { f.state = FlowState::Idle; }
        }
        if group && self.fmt.batch.as_ref().is_some_and(|f| f.batch.is_none() && f.state == FlowState::Idle) { self.fmt.batch = None; }
        cx.notify();
    }

    pub fn set_format_consent(&mut self, group: bool, on: bool, cx: &mut Context<Self>) {
        if let Some(FlowState::Confirm { consent, .. }) = self.flow_mut(group).map(|f| &mut f.state) { *consent = on; }
        cx.notify();
    }

    /// 确认整理: the request id and quote are stored before anything is sent.
    pub fn confirm_format(&mut self, group: bool, cx: &mut Context<Self>) {
        let Some(f) = self.flow(group) else { return };
        let FlowState::Confirm { quote, consent } = f.state.clone() else { return };
        if quote.counts.eligible == 0 || (quote.counts.private_targets > 0 && !consent) { return; }
        let revision_id = match &quote.selection { FormatSelection::Item { revision_id, .. } => Some(revision_id.clone()), _ => None };
        let p = Pending { request_id: format!("mac-fmt-{}", memex_client::biblio::new_request_id()), quote_id: quote.quote_id.clone(), batch_id: None, revision_id };
        let key = f.key.clone();
        self.fmt_set_pending(&key, Some(&p));
        if let Some(f) = self.flow_mut(group) { f.batch = None; f.results = None; f.verified.clear(); }
        self.fmt_follow(group, Some((p.quote_id, p.request_id)), None, cx);
    }

    /// 查询原任务: replays the stored request (same id and quote); the agent
    /// returns the original batch and never dispatches it again.
    pub fn query_format(&mut self, group: bool, cx: &mut Context<Self>) {
        let Some(key) = self.flow(group).map(|f| f.key.clone()) else { return };
        let Some(p) = self.fmt_pending(&key) else { return };
        self.fmt_follow(group, Some((p.quote_id, p.request_id)), None, cx);
    }

    /// 继续整理: resumes the paused batch under its original budget. Refused →
    /// nothing is requeued; it is not retried.
    pub fn resume_format(&mut self, group: bool, cx: &mut Context<Self>) {
        let Some(batch_id) = self.flow(group).and_then(|f| f.batch.as_ref()).filter(|b| b.state == "paused").map(|b| b.batch_id.clone()) else { return };
        let Some(f) = self.flow_mut(group) else { return };
        f.ticket += 1;
        let ticket = f.ticket;
        cx.notify();
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { client.format_resume(&batch_id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.flow(group).is_none_or(|f| f.ticket != ticket) { return; }
                match result {
                    Ok(b) => this.fmt_follow(group, None, Some(b.batch_id), cx),
                    Err(e) if whole_batch_denied(&e) => this.fmt_denied(group, cx),
                    Err(e) => { if let Some(f) = this.flow_mut(group) { f.state = FlowState::Refused(e.message()); } }
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    /// The whole batch can no longer be read: drop every cached row, then
    /// re-authorize the known succeeded targets one by one (their own reads).
    fn fmt_denied(&mut self, group: bool, cx: &mut Context<Self>) {
        let Some(f) = self.flow_mut(group) else { return };
        f.state = FlowState::Denied;
        f.results = None;
        let batch_id = f.batch.take().map(|b| b.batch_id);
        f.verified.clear();
        f.verifying = true;
        let ticket = f.ticket;
        let key = f.key.clone();
        let batch_id = batch_id.or_else(|| self.fmt_pending(&key).and_then(|p| p.batch_id));
        let handles = batch_id.map(|b| self.fmt_handles(&b)).unwrap_or_default();
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move {
            handles.into_iter().map(|h| { let ok = client.format_changes(&h, 0); (h, ok) }).collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let checked = task.await;
            this.update(cx, |this, cx| {
                for (h, r) in &checked {
                    // Refused now: forgotten, never shown from the cache.
                    if matches!(r, Err(e) if !outcome_unknown(e) && !whole_batch_denied(e)) { this.fmt_forget_handle(h); }
                }
                let Some(f) = this.flow_mut(group).filter(|f| f.ticket == ticket) else { return };
                f.verifying = false;
                f.verified = checked.into_iter().filter(|(_, r)| r.is_ok()).map(|(h, _)| h).collect();
                cx.notify();
            }).ok();
        }).detach();
        cx.notify();
    }

    /// Sends (or replays) the request, or reads `batch_id`, follows the batch to
    /// a settled state and reads its results.
    pub(super) fn fmt_follow(&mut self, group: bool, send: Option<(String, String)>, batch_id: Option<String>, cx: &mut Context<Self>) {
        let Some(f) = self.flow_mut(group) else { return };
        f.ticket += 1;
        let ticket = f.ticket;
        f.state = FlowState::Following;
        let key = f.key.clone();
        let offset = f.results.as_ref().map_or(0, |r| r.offset);
        if send.is_some() { self.fmt.requests += 1; }
        cx.notify();
        let client = self.client.clone();
        let alive = move |this: &Reader| this.flow(group).is_some_and(|f| f.ticket == ticket);
        cx.spawn(async move |this, cx| {
            let first = cx.background_executor().spawn({
                let client = client.clone();
                async move {
                    match (send, batch_id) {
                        (Some((quote, request)), _) => client.format_request(&quote, &request).map_err(|e| (true, e)),
                        (None, Some(b)) => client.format_status(&b).map_err(|e| (false, e)),
                        _ => Err((false, ClientError::InvalidFrame)),
                    }
                }
            }).await;
            let mut batch = match first {
                Ok(b) => b,
                Err((sent, e)) => {
                    this.update(cx, |this, cx| {
                        if !alive(this) { return; }
                        match after_error(&e, sent) {
                            AfterError::Unknown => if let Some(f) = this.flow_mut(group) { f.state = FlowState::Unknown; },
                            AfterError::Denied => this.fmt_denied(group, cx),
                            AfterError::Refused { forget_request, message } => {
                                if forget_request { this.fmt_set_pending(&key, None); }
                                if let Some(f) = this.flow_mut(group) { f.state = FlowState::Refused(message); }
                            }
                        }
                        cx.notify();
                    }).ok();
                    return;
                }
            };
            // Remember the batch: leaving and returning reads it instead of resending.
            this.update(cx, |this, _| {
                if let Some(mut p) = this.fmt_pending(&key) {
                    if p.batch_id.as_deref() != Some(&batch.batch_id) { p.batch_id = Some(batch.batch_id.clone()); this.fmt_set_pending(&key, Some(&p)); }
                }
            }).ok();
            loop {
                let b = batch.clone();
                let go = this.update(cx, |this, cx| {
                    if !alive(this) { return false; }
                    if let Some(f) = this.flow_mut(group) { f.batch = Some(b); }
                    cx.notify();
                    true
                }).unwrap_or(false);
                if !go { return; }
                if batch.is_settled() { break; }
                cx.background_executor().timer(Duration::from_millis(1000)).await;
                let id = batch.batch_id.clone();
                match cx.background_executor().spawn({ let client = client.clone(); async move { client.format_status(&id) } }).await {
                    Ok(next) => batch = next,
                    Err(e) => {
                        this.update(cx, |this, cx| {
                            if !alive(this) { return; }
                            if whole_batch_denied(&e) { this.fmt_denied(group, cx); }
                            else if let Some(f) = this.flow_mut(group) { f.state = FlowState::Unknown; }
                            cx.notify();
                        }).ok();
                        return;
                    }
                }
            }
            this.update(cx, |this, cx| { if alive(this) { this.fmt_read_results(group, offset, cx); } }).ok();
        }).detach();
    }

    /// One page of results (reads only). Succeeded targets are remembered as handles.
    pub fn fmt_read_results(&mut self, group: bool, offset: usize, cx: &mut Context<Self>) {
        let Some(f) = self.flow(group) else { return };
        let Some(batch_id) = f.batch.as_ref().map(|b| b.batch_id.clone()) else { return };
        let ticket = f.ticket;
        let client = self.client.clone();
        let limit = if group { RESULTS_PAGE } else { 1 };
        let task = cx.background_executor().spawn(async move { client.format_results(&batch_id, offset, limit) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.flow(group).is_none_or(|f| f.ticket != ticket) { return; }
                match result {
                    Ok(page) => {
                        let mut reread = false;
                        let mut saved = false;
                        for row in &page.results {
                            if let Some(h) = row.handle(&page.batch_id) {
                                this.fmt_store_handle(&h);
                                if h.changed {
                                    saved = true;
                                    // The item on screen moved to the result revision: read it again.
                                    let current = this.model.book().map(|b| b.doc.revision_id.clone()).or_else(|| this.model.detail().map(|d| d.canonical.revision_id.clone()));
                                    if this.model.active_item() == Some(h.item_id.as_str()) && current.as_deref() == Some(h.input_revision_id.as_str()) { reread = true; }
                                }
                            }
                        }
                        if let Some(f) = this.flow_mut(group) { f.results = Some(page); }
                        if saved { cx.emit(ReaderEvent::Saved); }
                        if reread {
                            if let Some(at) = this.model.book().map(|b| b.at) { this.after_repair = Some(at); }
                            this.reload(cx);
                        }
                    }
                    Err(e) if whole_batch_denied(&e) => this.fmt_denied(group, cx),
                    Err(e) => if let Some(f) = this.flow_mut(group) { f.state = if outcome_unknown(&e) { FlowState::Unknown } else { FlowState::Refused(e.message()) }; },
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    /// 查看修改: the input and result revisions side by side, and the proof list.
    pub fn review_format(&mut self, h: TargetHandle, cx: &mut Context<Self>) {
        if self.revision_text_supported {
            let title = self.fmt.titles.get(&h.item_id).cloned().or_else(|| self.model.detail().filter(|d| d.canonical.item_id == h.item_id).map(|d| d.canonical.title.clone())).unwrap_or_default();
            // Any row of a batch (not only the item on screen); closing it closes the list too.
            let spec = crate::compare::CompareSpec { item_id: h.item_id.clone(), target: h.target.clone(), old_revision: h.input_revision_id.clone(), new_revision: h.result_revision_id.clone(), title };
            let client = self.client.clone();
            let compare = cx.new(|cx| crate::compare::RevisionCompare::new(client, spec, cx));
            let sub = cx.subscribe(&compare, |this: &mut Self, _, event, cx| match event {
                crate::compare::CompareEvent::Close => { this.compare = None; this.fmt.review = None; cx.notify(); }
            });
            self.compare = Some((compare, sub));
            self.editing.visible = false;
        }
        self.fmt.review = Some(Review { handle: h.clone(), page: None, texts: HashMap::new(), error: None });
        cx.notify();
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move {
            let page = client.format_changes(&h, 0)?;
            // Short excerpts of each change; the full text is in the comparison.
            let mut texts = HashMap::new();
            for c in &page.changes {
                let before = if c.before_bytes > 0 { client.format_change_text(&h, &c.id, "before", 360)? } else { (String::new(), 0) };
                let after = if c.after_bytes > 0 { client.format_change_text(&h, &c.id, "after", 360)? } else { (String::new(), 0) };
                texts.insert(c.id.clone(), (before.0, before.1, after.0, after.1));
            }
            Ok::<_, ClientError>((h, page, texts))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(r) = this.fmt.review.as_mut() else { return };
                match result {
                    Ok((h, page, texts)) if h == r.handle => { r.page = Some(page); r.texts = texts; }
                    Ok(_) => {}
                    Err(e) => {
                        let gone = !outcome_unknown(&e);
                        r.error = Some(if gone { tx("这条整理记录现在不可读取").to_string() } else { e.message() });
                        if gone && !whole_batch_denied(&e) { let h = r.handle.clone(); this.fmt_forget_handle(&h); }
                    }
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    /// 退回旧版: a new revision with this target's bytes from before the
    /// formatting (CAS on the result revision). The id is stored first; an
    /// unknown outcome is checked with the same id, never resent as new.
    pub fn restore_format(&mut self, h: TargetHandle, cx: &mut Context<Self>) {
        let key = format!("format-restore|{}|{}", h.batch_id, h.target_id);
        if matches!(self.fmt.restores.get(&h.target_id), Some(RestoreState::Sending | RestoreState::Done(_))) { return; }
        let request_id = self.repair_ids.get(&key).cloned().unwrap_or_else(|| format!("mac-fmt-restore-{}", memex_client::biblio::new_request_id()));
        self.repair_ids.insert(key.clone(), request_id.clone());
        self.save_repair_ids();
        self.fmt.restores.insert(h.target_id.clone(), RestoreState::Sending);
        self.fmt.last_restore = Some((h.clone(), request_id.clone()));
        cx.notify();
        let client = self.client.clone();
        let sent = h.clone();
        let task = cx.background_executor().spawn(async move { client.format_restore(&sent, &request_id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let state = match result {
                    Ok(receipt) => {
                        this.repair_ids.remove(&key);
                        this.save_repair_ids();
                        if receipt.changed {
                            cx.emit(ReaderEvent::Saved);
                            let current = this.model.book().map(|b| b.doc.revision_id.clone()).or_else(|| this.model.detail().map(|d| d.canonical.revision_id.clone()));
                            if this.model.active_item() == Some(h.item_id.as_str()) && current.as_deref() == Some(h.result_revision_id.as_str()) {
                                if let Some(at) = this.model.book().map(|b| b.at) { this.after_repair = Some(at); }
                                this.reload(cx);
                            }
                        }
                        RestoreState::Done(receipt)
                    }
                    Err(e) if outcome_unknown(&e) => RestoreState::Unknown,
                    Err(e) => {
                        this.repair_ids.remove(&key);
                        this.save_repair_ids();
                        let conflict = matches!(&e, ClientError::Daemon(c) if matches!(c.as_str(), "content_format_restore_conflict" | "stale_item_revision"));
                        if !conflict && !whole_batch_denied(&e) && !matches!(&e, ClientError::Daemon(c) if c == "reader_job_in_progress") { this.fmt_forget_handle(&h); }
                        RestoreState::Failed(if conflict { tx("条目在整理后又被修改，没有退回；新的修改保留。").into() } else { e.message() })
                    }
                };
                this.fmt.restores.insert(h.target_id.clone(), state);
                cx.notify();
            }).ok();
        }).detach();
    }

    // ---- rendering ----

    /// 格式整理 in the toolbar (off in 对照, with the reason).
    pub(super) fn format_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.fmt.supported || self.model.detail().is_none() { return None; }
        let target = self.fmt_target();
        // Busy only until the flow settles: afterwards 格式整理 previews again
        // (an already formatted target shows so without any model call).
        let f = &self.fmt.single;
        let busy = matches!(f.state, FlowState::Previewing) || (f.state == FlowState::Following && f.results.is_none());
        let enabled = target.is_ok() && !self.editing.visible && !busy;
        let hint = target.err().flatten();
        Some(Button::new("format-cleanup").small().outline().label(tx("格式整理")).disabled(!enabled)
            .when_some(hint, |b, h| b.tooltip(tx(h)))
            .on_click(cx.listener(|r, _, _, cx| r.ask_format(cx))).into_any_element())
    }

    fn confirm_card(&self, group: bool, quote: &FormatQuote, consent: bool, scope: &str, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let text = |t: String| div().child(t);
        let muted = |t: String| div().text_color(theme.muted_foreground).child(t);
        let c = &quote.counts;
        let scope_line = if group {
            let mut s = txf("范围：{0} · {1} 条", &[&scope, &c.items]);
            if c.skipped > 0 {
                let reasons = c.skipped_by_reason.iter().map(|(r, n)| format!("{} {n}", reason_label(r))).collect::<Vec<_>>().join("、");
                s.push_str(&txf("；整理 {0} 条，跳过 {1} 条（{2}）", &[&c.eligible, &c.skipped, &reasons]));
            }
            s
        } else {
            txf("范围：{0}", &[&scope])
        };
        let route = match &quote.route {
            Some(r) => txf("模型 {0} · 推理强度 {1} · 服务 {2}", &[&r.model_id, &r.reasoning_effort.clone().unwrap_or_else(|| tx("默认").into()), &r.provider_id]),
            None => tx("不需要调用模型").into(),
        };
        let b = &quote.budget;
        let budget = txf("计划调用至多 {0} 次；输入至多 {1} KB，输出至多 {2} tokens", &[&b.max_requests, &b.max_input_bytes.div_ceil(1024), &b.max_completion_tokens]);
        let private = c.private_targets > 0;
        let descendants = match &quote.selection { FormatSelection::Group { include_descendants, .. } => Some(*include_descendants), _ => None };
        v_flex().gap_1p5().px_3().py_2().rounded(px(8.)).border_1().border_color(theme.border).bg(theme.secondary).text_xs()
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(tx("格式整理前请确认")))
            .child(text(scope_line))
            .when_some(descendants, |d, on| d.child(Checkbox::new("format-descendants").label(tx("含子文件夹")).checked(on)
                .on_click(cx.listener(|r, checked: &bool, _, cx| r.set_format_descendants(*checked, cx)))))
            .child(text(route))
            .child(text(budget))
            .when_some(quote.estimated_cost.as_ref(), |d, c| d.child(text(txf("预估费用约 {0} {1}", &[&c.amount, &c.currency]))))
            .child(muted(tx("用 Low 挡位模型整理分段、空白和明显的无关内容，不翻译、不改写观点；旧版保留，可查看修改或退回旧版。").into()))
            .when(group, |d| d.child(muted(tx("修改的是条目本身，在其他分组里也会看到；之后新加入文件夹的条目不包括在内。").into())))
            .when(private, |d| d.child(muted(txf("其中 {0} 条是私密内容", &[&c.private_targets])))
                .child(Checkbox::new("format-consent").label(tx("同意把所选正文发给模型")).checked(consent)
                    .on_click(cx.listener(move |r, checked: &bool, _, cx| r.set_format_consent(group, *checked, cx)))))
            .child(h_flex().gap_2().pt_1()
                .child(Button::new("format-confirm").xsmall().primary().label(tx("确认整理")).disabled(private && !consent)
                    .on_click(cx.listener(move |r, _, _, cx| r.confirm_format(group, cx))))
                .child(Button::new("format-cancel").xsmall().outline().label(tx("取消")).on_click(cx.listener(move |r, _, _, cx| r.cancel_format(group, cx)))))
    }

    fn restore_controls(&self, id: (&'static str, usize), h: &TargetHandle, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let muted = |t: String| div().text_color(theme.muted_foreground).child(t);
        let review = { let h = h.clone(); Button::new((id.0, id.1 * 2)).xsmall().outline().label(tx("查看修改")).on_click(cx.listener(move |r, _, _, cx| r.review_format(h.clone(), cx))) };
        let restore = |label: &'static str, cx: &mut Context<Self>| { let h = h.clone(); Button::new((id.0, id.1 * 2 + 1)).xsmall().ghost().label(tx(label)).on_click(cx.listener(move |r, _, _, cx| r.restore_format(h.clone(), cx))) };
        let row = h_flex().gap_2().items_center().flex_wrap();
        if !h.changed { return row; }
        match self.fmt.restores.get(&h.target_id) {
            None => row.child(review).child(restore("退回旧版", cx)),
            Some(RestoreState::Sending) => row.child(review).child(muted(tx("正在退回旧版…").into())),
            Some(RestoreState::Done(_)) => row.child(review).child(muted(tx("已退回旧版（新建了一个版本，整理结果仍可查看）").into())),
            Some(RestoreState::Unknown) => row.child(review).child(muted(tx("退回结果未知：可能已经退回，也可能没有。").into())).child(restore("核实退回结果", cx)),
            Some(RestoreState::Failed(m)) => row.child(review).child(muted(m.clone())),
        }
    }

    /// The single target's line under the toolbar.
    pub(super) fn format_line(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = &self.fmt.single;
        if !self.fmt.supported || f.key.is_empty() || self.fmt_target().ok().is_none_or(|t| t.1 != f.key) { return None; }
        let theme = cx.theme().clone();
        let line = || h_flex().gap_2().items_center().flex_wrap().px_3().py_1p5().rounded(px(6.)).bg(theme.secondary).text_xs();
        let text = |t: String| div().child(t);
        let muted = |t: String| div().text_color(theme.muted_foreground).child(t);
        let warn = || Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning);
        let row = f.results.as_ref().and_then(|p| p.results.first()).cloned();
        let el = match (&f.state, row) {
            (FlowState::Idle, _) => return None,
            (FlowState::Previewing, _) => line().child(Icon::new(IconName::Loader).xsmall().text_color(theme.primary)).child(text(tx("正在读取整理范围和预算…").into())),
            (FlowState::Confirm { quote, consent }, _) => self.confirm_card(false, quote, *consent, &f.scope, cx),
            (FlowState::Nothing(m), _) => line().child(Icon::new(IconName::Check).xsmall().text_color(theme.primary)).child(text(m.clone())),
            (FlowState::Refused(m), _) => line().child(warn()).child(text(tx("格式整理没有成功").into())).child(muted(m.clone()))
                .child(Button::new("format-again").xsmall().outline().label(tx("再试一次")).on_click(cx.listener(|r, _, _, cx| r.ask_format(cx)))),
            (FlowState::Unknown, _) => line().child(warn()).child(text(tx(UNKNOWN).into()))
                .child(Button::new("format-query").xsmall().outline().label(tx("查询原任务")).on_click(cx.listener(|r, _, _, cx| r.query_format(false, cx)))),
            (FlowState::Denied, _) => line().child(warn()).child(text(tx(DENIED).into())),
            (FlowState::Busy, _) => line().child(Icon::new(IconName::Info).xsmall().text_color(theme.muted_foreground)).child(text(tx("Mac 正忙，稍后重试").into()))
                .child(Button::new("format-busy-retry").xsmall().outline().label(tx("重试")).on_click(cx.listener(|r, _, _, cx| r.ask_format(cx)))),
            (FlowState::Following, None) => {
                let paused = f.batch.as_ref().is_some_and(|b| b.state == "paused");
                if paused {
                    line().child(muted(tx("格式整理已暂停").into())).child(Button::new("format-resume").xsmall().outline().label(tx("继续整理")).on_click(cx.listener(|r, _, _, cx| r.resume_format(false, cx))))
                } else {
                    line().child(Icon::new(IconName::Loader).xsmall().text_color(theme.primary)).child(text(tx("正在整理格式…").into()))
                        .when_some(f.batch.as_ref().filter(|b| b.usage.requests_dispatched > 0), |d, b| d.child(muted(usage_label(&b.usage))))
                }
            }
            (FlowState::Following, Some(row)) => self.single_result(&row, f.results.as_ref().map(|p| p.batch_id.as_str()).unwrap_or_default(), cx),
        };
        Some(el.into_any_element())
    }

    fn single_result(&self, row: &FormatResult, batch_id: &str, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let text = |t: String| div().child(t);
        let muted = |t: String| div().text_color(theme.muted_foreground).child(t);
        let usage = row.job.as_ref().map(|j| usage_label(&j.usage));
        match row.state {
            TargetState::Succeeded => {
                let h = row.handle(batch_id);
                let f = row.formatting.clone();
                let details = f.as_ref().map(|f| {
                    let mut parts = vec![if f.changed { txf("{0} 处修改", &[&f.applied_count]) } else { tx("没有需要修改的地方").into() }];
                    if f.deleted_blocks > 0 { parts.push(txf("删除 {0} 段无关内容", &[&f.deleted_blocks])); }
                    if f.correction_count > 0 { parts.push(txf("校正 {0} 处", &[&f.correction_count])); }
                    if f.status == "partial" { parts.push(txf("{0} 处未确定，保持原样", &[&f.unresolved_count])); }
                    parts.extend(usage.clone());
                    parts.join(" · ")
                }).unwrap_or_default();
                v_flex().gap_1().px_3().py_1p5().rounded(px(6.)).bg(theme.secondary).text_xs()
                    .child(h_flex().gap_2().items_center().child(Icon::new(IconName::Check).xsmall().text_color(theme.primary)).child(text(tx("格式已整理").into())))
                    .child(muted(details))
                    .when_some(h, |d, h| d.child(self.restore_controls(("format-single", 0), &h, cx)))
            }
            TargetState::Skipped => h_flex().gap_2().items_center().px_3().py_1p5().rounded(px(6.)).bg(theme.secondary).text_xs()
                .child(Icon::new(IconName::Check).xsmall().text_color(theme.primary))
                .child(text(row.reason.as_deref().map(reason_label).unwrap_or_else(|| tx("已跳过").into()))),
            TargetState::OutcomeUnknown => h_flex().gap_2().items_center().flex_wrap().px_3().py_1p5().rounded(px(6.)).bg(theme.secondary).text_xs()
                .child(Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning)).child(text(tx(UNKNOWN).into()))
                .when_some(usage, |d, u| d.child(muted(u)))
                .child(Button::new("format-query").xsmall().outline().label(tx("查询原任务")).on_click(cx.listener(|r, _, _, cx| r.query_format(false, cx)))),
            TargetState::Failed => {
                let reason = row.job.as_ref().and_then(|j| j.error_code.clone()).or(row.reason.clone()).map(|c| reason_label(&c));
                let head = h_flex().gap_2().items_center().flex_wrap().child(Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning))
                    .child(text(tx("格式整理没有成功").into())).child(muted("·".into()))
                    .child(Button::new("format-again").xsmall().outline().label(tx("再试一次")).on_click(cx.listener(|r, _, _, cx| r.ask_format(cx))));
                let details = [reason, usage].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                v_flex().gap_1().px_3().py_1p5().rounded(px(6.)).bg(theme.secondary).text_xs().child(head).child(muted(details))
            }
            TargetState::Queued | TargetState::Running => h_flex().gap_2().items_center().px_3().py_1p5().rounded(px(6.)).bg(theme.secondary).text_xs()
                .child(Icon::new(IconName::Loader).xsmall().text_color(theme.primary)).child(text(tx("正在整理格式…").into())),
        }
    }

    fn target_label(&self, item_id: &str, target: &RevisionTarget) -> String {
        let title = self.fmt.titles.get(item_id).cloned().unwrap_or_else(|| item_id.chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect());
        match target {
            RevisionTarget::Body {} => title,
            RevisionTarget::Chapter { side, chapter } => format!("{title} · {} {chapter}", tx(if side == "original" { "原文" } else { "译文" })),
        }
    }

    /// 批量格式整理: replaces the text while open.
    pub(super) fn format_batch_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = self.fmt.batch.as_ref()?;
        let theme = cx.theme().clone();
        let text = |t: String| div().child(t);
        let muted = |t: String| div().text_color(theme.muted_foreground).child(t);
        let warn = || Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning);
        let head = h_flex().gap_2().items_baseline().pb_2()
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(tx("批量格式整理")))
            .child(div().flex_1().min_w_0().truncate().text_sm().text_color(theme.muted_foreground).child(f.scope.clone()))
            .child(Button::new("format-batch-close").small().ghost().icon(IconName::Close).label(tx("关闭")).on_click(cx.listener(|r, _, _, cx| r.close_format_batch(cx))));
        let mut body = v_flex().gap_2().text_sm();
        match &f.state {
            FlowState::Idle => {}
            FlowState::Previewing => body = body.child(h_flex().gap_2().child(Icon::new(IconName::Loader).xsmall()).child(text(tx("正在读取整理范围和预算…").into()))),
            FlowState::Confirm { quote, consent } => body = body.child(self.confirm_card(true, quote, *consent, &f.scope, cx)),
            FlowState::Nothing(m) => body = body.child(text(tx("这个文件夹里没有需要整理的条目").into())).child(muted(m.clone())),
            FlowState::Refused(m) => body = body.child(h_flex().gap_2().items_center().child(warn()).child(text(tx("格式整理没有成功").into())).child(muted(m.clone()))
                .child(Button::new("format-batch-again").xsmall().outline().label(tx("再试一次")).on_click(cx.listener(|r, _, _, cx| r.restart_format_batch(cx))))),
            FlowState::Unknown => body = body.child(h_flex().gap_2().items_center().flex_wrap().child(warn()).child(text(tx(UNKNOWN).into()))
                .child(Button::new("format-batch-query").xsmall().outline().label(tx("查询原任务")).on_click(cx.listener(|r, _, _, cx| r.query_format(true, cx))))),
            FlowState::Busy => body = body.child(h_flex().gap_2().items_center().child(Icon::new(IconName::Info).xsmall().text_color(theme.muted_foreground))
                .child(text(tx("Mac 正忙，稍后重试").into()))
                .child(Button::new("format-batch-busy-retry").xsmall().outline().label(tx("重试")).on_click(cx.listener(|r, _, _, cx| r.restart_format_batch(cx))))),
            FlowState::Denied => {
                body = body.child(h_flex().gap_2().items_center().child(warn()).child(text(tx(DENIED).into())))
                    .child(muted(tx("整批记录仍然保存，没有重新排队，也没有发送新的请求。下面只列出现在仍可读取的已整理条目。").into()));
                if f.verifying { body = body.child(muted(tx("正在逐条核对…").into())); }
                for (i, h) in f.verified.iter().enumerate() {
                    body = body.child(h_flex().gap_3().items_center().py_1().border_b_1().border_color(theme.border)
                        .child(div().flex_1().min_w_0().truncate().child(self.target_label(&h.item_id, &h.target)))
                        .child(self.restore_controls(("format-verified", i), h, cx)));
                }
            }
            FlowState::Following => {
                if let Some(b) = &f.batch {
                    let c = &b.counts;
                    let summary = txf("共 {0} 条：已整理 {1} · 没有成功 {2} · 结果未知 {3} · 跳过 {4}", &[&c.total, &c.succeeded, &c.failed, &c.outcome_unknown, &c.skipped]);
                    body = match b.state.as_str() {
                        "done" => body.child(h_flex().gap_2().items_center().child(Icon::new(IconName::Check).xsmall().text_color(theme.primary))
                            .child(text(tx(if c.succeeded == c.total - c.skipped && c.skipped < c.total { "格式已整理" } else { "整理结束，部分条目没有整理" }).into()))),
                        "paused" => body.child(h_flex().gap_2().items_center().child(text(tx("格式整理已暂停").into()))
                            .child(Button::new("format-batch-resume").xsmall().outline().label(tx("继续整理")).on_click(cx.listener(|r, _, _, cx| r.resume_format(true, cx))))),
                        _ => body.child(h_flex().gap_2().items_center().child(Icon::new(IconName::Loader).xsmall().text_color(theme.primary))
                            .child(text(tx("正在整理格式…").into())).child(muted(txf("{0}/{1}", &[&c.settled(), &c.total])))),
                    };
                    body = body.child(muted(summary)).child(muted(usage_label(&b.usage)));
                    if b.state == "done" && c.outcome_unknown > 0 {
                        body = body.child(muted(tx("结果未知的条目不会自动重试；它们的花费已计入。").into()));
                    }
                    if b.state == "done" {
                        body = body.child(Button::new("format-batch-new").xsmall().ghost().label(tx("重新整理这个文件夹")).on_click(cx.listener(|r, _, _, cx| r.restart_format_batch(cx))));
                    }
                }
                if let Some(page) = &f.results {
                    let mut list = v_flex().gap_0p5().pt_1();
                    for (i, row) in page.results.iter().enumerate() {
                        let status = match row.state {
                            TargetState::Succeeded if row.formatting.as_ref().is_some_and(|f| !f.changed) => tx("已整理：没有需要修改的地方").to_string(),
                            TargetState::Succeeded => row.formatting.as_ref().map(|f| txf("已整理 · {0} 处修改", &[&f.applied_count])).unwrap_or_else(|| tx("已整理").into()),
                            TargetState::Skipped => txf("跳过：{0}", &[&row.reason.as_deref().map(reason_label).unwrap_or_default()]),
                            TargetState::Failed => txf("没有成功：{0}", &[&row.job.as_ref().and_then(|j| j.error_code.clone()).or(row.reason.clone()).map(|c| reason_label(&c)).unwrap_or_default()]),
                            TargetState::OutcomeUnknown => tx("结果未知，不会自动重试").to_string(),
                            TargetState::Queued => tx("排队中").to_string(),
                            TargetState::Running => tx("整理中").to_string(),
                        };
                        let handle = row.handle(&page.batch_id);
                        list = list.child(h_flex().gap_3().items_center().py_1().border_b_1().border_color(theme.border)
                            .child(div().w(px(24.)).text_xs().text_color(theme.muted_foreground).child((page.offset + i + 1).to_string()))
                            .child(div().flex_1().min_w_0().truncate().child(self.target_label(&row.item_id, &row.target)))
                            .child(div().flex_none().text_xs().text_color(if matches!(row.state, TargetState::Failed | TargetState::OutcomeUnknown) { theme.warning } else { theme.muted_foreground }).child(status))
                            .when_some(handle, |d, h| d.child(self.restore_controls(("format-row", page.offset + i), &h, cx))));
                    }
                    let (offset, total) = (page.offset, page.total);
                    let pager = h_flex().gap_2().items_center().pt_1().text_xs()
                        .child(muted(txf("第 {0}–{1} 条，共 {2} 条", &[&(offset + 1).min(total), &(offset + page.results.len()), &total])))
                        .child(Button::new("format-page-prev").xsmall().ghost().label(tx("上一页")).disabled(offset == 0)
                            .on_click(cx.listener(move |r, _, _, cx| r.fmt_read_results(true, offset.saturating_sub(RESULTS_PAGE), cx))))
                        .child(Button::new("format-page-next").xsmall().ghost().label(tx("下一页")).disabled(page.next_offset.is_none())
                            .on_click(cx.listener(move |r, _, _, cx| r.fmt_read_results(true, offset + RESULTS_PAGE, cx))));
                    body = body.child(list).child(pager);
                }
            }
        }
        Some(v_flex().id("format-batch-panel").flex_1().min_h_0().overflow_y_scroll().child(head).child(body).into_any_element())
    }

    /// The proof list under the comparison (查看修改).
    pub(super) fn format_review_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let r = self.fmt.review.as_ref()?;
        let theme = cx.theme().clone();
        let muted = |t: String| div().text_xs().text_color(theme.muted_foreground).child(t);
        let mut col = v_flex().id("format-review").flex_none().max_h(px(260.)).overflow_y_scroll().gap_1().pt_2().border_t_1().border_color(theme.border).text_sm();
        let title = match &r.page {
            Some(p) => txf("修改明细（{0} 处）", &[&p.total]),
            None => tx("修改明细").to_string(),
        };
        col = col.child(h_flex().gap_2().items_center().child(div().font_weight(FontWeight::SEMIBOLD).child(title)).child(muted(tx("每处修改都有记录，不需要再次调用模型。").into()))
            .when(self.compare.is_none(), |d| d.child(Button::new("format-review-close").xsmall().ghost().label(tx("关闭")).on_click(cx.listener(|r, _, _, cx| { r.fmt.review = None; cx.notify(); })))));
        if let Some(e) = &r.error { return Some(col.child(muted(e.clone())).into_any_element()); }
        let Some(page) = &r.page else { return Some(col.child(muted(tx("正在读取修改明细…").into())).into_any_element()) };
        let quote = |t: &str, total: usize| {
            // Whitespace edits stay visible: ⏎ newline, ⇥ tab, ␣ space when nothing else is there.
            let mut shown = t.replace('\n', "⏎").replace('\t', "⇥");
            if t.trim().is_empty() { shown = shown.replace(' ', "␣"); }
            if total > t.len() { format!("{shown}…") } else { shown }
        };
        for c in &page.changes {
            let (before, bt, after, at) = r.texts.get(&c.id).cloned().unwrap_or_default();
            let basis = if c.basis == "source_verified" { tx("有来源依据") } else { tx("按上下文判断") };
            col = col.child(v_flex().gap(px(2.)).py_1().border_b_1().border_color(theme.border.opacity(0.6))
                .child(muted(format!("{} · {} · {} · {}", kind_label(&c.kind), change_reason_label(&c.reason_code), basis, txf("删除 {0} 字节，新增 {1} 字节", &[&c.before_bytes, &c.after_bytes]))))
                .when(c.before_bytes > 0, |d| d.child(div().text_xs().child(format!("− {}", quote(&before, bt)))))
                .when(c.after_bytes > 0, |d| d.child(div().text_xs().child(format!("+ {}", quote(&after, at))))));
        }
        if page.next_offset.is_some() { col = col.child(muted(txf("另有 {0} 处见上方对比", &[&(page.total - page.changes.len())]))); }
        Some(col.into_any_element())
    }

    /// Walks: one line describing the formatting state.
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_format(&self) -> String {
        let one = |f: &Flow| {
            let state = match &f.state {
                FlowState::Confirm { quote, consent } => format!("Confirm(eligible={} skipped={} private={} reasons={:?} max_requests={} consent={consent})", quote.counts.eligible, quote.counts.skipped, quote.counts.private_targets, quote.counts.skipped_by_reason, quote.budget.max_requests),
                s => format!("{s:?}"),
            };
            let batch = f.batch.as_ref().map(|b| format!("{}:{}:{:?}:dispatched={}", &b.batch_id[b.batch_id.len().saturating_sub(8)..], b.state, b.counts, b.usage.requests_dispatched));
            let rows = f.results.as_ref().map(|p| p.results.iter().map(|r| format!("{:?}{}", r.state, r.reason.as_deref().map(|x| format!("({x})")).unwrap_or_default())).collect::<Vec<_>>().join(","));
            format!("key={} state={state} batch={batch:?} rows={rows:?} verified={}", f.key, f.verified.len())
        };
        format!("previews={} requests={} single[{}] batch[{}] review={:?} restores={:?}", self.fmt.previews, self.fmt.requests, one(&self.fmt.single),
            self.fmt.batch.as_ref().map(one).unwrap_or_default(), self.fmt.review.as_ref().map(|r| (r.page.as_ref().map(|p| p.total), r.error.clone())),
            self.fmt.restores.values().map(|s| match s { RestoreState::Done(r) => format!("Done(changed={})", r.changed), s => format!("{s:?}") }).collect::<Vec<_>>())
    }

    /// Walks: the known handles of the batch the flow follows (or followed).
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_format_handles(&self, group: bool) -> Vec<TargetHandle> {
        let Some(f) = self.flow(group) else { return vec![] };
        let batch = f.batch.as_ref().map(|b| b.batch_id.clone()).or_else(|| self.fmt_pending(&f.key).and_then(|p| p.batch_id));
        batch.map(|b| self.fmt_handles(&b)).unwrap_or_default()
    }

    /// Walks: the last 退回旧版 request (target, request id).
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_format_last_restore(&self) -> Option<(TargetHandle, String)> { self.fmt.last_restore.clone() }

    /// Walks: the stored request of a flow (request id, quote id, batch id).
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_format_pending(&self, group: bool) -> Option<(String, String, Option<String>)> {
        self.flow(group).and_then(|f| self.fmt_pending(&f.key)).map(|p| (p.request_id, p.quote_id, p.batch_id))
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::{after_error, change_reason_label, kind_label, reason_label, AfterError, Pending, DENIED, UNKNOWN};
    use memex_client::ClientError;

    #[test]
    fn stored_request_round_trips_and_rejects_partial_records() {
        let p = Pending { request_id: "mac-fmt-1".into(), quote_id: "q1".into(), batch_id: None, revision_id: Some("rev_a".into()) };
        assert_eq!(Pending::from_json(&p.to_json()), Some(p.clone()));
        let with_batch = Pending { batch_id: Some("batch_1".into()), ..p };
        assert_eq!(Pending::from_json(&with_batch.to_json()), Some(with_batch));
        // Without the request id or quote nothing could be replayed exactly: not a stored request.
        assert_eq!(Pending::from_json(r#"{"quote_id":"q1"}"#), None);
        assert_eq!(Pending::from_json(r#"{"request_id":"r"}"#), None);
        assert_eq!(Pending::from_json("not json"), None);
    }

    #[test]
    fn errors_never_lead_to_a_retry() {
        memex_client::i18n::set_language(memex_client::i18n::Lang::Zh);
        // Transport loss after sending: unknown, the id stays for 查询原任务.
        for e in [ClientError::Timeout, ClientError::Unavailable, ClientError::InvalidFrame, ClientError::Daemon("provider_outcome_unknown".into())] {
            assert_eq!(after_error(&e, true), AfterError::Unknown, "{e:?}");
        }
        // Whole-batch refusal: denied (no partial aggregates), whatever was sent.
        for code in ["library_resource_not_found", "item_not_found", "permission_denied", "content_format_batch_not_found"] {
            assert_eq!(after_error(&ClientError::Daemon(code.into()), false), AfterError::Denied, "{code}");
            assert_eq!(after_error(&ClientError::Daemon(code.into()), true), AfterError::Denied, "{code}");
        }
        // A definitive refusal of a send drops the id (nothing admitted); of a read it keeps it.
        let stale = ClientError::Daemon("content_format_quote_stale".into());
        assert!(matches!(after_error(&stale, true), AfterError::Refused { forget_request: true, .. }));
        assert!(matches!(after_error(&stale, false), AfterError::Refused { forget_request: false, .. }));
        let AfterError::Refused { message, .. } = after_error(&stale, true) else { unreachable!() };
        assert_eq!(message, "确认已过期或设置已变化，请重新确认");
    }

    #[test]
    fn every_contract_code_has_a_label() {
        memex_client::i18n::set_language(memex_client::i18n::Lang::Zh);
        assert_eq!(reason_label("already_formatted"), "已整理过，无需再整理");
        for code in ["already_formatted", "chapter_required", "unsupported_body", "restricted", "budget_exceeded", "busy", "stale_revision", "scope_changed"] {
            assert!(!reason_label(code).contains('_'), "{code}");
        }
        for kind in ["whitespace", "paragraph_boundary", "garbage_delete", "typo", "extraction_garble", "punctuation"] {
            assert_ne!(kind_label(kind), kind);
        }
        for code in ["navigation", "advertisement", "saved_control", "account_widget", "paragraph_layout", "short_correction", "source_garble", "punctuation"] {
            assert_ne!(change_reason_label(code), code);
        }
        // Shared wording with the phone.
        assert_eq!(DENIED, "无法读取整批：其中有条目已不可访问");
        assert!(UNKNOWN.starts_with("结果未知：") && UNKNOWN.ends_with("可以查询原任务，不会自动重试。"));
    }
}
