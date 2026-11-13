//! 重点标注 / 金句 / 事实核查: which view is shown, whether emphasis is on, the
//! state each button shows, and the `insight_request` + polling state machine.
//! Pure; the reader view sends the requests. Nothing is shown as done unless the
//! agent says `succeeded`.

use crate::translate::{backoff, Next};
use memex_client::insights::{insight_reason_label, AiInsights, FactItem, HighlightItem, InsightKind, InsightKinds, QuoteItem, SIDES_BOTH, TRANSLATION_CHANGED};
use memex_client::{txf, ClientError, MinutesState, TranslationItem};
use std::collections::{BTreeMap, BTreeSet};

/// What the reader body shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Text,
    Quotes,
    FactCheck,
    /// AI 纪要 read in the full reading area (large screens).
    Minutes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ButtonState {
    NotGenerated,
    Generating,
    Done,
    Failed(String),
    /// Highlights only: the translation changed since they were made
    /// (`unavailable / insight_translation_changed`); no marks are drawn.
    Stale(String),
}

impl ButtonState {
    pub fn label(&self) -> &'static str {
        memex_client::tx(match self {
            ButtonState::NotGenerated => "未生成",
            ButtonState::Generating => "生成中",
            ButtonState::Done => "已完成",
            ButtonState::Failed(_) => "失败",
            ButtonState::Stale(_) => "已过期",
        })
    }
}

const POLL_ERRORS: u32 = 5;

#[derive(Debug, Clone, Default)]
pub struct Insights {
    /// `None`: the agent does not advertise `ai_insights_v1` (buttons hidden).
    kinds: Option<InsightKinds>,
    item: Option<String>,
    data: Option<AiInsights>,
    /// Requests sent and not yet answered.
    requesting: BTreeSet<InsightKind>,
    /// Request errors (the agent refused or could not be reached).
    errors: BTreeMap<InsightKind, String>,
    /// 重点标注 shown on the original text; one choice for the session.
    pub emphasis: bool,
    pub view: View,
    attempt: u32,
    poll_errors: u32,
}

impl Insights {
    pub fn set_capability(&mut self, kinds: Option<InsightKinds>) {
        self.kinds = kinds;
        if kinds.is_none() {
            self.view = View::Text;
        }
    }

    /// The three buttons are shown.
    pub fn visible(&self) -> bool {
        self.kinds.is_some()
    }

    pub fn available(&self, kind: InsightKind) -> bool {
        self.kinds.is_some_and(|k| k.has(kind))
    }

    /// The item's insights after a (re)load; follows kinds still generating.
    pub fn reset(&mut self, item_id: &str, data: Option<AiInsights>) -> Next {
        if self.item.as_deref() != Some(item_id) {
            self.errors.clear();
            self.view = View::Text;
        }
        self.item = Some(item_id.to_string());
        self.data = data;
        self.requesting.clear();
        self.attempt = 0;
        self.poll_errors = 0;
        self.next()
    }

    fn pending(&self) -> bool {
        !self.requesting.is_empty() || self.data.as_ref().is_some_and(|d| InsightKind::ALL.iter().any(|k| d.is_pending(*k)))
    }

    fn next(&mut self) -> Next {
        if self.data.as_ref().is_some_and(|d| InsightKind::ALL.iter().any(|k| d.is_pending(*k))) {
            let delay = backoff(self.attempt);
            self.attempt += 1;
            Next::Poll(delay)
        } else {
            Next::Stop
        }
    }

    pub fn state(&self, kind: InsightKind) -> ButtonState {
        if self.requesting.contains(&kind) {
            return ButtonState::Generating;
        }
        if let Some(e) = self.errors.get(&kind) {
            return ButtonState::Failed(e.clone());
        }
        match self.data.as_ref().and_then(|d| d.state(kind)) {
            Some((MinutesState::Queued | MinutesState::Running, _)) => ButtonState::Generating,
            Some((MinutesState::Succeeded, _)) => ButtonState::Done,
            Some((MinutesState::Failed, reason)) => ButtonState::Failed(insight_reason_label(reason)),
            Some((MinutesState::Unavailable, Some(TRANSLATION_CHANGED))) => ButtonState::Stale(insight_reason_label(Some(TRANSLATION_CHANGED))),
            _ => ButtonState::NotGenerated,
        }
    }

    /// A button press. Returns the kind to request, if one must be sent.
    pub fn press(&mut self, kind: InsightKind) -> Option<InsightKind> {
        if !self.available(kind) {
            return None;
        }
        let state = self.state(kind);
        match kind {
            InsightKind::Highlights if state == ButtonState::Done => {
                self.emphasis = !self.emphasis;
                return None;
            }
            InsightKind::Highlights => self.emphasis = true,
            InsightKind::Quotes | InsightKind::FactCheck => {
                let view = if kind == InsightKind::Quotes { View::Quotes } else { View::FactCheck };
                if self.view == view {
                    self.view = View::Text;
                    return None;
                }
                self.view = view;
            }
        }
        self.begin(kind).then_some(kind)
    }

    /// Starts a request unless one is running or the kind is done.
    pub fn begin(&mut self, kind: InsightKind) -> bool {
        if !self.available(kind) || matches!(self.state(kind), ButtonState::Generating | ButtonState::Done) {
            return false;
        }
        self.errors.remove(&kind);
        self.requesting.insert(kind);
        true
    }

    pub fn on_request(&mut self, kind: InsightKind, result: Result<AiInsights, ClientError>) -> Next {
        self.requesting.remove(&kind);
        match result {
            Ok(data) => {
                self.data = Some(data);
                self.attempt = 0;
            }
            Err(e) => {
                self.errors.insert(kind, e.message());
            }
        }
        self.next()
    }

    /// One poll of the item while some kind is generating.
    pub fn on_poll(&mut self, result: Result<TranslationItem, ClientError>) -> Next {
        if !self.pending() {
            return Next::Stop;
        }
        match result {
            Ok(item) => {
                self.poll_errors = 0;
                self.data = item.ai_insights;
                self.next()
            }
            Err(e) => {
                self.poll_errors += 1;
                if self.poll_errors >= POLL_ERRORS {
                    for kind in InsightKind::ALL {
                        if self.data.as_ref().is_some_and(|d| d.is_pending(kind)) {
                            self.errors.insert(kind, txf("无法读取生成进度：{0}", &[&e.message()]));
                        }
                    }
                    self.data = None;
                    return Next::Stop;
                }
                let delay = backoff(self.attempt);
                self.attempt += 1;
                Next::Poll(delay)
            }
        }
    }

    pub fn highlights(&self) -> Option<&[HighlightItem]> {
        self.data.as_ref()?.highlights.as_ref()?.done()
    }

    pub fn quotes(&self) -> Option<&[QuoteItem]> {
        self.data.as_ref()?.quotes.as_ref()?.done()
    }

    pub fn facts(&self) -> Option<&[FactItem]> {
        self.data.as_ref()?.fact_check.as_ref()?.done()
    }

    /// The fact checks rest on the model's own knowledge, not the web.
    pub fn model_knowledge(&self) -> bool {
        self.data.as_ref().and_then(|d| d.fact_check.as_ref()).and_then(|f| f.basis.as_deref()) == Some("model_knowledge")
    }

    /// The highlights are marked on the translation too: `original_and_translation`,
    /// or (no `sides`) some item carries a translation span.
    pub fn translation_marked(&self) -> bool {
        let Some(section) = self.data.as_ref().and_then(|d| d.highlights.as_ref()) else { return false };
        let Some(items) = section.done() else { return false };
        match section.sides.as_deref() {
            Some(sides) => sides == SIDES_BOTH,
            None => items.iter().any(|i| i.translation.is_some()),
        }
    }

    /// 重点标注 is shown on the original text now.
    pub fn emphasis_shown(&self) -> bool {
        self.emphasis && self.highlights().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_client::insights::InsightSection;
    use memex_client::{fixture::{FixtureBackend, FixtureControls, SHOWCASE_ITEM}, Backend, BackendMode, Client, Command, Response};
    use std::sync::{Arc, Mutex};

    fn all() -> Option<InsightKinds> {
        Some(InsightKinds { highlights: true, quotes: true, fact_check: true })
    }

    fn data(state: MinutesState) -> AiInsights {
        AiInsights { highlights: Some(InsightSection::new(state.clone())), quotes: Some(InsightSection::new(state.clone())), fact_check: Some(InsightSection::new(state)) }
    }

    #[test]
    fn buttons_switch_views_toggle_emphasis_and_request_only_what_is_missing() {
        let mut i = Insights::default();
        i.set_capability(all());
        assert_eq!(i.reset("a", Some(data(MinutesState::NotRequested))), Next::Stop);
        assert_eq!(i.state(InsightKind::Quotes), ButtonState::NotGenerated);
        // 金句 not generated: switch to its view and ask once.
        assert_eq!(i.press(InsightKind::Quotes), Some(InsightKind::Quotes));
        assert_eq!((i.view, i.state(InsightKind::Quotes)), (View::Quotes, ButtonState::Generating));
        assert_eq!(i.begin(InsightKind::Quotes), false, "no second request while one runs");
        let mut queued = data(MinutesState::NotRequested);
        queued.quotes = Some(InsightSection::new(MinutesState::Queued));
        assert_eq!(i.on_request(InsightKind::Quotes, Ok(queued)), Next::Poll(backoff(0)));
        let mut done = data(MinutesState::NotRequested);
        done.quotes = Some(InsightSection::new(MinutesState::Succeeded));
        let item = TranslationItem {
            item_id: "a".into(),
            revision_id: "r".into(),
            summary_state: memex_client::AnalysisState::NotRequested,
            summary: None,
            translation_state: memex_client::AnalysisState::NotRequested,
            title: None,
            markdown: None,
            translation_reason: None,
            ai_minutes: None,
            ai_insights: Some(done.clone()),
            translated_title: None,
            translation_progress: None,
            title_source: None,
            translation_failure_detail: None,
            untranslated_blocks: None,
        };
        assert_eq!(i.on_poll(Ok(item)), Next::Stop);
        assert_eq!(i.state(InsightKind::Quotes), ButtonState::Done);
        // Pressing the active view's button returns to the text; again shows it without a request.
        assert_eq!(i.press(InsightKind::Quotes), None);
        assert_eq!(i.view, View::Text);
        assert_eq!(i.press(InsightKind::Quotes), None);
        assert_eq!(i.view, View::Quotes);
        // 重点标注: done → a plain toggle, no request.
        i.reset("a", Some(data(MinutesState::Succeeded)));
        assert_eq!(i.view, View::Quotes, "a reload of the same item keeps the view");
        assert_eq!(i.press(InsightKind::Highlights), None);
        assert!(i.emphasis_shown());
        assert_eq!(i.press(InsightKind::Highlights), None);
        assert!(!i.emphasis);
        // Another item: back to the text, failures reported in words.
        let mut failed = data(MinutesState::NotRequested);
        failed.fact_check = Some(InsightSection { reason: Some("no_insight_provider".into()), ..InsightSection::new(MinutesState::Failed) });
        i.reset("b", Some(failed));
        assert!(matches!(i.state(InsightKind::FactCheck), ButtonState::Failed(r) if r.contains("分析模型") || r.contains("analysis")));
        assert_eq!(i.press(InsightKind::FactCheck), Some(InsightKind::FactCheck), "a failed kind can be asked again");
        assert!(matches!(i.on_request(InsightKind::FactCheck, Err(ClientError::Daemon("no_insight_provider".into()))), Next::Stop));
        assert!(matches!(i.state(InsightKind::FactCheck), ButtonState::Failed(_)));
    }

    #[test]
    fn changed_translation_shows_expired_highlights_without_marks_and_can_be_redone() {
        let client = Client::new(FixtureBackend::new(30));
        let mut i = Insights::default();
        i.set_capability(all());
        let read = client.library_item_read(memex_client::fixture::STALE_HIGHLIGHTS_ITEM).unwrap();
        i.reset(memex_client::fixture::STALE_HIGHLIGHTS_ITEM, read.ai_insights);
        assert!(matches!(i.state(InsightKind::Highlights), ButtonState::Stale(r) if r.contains("过期")));
        assert_eq!(i.state(InsightKind::Highlights).label(), "已过期");
        assert!(i.highlights().is_none() && !i.translation_marked(), "no old offsets are drawn");
        assert_eq!(i.press(InsightKind::Highlights), Some(InsightKind::Highlights));
        let next = i.on_request(InsightKind::Highlights, client.insight_request(memex_client::fixture::STALE_HIGHLIGHTS_ITEM, &[InsightKind::Highlights]));
        assert!(matches!(next, Next::Poll(_)));
        assert_eq!(i.state(InsightKind::Highlights), ButtonState::Generating);
        // `original_only`: done, but nothing on the translation.
        let read = client.library_item_read(memex_client::fixture::ORIGINAL_ONLY_HIGHLIGHTS_ITEM).unwrap();
        i.reset(memex_client::fixture::ORIGINAL_ONLY_HIGHLIGHTS_ITEM, read.ai_insights);
        assert_eq!(i.state(InsightKind::Highlights), ButtonState::Done);
        assert!(i.highlights().is_some() && !i.translation_marked());
        let read = client.library_item_read(SHOWCASE_ITEM).unwrap();
        i.reset(SHOWCASE_ITEM, read.ai_insights);
        assert!(i.translation_marked());
    }

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

    #[test]
    fn without_ai_insights_v1_the_buttons_are_hidden_and_nothing_is_sent() {
        let fixture = FixtureBackend::new(30);
        fixture.set_controls(FixtureControls { no_insights: true, ..Default::default() });
        let sent = Arc::new(Mutex::new(vec![]));
        let client = Client::new(Recording(fixture.clone(), sent.clone()));
        let caps = client.capabilities().unwrap();
        let mut i = Insights::default();
        i.set_capability(caps.insights);
        let read = client.library_item_read(SHOWCASE_ITEM).unwrap();
        assert!(read.ai_insights.is_none());
        i.reset(SHOWCASE_ITEM, read.ai_insights);
        assert!(!i.visible());
        assert_eq!(i.press(InsightKind::Quotes), None);
        assert_eq!(client.insight_request(SHOWCASE_ITEM, &[InsightKind::Quotes]), Err(ClientError::Unsupported));
        assert!(!sent.lock().unwrap().iter().any(|c| c == "insight_request"));
        // With the capability the request goes out and the kind starts generating.
        fixture.set_controls(FixtureControls::default());
        let client = Client::new(Recording(fixture, sent.clone()));
        let caps = client.capabilities().unwrap();
        i.set_capability(caps.insights);
        i.reset("item_fixture_000008", client.library_item_read("item_fixture_000008").unwrap().ai_insights);
        assert_eq!(i.press(InsightKind::Quotes), Some(InsightKind::Quotes));
        let next = i.on_request(InsightKind::Quotes, client.insight_request("item_fixture_000008", &[InsightKind::Quotes]));
        assert!(matches!(next, Next::Poll(_)));
        assert_eq!(i.state(InsightKind::Quotes), ButtonState::Generating);
        assert_eq!(sent.lock().unwrap().last().map(String::as_str), Some("insight_request"));
    }
}
