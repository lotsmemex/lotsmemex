//! 阅读计时 (pure): active dwell on one pass (on by default, ⏱ turns it off),
//! cut into server-session segments by a monotonic clock. Being on asks the
//! daemon for nothing until a real activity. Time accrues only while timing is on, the
//! window is in front, the reading view is shown and the last real scroll /
//! click / key was under 120 s ago. Mode changes, hiding, idle deadlines,
//! suspended ticks, the segment bound and the session window all cut a segment.
//! Segments keep their ids until acknowledged; a session the daemon no longer
//! admits loses its unsent segments (never moved into a new session).

use memex_client::passes::{TimingMode, TimingSegment, TimingSession, MAX_REPORT_SEGMENTS};
use std::time::{Duration, Instant};

pub const IDLE: Duration = Duration::from_secs(120);
/// An open segment is closed and reopened this often, so little stays unsent.
pub const CHECKPOINT: Duration = Duration::from_secs(30);
/// Ticks further apart than this mean the app was suspended: that gap is unmeasured.
pub const MAX_TICK_GAP: Duration = Duration::from_secs(5);
/// A new session is fetched this long before the current one's window ends.
pub const RENEW_BEFORE: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
struct Live {
    id: String,
    origin: Instant,
    window_ms: u64,
    max_segment_ms: u64,
    /// Offset after which the daemon no longer takes this session's reports.
    deadline_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsent {
    pub session_id: String,
    pub segment: TimingSegment,
    deadline: Instant,
}

impl Unsent {
    pub fn ms(&self) -> u64 { self.segment.end_offset_ms - self.segment.start_offset_ms }
}

/// What a failed report means for its segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFailure {
    /// Unknown / transport: keep them; the same ids go again (a duplicate is harmless).
    Retry,
    /// The session expired or the clock jumped: these can never be admitted.
    SessionGone,
    /// A definite refusal (closed, conflict, limit): drop them.
    Refused,
}

#[derive(Debug, Clone)]
pub struct Clock {
    pub enabled: bool,
    foreground: bool,
    shown: bool,
    mode: TimingMode,
    last_activity: Option<Instant>,
    last_tick: Option<Instant>,
    session: Option<Live>,
    /// The open segment: its mode and start offset in the live session.
    open: Option<(TimingMode, u64)>,
    unsent: Vec<Unsent>,
    /// Ids handed out by `report` and not yet answered.
    in_flight: usize,
    /// Closed time that can no longer be synchronized.
    pub lost_ms: u64,
    prefix: String,
    next: u64,
    wants_session: bool,
    /// Test hooks: the checkpoint and tick-gap rules (`None` turns them off).
    pub checkpoint: Option<Duration>,
    pub max_tick_gap: Option<Duration>,
}

impl Clock {
    pub fn new(prefix: impl Into<String>) -> Self {
        Clock {
            enabled: false, foreground: true, shown: true, mode: TimingMode::Bilingual, last_activity: None, last_tick: None, session: None, open: None,
            unsent: vec![], in_flight: 0, lost_ms: 0, prefix: prefix.into(), next: 0, wants_session: false, checkpoint: Some(CHECKPOINT), max_tick_gap: Some(MAX_TICK_GAP),
        }
    }

    fn offset(&self, at: Instant) -> u64 {
        self.session.as_ref().map_or(0, |s| at.saturating_duration_since(s.origin).as_millis() as u64)
    }

    fn idle_deadline(&self) -> Option<Instant> {
        self.last_activity.map(|t| t + IDLE)
    }

    /// Time may accrue at `now` (before any segment bound).
    pub fn accruing(&self, now: Instant) -> bool {
        self.enabled && self.foreground && self.shown && self.idle_deadline().is_some_and(|d| now < d)
    }

    /// Closes the open segment at `at` (never past the idle deadline or the bounds).
    fn close_at(&mut self, at: Instant) {
        let Some((mode, start)) = self.open.take() else { return };
        let Some(live) = self.session.as_ref() else { return };
        let at = self.idle_deadline().map_or(at, |d| at.min(d));
        let end = self.offset(at).min(live.window_ms).min(start + live.max_segment_ms);
        if end <= start { return; }
        self.next += 1;
        let deadline = live.origin + Duration::from_millis(live.deadline_ms);
        self.unsent.push(Unsent { session_id: live.id.clone(), segment: TimingSegment { segment_id: format!("{}-{}", self.prefix, self.next), mode, start_offset_ms: start, end_offset_ms: end }, deadline });
    }

    fn open_at(&mut self, at: Instant) {
        if self.open.is_some() || !self.accruing(at) { return; }
        match &self.session {
            Some(live) if self.offset(at) < live.window_ms => self.open = Some((self.mode.clone(), self.offset(at))),
            _ => self.wants_session = true,
        }
    }

    pub fn set_enabled(&mut self, on: bool, now: Instant) {
        if on == self.enabled { return; }
        if !on { self.close_at(now); }
        self.enabled = on;
        // Being on asks for nothing yet: the first real activity wants the session.
        self.wants_session = false;
        if on { self.open_at(now); }
    }

    /// A real scroll, click or key in the reading view.
    pub fn activity(&mut self, now: Instant) {
        // After the idle deadline the previous segment ended there; a new one starts now.
        if self.idle_deadline().is_some_and(|d| now >= d) { self.close_at(now); }
        self.last_activity = Some(now);
        self.open_at(now);
    }

    pub fn set_mode(&mut self, mode: TimingMode, now: Instant) {
        if mode == self.mode { return; }
        self.close_at(now);
        self.mode = mode;
        self.open_at(now);
    }

    /// Window in front (or not: unfocused, minimized, asleep).
    pub fn set_foreground(&mut self, on: bool, now: Instant) {
        if !on { self.close_at(now); self.last_activity = None; }
        self.foreground = on;
    }

    /// The reading view is on screen (or replaced by another view).
    pub fn set_shown(&mut self, on: bool, now: Instant) {
        if !on { self.close_at(now); self.last_activity = None; }
        self.shown = on;
    }

    /// The pass's timing ends (completion or another item): close and stop.
    pub fn stop(&mut self, now: Instant) {
        self.close_at(now);
        self.enabled = false;
        self.wants_session = false;
        self.session = None;
    }

    /// A new session, answered at `received`: offsets count from that instant.
    pub fn attach(&mut self, session: &TimingSession, received: Instant) {
        self.close_at(received);
        let deadline_ms = session.report_deadline_utc_ms.saturating_sub(session.origin_utc_ms);
        self.session = Some(Live { id: session.session_id.clone(), origin: received, window_ms: session.window_ms, max_segment_ms: session.max_segment_ms, deadline_ms });
        self.wants_session = false;
        self.open_at(received);
    }

    /// Whether a `timing_start` should be sent now (the caller sends one at a time).
    pub fn wants_session(&self, now: Instant) -> bool {
        self.enabled && self.foreground && self.shown && (self.wants_session || self.session.as_ref().is_some_and(|s| self.accruing(now) && self.offset(now) + RENEW_BEFORE.as_millis() as u64 >= s.window_ms))
    }

    pub fn tick(&mut self, now: Instant) {
        if let (Some(last), Some(gap)) = (self.last_tick, self.max_tick_gap) {
            if now.saturating_duration_since(last) > gap {
                // Suspended: credit nothing after the last observed tick; resume needs a fresh sample.
                self.close_at(last);
                self.last_activity = None;
            }
        }
        self.last_tick = Some(now);
        if self.open.is_some() && self.idle_deadline().is_some_and(|d| now >= d) { self.close_at(now); }
        while let (Some((_, start)), Some(live)) = (self.open.clone(), self.session.clone()) {
            let bound = (start + live.max_segment_ms).min(live.window_ms);
            if self.offset(now) < bound { break; }
            let at = live.origin + Duration::from_millis(bound);
            self.close_at(at);
            if bound >= live.window_ms {
                self.session = None;
                self.wants_session = self.accruing(now);
            } else {
                self.open = Some((self.mode.clone(), bound));
            }
        }
        if let (Some((_, start)), Some(every)) = (self.open.clone(), self.checkpoint) {
            if self.offset(now) >= start + every.as_millis() as u64 {
                self.close_at(now);
                self.open_at(now);
            }
        }
        let (kept, expired): (Vec<_>, Vec<_>) = std::mem::take(&mut self.unsent).into_iter().enumerate().partition(|(k, u)| *k < self.in_flight || now < u.deadline);
        self.lost_ms += expired.iter().map(|(_, u)| u.ms()).sum::<u64>();
        self.unsent = kept.into_iter().map(|(_, u)| u).collect();
    }

    /// Up to 64 unsent segments of one session, in order; `None` while a report is out.
    pub fn report(&mut self) -> Option<(String, Vec<TimingSegment>)> {
        if self.in_flight > 0 { return None; }
        let session = self.unsent.first()?.session_id.clone();
        let batch: Vec<_> = self.unsent.iter().take_while(|u| u.session_id == session).take(MAX_REPORT_SEGMENTS).map(|u| u.segment.clone()).collect();
        self.in_flight = batch.len();
        Some((session, batch))
    }

    /// The report was answered: its segments are on the daemon.
    pub fn acknowledged(&mut self) {
        let n = std::mem::take(&mut self.in_flight);
        self.unsent.drain(..n);
    }

    pub fn report_failed(&mut self, failure: ReportFailure) {
        let n = std::mem::take(&mut self.in_flight);
        match failure {
            ReportFailure::Retry => {}
            ReportFailure::SessionGone => {
                let gone = self.unsent.first().map(|u| u.session_id.clone());
                let (lost, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.unsent).into_iter().partition(|u| Some(&u.session_id) == gone.as_ref());
                self.lost_ms += lost.iter().map(Unsent::ms).sum::<u64>();
                self.unsent = kept;
                if self.session.as_ref().map(|s| &s.id) == gone.as_ref() {
                    self.open = None;
                    self.session = None;
                    self.wants_session = self.enabled;
                }
            }
            ReportFailure::Refused => {
                let lost: Vec<_> = self.unsent.drain(..n).collect();
                self.lost_ms += lost.iter().map(Unsent::ms).sum::<u64>();
            }
        }
    }

    /// Time measured here and not yet on the daemon (待同步): unsent plus the open segment.
    pub fn pending_ms(&self, now: Instant) -> u64 {
        let open = match (&self.open, &self.session) {
            (Some((_, start)), Some(_)) => {
                let at = self.idle_deadline().map_or(now, |d| now.min(d));
                self.offset(at).saturating_sub(*start)
            }
            _ => 0,
        };
        self.unsent.iter().map(Unsent::ms).sum::<u64>() + open
    }

    pub fn has_unsent(&self) -> bool { !self.unsent.is_empty() }
    pub fn is_open(&self) -> bool { self.open.is_some() }
    pub fn session_id(&self) -> Option<&str> { self.session.as_ref().map(|s| s.id.as_str()) }
    pub fn unsent(&self) -> &[Unsent] { &self.unsent }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, window_ms: u64) -> TimingSession {
        TimingSession { session_id: id.into(), item_id: "i".into(), pass_id: "pass_a".into(), revision_id: "r".into(), origin_utc_ms: 1_000_000, window_ms, max_segment_ms: 300_000, report_deadline_utc_ms: 1_000_000 + window_ms + 120_000 }
    }
    fn s(n: u64) -> Duration { Duration::from_secs(n) }
    fn clock(t0: Instant, window_ms: u64) -> Clock {
        let mut c = Clock::new("mac");
        c.checkpoint = None;
        c.max_tick_gap = None;
        c.set_enabled(true, t0);
        c.attach(&session("s1", window_ms), t0);
        c
    }
    fn spans(c: &Clock) -> Vec<(TimingMode, u64, u64)> {
        c.unsent().iter().map(|u| (u.segment.mode.clone(), u.segment.start_offset_ms, u.segment.end_offset_ms)).collect()
    }

    #[test]
    fn idle_cuts_at_exactly_120_seconds_after_the_last_activity() {
        let t0 = Instant::now();
        let mut c = clock(t0, 300_000);
        c.activity(t0);
        c.tick(t0 + s(119));
        assert!(c.is_open() && c.unsent().is_empty(), "119 s: still reading");
        assert_eq!(c.pending_ms(t0 + s(119)), 119_000);
        c.tick(t0 + s(120));
        assert_eq!(spans(&c), [(TimingMode::Bilingual, 0, 120_000)], "120 s: closed at the deadline");
        c.tick(t0 + s(121));
        assert_eq!(spans(&c), [(TimingMode::Bilingual, 0, 120_000)], "121 s: nothing more credited");
        assert!(!c.accruing(t0 + s(121)));
        // Without a tick in between, a late activity still ends the old segment at its deadline.
        let mut c = clock(t0, 300_000);
        c.activity(t0);
        c.activity(t0 + s(150));
        assert_eq!(spans(&c), [(TimingMode::Bilingual, 0, 120_000)]);
        assert!(c.is_open());
        c.tick(t0 + s(160));
        assert_eq!(c.pending_ms(t0 + s(160)), 130_000);
    }

    #[test]
    fn switched_on_when_the_reader_opens_it_asks_for_nothing_until_a_real_activity() {
        let t0 = Instant::now();
        let mut c = Clock::new("mac");
        c.checkpoint = None;
        c.max_tick_gap = None;
        c.set_enabled(true, t0);
        c.tick(t0 + s(30));
        assert!(!c.wants_session(t0 + s(30)) && c.pending_ms(t0 + s(30)) == 0, "opening the article is not reading it");
        // In the background or hidden, an activity still asks for nothing.
        c.set_foreground(false, t0 + s(31));
        c.activity(t0 + s(31));
        assert!(!c.wants_session(t0 + s(31)));
        c.set_foreground(true, t0 + s(32));
        c.activity(t0 + s(40));
        assert!(c.wants_session(t0 + s(40)), "the first real activity wants a session");
        c.attach(&session("s1", 300_000), t0 + s(41));
        assert!(c.is_open() && !c.wants_session(t0 + s(41)));
        // The normal rules still cut: idle 120 s after the last activity.
        c.tick(t0 + s(161));
        assert_eq!(spans(&c), [(TimingMode::Bilingual, 0, 119_000)]);
        // Switched off by hand: closes and asks for nothing more.
        let mut c = clock(t0, 300_000);
        c.activity(t0);
        c.set_enabled(false, t0 + s(5));
        c.activity(t0 + s(6));
        assert_eq!(spans(&c), [(TimingMode::Bilingual, 0, 5_000)]);
        assert!(!c.is_open() && !c.wants_session(t0 + s(6)));
    }

    #[test]
    fn mode_hide_and_focus_cut_and_resume_needs_a_fresh_activity() {
        let t0 = Instant::now();
        let mut c = clock(t0, 300_000);
        c.set_mode(TimingMode::Source, t0);
        c.activity(t0);
        c.set_mode(TimingMode::Translation, t0 + s(10));
        c.set_mode(TimingMode::Translation, t0 + s(12));
        c.set_foreground(false, t0 + s(25));
        c.set_foreground(true, t0 + s(40));
        c.tick(t0 + s(41));
        assert!(!c.is_open(), "back in front, no activity yet");
        c.activity(t0 + s(50));
        c.set_shown(false, t0 + s(55));
        assert_eq!(spans(&c), [(TimingMode::Source, 0, 10_000), (TimingMode::Translation, 10_000, 25_000), (TimingMode::Translation, 50_000, 55_000)]);
        // Timing off: nothing accrues however active.
        let mut c = clock(t0, 300_000);
        c.set_enabled(false, t0);
        c.activity(t0);
        c.tick(t0 + s(30));
        assert_eq!(c.pending_ms(t0 + s(30)), 0);
        assert!(!c.wants_session(t0));
    }

    #[test]
    fn long_reading_splits_at_the_segment_bound_and_the_session_window() {
        let t0 = Instant::now();
        // A long synthetic window shows the 300000 ms segment bound on its own.
        let mut c = clock(t0, 900_000);
        for k in 0..=610 {
            let now = t0 + s(k);
            if k % 60 == 0 { c.activity(now); }
            c.tick(now);
        }
        let got = spans(&c);
        assert_eq!(got[..2], [(TimingMode::Bilingual, 0, 300_000), (TimingMode::Bilingual, 300_000, 600_000)]);
        assert!(got.iter().all(|g| g.2 - g.1 <= 300_000));
        // The real 300000 ms window: the segment ends there and a new session is wanted.
        let mut c = clock(t0, 300_000);
        for k in 0..=301 {
            if k % 60 == 0 { c.activity(t0 + s(k)); }
            c.tick(t0 + s(k));
        }
        assert_eq!(spans(&c), [(TimingMode::Bilingual, 0, 300_000)]);
        assert!(c.session_id().is_none() && c.wants_session(t0 + s(301)));
        c.attach(&session("s2", 300_000), t0 + s(302));
        c.tick(t0 + s(312));
        c.set_shown(false, t0 + s(312));
        assert_eq!(c.unsent()[1].session_id, "s2");
        assert_eq!((c.unsent()[1].segment.start_offset_ms, c.unsent()[1].segment.end_offset_ms), (0, 10_000));
    }

    #[test]
    fn an_expired_session_is_restarted_and_old_segments_never_join_the_new_one() {
        let t0 = Instant::now();
        let mut c = clock(t0, 300_000);
        c.activity(t0);
        c.set_mode(TimingMode::Source, t0 + s(20));
        let (sid, batch) = c.report().unwrap();
        assert_eq!((sid.as_str(), batch.len()), ("s1", 1));
        assert!(c.report().is_none(), "one report at a time");
        // Unknown outcome: the same ids go again.
        c.report_failed(ReportFailure::Retry);
        let (_, again) = c.report().unwrap();
        assert_eq!(again, batch);
        c.report_failed(ReportFailure::SessionGone);
        assert_eq!(c.lost_ms, 20_000);
        assert!(c.session_id().is_none() && !c.is_open() && c.wants_session(t0 + s(21)));
        c.attach(&session("s9", 300_000), t0 + s(21));
        c.set_shown(false, t0 + s(31));
        let (sid, batch) = c.report().unwrap();
        assert_eq!(sid, "s9");
        assert_eq!(batch.iter().map(|b| (b.start_offset_ms, b.end_offset_ms)).collect::<Vec<_>>(), [(0, 10_000)]);
        assert!(batch.iter().all(|b| !again.iter().any(|a| a.segment_id == b.segment_id)));
        c.acknowledged();
        assert_eq!(c.pending_ms(t0 + s(40)), 0);
    }

    #[test]
    fn pending_counts_unsent_and_open_time_until_acknowledged_and_drops_after_the_deadline() {
        let t0 = Instant::now();
        let mut c = clock(t0, 300_000);
        c.checkpoint = Some(CHECKPOINT);
        c.activity(t0);
        c.tick(t0 + s(31));
        assert_eq!(spans(&c), [(TimingMode::Bilingual, 0, 31_000)], "checkpointed");
        assert_eq!(c.pending_ms(t0 + s(40)), 40_000);
        c.report().unwrap();
        c.acknowledged();
        assert_eq!(c.pending_ms(t0 + s(40)), 9_000);
        c.set_shown(false, t0 + s(40));
        assert_eq!(c.pending_ms(t0 + s(40)), 9_000);
        // Never synchronized before the report deadline: lost, not pending forever.
        c.tick(t0 + s(421));
        assert_eq!((c.pending_ms(t0 + s(421)), c.lost_ms), (0, 9_000));
        // A suspended app (ticks seconds apart) credits nothing after the last tick.
        let mut c = clock(t0, 300_000);
        c.max_tick_gap = Some(MAX_TICK_GAP);
        c.activity(t0);
        c.tick(t0 + s(1));
        c.tick(t0 + s(61));
        assert_eq!(spans(&c), [(TimingMode::Bilingual, 0, 1_000)]);
        assert!(!c.is_open());
    }
}
