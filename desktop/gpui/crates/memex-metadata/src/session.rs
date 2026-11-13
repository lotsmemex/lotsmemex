//! Metadata edit session: title/tag drafts against one loaded revision.
//!
//! Rules carried over from the AppKit client: Save sends the loaded revision as
//! `expected_revision_id`; a stale revision locks the editor until Cancel reloads;
//! while saving nothing can be edited or cancelled; dirty, saving and conflict all
//! block navigation away from the item.

use memex_client::{tx, txf, ClientError, ItemDetail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditStatus {
    Clean,
    Dirty,
    Saving,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditSession {
    item_id: Option<String>,
    revision_id: Option<String>,
    original_title: String,
    original_tags: Vec<String>,
    pub title: String,
    pub tags: Vec<String>,
    status: EditStatus,
    message: Option<String>,
}

impl Default for EditSession {
    fn default() -> Self {
        Self {
            item_id: None,
            revision_id: None,
            original_title: String::new(),
            original_tags: vec![],
            title: String::new(),
            tags: vec![],
            status: EditStatus::Clean,
            message: None,
        }
    }
}

pub fn normalize_tags(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = values.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    out.sort_by_key(|t| t.to_lowercase());
    out.dedup_by(|a, b| a.to_lowercase() == b.to_lowercase());
    out
}

impl EditSession {
    pub fn item_id(&self) -> Option<&str> {
        self.item_id.as_deref()
    }
    pub fn revision_id(&self) -> Option<&str> {
        self.revision_id.as_deref()
    }
    pub fn status(&self) -> EditStatus {
        self.status
    }
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn begin(&mut self, detail: &ItemDetail) {
        let item = &detail.canonical;
        self.item_id = Some(item.item_id.clone());
        self.revision_id = Some(item.revision_id.clone());
        self.original_title = item.title.clone();
        self.original_tags = normalize_tags(item.manual_tags.as_deref().unwrap_or_default());
        self.title = item.title.clone();
        self.tags = self.original_tags.clone();
        self.status = EditStatus::Clean;
        self.message = None;
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn is_dirty(&self) -> bool {
        self.title != self.original_title || normalize_tags(&self.tags) != self.original_tags
    }
    pub fn can_edit(&self) -> bool {
        self.item_id.is_some() && !matches!(self.status, EditStatus::Saving | EditStatus::Conflict)
    }
    pub fn can_save(&self) -> bool {
        self.can_edit() && self.is_dirty() && !self.title.trim().is_empty()
    }
    pub fn blocks_navigation(&self) -> bool {
        self.is_dirty() || matches!(self.status, EditStatus::Saving | EditStatus::Conflict)
    }
    pub fn can_cancel(&self) -> bool {
        self.blocks_navigation() && self.status != EditStatus::Saving
    }

    pub fn set_title(&mut self, title: &str) {
        if self.can_edit() {
            self.title = title.to_string();
            self.refresh_status();
        }
    }

    pub fn add_tag(&mut self, tag: &str) {
        if self.can_edit() && !tag.trim().is_empty() {
            let mut tags = self.tags.clone();
            tags.push(tag.to_string());
            self.tags = normalize_tags(&tags);
            self.refresh_status();
        }
    }

    pub fn remove_tag(&mut self, tag: &str) {
        if self.can_edit() {
            self.tags.retain(|t| t != tag);
            self.refresh_status();
        }
    }

    fn refresh_status(&mut self) {
        self.message = None;
        self.status = if self.is_dirty() { EditStatus::Dirty } else { EditStatus::Clean };
    }

    /// Discards edits. After a conflict the caller reloads the item and calls `begin`.
    pub fn cancel(&mut self) {
        if self.status == EditStatus::Saving {
            return;
        }
        self.title = self.original_title.clone();
        self.tags = self.original_tags.clone();
        self.refresh_status();
    }

    /// Returns the request to send, marking the session as saving.
    pub fn start_save(&mut self) -> Option<SaveRequest> {
        if !self.can_save() {
            return None;
        }
        self.status = EditStatus::Saving;
        Some(SaveRequest {
            item_id: self.item_id.clone()?,
            expected_revision_id: self.revision_id.clone()?,
            title: self.title.trim().to_string(),
            tags: normalize_tags(&self.tags),
        })
    }

    /// The bibliographic writer reported `stale_item_revision`: same lock as a
    /// stale title/tag save (Cancel reloads; nothing is rebased).
    pub fn mark_conflict(&mut self, message: String) {
        if self.item_id.is_some() {
            self.status = EditStatus::Conflict;
            self.message = Some(message);
        }
    }

    /// The bibliographic part of a save moved the record to `revision_id`.
    pub fn advance_revision(&mut self, revision_id: String) {
        if self.item_id.is_some() {
            self.revision_id = Some(revision_id);
        }
    }

    /// A status line for the whole save (bibliographic outcome included).
    pub fn set_message(&mut self, message: Option<String>) {
        self.message = message;
    }

    /// Applies a save result for `request`; results for another item/revision are ignored.
    pub fn finish_save(&mut self, request: &SaveRequest, result: Result<String, ClientError>) {
        if self.item_id.as_deref() != Some(&request.item_id) || self.status != EditStatus::Saving {
            return;
        }
        match result {
            Ok(new_revision) => {
                self.revision_id = Some(new_revision);
                self.original_title = request.title.clone();
                self.original_tags = request.tags.clone();
                self.title = request.title.clone();
                self.tags = request.tags.clone();
                self.status = EditStatus::Clean;
                self.message = Some(tx("已保存").into());
            }
            Err(error) if error.is_stale_revision() => {
                self.status = EditStatus::Conflict;
                self.message = Some(error.message());
            }
            Err(error) => {
                self.status = if self.is_dirty() { EditStatus::Dirty } else { EditStatus::Clean };
                self.message = Some(txf("保存失败：{0}", &[&error.message()]));
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveRequest {
    pub item_id: String,
    pub expected_revision_id: String,
    pub title: String,
    pub tags: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_client::{fixture::FixtureBackend, Client};

    fn save(client: &Client, session: &mut EditSession) {
        let request = session.start_save().expect("save allowed");
        let result = client
            .update_metadata(&request.item_id, &request.expected_revision_id, &request.title, &request.tags)
            .map(|u| u.revision_id);
        session.finish_save(&request, result);
    }

    #[test]
    fn save_uses_loaded_revision_and_conflict_locks_until_reload() {
        let fixture = FixtureBackend::new(20);
        let client = Client::new(fixture.clone());
        let mut session = EditSession::default();
        session.begin(&client.item_detail("item_fixture_000005").unwrap());

        session.set_title("修改后的标题 🌹");
        session.add_tag("新标签");
        assert!(session.blocks_navigation() && session.can_save());
        save(&client, &mut session);
        assert_eq!(session.status(), EditStatus::Clean);
        let saved = client.item_read("item_fixture_000005").unwrap();
        assert_eq!(saved.title, "修改后的标题 🌹");
        assert_eq!(session.revision_id(), Some(saved.revision_id.as_str()));

        // Someone else edits; our next save must conflict and lock the editor.
        session.set_title("第二次修改");
        fixture.simulate_external_edit("item_fixture_000005");
        save(&client, &mut session);
        assert_eq!(session.status(), EditStatus::Conflict);
        assert!(session.blocks_navigation() && !session.can_save());
        session.set_title("被锁定时不能改");
        assert_eq!(session.title, "第二次修改");
        // Cancel + reload resumes editing on the new revision.
        session.cancel();
        session.begin(&client.item_detail("item_fixture_000005").unwrap());
        assert_eq!(session.status(), EditStatus::Clean);
        assert!(session.title.ends_with("（他处已修改）"));
    }

    #[test]
    fn saving_blocks_cancel_and_late_results_for_other_items_are_ignored() {
        let client = Client::new(FixtureBackend::new(20));
        let mut session = EditSession::default();
        session.begin(&client.item_detail("item_fixture_000001").unwrap());
        session.set_title("改");
        let request = session.start_save().unwrap();
        session.cancel();
        assert_eq!(session.status(), EditStatus::Saving);
        assert!(!session.can_cancel());
        let other = SaveRequest { item_id: "item_fixture_000002".into(), ..request.clone() };
        session.finish_save(&other, Ok("rev_x".into()));
        assert_eq!(session.status(), EditStatus::Saving);
        session.finish_save(&request, Err(ClientError::Timeout));
        assert_eq!(session.status(), EditStatus::Dirty);
        assert!(session.message().unwrap().contains("超时"));
    }

    #[test]
    fn tags_are_trimmed_deduplicated_and_blank_title_cannot_save() {
        let client = Client::new(FixtureBackend::new(5));
        let mut session = EditSession::default();
        session.begin(&client.item_detail("item_fixture_000000").unwrap());
        let before = session.tags.clone();
        session.add_tag(&format!("  {}  ", before[0]));
        assert_eq!(session.tags, before);
        assert!(!session.is_dirty());
        session.set_title("   ");
        assert!(session.is_dirty() && !session.can_save());
    }
}
