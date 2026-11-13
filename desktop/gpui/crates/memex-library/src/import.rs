//! 文件拖入导入的纯逻辑：放到哪里（目标位置判定）和导入托盘的状态机。
//! 网络与文件系统都不在这里；`import_view` 驱动它，接口在
//! `memex_client::file_import`。
//!
//! 目标：放在侧栏某个库或文件夹上进那里；放在 Inbox 上进 Inbox；放在条目列表、
//! 阅读区或窗口其他地方进「当前所在的位置」——当前是库或文件夹时进它本身，
//! 是 Inbox、全部条目、类型、来源、回收站等虚拟视图时进 Inbox。
//!
//! 托盘：每个文件一行，排队 → 上传中 → 处理中 → 完成／失败；预检不过的直接是
//! 失败或暂不支持。提交结果未知（传输层错误、回执对不上）时显示「未知」，
//! 绝不自动重新提交，只能按原 `request_id` 对账。只有明确失败的行可以重试，
//! 重试用新的请求身份（B-DTO：若 B 规定失败后可按原 id 重放，改这里）。

use crate::model::{Filter, LibraryModel};
use memex_client::file_import::{reason_message, FileKind, ImportPhase, ImportReceipt, ImportRequest, Preflight, PreparedDrop, FILE_IMPORT_SCHEMA, MAX_FILES_PER_DROP};
use memex_client::{biblio::new_request_id, tx, txf, ClientError};
use std::path::PathBuf;

/// 拖放落在哪里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropSpot {
    /// 条目列表、阅读区或窗口的其他地方：当前位置。
    Current,
    /// 侧栏的 Inbox 行。
    Inbox,
    /// 侧栏的某个库或文件夹行。
    Group(String),
}

/// 导入去向。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    Inbox,
    Group {
        id: String,
        /// 从库开始的路径，如「Main / AmpCode」。
        path: String,
        read_only: bool,
    },
}

impl Destination {
    pub fn group_id(&self) -> Option<&str> {
        match self {
            Destination::Inbox => None,
            Destination::Group { id, .. } => Some(id),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Destination::Inbox => tx("Inbox").into(),
            Destination::Group { path, .. } => path.clone(),
        }
    }

    pub fn read_only(&self) -> bool {
        matches!(self, Destination::Group { read_only: true, .. })
    }
}

/// 一个库或文件夹作为去向；不认识的 id 返回 None。
pub fn group_destination(model: &LibraryModel, id: &str) -> Option<Destination> {
    let node = model.group(id)?;
    let path = model.group_path(id).iter().map(|g| g.name.as_str()).collect::<Vec<_>>().join(" / ");
    Some(Destination::Group { id: node.id.clone(), path, read_only: node.read_only })
}

/// 拖放落点 → 去向。`show_trash`：回收站正替换着列表（虚拟视图）。
pub fn destination(model: &LibraryModel, show_trash: bool, spot: &DropSpot) -> Destination {
    match spot {
        DropSpot::Inbox => Destination::Inbox,
        DropSpot::Group(id) => group_destination(model, id).unwrap_or(Destination::Inbox),
        DropSpot::Current => match model.filter() {
            Filter::Group(id) if !show_trash => group_destination(model, &id).unwrap_or(Destination::Inbox),
            _ => Destination::Inbox,
        },
    }
}

/// 一行的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowState {
    Queued,
    Uploading,
    Processing,
    Done { item_id: String },
    /// `retryable`：明确失败，可以用新的请求重试；预检拒绝的不能。
    Failed { reason: String, retryable: bool },
    Unsupported { reason: String },
    /// 提交了但结果未知：只能对账，不重发。
    Unknown,
    /// 正在按原请求身份对账。
    Reconciling,
    Cancelled,
}

impl RowState {
    pub fn label(&self) -> &'static str {
        match self {
            RowState::Queued => tx("排队"),
            RowState::Uploading => tx("上传中"),
            RowState::Processing => tx("处理中"),
            RowState::Done { .. } => tx("完成"),
            RowState::Failed { .. } => tx("失败"),
            RowState::Unsupported { .. } => tx("暂不支持"),
            RowState::Unknown => tx("未知"),
            RowState::Reconciling => tx("对账中"),
            RowState::Cancelled => tx("已取消"),
        }
    }

    /// 还在进行（或等着开始）。
    pub fn is_active(&self) -> bool {
        matches!(self, RowState::Queued | RowState::Uploading | RowState::Processing | RowState::Reconciling)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRow {
    pub id: u64,
    pub name: String,
    pub path: PathBuf,
    pub kind: Option<FileKind>,
    pub size: u64,
    pub destination: Destination,
    /// 当前这次提交的请求身份；排队时为 None，提交时分配。
    pub request_id: Option<String>,
    /// 第几次提交（重试 +1）。
    pub attempt: u32,
    pub state: RowState,
    /// 处理中连续读状态失败的次数。
    poll_failures: u32,
}

/// 处理中连续这么多次读不到状态，改为「未知」（等用户对账）。
pub const MAX_POLL_FAILURES: u32 = 3;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub total: usize,
    pub active: usize,
    pub done: usize,
    pub failed: usize,
    pub unsupported: usize,
    pub unknown: usize,
    pub cancelled: usize,
}

impl Summary {
    /// 托盘标题下的一行汇总，只列非零项。
    pub fn line(&self) -> String {
        let mut parts = vec![txf("共 {0} 个", &[&self.total])];
        for (n, key) in [(self.active, "进行中 {0}"), (self.done, "完成 {0}"), (self.failed, "失败 {0}"), (self.unknown, "未知 {0}"), (self.unsupported, "暂不支持 {0}"), (self.cancelled, "已取消 {0}")] {
            if n > 0 {
                parts.push(txf(key, &[&n]));
            }
        }
        parts.join(" · ")
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImportTray {
    rows: Vec<ImportRow>,
    next_id: u64,
    /// 托盘顶部的提示（能力缺失、文件过多、只读位置）。
    pub notice: Option<String>,
}

impl ImportTray {
    pub fn rows(&self) -> &[ImportRow] {
        &self.rows
    }

    pub fn row(&self, id: u64) -> Option<&ImportRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    fn row_mut(&mut self, id: u64) -> Option<&mut ImportRow> {
        self.rows.iter_mut().find(|r| r.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.notice.is_none()
    }

    /// 有排队或进行中的行（托盘不能关）。
    pub fn is_busy(&self) -> bool {
        self.rows.iter().any(|r| r.state.is_active())
    }

    /// 加入一次拖入准备好的文件；返回加入的行数。
    pub fn add(&mut self, drop: PreparedDrop, destination: &Destination) -> usize {
        self.notice = drop.truncated.then(|| txf("一次最多导入 {0} 个文件，其余的没有加入", &[&MAX_FILES_PER_DROP]));
        let added = drop.files.len();
        for file in drop.files {
            let (kind, size, state) = match file.preflight {
                Preflight::Ready { kind, size } => (Some(kind), size, RowState::Queued),
                Preflight::Unsupported { reason } => (None, 0, RowState::Unsupported { reason }),
                Preflight::Rejected { reason } => (memex_client::file_import::FileKind::from_path(&file.path), 0, RowState::Failed { reason, retryable: false }),
            };
            self.next_id += 1;
            self.rows.push(ImportRow { id: self.next_id, name: file.name, path: file.path, kind, size, destination: destination.clone(), request_id: None, attempt: 1, state, poll_failures: 0 });
        }
        added
    }

    /// 下一个要提交的行：按顺序取第一个「排队」，改为「上传中」并分配请求身份。
    pub fn next_submit(&mut self) -> Option<(u64, ImportRequest)> {
        let row = self.rows.iter_mut().find(|r| r.state == RowState::Queued)?;
        let kind = row.kind?;
        let request_id = row.request_id.get_or_insert_with(new_request_id).clone();
        row.state = RowState::Uploading;
        let request = ImportRequest {
            schema_version: FILE_IMPORT_SCHEMA,
            request_id,
            path: row.path.clone(),
            file_name: row.name.clone(),
            kind,
            size_bytes: row.size,
            target_group_id: row.destination.group_id().map(str::to_string),
        };
        Some((row.id, request))
    }

    /// 回执 → 状态；返回新完成的条目。
    fn apply_receipt(row: &mut ImportRow, receipt: ImportReceipt) -> Option<String> {
        row.poll_failures = 0;
        match receipt.phase {
            ImportPhase::Accepted | ImportPhase::Processing => {
                row.state = RowState::Processing;
                None
            }
            ImportPhase::Succeeded => {
                let item_id = receipt.item_id.unwrap_or_default();
                row.state = RowState::Done { item_id: item_id.clone() };
                Some(item_id)
            }
            ImportPhase::Failed => {
                let reason = receipt.reason_code.as_deref().map(reason_message).unwrap_or_else(|| tx("Mac 端没有说明原因").into());
                row.state = RowState::Failed { reason, retryable: true };
                None
            }
        }
    }

    /// 提交的结果。传输层错误或对不上的回执 → 未知（不重发）。
    pub fn apply_submit(&mut self, id: u64, result: Result<ImportReceipt, ClientError>) -> Option<String> {
        let row = self.row_mut(id)?;
        if row.state != RowState::Uploading {
            return None;
        }
        let request_id = row.request_id.clone().unwrap_or_default();
        match result.and_then(|r| r.check(&request_id)) {
            Ok(receipt) => Self::apply_receipt(row, receipt),
            Err(e) if e.is_transport() => {
                row.state = RowState::Unknown;
                None
            }
            Err(ClientError::Unsupported) => {
                row.state = RowState::Failed { reason: tx("Mac 端暂未开放文件导入").into(), retryable: false };
                None
            }
            Err(ClientError::Daemon(code)) => {
                row.state = RowState::Failed { reason: reason_message(&code), retryable: true };
                None
            }
            Err(e) => {
                row.state = RowState::Failed { reason: e.message(), retryable: true };
                None
            }
        }
    }

    /// 处理中、需要读状态的行：(行, 原请求身份)。
    pub fn to_poll(&self) -> Vec<(u64, String)> {
        self.rows.iter().filter(|r| r.state == RowState::Processing).filter_map(|r| Some((r.id, r.request_id.clone()?))).collect()
    }

    /// 处理中读到的状态。读失败几次后改为未知。
    pub fn apply_status(&mut self, id: u64, result: Result<ImportReceipt, ClientError>) -> Option<String> {
        let row = self.row_mut(id)?;
        if row.state != RowState::Processing {
            return None;
        }
        let request_id = row.request_id.clone().unwrap_or_default();
        match result.and_then(|r| r.check(&request_id)) {
            Ok(receipt) => Self::apply_receipt(row, receipt),
            Err(ClientError::Daemon(code)) if code == "file_import_not_found" => {
                row.state = RowState::Failed { reason: reason_message(&code), retryable: true };
                None
            }
            Err(_) => {
                row.poll_failures += 1;
                if row.poll_failures >= MAX_POLL_FAILURES {
                    row.state = RowState::Unknown;
                }
                None
            }
        }
    }

    /// 开始对账一个「未知」行：只按原请求身份读状态。
    pub fn reconcile(&mut self, id: u64) -> Option<String> {
        let row = self.row_mut(id)?;
        if row.state != RowState::Unknown {
            return None;
        }
        let request_id = row.request_id.clone()?;
        row.state = RowState::Reconciling;
        Some(request_id)
    }

    /// 对账结果。Mac 端没有这次请求的记录 → 明确失败（可重试）；读不到 → 仍未知。
    pub fn apply_reconcile(&mut self, id: u64, result: Result<ImportReceipt, ClientError>) -> Option<String> {
        let row = self.row_mut(id)?;
        if row.state != RowState::Reconciling {
            return None;
        }
        let request_id = row.request_id.clone().unwrap_or_default();
        match result.and_then(|r| r.check(&request_id)) {
            Ok(receipt) => Self::apply_receipt(row, receipt),
            Err(ClientError::Daemon(code)) if code == "file_import_not_found" => {
                row.state = RowState::Failed { reason: tx("Mac 端没有收到这次导入").into(), retryable: true };
                None
            }
            Err(_) => {
                row.state = RowState::Unknown;
                None
            }
        }
    }

    /// 取消还没开始的行。
    pub fn cancel(&mut self, id: u64) -> bool {
        match self.row_mut(id) {
            Some(row) if row.state == RowState::Queued => {
                row.state = RowState::Cancelled;
                true
            }
            _ => false,
        }
    }

    /// 取消全部还没开始的行；返回取消的个数。
    pub fn cancel_queued(&mut self) -> usize {
        let ids: Vec<u64> = self.rows.iter().filter(|r| r.state == RowState::Queued).map(|r| r.id).collect();
        ids.into_iter().filter(|id| self.cancel(*id)).count()
    }

    /// 重试明确失败的行：重新排队，下次提交用新的请求身份。
    pub fn retry(&mut self, id: u64) -> bool {
        match self.row_mut(id) {
            Some(row) if matches!(row.state, RowState::Failed { retryable: true, .. }) && row.kind.is_some() => {
                row.state = RowState::Queued;
                row.request_id = None;
                row.attempt += 1;
                row.poll_failures = 0;
                true
            }
            _ => false,
        }
    }

    /// 清掉已结束的行（完成、失败、暂不支持、已取消）和提示；「未知」的行留着，
    /// 它们还需要对账。
    pub fn clear_finished(&mut self) {
        self.rows.retain(|r| r.state.is_active() || matches!(r.state, RowState::Unknown));
        self.notice = None;
    }

    pub fn summary(&self) -> Summary {
        let mut s = Summary { total: self.rows.len(), ..Summary::default() };
        for r in &self.rows {
            match r.state {
                _ if r.state.is_active() => s.active += 1,
                RowState::Done { .. } => s.done += 1,
                RowState::Failed { .. } => s.failed += 1,
                RowState::Unsupported { .. } => s.unsupported += 1,
                RowState::Unknown => s.unknown += 1,
                RowState::Cancelled => s.cancelled += 1,
                _ => {}
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_client::file_import::{prepare_drop, ImportBackend, PreparedFile};
    use memex_client::fixture::{FixtureImport, ImportControls};
    use memex_client::GroupNode;
    use std::path::Path;

    fn node(id: &str, name: &str, parent: Option<&str>, root: &str, read_only: bool) -> GroupNode {
        GroupNode { id: id.into(), name: name.into(), kind: if parent.is_none() { "user".into() } else { "collection".into() }, parent_id: parent.map(str::to_string), root_group_id: root.into(), item_count: 0, subtree_item_count: 0, read_only, version: "v1".into() }
    }

    fn model() -> LibraryModel {
        let mut m = LibraryModel::default();
        m.set_group_tree(vec![node("main", "Main", None, "main", false), node("amp", "AmpCode", Some("main"), "main", false), node("ro", "只读库", None, "ro", true)], true);
        m
    }

    #[test]
    fn virtual_views_import_into_inbox_and_real_places_into_themselves() {
        let mut m = model();
        for filter in [Filter::Inbox, Filter::All, Filter::Source("arxiv.org".into()), Filter::ItemType("PDF".into())] {
            m.set_filter(filter.clone());
            assert_eq!(destination(&m, false, &DropSpot::Current), Destination::Inbox, "{filter:?}");
        }
        m.set_filter(Filter::Group("amp".into()));
        let amp = Destination::Group { id: "amp".into(), path: "Main / AmpCode".into(), read_only: false };
        assert_eq!(destination(&m, false, &DropSpot::Current), amp);
        assert_eq!(amp.label(), "Main / AmpCode");
        // The trash replaces the list: a virtual view, even with a folder selected.
        assert_eq!(destination(&m, true, &DropSpot::Current), Destination::Inbox);
        m.set_filter(Filter::Group("main".into()));
        assert_eq!(destination(&m, false, &DropSpot::Current).group_id(), Some("main"));
        // A folder that is gone (deleted elsewhere) never becomes a guess.
        m.set_filter(Filter::Group("gone".into()));
        assert_eq!(destination(&m, false, &DropSpot::Current), Destination::Inbox);
    }

    #[test]
    fn sidebar_drops_go_to_the_row_dropped_on_whatever_is_selected() {
        let mut m = model();
        m.set_filter(Filter::Inbox);
        assert_eq!(destination(&m, false, &DropSpot::Group("amp".into())).label(), "Main / AmpCode");
        m.set_filter(Filter::Group("amp".into()));
        assert_eq!(destination(&m, false, &DropSpot::Inbox), Destination::Inbox);
        assert_eq!(destination(&m, true, &DropSpot::Group("main".into())).group_id(), Some("main"));
        assert!(destination(&m, false, &DropSpot::Group("ro".into())).read_only());
    }

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("memex-tray-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, bytes: &[u8]) -> PathBuf {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        path.to_path_buf()
    }

    /// Runs the tray against a backend until nothing is left to submit or poll
    /// (what the view's driver does, without its timers).
    fn drive(tray: &mut ImportTray, backend: &dyn ImportBackend) -> Vec<String> {
        let mut done = vec![];
        for _ in 0..100 {
            if let Some((id, request)) = tray.next_submit() {
                done.extend(tray.apply_submit(id, backend.submit(&request)));
                continue;
            }
            let polls = tray.to_poll();
            if polls.is_empty() {
                break;
            }
            for (id, request_id) in polls {
                done.extend(tray.apply_status(id, backend.status(&request_id)));
            }
        }
        done
    }

    fn fixture(controls: ImportControls) -> FixtureImport {
        let f = FixtureImport::new(None);
        f.set_controls(ImportControls { instant: true, ..controls });
        f
    }

    #[test]
    fn several_files_and_a_folder_become_rows_and_finish_in_order() {
        let dir = scratch("multi");
        let a = write(&dir.join("a.pdf"), b"%PDF-1.7");
        let folder = dir.join("notes");
        write(&folder.join("x.md"), b"# x");
        write(&folder.join("deep/y.txt"), b"y");
        write(&folder.join("z.docx"), b"PK\x03\x04");
        let f = fixture(ImportControls::default());
        let mut tray = ImportTray::default();
        let dest = Destination::Group { id: "amp".into(), path: "Main / AmpCode".into(), read_only: false };
        assert_eq!(tray.add(prepare_drop(&f, &[a, folder]).unwrap(), &dest), 4);
        let names: Vec<_> = tray.rows().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["a.pdf", "y.txt", "x.md", "z.docx"]);
        assert!(matches!(tray.rows()[3].state, RowState::Unsupported { .. }));
        let done = drive(&mut tray, &f);
        assert_eq!(done.len(), 3);
        assert_eq!(f.submits().len(), 3, "the unsupported file is never sent");
        assert!(tray.rows()[..3].iter().all(|r| matches!(r.state, RowState::Done { .. })));
        assert_eq!(tray.summary(), Summary { total: 4, done: 3, unsupported: 1, ..Summary::default() });
        assert!(tray.summary().line().contains('4'));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn requests_carry_the_destination_and_a_fresh_identity_per_file() {
        let dir = scratch("request");
        let a = write(&dir.join("a.pdf"), b"%PDF-1.7");
        let b = write(&dir.join("b.html"), b"<p>");
        let f = fixture(ImportControls::default());
        let mut tray = ImportTray::default();
        tray.add(prepare_drop(&f, &[a, b]).unwrap(), &Destination::Inbox);
        let (_, first) = tray.next_submit().unwrap();
        assert_eq!(first.target_group_id, None);
        assert_eq!((first.kind, first.size_bytes, first.file_name.as_str()), (FileKind::Pdf, 8, "a.pdf"));
        let (_, second) = tray.next_submit().unwrap();
        assert_ne!(first.request_id, second.request_id);
        assert!(tray.next_submit().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unknown_outcome_is_never_resubmitted_only_reconciled_by_its_request_id() {
        let dir = scratch("unknown");
        let lost = write(&dir.join("lost.pdf"), b"%PDF-1.7");
        let ok = write(&dir.join("ok.md"), b"# ok");
        let f = fixture(ImportControls { lose_receipt_names: vec!["lost".into()], ..Default::default() });
        let mut tray = ImportTray::default();
        tray.add(prepare_drop(&f, &[lost, ok]).unwrap(), &Destination::Inbox);
        drive(&mut tray, &f);
        let row = tray.rows()[0].clone();
        assert_eq!(row.state, RowState::Unknown);
        assert_eq!(row.state.label(), tx("未知"));
        let original = row.request_id.clone().unwrap();
        // Driving again, waiting, retrying: nothing is submitted again.
        drive(&mut tray, &f);
        assert!(!tray.retry(row.id), "an unknown row cannot be retried");
        assert!(tray.next_submit().is_none());
        assert_eq!(f.submits().iter().filter(|r| **r == original).count(), 1);
        assert_eq!(f.submits().len(), 2);
        // Reconciling reads the status of the original request only.
        let polls_before = f.status_calls();
        assert_eq!(tray.reconcile(row.id).as_deref(), Some(original.as_str()));
        assert_eq!(tray.rows()[0].state, RowState::Reconciling);
        let done = tray.apply_reconcile(row.id, f.status(&original));
        assert!(done.is_some());
        assert!(matches!(tray.rows()[0].state, RowState::Done { .. }));
        assert_eq!(f.status_calls(), polls_before + 1);
        assert_eq!(f.submits().len(), 2, "reconciling never submits");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn transport_errors_and_mismatched_receipts_mean_unknown() {
        let mut tray = ImportTray::default();
        let file = |name: &str| PreparedFile { path: format!("/synthetic/{name}").into(), name: name.into(), preflight: Preflight::Ready { kind: FileKind::Text, size: 1 } };
        tray.add(PreparedDrop { files: vec![file("a.txt"), file("b.txt"), file("c.txt")], truncated: false }, &Destination::Inbox);
        let (a, _) = tray.next_submit().unwrap();
        tray.apply_submit(a, Err(ClientError::Timeout));
        let (b, _) = tray.next_submit().unwrap();
        let other = ImportReceipt { schema_version: FILE_IMPORT_SCHEMA, request_id: "someone-else".into(), phase: ImportPhase::Accepted, item_id: None, reason_code: None };
        tray.apply_submit(b, Ok(other));
        let (c, _) = tray.next_submit().unwrap();
        tray.apply_submit(c, Err(ClientError::Daemon("file_import_pdf_no_text".into())));
        assert_eq!(tray.row(a).unwrap().state, RowState::Unknown);
        assert_eq!(tray.row(b).unwrap().state, RowState::Unknown);
        assert!(matches!(&tray.row(c).unwrap().state, RowState::Failed { retryable: true, reason } if reason.contains("OCR")));
        // A reconcile the Mac has no record of: a definite failure, retryable.
        let rid = tray.reconcile(a).unwrap();
        tray.apply_reconcile(a, Err(ClientError::Daemon("file_import_not_found".into())));
        assert!(matches!(tray.row(a).unwrap().state, RowState::Failed { retryable: true, .. }));
        assert!(tray.retry(a));
        let (_, again) = tray.next_submit().unwrap();
        assert_ne!(again.request_id, rid, "an explicit retry after a definite failure is a new request");
        // A reconcile that cannot read stays unknown.
        tray.reconcile(b).unwrap();
        tray.apply_reconcile(b, Err(ClientError::Unavailable));
        assert_eq!(tray.row(b).unwrap().state, RowState::Unknown);
    }

    #[test]
    fn queued_rows_can_be_cancelled_and_failed_rows_retried() {
        let dir = scratch("cancel");
        let bad = write(&dir.join("bad.pdf"), b"%PDF-1.7");
        let keep = write(&dir.join("keep.md"), b"# k");
        let drop_me = write(&dir.join("drop.txt"), b"d");
        let huge = write(&dir.join("huge.pdf"), b"%PDF-1.4");
        std::fs::OpenOptions::new().write(true).open(&huge).unwrap().set_len(memex_client::file_import::MAX_PDF_BYTES + 1).unwrap();
        let f = fixture(ImportControls { fail_names: vec!["bad".into()], ..Default::default() });
        let mut tray = ImportTray::default();
        tray.add(prepare_drop(&f, &[bad, keep, drop_me, huge]).unwrap(), &Destination::Inbox);
        // The client-side refusal is a failure with its reason, never retryable or sent.
        let huge_row = tray.rows()[3].clone();
        assert!(matches!(&huge_row.state, RowState::Failed { retryable: false, reason } if reason.contains("100")));
        assert!(!tray.retry(huge_row.id));
        // Start the first; cancel the third before it starts; the started one cannot be.
        let (first, _) = tray.next_submit().unwrap();
        assert!(!tray.cancel(first));
        let drop_id = tray.rows()[2].id;
        assert!(tray.cancel(drop_id));
        assert_eq!(tray.rows()[2].state, RowState::Cancelled);
        assert!(!tray.retry(drop_id));
        tray.apply_submit(first, f.submit(&{
            let r = tray.row(first).unwrap();
            ImportRequest { schema_version: FILE_IMPORT_SCHEMA, request_id: r.request_id.clone().unwrap(), path: r.path.clone(), file_name: r.name.clone(), kind: r.kind.unwrap(), size_bytes: r.size, target_group_id: None }
        }));
        drive(&mut tray, &f);
        let failed = tray.row(first).unwrap().clone();
        assert!(matches!(&failed.state, RowState::Failed { retryable: true, reason } if reason.contains("OCR")), "{:?}", failed.state);
        assert!(matches!(tray.rows()[1].state, RowState::Done { .. }));
        assert_eq!(f.submits().len(), 2, "cancelled and refused files are never sent");
        // Retry: queued again with a new identity, then fails again (synthetic) or succeeds.
        let before = failed.request_id.clone().unwrap();
        assert!(tray.retry(first));
        assert_eq!(tray.row(first).unwrap().state, RowState::Queued);
        f.set_controls(ImportControls { instant: true, ..Default::default() });
        drive(&mut tray, &f);
        let retried = tray.row(first).unwrap();
        assert!(matches!(retried.state, RowState::Done { .. }));
        assert_eq!(retried.attempt, 2);
        assert_ne!(retried.request_id.as_deref(), Some(before.as_str()));
        assert_eq!(f.submits().len(), 3);
        let s = tray.summary();
        assert_eq!((s.done, s.failed, s.cancelled, s.active), (2, 1, 1, 0));
        // Clearing keeps nothing finished.
        tray.clear_finished();
        assert!(tray.rows().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn processing_that_cannot_be_read_turns_unknown_after_a_few_tries() {
        let mut tray = ImportTray::default();
        tray.add(PreparedDrop { files: vec![PreparedFile { path: "/synthetic/a.txt".into(), name: "a.txt".into(), preflight: Preflight::Ready { kind: FileKind::Text, size: 1 } }], truncated: true }, &Destination::Inbox);
        assert!(tray.notice.as_deref().is_some_and(|n| n.contains("500")));
        let (id, request) = tray.next_submit().unwrap();
        let accepted = ImportReceipt { schema_version: FILE_IMPORT_SCHEMA, request_id: request.request_id.clone(), phase: ImportPhase::Accepted, item_id: None, reason_code: None };
        tray.apply_submit(id, Ok(accepted));
        assert_eq!(tray.row(id).unwrap().state, RowState::Processing);
        for _ in 1..MAX_POLL_FAILURES {
            tray.apply_status(id, Err(ClientError::Timeout));
            assert_eq!(tray.row(id).unwrap().state, RowState::Processing);
        }
        tray.apply_status(id, Err(ClientError::Timeout));
        assert_eq!(tray.row(id).unwrap().state, RowState::Unknown);
        assert!(tray.to_poll().is_empty() && tray.next_submit().is_none());
        // Unknown rows survive clearing: they still need reconciling.
        tray.clear_finished();
        assert_eq!(tray.rows().len(), 1);
        assert!(tray.notice.is_none());
    }
}
