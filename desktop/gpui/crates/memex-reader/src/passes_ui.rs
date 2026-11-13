//! 阅读记录 on the Mac (approved draft): the 「第 n 遍 · 读完了？」 card in the
//! end zone with method, dates, 标记读完 / 稍后 / 补记一遍 and the history; the
//! ⏱ timing switch, pass time and 待同步 in the stats strip; and the 阅读统计
//! panel (merged active dwell, three modes, three rates, days). Behind
//! `reader_passes_v1` only; the timer rules live in `crate::timing`.

use super::*;
use crate::timing::{Clock, ReportFailure};
use gpui_kit::component::input::{Input, InputState};
use memex_client::passes::{
    date_label, dates_possible, days_in_month, local_timezone, local_today, method_label, minutes_label, ms_label, new_pass_id, parse_duration, rate_caption,
    rate_rows, refusal_message, session_gone, DurationUnit, Method, Pass, PassAttempt, PassChange, PassDate, PassKind, PassList, PassStats, PassWrite, Precision,
    TimingMode, WriteResult,
};
use std::time::Instant;

fn gold() -> Hsla { hsla(30. / 360., 0.78, 0.50, 1.) }
fn stone(dark: bool) -> Hsla { if dark { hsla(30. / 360., 0.08, 0.62, 1.) } else { hsla(30. / 360., 0.10, 0.58, 1.) } }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateField {
    Start,
    End,
}

/// One date being chosen: its precision and parts (a day is clamped to its month).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub precision: Precision,
    pub y: i32,
    pub m: u32,
    pub d: u32,
}

impl Draft {
    pub fn today() -> Self {
        let (y, m, d) = local_today();
        Draft { precision: Precision::Day, y, m, d }
    }
    pub fn of(date: Option<&PassDate>) -> Self {
        match date.and_then(PassDate::parts) {
            Some((y, m, d)) => Draft { precision: date.map_or(Precision::Day, |d| d.precision), y, m: m.unwrap_or(1), d: d.unwrap_or(1) },
            None => Self::today(),
        }
    }
    pub fn date(&self) -> Option<PassDate> {
        match self.precision {
            Precision::Year => PassDate::new(self.y, None, None),
            Precision::Month => PassDate::new(self.y, Some(self.m), None),
            _ => PassDate::new(self.y, Some(self.m), Some(self.d)),
        }
    }
    /// Moves one part by `by`; the day stays within its month.
    pub fn step(&mut self, part: u8, by: i32) {
        match part {
            0 => self.y = (self.y + by).clamp(1900, local_today().0 + 1),
            1 => self.m = ((self.m as i32 - 1 + by).rem_euclid(12) + 1) as u32,
            _ => self.d = ((self.d as i32 - 1 + by).rem_euclid(days_in_month(self.y, self.m) as i32) + 1) as u32,
        }
        self.d = self.d.min(days_in_month(self.y, self.m));
    }
}

pub struct Backfill {
    pub method: Option<Method>,
    pub start: Draft,
    pub end: Draft,
    pub amount: Entity<InputState>,
    pub unit: DurationUnit,
    pub error: Option<String>,
}

pub struct StatsPanel {
    pub pass_id: String,
    pub stats: Option<Result<PassStats, String>>,
}

pub struct Passes {
    /// `reader_passes_v1` advertised (None: not yet probed).
    pub supported: Option<bool>,
    pub item: Option<(String, String)>,
    pub list: Option<PassList>,
    pub error: Option<String>,
    pub busy: bool,
    pub notice: Option<String>,
    /// A write whose outcome is unknown, kept exactly for 重新核对 / 重试同一请求.
    pub unknown: Option<PassAttempt>,
    pub method: Option<Method>,
    pub start: Option<PassDate>,
    pub end: Option<PassDate>,
    pub editing: Option<(DateField, Draft)>,
    pub later: bool,
    /// The pass just marked read (the card shows 「第 n 遍已读完」).
    pub done: Option<String>,
    pub backfill: Option<Backfill>,
    pub stats: Option<StatsPanel>,
    /// ⏱ on (the default; off only by hand, remembered). Opening an item creates
    /// nothing: the first real activity creates or continues the tracked pass.
    pub pref: bool,
    pub clock: Clock,
    /// The one tracked pass the timer targets.
    pub target: Option<String>,
    pub starting: bool,
    pub reporting: bool,
    retry_at: Option<Instant>,
    ticker: u64,
    ticket: u64,
    /// The reading view is on screen in the shell (not the classic view).
    pub view_shown: bool,
    shown: bool,
    /// First local open date of each item this session: a proposal, not a measurement.
    opened_on: HashMap<String, PassDate>,
    shown_key: (u64, u64),
}

impl Default for Passes {
    fn default() -> Self {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        Passes {
            supported: None, item: None, list: None, error: None, busy: false, notice: None, unknown: None, method: None, start: None, end: None, editing: None,
            later: false, done: None, backfill: None, stats: None, pref: true, clock: Clock::new(format!("mac{nanos:x}")), target: None, starting: false,
            reporting: false, retry_at: None, ticker: 0, ticket: 0, view_shown: true, shown: true, opened_on: HashMap::new(), shown_key: (0, 0),
        }
    }
}

pub fn timing_mode(mode: Mode) -> TimingMode {
    match mode {
        Mode::Original => TimingMode::Source,
        Mode::Bilingual => TimingMode::Bilingual,
        Mode::Translation => TimingMode::Translation,
    }
}

impl Reader {
    fn this_year() -> i32 { local_today().0 }

    /// The card's row changed height: measure the last list row again.
    fn pass_changed(&mut self, cx: &mut Context<Self>) {
        let n = self.list.item_count();
        if n > 0 { self.list.remeasure_items(n - 1..n); }
        cx.notify();
    }

    /// Leaves the item: stop timing and send what is left (best effort, same ids).
    pub(super) fn pass_reset(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.passes.clock.stop(now);
        if let (Some((item, _)), Some(pass), Some((session, segments))) = (self.passes.item.clone(), self.passes.target.clone(), self.passes.clock.report()) {
            let client = self.client.clone();
            cx.background_executor().spawn(async move { let _ = client.passes_timing_report(&item, &pass, &session, &segments); }).detach();
        }
        let p = &mut self.passes;
        let (pref, opened, view_shown) = (p.pref, std::mem::take(&mut p.opened_on), p.view_shown);
        let ticket = p.ticket + 1;
        let supported = p.supported;
        *p = Passes { pref, opened_on: opened, view_shown, ticket, ticker: p.ticker + 1, supported, ..Default::default() };
    }

    pub(super) fn pass_load(&mut self, item: String, revision: String, cx: &mut Context<Self>) {
        let today = Draft::today().date();
        self.passes.opened_on.entry(item.clone()).or_insert_with(|| today.clone().unwrap_or(PassDate { value: None, precision: Precision::Unknown, raw: None }));
        self.passes.item = Some((item.clone(), revision));
        let client = self.client.clone();
        let ticket = self.passes.ticket;
        let task = cx.background_executor().spawn(async move {
            let supported = client.passes_supported();
            (supported, supported.then(|| client.passes_list(&item)))
        });
        cx.spawn(async move |this, cx| {
            let (supported, list) = task.await;
            this.update(cx, |this, cx| {
                if this.passes.ticket != ticket { return; }
                this.passes.supported = Some(supported);
                match list {
                    Some(Ok(list)) => this.pass_take_list(list, true, cx),
                    Some(Err(e)) => this.passes.error = Some(txf("阅读记录读取失败：{0}", &[&e.message()])),
                    None => {}
                }
                this.pass_changed(cx);
            }).ok();
        }).detach();
    }

    /// A list read: the timer continues an unfinished tracked pass of this revision
    /// (or, with none, creates one at the first real activity).
    fn pass_take_list(&mut self, list: PassList, first: bool, cx: &mut Context<Self>) {
        let rev = self.passes.item.as_ref().map(|i| i.1.clone()).unwrap_or_default();
        let open = list.open_tracked(&rev).cloned();
        if first {
            self.passes.target = open.as_ref().map(|p| p.pass_id.clone());
            self.pass_reset_draft(open.as_ref());
            self.passes.list = Some(list);
            if self.passes.pref { self.pass_enable(false, cx); }
            return;
        } else if self.passes.target.as_ref().is_some_and(|t| list.get(t).is_none_or(|p| !p.in_progress())) {
            // Completed or deleted elsewhere: this timer stops.
            self.passes.clock.stop(Instant::now());
            self.passes.target = None;
        }
        self.passes.list = Some(list);
    }

    fn pass_reset_draft(&mut self, open: Option<&Pass>) {
        let item = self.passes.item.as_ref().map(|i| i.0.clone()).unwrap_or_default();
        self.passes.method = open.and_then(|p| p.method);
        self.passes.start = open.and_then(|p| p.start_date.clone()).or_else(|| self.passes.opened_on.get(&item).cloned().filter(|d| d.value.is_some()));
        self.passes.end = Draft::today().date();
        self.passes.editing = None;
    }

    pub fn restore_pass_timing(&mut self, on: bool) {
        self.passes.pref = on;
    }

    /// The window came to the front or left it (unfocused, minimized, asleep).
    pub fn pass_foreground(&mut self, active: bool, cx: &mut Context<Self>) {
        self.passes.clock.set_foreground(active, Instant::now());
        cx.notify();
    }

    /// The shell shows the reading view (or another one in its place).
    pub fn pass_view_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.passes.view_shown = shown;
        self.pass_sync_shown();
        cx.notify();
    }

    fn pass_sync_shown(&mut self) {
        let shown = self.passes.view_shown && self.insights.view == View::Text && !self.editing.visible && self.compare.is_none() && self.model.detail().is_some();
        if shown != self.passes.shown {
            self.passes.shown = shown;
            self.passes.clock.set_shown(shown, Instant::now());
        }
    }

    /// A real scroll, click or key in the reading area.
    pub(super) fn pass_activity(&mut self, cx: &mut Context<Self>) {
        if !self.passes.clock.enabled { return; }
        self.pass_sync_shown();
        self.passes.clock.activity(Instant::now());
        if self.passes.clock.wants_session(Instant::now()) { self.pass_tick(cx); }
    }

    pub(super) fn pass_mode(&mut self, mode: Mode) {
        self.passes.clock.set_mode(timing_mode(mode), Instant::now());
    }

    /// Timing on: by a click on ⏱ (itself a real interaction), or automatically
    /// when the item opens (nothing accrues or is sent before a real activity).
    fn pass_enable(&mut self, clicked: bool, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.passes.clock.set_mode(timing_mode(self.model.mode()), now);
        self.passes.clock.set_enabled(true, now);
        self.pass_sync_shown();
        if clicked { self.passes.clock.activity(now); }
        self.passes.ticker += 1;
        let ticker = self.passes.ticker;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let alive = this.update(cx, |this, cx| {
                if this.passes.ticker != ticker || !this.passes.clock.enabled && !this.passes.clock.has_unsent() { return false; }
                this.pass_tick(cx);
                true
            }).unwrap_or(false);
            if !alive { break; }
        }).detach();
        self.pass_tick(cx);
    }

    fn pass_tick(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.pass_sync_shown();
        self.passes.clock.tick(now);
        let backoff = self.passes.retry_at.is_some_and(|t| now < t);
        if !backoff && !self.passes.starting && self.passes.clock.wants_session(now) {
            if self.passes.target.is_some() { self.pass_start_session(cx); } else { self.pass_create_tracked(false, cx); }
        }
        if !backoff && !self.passes.reporting && self.passes.clock.has_unsent() { self.pass_report(cx); }
        let key = (self.passes.clock.pending_ms(now) / 30_000, self.passes.clock.lost_ms / 30_000);
        if key != self.passes.shown_key { self.passes.shown_key = key; cx.notify(); }
    }

    fn pass_start_session(&mut self, cx: &mut Context<Self>) {
        let (Some((item, _)), Some(pass)) = (self.passes.item.clone(), self.passes.target.clone()) else { return };
        self.passes.starting = true;
        let client = self.client.clone();
        let ticket = self.passes.ticket;
        let task = cx.background_executor().spawn(async move { let r = client.passes_timing_start(&item, &pass); (r, Instant::now()) });
        cx.spawn(async move |this, cx| {
            let (result, received) = task.await;
            this.update(cx, |this, cx| {
                if this.passes.ticket != ticket { return; }
                this.passes.starting = false;
                match result {
                    PassWrite::Done(session) if this.passes.target.as_deref() == Some(session.pass_id.as_str()) => { this.passes.retry_at = None; this.passes.clock.attach(&session, received); }
                    PassWrite::Done(_) => {}
                    PassWrite::Rejected(code) => {
                        // Closed, not timeable, another revision: this pass takes no more time.
                        this.passes.clock.stop(Instant::now());
                        this.passes.notice = Some(refusal_message(&code));
                        this.pass_changed(cx);
                    }
                    PassWrite::Stale | PassWrite::Unknown(_) => this.passes.retry_at = Some(Instant::now() + Duration::from_secs(5)),
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    fn pass_report(&mut self, cx: &mut Context<Self>) {
        let (Some((item, _)), Some(pass)) = (self.passes.item.clone(), self.passes.target.clone()) else { return };
        let Some((session, segments)) = self.passes.clock.report() else { return };
        self.passes.reporting = true;
        let client = self.client.clone();
        let ticket = self.passes.ticket;
        let task = cx.background_executor().spawn(async move { client.passes_timing_report(&item, &pass, &session, &segments) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.passes.ticket != ticket { return; }
                this.passes.reporting = false;
                match result {
                    PassWrite::Done(report) => {
                        this.passes.clock.acknowledged();
                        this.passes.retry_at = None;
                        if let Some(p) = this.passes.list.as_mut().and_then(|l| l.passes.iter_mut().find(|p| p.pass_id == report.pass_id)) { p.timing = Some(report.totals); }
                    }
                    PassWrite::Rejected(code) if session_gone(&code) => this.passes.clock.report_failed(ReportFailure::SessionGone),
                    PassWrite::Rejected(code) => {
                        this.passes.clock.report_failed(ReportFailure::Refused);
                        this.passes.notice = Some(refusal_message(&code));
                    }
                    PassWrite::Stale | PassWrite::Unknown(_) => {
                        this.passes.clock.report_failed(ReportFailure::Retry);
                        this.passes.retry_at = Some(Instant::now() + Duration::from_secs(5));
                    }
                }
                this.pass_changed(cx);
            }).ok();
        }).detach();
    }

    /// ⏱: on targets the unfinished pass, or starts one (a confirmed start); off stops.
    pub fn pass_toggle_timing(&mut self, cx: &mut Context<Self>) {
        if self.passes.clock.enabled {
            self.passes.clock.set_enabled(false, Instant::now());
            self.passes.pref = false;
            cx.emit(ReaderEvent::PassTimingChanged(false));
            self.pass_tick(cx);
            self.pass_changed(cx);
            return;
        }
        self.passes.pref = true;
        cx.emit(ReaderEvent::PassTimingChanged(true));
        if self.passes.target.is_some() { self.pass_enable(true, cx); self.pass_changed(cx); return; }
        self.pass_create_tracked(true, cx);
    }

    /// The tracked pass the timer needs: on a click on ⏱, or at the first real
    /// activity while timing is on (one at a time; not right after 标记读完).
    fn pass_create_tracked(&mut self, clicked: bool, cx: &mut Context<Self>) {
        let p = &self.passes;
        if p.supported != Some(true) || p.busy || p.unknown.is_some() || (p.done.is_some() && !clicked) || p.target.is_some() || p.list.is_none() { return; }
        let Some((_, rev)) = p.item.clone() else { return };
        let change = PassChange::CreateTracked { pass_id: new_pass_id(), revision_id: rev, method: p.method, start_date: p.start.clone(), timezone: local_timezone() };
        self.pass_write(vec![change], true, cx);
    }

    /// Sends `changes` in order as one intent (each with stale replay and read back).
    fn pass_write(&mut self, changes: Vec<PassChange>, then_time: bool, cx: &mut Context<Self>) {
        let (Some(item), Some(version)) = (self.passes.item.as_ref().map(|i| i.0.clone()), self.passes.list.as_ref().map(|l| l.version.clone())) else { return };
        self.passes.busy = true;
        self.passes.notice = None;
        let client = self.client.clone();
        let ticket = self.passes.ticket;
        let task = cx.background_executor().spawn(async move {
            let mut version = version;
            let mut last = None;
            for change in changes {
                let result = client.passes_write(&item, &version, &change);
                match &result {
                    WriteResult::Saved { list: Some(l), .. } => version = l.version.clone(),
                    WriteResult::Saved { .. } => {}
                    _ => return (result, Some(change)),
                }
                last = Some((result, change));
            }
            match last { Some((r, c)) => (r, Some(c)), None => (WriteResult::Saved { receipt: None, list: None }, None) }
        });
        cx.spawn(async move |this, cx| {
            let (result, change) = task.await;
            this.update(cx, |this, cx| {
                if this.passes.ticket != ticket { return; }
                this.pass_written(result, change, then_time, cx);
            }).ok();
        }).detach();
    }

    fn pass_written(&mut self, result: WriteResult, change: Option<PassChange>, then_time: bool, cx: &mut Context<Self>) {
        self.passes.busy = false;
        match result {
            WriteResult::Saved { list, .. } => {
                self.passes.unknown = None;
                match &change {
                    Some(PassChange::CreateTracked { pass_id, .. }) => {
                        self.passes.target = Some(pass_id.clone());
                        // Created at a real activity, the timer is already on; by ⏱, it starts now.
                        if then_time { if self.passes.clock.enabled { self.pass_tick(cx) } else { self.pass_enable(true, cx) } }
                    }
                    Some(PassChange::Complete { pass_id, .. }) => {
                        self.passes.done = Some(pass_id.clone());
                        self.passes.target = None;
                        self.passes.clock.stop(Instant::now());
                        self.passes.later = false;
                    }
                    Some(PassChange::CreateRetrospective { .. }) => self.passes.backfill = None,
                    _ => {}
                }
                if let Some(list) = list { self.pass_take_list(list, false, cx); }
            }
            WriteResult::Refused { code, list } => {
                // Nothing was written: the card shows the stored record again.
                if then_time { self.passes.clock.set_enabled(false, Instant::now()); }
                self.passes.notice = Some(refusal_message(&code));
                if let Some(list) = list { self.pass_take_list(list, false, cx); }
            }
            WriteResult::Unknown { attempt, message, list } => {
                // No pass to time until 重新核对 settles it (⏱ turns timing on again).
                if then_time { self.passes.clock.set_enabled(false, Instant::now()); }
                self.passes.notice = Some(txf("没能确定是否已保存（{0}）。可以重新核对，或重试同一请求", &[&message]));
                self.passes.unknown = Some(attempt);
                if let Some(list) = list { self.pass_take_list(list, false, cx); }
            }
        }
        self.pass_changed(cx);
    }

    /// 重新核对 (read back only) or 重试同一请求 (exactly the kept attempt).
    pub fn pass_settle_unknown(&mut self, resend: bool, cx: &mut Context<Self>) {
        let Some(attempt) = self.passes.unknown.clone() else { return };
        self.passes.busy = true;
        let client = self.client.clone();
        let ticket = self.passes.ticket;
        let change = attempt.change.clone();
        let task = cx.background_executor().spawn(async move {
            if resend { client.passes_resend(&attempt) } else { client.passes_reconcile(attempt, tx("仍未确定是否已保存").into()) }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| if this.passes.ticket == ticket { this.pass_written(result, Some(change), false, cx) }).ok();
        }).detach();
    }

    /// 标记读完: flush the timer, then (create and) complete with the dates shown.
    pub fn pass_mark_done(&mut self, cx: &mut Context<Self>) {
        let Some((item, rev)) = self.passes.item.clone() else { return };
        let Some(end) = self.passes.end.clone() else { return };
        if !dates_possible(self.passes.start.as_ref(), Some(&end)) { self.passes.notice = Some(tx("结束日期早于开始日期").into()); self.pass_changed(cx); return; }
        let now = Instant::now();
        self.passes.clock.set_enabled(false, now);
        let flush = self.passes.clock.report();
        let flushed = flush.is_some();
        let target = self.passes.target.clone().and_then(|t| self.passes.list.as_ref().and_then(|l| l.get(&t).cloned()));
        let (method, start) = (self.passes.method, self.passes.start.clone());
        let pass_id = target.as_ref().map_or_else(new_pass_id, |p| p.pass_id.clone());
        let Some(version) = self.passes.list.as_ref().map(|l| l.version.clone()) else { return };
        self.passes.busy = true;
        self.passes.notice = None;
        let client = self.client.clone();
        let ticket = self.passes.ticket;
        let task = cx.background_executor().spawn(async move {
            // What the timer still holds goes first; completion then closes timing.
            let mut unsynced = false;
            if let Some((session, segments)) = &flush {
                unsynced = !matches!(client.passes_timing_report(&item, &pass_id, session, segments), PassWrite::Done(_));
            }
            let first = match &target {
                None => Some(PassChange::CreateTracked { pass_id: pass_id.clone(), revision_id: rev.clone(), method, start_date: start.clone(), timezone: local_timezone() }),
                Some(p) if p.method != method || p.start_date != start => Some(PassChange::Update { pass_id: pass_id.clone(), method, start_date: start.clone(), end_date: None, estimated_duration_minutes: None, duration_display_unit: None }),
                Some(_) => None,
            };
            if let Some(change) = first {
                match client.passes_write(&item, &version, &change) {
                    WriteResult::Saved { .. } => {}
                    other => return (other, Some(change), unsynced),
                }
            }
            // The completion fences the timing version just read.
            let mut result = None;
            for _ in 0..2 {
                let list = match client.passes_list(&item) { Ok(l) => l, Err(e) => return (WriteResult::Refused { code: e.message(), list: None }, None, unsynced) };
                let tv = list.get(&pass_id).and_then(Pass::timing_version);
                let change = PassChange::Complete { pass_id: pass_id.clone(), end_date: Some(end.clone()), expected_timing_version: tv };
                let r = client.passes_write(&item, &list.version, &change);
                let again = matches!(&r, WriteResult::Refused { code, .. } if code == "stale_reader_pass_timing");
                result = Some((r, Some(change)));
                if !again { break; }
            }
            let (r, c) = result.unwrap();
            (r, c, unsynced)
        });
        cx.spawn(async move |this, cx| {
            let (result, change, unsynced) = task.await;
            this.update(cx, |this, cx| {
                if this.passes.ticket != ticket { return; }
                // The flushed segments were answered (or are lost): never left 待同步.
                if flushed { if unsynced { this.passes.clock.report_failed(ReportFailure::Refused) } else { this.passes.clock.acknowledged() } }
                this.pass_written(result, change, false, cx);
                if unsynced && this.passes.done.is_some() { this.passes.notice = Some(tx("已标记读完；最后一段计时没同步，未计入").into()); }
            }).ok();
        }).detach();
    }

    /// 开始第 n+1 遍: a fresh draft; the pass is created by the next real activity
    /// while timing is on, by ⏱ or by 标记读完.
    pub fn pass_again(&mut self, cx: &mut Context<Self>) {
        self.passes.done = None;
        self.pass_reset_draft(None);
        self.passes.start = Draft::today().date();
        // The next real activity creates the new pass.
        if self.passes.pref && self.passes.target.is_none() { self.pass_enable(false, cx); }
        self.pass_changed(cx);
    }

    pub fn pass_open_backfill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let amount = cx.new(|cx| InputState::new(window, cx).placeholder(tx("例如 2")));
        let (y, _, _) = local_today();
        self.passes.backfill = Some(Backfill {
            method: Some(Method::Full),
            start: Draft { precision: Precision::Month, y: y - 1, m: 3, d: 1 },
            end: Draft { precision: Precision::Month, y: y - 1, m: 4, d: 1 },
            amount,
            unit: DurationUnit::Hours,
            error: None,
        });
        self.pass_changed(cx);
    }

    /// 记下这一遍: three approximate values; nothing is rounded or filled in.
    pub fn pass_save_backfill(&mut self, cx: &mut Context<Self>) {
        let Some(b) = self.passes.backfill.as_mut() else { return };
        let (start, end) = (b.start.date(), b.end.date());
        let text = b.amount.read(cx).value().to_string();
        let minutes = match parse_duration(&text, b.unit) {
            Ok(m) => m,
            Err(e) => { b.error = Some(e.message().into()); return self.pass_changed(cx); }
        };
        if start.is_none() || end.is_none() || !dates_possible(start.as_ref(), end.as_ref()) {
            b.error = Some(tx("约结束早于约开始").into());
            return self.pass_changed(cx);
        }
        b.error = None;
        let change = PassChange::CreateRetrospective {
            pass_id: new_pass_id(), method: b.method, start_date: start, end_date: end, estimated_duration_minutes: Some(minutes), duration_display_unit: Some(b.unit),
            timezone: local_timezone(), reference_revision_id: self.passes.item.as_ref().map(|i| i.1.clone()),
        };
        self.pass_write(vec![change], false, cx);
    }

    pub fn pass_open_stats(&mut self, pass_id: Option<String>, cx: &mut Context<Self>) {
        let Some(list) = self.passes.list.as_ref() else { return };
        let pass_id = pass_id.or_else(|| self.passes.target.clone()).or_else(|| self.passes.done.clone()).or_else(|| list.history().first().map(|p| p.pass_id.clone()));
        let Some(pass_id) = pass_id else {
            self.passes.notice = Some(tx("还没有阅读记录：开启 ⏱ 或标记读完后才有统计").into());
            return self.pass_changed(cx);
        };
        let retro = list.get(&pass_id).is_some_and(|p| p.kind == PassKind::Retrospective);
        self.passes.stats = Some(StatsPanel { pass_id: pass_id.clone(), stats: None });
        cx.notify();
        if retro { return; }
        let Some((item, _)) = self.passes.item.clone() else { return };
        let client = self.client.clone();
        let ticket = self.passes.ticket;
        let task = cx.background_executor().spawn(async move { (client.passes_stats(&item, &pass_id).map_err(|e| e.message()), client.passes_list(&item).ok()) });
        cx.spawn(async move |this, cx| {
            let (stats, list) = task.await;
            this.update(cx, |this, cx| {
                if this.passes.ticket != ticket { return; }
                if let Some(list) = list { this.pass_take_list(list, false, cx); }
                if let Some(panel) = this.passes.stats.as_mut() { panel.stats = Some(stats); }
                cx.notify();
            }).ok();
        }).detach();
    }

    fn pass_rank_next(list: &PassList) -> u64 {
        list.passes.iter().filter(|p| p.start_date.as_ref().and_then(PassDate::parts).is_some()).count() as u64 + 1
    }

    // ----- rendering --------------------------------------------------------------

    fn pass_dur(p: &Pass) -> String {
        match (p.kind, p.estimated_duration_minutes) {
            (PassKind::Retrospective, Some(m)) => format!("{} {}", tx("约"), minutes_label(m as u64)),
            (PassKind::Retrospective, None) => tx("时长未知").into(),
            _ if p.unique_ms() == 0 => tx("未计时").into(),
            _ => ms_label(p.unique_ms()),
        }
    }

    fn pass_dates(p: &Pass) -> String {
        let year = Self::this_year();
        let end = if p.in_progress() { tx("进行中").to_string() } else { date_label(p.end_date.as_ref(), year) };
        let line = format!("{} → {end}", date_label(p.start_date.as_ref(), year));
        if p.kind == PassKind::Retrospective { format!("{} {line}", tx("约")) } else { line }
    }

    fn pass_pill(id: ElementId, label: &str, on: bool, p: &Palette, dark: bool) -> Stateful<Div> {
        div().id(id).px(px(10.)).py(px(3.)).rounded_full().text_xs().cursor_pointer()
            .when(on, |d| d.bg(p.rose).text_color(if dark { hsla(25. / 360., 0.12, 0.12, 1.) } else { hsla(36. / 360., 0.30, 0.98, 1.) }))
            .when(!on, |d| d.border_1().border_color(p.muted.opacity(0.35)).text_color(p.fg).hover(|d| d.bg(p.rose.opacity(0.08))))
            .child(label.to_string())
    }

    fn pass_stepper(&self, id: &'static str, text: String, part: u8, field: Option<DateField>, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let go = move |by: i32| move |r: &mut Reader, _: &ClickEvent, _: &mut Window, cx: &mut Context<Reader>| {
            match field {
                Some(f) => if let Some((_, d)) = r.passes.editing.as_mut().filter(|e| e.0 == f) { d.step(part, by) },
                None => {}
            }
            if field.is_none() {
                if let Some(b) = r.passes.backfill.as_mut() { if id.starts_with("bs") { b.start.step(part, by) } else { b.end.step(part, by) } }
            }
            r.pass_changed(cx);
        };
        h_flex().items_center().rounded(px(6.)).border_1().border_color(p.muted.opacity(0.3)).text_xs()
            .child(div().id(SharedString::from(format!("{id}-dec"))).px_1p5().cursor_pointer().text_color(p.muted).hover(|d| d.text_color(p.rose)).child("‹").on_click(cx.listener(go(-1))))
            .child(div().min_w(px(34.)).flex().justify_center().text_color(p.fg).child(text))
            .child(div().id(SharedString::from(format!("{id}-inc"))).px_1p5().cursor_pointer().text_color(p.muted).hover(|d| d.text_color(p.rose)).child("›").on_click(cx.listener(go(1))))
            .into_any_element()
    }

    fn pass_draft_row(&self, prefix: &'static str, d: &Draft, field: Option<DateField>, p: &Palette, cx: &mut Context<Self>) -> Div {
        let ids: [&'static str; 3] = match prefix { "bs" => ["bs-y", "bs-m", "bs-d"], "be" => ["be-y", "be-m", "be-d"], _ => ["ed-y", "ed-m", "ed-d"] };
        // Chinese suffixes; English shows the year, the month name and the day number.
        let en = memex_client::i18n::language() == memex_client::Lang::En;
        let month = |m: u32| if en { ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][(m as usize + 11) % 12].to_string() } else { format!("{m}月") };
        let mut row = h_flex().gap_1().items_center().child(self.pass_stepper(ids[0], if en { d.y.to_string() } else { format!("{}年", d.y) }, 0, field, p, cx));
        if d.precision != Precision::Year { row = row.child(self.pass_stepper(ids[1], month(d.m), 1, field, p, cx)); }
        if d.precision == Precision::Day { row = row.child(self.pass_stepper(ids[2], if en { d.d.to_string() } else { format!("{}日", d.d) }, 2, field, p, cx)); }
        row
    }

    fn pass_backfill_el(&self, p: &Palette, dark: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let b = self.passes.backfill.as_ref()?;
        let tint = p.rose.opacity(if dark { 0.14 } else { 0.07 });
        let label = |t: &str| div().w(px(52.)).flex_none().text_xs().text_color(p.muted).child(t.to_string());
        let mut methods = h_flex().gap_1p5().flex_wrap();
        for (k, m) in Method::ALL.into_iter().enumerate() {
            let on = b.method == Some(m);
            methods = methods.child(Self::pass_pill(ElementId::from(("bf-method", k)), m.label(), on, p, dark)
                .on_click(cx.listener(move |r, _, _, cx| { if let Some(b) = r.passes.backfill.as_mut() { b.method = if b.method == Some(m) { None } else { Some(m) }; } r.pass_changed(cx); })));
        }
        let precision = |which: &'static str, cur: Precision, cx: &mut Context<Self>| {
            let mut seg = h_flex().rounded(px(6.)).bg(p.muted.opacity(0.12)).p(px(1.));
            for (k, (pr, t)) in [(Precision::Year, tx("年")), (Precision::Month, tx("年月")), (Precision::Day, tx("年月日"))].into_iter().enumerate() {
                let on = cur == pr;
                seg = seg.child(div().id(ElementId::from((which, k))).px(px(6.)).py(px(1.)).rounded(px(5.)).text_xs().cursor_pointer()
                    .when(on, |d| d.bg(p.rose).text_color(white())).when(!on, |d| d.text_color(p.muted))
                    .child(t).on_click(cx.listener(move |r, _, _, cx| {
                        if let Some(b) = r.passes.backfill.as_mut() { if which == "bf-ps" { b.start.precision = pr } else { b.end.precision = pr } }
                        r.pass_changed(cx);
                    })));
            }
            seg
        };
        let unit = b.unit;
        let unit_seg = {
            let mut seg = h_flex().rounded(px(6.)).bg(p.muted.opacity(0.12)).p(px(1.));
            for (k, (u, t)) in [(DurationUnit::Minutes, tx("分钟")), (DurationUnit::Hours, tx("小时"))].into_iter().enumerate() {
                let on = unit == u;
                seg = seg.child(div().id(ElementId::from(("bf-unit", k))).px(px(6.)).py(px(1.)).rounded(px(5.)).text_xs().cursor_pointer()
                    .when(on, |d| d.bg(p.rose).text_color(white())).when(!on, |d| d.text_color(p.muted))
                    .child(t).on_click(cx.listener(move |r, _, _, cx| { if let Some(b) = r.passes.backfill.as_mut() { b.unit = u; } r.pass_changed(cx); })));
            }
            seg
        };
        let (start, end) = (b.start.clone(), b.end.clone());
        Some(v_flex().gap_2().p_2p5().rounded(px(10.)).bg(tint)
            .child(h_flex().items_baseline().gap_2().child(div().text_sm().text_color(p.fg).child(tx("补记一遍"))).child(div().ml_auto().text_xs().text_color(p.muted).child(tx("大约即可"))))
            .child(methods)
            .child(h_flex().gap_1p5().items_center().flex_wrap().child(label(tx("约开始"))).child(precision("bf-ps", start.precision, cx)).child(self.pass_draft_row("bs", &start, None, p, cx)))
            .child(h_flex().gap_1p5().items_center().flex_wrap().child(label(tx("约结束"))).child(precision("bf-pe", end.precision, cx)).child(self.pass_draft_row("be", &end, None, p, cx)))
            .child(h_flex().gap_1p5().items_center().child(label(tx("约时长"))).child(div().w(px(72.)).child(Input::new(&b.amount).small())).child(unit_seg))
            .when_some(b.error.clone(), |d, e| d.child(div().text_xs().text_color(p.neg).child(e)))
            .child(h_flex().gap_2()
                .child(Button::new("pass-backfill-save").small().primary().label(tx("记下这一遍")).disabled(self.passes.busy).on_click(cx.listener(|r, _, _, cx| r.pass_save_backfill(cx))))
                .child(Button::new("pass-backfill-cancel").small().outline().label(tx("取消")).on_click(cx.listener(|r, _, _, cx| { r.passes.backfill = None; r.pass_changed(cx); }))))
            .into_any_element())
    }

    fn pass_history(&self, list: &PassList, p: &Palette, dark: bool, cx: &mut Context<Self>) -> Div {
        let mut hist = v_flex().text_xs();
        for (k, pass) in list.history().into_iter().enumerate() {
            let id = pass.pass_id.clone();
            let retro = pass.kind == PassKind::Retrospective;
            hist = hist.child(h_flex().id(ElementId::from(("pass-row", k))).flex_wrap().gap_x_2().gap_y_0p5().items_center().py_1p5().border_t_1().border_color(p.muted.opacity(0.18))
                .cursor_pointer().hover(|d| d.bg(p.rose.opacity(0.05)))
                .tooltip(|window, cx| Tooltip::new(tx("按填写日期排序的第 n 遍；点开看这一遍的统计")).build(window, cx))
                .on_click(cx.listener(move |r, _, _, cx| r.pass_open_stats(Some(id.clone()), cx)))
                .child(div().w(px(46.)).text_color(p.muted).child(txf("第 {0} 遍", &[&pass.display_rank])))
                .child(div().px(px(7.)).py(px(1.)).rounded_full().bg(p.rose.opacity(if dark { 0.2 } else { 0.1 })).text_color(p.fg).child(method_label(pass.method)))
                .when(retro, |d| d.child(div().px(px(7.)).py(px(1.)).rounded_full().border_1().border_color(gold()).text_color(gold()).child(tx("补记"))))
                .when(pass.in_progress(), |d| d.child(div().text_color(p.rose).child(tx("进行中"))))
                .when(pass.source_status.as_deref() == Some("changed"), |d| d.child(div().text_color(p.muted).child(tx("原文已更新"))))
                .child(div().w_full().text_color(p.muted).child(format!("{} · {}", Self::pass_dates(pass), Self::pass_dur(pass)))));
        }
        hist
    }

    /// The card under 正文结尾 (`None` without `reader_passes_v1`; a line says so).
    pub(super) fn pass_card(&mut self, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dark = cx.theme().background.l < 0.5;
        let card = v_flex().gap_2p5().p_3().mb_3().rounded(px(12.)).border_1().border_color(p.muted.opacity(0.25)).text_sm();
        match self.passes.supported {
            None => return None,
            Some(false) => return Some(div().mb_3().text_xs().text_color(p.muted).child(txf("阅读记录：这台 Mac 上的 {0} 代理还不支持（需要 reader_passes_v1），读完标记和计时暂不可用", &[&memex_client::BRAND_NAME])).into_any_element()),
            Some(true) => {}
        }
        let Some(list) = self.passes.list.clone() else {
            return Some(card.child(div().text_xs().text_color(p.muted).child(self.passes.error.clone().unwrap_or_else(|| tx("正在读取阅读记录…").into()))).into_any_element());
        };
        let year = Self::this_year();
        let busy = self.passes.busy;
        let notice = self.passes.notice.clone().map(|n| {
            h_flex().gap_2().flex_wrap().text_xs().text_color(p.muted).child(n)
                .when(self.passes.unknown.is_some(), |d| d
                    .child(Button::new("pass-reconcile").xsmall().outline().label(tx("重新核对")).disabled(busy).on_click(cx.listener(|r, _, _, cx| r.pass_settle_unknown(false, cx))))
                    .child(Button::new("pass-resend").xsmall().outline().label(tx("重试同一请求")).disabled(busy).on_click(cx.listener(|r, _, _, cx| r.pass_settle_unknown(true, cx)))))
        });
        let backfill_btn = self.passes.backfill.is_none().then(|| Button::new("pass-backfill").small().outline().label(tx("补记一遍")).on_click(cx.listener(|r, _, window, cx| r.pass_open_backfill(window, cx))));
        let backfill = self.pass_backfill_el(p, dark, cx);
        let history = self.pass_history(&list, p, dark, cx);
        if let Some(done) = self.passes.done.clone().and_then(|d| list.get(&d).cloned()) {
            let next = Self::pass_rank_next(&list);
            return Some(card
                .child(h_flex().items_baseline().gap_2().child(div().text_color(p.fg).child(txf("第 {0} 遍已读完", &[&done.display_rank]))).child(div().ml_auto().text_xs().text_color(p.muted).child(method_label(done.method))))
                .child(h_flex().gap_2().flex_wrap()
                    .child(Button::new("pass-again").small().primary().label(txf("开始第 {0} 遍", &[&next])).on_click(cx.listener(|r, _, _, cx| r.pass_again(cx))))
                    .child(Button::new("pass-done-stats").small().outline().label(tx("看这一遍的统计")).on_click(cx.listener(move |r, _, _, cx| r.pass_open_stats(Some(done.pass_id.clone()), cx))))
                    .children(backfill_btn))
                .children(notice).children(backfill).child(history).into_any_element());
        }
        let target = self.passes.target.clone().and_then(|t| list.get(&t).cloned());
        let n = target.as_ref().map_or_else(|| Self::pass_rank_next(&list), |t| t.display_rank);
        if self.passes.later {
            return Some(h_flex().gap_2().mb_3().text_xs().text_color(p.muted).child(txf("第 {0} 遍 · 读完了？", &[&n]))
                .child(Button::new("pass-expand").xsmall().ghost().label(tx("展开")).on_click(cx.listener(|r, _, _, cx| { r.passes.later = false; r.pass_changed(cx); })))
                .into_any_element());
        }
        let dwell = if self.passes.clock.enabled || target.as_ref().is_some_and(|t| t.unique_ms() > 0) {
            txf("活跃停留 {0}", &[&target.as_ref().map_or_else(|| ms_label(0), |t| ms_label(t.unique_ms()))])
        } else { tx("未计时").into() };
        let mut methods = h_flex().gap_1p5().flex_wrap();
        for (k, m) in Method::ALL.into_iter().enumerate() {
            let on = self.passes.method == Some(m);
            methods = methods.child(Self::pass_pill(ElementId::from(("pass-method", k)), m.label(), on, p, dark)
                .on_click(cx.listener(move |r, _, _, cx| { r.passes.method = if r.passes.method == Some(m) { None } else { Some(m) }; r.pass_changed(cx); })));
        }
        let chip = |id: &'static str, d: Option<&PassDate>, field: DateField, cx: &mut Context<Self>| {
            let current = d.cloned();
            div().id(id).px(px(7.)).py(px(2.)).rounded(px(6.)).bg(p.rose.opacity(if dark { 0.18 } else { 0.09 })).text_color(p.fg).cursor_pointer()
                .child(date_label(d, year))
                .on_click(cx.listener(move |r, _, _, cx| { r.passes.editing = Some((field, Draft { precision: Precision::Day, ..Draft::of(current.as_ref()) })); r.pass_changed(cx); }))
        };
        let editor = self.passes.editing.clone().map(|(field, d)| {
            h_flex().gap_2().items_center().text_xs()
                .child(div().text_color(p.muted).child(if field == DateField::Start { tx("起") } else { tx("止") }))
                .child(self.pass_draft_row("ed", &d, Some(field), p, cx))
                .child(Button::new("pass-date-ok").xsmall().primary().label(tx("好")).on_click(cx.listener(|r, _, _, cx| {
                    if let Some((field, d)) = r.passes.editing.take() { match field { DateField::Start => r.passes.start = d.date(), DateField::End => r.passes.end = d.date() } }
                    r.pass_changed(cx);
                })))
                .child(Button::new("pass-date-cancel").xsmall().ghost().label(tx("取消")).on_click(cx.listener(|r, _, _, cx| { r.passes.editing = None; r.pass_changed(cx); })))
        });
        Some(card
            .child(h_flex().items_baseline().gap_2().child(div().text_color(p.fg).child(txf("第 {0} 遍 · 读完了？", &[&n]))).child(div().ml_auto().text_xs().text_color(p.muted).child(dwell)))
            .child(methods)
            .child(h_flex().gap_1p5().items_center().text_xs().text_color(p.muted)
                .child(tx("起")).child(chip("pass-start-date", self.passes.start.as_ref(), DateField::Start, cx))
                .child("→").child(tx("止")).child(chip("pass-end-date", self.passes.end.as_ref(), DateField::End, cx))
                .child(div().ml_auto().child(tx("点日期可改"))))
            .when(self.passes.method.is_none(), |d| d.child(div().text_xs().text_color(p.muted).child(tx("阅读方式可不选，之后再补"))))
            .children(editor)
            .child(h_flex().gap_2().flex_wrap()
                .child(Button::new("pass-mark-done").small().primary().label(tx("标记读完")).disabled(busy).on_click(cx.listener(|r, _, _, cx| r.pass_mark_done(cx))))
                .child(Button::new("pass-later").small().outline().label(tx("稍后")).on_click(cx.listener(|r, _, _, cx| { r.passes.later = true; r.pass_changed(cx); })))
                .children(backfill_btn))
            .children(notice).children(backfill).child(history).into_any_element())
    }

    /// ⏱ 计时中/计时关 · 第 n 遍 · time · n 分待同步 · 阅读统计, in the stats strip.
    pub(super) fn pass_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.passes.supported != Some(true) || self.model.book().is_some() { return None; }
        let list = self.passes.list.as_ref()?;
        let theme = cx.theme().clone();
        let on = self.passes.clock.enabled;
        let rose = theme.primary;
        let target = self.passes.target.as_ref().and_then(|t| list.get(t));
        let n = self.passes.done.as_ref().and_then(|d| list.get(d)).or(target).map_or_else(|| Self::pass_rank_next(list), |p| p.display_rank);
        let now = Instant::now();
        let pending = self.passes.clock.pending_ms(now);
        let lost = self.passes.clock.lost_ms;
        Some(h_flex().gap_1p5().items_center()
            .child(div().id("pass-timing-toggle").px(px(7.)).py(px(1.)).rounded_full().border_1().cursor_pointer()
                .border_color(if on { rose } else { theme.border }).text_color(if on { rose } else { theme.muted_foreground })
                .tooltip(move |window, cx| Tooltip::new(tx(if on { "点击关闭计时" } else { "点击开启计时：只在窗口在前台、2 分钟内有滚动/点击/按键时累计" })).build(window, cx))
                .child(if on { tx("⏱ 计时中") } else { tx("⏱ 计时关") })
                .on_click(cx.listener(|r, _, _, cx| r.pass_toggle_timing(cx))))
            .child(txf("第 {0} 遍", &[&n]))
            .when_some(target.filter(|_| on || pending > 0).map(|t| ms_label(t.unique_ms())), |d, t| d.child("·").child(t))
            // Only closed time the daemon has not acknowledged; the open stretch is not news.
            .when(self.passes.clock.has_unsent() && pending > 0, |d| d.child("·").child(div().text_color(gold()).child(txf("{0}待同步", &[&ms_label(pending)]))))
            .when(lost >= 30_000, |d| d.child("·").child(div().text_color(gold()).child(txf("{0}未同步", &[&ms_label(lost)]))))
            .child("·")
            .child(div().id("pass-open-stats").cursor_pointer().text_color(rose).child(tx("阅读统计")).on_click(cx.listener(|r, _, _, cx| r.pass_open_stats(None, cx))))
            .into_any_element())
    }

    /// 阅读统计 as a floating panel over the reader.
    pub(super) fn pass_stats_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = self.passes.stats.as_ref()?;
        let list = self.passes.list.as_ref()?;
        let pass = list.get(&panel.pass_id)?.clone();
        let theme = cx.theme().clone();
        let dark = theme.background.l < 0.5;
        let (fg, muted, rose) = (theme.foreground, theme.muted_foreground, theme.primary);
        let year = Self::this_year();
        let retro = pass.kind == PassKind::Retrospective;
        let lab = |t: String| div().text_xs().text_color(muted).mb_1p5().child(t);
        let pending = if self.passes.target.as_deref() == Some(pass.pass_id.as_str()) && self.passes.clock.has_unsent() { self.passes.clock.pending_ms(Instant::now()) } else { 0 };
        let t = pass.timing.clone().unwrap_or_default();
        let colors = [(tx("原文"), stone(dark), t.source_ms), (tx("对照"), rose, t.bilingual_ms), (tx("译文"), gold(), t.translation_ms)];
        let mut body = v_flex().gap_3();
        let total = if retro { Self::pass_dur(&pass) } else { ms_label(t.unique_ms) };
        body = body.child(v_flex()
            .child(lab(if retro { tx("补记的约数时长（不是设备计时）").into() } else { tx("设备上报的活跃停留（Mac 和手机合并，重叠只算一次）").into() }))
            .child(div().text_xl().text_color(fg).child(if t.unique_ms == 0 && !retro { tx("还没有计时").to_string() } else { total.clone() }))
            .when(pending > 0, |d| d.child(div().text_xs().text_color(muted).child(txf("另有 {0}在本机待同步，未计入", &[&ms_label(pending)]))))
            .when(t.overlap_excluded_ms >= 30_000, |d| d.child(div().text_xs().text_color(muted).child(txf("两台设备重叠的 {0}只算一次", &[&ms_label(t.overlap_excluded_ms)])))));
        if !retro && t.unique_ms > 0 {
            let mut bar = h_flex().h(px(8.)).w_full().rounded(px(4.)).overflow_hidden().gap(px(2.));
            let mut legend = h_flex().gap_3().flex_wrap().text_xs().text_color(muted);
            for (label, color, ms) in colors.iter().cloned() {
                if ms > 0 { bar = bar.child(div().h_full().flex_basis(px(0.)).flex_grow(ms as f32 / t.unique_ms as f32).bg(color)); }
                legend = legend.child(h_flex().gap_1().items_center().child(div().size(px(8.)).rounded(px(2.)).bg(color)).child(format!("{label} {}", ms_label(ms))));
            }
            body = body.child(v_flex().gap_1p5().child(bar).child(legend));
        }
        let caption = rate_caption(&pass);
        let minutes = pass.rates.as_ref().map(|r| r.minutes);
        let head = match minutes {
            Some(m) => txf("同一时长 {0} 的三种速率", &[&if retro { format!("{} {}", tx("约"), minutes_label(m.round() as u64)) } else { ms_label(t.unique_ms) }]),
            None => tx("三种速率").into(),
        };
        let mut table = v_flex().text_xs();
        for row in rate_rows(&pass) {
            table = table.child(h_flex().py_1().border_t_1().border_color(theme.border).gap_2()
                .child(div().w(px(56.)).text_color(fg).child(row.label))
                .child(div().flex_1().text_right().text_color(muted).child(row.length))
                .child(div().w(px(150.)).text_right().text_color(fg).child(row.rate)));
        }
        body = body.child(v_flex()
            .child(lab(match &caption { Some(c) => format!("{head} · {c}"), None => head }))
            .child(table)
            .when(pass.rates.is_none(), |d| d.child(div().pt_1().text_xs().text_color(muted).child(tx("没有时长或没有可用的全文长度时显示「—」")))));
        if !retro {
            let days = match &panel.stats {
                None => div().text_xs().text_color(muted).child(tx("正在读取…")).into_any_element(),
                Some(Err(e)) => div().text_xs().text_color(muted).child(e.clone()).into_any_element(),
                Some(Ok(s)) if s.days.is_empty() => div().text_xs().text_color(muted).child(tx("还没有每天的数据")).into_any_element(),
                Some(Ok(s)) => {
                    let max = s.days.iter().map(|d| d.ms).max().unwrap_or(1).max(1);
                    let mut bars = h_flex().items_end().gap(px(5.)).h(px(104.)).mt_2();
                    for day in s.days.iter().rev().take(14).rev() {
                        let parts: Vec<u32> = day.date.split('-').filter_map(|x| x.parse().ok()).collect();
                        let label = match parts[..] { [y, m, d] => date_label(PassDate::day(y as i32, m, d).as_ref(), year), _ => day.date.clone() };
                        bars = bars.child(v_flex().flex_1().items_center().justify_end().gap(px(3.)).text_size(px(10.)).line_height(px(13.)).text_color(muted)
                            .child(div().w_full().rounded_t(px(3.)).bg(rose).h(px((day.ms as f32 / max as f32 * 56.).max(2.))))
                            .child(label).child(ms_label(day.ms)));
                    }
                    bars.into_any_element()
                }
            };
            body = body.child(v_flex().child(lab(tx("每天活跃停留").into())).child(days));
        }
        body = body.child(div().text_xs().text_color(muted).line_height(relative(1.5)).child(tx(if retro {
            "补记的遍只有约数时长，没有分模式和每日数据，不进每日柱状图。"
        } else {
            "阅读界面在前台时自动计时（⏱ 可关闭）；只在窗口在前台且 2 分钟内有操作时累计，是设备上报的活跃停留，不等于专注时间。没有译文时，译文与对照速率显示「—」。补记的遍只有约数时长，不进每日柱状图。"
        })));
        let title = txf("阅读统计 · 第 {0} 遍", &[&pass.display_rank]);
        let sub = format!("{} · {}{}", method_label(pass.method), Self::pass_dates(&pass), if retro { format!(" · {}", tx("补记")) } else { String::new() });
        Some(div().absolute().right(px(28.)).bottom(px(56.)).w(px(400.)).max_h(px(560.)).occlude()
            .bg(theme.background).rounded(px(14.)).border_1().border_color(theme.border).shadow_lg()
            .child(v_flex().id("pass-stats-scroll").max_h(px(560.)).overflow_y_scroll().p_4().gap_3()
                .child(h_flex().items_center().gap_2()
                    .child(v_flex().child(div().text_sm().text_color(fg).child(title)).child(div().text_xs().text_color(muted).child(sub)))
                    .child(div().ml_auto().child(Button::new("pass-stats-close").small().outline().label(tx("关闭")).on_click(cx.listener(|r, _, _, cx| { r.passes.stats = None; cx.notify(); })))))
                .child(body))
            .into_any_element())
    }

    // ----- development walk accessors -------------------------------------------

    pub fn dev_passes(&self) -> String {
        let now = Instant::now();
        format!(
            "supported={:?} passes={} target={:?} done={:?} timing={} session={:?} pending_ms={} lost_ms={} unknown={} notice={:?} stats={:?} backfill={}",
            self.passes.supported, self.passes.list.as_ref().map_or(0, |l| l.passes.len()), self.passes.target, self.passes.done, self.passes.clock.enabled,
            self.passes.clock.session_id(), self.passes.clock.pending_ms(now), self.passes.clock.lost_ms, self.passes.unknown.is_some(), self.passes.notice,
            self.passes.stats.as_ref().map(|s| s.pass_id.clone()), self.passes.backfill.is_some()
        )
    }

    pub fn dev_pass_activity(&mut self, cx: &mut Context<Self>) { self.pass_activity(cx); }
    pub fn dev_forget_capabilities(&mut self) { self.client.forget_capabilities(); self.passes.supported = None; }

    pub fn dev_backfill(&mut self, amount: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.passes.backfill.is_none() { self.pass_open_backfill(window, cx); }
        if let Some(b) = self.passes.backfill.as_ref() { b.amount.update(cx, |s, cx| s.set_value(amount.to_string(), window, cx)); }
        self.pass_changed(cx);
    }
}
