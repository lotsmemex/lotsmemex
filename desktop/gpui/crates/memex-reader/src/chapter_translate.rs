//! 翻译本章: an AI translation of one English chapter that has no translation
//! (`reader_chapter_translation_v1`). Preview → explicit confirmation → durable
//! job → read the published projection. Paired chapters are never offered.
//! The result is labelled AI 译文, never shown as the human translation, and no
//! draft (composer, title, chapter edit) is touched: nothing here reloads the item.
use super::*;
use memex_client::book::{AiAlignment, JobState, JobUsage, TranslationMeta, TranslationQuote};
use memex_client::ClientError;

/// The chapter-translation line of the chapter on screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) enum TrState {
    /// Not offered: no capability, not a book, paired, not English, or not checked.
    #[default]
    Hidden,
    /// Reading a result already known in this session (no line shown).
    Checking,
    /// Eligible, no result yet: 翻译本章.
    Offer,
    /// Reading the route and budget for the confirmation.
    Previewing,
    Confirm(TranslationQuote),
    Running { done: usize, total: usize },
    /// The job just finished and its text is on screen.
    Done(Option<JobUsage>),
    /// An existing AI translation was read (no cost).
    Ready,
    /// The agent says this chapter cannot be translated now (no button).
    Unavailable(String),
    /// Ended without success: 再试一次 goes through preview and confirmation again.
    Failed { message: String, done: usize, total: usize, usage: Option<JobUsage> },
    /// Outcome unknown: only 查询原任务, which replays the same request id.
    Unknown { done: usize, total: usize, usage: Option<JobUsage> },
    /// The result exists but its text could not be read.
    ReadFailed(String),
    /// The agent stayed busy (`library_loading`) through the bounded read
    /// retries: the known result is kept; 重试 reads it again.
    Busy,
}

/// The recorded block correspondence of the AI text on screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) enum AiMap {
    /// Not read, or the agent has no `reader_chapter_translation_alignment_v1`
    /// (the AI text is shown as before, without paragraph controls).
    #[default]
    None,
    Loading,
    /// A legacy translation without a recorded map: readable, never synced.
    Missing,
    /// Rows in block indexes of the original and the AI text, all matched.
    Aligned(Vec<crate::model::AlignedRow>),
    /// Unreadable or inconsistent with the texts: not used.
    Invalid(String),
}

#[derive(Default)]
pub(super) struct ChapterTr {
    pub supported: bool,
    /// `translate_metadata`: an eligible chapter on screen is looked up on every
    /// open / reopen / refresh (no preview, quote or credential read).
    pub metadata: bool,
    /// `translate_alignment`: the AI text's recorded block correspondence.
    pub alignment: bool,
    pub map: AiMap,
    /// 段落 rows of an AI translation (a list of its own).
    pub list_ai: Option<ListState>,
    /// Stale reads of the current key refreshed so far (bounded).
    pub stale: u8,
    /// Rows per `translate_alignment` page (0: the largest allowed).
    pub page: usize,
    /// The Busy line came from an asked preview: 重试 previews again (once, by the user).
    pub busy_preview: bool,
    /// Walks and tests: `translate_metadata` reads made by this reader.
    pub discoveries: usize,
    /// item|revision|chapter the state belongs to.
    pub key: Option<String>,
    pub state: TrState,
    pub ticket: u64,
    /// The published AI text of `key`: metadata, Markdown and its blocks.
    pub text: Option<(TranslationMeta, String, Vec<Block>)>,
    /// Published results seen this session, by key: reopening reads them with
    /// `translate_chunk` (no quote, no credential read), never with a preview.
    pub known: std::collections::HashMap<String, TranslationMeta>,
    /// Walks and tests: `translate_preview` calls made by this reader.
    pub previews: usize,
    /// Walks: the last request sent (request id, quote id, job id).
    #[cfg(feature = "dev-snapshot")]
    pub last: Option<(String, String, String)>,
}

/// Where a pending request is kept among the repair ids: `request_id quote_id [job_id]`.
fn store_key(key: &str) -> String {
    format!("translate|{key}")
}

const UNKNOWN: &str = "结果未知：不确定翻译请求是否完成，没有显示不完整的译文。可以查询原任务，不会自动重试。";

/// Outcome of a send we cannot know (the job may exist).
/// `library_loading` is not a definite rejection in the reader contract, so a
/// request that meets it keeps its id like a lost reply.
fn unknown_error(e: &ClientError) -> bool {
    matches!(e, ClientError::Timeout | ClientError::Unavailable | ClientError::InvalidFrame | ClientError::TooLarge) || e.is_busy()
}

/// 「调用 1 次 · 共 17 tokens · 输入 11 · 输出 3」: totals only when every request
/// reported them, input/output only when the agent knows them; never derived.
pub fn usage_label(u: &JobUsage) -> String {
    if u.requests_dispatched == 0 { return tx("未调用模型").into(); }
    let mut parts = vec![txf("调用 {0} 次", &[&u.requests_dispatched])];
    match u.known_total() {
        Some(total) => parts.push(txf("共 {0} tokens", &[&total])),
        None => parts.push(tx("用量未知").into()),
    }
    if let Some(i) = u.input_tokens { parts.push(txf("输入 {0}", &[&i])); }
    if let Some(o) = u.output_tokens { parts.push(txf("输出 {0}", &[&o])); }
    parts.join(" · ")
}

/// What the fixed 翻译 button does for the chapter on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterAction {
    Ask,
    Reread,
    RetryBusy,
}

impl Reader {
    /// The 翻译 button of a book chapter: its label and, when enabled, its action.
    /// Read from the chapter state; nothing is sent until it is clicked.
    pub fn chapter_tr_button(&self) -> (String, Option<ChapterAction>) {
        let off = |s: &'static str| (tx(s).to_string(), None);
        let Some(book) = self.model.book() else { return off("翻译本章") };
        match book.keys(book.at) {
            (Some(_), Some(_)) => return off("这一章已有译本"),
            (None, _) => return off("这一章只有译文"),
            (Some(_), None) if !self.chapter_tr.supported => return off("当前代理不支持翻译本章"),
            (Some(key), None) if !book.doc.translation_eligible(&key) => return off("这一章不需要翻译"),
            _ => {}
        }
        if self.chapter_loading { return off("正在载入…"); }
        match &self.chapter_tr.state {
            TrState::Offer => (tx("翻译本章").into(), Some(ChapterAction::Ask)),
            TrState::Failed { .. } => (tx("再试一次").into(), Some(ChapterAction::Ask)),
            TrState::Hidden | TrState::Checking | TrState::Previewing => off("正在读取…"),
            TrState::Confirm(_) => off("翻译本章"),
            TrState::Running { done, total } if *total > 0 => (txf("翻译中 {0}", &[&format!("{done}/{total}")]), None),
            TrState::Running { .. } => off("翻译中"),
            TrState::Done(_) | TrState::Ready => off("已翻译"),
            TrState::Unavailable(m) => (m.clone(), None),
            TrState::Unknown { .. } => off("结果未知"),
            TrState::ReadFailed(_) => (tx("重新读取").into(), Some(ChapterAction::Reread)),
            TrState::Busy => (tx("重试").into(), Some(ChapterAction::RetryBusy)),
        }
    }

    pub fn press_chapter_tr(&mut self, action: ChapterAction, cx: &mut Context<Self>) {
        match action {
            ChapterAction::Ask => self.ask_chapter_translation(cx),
            ChapterAction::Reread => self.reread_chapter_translation(cx),
            ChapterAction::RetryBusy => self.retry_busy_chapter_translation(cx),
        }
    }

    /// The original chapter on screen that may get an AI translation.
    fn tr_target(&self) -> Option<(memex_client::book::ReaderDocument, String, String)> {
        let book = self.model.book()?;
        if !self.chapter_tr.supported || self.chapter_loading { return None; }
        let (Some(key), None) = book.keys(book.at) else { return None };
        if !book.doc.translation_eligible(&key) { return None; }
        let id = format!("{}|{}|{}", book.doc.item_id, book.doc.revision_id, key);
        Some((book.doc.clone(), key, id))
    }

    fn tr_pending(&self, id: &str) -> Option<(String, String, Option<String>)> {
        let v = self.repair_ids.get(&store_key(id))?;
        let mut parts = v.split(' ');
        Some((parts.next()?.to_string(), parts.next()?.to_string(), parts.next().map(str::to_string)))
    }

    fn tr_set_pending(&mut self, id: &str, pending: Option<(&str, &str, Option<&str>)>) {
        match pending {
            Some((r, q, j)) => { self.repair_ids.insert(store_key(id), [Some(r), Some(q), j].into_iter().flatten().collect::<Vec<_>>().join(" ")); }
            None => { self.repair_ids.remove(&store_key(id)); }
        }
        self.save_repair_ids();
    }

    /// After a chapter (or the capability) arrived: what the line shows for it.
    /// Reading an existing result costs nothing; a job left running is followed
    /// by its job id (a read). Without a job id the outcome stays unknown until
    /// 查询原任务 — nothing is resent automatically.
    pub(super) fn refresh_chapter_translation(&mut self, cx: &mut Context<Self>) {
        // 格式整理 follows the same item / chapter / side changes.
        self.refresh_format(cx);
        let Some((doc, chapter, id)) = self.tr_target() else {
            if self.chapter_tr.key.is_some() || self.chapter_tr.state != TrState::Hidden {
                self.chapter_tr.ticket += 1;
                self.chapter_tr.key = None;
                self.chapter_tr.text = None;
                self.chapter_tr.state = TrState::Hidden;
            }
            return;
        };
        if self.chapter_tr.key.as_deref() == Some(&id) { return; }
        if self.chapter_tr.key.as_deref().is_none_or(|k| k.rsplit('|').next() != id.rsplit('|').next()) { self.chapter_tr.stale = 0; }
        self.chapter_tr.ticket += 1;
        self.chapter_tr.key = Some(id.clone());
        self.chapter_tr.text = None;
        self.chapter_tr.map = AiMap::None;
        if let Some((request_id, quote_id, job)) = self.tr_pending(&id) {
            match job {
                Some(job_id) => self.follow_chapter_translation(doc, chapter, id, request_id, quote_id, Some(job_id), cx),
                None => self.chapter_tr.state = TrState::Unknown { done: 0, total: 0, usage: None },
            }
            return;
        }
        // Discovery: a completed translation is read and shown, from any device
        // and without a remembered version. A miss is not remembered.
        if self.chapter_tr.metadata {
            self.chapter_tr.state = TrState::Checking;
            self.discover_chapter_translation(doc, chapter, id, cx);
            return;
        }
        // Older agents: no preview on open: it reads the credential status and issues a quote.
        // The contract has no quote-free "does a result exist" read, so an
        // eligible chapter offers 翻译本章; a click previews, and an existing
        // result is then shown without cost. Results seen this session are read.
        // A result seen at an earlier revision of the same item and chapter (a
        // title edit makes a metadata-only revision) is looked up again: the
        // agent serves an inherited completed translation as a zero-dispatch
        // cache hit; if nothing is inherited this only ends at 翻译本章.
        let earlier = self.chapter_tr.known.values().any(|m| m.item_id == doc.item_id && m.original_chapter == chapter && m.revision_id != doc.revision_id);
        match self.chapter_tr.known.get(&id).cloned() {
            Some(meta) => { self.chapter_tr.state = TrState::Checking; self.read_known_translation(meta, id, cx); }
            None if earlier => { self.chapter_tr.state = TrState::Checking; self.preview_chapter_translation(doc, chapter, id, false, cx); }
            None => self.chapter_tr.state = TrState::Offer,
        }
    }

    /// 翻译本章 / 再试一次: a fresh preview (route and budget), then the confirmation.
    pub fn ask_chapter_translation(&mut self, cx: &mut Context<Self>) {
        let Some((doc, chapter, id)) = self.tr_target() else { return };
        if matches!(self.chapter_tr.state, TrState::Running { .. } | TrState::Unknown { .. } | TrState::Previewing) { return; }
        self.chapter_tr.ticket += 1;
        self.chapter_tr.key = Some(id.clone());
        self.chapter_tr.state = TrState::Previewing;
        cx.notify();
        self.preview_chapter_translation(doc, chapter, id, true, cx);
    }

    pub fn cancel_chapter_translation(&mut self, cx: &mut Context<Self>) {
        if matches!(self.chapter_tr.state, TrState::Confirm(_)) { self.chapter_tr.state = TrState::Offer; }
        cx.notify();
    }

    /// `translate_metadata`, then the text at the returned version. A version race
    /// is read again (bounded); errors are shown as errors, never as "none".
    fn discover_chapter_translation(&mut self, doc: memex_client::book::ReaderDocument, chapter: String, id: String, cx: &mut Context<Self>) {
        self.chapter_tr.discoveries += 1;
        let ticket = self.chapter_tr.ticket;
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move {
            let mut last = ClientError::Daemon("stale_reader_translation".into());
            for _ in 0..3 {
                let Some(meta) = client.reader_translate_metadata(&doc, &chapter)? else { return Ok(None) };
                match client.reader_translation_text(&meta) {
                    Ok(text) => return Ok(Some((meta, text))),
                    Err(e @ ClientError::Daemon(_)) if matches!(&e, ClientError::Daemon(c) if c == "stale_reader_translation") => last = e,
                    Err(e) => return Err(e),
                }
            }
            Err(last)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.chapter_tr.ticket != ticket || this.chapter_tr.key.as_deref() != Some(&id) { return; }
                this.chapter_tr.state = match result {
                    Ok(Some((meta, text))) => { this.set_tr_text(meta, text, cx); TrState::Ready }
                    // None published now: the action (a click previews; it may still find one).
                    Ok(None) | Err(ClientError::Unsupported) => TrState::Offer,
                    Err(e) if e.is_busy() => TrState::Busy,
                    // The item moved on: read it again (bounded), keeping drafts and the chapter.
                    Err(ClientError::Daemon(c)) if c == "stale_item_revision" && this.chapter_tr.stale < 2 && !this.blocks_navigation(cx) => {
                        this.chapter_tr.stale += 1;
                        this.reload(cx);
                        TrState::Checking
                    }
                    Err(e) => TrState::ReadFailed(txf("AI 译文没有读取成功：{0}", &[&e.message()])),
                };
                cx.notify();
            }).ok();
        }).detach();
    }

    /// The recorded correspondence of the AI text just shown (reads only). A
    /// version race refreshes the whole set — metadata, text and map — bounded.
    fn load_ai_map(&mut self, meta: TranslationMeta, cx: &mut Context<Self>) {
        let Some(id) = self.chapter_tr.key.clone() else { return };
        if !self.chapter_tr.alignment { self.chapter_tr.map = AiMap::None; return; }
        let (Some(original), Some(text)) = (self.model.book().and_then(|b| b.original.clone()), self.chapter_tr.text.as_ref().map(|t| t.1.clone())) else { return };
        self.chapter_tr.map = AiMap::Loading;
        let ticket = self.chapter_tr.ticket;
        let client = self.client.clone();
        let page = if self.chapter_tr.page == 0 { memex_client::book::AI_ALIGNMENT_PAGE } else { self.chapter_tr.page };
        let task = cx.background_executor().spawn(async move {
            // Checked against both complete texts: every byte once, in order.
            match client.reader_translate_alignment(&meta, page)? {
                AiAlignment::Aligned(a) => a.validate(&original, Some(&text)).map(|_| AiAlignment::Aligned(a)),
                missing => Ok(missing),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.chapter_tr.ticket != ticket || this.chapter_tr.key.as_deref() != Some(&id) { return; }
                this.chapter_tr.map = match result {
                    Ok(AiAlignment::Missing) => AiMap::Missing,
                    Ok(AiAlignment::Aligned(a)) => match &this.chapter_tr.text {
                        Some((_, _, blocks)) => AiMap::Aligned(crate::model::aligned_rows(&a, this.model.original(), blocks)),
                        None => AiMap::None,
                    },
                    Err(ClientError::Unsupported) => AiMap::None,
                    Err(ClientError::Daemon(c)) if matches!(c.as_str(), "stale_reader_translation" | "stale_item_revision") && this.chapter_tr.stale < 2 => {
                        this.chapter_tr.stale += 1;
                        this.chapter_tr.key = None;
                        this.refresh_chapter_translation(cx);
                        return;
                    }
                    Err(e) => AiMap::Invalid(e.message()),
                };
                if let AiMap::Aligned(rows) = &this.chapter_tr.map { if let Some(l) = &this.chapter_tr.list_ai { l.reset(rows.len()); } }
                cx.notify();
            }).ok();
        }).detach();
    }

    /// Reads a published result whose metadata is known (chunk reads only).
    fn read_known_translation(&mut self, meta: TranslationMeta, id: String, cx: &mut Context<Self>) {
        let ticket = self.chapter_tr.ticket;
        let client = self.client.clone();
        let task = cx.background_executor().spawn({ let meta = meta.clone(); async move { client.reader_translation_text(&meta) } });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.chapter_tr.ticket != ticket || this.chapter_tr.key.as_deref() != Some(&id) { return; }
                this.chapter_tr.state = match result {
                    Ok(t) => { this.set_tr_text(meta, t, cx); TrState::Ready }
                    // A stale version: look it up again under the current one (no cost).
                    Err(ClientError::Daemon(c)) if matches!(c.as_str(), "stale_reader_translation" | "stale_item_revision") => {
                        this.chapter_tr.known.remove(&id);
                        if let Some((doc, chapter, key)) = this.tr_target().filter(|t| t.2 == id) { this.preview_chapter_translation(doc, chapter, key, false, cx); }
                        TrState::Checking
                    }
                    // Busy, not bad: the known version stays for 重试.
                    Err(e) if e.is_busy() => TrState::Busy,
                    // Changed or gone: forget it and offer the action (its preview decides).
                    Err(_) => { this.chapter_tr.known.remove(&id); TrState::Offer }
                };
                cx.notify();
            }).ok();
        }).detach();
    }

    fn preview_chapter_translation(&mut self, doc: memex_client::book::ReaderDocument, chapter: String, id: String, asked: bool, cx: &mut Context<Self>) {
        self.chapter_tr.previews += 1;
        let ticket = self.chapter_tr.ticket;
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move {
            let preview = client.reader_translate_preview(&doc, &chapter)?;
            let text = match &preview.translation {
                Some(meta) => Some(client.reader_translation_text(meta).map(|t| (meta.clone(), t))),
                None => None,
            };
            Ok::<_, ClientError>((preview, text))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.chapter_tr.ticket != ticket || this.chapter_tr.key.as_deref() != Some(&id) { return; }
                this.chapter_tr.state = match result {
                    Ok((_, Some(Ok((meta, text))))) => { this.set_tr_text(meta, text, cx); TrState::Ready }
                    Ok((_, Some(Err(e)))) => TrState::ReadFailed(txf("AI 译文已生成，但没有读取成功：{0}", &[&e.message()])),
                    Ok((p, None)) if p.eligible => match p.quote {
                        Some(q) if asked => TrState::Confirm(q),
                        _ => TrState::Offer,
                    },
                    Ok((p, None)) => match p.reason.as_deref() {
                        Some("reader_translation_not_needed" | "reader_translation_language_unsupported") | None => TrState::Hidden,
                        Some(code) => TrState::Unavailable(ClientError::Daemon(code.into()).message()),
                    },
                    Err(ClientError::Unsupported) => TrState::Hidden,
                    // Never resent by itself (a preview may write a quote): 重试 is the user's.
                    Err(e) if e.is_busy() => { this.chapter_tr.busy_preview = asked; TrState::Busy }
                    // The background check failing still leaves the button (it previews again).
                    Err(_) if !asked => TrState::Offer,
                    Err(e) => TrState::Failed { message: e.message(), done: 0, total: 0, usage: None },
                };
                cx.notify();
            }).ok();
        }).detach();
    }

    fn set_tr_text(&mut self, meta: TranslationMeta, text: String, cx: &mut Context<Self>) {
        if let Some(key) = self.chapter_tr.key.clone() { self.chapter_tr.known.insert(key, meta.clone()); }
        let blocks = crate::model::blocks(&text);
        self.list_t.reset(blocks.len());
        self.list_o.reset(self.model.original().len());
        self.chapter_tr.text = Some((meta.clone(), text, blocks));
        self.load_ai_map(meta, cx);
        // The AI marks of this book (read again with each published text).
        self.ai_load(cx);
    }

    /// Recorded rows usable for 段落 / 并排 sync of the AI text on screen.
    pub(super) fn ai_rows(&self) -> Option<&[crate::model::AlignedRow]> {
        self.tr_blocks_ready()?;
        match &self.chapter_tr.map { AiMap::Aligned(rows) if !rows.is_empty() => Some(rows), _ => None }
    }

    fn tr_blocks_ready(&self) -> Option<()> {
        let (_, _, id) = self.tr_target()?;
        (self.chapter_tr.key.as_deref() == Some(&id) && self.chapter_tr.text.is_some()).then_some(())
    }

    /// 确认翻译: the request id and quote are stored before anything is sent.
    pub fn confirm_chapter_translation(&mut self, cx: &mut Context<Self>) {
        let TrState::Confirm(quote) = self.chapter_tr.state.clone() else { return };
        let Some((doc, chapter, id)) = self.tr_target() else { return };
        if quote.chapter != chapter || quote.revision_id != doc.revision_id { return; }
        let request_id = format!("mac-tr-{}", memex_client::biblio::new_request_id());
        self.tr_set_pending(&id, Some((&request_id, &quote.quote_id, None)));
        self.follow_chapter_translation(doc, chapter, id, request_id, quote.quote_id, None, cx);
    }

    /// 查询原任务: replays the stored request (same id and quote); the agent
    /// returns the original job and never dispatches it twice.
    pub fn query_chapter_translation(&mut self, cx: &mut Context<Self>) {
        let Some((doc, chapter, id)) = self.tr_target() else { return };
        let Some((request_id, quote_id, _)) = self.tr_pending(&id) else { return };
        self.follow_chapter_translation(doc, chapter, id, request_id, quote_id, None, cx);
    }

    /// Sends (or replays) the request, or reads `job_id`, then follows the job.
    #[allow(clippy::too_many_arguments)]
    fn follow_chapter_translation(&mut self, doc: memex_client::book::ReaderDocument, chapter: String, id: String, request_id: String, quote_id: String, job_id: Option<String>, cx: &mut Context<Self>) {
        self.chapter_tr.ticket += 1;
        let ticket = self.chapter_tr.ticket;
        self.chapter_tr.state = TrState::Running { done: 0, total: 0 };
        cx.notify();
        let client = self.client.clone();
        let key = id.clone();
        let alive = move |this: &Reader| this.chapter_tr.ticket == ticket && this.chapter_tr.key.as_deref() == Some(key.as_str());
        cx.spawn(async move |this, cx| {
            let first = cx.background_executor().spawn({
                let (client, doc, chapter, request_id, quote_id, item) = (client.clone(), doc.clone(), chapter.clone(), request_id.clone(), quote_id.clone(), doc.item_id.clone());
                async move {
                    match job_id {
                        Some(job) => client.reader_translate_status(&item, &job),
                        None => client.reader_translate_request(&doc, &chapter, &request_id, &quote_id),
                    }
                }
            }).await;
            let mut job = match first {
                Ok(job) => job,
                Err(e) => {
                    this.update(cx, |this, cx| {
                        if !alive(this) { return; }
                        if unknown_error(&e) {
                            this.chapter_tr.state = TrState::Unknown { done: 0, total: 0, usage: None };
                        } else {
                            // Definitive: nothing was admitted; a new attempt needs a new confirmation.
                            this.tr_set_pending(&id, None);
                            this.chapter_tr.state = TrState::Failed { message: e.message(), done: 0, total: 0, usage: None };
                        }
                        cx.notify();
                    }).ok();
                    return;
                }
            };
            #[cfg(feature = "dev-snapshot")]
            this.update(cx, |this, _| this.chapter_tr.last = Some((request_id.clone(), quote_id.clone(), job.job_id.clone()))).ok();
            // Remember the job: leaving and returning reads it instead of resending.
            this.update(cx, |this, _| if this.tr_pending(&id).is_some() { this.tr_set_pending(&id, Some((&request_id, &quote_id, Some(&job.job_id)))); }).ok();
            loop {
                let state = job.state;
                let go = this.update(cx, |this, cx| {
                    if !alive(this) { return false; }
                    if !state.is_terminal() { this.chapter_tr.state = TrState::Running { done: job.completed_blocks, total: job.total_blocks }; cx.notify(); }
                    true
                }).unwrap_or(false);
                if !go { return; }
                if state.is_terminal() { break; }
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                let (item, jid) = (job.item_id.clone(), job.job_id.clone());
                match cx.background_executor().spawn({ let client = client.clone(); async move { client.reader_translate_status(&item, &jid) } }).await {
                    Ok(next) => job = next,
                    Err(e) => {
                        this.update(cx, |this, cx| {
                            if !alive(this) { return; }
                            // The job exists; its outcome is just not known here.
                            this.chapter_tr.state = if unknown_error(&e) { TrState::Unknown { done: job.completed_blocks, total: job.total_blocks, usage: job.usage.clone() } }
                                else { TrState::Failed { message: e.message(), done: job.completed_blocks, total: job.total_blocks, usage: job.usage.clone() } };
                            cx.notify();
                        }).ok();
                        return;
                    }
                }
            }
            // Only a fully published chapter is shown; read it (reads dispatch nothing).
            let text = match (&job.state, &job.translation) {
                (JobState::Succeeded, Some(meta)) => {
                    let (client, meta) = (client.clone(), meta.clone());
                    Some(cx.background_executor().spawn(async move { client.reader_translation_text(&meta).map(|t| (meta, t)) }).await)
                }
                _ => None,
            };
            this.update(cx, |this, cx| {
                if !alive(this) { return; }
                let (done, total, usage) = (job.completed_blocks, job.total_blocks, job.usage.clone());
                this.chapter_tr.state = match (job.state, text) {
                    (JobState::Succeeded, Some(Ok((meta, t)))) => { this.tr_set_pending(&id, None); this.set_tr_text(meta, t, cx); TrState::Done(usage) }
                    // Busy: remember the published version so 重试 reads it (no new request).
                    (JobState::Succeeded, Some(Err(e))) if e.is_busy() => {
                        this.tr_set_pending(&id, None);
                        if let Some(meta) = job.translation.clone() { this.chapter_tr.known.insert(id.clone(), meta); }
                        TrState::Busy
                    }
                    (JobState::Succeeded, Some(Err(e))) => { this.tr_set_pending(&id, None); TrState::ReadFailed(txf("AI 译文已生成，但没有读取成功：{0}", &[&e.message()])) }
                    // Succeeded without a published result is not a success to show; keep the id.
                    (JobState::Succeeded, None) | (JobState::OutcomeUnknown, _) => TrState::Unknown { done, total, usage },
                    (JobState::Failed, _) => {
                        this.tr_set_pending(&id, None);
                        let code = job.error_code.clone().unwrap_or_else(|| "reader_translation_unavailable".into());
                        TrState::Failed { message: ClientError::Daemon(code).message(), done, total, usage }
                    }
                    _ => TrState::Unknown { done, total, usage },
                };
                cx.notify();
            }).ok();
        }).detach();
    }

    /// The AI text of the chapter on screen, for selection copy.
    pub(super) fn tr_markdown(&self) -> &str {
        self.chapter_tr.text.as_ref().filter(|_| self.tr_target().is_some_and(|t| self.chapter_tr.key.as_deref() == Some(&t.2))).map_or("", |t| t.1.as_str())
    }

    fn tr_blocks(&self) -> Option<(TranslationMeta, Vec<Block>)> {
        let (_, _, id) = self.tr_target()?;
        if self.chapter_tr.key.as_deref() != Some(&id) { return None; }
        self.chapter_tr.text.as_ref().map(|(m, _, b)| (m.clone(), b.clone()))
    }

    /// 「AI 译文」: deliberately unlike the human 译文 label (a filled amber tag).
    fn ai_badge(dark: bool) -> Div {
        let (bg, fg) = if dark { (hsla(38. / 360., 0.55, 0.30, 1.), hsla(42. / 360., 0.90, 0.85, 1.)) } else { (hsla(42. / 360., 0.90, 0.88, 1.), hsla(30. / 360., 0.80, 0.28, 1.)) };
        div().flex_none().px(px(6.)).py(px(1.)).rounded(px(4.)).bg(bg).text_color(fg).text_xs().font_weight(FontWeight::SEMIBOLD).child(tx("AI 译文"))
    }

    fn ai_caption(meta: &TranslationMeta, theme: &gpui_kit::component::Theme) -> Div {
        h_flex().flex_none().flex_wrap().gap_2().items_center().child(Self::ai_badge(theme.background.l < 0.5))
            .child(div().text_xs().text_color(theme.muted_foreground).child(txf("由 {0} 生成，不是出版译本", &[&meta.route.model_id])))
    }

    /// 译文 / 对照 of a chapter with an AI translation: the generated text beside
    /// the original, never paired by paragraph (v1 has no correspondence).
    pub(super) fn ai_translation_content(&mut self, mode: Mode, palette: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !matches!(mode, Mode::Translation | Mode::Bilingual) { return None; }
        let (meta, blocks) = self.tr_blocks()?;
        if self.list_t.item_count() != blocks.len() { self.list_t.reset(blocks.len()); }
        if self.list_o.item_count() != self.model.original().len() { self.list_o.reset(self.model.original().len()); }
        let theme = cx.theme().clone();
        let reader = cx.entity().downgrade();
        let end = self.chapter_after();
        // The owner's marks on the AI text (its own ledger, never the original's).
        let ai_spans = Rc::new(self.ai_spans());
        let none: Rc<HashMap<usize, Vec<Span>>> = Rc::default();
        let column = |state: ListState, blocks: Vec<Block>, prefix: char, end: Option<Option<(usize, String)>>, spans: Rc<HashMap<usize, Vec<Span>>>| {
            let (p, reader, n) = (palette.clone(), reader.clone(), blocks.len());
            list(state, move |ix, _window, cx| match blocks.get(ix) {
                Some(b) => {
                    let el = block_element(b, &p, false, false, spans.get(&ix).map(Vec::as_slice).unwrap_or(&[]), format!("{prefix}{ix}"), &reader, cx);
                    match &end {
                        Some(next) if ix + 1 == n => v_flex().child(el).child(chapter_end(next.clone(), &p, &reader, "chapter-end-ai")).into_any_element(),
                        _ => el.into_any_element(),
                    }
                }
                None => div().into_any_element(),
            }).flex_1()
        };
        // 对照 only: what the recorded correspondence allows, said plainly.
        let note = (mode == Mode::Bilingual).then(|| match &self.chapter_tr.map {
            AiMap::Aligned(_) => Some(tx("按翻译分块对照，未经人工核对").to_string()),
            AiMap::Missing => Some(tx("这份 AI 译文没有保存逐段对照").to_string()),
            AiMap::Invalid(m) => Some(txf("逐段对照暂不可用：{0}", &[m])),
            AiMap::None | AiMap::Loading => None,
        }).flatten();
        let caption = Self::ai_caption(&meta, &theme).when_some(note, |d, n| d.child(div().text_xs().text_color(theme.muted_foreground).child(n)));
        let caption = v_flex().flex_none().gap_1().child(caption).children(self.ai_stale_line(cx));
        let rows = self.ai_rows().map(<[_]>::to_vec);
        if mode == Mode::Bilingual && !self.model.parallel() {
            if let Some(rows) = rows {
                // 段落: each recorded row, original above its AI blocks (never human-edition offsets).
                let n = rows.len();
                let state = match &self.chapter_tr.list_ai {
                    Some(l) if l.item_count() == n => l.clone(),
                    _ => { let l = ListState::new(n, ListAlignment::Top, px(800.)); self.chapter_tr.list_ai = Some(l.clone()); l }
                };
                let (p, reader, original, spans) = (palette.clone(), reader.clone(), self.model.original().to_vec(), ai_spans.clone());
                let lst = list(state, move |ix, _window, cx| match rows.get(ix) {
                    Some(r) => {
                        let el = aligned_row(r, &original, &blocks, 'a', &spans, &p, &reader, cx);
                        match &end {
                            Some(next) if ix + 1 == n => v_flex().child(el).child(chapter_end(next.clone(), &p, &reader, "chapter-end-ai")).into_any_element(),
                            _ => el,
                        }
                    }
                    None => div().into_any_element(),
                }).flex_1();
                return Some(v_flex().id("book-ai-pairs").flex_1().min_h_0().gap_1().child(caption).child(lst).into_any_element());
            }
        }
        // 并排: follows only at recorded matched rows; without them each side scrolls alone.
        if mode == Mode::Bilingual { if let Some(rows) = &rows { self.sync_columns_by(rows); } }
        let bubble = self.ai_speaker(&blocks).map(|(name, unavailable)| longread::speaker_bubble(name, unavailable, palette, &theme));
        let ai = v_flex().id("book-ai-translation").flex_1().h_full().min_w_0().min_h_0().gap_1()
            .child(caption)
            .child(div().relative().flex_1().min_h_0().flex().flex_col().child(column(self.list_t.clone(), blocks, 'a', end.clone(), ai_spans.clone())).children(bubble));
        Some(if mode == Mode::Translation {
            ai.into_any_element()
        } else {
            let language = self.model.book().map(|b| b.language("original")).unwrap_or_default();
            let language = if language.starts_with("en") { "English".to_string() } else { language };
            h_flex().flex_1().min_h_0().items_stretch().gap_6()
                .child(v_flex().id("book-original").flex_1().h_full().min_w_0().min_h_0().gap_1()
                    .child(div().flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(theme.primary).child(format!("{} · {}", tx("原文"), language)))
                    .child(column(self.list_o.clone(), self.model.original().to_vec(), 'o', None, none)))
                .child(div().flex_none().w(px(1.)).h_full().bg(theme.border))
                .child(ai)
                .into_any_element()
        })
    }

    /// The 翻译本章 line: the action, the confirmation and the job's progress.
    pub(super) fn translation_line(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.tr_target()?;
        let theme = cx.theme().clone();
        let mode = self.model.mode();
        let line = || h_flex().gap_2().items_center().flex_wrap().px_3().py_1p5().rounded(px(6.)).bg(theme.secondary).text_xs();
        let button = |id: &'static str, label: &'static str, cx: &mut Context<Self>, f: fn(&mut Self, &mut Context<Self>)| {
            Button::new(id).xsmall().outline().label(tx(label)).on_click(cx.listener(move |r, _, _, cx| f(r, cx)))
        };
        let view = |cx: &mut Context<Self>| Button::new("chapter-tr-view").xsmall().ghost().label(tx("查看 AI 译文"))
            .on_click(cx.listener(|r, _, _, cx| r.set_mode(Mode::Translation, cx)));
        let text = |t: String| div().child(t);
        let muted = |t: String| div().text_color(theme.muted_foreground).child(t);
        let partial = |done: usize, total: usize| (total > 0 && done < total).then(|| txf("已完成 {0}/{1} 段，未完成的译文不会显示", &[&done, &total]));
        let el = match &self.chapter_tr.state {
            TrState::Hidden | TrState::Checking => return None,
            TrState::Offer => line().child(muted(tx("这一章只有英文原文").into())).when(mode == Mode::Original, |d| d.child(button("chapter-tr-ask", "翻译本章", cx, Self::ask_chapter_translation))),
            TrState::Previewing => line().child(Icon::new(IconName::Loader).xsmall().text_color(theme.primary)).child(text(tx("正在读取翻译设置…").into())),
            TrState::Confirm(q) => {
                let effort = q.route.reasoning_effort.clone().unwrap_or_else(|| tx("默认").into());
                let b = &q.budget;
                let budget = txf("计划调用 {0} 次（上限 {1} 次）；每次输入至多 {2} KB，输出至多 {3} tokens", &[&q.planned_requests, &b.max_requests, &(b.max_input_bytes_per_request / 1024), &b.max_completion_tokens_per_request]);
                v_flex().gap_1p5().px_3().py_2().rounded(px(8.)).border_1().border_color(theme.border).bg(theme.secondary).text_xs()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(tx("翻译本章前请确认")))
                    .child(text(txf("模型 {0} · 推理强度 {1} · 服务 {2}", &[&q.route.model_id, &effort, &q.route.provider_id])))
                    .child(text(budget))
                    .when_some(q.estimated_cost.as_ref(), |d, c| d.child(text(txf("预估费用约 {0} {1}", &[&c.amount, &c.currency]))))
                    .child(muted(tx("只翻译这一章，结果单独保存为 AI 译文，不改动原书和人工译文。").into()))
                    .child(h_flex().gap_2().pt_1()
                        .child(Button::new("chapter-tr-confirm").xsmall().primary().label(tx("确认翻译")).on_click(cx.listener(|r, _, _, cx| r.confirm_chapter_translation(cx))))
                        .child(button("chapter-tr-cancel", "取消", cx, Self::cancel_chapter_translation)))
            }
            TrState::Running { done, total } => line().child(Icon::new(IconName::Loader).xsmall().text_color(theme.primary)).child(text(tx("正在翻译本章…").into()))
                .when(*total > 0, |d| d.child(muted(txf("{0}/{1} 段", &[done, total])))),
            TrState::Done(usage) => line().child(Icon::new(IconName::Check).xsmall().text_color(theme.primary)).child(text(tx("本章 AI 译文已就绪").into()))
                .when_some(usage.as_ref(), |d, u| d.child(muted(usage_label(u))))
                .when(mode == Mode::Original, |d| d.child(view(cx))),
            TrState::Ready if mode == Mode::Original => line().child(Self::ai_badge(theme.background.l < 0.5)).child(muted(tx("这一章有 AI 译文").into())).child(view(cx)),
            TrState::Ready => return None,
            TrState::Unavailable(m) => line().child(muted(m.clone())),
            // The outcome on the first row; reason, progress and usage below it.
            TrState::Failed { message, done, total, usage } => {
                let head = h_flex().gap_2().items_center().flex_wrap().child(Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning))
                    .child(text(tx("翻译没有成功").into()))
                    // 对照 / 译文 have the toolbar's 再试一次.
                    .when(mode == Mode::Original, |d| d.child(muted("·".into())).child(button("chapter-tr-again", "再试一次", cx, Self::ask_chapter_translation)));
                let details = [Some(message.clone()), partial(*done, *total), usage.as_ref().map(usage_label)].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                v_flex().gap_1().px_3().py_1p5().rounded(px(6.)).bg(theme.secondary).text_xs().child(head).child(muted(details))
            }
            TrState::Unknown { done, total, usage } => {
                let head = h_flex().gap_2().items_center().flex_wrap().child(Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning))
                    .child(text(tx(UNKNOWN).into()))
                    .child(button("chapter-tr-query", "查询原任务", cx, Self::query_chapter_translation));
                let details = [partial(*done, *total), usage.as_ref().map(usage_label)].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                v_flex().gap_1().px_3().py_1p5().rounded(px(6.)).bg(theme.secondary).text_xs().child(head).when(!details.is_empty(), |d| d.child(muted(details)))
            }
            TrState::ReadFailed(m) => line().child(Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning)).child(text(m.clone()))
                .when(mode == Mode::Original, |d| d.child(button("chapter-tr-reread", "重新读取", cx, Self::reread_chapter_translation))),
            TrState::Busy => line().child(Icon::new(IconName::Info).xsmall().text_color(theme.muted_foreground)).child(text(tx("Mac 正忙，稍后重试").into()))
                .when(mode == Mode::Original, |d| d.child(button("chapter-tr-busy-retry", "重试", cx, Self::retry_busy_chapter_translation))),
        };
        Some(el.into_any_element())
    }

    /// 重试 after 「Mac 正忙」: a busy preview the user asked for is asked again
    /// (one new preview); a busy read is read again.
    pub fn retry_busy_chapter_translation(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.chapter_tr.busy_preview) {
            self.chapter_tr.state = TrState::Offer;
            self.ask_chapter_translation(cx);
        } else {
            self.reread_chapter_translation(cx);
        }
    }

    /// After a failed read: checks again (an existing result is read without cost).
    pub fn reread_chapter_translation(&mut self, cx: &mut Context<Self>) {
        self.chapter_tr.key = None;
        self.refresh_chapter_translation(cx);
        cx.notify();
    }

    /// Walks: rows per `translate_alignment` page (1 exercises paging), read again.
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_set_ai_alignment_page(&mut self, rows: usize, cx: &mut Context<Self>) {
        self.chapter_tr.page = rows;
        self.reread_chapter_translation(cx);
    }

    /// Walks: the AI 段落 rows as (original blocks, AI blocks).
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_ai_rows(&self) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
        self.ai_rows().map(|r| r.iter().map(|r| (r.o.clone(), r.t.clone())).collect()).unwrap_or_default()
    }

    /// Walks: a one-line summary of the chapter-translation state.
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_chapter_translation(&self) -> String {
        let s = match &self.chapter_tr.state {
            TrState::Confirm(q) => format!("Confirm(model={} effort={:?} planned={} max={})", q.route.model_id, q.route.reasoning_effort, q.planned_requests, q.budget.max_requests),
            other => format!("{other:?}"),
        };
        let map = match &self.chapter_tr.map { AiMap::Aligned(rows) => format!("Aligned(rows={} matched={})", rows.len(), rows.iter().filter(|r| r.relation == memex_client::book::Relation::Matched).count()), m => format!("{m:?}") };
        format!("previews={} discoveries={} metadata={} alignment={} map={map} supported={} key={:?} state={} text_bytes={:?} pending={:?} last={:?}", self.chapter_tr.previews, self.chapter_tr.discoveries, self.chapter_tr.metadata, self.chapter_tr.alignment, self.chapter_tr.supported, self.chapter_tr.key, s,
            self.chapter_tr.text.as_ref().map(|t| t.1.len()), self.chapter_tr.key.as_deref().and_then(|k| self.repair_ids.get(&store_key(k))), self.chapter_tr.last)
    }

    #[cfg(feature = "dev-snapshot")]
    pub fn dev_chapter_translation_previews(&self) -> usize { self.chapter_tr.previews }
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_chapter_translation_last(&self) -> Option<(String, String, String)> { self.chapter_tr.last.clone() }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::usage_label;
    use memex_client::book::{DispatchUsage, JobUsage};

    #[test]
    fn usage_shows_only_what_is_known() {
        memex_client::i18n::set_language(memex_client::i18n::Lang::Zh);
        let success = JobUsage { requests_dispatched: 1, reported_total_tokens: 17, unknown_usage_requests: 0,
            dispatches: Some(vec![DispatchUsage { input_tokens: Some(11), output_tokens: Some(3), total_tokens: Some(17) }]), input_tokens: Some(11), output_tokens: Some(3) };
        assert_eq!(usage_label(&success), "调用 1 次 · 共 17 tokens · 输入 11 · 输出 3");
        // Known total, unknown split: no input/output is derived from the total.
        let split = JobUsage { input_tokens: None, output_tokens: None, ..success.clone() };
        assert_eq!(usage_label(&split), "调用 1 次 · 共 17 tokens");
        // An unknown dispatch: the total is not shown as if complete.
        let unknown = JobUsage { requests_dispatched: 1, reported_total_tokens: 0, unknown_usage_requests: 1,
            dispatches: Some(vec![DispatchUsage { input_tokens: None, output_tokens: None, total_tokens: None }]), input_tokens: None, output_tokens: None };
        assert_eq!(usage_label(&unknown), "调用 1 次 · 用量未知");
        assert_eq!(usage_label(&JobUsage::default()), "未调用模型");
    }
}
