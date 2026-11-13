//! 文件导入：把 Mac 上的文件拖进窗口，新建条目（首次入库）。
//!
//! **占位接缝。** 后端还没有「任意文件 → 新建条目」的 UDS 命令，也没有能力名；
//! B 补上通用首次入库入口后，以 B 的 DTO 为准替换本文件里标了 `B-DTO` 的地方
//! （能力名、请求与回执的字段、原因码、类型表与限制）。编辑已有条目的命令
//! （`apply-conversion`、`markdown_edit` 等）不是入库入口，不能接到这里。
//!
//! 这里集中放：能力名、可接受的类型表与限制、只读元数据的预检、拖入路径的
//! 展开（文件夹递归，最多 [`MAX_FILES_PER_DROP`] 个）、请求与回执 DTO、
//! [`ImportBackend`] 接口，以及真实代理的占位实现 [`AgentImport`]（能力缺失时
//! 一个请求都不发）。合成后端在 `fixture::FixtureImport`。
//!
//! 预检只读文件系统元数据（大小、扩展名）和开头几个字节的魔数，从不读正文，
//! 也不把文件内容写进日志。

use crate::{tx, txf, Client, ClientError};
use serde::{Deserialize, Serialize};
use std::{
    io::Read as _,
    path::{Path, PathBuf},
};

/// B-DTO：UDS `capabilities.features` 里打开文件导入的能力名（暂定）。
/// 代理不宣告它时，客户端不发任何导入请求。
pub const FILE_IMPORT_FEATURE: &str = "file_import_v1";

/// B-DTO：请求与回执的 schema 版本（暂定）。
pub const FILE_IMPORT_SCHEMA: u32 = 1;

/// 一次拖入（含文件夹递归）最多收集的文件数。
pub const MAX_FILES_PER_DROP: usize = 500;

/// 后端已知限制：单个 PDF 不超过 100 MiB（客户端直接拒绝更大的）。
pub const MAX_PDF_BYTES: u64 = 100 * 1024 * 1024;
/// 后端已知限制：PDF 须为 1–1000 页、带文本层，没有 OCR。页数和文本层要读正文
/// 才知道，客户端不检查，由后端回执的原因码说明。
pub const PDF_PAGES: std::ops::RangeInclusive<u32> = 1..=1000;

/// 魔数最多读的字节数。
const MAGIC_LEN: usize = 8;

/// 可接受的文件类型（B-DTO：最终以 B 的 DTO 为准）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Pdf,
    Markdown,
    Text,
    Html,
    Epub,
}

/// 一种类型的规则：扩展名（小写，不带点）、开头魔数、大小上限。
#[derive(Debug, Clone, Copy)]
pub struct KindRule {
    pub kind: FileKind,
    pub extensions: &'static [&'static str],
    pub magic: Option<&'static [u8]>,
    pub max_bytes: Option<u64>,
    /// 界面上的类型名（中文键，经 `tx`）。
    pub label: &'static str,
}

/// 类型表与限制，集中在这一处（B-DTO）。
pub const KIND_RULES: [KindRule; 5] = [
    KindRule { kind: FileKind::Pdf, extensions: &["pdf"], magic: Some(b"%PDF-"), max_bytes: Some(MAX_PDF_BYTES), label: "PDF" },
    KindRule { kind: FileKind::Markdown, extensions: &["md"], magic: None, max_bytes: None, label: "Markdown" },
    KindRule { kind: FileKind::Text, extensions: &["txt"], magic: None, max_bytes: None, label: "纯文本" },
    KindRule { kind: FileKind::Html, extensions: &["html", "htm"], magic: None, max_bytes: None, label: "HTML" },
    KindRule { kind: FileKind::Epub, extensions: &["epub"], magic: Some(b"PK\x03\x04"), max_bytes: None, label: "EPUB" },
];

impl FileKind {
    pub fn rule(self) -> &'static KindRule {
        KIND_RULES.iter().find(|r| r.kind == self).expect("every kind has a rule")
    }

    pub fn label(self) -> &'static str {
        tx(self.rule().label)
    }

    /// 按扩展名（不区分大小写）认类型。
    pub fn from_path(path: &Path) -> Option<FileKind> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        KIND_RULES.iter().find(|r| r.extensions.contains(&ext.as_str())).map(|r| r.kind)
    }
}

/// 提示层与托盘里列出的可接受类型。
pub fn accepted_summary() -> String {
    KIND_RULES.iter().map(|r| tx(r.label)).collect::<Vec<_>>().join(tx("、"))
}

/// 预检结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preflight {
    /// 可以提交。
    Ready { kind: FileKind, size: u64 },
    /// 不在类型表里：列为「暂不支持」，不提交。
    Unsupported { reason: String },
    /// 类型认得，但没通过客户端检查（过大、空文件、魔数不符、读不到）：不提交。
    Rejected { reason: String },
}

/// 只读元数据和魔数的预检；不读正文。
pub fn preflight(path: &Path) -> Preflight {
    let Some(kind) = FileKind::from_path(path) else {
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| format!(".{}", e.to_ascii_lowercase()));
        return Preflight::Unsupported {
            reason: match ext {
                Some(ext) => txf("暂不支持 {0} 文件", &[&ext]),
                None => tx("暂不支持没有扩展名的文件").into(),
            },
        };
    };
    let rule = kind.rule();
    let size = match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m.len(),
        Ok(_) => return Preflight::Rejected { reason: tx("不是普通文件").into() },
        Err(_) => return Preflight::Rejected { reason: tx("无法读取文件信息").into() },
    };
    if size == 0 {
        return Preflight::Rejected { reason: tx("文件是空的").into() };
    }
    if let Some(max) = rule.max_bytes.filter(|max| size > *max) {
        return Preflight::Rejected { reason: txf("{0} 超过 {1} MiB（{2} MiB），后端不接受", &[&kind.label(), &(max / 1024 / 1024), &mib(size)]) };
    }
    if let Some(magic) = rule.magic {
        let mut head = [0u8; MAGIC_LEN];
        let read = std::fs::File::open(path).and_then(|f| {
            let mut n = 0;
            let mut f = f.take(MAGIC_LEN as u64);
            loop {
                match f.read(&mut head[n..])? {
                    0 => break,
                    k => n += k,
                }
            }
            Ok(n)
        });
        match read {
            Ok(n) if head[..n].starts_with(magic) => {}
            Ok(_) => return Preflight::Rejected { reason: txf("文件内容不像 {0}（扩展名与文件头不符）", &[&kind.label()]) },
            Err(_) => return Preflight::Rejected { reason: tx("无法读取文件").into() },
        }
    }
    Preflight::Ready { kind, size }
}

fn mib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1024. / 1024.)
}

/// 拖入路径展开后的文件。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expansion {
    pub files: Vec<PathBuf>,
    /// 超过 [`MAX_FILES_PER_DROP`]，后面的没收集。
    pub truncated: bool,
}

/// 展开拖入的路径：文件原样保留，文件夹按名称顺序递归收集；跳过隐藏项
/// （`.` 开头）和符号链接的文件夹（避免循环），总数不超过 `limit`。
pub fn expand(paths: &[PathBuf], limit: usize) -> Expansion {
    let mut out = Expansion::default();
    for path in paths {
        if !collect(path, limit, &mut out) {
            break;
        }
    }
    out
}

/// 返回 false 表示已到上限。
fn collect(path: &Path, limit: usize, out: &mut Expansion) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        // 读不到的路径也列出来，由预检说明原因。
        return push(path, limit, out);
    };
    if meta.is_dir() {
        let Ok(dir) = std::fs::read_dir(path) else { return true };
        let mut entries: Vec<PathBuf> = dir
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| !p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.')))
            .collect();
        entries.sort();
        for entry in entries {
            let is_link_dir = std::fs::symlink_metadata(&entry).is_ok_and(|m| m.file_type().is_symlink()) && entry.is_dir();
            if is_link_dir {
                continue;
            }
            if !collect(&entry, limit, out) {
                return false;
            }
        }
        true
    } else {
        push(path, limit, out)
    }
}

fn push(path: &Path, limit: usize, out: &mut Expansion) -> bool {
    if out.files.len() >= limit {
        out.truncated = true;
        return false;
    }
    out.files.push(path.to_path_buf());
    true
}

/// 一次拖入准备好的文件：路径、显示名和预检结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedFile {
    pub path: PathBuf,
    pub name: String,
    pub preflight: Preflight,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDrop {
    pub files: Vec<PreparedFile>,
    pub truncated: bool,
}

/// 拖入为什么没有开始。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropRefusal {
    /// 代理没有宣告导入能力：一个请求都不发。
    NotOpen,
    /// 能力探测本身失败（离线等）。
    Probe(ClientError),
    /// 拖入的东西里一个文件都没有。
    Empty,
}

impl DropRefusal {
    pub fn message(&self) -> String {
        match self {
            DropRefusal::NotOpen => tx("Mac 端暂未开放文件导入").into(),
            DropRefusal::Probe(e) => txf("无法确认 Mac 端是否开放文件导入：{0}", &[&e.message()]),
            DropRefusal::Empty => tx("拖入的内容里没有文件").into(),
        }
    }
}

/// 拖入的第一步：先问能力（缺失时不碰文件系统、不发请求），再展开与预检。
pub fn prepare_drop(backend: &dyn ImportBackend, paths: &[PathBuf]) -> Result<PreparedDrop, DropRefusal> {
    match backend.supported() {
        Ok(true) => {}
        Ok(false) | Err(ClientError::Unsupported) => return Err(DropRefusal::NotOpen),
        Err(e) => return Err(DropRefusal::Probe(e)),
    }
    let expansion = expand(paths, MAX_FILES_PER_DROP);
    if expansion.files.is_empty() {
        return Err(DropRefusal::Empty);
    }
    let files = expansion
        .files
        .into_iter()
        .map(|path| {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
            let preflight = preflight(&path);
            PreparedFile { path, name, preflight }
        })
        .collect();
    Ok(PreparedDrop { files, truncated: expansion.truncated })
}

/// B-DTO：一次导入请求（暂定形状）。同一个 `request_id` 只代表这一个文件的这一次
/// 提交；回执丢失后只能按它对账（[`ImportBackend::status`]），不能换号重发。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportRequest {
    pub schema_version: u32,
    pub request_id: String,
    /// 本机绝对路径（代理与客户端在同一台 Mac 上）。B-DTO：若 B 要求分块上传，
    /// 在这里换成上传会话。
    pub path: PathBuf,
    pub file_name: String,
    pub kind: FileKind,
    pub size_bytes: u64,
    /// 放进哪个库或文件夹；`None` 进 Inbox。
    pub target_group_id: Option<String>,
}

/// B-DTO：回执里的状态（暂定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportPhase {
    /// 已收下，排在后端队列里。
    Accepted,
    Processing,
    Succeeded,
    Failed,
}

/// B-DTO：导入回执（暂定）：提交和对账都返回它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReceipt {
    pub schema_version: u32,
    pub request_id: String,
    pub phase: ImportPhase,
    /// 成功时的新条目。
    #[serde(default)]
    pub item_id: Option<String>,
    /// 失败时的原因码。
    #[serde(default)]
    pub reason_code: Option<String>,
}

impl ImportReceipt {
    /// 回执必须属于这次请求，成功必须给出条目。
    pub fn check(self, request_id: &str) -> Result<ImportReceipt, ClientError> {
        let valid = self.schema_version == FILE_IMPORT_SCHEMA
            && self.request_id == request_id
            && (self.phase != ImportPhase::Succeeded || self.item_id.as_deref().is_some_and(|i| !i.is_empty()));
        if valid { Ok(self) } else { Err(ClientError::InvalidFrame) }
    }
}

/// B-DTO：原因码到界面文字（暂定码名，按后端已知限制）。
pub fn reason_message(code: &str) -> String {
    match code {
        "file_import_pdf_too_large" => txf("PDF 超过 {0} MiB，后端不接受", &[&(MAX_PDF_BYTES / 1024 / 1024)]),
        "file_import_pdf_no_text" => tx("PDF 没有文本层（暂不支持 OCR）").into(),
        "file_import_pdf_pages" => txf("PDF 页数须在 {0}–{1} 页之间", &[PDF_PAGES.start(), PDF_PAGES.end()]),
        "file_import_unsupported_type" => tx("后端不接受这种文件").into(),
        "file_import_unreadable" => tx("Mac 端读不到这个文件").into(),
        "file_import_not_found" => tx("Mac 端没有这次导入的记录").into(),
        "group_read_only" | "group_not_found" => ClientError::Daemon(code.into()).message(),
        other => ClientError::Daemon(other.into()).message(),
    }
}

/// 导入请求去向。
pub trait ImportBackend: Send + Sync + 'static {
    /// 代理是否宣告了导入能力。
    fn supported(&self) -> Result<bool, ClientError>;
    /// 提交一个文件。传输层错误（超时、断开）表示结果未知。
    fn submit(&self, request: &ImportRequest) -> Result<ImportReceipt, ClientError>;
    /// 按原请求身份读一次状态（处理中轮询，或回执丢失后的对账）。只读，不会提交。
    fn status(&self, request_id: &str) -> Result<ImportReceipt, ClientError>;
}

/// 真实代理的占位实现：能力从 `capabilities.features` 读；B 的命令就绪前，
/// 提交和对账都不发送（返回 `Unsupported`）。
pub struct AgentImport {
    client: Client,
}

impl AgentImport {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

impl ImportBackend for AgentImport {
    fn supported(&self) -> Result<bool, ClientError> {
        Ok(self.client.capabilities()?.features.iter().any(|f| f == FILE_IMPORT_FEATURE))
    }

    // B-DTO：在这里发送 B 的首次入库命令（经 `Command` 新增的变体），并用
    // `ImportReceipt::check` 校验回执。
    fn submit(&self, _request: &ImportRequest) -> Result<ImportReceipt, ClientError> {
        Err(ClientError::Unsupported)
    }

    // B-DTO：在这里发送 B 的导入状态读取命令（按 request_id）。
    fn status(&self, _request_id: &str) -> Result<ImportReceipt, ClientError> {
        Err(ClientError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{FixtureImport, ImportControls};

    /// 一个只属于本测试的空目录，里面放合成生成的小文件。
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("memex-import-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, bytes: &[u8]) -> PathBuf {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        path.to_path_buf()
    }

    #[test]
    fn preflight_reads_type_size_and_magic_only() {
        let dir = scratch("preflight");
        let pdf = write(&dir.join("a.PDF"), b"%PDF-1.7\nsynthetic");
        let fake_pdf = write(&dir.join("fake.pdf"), b"hello, not a pdf");
        let md = write(&dir.join("n.md"), b"# synthetic");
        let txt = write(&dir.join("n.txt"), b"synthetic");
        let html = write(&dir.join("n.htm"), b"<p>synthetic</p>");
        let epub = write(&dir.join("b.epub"), b"PK\x03\x04synthetic");
        let bad_epub = write(&dir.join("c.epub"), b"synthetic");
        let empty = write(&dir.join("e.txt"), b"");
        let docx = write(&dir.join("w.docx"), b"PK\x03\x04");
        let bare = write(&dir.join("README"), b"x");
        assert_eq!(preflight(&pdf), Preflight::Ready { kind: FileKind::Pdf, size: 18 });
        assert!(matches!(preflight(&fake_pdf), Preflight::Rejected { .. }));
        assert!(matches!(preflight(&md), Preflight::Ready { kind: FileKind::Markdown, .. }));
        assert!(matches!(preflight(&txt), Preflight::Ready { kind: FileKind::Text, .. }));
        assert!(matches!(preflight(&html), Preflight::Ready { kind: FileKind::Html, .. }));
        assert!(matches!(preflight(&epub), Preflight::Ready { kind: FileKind::Epub, .. }));
        assert!(matches!(preflight(&bad_epub), Preflight::Rejected { .. }));
        assert!(matches!(preflight(&empty), Preflight::Rejected { .. }));
        assert!(matches!(preflight(&docx), Preflight::Unsupported { .. }));
        assert!(matches!(preflight(&bare), Preflight::Unsupported { .. }));
        assert!(matches!(preflight(&dir.join("missing.pdf")), Preflight::Rejected { .. }));
        assert!(matches!(preflight(&dir), Preflight::Unsupported { .. }));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pdf_over_100_mib_is_refused_on_the_client_with_its_reason() {
        let dir = scratch("pdf-size");
        // Sparse synthetic files: only the header is written, the length is set.
        let make = |name: &str, len: u64| {
            let path = write(&dir.join(name), b"%PDF-1.4\n");
            std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(len).unwrap();
            path
        };
        let at_limit = make("limit.pdf", MAX_PDF_BYTES);
        let over = make("over.pdf", MAX_PDF_BYTES + 1);
        assert_eq!(preflight(&at_limit), Preflight::Ready { kind: FileKind::Pdf, size: MAX_PDF_BYTES });
        let Preflight::Rejected { reason } = preflight(&over) else { panic!("over the limit") };
        assert!(reason.contains("100") && reason.contains("PDF"), "{reason}");
        // The limit lives in the one type table.
        assert_eq!(FileKind::Pdf.rule().max_bytes, Some(100 * 1024 * 1024));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn folders_expand_recursively_in_name_order_up_to_the_limit() {
        let dir = scratch("expand");
        let single = write(&dir.join("single.md"), b"x");
        let folder = dir.join("folder");
        write(&folder.join("b.txt"), b"x");
        write(&folder.join("a.pdf"), b"%PDF-");
        write(&folder.join("sub/c.html"), b"x");
        write(&folder.join(".DS_Store"), b"x");
        write(&folder.join(".hidden/d.txt"), b"x");
        let got = expand(&[single.clone(), folder.clone()], 500);
        assert_eq!(got.files, vec![single.clone(), folder.join("a.pdf"), folder.join("b.txt"), folder.join("sub/c.html")]);
        assert!(!got.truncated);
        // The cap counts every collected file across the whole drop.
        let capped = expand(&[single.clone(), folder.clone()], 2);
        assert_eq!(capped.files, vec![single, folder.join("a.pdf")]);
        assert!(capped.truncated);
        let many = dir.join("many");
        for i in 0..(MAX_FILES_PER_DROP + 3) {
            write(&many.join(format!("{i:04}.txt")), b"x");
        }
        let got = expand(&[many], MAX_FILES_PER_DROP);
        assert_eq!(got.files.len(), MAX_FILES_PER_DROP);
        assert!(got.truncated);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn without_the_capability_nothing_is_read_or_sent() {
        let dir = scratch("nocap");
        let pdf = write(&dir.join("a.pdf"), b"%PDF-1.7");
        let fixture = FixtureImport::new(None);
        fixture.set_controls(ImportControls { capability_missing: true, instant: true, ..Default::default() });
        assert_eq!(prepare_drop(&fixture, &[pdf.clone()]), Err(DropRefusal::NotOpen));
        assert_eq!(DropRefusal::NotOpen.message(), tx("Mac 端暂未开放文件导入"));
        assert_eq!((fixture.submits().len(), fixture.status_calls()), (0, 0));
        // The real agent today: no feature in `capabilities`, so no import command.
        let agent = AgentImport::new(Client::new(crate::fixture::FixtureBackend::new(4)));
        assert_eq!(agent.supported(), Ok(false));
        assert_eq!(prepare_drop(&agent, &[pdf]), Err(DropRefusal::NotOpen));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prepared_drops_keep_names_and_preflight_and_receipts_are_checked() {
        let dir = scratch("prepare");
        let pdf = write(&dir.join("a.pdf"), b"%PDF-1.7");
        let zip = write(&dir.join("z.zip"), b"PK\x03\x04");
        let fixture = FixtureImport::new(None);
        fixture.set_controls(ImportControls { instant: true, ..Default::default() });
        let prepared = prepare_drop(&fixture, &[pdf, zip, dir.join("gone")]).unwrap();
        let names: Vec<_> = prepared.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["a.pdf", "z.zip", "gone"]);
        assert!(matches!(prepared.files[0].preflight, Preflight::Ready { .. }));
        assert!(matches!(prepared.files[1].preflight, Preflight::Unsupported { .. }));
        assert!(matches!(prepared.files[2].preflight, Preflight::Unsupported { .. }));
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(prepare_drop(&fixture, &[empty]), Err(DropRefusal::Empty));
        assert_eq!(fixture.submits().len(), 0, "preparing sends nothing");
        let receipt = |request_id: &str, phase, item: Option<&str>| ImportReceipt { schema_version: FILE_IMPORT_SCHEMA, request_id: request_id.into(), phase, item_id: item.map(str::to_string), reason_code: None };
        assert!(receipt("r1", ImportPhase::Accepted, None).check("r1").is_ok());
        assert_eq!(receipt("r2", ImportPhase::Accepted, None).check("r1"), Err(ClientError::InvalidFrame));
        assert_eq!(receipt("r1", ImportPhase::Succeeded, None).check("r1"), Err(ClientError::InvalidFrame));
        let wire = serde_json::to_value(ImportRequest { schema_version: 1, request_id: "r".into(), path: "/x/a.pdf".into(), file_name: "a.pdf".into(), kind: FileKind::Pdf, size_bytes: 3, target_group_id: None }).unwrap();
        assert_eq!(wire["kind"], "pdf");
        assert_eq!(wire["target_group_id"], serde_json::Value::Null);
        std::fs::remove_dir_all(&dir).ok();
    }
}
