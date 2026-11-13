//! 补全预览 (md1 panel C) as pure state: which candidates may be chosen, the
//! default choice, `requires` gating, and the apply edit (preview id + chosen
//! candidate ids only; the agent applies its own frozen values).
//!
//! Defaults follow the contract: unchanged rows cannot be chosen, conflicts start
//! unchosen (keep the current value), at most one candidate per field, and a
//! candidate whose required field is not chosen is never silently completed.

use memex_client::biblio::{identifier_label, provider_label, LookupCandidate, LookupPreview, MetadataEdit, PartialDate, METADATA_SCHEMA_VERSION};
use memex_client::{tx, txf, ClientError};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupSheet {
    /// `doi | isbn` and the advertised provider.
    pub kind: String,
    pub provider: String,
    pub preview: Option<LookupPreview>,
    selected: BTreeSet<String>,
    pub loading: bool,
    pub applying: bool,
    /// Shown above the table or instead of it.
    pub error: Option<String>,
    /// The preview can no longer be applied; offer a new lookup.
    pub expired: bool,
    /// Apply whose outcome is unknown; a retry reuses its request identity.
    pending: Option<MetadataEdit>,
}

/// What the inspector does after an apply attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// A new revision exists; reload everything.
    Applied { revision_id: String },
    /// The item changed elsewhere: close the sheet, show the conflict.
    Conflict(String),
    /// Stay in the sheet with `error` set.
    Stay,
}

impl LookupSheet {
    pub fn loading(kind: &str, provider: &str) -> Self {
        Self { kind: kind.into(), provider: provider.into(), preview: None, selected: BTreeSet::new(), loading: true, applying: false, error: None, expired: false, pending: None }
    }

    pub fn set_result(&mut self, result: Result<LookupPreview, ClientError>) {
        self.loading = false;
        self.expired = false;
        self.pending = None;
        match result {
            Ok(preview) => {
                self.selected = default_selection(&preview.candidates);
                self.preview = Some(preview);
                self.error = None;
            }
            Err(e) => {
                self.preview = None;
                self.error = Some(lookup_error_message(&e, &self.kind, &self.provider));
            }
        }
    }

    pub fn is_selected(&self, id: &str) -> bool {
        self.selected.contains(id)
    }

    pub fn selectable(c: &LookupCandidate) -> bool {
        !c.unchanged
    }

    /// Toggle one candidate; choosing it unchooses any other for the same field.
    pub fn toggle(&mut self, id: &str) {
        let Some(preview) = &self.preview else { return };
        let Some(c) = preview.candidates.iter().find(|c| c.candidate_id == id) else { return };
        if !Self::selectable(c) || self.applying {
            return;
        }
        if !self.selected.remove(id) {
            for other in preview.candidates.iter().filter(|o| o.field == c.field) {
                self.selected.remove(&other.candidate_id);
            }
            self.selected.insert(id.to_string());
        }
        self.error = None;
    }

    fn chosen(&self) -> Vec<&LookupCandidate> {
        self.preview.iter().flat_map(|p| p.candidates.iter()).filter(|c| self.selected.contains(&c.candidate_id)).collect()
    }

    /// Chosen candidates whose required field is not chosen: (field, missing).
    pub fn unmet(&self) -> Vec<(String, String)> {
        let fields: BTreeSet<&str> = self.chosen().iter().map(|c| c.field.as_str()).collect();
        self.chosen()
            .iter()
            .flat_map(|c| c.requires.iter().filter(|r| !fields.contains(r.as_str())).map(|r| (c.field.clone(), r.clone())))
            .collect()
    }

    pub fn count(&self) -> usize {
        self.selected.len()
    }

    pub fn can_apply(&self) -> bool {
        self.preview.is_some() && !self.expired && !self.applying && self.count() > 0 && self.unmet().is_empty()
    }

    /// The apply edit, reusing the unknown-outcome request identity on retry.
    pub fn start_apply(&mut self, expected_revision_id: &str) -> Option<MetadataEdit> {
        if !self.can_apply() {
            return None;
        }
        let preview = self.preview.as_ref()?;
        let mut edit = MetadataEdit {
            metadata_schema_version: METADATA_SCHEMA_VERSION,
            item_id: preview.item_id.clone(),
            expected_revision_id: expected_revision_id.into(),
            request_id: String::new(),
            set: Default::default(),
            clear: vec![],
            preview_id: Some(preview.preview_id.clone()),
            // Stable order, so a retry has the same body.
            selected_candidate_ids: self.selected.iter().cloned().collect(),
        };
        edit.request_id = match &self.pending {
            Some(p) if p.same_body(&edit) => p.request_id.clone(),
            _ => memex_client::biblio::new_request_id(),
        };
        self.applying = true;
        self.error = None;
        Some(edit)
    }

    pub fn finish_apply(&mut self, sent: MetadataEdit, result: Result<memex_client::ItemUpdate, ClientError>) -> ApplyOutcome {
        self.applying = false;
        match result {
            Ok(receipt) => {
                self.pending = None;
                ApplyOutcome::Applied { revision_id: receipt.revision_id }
            }
            Err(e) if e.is_stale_revision() => {
                self.pending = None;
                ApplyOutcome::Conflict(e.message())
            }
            Err(e) => {
                if e.is_transport() {
                    self.pending = Some(sent);
                } else {
                    self.pending = None;
                }
                if matches!(&e, ClientError::Daemon(c) if c == "preview_expired" || c == "preview_unavailable" || c == "invalid_lookup_preview") {
                    self.expired = true;
                }
                self.error = Some(match e.field_error() {
                    Some((field, reason)) if reason.starts_with("requires:") => {
                        txf("“{0}”需要同时选择“{1}”", &[&field_label(&field), &field_label(&reason["requires:".len()..])])
                    }
                    _ if matches!(&e, ClientError::Daemon(c) if c == "metadata_unchanged") => tx("所选字段与现有内容一致，未产生新修订").into(),
                    _ => txf("应用失败：{0}", &[&e.message()]),
                });
                ApplyOutcome::Stay
            }
        }
    }
}

/// Changed, non-conflicting, first per field; then drop any whose required field
/// is not also chosen (never add a field on the user's behalf).
pub fn default_selection(candidates: &[LookupCandidate]) -> BTreeSet<String> {
    let mut fields = BTreeSet::new();
    let mut chosen: Vec<&LookupCandidate> = vec![];
    for c in candidates.iter().filter(|c| !c.unchanged && !c.conflict) {
        if fields.insert(c.field.clone()) {
            chosen.push(c);
        }
    }
    loop {
        let fields: BTreeSet<&str> = chosen.iter().map(|c| c.field.as_str()).collect();
        let before = chosen.len();
        chosen.retain(|c| c.requires.iter().all(|r| fields.contains(r.as_str())));
        if chosen.len() == before {
            break;
        }
    }
    chosen.into_iter().map(|c| c.candidate_id.clone()).collect()
}

/// Lookup failures in words that keep their meaning: not found is not invalid,
/// rate limiting says to wait, an expired preview asks for a new lookup.
pub fn lookup_error_message(e: &ClientError, kind: &str, provider: &str) -> String {
    let (kind, provider) = (identifier_label(kind), provider_label(provider).to_string());
    match e {
        ClientError::Daemon(c) if c == "lookup_not_found" => txf("{0} 没有找到这个 {1}。这不代表 {1} 无效，只是该来源没有收录。", &[&provider, &kind]),
        ClientError::Daemon(c) if c == "lookup_rate_limited" => txf("{0} 暂时限制了查询频率。请稍后再试（代理未提供具体的可重试时间）。", &[&provider]),
        ClientError::Daemon(c) if c == "lookup_unavailable" => txf("暂时无法连接 {0}，未生成预览。", &[&provider]),
        ClientError::Daemon(c) if c == "lookup_identifier_invalid" => txf("这个 {0} 未通过本地校验，未向 {1} 发出查询。", &[&kind, &provider]),
        _ => e.message(),
    }
}

/// Provider notes in words; unknown notes are shown as given.
pub fn note_text(note: &str) -> String {
    if let Some(n) = note.strip_prefix("crossref_reference_count:") {
        return txf("Crossref 报告了 {0} 条参考文献（本次未导入引用关系）", &[&n]);
    }
    match note {
        "cited_references_not_supplied" => tx("来源未提供参考文献列表").into(),
        "cited_references_exceed_bound" => tx("参考文献过多，未纳入预览").into(),
        "creators_not_resolved_author_keys_only" => tx("Open Library 只给出作者编号，没有作者姓名，作者未纳入预览").into(),
        "creator_without_name_skipped" => tx("有作者缺少姓名，已跳过").into(),
        "creators_exceed_bound" => tx("作者过多，未纳入预览").into(),
        "fixture_synthetic_candidates" => tx("合成数据：未联网，候选值为样例").into(),
        other => other.to_string(),
    }
}

/// Field names as the inspector labels them.
pub fn field_label(field: &str) -> String {
    tx(match field {
        "title" => "标题",
        "creators" => "作者",
        "institutions" => "机构",
        "identifiers" => "标识符",
        "published_date" => "发布日期",
        "journal_title" => "期刊",
        "publisher" => "出版社",
        "volume" => "卷",
        "issue" => "期",
        "pages" => "页",
        "publication_place" => "出版地",
        "edition" => "版次",
        "number_of_pages" => "页数",
        "series_title" => "丛书",
        "series_number" => "丛书编号",
        "language" => "语言",
        "keywords" => "关键词",
        "site_name" => "网站",
        "abstract_text" => "文献摘要",
        "cited_references" => "参考文献",
        other => return other.to_string(),
    })
    .into()
}

/// One cell of the preview table: a candidate value in words.
pub fn value_summary(field: &str, value: Option<&Value>) -> String {
    let Some(value) = value.filter(|v| !v.is_null()) else { return "—".into() };
    let list = |v: &Value| v.as_array().cloned().unwrap_or_default();
    match field {
        "creators" => {
            let names: Vec<String> = list(value)
                .iter()
                .filter_map(|c| serde_json::from_value::<memex_client::Creator>(c.clone()).ok())
                .map(|c| c.name())
                .collect();
            let n = names.len();
            if n > 1 { format!("{}（{}）", names.join("; "), txf("{0} 位", &[&n])) } else { names.join("; ") }
        }
        "institutions" => list(value).iter().filter_map(|i| i["name"].as_str().map(str::to_string)).collect::<Vec<_>>().join("; "),
        "identifiers" => list(value)
            .iter()
            .map(|i| format!("{} {}", identifier_label(i["kind"].as_str().unwrap_or_default()), i["value"].as_str().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("; "),
        "published_date" => serde_json::from_value::<PartialDate>(value.clone()).map(|d| d.display()).unwrap_or_default(),
        "keywords" => list(value).iter().filter_map(|k| k.as_str().map(str::to_string)).collect::<Vec<_>>().join("、"),
        "cited_references" => txf("{0} 条参考文献", &[&list(value).len()]),
        _ => match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_client::{fixture::FixtureBackend, Client, Sensitivity};

    fn sheet(client: &Client, id: &str) -> (LookupSheet, String) {
        let rev = client.item_read(id).unwrap().revision_id;
        let doi = client.bibliographic(id, &rev, Sensitivity::Restricted).unwrap().identifier("doi").unwrap().display();
        let mut s = LookupSheet::loading("doi", "crossref");
        s.set_result(client.lookup_preview(id, &rev, "doi", &doi, Sensitivity::Restricted));
        (s, rev)
    }

    /// After an apply the earlier choices are unchanged; pick the date instead.
    fn choose_date(s: &mut LookupSheet) {
        if s.count() == 0 {
            let id = id_of(s, "published_date");
            s.toggle(&id);
        }
    }

    fn id_of(s: &LookupSheet, field: &str) -> String {
        s.preview.as_ref().unwrap().candidates.iter().find(|c| c.field == field).unwrap().candidate_id.clone()
    }

    #[test]
    fn defaults_keep_conflicts_and_unchanged_rows_and_one_candidate_per_field() {
        let client = Client::new(FixtureBackend::new(8));
        let (mut s, _) = sheet(&client, "item_fixture_000001");
        let p = s.preview.clone().unwrap();
        for c in &p.candidates {
            if c.unchanged || c.conflict {
                assert!(!s.is_selected(&c.candidate_id), "{} must start unchosen", c.field);
            }
        }
        assert!(s.is_selected(&id_of(&s, "institutions")) && s.is_selected(&id_of(&s, "abstract_text")));
        // Unchanged rows cannot be chosen.
        s.toggle(&id_of(&s, "title"));
        assert!(!s.is_selected(&id_of(&s, "title")));
        // Two published_date candidates: choosing the second unchooses the first.
        let dates: Vec<String> = p.candidates.iter().filter(|c| c.field == "published_date").map(|c| c.candidate_id.clone()).collect();
        assert_eq!(dates.len(), 2);
        s.toggle(&dates[0]);
        s.toggle(&dates[1]);
        assert!(!s.is_selected(&dates[0]) && s.is_selected(&dates[1]));
    }

    #[test]
    fn creators_cannot_be_applied_without_their_institutions() {
        let client = Client::new(FixtureBackend::new(8));
        let (mut s, rev) = sheet(&client, "item_fixture_000001");
        s.toggle(&id_of(&s, "institutions")); // unchoose
        s.toggle(&id_of(&s, "creators"));
        assert_eq!(s.unmet(), [("creators".to_string(), "institutions".to_string())]);
        assert!(!s.can_apply() && s.start_apply(&rev).is_none());
        // The default selection never adds the dependency by itself either.
        let mut only = s.preview.clone().unwrap().candidates;
        only.retain(|c| c.field == "creators");
        only[0].conflict = false;
        assert!(default_selection(&only).is_empty());
        s.toggle(&id_of(&s, "institutions"));
        assert!(s.unmet().is_empty() && s.can_apply());
    }

    #[test]
    fn apply_retry_reuses_the_request_and_stale_maps_to_conflict() {
        let fixture = FixtureBackend::new(8);
        let client = Client::new(fixture.clone());
        let (mut s, rev) = sheet(&client, "item_fixture_000001");
        let first = s.start_apply(&rev).unwrap();
        assert!(s.applying && s.start_apply(&rev).is_none());
        // A lost reply keeps the identity; the retry has the same id and body.
        assert_eq!(s.finish_apply(first.clone(), Err(ClientError::Timeout)), ApplyOutcome::Stay);
        let retry = s.start_apply(&rev).unwrap();
        assert_eq!(retry.request_id, first.request_id);
        let outcome = s.finish_apply(retry.clone(), client.bibliographic_edit(retry, Sensitivity::Restricted));
        assert!(matches!(outcome, ApplyOutcome::Applied { .. }));
        // A preview for a revision that changed elsewhere is a conflict, not a rebase.
        let (mut s, rev) = sheet(&client, "item_fixture_000001");
        choose_date(&mut s);
        fixture.simulate_external_edit("item_fixture_000001");
        let edit = s.start_apply(&rev).unwrap();
        let outcome = s.finish_apply(edit.clone(), client.bibliographic_edit(edit, Sensitivity::Restricted));
        assert!(matches!(outcome, ApplyOutcome::Conflict(_)));
        // Expiry asks for a new lookup.
        let (mut s, rev) = sheet(&client, "item_fixture_000001");
        choose_date(&mut s);
        fixture.expire_lookup_previews();
        let edit = s.start_apply(&rev).unwrap();
        assert_eq!(s.finish_apply(edit.clone(), client.bibliographic_edit(edit, Sensitivity::Restricted)), ApplyOutcome::Stay);
        assert!(s.expired && !s.can_apply());
    }

    #[test]
    fn lookup_errors_keep_their_meaning() {
        let nf = lookup_error_message(&ClientError::Daemon("lookup_not_found".into()), "isbn", "open_library");
        assert!(nf.contains("Open Library") && nf.contains("ISBN") && nf.contains("不代表"));
        assert!(lookup_error_message(&ClientError::Daemon("lookup_rate_limited".into()), "doi", "crossref").contains("稍后"));
        assert_eq!(value_summary("published_date", Some(&serde_json::json!({"value":"1974","precision":"year","raw":null}))), "1974");
        assert_eq!(value_summary("creators", None), "—");
    }
}
