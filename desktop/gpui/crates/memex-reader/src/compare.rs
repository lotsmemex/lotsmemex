//! 对比修改: two complete revisions of one target side by side, the old one on
//! the left and the new one on the right, as the book's 并排 columns. Lines that
//! were deleted are rose-red, added ones sage-green, a changed line carries both
//! with its exact changed spans marked. Both texts are read whole through
//! `revision_chunk` and verified (length and SHA-256) before anything is
//! compared; a read that stops early is reported, never shown as a partial
//! comparison. The diff is computed off the main thread, without any model.
//!
//! Reusable for any two revisions of one item/target: a chapter edit today,
//! a formatting result (input vs result revision) or an ordinary body later.

use crate::diff::{self, content, Diff, Item, RowKind};
use gpui_kit::component::{button::{Button, ButtonVariants as _}, h_flex, v_flex, ActiveTheme as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use memex_client::book::{RevisionReadError, RevisionTarget, RevisionText};
use memex_client::{tx, txf, Client};
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::sync::{atomic::{AtomicUsize, Ordering}, Arc};
use std::time::Duration;

/// Which two revisions of which target to compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareSpec {
    pub item_id: String,
    pub target: RevisionTarget,
    pub old_revision: String,
    pub new_revision: String,
    /// Chapter or item title shown next to the heading.
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareEvent {
    Close,
}

/// Bytes read so far and the total once known (0 = unknown), per side.
#[derive(Default)]
struct Progress {
    old: (AtomicUsize, AtomicUsize),
    new: (AtomicUsize, AtomicUsize),
}

struct Ready {
    old: RevisionText,
    new: RevisionText,
    diff: Diff,
    millis: u128,
}

enum State {
    Loading(Arc<Progress>),
    Failed { old: bool, error: RevisionReadError },
    Ready(Rc<Ready>),
}

pub struct RevisionCompare {
    client: Client,
    spec: CompareSpec,
    state: State,
    expanded: HashSet<usize>,
    items: Rc<Vec<Item>>,
    list: ListState,
    ticket: u64,
}

impl EventEmitter<CompareEvent> for RevisionCompare {}

/// Warm tints that read on light and dark grounds; no blue.
struct Tints {
    del_line: Hsla,
    del_mark: Hsla,
    del_sign: Hsla,
    ins_line: Hsla,
    ins_mark: Hsla,
    ins_sign: Hsla,
    empty: Hsla,
}

impl Tints {
    fn new(theme: &gpui_kit::component::Theme) -> Self {
        if theme.background.l < 0.5 {
            Tints {
                del_line: hsla(355. / 360., 0.34, 0.21, 1.),
                del_mark: hsla(355. / 360., 0.42, 0.33, 1.),
                del_sign: hsla(352. / 360., 0.60, 0.72, 1.),
                ins_line: hsla(135. / 360., 0.18, 0.19, 1.),
                ins_mark: hsla(135. / 360., 0.24, 0.30, 1.),
                ins_sign: hsla(140. / 360., 0.26, 0.66, 1.),
                empty: theme.muted.opacity(0.35),
            }
        } else {
            Tints {
                del_line: hsla(355. / 360., 0.62, 0.955, 1.),
                del_mark: hsla(355. / 360., 0.62, 0.85, 1.),
                del_sign: hsla(352. / 360., 0.55, 0.45, 1.),
                ins_line: hsla(135. / 360., 0.30, 0.935, 1.),
                ins_mark: hsla(135. / 360., 0.32, 0.81, 1.),
                ins_sign: hsla(140. / 360., 0.30, 0.32, 1.),
                empty: theme.muted.opacity(0.45),
            }
        }
    }
}

impl RevisionCompare {
    pub fn new(client: Client, spec: CompareSpec, cx: &mut Context<Self>) -> Self {
        let mut this = Self { client, spec, state: State::Loading(Default::default()), expanded: HashSet::new(), items: Rc::new(Vec::new()), list: ListState::new(0, ListAlignment::Top, px(600.)), ticket: 0 };
        this.load(cx);
        this
    }

    pub fn spec(&self) -> &CompareSpec {
        &self.spec
    }

    /// Reads both revisions whole (old first), then compares them off the main thread.
    pub fn load(&mut self, cx: &mut Context<Self>) {
        self.ticket += 1;
        let ticket = self.ticket;
        let progress = Arc::new(Progress::default());
        self.state = State::Loading(progress.clone());
        self.expanded.clear();
        self.items = Rc::new(Vec::new());
        self.list.reset(0);
        cx.notify();
        let (client, spec, p) = (self.client.clone(), self.spec.clone(), progress.clone());
        let task = cx.background_executor().spawn(async move {
            let read = |revision: &str, slot: &(AtomicUsize, AtomicUsize)| {
                client.reader_revision_text(&spec.item_id, revision, &spec.target, &|read, total| {
                    slot.0.store(read, Ordering::Relaxed);
                    slot.1.store(total, Ordering::Relaxed);
                })
            };
            let old = read(&spec.old_revision, &p.old).map_err(|e| (true, e))?;
            let new = read(&spec.new_revision, &p.new).map_err(|e| (false, e))?;
            let started = std::time::Instant::now();
            let diff = diff::diff(&old.text, &new.text);
            Ok::<_, (bool, RevisionReadError)>(Ready { old, new, diff, millis: started.elapsed().as_millis() })
        });
        // Repaints the byte counters while the reads run.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let more = this.update(cx, |this, cx| { cx.notify(); this.ticket == ticket && matches!(this.state, State::Loading(_)) }).unwrap_or(false);
            if !more { break; }
        }).detach();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.ticket != ticket { return; }
                this.state = match result {
                    Ok(ready) => State::Ready(Rc::new(ready)),
                    Err((old, error)) => State::Failed { old, error },
                };
                this.relayout();
                cx.notify();
            }).ok();
        }).detach();
    }

    fn relayout(&mut self) {
        self.items = Rc::new(match &self.state {
            State::Ready(r) => diff::visible(&r.diff, &self.expanded),
            _ => Vec::new(),
        });
        self.list.reset(self.items.len());
    }

    /// Opens the folded unchanged run `fold` in place, keeping the scroll position.
    pub fn expand(&mut self, fold: usize, cx: &mut Context<Self>) {
        let Some(at) = self.items.iter().position(|i| *i == Item::Fold(fold)) else { return };
        self.expanded.insert(fold);
        let State::Ready(r) = &self.state else { return };
        self.items = Rc::new(diff::visible(&r.diff, &self.expanded));
        let n = r.diff.folds[fold].len();
        self.list.splice(at..at + 1, n);
        cx.notify();
    }

    pub fn scroll_to_item(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(0.) });
        cx.notify();
    }

    /// Walks: one line describing what is on screen.
    pub fn summary(&self) -> String {
        match &self.state {
            State::Loading(p) => format!("loading old={}/{} new={}/{}", p.old.0.load(Ordering::Relaxed), p.old.1.load(Ordering::Relaxed), p.new.0.load(Ordering::Relaxed), p.new.1.load(Ordering::Relaxed)),
            State::Failed { old, error } => format!("failed side={} read={} total={:?} error={:?}", if *old { "old" } else { "new" }, error.read_bytes, error.total_bytes, error.error),
            State::Ready(r) => format!(
                "ready old_bytes={} new_bytes={} old_sha={} new_sha={} old_lines={} new_lines={} rows={} deleted={} inserted={} changed_rows={} folds={} visible={} expanded={} diff_ms={}",
                r.old.text.len(), r.new.text.len(), &r.old.sha256[..12], &r.new.sha256[..12], r.diff.old_lines.len(), r.diff.new_lines.len(), r.diff.rows.len(), r.diff.deleted, r.diff.inserted,
                r.diff.rows.iter().filter(|x| x.kind == RowKind::Changed).count(), r.diff.folds.len(), self.items.len(), self.expanded.len(), r.millis,
            ),
        }
    }

    fn title_line(&self) -> String {
        match &self.spec.target {
            RevisionTarget::Chapter { side, .. } => format!("{} · {}", tx(if side == "original" { "原文" } else { "译文" }), self.spec.title),
            RevisionTarget::Body {} => format!("{} · {}", tx("全文"), self.spec.title),
        }
    }
}

/// The distinguishing end of a revision id (ids begin with a shared time prefix).
fn short(revision: &str) -> &str {
    let tail = revision.rsplit('_').next().unwrap_or(revision);
    let mut at = tail.len().saturating_sub(8);
    while !tail.is_char_boundary(at) { at += 1; }
    &tail[at..]
}

/// One side of a row: line number, a −/+ sign (so the state never rests on color
/// alone), and the line with its changed spans marked.
fn cell(text: Option<(usize, &str, &[Range<usize>])>, kind: RowKind, old: bool, eol_only: bool, t: &Tints, muted: Hsla) -> Div {
    let Some((line, raw, marks)) = text else {
        return h_flex().flex_1().min_w_0().self_stretch().bg(t.empty);
    };
    let changed = kind != RowKind::Same;
    let (line_bg, mark_bg, sign_color, sign) = if old { (t.del_line, t.del_mark, t.del_sign, "−") } else { (t.ins_line, t.ins_mark, t.ins_sign, "+") };
    let shown = content(raw).to_string();
    // A line marked whole is tinted by its row; only partial marks get the stronger ground.
    let whole = marks.len() == 1 && marks[0] == (0..shown.len());
    let highlights: Vec<(Range<usize>, HighlightStyle)> = if changed && !whole {
        marks.iter().filter(|r| r.end <= shown.len() && r.start < r.end).map(|r| (r.clone(), HighlightStyle { background_color: Some(mark_bg), ..Default::default() })).collect()
    } else {
        Vec::new()
    };
    h_flex()
        .flex_1()
        .min_w_0()
        .self_stretch()
        .items_start()
        .when(changed, |d| d.bg(line_bg))
        .child(div().flex_none().w(px(44.)).pr_2().text_right().text_xs().pt(px(2.)).text_color(muted).child((line + 1).to_string()))
        .child(div().flex_none().w(px(14.)).text_sm().font_weight(FontWeight::SEMIBOLD).text_color(if changed { sign_color } else { muted.opacity(0.0) }).child(if changed { sign } else { " " }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pr_2()
                .text_sm()
                .child(StyledText::new(if shown.is_empty() { " ".to_string() } else { shown }).with_highlights(highlights))
                .when(eol_only, |d| d.child(div().text_xs().text_color(muted).child(tx("仅行尾换行不同")))),
        )
}

impl Render for RevisionCompare {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tints = Rc::new(Tints::new(&theme));
        let swatch = |c: Hsla, border: Hsla| div().flex_none().w(px(12.)).h(px(12.)).rounded(px(3.)).bg(c).border_1().border_color(border);
        let legend = h_flex()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(swatch(tints.del_mark, tints.del_sign.opacity(0.5)))
            .child(swatch(tints.ins_mark, tints.ins_sign.opacity(0.5)))
            .child(tx("红色为删除 · 绿色为新增"))
            .when_some(match &self.state { State::Ready(r) if !r.diff.identical() => Some(txf("删除 {0} 行 · 新增 {1} 行", &[&r.diff.deleted, &r.diff.inserted])), _ => None }, |d, s| d.child(div().text_color(theme.foreground).child(s)));
        let head = v_flex()
            .flex_none()
            .gap_1()
            .pb_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(tx("对比修改")))
                    .child(div().flex_1().min_w_0().truncate().text_sm().text_color(theme.muted_foreground).child(self.title_line()))
                    .child(Button::new("compare-close").small().ghost().icon(IconName::Close).label(tx("关闭对比")).on_click(cx.listener(|_, _, _, cx| cx.emit(CompareEvent::Close)))),
            )
            .child(legend);
        let column_head = |label: &'static str, revision: &str, text: Option<&RevisionText>| {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .items_baseline()
                .child(div().flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(theme.primary).child(tx(label)))
                .child(div().min_w_0().truncate().text_xs().text_color(theme.muted_foreground).child(match text {
                    Some(t) if t.text.is_empty() => format!("{} · {}", short(revision), tx("（空）")),
                    Some(t) => format!("{} · {}", short(revision), txf("{0} 字节", &[&t.text.len()])),
                    None => short(revision).to_string(),
                }))
        };
        let ready = match &self.state { State::Ready(r) => Some(r.clone()), _ => None };
        let columns = h_flex()
            .flex_none()
            .gap_6()
            .pb_1()
            .border_b_1()
            .border_color(theme.border)
            .child(column_head("旧版", &self.spec.old_revision, ready.as_ref().map(|r| &r.old)))
            .child(div().flex_none().w(px(1.)).h(px(14.)).bg(theme.border))
            .child(column_head("新版", &self.spec.new_revision, ready.as_ref().map(|r| &r.new)));
        let note = |s: String| div().flex_none().py_1().text_xs().text_color(theme.muted_foreground).child(s);
        let body: AnyElement = match &self.state {
            State::Loading(p) => {
                let (o, ot, n, nt) = (p.old.0.load(Ordering::Relaxed), p.old.1.load(Ordering::Relaxed), p.new.0.load(Ordering::Relaxed), p.new.1.load(Ordering::Relaxed));
                let total = |t: usize| if t == 0 { "?".to_string() } else { t.to_string() };
                v_flex().flex_1().items_center().justify_center().gap_1().text_sm().text_color(theme.muted_foreground)
                    .child(tx("正在读取两个版本的完整文本…"))
                    .child(div().text_xs().child(txf("旧版 {0} / {1} 字节 · 新版 {2} / {3} 字节", &[&o, &total(ot), &n, &total(nt)])))
                    .into_any_element()
            }
            State::Failed { old, error } => {
                let total = error.total_bytes.map_or_else(|| "?".to_string(), |t| t.to_string());
                let reason = match &error.error {
                    memex_client::ClientError::Daemon(c) if c == "result_too_large" => tx("文本超过 16 MiB，不能在这里对比").to_string(),
                    e => e.message(),
                };
                let line = txf(if *old { "未能完整读取旧版（已读 {0} / {1} 字节）：{2}" } else { "未能完整读取新版（已读 {0} / {1} 字节）：{2}" }, &[&error.read_bytes, &total, &reason]);
                v_flex().flex_1().items_center().justify_center().gap_2().text_sm()
                    .child(div().max_w(px(560.)).text_center().child(line))
                    .child(div().max_w(px(560.)).text_center().text_xs().text_color(theme.muted_foreground).child(tx("为避免把不完整的文本当作完整对比，没有显示任何差异。")))
                    .child(Button::new("compare-retry").small().outline().label(tx("重试")).on_click(cx.listener(|this, _, _, cx| this.load(cx))))
                    .into_any_element()
            }
            State::Ready(r) => {
                let items = self.items.clone();
                let r2 = r.clone();
                let t = tints.clone();
                let muted = theme.muted_foreground;
                let fold_bg = theme.muted.opacity(0.5);
                let me = cx.entity().downgrade();
                let rows = list(self.list.clone(), move |ix, _window, _cx| {
                    let d = &r2.diff;
                    match items.get(ix).copied() {
                        Some(Item::Fold(f)) => {
                            let n = d.folds[f].len();
                            let me = me.clone();
                            div().id(("compare-fold", f)).w_full().py_1().my(px(2.)).rounded(px(4.)).bg(fold_bg).text_xs().text_color(muted).text_center().cursor_pointer()
                                .hover(|s| s.opacity(0.8))
                                .child(txf("… {0} 行未改动", &[&n]))
                                .on_click(move |_, _, cx| { me.update(cx, |c, cx| c.expand(f, cx)).ok(); })
                                .into_any_element()
                        }
                        Some(Item::Row(i)) => {
                            let row = &d.rows[i];
                            let old = row.old.map(|l| (l, &r2.old.text[d.old_lines[l].clone()], row.old_marks.as_slice()));
                            let new = row.new.map(|l| (l, &r2.new.text[d.new_lines[l].clone()], row.new_marks.as_slice()));
                            h_flex().w_full().items_stretch().gap_6()
                                .child(cell(old, row.kind, true, row.eol_only, &t, muted))
                                .child(div().flex_none().w(px(1.)).bg(fold_bg))
                                .child(cell(new, row.kind, false, row.eol_only, &t, muted))
                                .into_any_element()
                        }
                        None => div().into_any_element(),
                    }
                })
                .flex_1();
                let verified = txf("两个版本均已完整读取，长度与 SHA-256 校验一致（旧版 {0} · 新版 {1}）", &[&&r.old.sha256[..12], &&r.new.sha256[..12]]);
                v_flex().flex_1().min_h_0()
                    .child(note(verified))
                    .when(r.diff.identical(), |d| d.child(div().py_2().text_sm().child(tx("两个版本的这部分完全相同"))))
                    .child(rows)
                    .into_any_element()
            }
        };
        v_flex().flex_1().min_h_0().size_full().child(head).child(columns).child(body)
    }
}
