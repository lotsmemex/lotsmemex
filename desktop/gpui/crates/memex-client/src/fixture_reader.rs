//! Synthetic owner reader ops for ordinary items: the source toc (`section-*`
//! chapters split at headings), B2 state/save/resume with per-item CAS, and
//! `reader_annotations_v1` list/edit with a monotonic ledger version (no ABA)
//! and a single last-mutation receipt, as the frozen contracts describe.

use crate::book::ReaderRequest;
use crate::reader_state::{Annotation, AnnotationChange, MarkAnchor, ReaderAnchor, ReaderState};
use crate::Response;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

fn sha(s: &[u8]) -> String {
    format!("{:x}", Sha256::digest(s))
}

/// (key, title, level, start, end) of the source sections of `markdown`.
pub(crate) fn sections(markdown: &str) -> Vec<(String, String, u8, usize, usize)> {
    let mut starts: Vec<(usize, String, u8)> = vec![];
    let mut at = 0;
    for line in markdown.split_inclusive('\n') {
        let level = line.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&level) && line.as_bytes().get(level) == Some(&b' ') {
            starts.push((at, line[level..].trim().to_string(), level as u8));
        }
        at += line.len();
    }
    if starts.first().is_none_or(|s| s.0 != 0) {
        starts.insert(0, (0, String::new(), 0));
    }
    let mut out = vec![];
    for (k, (start, title, level)) in starts.iter().enumerate() {
        let end = starts.get(k + 1).map_or(markdown.len(), |n| n.0);
        out.push((format!("section-{k}"), title.clone(), *level, *start, end));
    }
    out
}

#[derive(Default)]
struct Ledger {
    counter: u64,
    marks: Vec<Annotation>,
    /// (expected version, canonical change JSON, result) of the last mutation.
    last: Option<(String, String, serde_json::Value)>,
}

#[derive(Default)]
pub(crate) struct ReaderStore {
    states: HashMap<String, ReaderState>,
    latest: Option<String>,
    ledgers: HashMap<String, Ledger>,
}

fn err(code: &str) -> Response {
    Response { ok: false, reason_code: Some(code.into()), ..Default::default() }
}
fn ok(v: serde_json::Value) -> Response {
    Response { ok: true, reader: Some(v), ..Default::default() }
}

fn state_version(s: &ReaderState) -> String {
    sha(&serde_json::to_vec(s).unwrap())
}

impl ReaderStore {
    /// Answers `request` when it is one of these ops; `None` leaves it to the book fixture.
    pub(crate) fn handle(&mut self, request: &ReaderRequest, item: &dyn Fn(&str) -> Option<(String, String)>, annotations: bool) -> Option<Response> {
        let get = |id: &str| item(id);
        Some(match request {
            ReaderRequest::Toc { item_id, revision_id } => {
                let (rev, md) = get(item_id)?;
                if *revision_id != rev { return Some(err("stale_item_revision")); }
                let chapters: Vec<_> = sections(&md).into_iter().map(|(key, title, level, start, end)| serde_json::json!({"key": key, "title": title, "level": level, "start": start, "end": end})).collect();
                ok(serde_json::json!({"item_id": item_id, "revision_id": rev, "default_side": "source", "alignment": "none",
                    "editions": [{"side": "source", "language": "und", "sha256": sha(md.as_bytes()), "bytes": md.len(), "chapters": chapters}]}))
            }
            ReaderRequest::State { item_id } => {
                if get(item_id).is_none() { return Some(err("library_resource_not_found")); }
                let s = self.states.get(item_id).cloned().unwrap_or_default();
                ok(serde_json::json!({"version": state_version(&s), "state": s}))
            }
            ReaderRequest::Resume => match self.latest.clone().filter(|id| get(id).is_some()) {
                Some(id) => ok(serde_json::json!([id, self.states.get(&id).cloned().unwrap_or_default()])),
                None => ok(serde_json::Value::Null),
            },
            ReaderRequest::Save { item_id, expected_version, state } => {
                let Some((rev, md)) = get(item_id) else { return Some(err("library_resource_not_found")) };
                let previous = self.states.get(item_id).cloned().unwrap_or_default();
                let new_anchors = state.position.iter().chain(state.bookmarks.iter().filter(|b| !previous.bookmarks.contains(b)).map(|b| &b.anchor));
                for a in new_anchors {
                    if let Some(code) = check_anchor(a, &rev, &md) { return Some(err(code)); }
                }
                if previous == *state && self.states.contains_key(item_id) {
                    return Some(ok(serde_json::json!({"version": state_version(&previous), "state": previous})));
                }
                if state_version(&previous) != *expected_version { return Some(err("stale_reader_state")); }
                if previous.visit_id != state.visit_id { self.latest = Some(item_id.clone()); }
                self.states.insert(item_id.clone(), state.clone());
                ok(serde_json::json!({"version": state_version(state), "state": state}))
            }
            ReaderRequest::AnnotationsList { item_id, offset, limit, expected_version } => {
                if !annotations { return Some(err("invalid_request")); }
                let Some((rev, md)) = get(item_id) else { return Some(err("library_resource_not_found")) };
                if !(1..=100).contains(limit) { return Some(err("invalid_reader_limit")); }
                let ledger = self.ledgers.entry(item_id.clone()).or_default();
                let version = format!("ann-v{}", ledger.counter);
                if expected_version.as_ref().is_some_and(|v| *v != version) { return Some(err("stale_reader_annotations")); }
                let mut marks = ledger.marks.clone();
                marks.sort_by(|a, b| a.id.cmp(&b.id));
                let page: Vec<_> = marks.iter().skip(*offset).take(*limit).map(|m| {
                    let current = m.anchor.revision_id == rev && m.anchor.sha256 == sha(md.as_bytes());
                    serde_json::json!({"id": m.id, "style": m.style, "anchor": m.anchor, "status": if current { "current" } else { "stale" }})
                }).collect();
                let next = (offset + page.len() < marks.len()).then_some(offset + page.len());
                ok(serde_json::json!({"item_id": item_id, "version": version, "annotations": page, "total": marks.len(), "next_offset": next}))
            }
            ReaderRequest::AnnotationsEdit { item_id, expected_version, change } => {
                if !annotations { return Some(err("invalid_request")); }
                let Some((rev, md)) = get(item_id) else { return Some(err("library_resource_not_found")) };
                let ledger = self.ledgers.entry(item_id.clone()).or_default();
                let version = format!("ann-v{}", ledger.counter);
                let key = serde_json::to_string(change).unwrap();
                if *expected_version != version {
                    // The immediate exact retry of the last mutation returns its result.
                    if let Some((e, c, result)) = &ledger.last {
                        if e == expected_version && *c == key { return Some(ok(result.clone())); }
                    }
                    return Some(err("stale_reader_annotations"));
                }
                let (id, outcome) = match change {
                    AnnotationChange::Put { annotation } => {
                        if let Some(code) = check_mark(&annotation.anchor, &rev, &md) { return Some(err(code)); }
                        match ledger.marks.iter_mut().find(|m| m.id == annotation.id) {
                            Some(m) if m.anchor != annotation.anchor => return Some(err("reader_annotation_conflict")),
                            Some(m) if m.style == annotation.style => return Some(err("reader_annotation_conflict")),
                            Some(m) => { m.style = annotation.style; (annotation.id.clone(), "updated") }
                            None => {
                                if ledger.marks.iter().any(|m| m.anchor == annotation.anchor && m.style == annotation.style) { return Some(err("reader_annotation_conflict")); }
                                ledger.marks.push(annotation.clone());
                                (annotation.id.clone(), "created")
                            }
                        }
                    }
                    AnnotationChange::Delete { id } => {
                        let Some(at) = ledger.marks.iter().position(|m| m.id == *id) else { return Some(err("reader_annotation_not_found")) };
                        ledger.marks.remove(at);
                        (id.clone(), "deleted")
                    }
                };
                ledger.counter += 1;
                let result = serde_json::json!({"item_id": item_id, "version": format!("ann-v{}", ledger.counter), "id": id, "outcome": outcome});
                ledger.last = Some((expected_version.clone(), key, result.clone()));
                ok(result)
            }
            _ => return None,
        })
    }
}

fn chapter(md: &str, key: &str) -> Option<(usize, usize)> {
    sections(md).into_iter().find(|s| s.0 == key).map(|s| (s.3, s.4))
}

fn check_anchor(a: &ReaderAnchor, rev: &str, md: &str) -> Option<&'static str> {
    if a.revision_id != rev { return Some("stale_item_revision"); }
    if a.side != "source" || a.sha256 != sha(md.as_bytes()) { return Some("invalid_reader_anchor"); }
    let Some((start, end)) = chapter(md, &a.chapter) else { return Some("invalid_reader_anchor") };
    (a.offset > end - start || !md.is_char_boundary(start + a.offset)).then_some("invalid_reader_anchor")
}

fn check_mark(a: &MarkAnchor, rev: &str, md: &str) -> Option<&'static str> {
    if a.revision_id != rev { return Some("stale_item_revision"); }
    if a.side != "source" || a.sha256 != sha(md.as_bytes()) || a.segments.is_empty() || a.segments.len() > 64 { return Some("invalid_reader_anchor"); }
    let mut last = 0usize;
    for s in &a.segments {
        let Some((start, end)) = chapter(md, &s.chapter) else { return Some("invalid_reader_anchor") };
        let (b, e) = (start + s.start, start + s.end);
        if s.start >= s.end || s.end > end - start || b < last || !md.is_char_boundary(b) || !md.is_char_boundary(e) { return Some("invalid_reader_anchor"); }
        last = e;
    }
    None
}
