//! Synthetic paired book for `reader_v1` (development and tests only).
//!
//! Like a real paired import, the canonical Markdown is the Chinese translation;
//! the English original exists only as its own edition. All text is invented.

use crate::book::ReaderRequest;
use crate::{Response, Sensitivity};
use sha2::{Digest, Sha256};

pub const BOOK_INDEX: usize = 28;
pub const BOOK_ITEM: &str = "item_fixture_000028";
pub const BOOK_TITLE: &str = "The Lantern Keeper (synthetic paired book)";
pub const BOOK_TITLE_ZH: &str = "守灯人（合成双语书）";

struct Chapter {
    key: &'static str,
    title: String,
    level: u8,
    counterpart: Option<&'static str>,
    body: String,
}

fn en_paragraph(seed: usize) -> String {
    const S: [&str; 6] = [
        "The keeper climbed the spiral stair before dusk and trimmed the wick by hand.",
        "Below, the harbour lay quiet, and the fishing boats rocked against their ropes.",
        "She wrote the weather into the logbook in a small, careful script.",
        "Nobody in the village remembered a night when the lamp had failed.",
        "The wind turned east, and the glass rattled softly in its iron frame.",
        "By midnight the sea had calmed, and the beam swept on across the water.",
    ];
    (0..4).map(|k| S[(seed + k) % S.len()]).collect::<Vec<_>>().join(" ")
}

fn zh_paragraph(seed: usize) -> String {
    const S: [&str; 6] = [
        "天黑之前，守灯人沿着旋梯爬上去，亲手修剪灯芯。",
        "下面的港湾很安静，渔船靠着缆绳轻轻摇晃。",
        "她用细小而工整的字，把天气记进日志。",
        "村里没有人记得哪一夜灯曾经熄灭过。",
        "风转向东边，玻璃在铁框里轻轻作响。",
        "到了午夜，海面平静下来，光束继续扫过水面。",
    ];
    (0..4).map(|k| S[(seed + k) % S.len()]).collect::<Vec<_>>().join("")
}

fn chapters() -> (Vec<Chapter>, Vec<Chapter>) {
    let en = |n: usize, paragraphs: usize| -> String {
        let mut s = format!("## Chapter {n}\n\n");
        for p in 0..paragraphs {
            s.push_str(&format!("{} ({n}.{})\n\n", en_paragraph(n + p), p + 1));
        }
        s
    };
    let zh = |n: usize, paragraphs: usize| -> String {
        let mut s = format!("## 第{}章\n\n", ["一", "二", "三"][n - 1]);
        for p in 0..paragraphs {
            s.push_str(&format!("{}（{n}.{}）\n\n", zh_paragraph(n + p), p + 1));
        }
        s
    };
    // Chapter 3 is longer than one 32 KiB chunk on both sides.
    let original = vec![
        Chapter { key: "chapter-1", title: "Chapter 1".into(), level: 2, counterpart: Some("chapter-1"), body: en(1, 6) },
        Chapter { key: "chapter-2", title: "Chapter 2".into(), level: 2, counterpart: Some("chapter-2"), body: en(2, 9) },
        Chapter { key: "chapter-3", title: "Chapter 3".into(), level: 2, counterpart: Some("chapter-3"), body: en(3, 140) },
    ];
    let translation = vec![
        Chapter { key: "front-1", title: "译者序".into(), level: 2, counterpart: None, body: "## 译者序\n\n这是一部合成的小说，用来检验原文、译文与章节对照的显示。书中所有文字都是虚构的样例。\n\n".into() },
        Chapter { key: "chapter-1", title: "第一章".into(), level: 2, counterpart: Some("chapter-1"), body: zh(1, 6) + "① 合成译注：守灯人的日志格式是虚构的样例。\n\n" },
        Chapter { key: "chapter-2", title: "第二章".into(), level: 2, counterpart: Some("chapter-2"), body: zh(2, 9) },
        Chapter { key: "chapter-3", title: "第三章".into(), level: 2, counterpart: Some("chapter-3"), body: zh(3, 200) },
    ];
    (original, translation)
}

fn body(side: &[Chapter]) -> String {
    side.iter().map(|c| c.body.as_str()).collect()
}

/// The canonical Markdown of the book item: its Chinese translation.
pub fn canonical_markdown() -> String {
    body(&chapters().1)
}

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

fn edition(side: &str, language: &str, chapters: &[Chapter]) -> serde_json::Value {
    let text = body(chapters);
    let mut at = 0;
    let list: Vec<_> = chapters
        .iter()
        .map(|c| {
            let start = at;
            at += c.body.len();
            serde_json::json!({"key": c.key, "title": c.title, "level": c.level, "start": start, "end": at, "counterpart": c.counterpart})
        })
        .collect();
    serde_json::json!({"side": side, "language": language, "sha256": sha(&text), "bytes": text.len(), "chapters": list})
}

/// Byte ranges of a chapter's blank-line blocks, separators included.
fn blocks(body: &str) -> Vec<[usize; 2]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < body.len() {
        let end = body[at..].find("\n\n").map_or(body.len(), |i| at + i + 2);
        out.push([at, end]);
        at = end;
    }
    out
}

fn span(b: &[[usize; 2]], from: usize, to: usize) -> Option<[usize; 2]> {
    Some([b[from][0], b[to][1]])
}

type Row = (Option<[usize; 2]>, Option<[usize; 2]>, &'static str, &'static str);

/// The stored paragraph index of a chapter: (version, status, rows).
fn index(key: &str, repaired: bool, o: &str, t: &str) -> Option<(String, &'static str, Vec<Row>)> {
    let (bo, bt) = (blocks(o), blocks(t));
    let m = |a: usize, b: usize, c: usize, d: usize, kind: &'static str| -> Row { (span(&bo, a, b), span(&bt, c, d), kind, "matched") };
    match (key, repaired) {
        // Heading, 1:1, n:1 (o2+o3 → t2), 1:n (o4 → t3+t4), and the translator's note.
        ("chapter-1", _) => Some(("a1".repeat(32), "aligned", vec![
            m(0, 0, 0, 0, "heading"), m(1, 1, 1, 1, "paragraph"), m(2, 3, 2, 2, "paragraph"), m(4, 4, 3, 4, "paragraph"),
            m(5, 5, 5, 5, "paragraph"), m(6, 6, 6, 6, "paragraph"), (None, span(&bt, 7, 7), "note", "translation_only"),
        ])),
        ("chapter-2", false) => {
            let mut rows: Vec<Row> = (0..6).map(|i| m(i, i, i, i, if i == 0 { "heading" } else { "paragraph" })).collect();
            rows.push((span(&bo, 6, bo.len() - 1), None, "paragraph", "unresolved"));
            rows.push((None, span(&bt, 6, bt.len() - 1), "paragraph", "unresolved"));
            Some(("b2".repeat(32), "partial", rows))
        }
        ("chapter-2", true) => Some(("b3".repeat(32), "aligned", (0..bo.len()).map(|i| m(i, i, i, i, if i == 0 { "heading" } else { "paragraph" })).collect())),
        ("chapter-3", true) => {
            let mut rows: Vec<Row> = (0..bo.len()).map(|i| m(i, i, i, i, if i == 0 { "heading" } else { "paragraph" })).collect();
            rows.push((None, span(&bt, bo.len(), bt.len() - 1), "paragraph", "unresolved"));
            Some(("c3".repeat(32), "partial", rows))
        }
        _ => None,
    }
}

/// Paragraph indexes and repair jobs of the synthetic book.
#[derive(Debug, Default)]
pub struct BookState {
    repaired: std::collections::HashSet<String>,
    jobs: Vec<Job>,
    /// The next admitted job fails (`reader_repair_failed`).
    pub fail_next: bool,
}

#[derive(Debug, Clone)]
struct Job {
    id: String,
    request_id: String,
    chapter: String,
    expected: String,
    revision: String,
    started: std::time::Instant,
    fail: bool,
    /// Set once the job committed: (result revision, version, status).
    done: Option<(String, String, &'static str)>,
}

/// Stage boundaries of a synthetic job (no model, no Library write).
const STAGES: [(u64, &str); 4] = [(400, "queued"), (1000, "formatting"), (1800, "matching"), (2200, "committing")];

/// What a reader command asks of the item: `Some(next revision)` when a repair
/// committed a formatting change.
pub struct Outcome {
    pub response: Response,
    pub bump: bool,
}

/// Answers a `reader` command for the book item at `revision`. `canonical` is the
/// item's current Markdown: an edited canonical text invalidates the manifest.
pub fn handle_with(state: &mut BookState, request: &ReaderRequest, max_sensitivity: Sensitivity, revision: &str, next_revision: &str, canonical: &str) -> Outcome {
    let err = |code: &str| Outcome { response: Response { ok: false, reason_code: Some(code.into()), ..Default::default() }, bump: false };
    let ok = |v: serde_json::Value| Outcome { response: Response { ok: true, reader: Some(v), ..Default::default() }, bump: false };
    if max_sensitivity < Sensitivity::Private {
        return err("library_resource_not_found");
    }
    let (original, translation) = chapters();
    let job_wire = |j: &Job| {
        let ms = j.started.elapsed().as_millis() as u64;
        let (state, stage) = match (&j.done, j.fail) {
            (Some(_), _) => ("succeeded", "done"),
            (None, true) if ms >= 1000 => ("failed", "done"),
            (None, _) => match STAGES.iter().find(|(t, _)| ms < *t) { Some((_, "queued")) => ("queued", "queued"), Some((_, s)) => ("running", *s), None => ("running", "committing") },
        };
        let (rev, ver, st) = j.done.clone().map_or((None, None, None), |(r, v, s)| (Some(r), Some(v), Some(s)));
        serde_json::json!({"job": {
            "job_id": j.id, "request_id": j.request_id, "item_id": BOOK_ITEM, "revision_id": j.revision, "chapter": j.chapter,
            "state": state, "stage": stage, "result_revision_id": rev, "alignment_version": ver, "alignment_status": st,
            "error_code": (state == "failed").then_some("reader_repair_failed"),
            "budget": {"max_requests": 2, "max_input_bytes_per_request": 48000, "max_completion_tokens_per_request": 4000},
            "usage": {"requests_dispatched": 0, "reported_total_tokens": 0, "unknown_usage_requests": 0}, "elapsed_ms": ms,
        }})
    };
    if let ReaderRequest::AlignStatus { item_id, job_id } = request {
        if item_id != BOOK_ITEM { return err("library_resource_not_found"); }
        let Some(i) = state.jobs.iter().position(|j| &j.id == job_id) else { return err("reader_job_not_found") };
        let finished = state.jobs[i].done.is_none() && !state.jobs[i].fail && state.jobs[i].started.elapsed().as_millis() as u64 >= 2200;
        let mut bump = false;
        if finished {
            // Commit: chapter 3's repair changed its formatting (a new revision);
            // chapter 2's only rebuilt the index (same revision).
            let chapter = state.jobs[i].chapter.clone();
            state.repaired.insert(chapter.clone());
            let o = &original.iter().find(|c| c.key == chapter).unwrap().body;
            let t = &translation.iter().find(|c| c.key == chapter).unwrap().body;
            let (version, status, _) = index(&chapter, true, o, t).unwrap();
            bump = chapter == "chapter-3";
            let rev = if bump { next_revision.to_string() } else { revision.to_string() };
            state.jobs[i].done = Some((rev, version, status));
        }
        return Outcome { bump, ..ok(job_wire(&state.jobs[i])) };
    }
    if let ReaderRequest::AlignRequest { item_id, revision_id, chapter, request_id, expected_alignment_version } = request {
        if item_id != BOOK_ITEM { return err("library_resource_not_found"); }
        if let Some(j) = state.jobs.iter().find(|j| &j.request_id == request_id) {
            return if &j.chapter == chapter && &j.expected == expected_alignment_version && &j.revision == revision_id { ok(job_wire(j)) } else { err("request_identity_conflict") };
        }
        if revision_id != revision { return err("stale_item_revision"); }
        if request_id.is_empty() || request_id.len() > 128 || !request_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return err("invalid_reader_alignment_request");
        }
        let (Some(o), Some(t)) = (original.iter().find(|c| c.key == chapter), translation.iter().find(|c| Some(c.key) == original.iter().find(|x| x.key == chapter).and_then(|x| x.counterpart))) else { return err("reader_chapter_not_found") };
        let current = index(chapter, state.repaired.contains(chapter), &o.body, &t.body).map_or("missing".to_string(), |x| x.0);
        if &current != expected_alignment_version { return err("stale_chapter_alignment"); }
        if state.jobs.iter().any(|j| j.done.is_none() && !(j.fail && j.started.elapsed().as_millis() >= 1000) ) { return err("reader_job_in_progress"); }
        let job = Job { id: format!("job_fixture_book_{}", state.jobs.len() + 1), request_id: request_id.clone(), chapter: chapter.clone(), expected: current, revision: revision.into(), started: std::time::Instant::now(), fail: std::mem::take(&mut state.fail_next), done: None };
        let wire = job_wire(&job);
        state.jobs.push(job);
        return ok(wire);
    }
    let mut out = handle(request, max_sensitivity, revision, canonical);
    if let (true, ReaderRequest::Alignment { item_id, revision_id, chapter, offset, limit, expected_version }) = (out.ok || out.reason_code.is_none(), request) {
        if item_id != BOOK_ITEM { return err("library_resource_not_found"); }
        if revision_id != revision { return err("stale_item_revision"); }
        if !(1..=256).contains(limit) { return err("invalid_reader_alignment_request"); }
        let Some(o) = original.iter().find(|c| c.key == chapter) else { return err("reader_chapter_not_found") };
        let t = o.counterpart.and_then(|k| translation.iter().find(|c| c.key == k));
        let found = t.and_then(|t| index(chapter, state.repaired.contains(chapter), &o.body, &t.body));
        let (version, status, rows) = found.unwrap_or(("missing".into(), "none", Vec::new()));
        if expected_version != "any" && expected_version != &version { return err("stale_chapter_alignment"); }
        let page: Vec<_> = rows.iter().skip(*offset).take(*limit).map(|(o, t, kind, rel)| serde_json::json!({"o": o, "t": t, "kind": kind, "relation": rel})).collect();
        let next = (offset + page.len() < rows.len()).then_some(offset + page.len());
        out = Response { ok: true, reader: Some(serde_json::json!({
            "schema_version": 1, "item_id": item_id, "revision_id": revision_id, "version": version,
            "original_chapter": chapter, "translation_chapter": t.map(|t| t.key),
            "original_sha256": sha(&o.body), "translation_sha256": t.map(|t| sha(&t.body)),
            "original_bytes": o.body.len(), "translation_bytes": t.map(|t| t.body.len()),
            "status": status, "quality": "not_certified", "offset": offset, "total": rows.len(), "next_offset": next, "pairs": page,
        })), ..Default::default() };
    }
    Outcome { response: out, bump: false }
}

fn handle(request: &ReaderRequest, max_sensitivity: Sensitivity, revision: &str, canonical: &str) -> Response {
    let err = |code: &str| Response { ok: false, reason_code: Some(code.into()), ..Default::default() };
    if max_sensitivity < Sensitivity::Private {
        return err("library_resource_not_found");
    }
    let (original, translation) = chapters();
    let (item_id, revision_id) = match request {
        ReaderRequest::Toc { item_id, revision_id } | ReaderRequest::Chunk { item_id, revision_id, .. } | ReaderRequest::Alignment { item_id, revision_id, .. } => (item_id, revision_id),
        _ => return Response::default(),
    };
    if item_id != BOOK_ITEM {
        return err("library_resource_not_found");
    }
    if revision_id != revision {
        return err("stale_item_revision");
    }
    if canonical != body(&translation) {
        return err("stale_book_manifest");
    }
    let reply = match request {
        // Answered by `handle_with`.
        ReaderRequest::Alignment { .. } | ReaderRequest::AlignRequest { .. } | ReaderRequest::AlignStatus { .. } => return Response::default(),
        // No `reader_chapter_edit_v1` here: never advertised, never answered.
        ReaderRequest::EditBegin { .. } | ReaderRequest::EditChunk { .. } | ReaderRequest::EditCommit { .. } => return Response::default(),
        // No `reader_chapter_translation_v1` here either.
        ReaderRequest::TranslatePreview { .. } | ReaderRequest::TranslateRequest { .. } | ReaderRequest::TranslateStatus { .. } | ReaderRequest::TranslateChunk { .. }
            | ReaderRequest::TranslateMetadata { .. } | ReaderRequest::TranslateAlignment { .. } => return Response::default(),
        // Nor `reader_revision_text_v1`.
        ReaderRequest::RevisionChunk { .. } => return Response::default(),
        // Nor content formatting.
        ReaderRequest::FormatPreview(_) | ReaderRequest::FormatTargets { .. } | ReaderRequest::FormatRequest { .. } | ReaderRequest::FormatStatus { .. }
        | ReaderRequest::FormatResults { .. } | ReaderRequest::FormatChanges(_) | ReaderRequest::FormatRestore { .. } | ReaderRequest::FormatResume { .. } => return Response::default(),
        // Answered by the fixture for every item (`fixture::reader_ordinary`).
        ReaderRequest::State { .. } | ReaderRequest::Save { .. } | ReaderRequest::Resume | ReaderRequest::AnnotationsList { .. } | ReaderRequest::AnnotationsEdit { .. }
        | ReaderRequest::AiAnnotationsList { .. } | ReaderRequest::AiAnnotationsEdit { .. }
        | ReaderRequest::PassesList { .. } | ReaderRequest::PassesStats { .. } | ReaderRequest::PassesEdit { .. } | ReaderRequest::PassesTimingStart { .. } | ReaderRequest::PassesTimingReport { .. } => return Response::default(),
        ReaderRequest::Toc { .. } => serde_json::json!({
            "item_id": item_id, "revision_id": revision_id, "canonical_sha256": sha(canonical),
            "default_side": "translation", "alignment": "chapter_order",
            "editions": [edition("original", "en", &original), edition("translation", "zh", &translation)],
        }),
        ReaderRequest::Chunk { side, chapter, offset, limit, .. } => {
            if !(4..=crate::book::CHUNK_LIMIT).contains(limit) {
                return err("invalid_reader_limit");
            }
            let chapters = match side.as_str() {
                "original" => &original,
                "translation" => &translation,
                _ => return err("reader_side_not_found"),
            };
            let Some(index) = chapters.iter().position(|c| c.key == chapter) else { return err("reader_chapter_not_found") };
            let text = body(chapters);
            let ch_start: usize = chapters[..index].iter().map(|c| c.body.len()).sum();
            let ch_end = ch_start + chapters[index].body.len();
            let start = ch_start + offset;
            if start > ch_end || !text.is_char_boundary(start) {
                return err("invalid_reader_offset");
            }
            let mut end = (start + limit).min(ch_end);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            serde_json::json!({
                "item_id": item_id, "revision_id": revision_id, "side": side, "sha256": sha(&text),
                "evidence_id": format!("evidence_fixture_book_{side}"), "chapter": chapter, "start": start, "end": end,
                "next_offset": (end < ch_end).then_some(end - ch_start), "markdown": &text[start..end],
            })
        }
    };
    Response { ok: true, reader: Some(reply), ..Default::default() }
}
