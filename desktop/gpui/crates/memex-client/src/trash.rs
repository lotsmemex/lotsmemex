//! Recoverable trash (`dedup_trash_v1`): items the owner's dedup run moved out
//! of the library. They can be listed (`trash_list`, 50 per page) and restored
//! (`item_trash_restore`); there is no delete-forever. A trashed item reads as
//! not found, so it cannot be opened until it is restored.

use crate::{tx, txf};
use serde::{Deserialize, Serialize};

pub const TRASH_FEATURE: &str = "dedup_trash_v1";

/// Pages read at most for one listing (50 items each).
pub const MAX_TRASH_PAGES: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashEntry {
    pub item_id: String,
    pub revision_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub source_url: String,
    /// RFC 3339.
    #[serde(default)]
    pub trashed_at: String,
    /// Fixed code, e.g. `duplicate`.
    #[serde(default)]
    pub reason: String,
    /// The copy that stayed in the library (duplicates only).
    #[serde(default)]
    pub kept_item_id: Option<String>,
    #[serde(default)]
    pub plan_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashRestore {
    pub item_id: String,
    pub revision_id: String,
    pub restored: bool,
}

impl TrashEntry {
    /// Host of `source_url` (http/https only), without `www.`.
    pub fn source_host(&self) -> Option<String> {
        let url = self.source_url.trim();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return None;
        }
        let rest = url.split_once("://")?.1;
        let host = rest.split(['/', '?', '#']).next()?.rsplit('@').next()?;
        let host = host.strip_prefix("www.").unwrap_or(host);
        (!host.is_empty()).then(|| host.to_string())
    }

    /// 移入时间 in local time (minute precision), or the raw value when unreadable.
    pub fn trashed_label(&self) -> String {
        crate::biblio::added_at_local(Some(&self.trashed_at)).unwrap_or_else(|| self.trashed_at.clone())
    }

    /// Why the item is in the trash: 重复（保留：<kept title or id>）, other codes as given.
    pub fn reason_label(&self, kept_title: Option<&str>) -> String {
        match (self.reason.as_str(), &self.kept_item_id) {
            ("duplicate", Some(kept)) => {
                let kept = kept_title.map(str::trim).filter(|t| !t.is_empty()).unwrap_or(kept);
                txf("重复（保留：{0}）", &[&kept])
            }
            ("duplicate", None) => tx("重复").into(),
            ("", _) => tx("代理未说明原因").into(),
            (other, _) => other.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_round_trip_and_label_their_reason_host_and_time() {
        let wire = r#"{"item_id":"item_b","revision_id":"rev_b","title":"Dup","source_url":"https://www.weekly.example/a/1?x=2","trashed_at":"2026-11-01T08:30:00Z","reason":"duplicate","kept_item_id":"item_a","plan_id":"plan_1"}"#;
        let e: TrashEntry = serde_json::from_str(wire).unwrap();
        assert_eq!(serde_json::from_value::<TrashEntry>(serde_json::to_value(&e).unwrap()).unwrap(), e);
        assert_eq!(e.source_host().as_deref(), Some("weekly.example"));
        assert!(e.trashed_label().starts_with("2026-11-0"), "{}", e.trashed_label());
        // The interface language is global (another test switches it): check the content.
        assert!(e.reason_label(Some("原文标题")).contains("原文标题"));
        assert!(e.reason_label(None).contains("item_a"));
        assert!(["重复（保留：x）", "Duplicate (kept: x)"].contains(&e.reason_label(Some("x")).as_str()));
        // Absent optional fields (an entry without a plan) and an unknown reason.
        let e: TrashEntry = serde_json::from_str(r#"{"item_id":"i","revision_id":"r","title":"t","source_url":"file:///x","trashed_at":"bad","reason":"manual"}"#).unwrap();
        assert_eq!((e.kept_item_id.clone(), e.plan_id.clone()), (None, None));
        assert_eq!(e.source_host(), None);
        assert_eq!(e.trashed_label(), "bad");
    }
}
