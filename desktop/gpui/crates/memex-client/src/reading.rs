//! Revision-bound Markdown editing and manual reading marks.
use crate::{Client, ClientError, Command, ItemUpdate, Sensitivity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Deserialize)]
pub struct LibraryEntry {
    pub item_id: String,
    pub revision_id: String,
    pub title: String,
    pub manual_tags: Vec<String>,
    pub coordinate_space: String,
    pub content_sha256: String,
    pub total_bytes: usize,
    pub markdown: String,
    pub next_byte: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownEdit {
    pub item_id: String,
    pub expected_revision_id: String,
    pub request_id: String,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Underline { pub start: usize, pub end: usize }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadingAnnotations {
    pub item_id: String,
    pub revision_id: String,
    pub markdown_sha256: String,
    pub underlines: Vec<Underline>,
}

impl ReadingAnnotations {
    pub fn empty(item_id: String, revision_id: String, markdown: &str) -> Self {
        Self { item_id, revision_id, markdown_sha256: format!("sha256:{:x}", Sha256::digest(markdown.as_bytes())), underlines: vec![] }
    }
    pub fn version(&self) -> String { format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(self).expect("annotation JSON"))) }
    pub fn mark(&mut self, range: std::ops::Range<usize>) {
        self.underlines.push(Underline { start: range.start, end: range.end });
        self.underlines.sort_by_key(|r| r.start);
        let mut merged: Vec<Underline> = vec![];
        for r in self.underlines.drain(..) {
            if let Some(last) = merged.last_mut().filter(|last| last.end >= r.start) { last.end = last.end.max(r.end); }
            else { merged.push(r); }
        }
        self.underlines = merged;
    }
    pub fn unmark(&mut self, range: std::ops::Range<usize>) {
        self.underlines = self.underlines.iter().flat_map(|r| {
            if r.end <= range.start || r.start >= range.end { return vec![r.clone()]; }
            let mut rest = vec![];
            if r.start < range.start { rest.push(Underline { start: r.start, end: range.start }); }
            if r.end > range.end { rest.push(Underline { start: range.end, end: r.end }); }
            rest
        }).collect();
    }
}

impl Client {
    /// Existing bounded Store/context contract, used only when the legacy full
    /// response is too large. Every segment is revision/hash/length checked.
    pub(crate) fn segmented_item_read(&self, item_id: &str) -> Result<crate::CanonicalItem, ClientError> {
        let entry = self.backend.send(&Command::ScopedItemRead { item_id: item_id.into(), max_sensitivity: Sensitivity::Restricted })?
            .library_entry.ok_or(ClientError::InvalidFrame)?;
        if entry.item_id != item_id || entry.coordinate_space != "revision_markdown_utf8_v1"
            || entry.total_bytes > 16 * 1024 * 1024 || entry.markdown.len() > entry.total_bytes
            || entry.next_byte != (entry.markdown.len() < entry.total_bytes).then_some(entry.markdown.len()) {
            return Err(ClientError::InvalidFrame);
        }
        let mut markdown = entry.markdown;
        while markdown.len() < entry.total_bytes {
            let start = markdown.len();
            let mut end = (start + crate::MAX_CONTEXT_BYTES).min(entry.total_bytes);
            let mut slice = None;
            // A UTF-8 character is at most four bytes; narrow only the end boundary.
            for _ in 0..4 {
                match self.context_read(crate::ContextRequest { item_id: item_id.into(), expected_revision_id: entry.revision_id.clone(), start_byte: start, end_byte: end, max_sensitivity: Sensitivity::Restricted }) {
                    Ok(value) => { slice = Some(value); break; }
                    Err(ClientError::Daemon(code)) if code == "invalid_context_range" && end > start + 1 => end -= 1,
                    Err(error) => return Err(error),
                }
            }
            let slice = slice.ok_or(ClientError::InvalidFrame)?;
            if slice.coordinate_space != entry.coordinate_space || slice.content_sha256 != entry.content_sha256
                || slice.total_bytes != entry.total_bytes || slice.content_markdown.len() != end - start || end <= start {
                return Err(ClientError::InvalidFrame);
            }
            markdown.push_str(&slice.content_markdown);
        }
        if format!("sha256:{:x}", Sha256::digest(markdown.as_bytes())) != entry.content_sha256 { return Err(ClientError::InvalidFrame); }
        Ok(crate::CanonicalItem { item_id: entry.item_id, revision_id: entry.revision_id, title: entry.title, markdown, manual_tags: Some(entry.manual_tags) })
    }

    pub fn edit_markdown(&self, edit: MarkdownEdit) -> Result<ItemUpdate, ClientError> {
        self.backend.send(&Command::MarkdownEdit { max_sensitivity: Sensitivity::Restricted, edit })?.item_update.ok_or(ClientError::InvalidFrame)
    }
    pub fn reading_annotations(&self, item_id: String, revision_id: String) -> Result<ReadingAnnotations, ClientError> {
        self.backend.send(&Command::ReadingAnnotationsRead { item_id, revision_id, max_sensitivity: Sensitivity::Restricted })?.reading_annotations.ok_or(ClientError::InvalidFrame)
    }
    pub fn save_annotations(&self, expected_version: String, annotations: ReadingAnnotations) -> Result<ReadingAnnotations, ClientError> {
        self.backend.send(&Command::ReadingAnnotationsSave { expected_version, annotations, max_sensitivity: Sensitivity::Restricted })?.reading_annotations.ok_or(ClientError::InvalidFrame)
    }
}
