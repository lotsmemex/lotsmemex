//! The 翻译此文 action and the AI 纪要: capability gate, `translation_request`,
//! then polling the item (`library_item_read`) with backoff until both the
//! translation and the minutes end. With `minutes_request_v1` the minutes are
//! manual: only the 纪要 button (`minutes_request`) starts them, and a
//! translation request never does. An older agent makes them in the translation
//! request, so a Chinese source's button reads 生成纪要 there. Pure state; the
//! reader view sends the requests. Nothing is reported as done unless the agent
//! says `succeeded`.

use crate::insights::ButtonState;
use memex_client::{failure_detail_label, minutes_reason_label, translating_label, translation_reason_label, tx, txf, unfinished_label, AiMinutes, AnalysisState, ClientError, MinutesState, TranslationItem, TranslationProgress, TranslationStatus};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Phase {
    /// Nothing asked in this session: the item's read state applies.
    #[default]
    Idle,
    Requesting,
    /// Queued or running; poll again.
    Waiting { running: bool, attempt: u32 },
    Succeeded,
    Failed(Option<String>),
    /// The source is Chinese.
    NotNeeded,
    /// No translation model is configured.
    NoProvider,
    Error(String),
}

/// The 翻译 button fixed in the 对照 / 译文 toolbar of an ordinary item,
/// derived from the same state as the status chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrButton {
    /// 翻译此文 / 重试: sends one `translation_request`.
    Request(&'static str),
    /// The state could not be read: 重试 reads it first, and requests only if
    /// it is still not running or finished.
    Recheck(String),
    /// Queued or running (翻译中 n/m), disabled.
    Running(String),
    /// Translated, disabled (the agent has no forced retranslation).
    Done,
    /// No model configured: the way to settings.
    Settings,
    /// Disabled with the reason (Chinese source, skipped, unsupported).
    Reason(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    Poll(Duration),
    /// The translation is ready: read the item again.
    Reload,
    Stop,
}

/// Consecutive failed polls tolerated before giving up.
const POLL_ERRORS: u32 = 5;

/// While a translation is running: one read every 3 s (its progress and time).
pub const RUNNING_POLL: Duration = Duration::from_secs(3);

/// 0.5 s, growing by 1.6× per attempt, at most 4 s.
pub fn backoff(attempt: u32) -> Duration {
    let ms = 500.0 * 1.6f64.powi(attempt.min(10) as i32);
    Duration::from_millis(ms.min(4000.0) as u64)
}

/// Whether text reads as Chinese (as the agent's language gate would, before it
/// has said so): Han characters outweigh Latin letters a third as numerous.
pub fn looks_chinese(title: &str, markdown: &str) -> bool {
    let (mut han, mut latin) = (0usize, 0usize);
    for c in title.chars().chain(markdown.chars().take(20_000)) {
        if matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x20000..=0x2A6DF) {
            han += 1;
        } else if c.is_ascii_alphabetic() {
            latin += 1;
        }
    }
    han > 0 && han * 3 >= latin
}

#[derive(Debug, Clone, Default)]
pub struct Translate {
    /// `None`: not advertised (never request). `Some(false)`: no model configured.
    capability: Option<bool>,
    /// `translation_provider_v1.minutes`; `Some(false)`: the request makes no minutes.
    minutes_capability: Option<bool>,
    item: Option<String>,
    read_state: Option<AnalysisState>,
    read_reason: Option<String>,
    /// AI minutes as last read; `None` when the agent does not report them.
    minutes: Option<AiMinutes>,
    /// The text looks Chinese (until the agent answers for itself).
    chinese_text: bool,
    phase: Phase,
    poll_errors: u32,
    minutes_attempt: u32,
    /// Document batches finished, as last reported (`None`: not reported).
    progress: Option<TranslationProgress>,
    /// A newer agent's diagnostic of the last failed attempt.
    failure_detail: Option<String>,
    /// Blocks of the finished translation kept in the original (newer agents).
    untranslated: Option<usize>,
    /// `minutes_request_v1`: minutes start only from their own request.
    manual_minutes: bool,
    /// The last `minutes_request` (manual minutes only).
    minutes_ask: MinutesAsk,
}

/// Where the last `minutes_request` stands; the minutes' own state comes from
/// the agent's reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MinutesAsk {
    #[default]
    Idle,
    Requesting,
    /// `no_minutes_provider`: say so and point to settings.
    NoProvider,
    Error(String),
}

fn wire(state: AnalysisState) -> &'static str {
    match state {
        AnalysisState::NotRequested => "not_requested",
        AnalysisState::Queued => "queued",
        AnalysisState::Running => "running",
        AnalysisState::Succeeded => "succeeded",
        AnalysisState::Failed => "failed",
        AnalysisState::Unavailable => "unavailable",
    }
}

impl Translate {
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn set_capability(&mut self, available: Option<bool>) {
        self.capability = available;
    }

    pub fn set_minutes_capability(&mut self, minutes: Option<bool>) {
        self.minutes_capability = minutes;
    }

    /// `minutes_request_v1` advertised: minutes are manual.
    pub fn set_manual_minutes(&mut self, manual: bool) {
        self.manual_minutes = manual;
    }

    pub fn manual_minutes(&self) -> bool {
        self.manual_minutes
    }

    pub fn minutes_ask(&self) -> &MinutesAsk {
        &self.minutes_ask
    }

    /// The 纪要 button's state; `None` hides it (an agent without manual minutes).
    pub fn minutes_button(&self) -> Option<ButtonState> {
        if !self.manual_minutes {
            return None;
        }
        Some(match (&self.minutes_ask, self.minutes.as_ref().map(|m| &m.state)) {
            (MinutesAsk::Requesting, _) | (_, Some(MinutesState::Queued | MinutesState::Running)) => ButtonState::Generating,
            (MinutesAsk::NoProvider, _) => ButtonState::Failed(minutes_reason_label(Some("no_minutes_provider"))),
            (MinutesAsk::Error(message), _) => ButtonState::Failed(message.clone()),
            (_, Some(MinutesState::Succeeded)) => ButtonState::Done,
            (_, Some(MinutesState::Failed | MinutesState::Unavailable)) => ButtonState::Failed(minutes_reason_label(self.minutes.as_ref().and_then(|m| m.reason.as_deref()))),
            _ => ButtonState::NotGenerated,
        })
    }

    /// Starts a `minutes_request`: manual minutes that are not generated or
    /// failed. `false` means nothing may be sent.
    pub fn begin_minutes(&mut self) -> bool {
        if !matches!(self.minutes_button(), Some(ButtonState::NotGenerated | ButtonState::Failed(_))) {
            return false;
        }
        self.minutes_ask = MinutesAsk::Requesting;
        self.minutes_attempt = 0;
        true
    }

    /// The agent's answer to `minutes_request`: the minutes' state; follow it
    /// while it is queued or running.
    pub fn on_minutes_request(&mut self, result: Result<AiMinutes, ClientError>) -> Next {
        match result {
            Ok(minutes) => {
                self.minutes_ask = MinutesAsk::Idle;
                self.minutes = Some(minutes);
                self.with_minutes(Next::Stop)
            }
            Err(ClientError::Daemon(code)) if code == "no_minutes_provider" => {
                self.minutes_ask = MinutesAsk::NoProvider;
                Next::Stop
            }
            Err(ClientError::Unsupported) => {
                self.minutes_ask = MinutesAsk::Error(tx("当前代理不支持纪要请求").into());
                Next::Stop
            }
            Err(e) => {
                self.minutes_ask = MinutesAsk::Error(txf("纪要请求失败：{0}", &[&e.message()]));
                Next::Stop
            }
        }
    }

    /// The item's read state after a (re)load. A job already queued or running
    /// elsewhere is followed too, and so are minutes still being generated.
    pub fn reset(&mut self, item_id: &str, state: AnalysisState, reason: Option<String>, minutes: Option<AiMinutes>, chinese_text: bool) -> Next {
        let same = self.item.as_deref() == Some(item_id);
        if !same {
            self.minutes_ask = MinutesAsk::Idle;
        }
        self.item = Some(item_id.to_string());
        self.progress = None;
        self.failure_detail = None;
        self.untranslated = None;
        self.read_state = Some(state);
        self.read_reason = reason;
        self.minutes = minutes;
        self.chinese_text = chinese_text;
        self.poll_errors = 0;
        self.minutes_attempt = 0;
        let keep = same && matches!((&self.phase, state), (Phase::Succeeded, AnalysisState::Succeeded) | (Phase::NotNeeded, AnalysisState::NotRequested));
        if !keep {
            self.phase = Phase::Idle;
        }
        match state {
            AnalysisState::Queued | AnalysisState::Running => {
                self.phase = Phase::Waiting { running: state == AnalysisState::Running, attempt: 0 };
                Next::Poll(backoff(0))
            }
            _ => self.with_minutes(Next::Stop),
        }
    }

    fn reason(&self) -> Option<&str> {
        self.read_reason.as_deref()
    }

    /// Batch progress from the item read (call after `reset`).
    pub fn set_progress(&mut self, progress: Option<TranslationProgress>) {
        self.progress = progress;
    }

    /// A newer agent's failure diagnostic and kept-original block count (after `reset`).
    pub fn set_diagnostics(&mut self, failure_detail: Option<String>, untranslated: Option<usize>) {
        self.failure_detail = failure_detail;
        self.untranslated = untranslated;
    }

    /// 有 N 段未能翻译，已保留原文 for a finished translation with kept blocks.
    pub fn untranslated_note(&self) -> Option<String> {
        let done = self.phase == Phase::Succeeded || (self.phase == Phase::Idle && self.read_state == Some(AnalysisState::Succeeded));
        self.untranslated.filter(|n| done && *n > 0).map(|n| txf("有 {0} 段未能翻译，已保留原文", &[&n]))
    }

    /// The failure in plain words: the agent's diagnostic when it gave one this
    /// desktop reads, else its reason.
    fn failure_words(&self, reason: &str) -> String {
        self.failure_detail.as_deref().and_then(failure_detail_label).unwrap_or_else(|| translation_reason_label(reason))
    }

    /// The last translation failed part way: a request resumes it.
    fn unfinished(&self) -> Option<String> {
        let failed = matches!(self.phase, Phase::Failed(_)) || (self.phase == Phase::Idle && self.read_state == Some(AnalysisState::Failed));
        failed.then(|| unfinished_label(self.progress.as_ref())).flatten()
    }

    /// AI minutes as last read (`None`: the agent does not report them).
    pub fn minutes(&self) -> Option<&AiMinutes> {
        self.minutes.as_ref()
    }

    fn minutes_pending(&self) -> bool {
        self.minutes.as_ref().is_some_and(AiMinutes::is_pending)
    }

    /// The source is Chinese: the agent said so, or the text reads that way.
    fn chinese(&self) -> bool {
        self.reason() == Some("source_is_chinese") || (self.reason().is_none() && self.chinese_text)
    }

    fn translation_requestable(&self) -> bool {
        matches!(self.read_state, Some(AnalysisState::NotRequested | AnalysisState::Failed | AnalysisState::Unavailable)) && !self.chinese()
    }

    fn minutes_requestable(&self) -> bool {
        // Manual minutes have their own button; the translation never makes them.
        !self.manual_minutes
            && self.minutes_capability != Some(false) && self.minutes.as_ref().is_some_and(|m| matches!(m.state, MinutesState::NotRequested | MinutesState::Failed))
    }

    /// The request button applies: the agent can translate, and the item still
    /// needs a translation or its minutes (none yet, or the last attempt failed).
    pub fn can_request(&self) -> bool {
        let open = !matches!(self.phase, Phase::Requesting | Phase::Waiting { .. } | Phase::NoProvider) && !self.minutes_pending();
        self.capability == Some(true) && open && (self.translation_requestable() || self.minutes_requestable())
    }

    /// 翻译此文 while there is something to translate; otherwise the same request
    /// only generates minutes (a Chinese source, or a finished translation).
    /// 重试 when a failed translation resumes from its finished batches.
    pub fn action_label(&self) -> &'static str {
        if self.translation_requestable() && self.unfinished().is_some() {
            tx("重试")
        } else if self.translation_requestable() {
            tx("翻译此文")
        } else {
            tx("生成纪要")
        }
    }

    /// The source is Chinese: there is nothing to translate.
    pub fn not_needed(&self) -> bool {
        self.phase == Phase::NotNeeded || (self.phase == Phase::Idle && self.chinese())
    }

    /// No model configured: say so and point to settings.
    pub fn needs_provider(&self) -> bool {
        let requestable = matches!(self.read_state, Some(AnalysisState::NotRequested | AnalysisState::Failed | AnalysisState::Unavailable)) && !self.chinese();
        self.phase == Phase::NoProvider || (requestable && matches!(self.phase, Phase::Idle) && (self.capability == Some(false) || self.reason() == Some("no_translation_provider")))
    }

    /// Starts a request; `false` means nothing may be sent.
    pub fn begin(&mut self) -> bool {
        if !self.can_request() {
            return false;
        }
        self.phase = Phase::Requesting;
        self.poll_errors = 0;
        true
    }

    pub fn on_request(&mut self, result: Result<TranslationStatus, ClientError>) -> Next {
        match result {
            Ok(status) => {
                // With manual minutes the reply's minutes state is not a trigger.
                if status.ai_minutes.is_some() && !self.manual_minutes {
                    self.minutes = status.ai_minutes.clone();
                }
                if status.progress.is_some() {
                    self.progress = status.progress;
                }
                self.failure_detail = status.failure_detail.clone();
                let next = self.apply(&status.state, status.reason, 0);
                self.with_minutes(next)
            }
            Err(ClientError::Unsupported) => {
                self.phase = Phase::Error(tx("当前代理不支持翻译请求").into());
                Next::Stop
            }
            Err(e) => {
                self.phase = Phase::Error(txf("翻译请求失败：{0}", &[&e.message()]));
                Next::Stop
            }
        }
    }

    /// Keeps polling while the minutes are still being generated.
    fn with_minutes(&mut self, next: Next) -> Next {
        if next == Next::Stop && self.minutes_pending() {
            let delay = backoff(self.minutes_attempt);
            self.minutes_attempt += 1;
            return Next::Poll(delay);
        }
        next
    }

    /// One poll of the item's translation and minutes state.
    pub fn on_poll(&mut self, result: Result<TranslationItem, ClientError>) -> Next {
        let translating = match self.phase {
            Phase::Waiting { attempt, .. } => Some(attempt),
            _ => None,
        };
        if translating.is_none() && !self.minutes_pending() {
            return Next::Stop;
        }
        match result {
            Ok(item) => {
                self.poll_errors = 0;
                let was_pending = self.minutes_pending();
                self.minutes = item.ai_minutes.clone();
                let next = match translating {
                    Some(attempt) => {
                        self.progress = item.translation_progress;
                        self.read_state = Some(item.translation_state);
                        self.read_reason = item.translation_reason.clone();
                        self.failure_detail = item.translation_failure_detail.clone();
                        self.untranslated = item.untranslated_blocks;
                        self.apply(wire(item.translation_state), item.translation_reason, attempt + 1)
                    }
                    None => Next::Stop,
                };
                match self.with_minutes(next) {
                    // The minutes just ended: read the item again to show them.
                    Next::Stop if was_pending => Next::Reload,
                    other => other,
                }
            }
            Err(e) => {
                self.poll_errors += 1;
                if self.poll_errors >= POLL_ERRORS {
                    let message = if translating.is_some() { "无法读取翻译进度：{0}" } else { "无法读取纪要进度：{0}" };
                    self.phase = Phase::Error(txf(message, &[&e.message()]));
                    if let Some(m) = self.minutes.as_mut().filter(|m| m.is_pending()) {
                        // Unknown, not failed: stop following without claiming an outcome.
                        m.state = MinutesState::Other("unknown".into());
                    }
                    return Next::Stop;
                }
                match &mut self.phase {
                    Phase::Waiting { attempt, .. } => {
                        *attempt += 1;
                        Next::Poll(backoff(*attempt))
                    }
                    _ => self.with_minutes(Next::Stop),
                }
            }
        }
    }

    fn apply(&mut self, state: &str, reason: Option<String>, attempt: u32) -> Next {
        match reason.as_deref() {
            Some("source_is_chinese") => {
                self.read_reason = reason;
                self.phase = Phase::NotNeeded;
                return Next::Stop;
            }
            Some("no_translation_provider") => {
                self.phase = Phase::NoProvider;
                return Next::Stop;
            }
            _ => {}
        }
        match state {
            "queued" | "leased" | "running" => {
                let running = state != "queued";
                self.phase = Phase::Waiting { running, attempt };
                // A long translation is read every 3 s, so its progress and time move.
                Next::Poll(if running { RUNNING_POLL } else { backoff(attempt) })
            }
            "succeeded" => {
                self.phase = Phase::Succeeded;
                Next::Reload
            }
            "failed" | "unavailable" => {
                self.phase = Phase::Failed(reason);
                Next::Stop
            }
            // Not queued: say why when the agent does (never as success).
            "not_requested" => {
                self.phase = Phase::Error(reason.as_deref().map(translation_reason_label).unwrap_or_else(|| tx("代理没有排入这次翻译").into()));
                Next::Stop
            }
            other => {
                self.phase = Phase::Error(txf("代理返回了未知的翻译状态：{0}", &[&other]));
                Next::Stop
            }
        }
    }

    /// Status shown next to the mode switch, if any.
    pub fn status(&self) -> Option<String> {
        let failed = |reason: Option<&str>| match reason {
            Some(r) => txf("失败（{0}）", &[&self.failure_words(r)]),
            None => tx("失败（代理未说明原因）").into(),
        };
        let unfinished = |reason: Option<&str>| {
            let label = self.unfinished()?;
            Some(match reason {
                Some(r) => txf("翻译未完成（已完成 {0}）：{1}", &[&self.progress.as_ref().and_then(TranslationProgress::fraction).unwrap_or_default(), &self.failure_words(r)]),
                None => label,
            })
        };
        match &self.phase {
            Phase::Requesting => Some(tx("正在请求翻译…").into()),
            Phase::Waiting { running: false, .. } => Some(match self.progress.as_ref().and_then(TranslationProgress::fraction) {
                Some(f) if self.progress.is_some_and(|p| p.done_batches > 0) => txf("排队中 {0}", &[&f]),
                _ => tx("排队中").into(),
            }),
            Phase::Waiting { running: true, .. } => Some(translating_label(self.progress.as_ref())),
            Phase::Failed(reason) if self.unfinished().is_some() => unfinished(reason.as_deref()),
            Phase::Succeeded => Some(self.untranslated_note().unwrap_or_else(|| tx("已完成").into())),
            Phase::Failed(reason) => Some(failed(reason.as_deref())),
            Phase::NotNeeded => Some(translation_reason_label("source_is_chinese")),
            Phase::NoProvider => Some(translation_reason_label("no_translation_provider")),
            Phase::Error(message) => Some(message.clone()),
            Phase::Idle => match (self.read_state?, self.reason()) {
                (_, Some("source_is_chinese")) => Some(translation_reason_label("source_is_chinese")),
                (AnalysisState::Succeeded, _) if self.untranslated_note().is_some() => self.untranslated_note(),
                (AnalysisState::Failed, reason) if self.unfinished().is_some() => unfinished(reason),
                (AnalysisState::Failed, reason) => Some(failed(reason)),
                (AnalysisState::Unavailable, Some(r)) => Some(translation_reason_label(r)),
                (AnalysisState::Unavailable, None) if self.capability == Some(false) => Some(translation_reason_label("no_translation_provider")),
                (AnalysisState::Unavailable, None) => Some(tx("暂无译文").into()),
                (AnalysisState::NotRequested, _) if self.capability == Some(false) => Some(translation_reason_label("no_translation_provider")),
                _ => None,
            },
        }
    }

    /// The toolbar's 翻译 button. Never changes state by itself.
    pub fn button(&self) -> TrButton {
        if self.capability.is_none() {
            return TrButton::Reason(tx("当前代理不支持翻译请求").into());
        }
        let failed = matches!(self.phase, Phase::Failed(_)) || (self.phase == Phase::Idle && self.read_state == Some(AnalysisState::Failed));
        match &self.phase {
            Phase::Requesting => return TrButton::Running(tx("正在请求翻译…").into()),
            Phase::Waiting { running, .. } => return TrButton::Running(if *running { translating_label(self.progress.as_ref()) } else { tx("排队中").into() }),
            Phase::Succeeded => return TrButton::Done,
            Phase::Error(message) => return TrButton::Recheck(message.clone()),
            _ => {}
        }
        if self.not_needed() {
            return TrButton::Reason(translation_reason_label("source_is_chinese"));
        }
        if self.needs_provider() {
            return TrButton::Settings;
        }
        match (self.read_state, self.reason()) {
            (None, _) => TrButton::Recheck(tx("还没有读到翻译状态").into()),
            (Some(AnalysisState::Succeeded), _) => TrButton::Done,
            (Some(AnalysisState::Queued | AnalysisState::Running), _) => TrButton::Running(translating_label(self.progress.as_ref())),
            // Skipped by the agent (e.g. unsupported type): not requestable from here.
            (Some(AnalysisState::NotRequested), Some(r)) => TrButton::Reason(translation_reason_label(r)),
            _ if failed => TrButton::Request(tx("重试")),
            _ => TrButton::Request(tx("翻译此文")),
        }
    }

    /// The state read again after a read failure. `None`: still nothing running
    /// or finished, so the caller may request; otherwise what to do instead.
    pub fn on_recheck(&mut self, result: Result<TranslationItem, ClientError>) -> Option<Next> {
        match result {
            Ok(item) => {
                self.read_state = Some(item.translation_state);
                self.read_reason = item.translation_reason.clone();
                self.progress = item.translation_progress;
                if item.ai_minutes.is_some() { self.minutes = item.ai_minutes.clone(); }
                self.poll_errors = 0;
                match item.translation_state {
                    AnalysisState::Queued | AnalysisState::Running => {
                        self.phase = Phase::Waiting { running: item.translation_state == AnalysisState::Running, attempt: 0 };
                        Some(Next::Poll(backoff(0)))
                    }
                    AnalysisState::Succeeded => { self.phase = Phase::Succeeded; Some(Next::Reload) }
                    _ => {
                        self.phase = Phase::Idle;
                        if matches!(self.button(), TrButton::Request(_)) { None } else { Some(Next::Stop) }
                    }
                }
            }
            Err(e) => {
                self.phase = Phase::Error(txf("无法读取翻译状态：{0}", &[&e.message()]));
                Some(Next::Stop)
            }
        }
    }

    /// The status is a problem (shown in a warning tone).
    pub fn is_problem(&self) -> bool {
        matches!(self.phase, Phase::Failed(_) | Phase::Error(_)) || (self.phase == Phase::Idle && self.read_state == Some(AnalysisState::Failed)) || self.untranslated_note().is_some()
    }

    /// The translation is running now (the 翻译 button shows a moving bar).
    pub fn is_running(&self) -> bool {
        matches!(self.phase, Phase::Waiting { running: true, .. }) || (self.phase == Phase::Idle && self.read_state == Some(AnalysisState::Running))
    }
}

#[cfg(test)]
fn now_for_test() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_show_the_failed_part_kept_blocks_and_running_time() {
        let mut t = Translate::default();
        t.set_capability(Some(true));
        // Failed with a diagnostic: the part and check in plain words.
        t.reset("i", AnalysisState::Failed, Some("retry_exhausted".into()), None, false);
        t.set_progress(Some(TranslationProgress { done_batches: 2, total_batches: 5, started_at: None }));
        t.set_diagnostics(Some("batch_3_of_5:count_mismatch".into()), None);
        let status = t.status().unwrap();
        assert!(status.contains("2/5") && (status.contains("第 3 段：模型返回块数不对") || status.contains("Part 3")), "{status}");
        // An older agent: the reason, as before.
        t.reset("i", AnalysisState::Failed, Some("retry_exhausted".into()), None, false);
        let status = t.status().unwrap();
        assert!(status.contains("多次重试后仍未成功") || status.contains("retr"), "{status}");
        // Finished with kept blocks: said, in the problem tone.
        t.reset("i", AnalysisState::Succeeded, None, None, false);
        t.set_diagnostics(None, Some(3));
        assert!(matches!(t.status().as_deref(), Some("有 3 段未能翻译，已保留原文" | "3 parts couldn't be translated and were kept in the original")));
        assert!(t.is_problem());
        t.set_diagnostics(None, Some(0));
        assert_eq!(t.untranslated_note(), None);
        // Running: read every 3 s at least, and the button moves.
        t.reset("j", AnalysisState::NotRequested, None, None, false);
        assert!(t.begin());
        let reply = TranslationStatus { progress: Some(TranslationProgress { done_batches: 1, total_batches: 2, started_at: Some(super::super::translate::now_for_test() - 190) }), ..status_of("running") };
        assert_eq!(t.on_request(Ok(reply)), Next::Poll(RUNNING_POLL));
        assert!(t.is_running());
        match t.button() {
            TrButton::Running(label) => assert!(label.contains("1/2") && label.contains('3'), "{label}"),
            other => panic!("{other:?}"),
        }
        // Long-running jobs do not drift into the queued job's 4 s backoff.
        for _ in 0..12 {
            let polled = TranslationItem { translation_progress: Some(TranslationProgress { done_batches: 1, total_batches: 2, started_at: Some(now_for_test() - 190) }), ..item(AnalysisState::Running, None) };
            assert_eq!(t.on_poll(Ok(polled)), Next::Poll(RUNNING_POLL));
            assert!(t.is_running());
        }
    }

    #[test]
    fn progress_shows_in_the_chip_and_a_partial_failure_offers_retry() {
        let p = |d, t| Some(TranslationProgress { done_batches: d, total_batches: t, started_at: None });
        let mut t = Translate::default();
        t.set_capability(Some(true));
        t.reset("i", AnalysisState::Failed, Some("provider_timeout".into()), None, false);
        t.set_progress(p(5, 18));
        assert!(t.can_request());
        assert!(matches!(t.action_label(), "重试" | "Retry"));
        let status = t.status().unwrap();
        assert!(status.contains("5/18") && (status.contains("翻译未完成") || status.contains("incomplete")), "{status}");
        // Without finished batches it is an ordinary failure with 翻译此文.
        t.reset("i", AnalysisState::Failed, None, None, false);
        t.set_progress(p(0, 18));
        assert!(matches!(t.action_label(), "翻译此文" | "Translate"));
        // Running: 翻译中 5/18, from the request reply and then the polls.
        t.reset("j", AnalysisState::NotRequested, None, None, false);
        assert!(t.begin());
        let reply = TranslationStatus { progress: p(5, 18), ..status_of("running") };
        assert!(matches!(t.on_request(Ok(reply)), Next::Poll(_)));
        assert!(matches!(t.status().as_deref(), Some("翻译中 5/18" | "Translating 5/18")));
        let polled = TranslationItem { translation_progress: p(9, 18), ..item(AnalysisState::Running, None) };
        t.on_poll(Ok(polled));
        assert!(matches!(t.status().as_deref(), Some("翻译中 9/18" | "Translating 9/18")));
        // Unknown progress: plain 翻译中.
        t.on_poll(Ok(item(AnalysisState::Running, None)));
        assert!(matches!(t.status().as_deref(), Some("翻译中" | "Translating")));
    }

    fn status_of(state: &str) -> TranslationStatus {
        status(state, None)
    }
    use memex_client::{fixture::FixtureBackend, Backend, BackendMode, Client, Command, Response};
    use std::sync::{Arc, Mutex};

    fn status(state: &str, reason: Option<&str>) -> TranslationStatus {
        TranslationStatus { state: state.into(), reason: reason.map(str::to_string), revision_id: Some("r".into()), ai_minutes: None, translated_title: None, progress: None, failure_detail: None, untranslated_blocks: None }
    }

    fn item(state: AnalysisState, reason: Option<&str>) -> TranslationItem {
        TranslationItem { item_id: "i".into(), revision_id: "r".into(), summary_state: AnalysisState::NotRequested, summary: None, translation_state: state, title: None, markdown: None, translation_reason: reason.map(str::to_string), ai_minutes: None, ai_insights: None, translated_title: None, translation_progress: None, title_source: None, translation_failure_detail: None, untranslated_blocks: None }
    }

    #[test]
    fn request_queue_run_and_succeed_with_backoff() {
        let mut t = Translate::default();
        t.set_capability(Some(true));
        assert_eq!(t.reset("i", AnalysisState::NotRequested, None, None, false), Next::Stop);
        assert!(t.can_request() && t.status().is_none());
        assert!(t.begin());
        assert!(!t.begin(), "no second request while one is in flight");
        assert_eq!(t.on_request(Ok(status("queued", None))), Next::Poll(backoff(0)));
        assert_eq!(t.status().as_deref(), Some("排队中"));
        // Running: read every 3 s (progress and time keep moving).
        assert_eq!(t.on_poll(Ok(item(AnalysisState::Running, None))), Next::Poll(RUNNING_POLL));
        assert_eq!(t.status().as_deref(), Some("翻译中"));
        // A transient read error keeps polling, later and later.
        assert_eq!(t.on_poll(Err(ClientError::Timeout)), Next::Poll(backoff(2)));
        assert!(backoff(2) > backoff(1) && backoff(40) == Duration::from_secs(4));
        assert_eq!(t.on_poll(Ok(item(AnalysisState::Succeeded, None))), Next::Reload);
        assert_eq!(t.status().as_deref(), Some("已完成"));
        // The reload after success keeps 已完成 for the same item.
        t.reset("i", AnalysisState::Succeeded, None, None, false);
        assert_eq!(t.phase(), &Phase::Succeeded);
    }

    #[test]
    fn failures_and_chinese_sources_are_reported_in_plain_words_never_as_success() {
        let mut t = Translate::default();
        t.set_capability(Some(true));
        t.reset("i", AnalysisState::NotRequested, None, None, false);
        assert!(t.begin());
        assert_eq!(t.on_request(Ok(status("queued", None))), Next::Poll(backoff(0)));
        assert_eq!(t.on_poll(Ok(item(AnalysisState::Failed, Some("provider_timeout")))), Next::Stop);
        assert_eq!(t.status().as_deref(), Some("失败（翻译服务响应超时）"));
        assert!(t.is_problem() && t.can_request(), "a failed translation can be asked again");
        t.reset("j", AnalysisState::NotRequested, None, None, false);
        assert!(t.begin());
        // As the agent answers a Chinese source: not queued, with the reason.
        assert_eq!(t.on_request(Ok(status("not_requested", Some("source_is_chinese")))), Next::Stop);
        assert_eq!(t.status().as_deref(), Some("原文为中文，无需翻译"));
        assert!(!t.can_request());
        // An answer that is neither queued nor done is not success.
        t.reset("k", AnalysisState::NotRequested, None, None, false);
        assert!(t.begin());
        assert_eq!(t.on_request(Ok(status("not_requested", None))), Next::Stop);
        assert!(matches!(t.phase(), Phase::Error(_)));
        // The read state alone also carries the reason.
        t.reset("l", AnalysisState::Failed, None, None, false);
        assert_eq!(t.status().as_deref(), Some("失败（代理未说明原因）"));
    }

    #[test]
    fn the_toolbar_button_follows_the_state_and_a_failed_read_is_read_again_before_any_request() {
        let mut t = Translate::default();
        t.set_capability(Some(true));
        t.reset("i", AnalysisState::NotRequested, None, None, false);
        assert!(matches!(t.button(), TrButton::Request("翻译此文" | "Translate")));
        t.reset("i", AnalysisState::Failed, Some("provider_timeout".into()), None, false);
        assert!(matches!(t.button(), TrButton::Request("重试" | "Retry")));
        t.reset("i", AnalysisState::Running, None, None, false);
        assert!(matches!(t.button(), TrButton::Running(_)));
        t.reset("i", AnalysisState::Succeeded, None, None, false);
        assert_eq!(t.button(), TrButton::Done);
        // A read failure: 重试 reads first. Running or finished: no request.
        t.reset("i", AnalysisState::NotRequested, None, None, false);
        assert_eq!(t.on_recheck(Err(ClientError::Timeout)), Some(Next::Stop));
        assert!(matches!(t.button(), TrButton::Recheck(_)));
        assert_eq!(t.on_recheck(Ok(item(AnalysisState::Running, None))), Some(Next::Poll(backoff(0))));
        assert!(matches!(t.button(), TrButton::Running(_)));
        t.reset("i", AnalysisState::NotRequested, None, None, false);
        let _ = t.on_recheck(Err(ClientError::Timeout));
        assert_eq!(t.on_recheck(Ok(item(AnalysisState::Succeeded, None))), Some(Next::Reload));
        assert_eq!(t.button(), TrButton::Done);
        // Still not requested after the read: the caller may now request.
        t.reset("i", AnalysisState::NotRequested, None, None, false);
        let _ = t.on_recheck(Err(ClientError::Timeout));
        assert_eq!(t.on_recheck(Ok(item(AnalysisState::NotRequested, None))), None);
        // Skipped by the agent: disabled with its reason, never requested.
        t.reset("i", AnalysisState::NotRequested, Some("source_is_chinese".into()), None, false);
        assert!(matches!(t.button(), TrButton::Reason(_)));
    }

    fn minutes(state: MinutesState, text: Option<&str>) -> Option<AiMinutes> {
        Some(AiMinutes::new(state, None, text))
    }

    #[test]
    fn a_chinese_source_offers_minutes_and_follows_them_until_they_end() {
        assert!(looks_chinese("中文学术写作中的常见问题", "这是第 1 段合成示例正文，包含 Retrieval-Augmented Generation (RAG)。"));
        assert!(!looks_chinese("A Survey on Retrieval-Augmented Generation", "This is synthetic paragraph 1 about 检索."));
        let mut t = Translate::default();
        t.set_capability(Some(true));
        t.reset("zh", AnalysisState::NotRequested, None, minutes(MinutesState::NotRequested, None), true);
        assert!(t.can_request());
        assert_eq!(t.action_label(), "生成纪要");
        assert!(t.begin());
        // Translation is not needed, but the minutes were queued: keep following.
        let reply = TranslationStatus { ai_minutes: minutes(MinutesState::Queued, None), ..status("not_requested", Some("source_is_chinese")) };
        assert_eq!(t.on_request(Ok(reply)), Next::Poll(backoff(0)));
        assert!(t.not_needed() && !t.can_request());
        let poll = |state, text| TranslationItem { ai_minutes: minutes(state, text), ..item(AnalysisState::NotRequested, None) };
        assert_eq!(t.on_poll(Ok(poll(MinutesState::Running, None))), Next::Poll(backoff(1)));
        assert_eq!(t.minutes().unwrap().state_line().as_deref(), Some("纪要生成中…"));
        assert_eq!(t.on_poll(Ok(poll(MinutesState::Succeeded, Some("纪要")))), Next::Reload);
        t.reset("zh", AnalysisState::NotRequested, None, minutes(MinutesState::Succeeded, Some("纪要")), true);
        assert!(!t.can_request() && t.not_needed());
        // An English item with a translation but failed minutes: the button only makes minutes.
        t.reset("en", AnalysisState::Succeeded, None, Some(AiMinutes::new(MinutesState::Failed, Some("provider_timeout"), None)), false);
        assert!(t.can_request());
        assert_eq!(t.action_label(), "生成纪要");
        t.reset("en2", AnalysisState::NotRequested, None, minutes(MinutesState::NotRequested, None), false);
        assert_eq!(t.action_label(), "翻译此文");
        // An older agent without minutes: a Chinese source offers nothing.
        t.reset("old", AnalysisState::NotRequested, None, None, true);
        assert!(!t.can_request());
        // Minutes already running elsewhere are followed after a load.
        assert_eq!(t.reset("run", AnalysisState::Succeeded, None, minutes(MinutesState::Running, None), false), Next::Poll(backoff(0)));
    }

    #[test]
    fn manual_minutes_button_state_machine() {
        let label = |t: &Translate| t.minutes_button().map(|b| b.label());
        let mut t = Translate::default();
        t.set_capability(Some(true));
        // An older agent: no button, and begin_minutes refuses.
        t.reset("en", AnalysisState::Succeeded, None, minutes(MinutesState::NotRequested, None), false);
        assert_eq!(t.minutes_button(), None);
        assert!(!t.begin_minutes());
        t.set_manual_minutes(true);
        assert_eq!(label(&t), Some("未生成"));
        // A translated item has nothing left for 翻译此文: no header offer, no 生成纪要.
        assert!(!t.can_request());
        assert!(t.begin_minutes());
        assert_eq!(label(&t), Some("生成中"));
        assert!(!t.begin_minutes(), "one request at a time");
        assert_eq!(t.on_minutes_request(Ok(AiMinutes::new(MinutesState::Queued, None, None))), Next::Poll(backoff(0)));
        assert_eq!(label(&t), Some("生成中"));
        let poll = |state, reason, text| TranslationItem { ai_minutes: Some(AiMinutes::new(state, reason, text)), ..item(AnalysisState::Succeeded, None) };
        assert_eq!(t.on_poll(Ok(poll(MinutesState::Running, None, None))), Next::Poll(backoff(1)));
        assert_eq!(t.on_poll(Ok(poll(MinutesState::Failed, Some("provider_timeout"), None))), Next::Reload);
        assert!(matches!(t.minutes_button(), Some(ButtonState::Failed(r)) if r == "模型服务响应超时"));
        // 失败 → the button retries.
        assert!(t.begin_minutes());
        assert_eq!(t.on_minutes_request(Ok(AiMinutes::new(MinutesState::Running, None, None))), Next::Poll(backoff(0)));
        assert_eq!(t.on_poll(Ok(poll(MinutesState::Succeeded, None, Some("纪要")))), Next::Reload);
        assert_eq!(label(&t), Some("已完成"));
        assert!(!t.begin_minutes(), "已完成 toggles the card, it never asks again");
        // No model: a failure the card turns into 去设置; another item starts clean.
        t.reset("x", AnalysisState::Succeeded, None, minutes(MinutesState::NotRequested, None), false);
        assert!(t.begin_minutes());
        assert_eq!(t.on_minutes_request(Err(ClientError::Daemon("no_minutes_provider".into()))), Next::Stop);
        assert_eq!(t.minutes_ask(), &MinutesAsk::NoProvider);
        assert!(matches!(t.minutes_button(), Some(ButtonState::Failed(r)) if r == "还没有配置纪要模型"));
        t.reset("y", AnalysisState::Succeeded, None, minutes(MinutesState::NotRequested, None), false);
        assert_eq!((t.minutes_ask(), label(&t)), (&MinutesAsk::Idle, Some("未生成")));
    }

    #[test]
    fn with_manual_minutes_a_translation_reply_never_starts_or_shows_minutes() {
        let mut t = Translate::default();
        t.set_capability(Some(true));
        t.set_manual_minutes(true);
        t.reset("en", AnalysisState::NotRequested, None, minutes(MinutesState::NotRequested, None), false);
        assert_eq!(t.action_label(), "翻译此文");
        assert!(t.begin());
        // Even if an agent's reply carried a queued minutes state, it is not a trigger.
        let reply = TranslationStatus { ai_minutes: minutes(MinutesState::Queued, None), ..status("queued", None) };
        assert_eq!(t.on_request(Ok(reply)), Next::Poll(backoff(0)));
        assert_eq!(t.minutes().map(|m| m.state.clone()), Some(MinutesState::NotRequested));
        assert_eq!(t.minutes_button().map(|b| b.label()), Some("未生成"));
        // The translation finishes; the minutes are still not generated.
        assert_eq!(t.on_poll(Ok(TranslationItem { ai_minutes: minutes(MinutesState::NotRequested, None), ..item(AnalysisState::Succeeded, None) })), Next::Reload);
        assert_eq!(t.minutes_button().map(|b| b.label()), Some("未生成"));
    }

    #[test]
    fn a_chinese_source_with_manual_minutes_offers_only_the_minutes_button() {
        let mut t = Translate::default();
        t.set_capability(Some(true));
        t.set_manual_minutes(true);
        t.reset("zh", AnalysisState::NotRequested, Some("source_is_chinese".into()), minutes(MinutesState::NotRequested, None), true);
        // No 生成纪要 header offer through translation_request any more.
        assert!(!t.can_request() && t.not_needed());
        assert_eq!(t.status().as_deref(), Some("原文为中文，无需翻译"));
        assert!(t.begin_minutes());
        assert_eq!(t.on_minutes_request(Ok(AiMinutes::new(MinutesState::Queued, None, None))), Next::Poll(backoff(0)));
        assert!(t.not_needed(), "the notice stays; it does not suppress the minutes");
        let poll = |state, text| TranslationItem { ai_minutes: minutes(state, text), ..item(AnalysisState::NotRequested, Some("source_is_chinese")) };
        assert_eq!(t.on_poll(Ok(poll(MinutesState::Succeeded, Some("纪要")))), Next::Reload);
        assert_eq!(t.minutes_button(), Some(ButtonState::Done));
    }

    #[test]
    fn manual_minutes_are_never_requested_without_the_capability() {
        struct Recording(FixtureBackend, Arc<Mutex<Vec<String>>>);
        impl Backend for Recording {
            fn send(&self, c: &Command) -> Result<Response, ClientError> {
                self.1.lock().unwrap().push(serde_json::to_value(c).unwrap()["command"].as_str().unwrap().to_string());
                self.0.send(c)
            }
            fn mode(&self) -> BackendMode {
                self.0.mode()
            }
        }
        let fixture = FixtureBackend::new(12);
        fixture.set_controls(memex_client::fixture::FixtureControls { no_minutes_request: true, ..Default::default() });
        let sent = Arc::new(Mutex::new(vec![]));
        let client = Client::new(Recording(fixture, sent.clone()));
        let caps = client.capabilities().unwrap();
        let mut t = Translate::default();
        t.set_capability(caps.translation);
        t.set_manual_minutes(caps.minutes_request);
        let read = client.library_item_read("item_fixture_000008").unwrap();
        t.reset("item_fixture_000008", read.translation_state, read.translation_reason, read.ai_minutes, true);
        // The old behaviour: no 纪要 button, the header offers 生成纪要 via translation_request.
        assert_eq!(t.minutes_button(), None);
        assert!(!t.begin_minutes());
        assert_eq!(t.action_label(), "生成纪要");
        assert_eq!(client.minutes_request("item_fixture_000008"), Err(ClientError::Unsupported));
        assert!(!sent.lock().unwrap().iter().any(|c| c == "minutes_request"), "{:?}", sent.lock().unwrap());
    }

    #[test]
    fn without_the_capability_no_request_is_sent() {
        // The model refuses to start.
        let mut t = Translate::default();
        t.reset("i", AnalysisState::NotRequested, None, None, false);
        assert!(!t.begin(), "capability not advertised");
        t.set_capability(Some(false));
        assert!(!t.begin() && t.needs_provider());
        assert_eq!(t.status().as_deref(), Some("还没有配置翻译模型"));

        // And the client never sends `translation_request` on its own either.
        struct Recording(FixtureBackend, Arc<Mutex<Vec<String>>>, bool);
        impl Backend for Recording {
            fn send(&self, c: &Command) -> Result<Response, ClientError> {
                let name = serde_json::to_value(c).unwrap()["command"].as_str().unwrap().to_string();
                self.1.lock().unwrap().push(name);
                let mut r = self.0.send(c)?;
                if let (Some(caps), false) = (r.capabilities.as_mut(), self.2) {
                    caps.translation_provider_v1 = None;
                }
                Ok(r)
            }
            fn mode(&self) -> BackendMode {
                self.0.mode()
            }
        }
        let fixture = FixtureBackend::new(4);
        let sent = Arc::new(Mutex::new(vec![]));
        let old = Client::new(Recording(fixture.clone(), sent.clone(), false));
        assert_eq!(old.translation_request("item_fixture_000002"), Err(ClientError::Unsupported));
        fixture.set_controls(memex_client::fixture::FixtureControls { no_translation_provider: true, ..Default::default() });
        let unconfigured = Client::new(Recording(fixture.clone(), sent.clone(), true));
        assert_eq!(unconfigured.translation_request("item_fixture_000002"), Err(ClientError::Unsupported));
        assert_eq!(*sent.lock().unwrap(), ["capabilities", "capabilities"]);
        // With a configured model the request goes out and the job is queued.
        fixture.set_controls(Default::default());
        let ready = Client::new(Recording(fixture, sent.clone(), true));
        assert_eq!(ready.translation_request("item_fixture_000002").map(|s| s.state).as_deref(), Ok("queued"));
        assert_eq!(sent.lock().unwrap().last().map(String::as_str), Some("translation_request"));
    }
}
