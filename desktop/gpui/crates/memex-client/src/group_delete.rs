//! Recoverable deletion of a library / folder subtree (`groups_v1.node_delete`):
//! four owner-local commands, never a `group_edit` op. A delete is confirmed
//! against a server preview (its token binds the whole affected subtree); the
//! result is read back from the tree, Inbox and deletion list, never taken from
//! a receipt's `outcome` (a replay returns the historical receipt). No permanent
//! delete exists.

use crate::{Client, ClientError, Command, Sensitivity};
use serde::Deserialize;

/// The fields a deletion reply may carry, flat beside `ok` (as `group_trash_list`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeleteFields {
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub confirmation_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub affected_node_count: Option<usize>,
    #[serde(default)]
    pub affected_membership_count: Option<usize>,
    #[serde(default)]
    pub distinct_item_count: Option<usize>,
    #[serde(default)]
    pub includes_descendants: Option<bool>,
    #[serde(default)]
    pub items_retained: Option<bool>,
    #[serde(default)]
    pub recoverable: Option<bool>,
    #[serde(default)]
    pub deletion_id: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub group_deletions: Option<Vec<Deletion>>,
}

/// What deleting `group_id` would remove, as the server counted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletePreview {
    pub group_id: String,
    pub name: String,
    pub confirmation_token: String,
    pub expires_at: String,
    pub affected_node_count: usize,
    pub affected_membership_count: usize,
    pub distinct_item_count: usize,
}

/// One outstanding deletion the owner can restore.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Deletion {
    pub deletion_id: String,
    pub group_id: String,
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub deleted_at: String,
}

/// Pages read at most (50 each) before giving up on a list that keeps going.
const MAX_PAGES: usize = 200;

impl Client {
    fn node_delete(&self) -> Result<(), ClientError> {
        if self.capabilities()?.group_node_delete { Ok(()) } else { Err(ClientError::Unsupported) }
    }

    /// The impact of deleting `group_id` and its confirmation token.
    pub fn group_delete_preview(&self, group_id: &str) -> Result<DeletePreview, ClientError> {
        self.node_delete()?;
        let f = self.backend.send(&Command::GroupDeletePreview { group_id: group_id.into(), max_sensitivity: Sensitivity::Restricted })?.group_delete;
        // A preview that does not promise the whole subtree, kept items and a way back is not one to confirm.
        if f.group_id.as_deref() != Some(group_id) || f.includes_descendants != Some(true) || f.items_retained != Some(true) || f.recoverable != Some(true) {
            return Err(ClientError::InvalidFrame);
        }
        Ok(DeletePreview {
            group_id: group_id.into(),
            name: f.name.ok_or(ClientError::InvalidFrame)?,
            confirmation_token: f.confirmation_token.filter(|t| !t.is_empty()).ok_or(ClientError::InvalidFrame)?,
            expires_at: f.expires_at.unwrap_or_default(),
            affected_node_count: f.affected_node_count.ok_or(ClientError::InvalidFrame)?,
            affected_membership_count: f.affected_membership_count.ok_or(ClientError::InvalidFrame)?,
            distinct_item_count: f.distinct_item_count.ok_or(ClientError::InvalidFrame)?,
        })
    }

    /// Deletes the previewed subtree. Returns the deletion id; the caller reads
    /// the tree and list again instead of trusting the receipt's outcome.
    pub fn group_delete(&self, group_id: &str, confirmation_token: &str, request_id: &str) -> Result<String, ClientError> {
        self.node_delete()?;
        let f = self.backend.send(&Command::GroupDelete { group_id: group_id.into(), confirmation_token: confirmation_token.into(), request_id: request_id.into(), max_sensitivity: Sensitivity::Restricted })?.group_delete;
        f.deletion_id.ok_or(ClientError::InvalidFrame)
    }

    /// Every outstanding deletion (all pages).
    pub fn group_delete_list(&self) -> Result<Vec<Deletion>, ClientError> {
        self.node_delete()?;
        let mut out = vec![];
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let r = self.backend.send(&Command::GroupDeleteList { cursor: cursor.clone(), max_sensitivity: Sensitivity::Restricted })?;
            out.extend(r.group_delete.group_deletions.ok_or(ClientError::InvalidFrame)?);
            match r.next_cursor {
                Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
                Some(_) => return Err(ClientError::InvalidFrame),
                None => return Ok(out),
            }
        }
        Err(ClientError::InvalidFrame)
    }

    /// Restores one deletion (the subtree as it was deleted; later deletions stay).
    pub fn group_delete_restore(&self, deletion_id: &str, request_id: &str) -> Result<(), ClientError> {
        self.node_delete()?;
        let f = self.backend.send(&Command::GroupDeleteRestore { deletion_id: deletion_id.into(), request_id: request_id.into(), max_sensitivity: Sensitivity::Restricted })?.group_delete;
        f.deletion_id.filter(|d| d == deletion_id).map(|_| ()).ok_or(ClientError::InvalidFrame)
    }
}
