//! 「采集」卡片（条目列表底部）和侧栏的放置目标。卡片把两件事放在一起：
//! Chrome／手机提交的网页采集进度（`capture::CaptureFeed`，只读
//! `capture_jobs_recent`；有任务时每 `capture::ACTIVE_POLL` 读一次，空闲时
//! `capture::IDLE_POLL`，代理没有这项能力时不显示也不发请求），以及拖进来的
//! 文件导入（`import::ImportTray`）：在后台一次提交一个文件、轮询处理中的行、
//! 按原请求身份对账；网络与文件系统只经 `memex_client::file_import::ImportBackend`。
//! 默认收成一行摘要，点开列出各条。
//!
//! GPUI 外部拖放：系统把文件拖进窗口时，GPUI 把它变成类型为 `ExternalPaths`
//! 的内部拖动，所以放置目标用 `on_drop::<ExternalPaths>`、高亮用
//! `drag_over::<ExternalPaths>`、悬停位置用 `on_drag_move::<ExternalPaths>`。

use crate::capture::{self, CaptureFeed, Tone};
use crate::import::{Destination, DropSpot, ImportRow, ImportTray, RowState};
use crate::view::{Library, LibraryEvent};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use memex_client::capture_jobs::CaptureJob;
use memex_client::file_import::{prepare_drop, FileKind, ImportBackend};
use memex_client::{tx, txf, Client, ClientError};
use std::{path::PathBuf, sync::Arc, time::Duration};

/// 处理中的行多久读一次状态。
pub const POLL_INTERVAL: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImporterEvent {
    /// 点了已完成的行：打开新条目。
    Open(String),
    /// 有文件导入或网页采集完成：列表和侧栏计数需要刷新。
    Imported,
}

pub struct Importer {
    backend: Arc<dyn ImportBackend>,
    pub tray: ImportTray,
    pub collapsed: bool,
    /// 代理是否宣告了导入能力（None：还没问到）；只用于拖动时的提示。
    pub supported: Option<bool>,
    driving: bool,
    /// 正在展开与预检的拖入数。
    preparing: usize,
    /// 网页采集进度；`capture_client` 为 None 时不读。
    pub capture: CaptureFeed,
    capture_client: Option<Client>,
    /// 每次重新开始读取加一；旧的读取循环醒来发现不是自己就退出。
    capture_generation: u64,
}

/// 现在（Unix 秒）。
pub fn now_seconds() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl EventEmitter<ImporterEvent> for Importer {}

impl Importer {
    /// `capture`：读网页采集进度用的客户端（None：卡片只管文件导入）。
    pub fn new(backend: Arc<dyn ImportBackend>, capture: Option<Client>, cx: &mut Context<Self>) -> Self {
        let mut this = Self { backend, tray: ImportTray::default(), collapsed: true, supported: None, driving: false, preparing: 0, capture: CaptureFeed::default(), capture_client: capture, capture_generation: 0 };
        this.probe(cx);
        this
    }

    /// 重新读一次导入能力和采集进度（启动时、窗口回到前台时）。
    pub fn probe(&mut self, cx: &mut Context<Self>) {
        self.poll_capture(cx);
        let backend = self.backend.clone();
        let task = cx.background_executor().spawn(async move { backend.supported() });
        cx.spawn(async move |this, cx| {
            let supported = task.await.ok();
            this.update(cx, |t, cx| {
                t.supported = supported;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 卡片要不要显示：有导入，或有要显示的采集任务。
    pub fn visible(&self) -> bool {
        self.imports_visible() || self.capture.visible(now_seconds())
    }

    fn imports_visible(&self) -> bool {
        !self.tray.is_empty() || self.preparing > 0
    }

    /// 从头开始读采集进度：先问能力（缺失时什么都不发），再按
    /// `CaptureFeed::poll_interval` 一直读；读失败时保留上次的结果，下次再读。
    pub fn poll_capture(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.capture_client.clone() else { return };
        self.capture_generation += 1;
        let generation = self.capture_generation;
        cx.spawn(async move |this, cx| loop {
            let c = client.clone();
            let result = cx.background_executor().spawn(async move { c.capture_jobs_recent() }).await;
            let Ok(wait) = this.update(cx, |t, cx| {
                if t.capture_generation != generation {
                    return None;
                }
                match result {
                    Ok(recent) => {
                        if t.capture.apply(recent) {
                            cx.emit(ImporterEvent::Imported);
                        }
                    }
                    Err(ClientError::Unsupported) => t.capture.unsupported(),
                    Err(_) => {}
                }
                cx.notify();
                (t.capture.supported == Some(true)).then(|| t.capture.poll_interval())
            }) else {
                return;
            };
            let Some(wait) = wait else { return };
            cx.background_executor().timer(wait).await;
            if this.update(cx, |t, _| t.capture_generation != generation).unwrap_or(true) {
                return;
            }
        })
        .detach();
    }

    /// 一次拖放：只读位置直接提示；否则先问能力（缺失时不读文件、不发请求），
    /// 再在后台展开与预检，加入托盘并开始提交。
    pub fn drop_paths(&mut self, paths: Vec<PathBuf>, destination: Destination, cx: &mut Context<Self>) {
        self.collapsed = false;
        if destination.read_only() {
            self.tray.notice = Some(txf("「{0}」是只读的，不能导入到这里", &[&destination.label()]));
            cx.notify();
            return;
        }
        self.preparing += 1;
        cx.notify();
        let backend = self.backend.clone();
        let task = cx.background_executor().spawn(async move { prepare_drop(backend.as_ref(), &paths) });
        cx.spawn(async move |this, cx| {
            let prepared = task.await;
            this.update(cx, |t, cx| {
                t.preparing -= 1;
                match prepared {
                    Ok(prepared) => {
                        t.supported = Some(true);
                        t.tray.add(prepared, &destination);
                        t.drive(cx);
                    }
                    Err(refusal) => {
                        if refusal == memex_client::file_import::DropRefusal::NotOpen {
                            t.supported = Some(false);
                        }
                        t.tray.notice = Some(refusal.message());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn finished(&mut self, done: Option<String>, cx: &mut Context<Self>) {
        if done.is_some() {
            cx.emit(ImporterEvent::Imported);
        }
        cx.notify();
    }

    /// 一个接一个地提交排队的行，再轮询处理中的行，直到都结束。
    fn drive(&mut self, cx: &mut Context<Self>) {
        if self.driving {
            return;
        }
        self.driving = true;
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(next) = this.update(cx, |t, cx| {
                    let next = t.tray.next_submit();
                    cx.notify();
                    next
                }) else {
                    return;
                };
                let submitted = next.is_some();
                if let Some((id, request)) = next {
                    let b = backend.clone();
                    let result = cx.background_executor().spawn(async move { b.submit(&request) }).await;
                    if this.update(cx, |t, cx| { let done = t.tray.apply_submit(id, result); t.finished(done, cx); }).is_err() {
                        return;
                    }
                }
                let Ok(polls) = this.update(cx, |t, _| t.tray.to_poll()) else { return };
                if polls.is_empty() {
                    if submitted {
                        continue;
                    }
                    break;
                }
                // Between uploads the rows already processing are read once; with
                // nothing left to send, every `POLL_INTERVAL`.
                if !submitted {
                    cx.background_executor().timer(POLL_INTERVAL).await;
                }
                for (id, request_id) in polls {
                    let b = backend.clone();
                    let result = cx.background_executor().spawn(async move { b.status(&request_id) }).await;
                    if this.update(cx, |t, cx| { let done = t.tray.apply_status(id, result); t.finished(done, cx); }).is_err() {
                        return;
                    }
                }
            }
            this.update(cx, |t, cx| {
                t.driving = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn cancel(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.tray.cancel(id) {
            cx.notify();
        }
    }

    pub fn cancel_queued(&mut self, cx: &mut Context<Self>) {
        if self.tray.cancel_queued() > 0 {
            cx.notify();
        }
    }

    pub fn retry(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.tray.retry(id) {
            self.drive(cx);
            cx.notify();
        }
    }

    /// 对账一个「未知」行：只按原请求身份读一次状态，不提交。
    pub fn reconcile(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(request_id) = self.tray.reconcile(id) else { return };
        cx.notify();
        let backend = self.backend.clone();
        let task = cx.background_executor().spawn(async move { backend.status(&request_id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |t, cx| {
                let done = t.tray.apply_reconcile(id, result);
                if t.tray.row(id).is_some_and(|r| r.state == RowState::Processing) {
                    t.drive(cx);
                }
                t.finished(done, cx);
            })
            .ok();
        })
        .detach();
    }

    /// 关闭：清掉已结束的行（文件和网页）；还有进行中或未知的行时只收起。
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.tray.clear_finished();
        self.capture.dismiss_finished(now_seconds());
        if self.visible() {
            self.collapsed = true;
        }
        cx.notify();
    }

    fn capture_row(&self, job: &CaptureJob, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let color = tone_color(capture::tone(job), cx);
        let running = capture::tone(job) == Tone::Running;
        let id = job.job_id.clone();
        let icon = match capture::tone(job) {
            Tone::Running => None,
            Tone::Waiting => Some(IconName::Ellipsis),
            Tone::Attention => Some(IconName::CircleAlert),
            Tone::Failed => Some(IconName::CircleX),
            Tone::Done => Some(IconName::CircleCheck),
            Tone::Muted => Some(IconName::Minus),
        };
        let place = [capture::job_host(job), Some(capture::when_label(job, now)).filter(|w| !w.is_empty())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        let reason = capture::reason_label(job);
        let open = job.result_item_id.clone().filter(|_| job.state == memex_client::capture_jobs::CaptureState::Succeeded);
        let emphasized = matches!(capture::tone(job), Tone::Failed | Tone::Attention);
        h_flex()
            .id(SharedString::from(format!("capture-row-{id}")))
            .debug_selector(move || format!("capture-row-{id}"))
            .gap_2()
            .px_3()
            .py(px(6.))
            .items_start()
            .border_t_1()
            .border_color(theme.border.opacity(0.7))
            .when(emphasized, |d| d.bg(color.opacity(0.07)))
            .when(capture::tone(job) == Tone::Muted, |d| d.opacity(0.55))
            .child(div().pt(px(2.)).child(Icon::new(IconName::Globe).small().text_color(theme.muted_foreground)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(1.))
                    .child(div().text_sm().truncate().child(capture::job_title(job)))
                    .when(!place.is_empty(), |d| d.child(div().text_xs().text_color(theme.muted_foreground).truncate().child(place)))
                    .when_some(reason, |d, r| d.child(div().text_xs().text_color(color).child(r))),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .text_color(color)
                    .when(running, |d| d.child(spinner(color)))
                    .when_some(icon, |d, i| d.child(Icon::new(i).xsmall().text_color(color)))
                    .child(capture::state_label(job, now))
                    .when(open.is_some(), |d| d.child(div().pl_1().text_color(theme.primary).child(tx("打开")))),
            )
            .when_some(open, |d, item_id| {
                d.cursor_pointer().hover(|s| s.bg(theme.muted)).on_click(cx.listener(move |_, _, _, cx| cx.emit(ImporterEvent::Open(item_id.clone()))))
            })
            .into_any_element()
    }

    fn row_view(&self, row: &ImportRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let id = row.id;
        let success = hsla(150. / 360., 0.32, if theme.background.l < 0.5 { 0.62 } else { 0.38 }, 1.);
        let (icon, color): (IconName, Hsla) = match &row.state {
            RowState::Queued | RowState::Cancelled => (IconName::Minus, theme.muted_foreground),
            RowState::Uploading | RowState::Processing | RowState::Reconciling => (IconName::LoaderCircle, theme.primary),
            RowState::Done { .. } => (IconName::CircleCheck, success),
            RowState::Failed { .. } => (IconName::CircleX, theme.danger),
            RowState::Unsupported { .. } => (IconName::Ban, theme.muted_foreground),
            RowState::Unknown => (IconName::CircleAlert, theme.warning),
        };
        let kind_icon = match row.kind {
            Some(FileKind::Pdf | FileKind::Epub) => IconName::BookOpen,
            Some(_) => IconName::FileText,
            None => IconName::File,
        };
        let detail = match &row.state {
            RowState::Failed { reason, .. } | RowState::Unsupported { reason } => Some((reason.clone(), if matches!(row.state, RowState::Failed { .. }) { theme.danger } else { theme.muted_foreground })),
            RowState::Unknown => Some((tx("提交后没有收到回执，结果未知；可按原请求对账，不会重新提交").into(), theme.warning)),
            _ => None,
        };
        let place = if row.size > 0 { format!("{} · {}", row.destination.label(), size_label(row.size)) } else { row.destination.label() };
        let action = match &row.state {
            RowState::Queued => Some(Button::new(("import-cancel", id)).xsmall().ghost().label(tx("取消")).on_click(cx.listener(move |t, _, _, cx| { cx.stop_propagation(); t.cancel(id, cx); }))),
            RowState::Failed { retryable: true, .. } => Some(Button::new(("import-retry", id)).xsmall().ghost().icon(IconName::RotateCw).label(tx("重试")).on_click(cx.listener(move |t, _, _, cx| { cx.stop_propagation(); t.retry(id, cx); }))),
            RowState::Unknown => Some(Button::new(("import-reconcile", id)).xsmall().ghost().icon(IconName::RefreshCw).label(tx("对账")).tooltip(tx("按原请求身份查询这次导入的结果")).on_click(cx.listener(move |t, _, _, cx| { cx.stop_propagation(); t.reconcile(id, cx); }))),
            _ => None,
        };
        let open = match &row.state {
            RowState::Done { item_id } => Some(item_id.clone()),
            _ => None,
        };
        h_flex()
            .id(("import-row", id))
            .debug_selector(move || format!("import-row-{id}"))
            .gap_2()
            .px_3()
            .py(px(7.))
            .items_start()
            .border_t_1()
            .border_color(theme.border.opacity(0.7))
            .when(row.state == RowState::Cancelled, |d| d.opacity(0.55))
            .child(div().pt(px(2.)).child(Icon::new(kind_icon).small().text_color(theme.muted_foreground)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(1.))
                    .child(div().text_sm().truncate().child(row.name.clone()))
                    .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(place))
                    .when_some(detail, |d, (text, color)| d.child(div().text_xs().text_color(color).child(text))),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .child(h_flex().gap_1().items_center().text_xs().text_color(color).child(Icon::new(icon).xsmall().text_color(color)).child(row.state.label()))
                    .when_some(action, |d, b| d.child(b))
                    .when(open.is_some(), |d| d.child(div().text_xs().text_color(theme.primary).child(tx("打开")))),
            )
            .when_some(open, |d, item_id| {
                d.cursor_pointer().hover(|s| s.bg(theme.muted)).on_click(cx.listener(move |_, _, _, cx| cx.emit(ImporterEvent::Open(item_id.clone()))))
            })
            .into_any_element()
    }
}

fn display_rank(state: &RowState) -> u8 {
    match state {
        RowState::Uploading | RowState::Processing | RowState::Reconciling => 0,
        RowState::Queued => 1,
        RowState::Unknown => 2,
        RowState::Failed { .. } => 3,
        RowState::Unsupported { .. } => 4,
        RowState::Done { .. } => 5,
        RowState::Cancelled => 6,
    }
}

fn size_label(bytes: u64) -> String {
    match bytes {
        b if b >= 1024 * 1024 => format!("{:.1} MB", b as f64 / 1024. / 1024.),
        b if b >= 1024 => format!("{:.0} KB", b as f64 / 1024.),
        b => format!("{b} B"),
    }
}

/// 语气对应的颜色：玫瑰色主色表示进行中，失败和需处理用醒目色。
fn tone_color(tone: Tone, cx: &App) -> Hsla {
    let theme = cx.theme();
    match tone {
        Tone::Running => theme.primary,
        Tone::Waiting | Tone::Muted => theme.muted_foreground,
        Tone::Attention => theme.warning,
        Tone::Failed => theme.danger,
        Tone::Done => hsla(150. / 360., 0.32, if theme.background.l < 0.5 { 0.62 } else { 0.38 }, 1.),
    }
}

fn spinner(color: Hsla) -> impl IntoElement {
    gpui_kit::component::spinner::Spinner::new()
        .icon(Icon::default().data(br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none"><path d="M12 2a10 10 0 1 1-10 10" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"/></svg>"#))
        .with_size(px(12.))
        .color(color)
}

/// 卡片顶边那条在动的细进度条（有东西正在抓取或上传时）。
fn moving_bar(color: Hsla) -> impl IntoElement {
    div().relative().h(px(2.)).w_full().overflow_hidden().bg(color.opacity(0.12)).child(
        div().absolute().top_0().h_full().w(relative(0.32)).rounded_full().bg(color).with_animation(
            "capture-bar",
            Animation::new(Duration::from_millis(1600)).repeat().with_easing(ease_in_out),
            |d, delta| d.left(relative(delta * 1.32 - 0.32)),
        ),
    )
}

impl Importer {
    /// 摘要：网页采集与文件导入合在一起数。
    pub fn card_summary(&self, now: i64) -> capture::CaptureSummary {
        let mut s = self.capture.summary(now);
        for r in self.tray.rows() {
            match r.state {
                RowState::Uploading | RowState::Processing | RowState::Reconciling => s.running += 1,
                RowState::Queued => s.waiting += 1,
                RowState::Unknown => s.needs_user += 1,
                RowState::Failed { .. } => s.failed += 1,
                RowState::Done { .. } => s.done += 1,
                RowState::Unsupported { .. } | RowState::Cancelled => {}
            }
        }
        s
    }
}

impl Render for Importer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let now = now_seconds();
        let summary = self.card_summary(now);
        let queued = self.tray.rows().iter().filter(|r| r.state == RowState::Queued).count();
        let collapsed = self.collapsed;
        let moving = summary.running > 0 || self.preparing > 0;
        // What still moves or needs a decision first; finished rows last (each
        // group keeps the order the files were dropped in).
        let mut rows: Vec<ImportRow> = self.tray.rows().to_vec();
        rows.sort_by_key(|r| display_rank(&r.state));
        let jobs: Vec<CaptureJob> = self.capture.rows(now).into_iter().cloned().collect();
        let mut parts = summary.parts();
        // One line when folded: 完成 is the first to give way.
        if collapsed && parts.iter().any(|(tone, _)| *tone != Tone::Done) {
            parts.retain(|(tone, _)| *tone != Tone::Done);
        }
        let line: AnyElement = if parts.is_empty() {
            let text = if self.preparing > 0 { tx("正在检查文件…") } else if rows.is_empty() && jobs.is_empty() { tx("没有要导入的文件") } else { tx("都已结束") };
            div().text_xs().text_color(theme.muted_foreground).child(text).into_any_element()
        } else {
            h_flex()
                .gap_1()
                .text_xs()
                .min_w_0()
                // Folded it is one line; unfolded it may wrap rather than lose a count.
                .when(collapsed, |d| d.overflow_hidden())
                .when(!collapsed, |d| d.flex_wrap())
                .children(parts.into_iter().enumerate().flat_map(|(i, (tone, text))| {
                    let dot = (i > 0).then(|| div().text_color(theme.muted_foreground.opacity(0.6)).child("·").into_any_element());
                    let weight = if matches!(tone, Tone::Failed | Tone::Attention | Tone::Running) { FontWeight::MEDIUM } else { FontWeight::NORMAL };
                    let color = if tone == Tone::Waiting { theme.muted_foreground } else { tone_color(tone, cx) };
                    dot.into_iter().chain(Some(div().flex_none().font_weight(weight).text_color(color).child(text).into_any_element()))
                }))
                .into_any_element()
        };
        let badge = div().size(px(22.)).flex_none().rounded_full().bg(theme.accent).flex().items_center().justify_center();
        let badge = if moving { badge.child(spinner(theme.primary)) } else { badge.child(Icon::new(IconName::Inbox).xsmall().text_color(theme.primary)) };
        let header = h_flex()
            .id("capture-card-header")
            .debug_selector(|| "capture-card-header".into())
            .gap_2()
            .px_3()
            .py(px(7.))
            .items_center()
            .cursor_pointer()
            .on_click(cx.listener(|t, _, _, cx| { t.collapsed = !t.collapsed; cx.notify(); }))
            .child(badge)
            .child(div().flex_none().text_sm().font_weight(FontWeight::SEMIBOLD).child(tx("采集")))
            .child(div().flex_1().min_w_0().overflow_hidden().child(line))
            .when(queued > 0 && !collapsed, |d| d.child(Button::new("import-cancel-queued").xsmall().ghost().label(tx("取消排队")).on_click(cx.listener(|t, _, _, cx| { cx.stop_propagation(); t.cancel_queued(cx); }))))
            .child(Button::new("import-collapse").xsmall().ghost().icon(if collapsed { IconName::ChevronUp } else { IconName::ChevronDown }).tooltip(if collapsed { tx("展开") } else { tx("收起") }).on_click(cx.listener(|t, _, _, cx| { cx.stop_propagation(); t.collapsed = !t.collapsed; cx.notify(); })))
            .when(!collapsed, |d| d.child(Button::new("import-close").xsmall().ghost().icon(IconName::Close).tooltip(tx("清除已结束的项")).on_click(cx.listener(|t, _, _, cx| { cx.stop_propagation(); t.close(cx); }))));
        let notice = self.tray.notice.clone().filter(|_| !collapsed);
        v_flex()
            .id("import-tray")
            .debug_selector(|| "import-tray".into())
            .w_full()
            .flex_none()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .overflow_hidden()
            .text_color(theme.foreground)
            .child(if moving { moving_bar(theme.primary).into_any_element() } else { div().h(px(2.)).into_any_element() })
            .child(header)
            .when_some(notice, |d, n| {
                d.child(
                    h_flex()
                        .gap_2()
                        .mx_3()
                        .mb_2()
                        .px_2()
                        .py(px(6.))
                        .rounded(px(8.))
                        .bg(theme.accent)
                        .text_xs()
                        .child(Icon::new(IconName::Info).xsmall().text_color(theme.primary))
                        .child(div().flex_1().child(n)),
                )
            })
            .when(!collapsed && (!rows.is_empty() || !jobs.is_empty()), |d| {
                let mut views: Vec<AnyElement> = jobs.iter().map(|j| self.capture_row(j, now, cx)).collect();
                views.extend(rows.iter().map(|r| self.row_view(r, cx)));
                d.child(div().id("import-rows").max_h(px(200.)).overflow_y_scroll().children(views))
            })
    }
}

/// 侧栏的一行（Inbox、库、文件夹）成为文件放置目标：拖过时高亮，记下悬停位置
/// 给提示层用，放下时交给 shell（`LibraryEvent::FilesDropped`）。
pub fn file_drop_target(row: Stateful<Div>, library: Entity<Library>, spot: DropSpot) -> Stateful<Div> {
    let (hover_library, hover_spot) = (library.clone(), spot.clone());
    row.drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.18)).text_color(cx.theme().foreground))
        .on_drag_move::<ExternalPaths>(move |event, _, cx| {
            let inside = event.bounds.contains(&event.event.position);
            let spot = hover_spot.clone();
            hover_library.update(cx, |l, cx| l.set_drop_hover(spot, inside, cx));
        })
        .on_drop(move |paths: &ExternalPaths, _, cx| {
            let event = LibraryEvent::FilesDropped { paths: paths.paths().to_vec(), spot: spot.clone() };
            library.update(cx, |l, cx| {
                l.drop_hover = None;
                cx.emit(event);
            });
        })
}
