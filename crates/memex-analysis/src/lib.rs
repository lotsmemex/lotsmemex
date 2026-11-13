use anyhow::Result;
use chrono::DateTime;
use fs2::FileExt;
use memex_core::{ItemRevision, Sensitivity, SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

pub mod capture_quality;
mod context;
mod insights;
mod lineage;
mod metadata;
mod summary;
mod titles;
mod translation;
pub use context::*;
pub use insights::*;
pub use metadata::*;
pub use summary::*;
pub use titles::*;
pub use translation::*;

const MAX_ACTOR_FIELD_BYTES: usize = 256;
pub const FAITHFUL_SUMMARY_PROMPT_ID: &str = "memex.faithful-summary";
pub const FAITHFUL_TRANSLATION_PROMPT_ID: &str = "memex.faithful-translation";
pub const GENERATED_TEXT_PROMPT_VERSION: &str = "1";
pub const FAITHFUL_SUMMARY_PROMPT: &str = include_str!("../prompts/faithful-summary-v1.txt");
pub const FAITHFUL_TRANSLATION_PROMPT: &str =
    include_str!("../prompts/faithful-translation-v1.txt");
/// "AI 纪要": the summary job's current output (summary prompt version 3: minutes plus a final
/// `建议标题：` line, in the same model call). Version-2 minutes (no suggested title) stay
/// current and readable, verified against their own prompt. Version-1 faithful summaries stay
/// readable as summaries but are never shown as minutes.
pub const AI_MINUTES_PROMPT_ID: &str = "memex.ai-minutes";
pub const AI_MINUTES_PROMPT_VERSION: &str = "3";
pub const AI_MINUTES_PROMPT: &str = include_str!("../prompts/ai-minutes-v3.txt");
pub const AI_MINUTES_V2_PROMPT_VERSION: &str = "2";
pub const AI_MINUTES_V2_PROMPT: &str = include_str!("../prompts/ai-minutes-v2.txt");
/// Prefix of the optional last line of version-3 minutes.
pub const SUGGESTED_TITLE_PREFIX: &str = "建议标题";
const MAX_SUGGESTED_TITLE_CHARS: usize = 120;

/// Both minutes prompt versions are current minutes (no re-run is needed for version 2).
pub fn is_ai_minutes_version(prompt_version: &str) -> bool {
    prompt_version == AI_MINUTES_PROMPT_VERSION || prompt_version == AI_MINUTES_V2_PROMPT_VERSION
}

pub fn ai_minutes_prompt() -> (&'static str, &'static str, String) {
    (
        AI_MINUTES_PROMPT_ID,
        AI_MINUTES_PROMPT,
        format!("sha256:{:x}", Sha256::digest(AI_MINUTES_PROMPT)),
    )
}

pub(crate) fn ai_minutes_v2_prompt() -> (&'static str, &'static str, String) {
    (
        AI_MINUTES_PROMPT_ID,
        AI_MINUTES_V2_PROMPT,
        format!("sha256:{:x}", Sha256::digest(AI_MINUTES_V2_PROMPT)),
    )
}

/// Splits validated minutes text into the minutes shown to the reader and the optional
/// suggested title of its last line (`建议标题：…`). Text without such a line (version 2, or a
/// reply that omitted it) is returned unchanged with `None`. An unusable title line (empty,
/// too long, multi-line markup) is dropped from the minutes and yields `None`.
pub fn split_suggested_title(text: &str) -> (String, Option<String>) {
    let trimmed = text.trim_end();
    let (body, last) = match trimmed.rfind('\n') {
        Some(at) => (&trimmed[..at], trimmed[at + 1..].trim()),
        None => ("", trimmed.trim()),
    };
    let Some(rest) = last.strip_prefix(SUGGESTED_TITLE_PREFIX) else {
        return (text.to_owned(), None);
    };
    let Some(rest) = rest.strip_prefix('：').or_else(|| rest.strip_prefix(':')) else {
        return (text.to_owned(), None);
    };
    let title = rest
        .trim()
        .trim_matches(|c| matches!(c, '《' | '》' | '“' | '”' | '"' | '「' | '」'))
        .trim()
        .to_owned();
    let usable = !title.is_empty()
        && title.chars().count() <= MAX_SUGGESTED_TITLE_CHARS
        && !title.chars().any(char::is_control)
        && !title.starts_with('#');
    (body.trim_end().to_owned(), usable.then_some(title))
}

// Explicit reading profiles. Do not silently change prompts/provenance of queued
// or frozen jobs; these are selected only by an explicitly authorized host.
pub const CLASSICAL_TRANSLATION_PROMPT: &str =
    include_str!("../prompts/classical-translation-v1.txt");
pub const GROUNDED_ANSWER_PROMPT: &str = include_str!("../prompts/grounded-answer-v1.txt");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisKind {
    Claim,
    Summary,
    Note,
    Synthesis,
    Translation,
    CaptureQuality,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    Agent,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneratedTextPurpose {
    Summary,
    TitleTranslation,
    DocumentTranslation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextLengthUnit {
    Characters,
    Words,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextOutputLimit {
    pub unit: TextLengthUnit,
    pub maximum: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedTextDescriptor {
    pub purpose: GeneratedTextPurpose,
    pub source_language: String,
    pub target_language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_limit: Option<TextOutputLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation_job_id: Option<String>,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_sha256: Option<String>,
    pub prompt_id: String,
    pub prompt_version: String,
    pub prompt_sha256: String,
}

pub fn generated_text_prompt(
    purpose: GeneratedTextPurpose,
) -> (&'static str, &'static str, String) {
    let (id, text) = match purpose {
        GeneratedTextPurpose::Summary => (FAITHFUL_SUMMARY_PROMPT_ID, FAITHFUL_SUMMARY_PROMPT),
        GeneratedTextPurpose::TitleTranslation | GeneratedTextPurpose::DocumentTranslation => {
            (FAITHFUL_TRANSLATION_PROMPT_ID, FAITHFUL_TRANSLATION_PROMPT)
        }
    };
    (id, text, format!("sha256:{:x}", Sha256::digest(text)))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemRevisionRef {
    pub item_id: String,
    pub revision_id: String,
    pub revision_sha256: String,
    pub evidence_ids: Vec<String>,
}

pub fn item_revision_ref_matches(
    input: &ItemRevisionRef,
    item_id: &str,
    revision: &ItemRevision,
) -> Result<bool> {
    Ok(revision.item_id == item_id && *input == revision_ref(item_id, revision)?)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisRequest {
    pub schema_version: u32,
    pub analysis_kind: AnalysisKind,
    pub title: String,
    pub body_markdown: String,
    pub item_ids: Vec<String>,
    pub topics: Vec<String>,
    pub requested_sensitivity: Option<Sensitivity>,
    pub created_at: String,
    pub actor_kind: ActorKind,
    pub actor_id: String,
    pub tool_id: String,
    pub tool_version: String,
    pub config_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_text: Option<GeneratedTextDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisProvenance {
    pub actor_kind: ActorKind,
    pub actor_id: String,
    pub tool_id: String,
    pub tool_version: String,
    pub config_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisRecord {
    pub schema_version: u32,
    pub analysis_id: String,
    pub analysis_kind: AnalysisKind,
    pub title: String,
    pub body_markdown: String,
    pub inputs: Vec<ItemRevisionRef>,
    pub topics: Vec<String>,
    pub sensitivity: Sensitivity,
    pub created_at: String,
    pub provenance: AnalysisProvenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_text: Option<GeneratedTextDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStatus {
    Created,
    Replayed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisReceipt {
    pub analysis_id: String,
    pub status: ApplyStatus,
    pub sensitivity: Sensitivity,
    pub input_item_ids: Vec<String>,
    pub warning_codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisIndexEntry {
    pub analysis_id: String,
    pub analysis_kind: AnalysisKind,
    pub title: String,
    pub sensitivity: Sensitivity,
    pub created_at: String,
    pub input_item_ids: Vec<String>,
    pub topics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_text: Option<GeneratedTextDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkRelation {
    Supports,
    Contradicts,
    Corroborates,
    Contextualizes,
    DerivedFrom,
    Related,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkTargetKind {
    Analysis,
    ItemRevision,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkTargetRequest {
    pub kind: LinkTargetKind,
    pub id: String,
    pub revision_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRequest {
    pub schema_version: u32,
    pub from_analysis_id: String,
    pub target: LinkTargetRequest,
    pub relation: LinkRelation,
    pub rationale_markdown: String,
    pub created_at: String,
    pub actor_kind: ActorKind,
    pub actor_id: String,
    pub tool_id: String,
    pub tool_version: String,
    pub config_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisLink {
    pub schema_version: u32,
    pub link_id: String,
    pub from_analysis_id: String,
    pub target: LinkTargetRequest,
    pub relation: LinkRelation,
    pub rationale_markdown: String,
    pub created_at: String,
    pub actor_kind: ActorKind,
    pub actor_id: String,
    pub tool_id: String,
    pub tool_version: String,
    pub config_digest: String,
    pub sensitivity: Sensitivity,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkReceipt {
    pub link_id: String,
    pub status: ApplyStatus,
    pub sensitivity: Sensitivity,
    pub from_analysis_id: String,
    pub target: LinkTargetRequest,
    pub warning_codes: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallTargetKind {
    Analysis,
    ItemRevision,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallQuery {
    pub query: String,
    pub max_sensitivity: Sensitivity,
    pub limit: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallScoreBreakdown {
    pub exact_phrase: u32,
    pub title_terms: u32,
    pub topic_terms: u32,
    pub body_terms: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LexicalScore {
    pub score: u32,
    pub matched_terms: Vec<String>,
    pub score_breakdown: RecallScoreBreakdown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallHit {
    pub target_kind: RecallTargetKind,
    pub id: String,
    pub revision_id: Option<String>,
    pub title: String,
    pub snippet: String,
    pub sensitivity: Sensitivity,
    pub score: u32,
    pub matched_terms: Vec<String>,
    pub score_breakdown: RecallScoreBreakdown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallReport {
    pub schema_version: u32,
    pub retrieval_mode: String,
    pub query: String,
    pub max_sensitivity: Sensitivity,
    pub hit_count: usize,
    pub hits: Vec<RecallHit>,
}
pub fn validate_recall_query(query: &RecallQuery) -> Result<()> {
    if query.query.trim().is_empty()
        || query.query.contains('\0')
        || query.query.len() > 4096
        || !(1..=100).contains(&query.limit)
    {
        anyhow::bail!("invalid recall query")
    }
    Ok(())
}
pub fn lexical_v0_score(
    query: &str,
    title: &str,
    topics: &[String],
    body: &str,
) -> Result<Option<LexicalScore>> {
    let phrase = query.trim().to_lowercase();
    if phrase.is_empty() || phrase.contains('\0') || phrase.len() > 4096 {
        anyhow::bail!("invalid recall query")
    }
    let title = title.to_lowercase();
    let topics: Vec<_> = topics.iter().map(|v| v.to_lowercase()).collect();
    let body = body.to_lowercase();
    let mut terms = Vec::new();
    for term in phrase.split_whitespace() {
        if !terms.contains(&term) {
            terms.push(term)
        }
    }
    let mut exact = 0;
    if title.contains(&phrase)
        || topics.iter().any(|v| v.contains(&phrase))
        || body.contains(&phrase)
    {
        exact = 100
    }
    let (mut title_score, mut topic_score, mut body_score) = (0, 0, 0);
    let mut matched = Vec::new();
    for term in terms {
        let mut hit = false;
        if title.contains(term) {
            title_score += 20;
            hit = true
        }
        if topics.iter().any(|v| v.contains(term)) {
            topic_score += 10;
            hit = true
        }
        if body.contains(term) {
            body_score += 2;
            hit = true
        }
        if hit {
            matched.push(term.to_string())
        }
    }
    let score = exact + title_score + topic_score + body_score;
    if score == 0 {
        Ok(None)
    } else {
        Ok(Some(LexicalScore {
            score,
            matched_terms: matched,
            score_breakdown: RecallScoreBreakdown {
                exact_phrase: exact,
                title_terms: title_score,
                topic_terms: topic_score,
                body_terms: body_score,
            },
        }))
    }
}
pub fn recall(library: impl AsRef<Path>, query: &RecallQuery) -> Result<RecallReport> {
    validate_recall_query(query)?;
    let mut hits = Vec::new();
    for (item, revision) in
        memex_store::list_items_at_sensitivity(library.as_ref(), query.max_sensitivity)?
    {
        let sensitivity = item.sensitivity.max(revision.sensitivity);
        if sensitivity > query.max_sensitivity {
            continue;
        }
        if let Some(score) = lexical_v0_score(
            &query.query,
            &revision.title,
            &[],
            &revision.content_markdown,
        )? {
            hits.push(RecallHit {
                target_kind: RecallTargetKind::ItemRevision,
                id: item.item_id,
                revision_id: Some(revision.revision_id),
                title: revision.title,
                snippet: revision.content_markdown.chars().take(320).collect(),
                sensitivity,
                score: score.score,
                matched_terms: score.matched_terms,
                score_breakdown: score.score_breakdown,
            })
        }
    }
    for record in list_records_at_sensitivity(library.as_ref(), query.max_sensitivity)? {
        if let Some(score) = lexical_v0_score(
            &query.query,
            &record.title,
            &record.topics,
            &record.body_markdown,
        )? {
            hits.push(RecallHit {
                target_kind: RecallTargetKind::Analysis,
                id: record.analysis_id,
                revision_id: None,
                title: record.title,
                snippet: record.body_markdown.chars().take(320).collect(),
                sensitivity: record.sensitivity,
                score: score.score,
                matched_terms: score.matched_terms,
                score_breakdown: score.score_breakdown,
            })
        }
    }
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.target_kind.cmp(&b.target_kind))
            .then(a.id.cmp(&b.id))
    });
    hits.truncate(query.limit);
    Ok(RecallReport {
        schema_version: SCHEMA_VERSION,
        retrieval_mode: "lexical_v0".into(),
        query: query.query.clone(),
        max_sensitivity: query.max_sensitivity,
        hit_count: hits.len(),
        hits,
    })
}
fn serde_label<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .expect("label serialization")
        .trim_matches('"')
        .to_owned()
}
pub fn render_recall_markdown(report: &RecallReport) -> String {
    let mut output = format!(
        "# Recall\n\nmode: {}\nquery: {}\nmax_sensitivity: {}\nhit_count: {}\n",
        report.retrieval_mode,
        report.query,
        serde_label(&report.max_sensitivity),
        report.hit_count
    );
    for hit in &report.hits {
        output.push_str(&format!(
            "\n## {}\nkind: {}\nid: {}\n",
            hit.title,
            serde_label(&hit.target_kind),
            hit.id
        ));
        if let Some(revision) = &hit.revision_id {
            output.push_str(&format!("revision_id: {}\n", revision));
        }
        output.push_str(&format!("sensitivity: {}\nscore: {}\nmatched_terms: {}\nbreakdown: exact_phrase={}, title_terms={}, topic_terms={}, body_terms={}\n\n{}\n",serde_label(&hit.sensitivity),hit.score,hit.matched_terms.join(", "),hit.score_breakdown.exact_phrase,hit.score_breakdown.title_terms,hit.score_breakdown.topic_terms,hit.score_breakdown.body_terms,hit.snippet));
    }
    output
}

pub fn read_link_request(path: impl AsRef<Path>) -> Result<LinkRequest> {
    let bytes = read_bounded_regular(path.as_ref(), 1024 * 1024, "link request")?;
    let request: LinkRequest = serde_json::from_slice(&bytes)?;
    validate_link_request(&request)?;
    Ok(request)
}
pub fn prepare_link(library: impl AsRef<Path>, request: &LinkRequest) -> Result<AnalysisLink> {
    validate_link_request(request)?;
    let from = read_record(library.as_ref(), &request.from_analysis_id)?;
    let target_sensitivity = match request.target.kind {
        LinkTargetKind::Analysis => {
            if request.target.revision_id.is_some() {
                anyhow::bail!("analysis target revision forbidden")
            }
            read_record(library.as_ref(), &request.target.id)?.sensitivity
        }
        LinkTargetKind::ItemRevision => {
            let revision_id = request
                .target
                .revision_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("item target revision required"))?;
            if !safe_component(&request.target.id, "item_")
                || !safe_component(revision_id, "revision_")
            {
                anyhow::bail!("invalid item target")
            }
            let (item, revision) = memex_store::read_item(library.as_ref(), &request.target.id)?;
            if revision.revision_id != *revision_id {
                anyhow::bail!("item target not current")
            }
            item.sensitivity.max(revision.sensitivity)
        }
    };
    let mut link = AnalysisLink {
        schema_version: 1,
        link_id: String::new(),
        from_analysis_id: request.from_analysis_id.clone(),
        target: request.target.clone(),
        relation: request.relation.clone(),
        rationale_markdown: request.rationale_markdown.clone(),
        created_at: request.created_at.clone(),
        actor_kind: request.actor_kind.clone(),
        actor_id: request.actor_id.clone(),
        tool_id: request.tool_id.clone(),
        tool_version: request.tool_version.clone(),
        config_digest: request.config_digest.clone(),
        sensitivity: from.sensitivity.max(target_sensitivity),
    };
    link.link_id = link_id(&link)?;
    validate_link(&link)?;
    Ok(link)
}
pub fn validate_link(link: &AnalysisLink) -> Result<()> {
    if link.schema_version != 1 || !valid_link_id(&link.link_id) || link_id(link)? != link.link_id {
        anyhow::bail!("invalid link identity")
    }
    validate_link_request(&request_from_link(link))
}
fn request_from_link(link: &AnalysisLink) -> LinkRequest {
    LinkRequest {
        schema_version: link.schema_version,
        from_analysis_id: link.from_analysis_id.clone(),
        target: link.target.clone(),
        relation: link.relation.clone(),
        rationale_markdown: link.rationale_markdown.clone(),
        created_at: link.created_at.clone(),
        actor_kind: link.actor_kind.clone(),
        actor_id: link.actor_id.clone(),
        tool_id: link.tool_id.clone(),
        tool_version: link.tool_version.clone(),
        config_digest: link.config_digest.clone(),
    }
}
fn verify_link_endpoints(library: &Path, link: &AnalysisLink) -> Result<()> {
    if prepare_link(library, &request_from_link(link))? != *link {
        anyhow::bail!("link endpoints changed")
    }
    Ok(())
}
fn safe_existing_analysis(root: &Path) -> Result<PathBuf> {
    let path = root.join("analysis");
    let meta = fs::symlink_metadata(&path)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        anyhow::bail!("unsafe analysis directory")
    }
    Ok(path)
}
pub fn apply_link(library: impl AsRef<Path>, link: &AnalysisLink) -> Result<LinkReceipt> {
    validate_link(link)?;
    let root = safe_library_root(library.as_ref())?;
    verify_link_endpoints(&root, link)?;
    let analysis = safe_existing_analysis(&root)?;
    let _lock = lock_file(&analysis)?;
    verify_link_endpoints(&root, link)?;
    let links = safe_create_dir(&analysis, "links")?;
    let path = links.join(format!("{}.json", link.link_id));
    let bytes = serde_json::to_vec_pretty(link)?;
    let status = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut tmp = tempfile::NamedTempFile::new_in(&links)?;
            tmp.write_all(&bytes)?;
            tmp.flush()?;
            tmp.as_file().sync_all()?;
            tmp.persist_noclobber(&path).map_err(|e| e.error)?;
            File::open(&links)?.sync_all()?;
            ApplyStatus::Created
        }
        Ok(meta)
            if !meta.file_type().is_symlink() && meta.is_file() && meta.len() <= 1024 * 1024 =>
        {
            let decoded = read_link_file(&path)?;
            if decoded != *link {
                anyhow::bail!("link immutable bytes differ")
            }
            ApplyStatus::Replayed
        }
        Ok(_) => anyhow::bail!("unsafe link"),
        Err(e) => return Err(e.into()),
    };
    Ok(LinkReceipt {
        link_id: link.link_id.clone(),
        status,
        sensitivity: link.sensitivity,
        from_analysis_id: link.from_analysis_id.clone(),
        target: link.target.clone(),
        warning_codes: vec![],
    })
}
pub fn read_link(library: impl AsRef<Path>, link_id: &str) -> Result<AnalysisLink> {
    if !valid_link_id(link_id) {
        anyhow::bail!("invalid link id")
    }
    let root = safe_library_root(library.as_ref())?;
    let links = safe_existing_links(&root)?.ok_or_else(|| anyhow::anyhow!("link not found"))?;
    let link = read_link_file(&links.join(format!("{link_id}.json")))?;
    if link.link_id != link_id {
        anyhow::bail!("link path identity mismatch");
    }
    Ok(link)
}
pub fn list_links(library: impl AsRef<Path>) -> Result<Vec<AnalysisLink>> {
    let root = safe_library_root(library.as_ref())?;
    let Some(links) = safe_existing_links(&root)? else {
        return Ok(Vec::new());
    };
    let mut ids = Vec::new();
    for entry in fs::read_dir(&links)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("unsafe link entry"))?;
        if in_progress_write(&name) {
            continue;
        }
        let meta = entry.file_type()?;
        let Some(id) = name.strip_suffix(".json") else {
            anyhow::bail!("unsafe link entry")
        };
        if meta.is_symlink() || !meta.is_file() || !valid_link_id(id) {
            anyhow::bail!("unsafe link entry")
        }
        ids.push(id.to_owned())
    }
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let link = read_link_file(&links.join(format!("{id}.json")))?;
            if link.link_id != id {
                anyhow::bail!("link path identity mismatch");
            }
            Ok(link)
        })
        .collect()
}
fn safe_existing_links(root: &Path) -> Result<Option<PathBuf>> {
    let analysis = root.join("analysis");
    match fs::symlink_metadata(&analysis) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            anyhow::bail!("unsafe analysis directory")
        }
        Ok(_) => {}
        Err(e) => return Err(e.into()),
    }
    let links = analysis.join("links");
    match fs::symlink_metadata(&links) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(m) if !m.file_type().is_symlink() && m.is_dir() => Ok(Some(links)),
        Ok(_) => anyhow::bail!("unsafe links directory"),
        Err(e) => Err(e.into()),
    }
}
fn read_link_file(path: &Path) -> Result<AnalysisLink> {
    let bytes = read_bounded_regular(path, 1024 * 1024, "analysis link")?;
    let link: AnalysisLink = serde_json::from_slice(&bytes)?;
    validate_link(&link)?;
    if serde_json::to_vec_pretty(&link)? != bytes {
        anyhow::bail!("link bytes noncanonical")
    }
    Ok(link)
}

pub fn render_record_markdown(record: &AnalysisRecord) -> String {
    let inputs = record
        .inputs
        .iter()
        .map(|i| {
            format!(
                "- item_id: {}\n  revision_id: {}\n  revision_sha256: {}\n  evidence_ids: {}",
                i.item_id,
                i.revision_id,
                i.revision_sha256,
                i.evidence_ids.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    fn value<T: Serialize>(value: &T) -> String {
        serde_json::to_string(value)
            .expect("enum serialization")
            .trim_matches('"')
            .to_owned()
    }
    let generated = record.generated_text.as_ref().map_or_else(String::new, |g| {
        let output_limit = g.output_limit.as_ref().map_or_else(
            || "none".to_owned(),
            |limit| format!("{} {}", limit.maximum, value(&limit.unit)),
        );
        format!("generated_text: purpose={}, source_language={}, target_language={}, output_limit={}, summary_job_id={}, translation_job_id={}, model_id={}, model_sha256={}, prompt_id={}, prompt_version={}, prompt_sha256={}\n", value(&g.purpose), g.source_language, g.target_language, output_limit, g.summary_job_id.as_deref().unwrap_or("none"), g.translation_job_id.as_deref().unwrap_or("none"), g.model_id, g.model_sha256.as_deref().unwrap_or("none"), g.prompt_id, g.prompt_version, g.prompt_sha256)
    });
    format!("# {}\n\nanalysis_id: {}\nanalysis_kind: {}\nsensitivity: {}\ncreated_at: {}\ntopics: {}\nprovenance: actor_kind={}, actor_id={}, tool_id={}, tool_version={}, config_digest={}\n{}inputs:\n{}\n\n## Analysis\n\n{}\n",record.title,record.analysis_id,value(&record.analysis_kind),value(&record.sensitivity),record.created_at,record.topics.join(", "),value(&record.provenance.actor_kind),record.provenance.actor_id,record.provenance.tool_id,record.provenance.tool_version,record.provenance.config_digest,generated,inputs,record.body_markdown)
}

pub fn render_record_index_json(records: &[AnalysisRecord]) -> String {
    serde_json::to_string_pretty(&records.iter().map(index_entry).collect::<Vec<_>>())
        .expect("record index serialization")
}

pub fn render_record_index_markdown(records: &[AnalysisRecord]) -> String {
    records
        .iter()
        .map(|record| {
            format!(
                "- {} | {} | {}\n",
                record.analysis_id, record.title, record.created_at
            )
        })
        .collect()
}

pub fn read_request(path: impl AsRef<Path>) -> Result<AnalysisRequest> {
    let bytes = read_bounded_regular(path.as_ref(), 1024 * 1024, "analysis request")?;
    let request: AnalysisRequest = serde_json::from_slice(&bytes)?;
    validate_request(&request)?;
    Ok(request)
}
fn read_bounded_regular(path: &Path, max: usize, label: &str) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() as usize > max {
        anyhow::bail!("unsafe {label} file")
    }
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?
    };
    #[cfg(not(unix))]
    let mut file = OpenOptions::new().read(true).open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() as usize > max {
        anyhow::bail!("unsafe {label} file")
    }
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        anyhow::bail!("{label} is too large")
    }
    Ok(bytes)
}

pub fn apply_record(library: impl AsRef<Path>, record: &AnalysisRecord) -> Result<AnalysisReceipt> {
    validate_record(record)?;
    let root = safe_library_root(library.as_ref())?;
    verify_record_inputs(&root, record)?;
    let analysis = safe_create_dir(&root, "analysis")?;
    let records = safe_create_dir(&analysis, "records")?;
    let _lock = lock_file(&analysis)?;
    verify_record_inputs(&root, record)?;
    let status = install_record_file(&records, record)?;
    let mut receipt = receipt(record, status);
    if write_projection(
        &records.join(format!("{}.md", record.analysis_id)),
        render_record_markdown(record).as_bytes(),
    )
    .is_err()
    {
        receipt
            .warning_codes
            .push("record_markdown_projection_pending".into());
    }
    let projection_records = list_records(&root);
    if projection_records.is_err()
        || write_projection(
            &analysis.join("index.json"),
            render_record_index_json(projection_records.as_ref().expect("checked")).as_bytes(),
        )
        .is_err()
    {
        receipt
            .warning_codes
            .push("analysis_json_index_projection_pending".into());
    }
    if projection_records.is_err()
        || write_projection(
            &analysis.join("index.md"),
            render_record_index_markdown(projection_records.as_ref().expect("checked")).as_bytes(),
        )
        .is_err()
    {
        receipt
            .warning_codes
            .push("analysis_markdown_index_projection_pending".into());
    }
    Ok(receipt)
}

fn apply_record_pair(
    library: impl AsRef<Path>,
    first: &AnalysisRecord,
    second: &AnalysisRecord,
) -> Result<(AnalysisReceipt, AnalysisReceipt)> {
    if first.analysis_id == second.analysis_id {
        anyhow::bail!("analysis pair identities must differ");
    }
    validate_record(first)?;
    validate_record(second)?;
    let root = safe_library_root(library.as_ref())?;
    verify_record_inputs(&root, first)?;
    verify_record_inputs(&root, second)?;
    let analysis = safe_create_dir(&root, "analysis")?;
    let records = safe_create_dir(&analysis, "records")?;
    let _lock = lock_file(&analysis)?;
    verify_record_inputs(&root, first)?;
    verify_record_inputs(&root, second)?;

    let first_status = install_record_file(&records, first)?;
    let second_status = install_record_file(&records, second)?;
    let mut first_receipt = receipt(first, first_status);
    let mut second_receipt = receipt(second, second_status);

    for (record, receipt) in [(first, &mut first_receipt), (second, &mut second_receipt)] {
        if write_projection(
            &records.join(format!("{}.md", record.analysis_id)),
            render_record_markdown(record).as_bytes(),
        )
        .is_err()
        {
            receipt
                .warning_codes
                .push("record_markdown_projection_pending".into());
        }
    }

    let projection_records = list_records(&root);
    if projection_records.is_err()
        || write_projection(
            &analysis.join("index.json"),
            render_record_index_json(projection_records.as_ref().expect("checked")).as_bytes(),
        )
        .is_err()
    {
        first_receipt
            .warning_codes
            .push("analysis_json_index_projection_pending".into());
        second_receipt
            .warning_codes
            .push("analysis_json_index_projection_pending".into());
    }
    if projection_records.is_err()
        || write_projection(
            &analysis.join("index.md"),
            render_record_index_markdown(projection_records.as_ref().expect("checked")).as_bytes(),
        )
        .is_err()
    {
        first_receipt
            .warning_codes
            .push("analysis_markdown_index_projection_pending".into());
        second_receipt
            .warning_codes
            .push("analysis_markdown_index_projection_pending".into());
    }
    Ok((first_receipt, second_receipt))
}

fn install_record_file(records: &Path, record: &AnalysisRecord) -> Result<ApplyStatus> {
    let path = records.join(format!("{}.json", record.analysis_id));
    let bytes = serde_json::to_vec_pretty(record)?;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut temporary = tempfile::NamedTempFile::new_in(records)?;
            let file = temporary.as_file_mut();
            file.write_all(&bytes)?;
            file.flush()?;
            file.sync_all()?;
            temporary
                .persist_noclobber(&path)
                .map_err(|error| error.error)?;
            File::open(records)?.sync_all()?;
            Ok(ApplyStatus::Created)
        }
        Ok(metadata)
            if !metadata.file_type().is_symlink()
                && metadata.is_file()
                && metadata.len() <= 1024 * 1024 =>
        {
            let decoded = read_record_file(&path)?;
            if decoded != *record {
                anyhow::bail!("analysis record immutable bytes differ");
            }
            Ok(ApplyStatus::Replayed)
        }
        Ok(_) => anyhow::bail!("unsafe analysis record"),
        Err(error) => Err(error.into()),
    }
}

fn verify_record_inputs(library: &Path, record: &AnalysisRecord) -> Result<()> {
    for input in &record.inputs {
        let (item, current) = memex_store::read_item(library, &input.item_id)?;
        // A revision whose body is the current body (only metadata changed since) is still a
        // verified input; its full digest is checked below like the current one's.
        let revision = if input.revision_id == current.revision_id {
            current
        } else {
            memex_store::same_body_revision(
                library,
                &input.item_id,
                &current.revision_id,
                &input.revision_id,
            )?
            .ok_or_else(|| anyhow::anyhow!("analysis record inputs are no longer verified"))?
        };
        if !item_revision_ref_matches(input, &input.item_id, &revision)?
            || record.sensitivity < item.sensitivity
            || record.sensitivity < revision.sensitivity
        {
            anyhow::bail!("analysis record inputs are no longer verified");
        }
    }
    Ok(())
}

pub fn read_record(library: impl AsRef<Path>, analysis_id: &str) -> Result<AnalysisRecord> {
    if !valid_analysis_id(analysis_id) {
        anyhow::bail!("invalid analysis id");
    }
    let root = safe_library_root(library.as_ref())?;
    let records = safe_existing_records(&root)?
        .ok_or_else(|| anyhow::anyhow!("analysis record not found"))?;
    let record = read_record_file(&records.join(format!("{analysis_id}.json")))?;
    if record.analysis_id != analysis_id {
        anyhow::bail!("analysis record path identity mismatch");
    }
    Ok(record)
}

fn read_owned_index(root: &Path) -> Result<Vec<AnalysisIndexEntry>> {
    let analysis = root.join("analysis");
    match fs::symlink_metadata(&analysis) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(m) if !m.file_type().is_symlink() && m.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe analysis directory"),
        Err(e) => return Err(e.into()),
    }
    let records = analysis.join("records");
    let mut record_ids = Vec::new();
    match fs::symlink_metadata(&records) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return match fs::symlink_metadata(analysis.join("index.json")) {
                Err(index_error) if index_error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(Vec::new())
                }
                Ok(_) => anyhow::bail!("analysis index exists without records"),
                Err(index_error) => Err(index_error.into()),
            };
        }
        Ok(m) if !m.file_type().is_symlink() && m.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe analysis records"),
        Err(e) => return Err(e.into()),
    }
    for entry in fs::read_dir(&records)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("unsafe analysis record name"))?;
        if in_progress_write(&name) {
            continue;
        }
        if let Some(id) = name.strip_suffix(".json") {
            if entry.file_type()?.is_symlink()
                || !entry.file_type()?.is_file()
                || !valid_analysis_id(id)
            {
                anyhow::bail!("unsafe analysis record entry");
            }
            record_ids.push(id.to_owned());
        } else {
            let Some(id) = name.strip_suffix(".md") else {
                anyhow::bail!("unsafe analysis record entry");
            };
            let metadata = entry.file_type()?;
            if metadata.is_symlink() || !metadata.is_file() || !valid_analysis_id(id) {
                anyhow::bail!("unsafe analysis record entry");
            }
        }
    }
    record_ids.sort();
    let index = analysis.join("index.json");
    if record_ids.is_empty() {
        match fs::symlink_metadata(&index) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Ok(m) if !m.file_type().is_symlink() && m.is_file() => {}
            Ok(_) => anyhow::bail!("unsafe analysis index"),
            Err(e) => return Err(e.into()),
        }
    }
    let bytes = read_bounded_regular(&index, 8 * 1024 * 1024, "analysis index")?;
    let entries: Vec<AnalysisIndexEntry> = serde_json::from_slice(&bytes)?;
    let mut index_ids = entries
        .iter()
        .map(|e| e.analysis_id.clone())
        .collect::<Vec<_>>();
    if entries.iter().any(|e| !valid_analysis_id(&e.analysis_id)) {
        anyhow::bail!("invalid analysis index");
    }
    index_ids.sort();
    if index_ids.windows(2).any(|v| v[0] == v[1]) || index_ids != record_ids {
        anyhow::bail!("analysis index/records mismatch");
    }
    Ok(entries)
}

pub fn read_record_at_sensitivity(
    library: impl AsRef<Path>,
    analysis_id: &str,
    max: Sensitivity,
) -> Result<Option<AnalysisRecord>> {
    if !valid_analysis_id(analysis_id) {
        return Ok(None);
    }
    let root = safe_library_root(library.as_ref())?;
    let Some(entry) = read_owned_index(&root)?
        .into_iter()
        .find(|e| e.analysis_id == analysis_id)
    else {
        return Ok(None);
    };
    if entry.sensitivity > max {
        return Ok(None);
    }
    let record = read_record(&root, analysis_id)?;
    if index_entry(&record) != entry {
        anyhow::bail!("analysis index entry mismatch");
    }
    Ok(Some(record))
}

pub fn list_records_at_sensitivity(
    library: impl AsRef<Path>,
    max: Sensitivity,
) -> Result<Vec<AnalysisRecord>> {
    let root = safe_library_root(library.as_ref())?;
    let mut out = Vec::new();
    for entry in read_owned_index(&root)? {
        if entry.sensitivity <= max {
            let record = read_record(&root, &entry.analysis_id)?;
            if index_entry(&record) != entry {
                anyhow::bail!("analysis index entry mismatch");
            }
            out.push(record);
        }
    }
    Ok(out)
}

/// A writer's not-yet-persisted temporary file (`tempfile`'s `.tmp` prefix, created under the
/// analysis lock and renamed into place): never a record. Readers without the lock (provider
/// workers run concurrently) skip it instead of failing on a write in progress.
fn in_progress_write(name: &str) -> bool {
    name.starts_with(".tmp")
}

pub fn list_records(library: impl AsRef<Path>) -> Result<Vec<AnalysisRecord>> {
    let root = safe_library_root(library.as_ref())?;
    let Some(records) = safe_existing_records(&root)? else {
        return Ok(Vec::new());
    };
    let mut ids = Vec::new();
    for entry in fs::read_dir(&records)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("unsafe analysis record name"))?;
        if in_progress_write(&name) {
            continue;
        }
        let metadata = entry.file_type()?;
        if let Some(analysis_id) = name.strip_suffix(".md") {
            if !valid_analysis_id(analysis_id) {
                anyhow::bail!("unsafe analysis record entry");
            }
            continue;
        }
        let Some(analysis_id) = name.strip_suffix(".json") else {
            anyhow::bail!("unsafe analysis record entry");
        };
        if metadata.is_symlink() || !metadata.is_file() || !valid_analysis_id(analysis_id) {
            anyhow::bail!("unsafe analysis record entry");
        }
        ids.push(analysis_id.to_owned());
    }
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let record = read_record_file(&records.join(format!("{id}.json")))?;
            if record.analysis_id != id {
                anyhow::bail!("analysis record path identity mismatch");
            }
            Ok(record)
        })
        .collect()
}

pub fn prepare_record(
    library: impl AsRef<Path>,
    request: &AnalysisRequest,
) -> Result<AnalysisRecord> {
    validate_request(request)?;
    let mut inputs = Vec::with_capacity(request.item_ids.len());
    let mut sensitivity = request.requested_sensitivity.unwrap_or(Sensitivity::Public);
    for item_id in &request.item_ids {
        let (item, revision) = memex_store::read_item(library.as_ref(), item_id)?;
        if item.current_revision != revision.revision_id {
            anyhow::bail!("item revision is not current");
        }
        sensitivity = sensitivity.max(item.sensitivity).max(revision.sensitivity);
        inputs.push(revision_ref(item_id, &revision)?);
    }
    let mut record = AnalysisRecord {
        schema_version: SCHEMA_VERSION,
        analysis_id: String::new(),
        analysis_kind: request.analysis_kind.clone(),
        title: request.title.clone(),
        body_markdown: request.body_markdown.clone(),
        inputs,
        topics: request.topics.clone(),
        sensitivity,
        created_at: request.created_at.clone(),
        provenance: AnalysisProvenance {
            actor_kind: request.actor_kind.clone(),
            actor_id: request.actor_id.clone(),
            tool_id: request.tool_id.clone(),
            tool_version: request.tool_version.clone(),
            config_digest: request.config_digest.clone(),
        },
        generated_text: request.generated_text.clone(),
    };
    record.analysis_id = analysis_id(&record)?;
    validate_record(&record)?;
    Ok(record)
}

fn revision_ref(item_id: &str, revision: &ItemRevision) -> Result<ItemRevisionRef> {
    Ok(ItemRevisionRef {
        item_id: item_id.to_owned(),
        revision_id: revision.revision_id.clone(),
        revision_sha256: format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(revision)?)),
        evidence_ids: revision.evidence_ids.clone(),
    })
}

fn analysis_id(record: &AnalysisRecord) -> Result<String> {
    let mut payload = record.clone();
    payload.analysis_id.clear();
    Ok(format!(
        "analysis_{:x}",
        Sha256::digest(serde_json::to_vec(&payload)?)
    ))
}
fn link_id(link: &AnalysisLink) -> Result<String> {
    let mut payload = link.clone();
    payload.link_id.clear();
    Ok(format!(
        "analysis_link_{:x}",
        Sha256::digest(serde_json::to_vec(&payload)?)
    ))
}
fn valid_link_id(value: &str) -> bool {
    value.len() == 78
        && value.starts_with("analysis_link_")
        && value[14..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_lowercase() && b.is_ascii_hexdigit()))
}
fn validate_link_request(request: &LinkRequest) -> Result<()> {
    if request.schema_version != SCHEMA_VERSION
        || !valid_analysis_id(&request.from_analysis_id)
        || request.rationale_markdown.is_empty()
        || request.rationale_markdown.len() > 64 * 1024
        || request.rationale_markdown.contains('\0')
    {
        anyhow::bail!("invalid link request")
    }
    match (&request.target.kind, &request.target.revision_id) {
        (LinkTargetKind::Analysis, None) if valid_analysis_id(&request.target.id) => {}
        (LinkTargetKind::ItemRevision, Some(revision_id))
            if safe_component(&request.target.id, "item_")
                && safe_component(revision_id, "revision_") => {}
        _ => anyhow::bail!("invalid link target"),
    }
    DateTime::parse_from_rfc3339(&request.created_at)
        .map_err(|_| anyhow::anyhow!("invalid created_at"))?;
    for (name, value) in [
        ("actor_id", &request.actor_id),
        ("tool_id", &request.tool_id),
        ("tool_version", &request.tool_version),
    ] {
        validate_text(value, MAX_ACTOR_FIELD_BYTES, true, name)?;
    }
    validate_config_digest(&request.config_digest)
}

fn receipt(record: &AnalysisRecord, status: ApplyStatus) -> AnalysisReceipt {
    AnalysisReceipt {
        analysis_id: record.analysis_id.clone(),
        status,
        sensitivity: record.sensitivity,
        input_item_ids: record
            .inputs
            .iter()
            .map(|input| input.item_id.clone())
            .collect(),
        warning_codes: Vec::new(),
    }
}

fn index_entry(record: &AnalysisRecord) -> AnalysisIndexEntry {
    AnalysisIndexEntry {
        analysis_id: record.analysis_id.clone(),
        analysis_kind: record.analysis_kind.clone(),
        title: record.title.clone(),
        sensitivity: record.sensitivity,
        created_at: record.created_at.clone(),
        input_item_ids: record
            .inputs
            .iter()
            .map(|input| input.item_id.clone())
            .collect(),
        topics: record.topics.clone(),
        generated_text: record.generated_text.clone(),
    }
}

fn write_projection(path: &Path, bytes: &[u8]) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            anyhow::bail!("unsafe projection")
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("projection has no parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

pub(crate) fn safe_library_root(root: &Path) -> Result<PathBuf> {
    if root
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        anyhow::bail!("unsafe library root");
    }
    let absolute = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    let mut current = PathBuf::new();
    for component in absolute.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::RootDir | Component::CurDir) {
            continue;
        }
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() && !allowed_macos_system_alias(&current, &metadata) {
            anyhow::bail!("unsafe library ancestor");
        }
    }
    let metadata = fs::symlink_metadata(&absolute)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("unsafe library root");
    }
    Ok(absolute.canonicalize()?)
}

#[cfg(target_os = "macos")]
fn allowed_macos_system_alias(path: &Path, metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    let expected = match path.to_str() {
        Some("/var") => "private/var",
        Some("/tmp") => "private/tmp",
        Some("/etc") => "private/etc",
        _ => return false,
    };
    metadata.uid() == 0 && fs::read_link(path).ok().as_deref() == Some(Path::new(expected))
}

#[cfg(not(target_os = "macos"))]
fn allowed_macos_system_alias(_: &Path, _: &fs::Metadata) -> bool {
    false
}

pub(crate) fn safe_create_dir(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = parent.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe analysis directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        },
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("unsafe analysis directory");
    }
    Ok(path)
}

fn safe_existing_records(root: &Path) -> Result<Option<PathBuf>> {
    let analysis = root.join("analysis");
    match fs::symlink_metadata(&analysis) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("unsafe analysis directory")
        }
        Ok(_) => {}
        Err(error) => return Err(error.into()),
    }
    let records = analysis.join("records");
    match fs::symlink_metadata(&records) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {
            Ok(Some(records))
        }
        Ok(_) => anyhow::bail!("unsafe analysis records directory"),
        Err(error) => Err(error.into()),
    }
}

fn lock_file(analysis: &Path) -> Result<File> {
    let path = analysis.join(".lock");
    let file = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match create_regular_new(&path) {
                Ok(file) => file,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) =>
                {
                    open_regular(&path)?
                }
                Err(error) => return Err(error),
            }
        }
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
            open_regular(&path)?
        }
        Ok(_) => anyhow::bail!("unsafe analysis lock"),
        Err(error) => return Err(error.into()),
    };
    file.lock_exclusive()?;
    Ok(file)
}

#[cfg(unix)]
fn open_regular(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?)
}

#[cfg(not(unix))]
fn open_regular(path: &Path) -> Result<File> {
    Ok(OpenOptions::new().read(true).write(true).open(path)?)
}

#[cfg(unix)]
fn create_regular_new(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?)
}

#[cfg(not(unix))]
fn create_regular_new(path: &Path) -> Result<File> {
    Ok(OpenOptions::new().write(true).create_new(true).open(path)?)
}

fn read_record_file(path: &Path) -> Result<AnalysisRecord> {
    let bytes = read_bounded_regular(path, 1024 * 1024, "analysis record")?;
    let record: AnalysisRecord = serde_json::from_slice(&bytes)?;
    validate_record(&record)?;
    if serde_json::to_vec_pretty(&record)? != bytes {
        anyhow::bail!("analysis record bytes are not canonical");
    }
    Ok(record)
}

fn validate_request(request: &AnalysisRequest) -> Result<()> {
    if request.schema_version != SCHEMA_VERSION {
        anyhow::bail!("unsupported analysis request schema version");
    }
    if serde_json::to_vec(request)?.len() > 1024 * 1024 {
        anyhow::bail!("analysis request is too large");
    }
    validate_text(&request.title, 512, true, "title")?;
    if request.body_markdown.len() > 256 * 1024 || request.body_markdown.contains('\0') {
        anyhow::bail!("invalid analysis body");
    }
    if !(1..=64).contains(&request.item_ids.len()) || !unique(&request.item_ids) {
        anyhow::bail!("invalid analysis item ids");
    }
    if request.topics.len() > 64 || !unique(&request.topics) {
        anyhow::bail!("invalid analysis topics");
    }
    for topic in &request.topics {
        validate_text(topic, 256, true, "topic")?;
    }
    DateTime::parse_from_rfc3339(&request.created_at)
        .map_err(|_| anyhow::anyhow!("invalid created_at"))?;
    for (name, value) in [
        ("actor_id", &request.actor_id),
        ("tool_id", &request.tool_id),
        ("tool_version", &request.tool_version),
    ] {
        validate_text(value, MAX_ACTOR_FIELD_BYTES, true, name)?;
    }
    validate_config_digest(&request.config_digest)?;
    if let Some(generated) = &request.generated_text {
        validate_generated_text(generated, &request.analysis_kind, &request.body_markdown)?;
    }
    Ok(())
}

pub fn validate_record(record: &AnalysisRecord) -> Result<()> {
    if record.schema_version != SCHEMA_VERSION || analysis_id(record)? != record.analysis_id {
        anyhow::bail!("invalid analysis record identity");
    }
    validate_request(&AnalysisRequest {
        schema_version: record.schema_version,
        analysis_kind: record.analysis_kind.clone(),
        title: record.title.clone(),
        body_markdown: record.body_markdown.clone(),
        item_ids: record
            .inputs
            .iter()
            .map(|input| input.item_id.clone())
            .collect(),
        topics: record.topics.clone(),
        requested_sensitivity: Some(record.sensitivity),
        created_at: record.created_at.clone(),
        actor_kind: record.provenance.actor_kind.clone(),
        actor_id: record.provenance.actor_id.clone(),
        tool_id: record.provenance.tool_id.clone(),
        tool_version: record.provenance.tool_version.clone(),
        config_digest: record.provenance.config_digest.clone(),
        generated_text: record.generated_text.clone(),
    })?;
    if !(1..=64).contains(&record.inputs.len()) {
        anyhow::bail!("invalid analysis record inputs");
    }
    let item_ids: Vec<_> = record
        .inputs
        .iter()
        .map(|input| input.item_id.clone())
        .collect();
    if !unique(&item_ids) {
        anyhow::bail!("duplicate analysis record input");
    }
    for input in &record.inputs {
        if !safe_component(&input.item_id, "item_")
            || !safe_component(&input.revision_id, "revision_")
            || !valid_sha256(&input.revision_sha256)
            || input.evidence_ids.is_empty()
            || input.evidence_ids.len() > 4096
            || !unique(&input.evidence_ids)
            || input
                .evidence_ids
                .iter()
                .any(|evidence_id| !safe_component(evidence_id, "evidence_"))
        {
            anyhow::bail!("invalid analysis input reference");
        }
    }
    Ok(())
}

fn validate_text(value: &str, max_bytes: usize, reject_controls: bool, field: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > max_bytes
        || (reject_controls && value.chars().any(char::is_control))
    {
        anyhow::bail!("invalid {field}");
    }
    Ok(())
}

fn validate_generated_text(
    generated: &GeneratedTextDescriptor,
    kind: &AnalysisKind,
    body_markdown: &str,
) -> Result<()> {
    let expected_kind = match generated.purpose {
        GeneratedTextPurpose::Summary => AnalysisKind::Summary,
        GeneratedTextPurpose::TitleTranslation | GeneratedTextPurpose::DocumentTranslation => {
            AnalysisKind::Translation
        }
    };
    if kind != &expected_kind
        || body_markdown.is_empty()
        || !valid_language_tag(&generated.source_language)
        || !valid_language_tag(&generated.target_language)
        || generated
            .summary_job_id
            .as_deref()
            .is_some_and(|job_id| !valid_summary_job_id(job_id))
        || generated
            .translation_job_id
            .as_deref()
            .is_some_and(|job_id| !valid_translation_job_id(job_id))
        || !valid_generated_token(&generated.model_id)
        || generated
            .model_sha256
            .as_deref()
            .is_some_and(|digest| !valid_sha256(digest))
        || (generated.summary_job_id.is_some() || generated.translation_job_id.is_some())
            && generated.model_sha256.is_none()
        || !valid_generated_token(&generated.prompt_id)
        || !valid_generated_token(&generated.prompt_version)
        || !valid_sha256(&generated.prompt_sha256)
    {
        anyhow::bail!("invalid generated text descriptor");
    }
    match generated.purpose {
        GeneratedTextPurpose::Summary if generated.translation_job_id.is_some() => {
            anyhow::bail!("invalid generated summary job binding")
        }
        GeneratedTextPurpose::TitleTranslation | GeneratedTextPurpose::DocumentTranslation
            if generated.summary_job_id.is_some() =>
        {
            anyhow::bail!("invalid generated translation job binding")
        }
        _ => {}
    }
    if matches!(
        generated.purpose,
        GeneratedTextPurpose::TitleTranslation | GeneratedTextPurpose::DocumentTranslation
    ) && generated
        .source_language
        .eq_ignore_ascii_case(&generated.target_language)
    {
        anyhow::bail!("invalid generated translation descriptor");
    }
    match (&generated.purpose, &generated.output_limit) {
        (GeneratedTextPurpose::Summary, Some(limit)) if (1..=4096).contains(&limit.maximum) => {
            if generated_text_length(body_markdown, limit.unit) > limit.maximum as usize {
                anyhow::bail!("generated summary exceeds output limit");
            }
        }
        (GeneratedTextPurpose::Summary, _) => {
            anyhow::bail!("generated summary requires an output limit")
        }
        (
            GeneratedTextPurpose::TitleTranslation,
            Some(TextOutputLimit {
                unit: TextLengthUnit::Characters,
                maximum,
            }),
        ) if (1..=512).contains(maximum)
            && generated_text_length(body_markdown, TextLengthUnit::Characters)
                <= *maximum as usize
            && !body_markdown.contains('\n')
            && !body_markdown.starts_with('#') => {}
        (GeneratedTextPurpose::TitleTranslation, _) => {
            anyhow::bail!("generated title translation requires a character limit")
        }
        (GeneratedTextPurpose::DocumentTranslation, None) => {}
        _ => anyhow::bail!("invalid generated translation descriptor"),
    }
    Ok(())
}

fn generated_text_length(value: &str, unit: TextLengthUnit) -> usize {
    match unit {
        TextLengthUnit::Characters => value.chars().count(),
        TextLengthUnit::Words => value.split_whitespace().count(),
    }
}

fn valid_language_tag(value: &str) -> bool {
    if !(2..=35).contains(&value.len()) {
        return false;
    }
    let mut subtags = value.split('-');
    let Some(primary) = subtags.next() else {
        return false;
    };
    (2..=8).contains(&primary.len())
        && primary.bytes().all(|byte| byte.is_ascii_alphabetic())
        && subtags.all(|subtag| {
            (1..=8).contains(&subtag.len())
                && subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

fn valid_generated_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
        })
}

fn valid_summary_job_id(value: &str) -> bool {
    value.len() == "summary_".len() + 64
        && value.starts_with("summary_")
        && value["summary_".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn unique(values: &[String]) -> bool {
    values.iter().collect::<HashSet<_>>().len() == values.len()
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_analysis_id(value: &str) -> bool {
    value.len() == 73
        && value.starts_with("analysis_")
        && value[9..].bytes().all(|byte| {
            byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())
        })
}

fn valid_translation_job_id(value: &str) -> bool {
    value.len() == 76
        && value.starts_with("translation_")
        && value[12..].bytes().all(|byte| {
            byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())
        })
}

fn safe_component(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() > prefix.len()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn validate_config_digest(value: &str) -> Result<()> {
    if !valid_sha256(value) {
        anyhow::bail!("invalid config digest");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_core::{
        BlobDescriptor, CaptureEnvelope, ItemDraft, SourceProfile, VerifiedBlob, VerifiedCapture,
    };
    use tempfile::tempdir;

    fn request(item_id: String) -> AnalysisRequest {
        AnalysisRequest {
            schema_version: 1,
            analysis_kind: AnalysisKind::Summary,
            title: "Summary".into(),
            body_markdown: "body".into(),
            item_ids: vec![item_id],
            topics: vec!["topic".into()],
            requested_sensitivity: None,
            created_at: "2026-07-31T00:00:00Z".into(),
            actor_kind: ActorKind::Agent,
            actor_id: "agent".into(),
            tool_id: "tool".into(),
            tool_version: "1".into(),
            config_digest: format!("sha256:{:x}", Sha256::digest(b"config")),
            generated_text: None,
        }
    }

    fn ingest_one(root: &Path, sensitivity: Sensitivity) -> String {
        let capture = VerifiedCapture {
            source: SourceProfile {
                schema_version: 1,
                source_id: "source".into(),
                name: "source".into(),
                source_kind: "web".into(),
                canonical_uris: vec![],
                evidence_roles: vec![],
                default_sensitivity: sensitivity,
                preferred_extractor: None,
            },
            capture: CaptureEnvelope {
                schema_version: 1,
                adapter_id: "adapter".into(),
                adapter_version: "1".into(),
                external_capture_id: "capture".into(),
                source_id: "source".into(),
                captured_at: "2026-07-31T00:00:00Z".into(),
                capture_method: "test".into(),
                sensitivity,
                route_summary: None,
                title: None,
                source_url: "https://example.test".into(),
                canonical_url: None,
                artifacts: vec![],
            },
            blobs: vec![VerifiedBlob {
                descriptor: BlobDescriptor {
                    schema_version: 1,
                    locator: "body".into(),
                    media_type: "text/plain".into(),
                    byte_size: 4,
                    sha256: "sha256:body".into(),
                    source_url: None,
                },
                blob_id: "blob".into(),
                source_backend_ref: Some("test".into()),
                bytes: b"body".to_vec(),
            }],
        };
        let draft = ItemDraft {
            item_type: "webpage".into(),
            title: "Title".into(),
            source_url: "https://example.test".into(),
            canonical_url: None,
            content_markdown: "body".into(),
            sensitivity,
            translator_id: "translator".into(),
            translator_version: "1".into(),
            metadata: Default::default(),
            attachments: Vec::new(),
        };
        memex_store::ingest(root, &capture, draft)
            .unwrap()
            .item_ids
            .remove(0)
    }

    #[test]
    fn old_highlight_jobs_need_a_new_request_before_using_the_new_prompt() {
        struct NoProvider;
        impl InsightModel for NoProvider {
            fn descriptor(&self) -> TranslationModelDescriptor {
                panic!("old prompt must not reach the provider")
            }
            fn generate(
                &mut self,
                _: InsightModelInput<'_>,
            ) -> std::result::Result<InsightOutput, TranslationModelError> {
                panic!("old prompt must not reach the provider")
            }
        }
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Private);
        let (_, revision) = memex_store::read_item(root.path(), &item).unwrap();
        let mut queue = InsightQueue::open(root.path()).unwrap();
        queue.request(&revision, InsightKind::Highlights).unwrap();
        let database =
            rusqlite::Connection::open(root.path().join("analysis/translation-jobs.sqlite3"))
                .unwrap();
        database
            .execute("UPDATE insight_jobs SET prompt_version='1'", [])
            .unwrap();
        drop(queue);
        let mut queue = InsightQueue::open(root.path()).unwrap();
        assert!(queue.queued_kinds().unwrap().is_empty());
        assert!(!queue
            .process_next(InsightKind::Highlights, &mut NoProvider)
            .unwrap());
        assert_eq!(
            insight_view(root.path(), &revision, InsightKind::Highlights)
                .unwrap()
                .state,
            "not_requested"
        );
        memex_store::revise_item_metadata(
            root.path(),
            &item,
            memex_store::ItemMetadataUpdate {
                expected_revision_id: revision.revision_id.clone(),
                title: revision.title.clone(),
                manual_tags: vec!["synthetic-tag".into()],
            },
        )
        .unwrap();
        let (_, current) = memex_store::read_item(root.path(), &item).unwrap();
        queue.carry(&revision.revision_id, &current).unwrap();
        assert!(
            queue.queued_kinds().unwrap().is_empty(),
            "metadata carry must not upgrade a v1 request"
        );
        let revision = current;
        queue.request(&revision, InsightKind::Highlights).unwrap();
        assert_eq!(queue.queued_kinds().unwrap(), [InsightKind::Highlights]);
        // A result whose translated coordinate space no longer exists is not displayed.
        database.execute("UPDATE insight_jobs SET state='succeeded',items_json='[]',translation_analysis_id='analysis_missing'", []).unwrap();
        let view = insight_view(root.path(), &revision, InsightKind::Highlights).unwrap();
        assert_eq!(view.reason.as_deref(), Some("insight_translation_changed"));
        assert!(view.items.is_none());
    }

    #[test]
    fn metadata_carry_preserves_running_insight_outcomes() {
        struct DuringGeneration<'a> {
            root: &'a Path,
            item: &'a str,
            carry_before: bool,
            fail: bool,
            preflight: bool,
        }
        impl DuringGeneration<'_> {
            fn edit(&self) {
                let (_, previous) = memex_store::read_item(self.root, self.item).unwrap();
                memex_store::revise_item_metadata(
                    self.root,
                    self.item,
                    memex_store::ItemMetadataUpdate {
                        expected_revision_id: previous.revision_id.clone(),
                        title: previous.title,
                        manual_tags: vec![if self.preflight {
                            "during-preflight"
                        } else {
                            "during-provider"
                        }
                        .into()],
                    },
                )
                .unwrap();
                if self.carry_before {
                    let (_, current) = memex_store::read_item(self.root, self.item).unwrap();
                    InsightQueue::open(self.root)
                        .unwrap()
                        .carry(&previous.revision_id, &current)
                        .unwrap();
                }
            }
        }
        impl InsightModel for DuringGeneration<'_> {
            fn descriptor(&self) -> TranslationModelDescriptor {
                if self.preflight {
                    self.edit();
                }
                TranslationModelDescriptor {
                    model_id: "synthetic-carry".into(),
                    model_sha256: format!("sha256:{}", "7".repeat(64)),
                    adapter_id: "synthetic".into(),
                    adapter_version: "1".into(),
                    request_timeout: std::time::Duration::from_secs(1),
                }
            }
            fn generate(
                &mut self,
                _: InsightModelInput<'_>,
            ) -> std::result::Result<InsightOutput, TranslationModelError> {
                if !self.preflight {
                    self.edit();
                }
                if self.fail {
                    Err(TranslationModelError::OutcomeUnknown)
                } else {
                    Ok(InsightOutput {
                        items: serde_json::json!([{"block_index":0,"quote":"body","style":"bold",
                            "original":{"text":"body","block":0,"start":0,"end":4},"translation":null}]),
                        dropped: 0,
                        translation_used: false,
                    })
                }
            }
        }
        for carry_before in [false, true] {
            for fail in [false, true] {
                let root = tempdir().unwrap();
                let item = ingest_one(root.path(), Sensitivity::Private);
                let (_, previous) = memex_store::read_item(root.path(), &item).unwrap();
                let mut queue = InsightQueue::open(root.path()).unwrap();
                queue.request(&previous, InsightKind::Highlights).unwrap();
                let mut model = DuringGeneration {
                    root: root.path(),
                    item: &item,
                    carry_before,
                    fail,
                    preflight: false,
                };
                assert!(queue
                    .process_next(InsightKind::Highlights, &mut model)
                    .unwrap());
                let (_, current) = memex_store::read_item(root.path(), &item).unwrap();
                // The metadata writer may arrive before or after the provider's completion.
                queue.carry(&previous.revision_id, &current).unwrap();
                drop(queue);
                let mut queue = InsightQueue::open(root.path()).unwrap();
                assert!(queue.queued_kinds().unwrap().is_empty());
                let view = insight_view(root.path(), &current, InsightKind::Highlights).unwrap();
                if fail {
                    // An uncertain outcome waits for a bounded automatic retry (backoff).
                    assert_eq!(view.state, "queued");
                    // An explicit retry remains valid even if the metadata writer repeats carry.
                    queue.request(&current, InsightKind::Highlights).unwrap();
                    queue.carry(&previous.revision_id, &current).unwrap();
                    assert_eq!(queue.queued_kinds().unwrap(), [InsightKind::Highlights]);
                } else {
                    assert_eq!(view.state, "succeeded");
                    assert_eq!(view.items.unwrap()[0]["quote"], "body");
                }
            }
        }
        // Lease -> metadata-only edit before the provider call: the job is not stale (same
        // body), runs once and the new revision inherits its result.
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Private);
        let (_, previous) = memex_store::read_item(root.path(), &item).unwrap();
        let mut queue = InsightQueue::open(root.path()).unwrap();
        queue.request(&previous, InsightKind::Highlights).unwrap();
        let mut model = DuringGeneration {
            root: root.path(),
            item: &item,
            carry_before: true,
            fail: false,
            preflight: true,
        };
        assert!(queue
            .process_next(InsightKind::Highlights, &mut model)
            .unwrap());
        assert!(queue.queued_kinds().unwrap().is_empty());
        let (_, current) = memex_store::read_item(root.path(), &item).unwrap();
        assert_eq!(
            insight_view(root.path(), &current, InsightKind::Highlights)
                .unwrap()
                .state,
            "succeeded"
        );
        assert!(queue.queued_kinds().unwrap().is_empty());
    }

    #[test]
    fn expired_title_and_insight_attempts_cannot_complete_a_new_lease() {
        use std::sync::mpsc::{self, Receiver, Sender};
        use std::time::Duration;
        struct BlockedModel {
            entered: Sender<()>,
            release: Receiver<()>,
            newer: bool,
        }
        impl BlockedModel {
            fn descriptor(&self) -> TranslationModelDescriptor {
                TranslationModelDescriptor {
                    model_id: "synthetic-fence".into(),
                    model_sha256: format!("sha256:{}", "7".repeat(64)),
                    adapter_id: "synthetic".into(),
                    adapter_version: "1".into(),
                    request_timeout: Duration::from_secs(1),
                }
            }
            fn wait(&self) {
                self.entered.send(()).unwrap();
                self.release.recv_timeout(Duration::from_secs(15)).unwrap();
            }
        }
        impl TitleTranslationModel for BlockedModel {
            fn descriptor(&self) -> TranslationModelDescriptor {
                self.descriptor()
            }
            fn translate_titles(
                &mut self,
                _: TitleBatchInput<'_>,
            ) -> std::result::Result<Vec<Option<String>>, TranslationModelError> {
                self.wait();
                Ok(vec![Some(
                    if self.newer {
                        "新尝试标题"
                    } else {
                        "旧尝试标题"
                    }
                    .into(),
                )])
            }
        }
        impl InsightModel for BlockedModel {
            fn descriptor(&self) -> TranslationModelDescriptor {
                self.descriptor()
            }
            fn generate(
                &mut self,
                _: InsightModelInput<'_>,
            ) -> std::result::Result<InsightOutput, TranslationModelError> {
                self.wait();
                Ok(InsightOutput {
                    items: serde_json::json!([{"block_index":0,"quote":"body",
                    "style":"bold","label":if self.newer { "新结果" } else { "旧结果" },
                    "original":{"text":"body","block":0,"start":0,"end":4},"translation":null}]),
                    dropped: 0,
                    translation_used: false,
                })
            }
        }
        for title in [true, false] {
            let root = tempdir().unwrap();
            let item = ingest_one(root.path(), Sensitivity::Private);
            let (_, mut revision) = memex_store::read_item(root.path(), &item).unwrap();
            if title {
                TitleTranslationQueue::open(root.path())
                    .unwrap()
                    .request_capture(&item, &revision.revision_id)
                    .unwrap();
            } else {
                InsightQueue::open(root.path())
                    .unwrap()
                    .request(&revision, InsightKind::Highlights)
                    .unwrap();
            }
            let start = |newer| {
                let (entered, waiting) = mpsc::channel();
                let (release, resume) = mpsc::channel();
                let path = root.path().to_owned();
                let thread = std::thread::spawn(move || {
                    let mut model = BlockedModel {
                        entered,
                        release: resume,
                        newer,
                    };
                    if title {
                        TitleTranslationQueue::open(path)
                            .unwrap()
                            .process_next_batch(&mut model)
                    } else {
                        InsightQueue::open(path)
                            .unwrap()
                            .process_next(InsightKind::Highlights, &mut model)
                    }
                });
                waiting.recv_timeout(Duration::from_secs(15)).unwrap();
                (thread, release)
            };
            let (old, release_old) = start(false);
            if !title {
                let edit = memex_store::revise_item_metadata(
                    root.path(),
                    &item,
                    memex_store::ItemMetadataUpdate {
                        expected_revision_id: revision.revision_id.clone(),
                        title: revision.title.clone(),
                        manual_tags: vec!["before-expiry".into()],
                    },
                )
                .unwrap();
                revision = memex_store::read_item(root.path(), &item).unwrap().1;
                InsightQueue::open(root.path())
                    .unwrap()
                    .carry(&edit.previous_revision_id, &revision)
                    .unwrap();
            }
            let db =
                rusqlite::Connection::open(root.path().join("analysis/translation-jobs.sqlite3"))
                    .unwrap();
            let table = if title {
                "title_translations"
            } else {
                "insight_jobs"
            };
            db.execute(
                &format!("UPDATE {table} SET lease_until=0 WHERE state='running'"),
                [],
            )
            .unwrap();
            if title {
                let mut queue = TitleTranslationQueue::open(root.path()).unwrap();
                assert!(!queue.has_work().unwrap());
                assert!(queue.request_backfill(&item, &revision.title).unwrap());
            } else {
                let mut queue = InsightQueue::open(root.path()).unwrap();
                assert!(queue.queued_kinds().unwrap().is_empty());
                queue.request(&revision, InsightKind::Highlights).unwrap();
            }
            let (new, release_new) = start(true);
            release_old.send(()).unwrap();
            let old_result = old.join().unwrap();
            release_new.send(()).unwrap();
            let new_result = new.join().unwrap();
            assert!(
                old_result.is_err(),
                "expired attempt must lose its completion right"
            );
            assert!(new_result.unwrap());
            if title {
                assert_eq!(
                    translated_title(root.path(), &item, &revision.revision_id, &revision.title)
                        .unwrap()
                        .unwrap()
                        .translated_title,
                    "新尝试标题"
                );
            } else {
                assert_eq!(
                    insight_view(root.path(), &revision, InsightKind::Highlights)
                        .unwrap()
                        .items
                        .unwrap()[0]["label"],
                    "新结果"
                );
            }
        }
    }

    #[test]
    fn suggested_title_is_split_from_minutes_and_old_minutes_read_unchanged() {
        let v3 = "要旨。\n- 要点\n结论。\n\n建议标题：《人工智能与教育的未来》\n";
        assert_eq!(
            split_suggested_title(v3),
            ("要旨。\n- 要点\n结论。".to_owned(), Some("人工智能与教育的未来".to_owned()))
        );
        assert_eq!(
            split_suggested_title("要旨。\n建议标题: AI 与教育 / AI and Education").1.as_deref(),
            Some("AI 与教育 / AI and Education")
        );
        let v2 = "要旨。\n- 要点\n结论。";
        assert_eq!(split_suggested_title(v2), (v2.to_owned(), None));
        // An unusable title line is dropped from the minutes but yields no suggestion.
        assert_eq!(split_suggested_title("要旨。\n建议标题：   ").1, None);
        assert_eq!(split_suggested_title("要旨。\n建议标题：   ").0, "要旨。");
        assert!(is_ai_minutes_version("2") && is_ai_minutes_version("3") && !is_ai_minutes_version("1"));
    }

    #[test]
    fn malformed_insight_output_is_retried_automatically_with_a_bound() {
        struct Malformed(usize);
        impl InsightModel for Malformed {
            fn descriptor(&self) -> TranslationModelDescriptor {
                TranslationModelDescriptor {
                    model_id: "synthetic-malformed".into(),
                    model_sha256: format!("sha256:{}", "7".repeat(64)),
                    adapter_id: "synthetic".into(),
                    adapter_version: "1".into(),
                    request_timeout: std::time::Duration::from_secs(1),
                }
            }
            fn generate(
                &mut self,
                _: InsightModelInput<'_>,
            ) -> std::result::Result<InsightOutput, TranslationModelError> {
                self.0 += 1;
                Err(TranslationModelError::MalformedOutput)
            }
        }
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Private);
        let (_, revision) = memex_store::read_item(root.path(), &item).unwrap();
        let mut queue = InsightQueue::open(root.path()).unwrap();
        queue.request(&revision, InsightKind::Highlights).unwrap();
        let database =
            rusqlite::Connection::open(root.path().join("analysis/translation-jobs.sqlite3"))
                .unwrap();
        let mut model = Malformed(0);
        for _ in 0..6 {
            queue.process_next(InsightKind::Highlights, &mut model).unwrap();
            // Backoff elapses: make the pending automatic retry due.
            database
                .execute("UPDATE insight_jobs SET updated_at='2000-01-01T00:00:00+00:00' WHERE state='queued'", [])
                .unwrap();
        }
        assert_eq!(model.0, 3, "first attempt plus two automatic retries");
        let view = insight_view(root.path(), &revision, InsightKind::Highlights).unwrap();
        assert_eq!(view.state, "failed");
        assert!(queue.queued_kinds().unwrap().is_empty());
    }

    #[test]
    fn title_capture_is_one_shot_and_backfill_is_explicit() {
        struct SyntheticTitles {
            fail: bool,
            calls: usize,
        }
        impl TitleTranslationModel for SyntheticTitles {
            fn descriptor(&self) -> TranslationModelDescriptor {
                TranslationModelDescriptor {
                    model_id: "synthetic-title-test".into(),
                    model_sha256: format!("sha256:{}", "7".repeat(64)),
                    adapter_id: "synthetic".into(),
                    adapter_version: "1".into(),
                    request_timeout: std::time::Duration::from_secs(1),
                }
            }

            fn translate_titles(
                &mut self,
                input: TitleBatchInput<'_>,
            ) -> std::result::Result<Vec<Option<String>>, TranslationModelError> {
                self.calls += 1;
                assert_eq!(input.titles(), &["Title"]);
                if self.fail {
                    Err(TranslationModelError::OutcomeUnknown)
                } else {
                    Ok(vec![Some("合成标题".into())])
                }
            }
        }
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Private);
        let (_, revision) = memex_store::read_item(root.path(), &item).unwrap();
        let mut queue = TitleTranslationQueue::open(root.path()).unwrap();
        // Store/native import alone must not schedule a model call.
        assert!(!queue.has_work().unwrap());
        assert!(queue.request_capture(&item, &revision.revision_id).unwrap());
        assert!(!queue.request_capture(&item, &revision.revision_id).unwrap());
        let mut model = SyntheticTitles {
            fail: true,
            calls: 0,
        };
        assert!(queue.process_next_batch(&mut model).unwrap());
        drop(queue);
        let mut queue = TitleTranslationQueue::open(root.path()).unwrap();
        // A capture replay cannot repeat a potentially billed request after restart.
        assert!(!queue.request_capture(&item, &revision.revision_id).unwrap());
        assert!(!queue.process_next_batch(&mut model).unwrap());
        assert_eq!(model.calls, 1);
        assert!(queue.request_backfill(&item, &revision.title).unwrap());
        model.fail = false;
        assert!(queue.process_next_batch(&mut model).unwrap());
        let view = translated_title(root.path(), &item, &revision.revision_id, &revision.title)
            .unwrap()
            .unwrap();
        assert_eq!(view.translated_title, "合成标题");
        assert_eq!(view.title_source, "title_only");
        assert!(
            translated_title(root.path(), &item, &revision.revision_id, "Changed title")
                .unwrap()
                .is_none()
        );
        assert!(!queue.request_capture(&item, &revision.revision_id).unwrap());
        assert!(!queue.request_backfill(&item, &revision.title).unwrap());
        assert_eq!(model.calls, 2);

        let trash_root = tempdir().unwrap();
        let trashed_item = ingest_one(trash_root.path(), Sensitivity::Private);
        let (_, trashed_revision) =
            memex_store::read_item(trash_root.path(), &trashed_item).unwrap();
        let mut queue = TitleTranslationQueue::open(trash_root.path()).unwrap();
        assert!(queue
            .request_capture(&trashed_item, &trashed_revision.revision_id)
            .unwrap());
        memex_store::trash_item(
            trash_root.path(),
            &trashed_item,
            &trashed_revision.revision_id,
            "owner_request",
            None,
            None,
        )
        .unwrap();
        assert!(queue.process_next_batch(&mut model).unwrap());
        assert!(!queue
            .request_capture(&trashed_item, &trashed_revision.revision_id)
            .unwrap());
        assert!(!queue.has_work().unwrap());
        assert_eq!(model.calls, 2, "trashed titles must not reach the provider");
    }

    #[test]
    fn prepare_is_deterministic_with_typed_shape() {
        let root = tempdir().unwrap();
        let item_id = ingest_one(root.path(), Sensitivity::Public);
        let request = request(item_id);
        let first = prepare_record(root.path(), &request).unwrap();
        let second = prepare_record(root.path(), &request).unwrap();
        assert_eq!(first, second);
        assert!(first.analysis_id.starts_with("analysis_"));
        assert_eq!(first.inputs.len(), 1);
        assert!(first.inputs[0].revision_sha256.starts_with("sha256:"));
        assert!(!serde_json::to_string(&first)
            .unwrap()
            .contains("generated_text"));
    }

    #[test]
    fn changed_body_changes_analysis_id() {
        let root = tempdir().unwrap();
        let item_id = ingest_one(root.path(), Sensitivity::Public);
        let first = request(item_id);
        let mut second = first.clone();
        second.body_markdown = "changed".into();
        assert_ne!(
            prepare_record(root.path(), &first).unwrap().analysis_id,
            prepare_record(root.path(), &second).unwrap().analysis_id
        );
    }

    #[test]
    fn generated_text_is_typed_bounded_and_identity_bearing() {
        let root = tempdir().unwrap();
        let item_id = ingest_one(root.path(), Sensitivity::Public);
        let mut summary = request(item_id.clone());
        summary.title = "准确摘要".into();
        summary.body_markdown = "只保留原文事实。".into();
        summary.generated_text = Some(GeneratedTextDescriptor {
            purpose: GeneratedTextPurpose::Summary,
            source_language: "en".into(),
            target_language: "zh-CN".into(),
            output_limit: Some(TextOutputLimit {
                unit: TextLengthUnit::Characters,
                maximum: 32,
            }),
            summary_job_id: None,
            translation_job_id: None,
            model_id: "luna-low".into(),
            model_sha256: None,
            prompt_id: "memex.faithful-summary".into(),
            prompt_version: "1".into(),
            prompt_sha256: format!("sha256:{}", "2".repeat(64)),
        });
        let record = prepare_record(root.path(), &summary).unwrap();
        assert_eq!(record.generated_text, summary.generated_text);
        assert!(render_record_markdown(&record).contains("model_id=luna-low"));
        assert!(
            render_record_index_json(std::slice::from_ref(&record)).contains("\"generated_text\"")
        );

        let mut other_model = summary.clone();
        other_model.generated_text.as_mut().unwrap().model_id = "luna-medium".into();
        assert_ne!(
            record.analysis_id,
            prepare_record(root.path(), &other_model)
                .unwrap()
                .analysis_id
        );

        let mut over_limit = summary.clone();
        over_limit
            .generated_text
            .as_mut()
            .unwrap()
            .output_limit
            .as_mut()
            .unwrap()
            .maximum = 2;
        assert!(prepare_record(root.path(), &over_limit).is_err());
        let mut wrong_kind = summary.clone();
        wrong_kind.analysis_kind = AnalysisKind::Translation;
        assert!(prepare_record(root.path(), &wrong_kind).is_err());
        let mut missing_limit = summary.clone();
        missing_limit.generated_text.as_mut().unwrap().output_limit = None;
        assert!(prepare_record(root.path(), &missing_limit).is_err());
        let mut async_summary = summary.clone();
        async_summary
            .generated_text
            .as_mut()
            .unwrap()
            .summary_job_id = Some(format!("summary_{}", "4".repeat(64)));
        assert!(prepare_record(root.path(), &async_summary).is_err());
        async_summary.generated_text.as_mut().unwrap().model_sha256 =
            Some(format!("sha256:{}", "5".repeat(64)));
        assert!(prepare_record(root.path(), &async_summary).is_ok());
        async_summary
            .generated_text
            .as_mut()
            .unwrap()
            .translation_job_id = Some(format!("translation_{}", "6".repeat(64)));
        assert!(prepare_record(root.path(), &async_summary).is_err());

        let mut translation = request(item_id);
        translation.analysis_kind = AnalysisKind::Translation;
        translation.title = "完整译文".into();
        translation.body_markdown = "忠实的 Markdown 译文。".into();
        translation.generated_text = Some(GeneratedTextDescriptor {
            purpose: GeneratedTextPurpose::DocumentTranslation,
            source_language: "en".into(),
            target_language: "zh-CN".into(),
            output_limit: None,
            summary_job_id: None,
            translation_job_id: None,
            model_id: "luna-low".into(),
            model_sha256: None,
            prompt_id: "memex.faithful-translation".into(),
            prompt_version: "1".into(),
            prompt_sha256: format!("sha256:{}", "3".repeat(64)),
        });
        assert!(prepare_record(root.path(), &translation).is_ok());
        translation.generated_text.as_mut().unwrap().target_language = "en".into();
        assert!(prepare_record(root.path(), &translation).is_err());
        translation.generated_text.as_mut().unwrap().target_language = "EN".into();
        assert!(prepare_record(root.path(), &translation).is_err());
        translation.generated_text.as_mut().unwrap().target_language = "zh-CN".into();
        translation.generated_text.as_mut().unwrap().source_language = "en-123456789".into();
        assert!(prepare_record(root.path(), &translation).is_err());
        translation.generated_text.as_mut().unwrap().source_language = "en".into();

        let mut title_translation = translation;
        title_translation.body_markdown = "Conservative title".into();
        let descriptor = title_translation.generated_text.as_mut().unwrap();
        descriptor.purpose = GeneratedTextPurpose::TitleTranslation;
        descriptor.target_language = "zh-CN".into();
        descriptor.output_limit = Some(TextOutputLimit {
            unit: TextLengthUnit::Characters,
            maximum: 64,
        });
        assert!(prepare_record(root.path(), &title_translation).is_ok());
        title_translation.body_markdown = "# Heading".into();
        assert!(prepare_record(root.path(), &title_translation).is_err());

        let (prompt_id, prompt, prompt_sha256) =
            generated_text_prompt(GeneratedTextPurpose::Summary);
        assert_eq!(prompt_id, FAITHFUL_SUMMARY_PROMPT_ID);
        assert!(prompt.contains("Do not add facts"));
        assert_eq!(
            prompt_sha256,
            format!("sha256:{:x}", Sha256::digest(prompt))
        );
    }

    #[test]
    fn private_input_cannot_be_downgraded() {
        let root = tempdir().unwrap();
        let item_id = ingest_one(root.path(), Sensitivity::Private);
        let mut request = request(item_id);
        request.requested_sensitivity = Some(Sensitivity::Internal);
        assert_eq!(
            prepare_record(root.path(), &request).unwrap().sensitivity,
            Sensitivity::Private
        );
    }

    #[test]
    fn invalid_requests_are_rejected() {
        let root = tempdir().unwrap();
        let item_id = ingest_one(root.path(), Sensitivity::Public);
        let mut duplicate = request(item_id.clone());
        duplicate.item_ids.push(item_id);
        assert!(prepare_record(root.path(), &duplicate).is_err());
        assert!(prepare_record(root.path(), &request("item_missing".into())).is_err());
        let known_item = ingest_one(root.path(), Sensitivity::Public);
        let mut invalid_time = request(known_item.clone());
        invalid_time.created_at = "not-a-time".into();
        assert!(prepare_record(root.path(), &invalid_time).is_err());
        let mut invalid_digest = request(known_item);
        invalid_digest.config_digest = "sha256:ABC".into();
        assert!(prepare_record(root.path(), &invalid_digest).is_err());
    }

    #[test]
    fn record_validation_rejects_tampered_topics_and_evidence() {
        let root = tempdir().unwrap();
        let item_id = ingest_one(root.path(), Sensitivity::Public);
        let record = prepare_record(root.path(), &request(item_id)).unwrap();
        assert!(validate_record(&record).is_ok());

        let mut empty_topic = record.clone();
        empty_topic.topics = vec![String::new()];
        assert!(validate_record(&empty_topic).is_err());

        let mut invalid_evidence = record;
        invalid_evidence.inputs[0].evidence_ids[0] = "bad evidence".into();
        assert!(validate_record(&invalid_evidence).is_err());
    }

    #[test]
    fn apply_replay_receipt_is_private_and_readable() {
        let root = tempdir().unwrap();
        let item_id = ingest_one(root.path(), Sensitivity::Public);
        let record = prepare_record(root.path(), &request(item_id)).unwrap();
        let created = apply_record(root.path(), &record).unwrap();
        let analysis = root.path().join("analysis");
        let record_markdown = analysis
            .join("records")
            .join(format!("{}.md", record.analysis_id));
        let markdown = fs::read_to_string(&record_markdown).unwrap();
        assert_eq!(markdown, render_record_markdown(&record));
        for value in [
            &record.analysis_id,
            &record.inputs[0].revision_id,
            &record.inputs[0].revision_sha256,
            &record.provenance.config_digest,
            "## Analysis",
            &record.body_markdown,
        ] {
            assert!(markdown.contains(value));
        }
        assert_eq!(
            fs::read_to_string(analysis.join("index.json")).unwrap(),
            render_record_index_json(std::slice::from_ref(&record))
        );
        assert_eq!(
            fs::read_to_string(analysis.join("index.md")).unwrap(),
            render_record_index_markdown(std::slice::from_ref(&record))
        );
        fs::remove_file(&record_markdown).unwrap();
        fs::remove_file(analysis.join("index.json")).unwrap();
        fs::remove_file(analysis.join("index.md")).unwrap();
        let replayed = apply_record(root.path(), &record).unwrap();
        assert_eq!(created.status, ApplyStatus::Created);
        assert_eq!(replayed.status, ApplyStatus::Replayed);
        assert!(replayed.warning_codes.is_empty());
        assert!(record_markdown.exists());
        assert!(!serde_json::to_string(&created).unwrap().contains("body"));
        assert_eq!(
            read_record(root.path(), &record.analysis_id).unwrap(),
            record
        );
        assert_eq!(list_records(root.path()).unwrap(), vec![record]);
    }

    #[test]
    fn missing_list_does_not_mutate_library() {
        let root = tempdir().unwrap();
        assert!(list_records(root.path()).unwrap().is_empty());
        assert!(!root.path().join("analysis").exists());
    }

    #[test]
    fn tampered_record_fails_read_list_and_replay() {
        let root = tempdir().unwrap();
        let item_id = ingest_one(root.path(), Sensitivity::Public);
        let record = prepare_record(root.path(), &request(item_id)).unwrap();
        apply_record(root.path(), &record).unwrap();
        let path = root
            .path()
            .join("analysis/records")
            .join(format!("{}.json", record.analysis_id));
        let mut swapped = record.clone();
        swapped.body_markdown = "swapped valid body".into();
        swapped.analysis_id.clear();
        swapped.analysis_id = analysis_id(&swapped).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(&swapped).unwrap()).unwrap();
        assert!(validate_record(&swapped).is_ok());
        assert!(read_record(root.path(), &record.analysis_id).is_err());
        assert!(list_records(root.path()).is_err());
        assert!(apply_record(root.path(), &record).is_err());
        fs::write(&path, b"{}").unwrap();
        assert!(read_record(root.path(), &record.analysis_id).is_err());
        assert!(list_records(root.path()).is_err());
        assert!(apply_record(root.path(), &record).is_err());
        assert!(recall(
            root.path(),
            &RecallQuery {
                query: "body".into(),
                max_sensitivity: Sensitivity::Public,
                limit: 10
            }
        )
        .is_err());
    }

    #[test]
    fn read_request_rejects_invalid_and_oversize() {
        let root = tempdir().unwrap();
        let invalid = root.path().join("invalid.json");
        fs::write(&invalid, b"{}").unwrap();
        assert!(read_request(&invalid).is_err());
        let large = root.path().join("large.json");
        fs::write(&large, vec![b'x'; 1024 * 1024 + 1]).unwrap();
        assert!(read_request(&large).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_and_lock_fail_closed() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let alias = root.path().join("alias");
        symlink(outside.path(), &alias).unwrap();
        assert!(list_records(&alias).is_err());
        let request_alias = root.path().join("request.json");
        symlink(outside.path().join("missing.json"), &request_alias).unwrap();
        assert!(read_request(&request_alias).is_err());
        let item_id = ingest_one(root.path(), Sensitivity::Public);
        let record = prepare_record(root.path(), &request(item_id)).unwrap();
        fs::create_dir(root.path().join("analysis")).unwrap();
        let sentinel = outside.path().join("sentinel");
        fs::write(&sentinel, "safe").unwrap();
        symlink(&sentinel, root.path().join("analysis/.lock")).unwrap();
        assert!(apply_record(root.path(), &record).is_err());
        assert_eq!(fs::read_to_string(sentinel).unwrap(), "safe");
        fs::remove_file(root.path().join("analysis/.lock")).unwrap();
        symlink(
            outside.path().join("missing-lock"),
            root.path().join("analysis/.lock"),
        )
        .unwrap();
        assert!(apply_record(root.path(), &record).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_and_record_symlinks_do_not_touch_outside() {
        use std::os::unix::fs::symlink;
        let parent = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let real_library = outside.path().join("real-library");
        fs::create_dir(&real_library).unwrap();
        let ancestor = parent.path().join("link");
        symlink(outside.path(), &ancestor).unwrap();
        let aliased_library = ancestor.join("real-library");
        assert!(list_records(&aliased_library).is_err());

        let item_id = ingest_one(&real_library, Sensitivity::Public);
        let record = prepare_record(&real_library, &request(item_id)).unwrap();
        assert!(apply_record(&aliased_library, &record).is_err());
        assert!(!real_library.join("analysis").exists());

        fs::create_dir(real_library.join("analysis")).unwrap();
        fs::create_dir(real_library.join("analysis/records")).unwrap();
        let sentinel = parent.path().join("sentinel");
        fs::write(&sentinel, "safe").unwrap();
        let target = real_library
            .join("analysis/records")
            .join(format!("{}.json", record.analysis_id));
        symlink(&sentinel, &target).unwrap();
        assert!(apply_record(&real_library, &record).is_err());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "safe");
        fs::remove_file(&target).unwrap();
        symlink(parent.path().join("missing-record"), &target).unwrap();
        assert!(apply_record(&real_library, &record).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn projection_symlinks_warn_without_touching_sentinels() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let record = prepare_record(
            root.path(),
            &request(ingest_one(root.path(), Sensitivity::Public)),
        )
        .unwrap();
        apply_record(root.path(), &record).unwrap();
        let analysis = root.path().join("analysis");
        let sentinel = outside.path().join("sentinel");
        fs::write(&sentinel, "safe").unwrap();
        fs::remove_file(
            analysis
                .join("records")
                .join(format!("{}.md", record.analysis_id)),
        )
        .unwrap();
        symlink(
            &sentinel,
            analysis
                .join("records")
                .join(format!("{}.md", record.analysis_id)),
        )
        .unwrap();
        let receipt = apply_record(root.path(), &record).unwrap();
        assert_eq!(receipt.status, ApplyStatus::Replayed);
        assert_eq!(
            receipt.warning_codes,
            vec!["record_markdown_projection_pending"]
        );
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "safe");
        fs::remove_file(analysis.join("index.json")).unwrap();
        fs::remove_file(analysis.join("index.md")).unwrap();
        symlink(&sentinel, analysis.join("index.json")).unwrap();
        symlink(&sentinel, analysis.join("index.md")).unwrap();
        let receipt = apply_record(root.path(), &record).unwrap();
        assert_eq!(
            receipt.warning_codes,
            vec![
                "record_markdown_projection_pending",
                "analysis_json_index_projection_pending",
                "analysis_markdown_index_projection_pending"
            ]
        );
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "safe");
        assert_eq!(
            read_record(root.path(), &record.analysis_id).unwrap(),
            record
        );
    }

    #[test]
    fn foreign_record_entry_preserves_existing_indexes() {
        let root = tempdir().unwrap();
        let item_id = ingest_one(root.path(), Sensitivity::Public);
        let first = prepare_record(root.path(), &request(item_id.clone())).unwrap();
        apply_record(root.path(), &first).unwrap();
        let analysis = root.path().join("analysis");
        let json_before = fs::read(analysis.join("index.json")).unwrap();
        let markdown_before = fs::read(analysis.join("index.md")).unwrap();
        fs::write(analysis.join("records/foreign.tmp"), "foreign").unwrap();
        let mut second_request = request(item_id);
        second_request.body_markdown = "second".into();
        let second = prepare_record(root.path(), &second_request).unwrap();
        let receipt = apply_record(root.path(), &second).unwrap();
        assert_eq!(receipt.status, ApplyStatus::Created);
        assert_eq!(
            receipt.warning_codes,
            vec![
                "analysis_json_index_projection_pending",
                "analysis_markdown_index_projection_pending"
            ]
        );
        assert_eq!(
            read_record(root.path(), &second.analysis_id).unwrap(),
            second
        );
        assert_eq!(fs::read(analysis.join("index.json")).unwrap(), json_before);
        assert_eq!(
            fs::read(analysis.join("index.md")).unwrap(),
            markdown_before
        );
    }

    #[test]
    fn apply_rejects_valid_json_current_revision_integrity_tamper() {
        let root = tempdir().unwrap();
        let item_id = ingest_one(root.path(), Sensitivity::Public);
        let record = prepare_record(root.path(), &request(item_id)).unwrap();
        let path = root
            .path()
            .join("items")
            .join(&record.inputs[0].item_id)
            .join("revisions")
            .join(format!("{}.json", record.inputs[0].revision_id));
        let mut revision: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        revision["content_markdown"] = serde_json::json!("VALID_JSON_TAMPER_SENTINEL");
        revision["sensitivity"] = serde_json::json!("internal");
        fs::write(&path, serde_json::to_vec_pretty(&revision).unwrap()).unwrap();
        let error = apply_record(root.path(), &record).unwrap_err();
        assert!(!error.to_string().contains("VALID_JSON_TAMPER_SENTINEL"));
        assert!(!root.path().join("analysis").exists());
        assert!(recall(
            root.path(),
            &RecallQuery {
                query: "body".into(),
                max_sensitivity: Sensitivity::Public,
                limit: 10
            }
        )
        .is_err());
    }

    fn link_request(from: String, target: LinkTargetRequest) -> LinkRequest {
        LinkRequest {
            schema_version: 1,
            from_analysis_id: from,
            target,
            relation: LinkRelation::Related,
            rationale_markdown: "because".into(),
            created_at: "2026-07-31T00:00:00Z".into(),
            actor_kind: ActorKind::Agent,
            actor_id: "agent".into(),
            tool_id: "tool".into(),
            tool_version: "1".into(),
            config_digest: format!("sha256:{:x}", Sha256::digest(b"config")),
        }
    }

    #[test]
    fn prepare_analysis_link_is_deterministic() {
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Public);
        let a = prepare_record(root.path(), &request(item.clone())).unwrap();
        apply_record(root.path(), &a).unwrap();
        let mut r = request(item);
        r.body_markdown = "other".into();
        let b = prepare_record(root.path(), &r).unwrap();
        apply_record(root.path(), &b).unwrap();
        let request = link_request(
            a.analysis_id.clone(),
            LinkTargetRequest {
                kind: LinkTargetKind::Analysis,
                id: b.analysis_id,
                revision_id: None,
            },
        );
        let first = prepare_link(root.path(), &request).unwrap();
        assert_eq!(first, prepare_link(root.path(), &request).unwrap());
        assert!(first.link_id.starts_with("analysis_link_"));
        assert_eq!(first.sensitivity, Sensitivity::Public);
    }
    #[test]
    fn prepare_item_link_uses_current_revision_and_max_sensitivity() {
        let root = tempdir().unwrap();
        let private = ingest_one(root.path(), Sensitivity::Private);
        let from = prepare_record(root.path(), &request(private.clone())).unwrap();
        apply_record(root.path(), &from).unwrap();
        let (_, rev) = memex_store::read_item(root.path(), &private).unwrap();
        let mut request = link_request(
            from.analysis_id,
            LinkTargetRequest {
                kind: LinkTargetKind::ItemRevision,
                id: private,
                revision_id: Some(rev.revision_id),
            },
        );
        assert_eq!(
            prepare_link(root.path(), &request).unwrap().sensitivity,
            Sensitivity::Private
        );
        request.target.revision_id = Some("revision_wrong".into());
        assert!(prepare_link(root.path(), &request).is_err());
    }
    #[test]
    fn invalid_link_target_shapes_are_rejected() {
        let target = LinkTargetRequest {
            kind: LinkTargetKind::Analysis,
            id: "analysis_0000000000000000000000000000000000000000000000000000000000000000".into(),
            revision_id: Some("revision_x".into()),
        };
        let request = link_request(
            "analysis_0000000000000000000000000000000000000000000000000000000000000000".into(),
            target,
        );
        assert!(validate_link_request(&request).is_err());
        let mut request = request;
        request.target = LinkTargetRequest {
            kind: LinkTargetKind::ItemRevision,
            id: "bad".into(),
            revision_id: None,
        };
        assert!(validate_link_request(&request).is_err());
        request.rationale_markdown.clear();
        assert!(validate_link_request(&request).is_err());
    }

    #[test]
    fn read_link_request_accepts_regular_and_rejects_invalid_or_oversize() {
        let root = tempdir().unwrap();
        let request = link_request(
            "analysis_0000000000000000000000000000000000000000000000000000000000000000".into(),
            LinkTargetRequest {
                kind: LinkTargetKind::Analysis,
                id: "analysis_0000000000000000000000000000000000000000000000000000000000000000"
                    .into(),
                revision_id: None,
            },
        );
        let valid = root.path().join("request.json");
        fs::write(&valid, serde_json::to_vec(&request).unwrap()).unwrap();
        assert_eq!(read_link_request(&valid).unwrap(), request);
        fs::write(root.path().join("invalid.json"), b"{}").unwrap();
        assert!(read_link_request(root.path().join("invalid.json")).is_err());
        fs::write(root.path().join("large.json"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
        assert!(read_link_request(root.path().join("large.json")).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn read_link_request_rejects_symlink_without_touching_target() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let target = root.path().join("target.json");
        fs::write(&target, b"sentinel").unwrap();
        let link = root.path().join("link.json");
        symlink(&target, &link).unwrap();
        assert!(read_link_request(&link).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"sentinel");
        fs::remove_file(&link).unwrap();
        symlink(root.path().join("missing"), &link).unwrap();
        assert!(read_link_request(&link).is_err());
    }

    #[test]
    fn link_receipt_is_private_and_endpoint_verification_is_exact() {
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Public);
        let source = prepare_record(root.path(), &request(item.clone())).unwrap();
        apply_record(root.path(), &source).unwrap();
        let (_, revision) = memex_store::read_item(root.path(), &item).unwrap();
        let mut request = link_request(
            source.analysis_id.clone(),
            LinkTargetRequest {
                kind: LinkTargetKind::ItemRevision,
                id: item,
                revision_id: Some(revision.revision_id),
            },
        );
        request.rationale_markdown = "UNIQUE_RATIONALE_SENTINEL".into();
        let link = prepare_link(root.path(), &request).unwrap();
        let receipt = LinkReceipt {
            link_id: link.link_id.clone(),
            status: ApplyStatus::Created,
            sensitivity: link.sensitivity,
            from_analysis_id: link.from_analysis_id.clone(),
            target: link.target.clone(),
            warning_codes: vec![],
        };
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(!json.contains("UNIQUE_RATIONALE_SENTINEL"));
        assert_eq!(receipt.link_id, link.link_id);
        assert_eq!(receipt.from_analysis_id, source.analysis_id);
        assert_eq!(receipt.target, link.target);
        assert_eq!(receipt.status, ApplyStatus::Created);
        assert!(verify_link_endpoints(root.path(), &link).is_ok());
        let mut changed = link;
        changed.sensitivity = Sensitivity::Private;
        assert!(verify_link_endpoints(root.path(), &changed).is_err());
    }

    #[test]
    fn apply_link_created_and_replayed() {
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Public);
        let source = prepare_record(root.path(), &request(item.clone())).unwrap();
        apply_record(root.path(), &source).unwrap();
        let (_, revision) = memex_store::read_item(root.path(), &item).unwrap();
        let link = prepare_link(
            root.path(),
            &link_request(
                source.analysis_id,
                LinkTargetRequest {
                    kind: LinkTargetKind::ItemRevision,
                    id: item,
                    revision_id: Some(revision.revision_id),
                },
            ),
        )
        .unwrap();
        assert_eq!(
            apply_link(root.path(), &link).unwrap().status,
            ApplyStatus::Created
        );
        assert_eq!(
            apply_link(root.path(), &link).unwrap().status,
            ApplyStatus::Replayed
        );
        assert_eq!(
            fs::read(
                root.path()
                    .join("analysis/links")
                    .join(format!("{}.json", link.link_id))
            )
            .unwrap(),
            serde_json::to_vec_pretty(&link).unwrap()
        );
    }
    #[test]
    fn read_link_returns_applied_link() {
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Public);
        let source = prepare_record(root.path(), &request(item.clone())).unwrap();
        apply_record(root.path(), &source).unwrap();
        let (_, revision) = memex_store::read_item(root.path(), &item).unwrap();
        let link = prepare_link(
            root.path(),
            &link_request(
                source.analysis_id,
                LinkTargetRequest {
                    kind: LinkTargetKind::ItemRevision,
                    id: item,
                    revision_id: Some(revision.revision_id),
                },
            ),
        )
        .unwrap();
        apply_link(root.path(), &link).unwrap();
        assert_eq!(read_link(root.path(), &link.link_id).unwrap(), link);
    }
    #[test]
    fn list_links_missing_is_read_only() {
        let root = tempdir().unwrap();
        assert!(list_links(root.path()).unwrap().is_empty());
        assert!(!root.path().join("analysis").exists());
    }
    #[test]
    fn list_links_returns_sorted_applied_links() {
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Public);
        let source = prepare_record(root.path(), &request(item.clone())).unwrap();
        apply_record(root.path(), &source).unwrap();
        let (_, rev) = memex_store::read_item(root.path(), &item).unwrap();
        let mut a = link_request(
            source.analysis_id.clone(),
            LinkTargetRequest {
                kind: LinkTargetKind::ItemRevision,
                id: item.clone(),
                revision_id: Some(rev.revision_id.clone()),
            },
        );
        let mut b = a.clone();
        b.relation = LinkRelation::Supports;
        a.rationale_markdown = "a".into();
        b.rationale_markdown = "b".into();
        let a = prepare_link(root.path(), &a).unwrap();
        let b = prepare_link(root.path(), &b).unwrap();
        apply_link(root.path(), &a).unwrap();
        apply_link(root.path(), &b).unwrap();
        let mut expected = vec![a.clone(), b.clone()];
        expected.sort_by(|x, y| x.link_id.cmp(&y.link_id));
        assert_eq!(list_links(root.path()).unwrap(), expected);
        assert_eq!(read_link(root.path(), &a.link_id).unwrap(), a);
        assert_eq!(read_link(root.path(), &b.link_id).unwrap(), b);
    }
    #[test]
    fn apply_link_rejects_endpoint_drift_before_links_directory() {
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Public);
        let source = prepare_record(root.path(), &request(item.clone())).unwrap();
        apply_record(root.path(), &source).unwrap();
        let (_, rev) = memex_store::read_item(root.path(), &item).unwrap();
        let link = prepare_link(
            root.path(),
            &link_request(
                source.analysis_id.clone(),
                LinkTargetRequest {
                    kind: LinkTargetKind::ItemRevision,
                    id: item.clone(),
                    revision_id: Some(rev.revision_id.clone()),
                },
            ),
        )
        .unwrap();
        fs::write(
            root.path()
                .join("items")
                .join(&item)
                .join("revisions")
                .join(format!("{}.json", rev.revision_id)),
            "{}",
        )
        .unwrap();
        assert!(apply_link(root.path(), &link).is_err());
        assert!(!root.path().join("analysis/links").exists());
        assert_eq!(
            read_record(root.path(), &source.analysis_id).unwrap(),
            source
        );
    }
    #[test]
    fn tampered_link_fails_read_list_and_replay() {
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Public);
        let source = prepare_record(root.path(), &request(item.clone())).unwrap();
        apply_record(root.path(), &source).unwrap();
        let (_, rev) = memex_store::read_item(root.path(), &item).unwrap();
        let link = prepare_link(
            root.path(),
            &link_request(
                source.analysis_id.clone(),
                LinkTargetRequest {
                    kind: LinkTargetKind::ItemRevision,
                    id: item,
                    revision_id: Some(rev.revision_id),
                },
            ),
        )
        .unwrap();
        apply_link(root.path(), &link).unwrap();
        let path = root
            .path()
            .join("analysis/links")
            .join(format!("{}.json", link.link_id));
        let mut swapped = link.clone();
        swapped.rationale_markdown = "swapped rationale".into();
        swapped.link_id.clear();
        swapped.link_id = link_id(&swapped).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(&swapped).unwrap()).unwrap();
        assert!(validate_link(&swapped).is_ok());
        assert!(read_link(root.path(), &link.link_id).is_err());
        assert!(list_links(root.path()).is_err());
        assert!(apply_link(root.path(), &link).is_err());
        fs::write(&path, "{}").unwrap();
        assert!(read_link(root.path(), &link.link_id).is_err());
        assert!(list_links(root.path()).is_err());
        assert!(apply_link(root.path(), &link).is_err());
        assert_eq!(
            read_record(root.path(), &source.analysis_id).unwrap(),
            source
        );
    }
    #[cfg(unix)]
    #[test]
    fn link_directory_and_record_symlinks_fail_closed() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Public);
        let source = prepare_record(root.path(), &request(item.clone())).unwrap();
        apply_record(root.path(), &source).unwrap();
        let (_, rev) = memex_store::read_item(root.path(), &item).unwrap();
        let link = prepare_link(
            root.path(),
            &link_request(
                source.analysis_id,
                LinkTargetRequest {
                    kind: LinkTargetKind::ItemRevision,
                    id: item,
                    revision_id: Some(rev.revision_id),
                },
            ),
        )
        .unwrap();
        let sentinel = outside.path().join("sentinel");
        fs::write(&sentinel, "safe").unwrap();
        symlink(outside.path(), root.path().join("analysis/links")).unwrap();
        assert!(apply_link(root.path(), &link).is_err());
        assert!(read_link(root.path(), &link.link_id).is_err());
        assert!(list_links(root.path()).is_err());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "safe");
        fs::remove_file(root.path().join("analysis/links")).unwrap();
        fs::create_dir(root.path().join("analysis/links")).unwrap();
        let path = root
            .path()
            .join("analysis/links")
            .join(format!("{}.json", link.link_id));
        symlink(&sentinel, &path).unwrap();
        assert!(apply_link(root.path(), &link).is_err());
        assert!(read_link(root.path(), &link.link_id).is_err());
        assert!(list_links(root.path()).is_err());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "safe");
        fs::remove_file(&path).unwrap();
        symlink(outside.path().join("missing"), &path).unwrap();
        assert!(apply_link(root.path(), &link).is_err());
    }
    #[test]
    fn recall_query_validation_rejects_invalid_bounds() {
        let valid = RecallQuery {
            query: "term".into(),
            max_sensitivity: Sensitivity::Public,
            limit: 1,
        };
        assert!(validate_recall_query(&valid).is_ok());
        for query in [" ".into(), "a\0b".into(), "x".repeat(4097)] {
            assert!(validate_recall_query(&RecallQuery {
                query,
                max_sensitivity: Sensitivity::Public,
                limit: 1
            })
            .is_err())
        }
        for limit in [0, 101] {
            assert!(validate_recall_query(&RecallQuery {
                query: "term".into(),
                max_sensitivity: Sensitivity::Public,
                limit
            })
            .is_err())
        }
    }
    #[test]
    fn lexical_v0_score_has_fixed_weighted_breakdown() {
        let score = lexical_v0_score("alpha beta", "alpha beta", &["alpha".into()], "beta")
            .unwrap()
            .unwrap();
        assert_eq!(score.score, 152);
        assert_eq!(score.matched_terms, vec!["alpha", "beta"]);
        assert_eq!(
            score.score_breakdown,
            RecallScoreBreakdown {
                exact_phrase: 100,
                title_terms: 40,
                topic_terms: 10,
                body_terms: 2
            }
        );
    }
    #[test]
    fn lexical_v0_score_is_case_unicode_and_duplicate_deterministic() {
        let topics = vec!["元芳".into()];
        let first = lexical_v0_score("ALPHA alpha 元芳", "alpha", &topics, "元芳")
            .unwrap()
            .unwrap();
        let second = lexical_v0_score("ALPHA alpha 元芳", "alpha", &topics, "元芳")
            .unwrap()
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.matched_terms, vec!["alpha", "元芳"]);
        assert_eq!(first.score, 32);
        assert_eq!(
            first.score_breakdown,
            RecallScoreBreakdown {
                exact_phrase: 0,
                title_terms: 20,
                topic_terms: 10,
                body_terms: 2
            }
        );
    }
    #[test]
    fn lexical_v0_score_returns_none_or_rejects_invalid_query() {
        assert_eq!(
            lexical_v0_score("missing", "title", &[], "body").unwrap(),
            None
        );
        for query in [" ".into(), "a\0b".into(), "x".repeat(4097)] {
            assert!(lexical_v0_score(&query, "", &[], "").is_err())
        }
    }
    #[test]
    fn recall_reads_verified_public_item() {
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Public);
        let (_, revision) = memex_store::read_item(root.path(), &item).unwrap();
        let report = recall(
            root.path(),
            &RecallQuery {
                query: "title".into(),
                max_sensitivity: Sensitivity::Public,
                limit: 10,
            },
        )
        .unwrap();
        assert_eq!(report.schema_version, 1);
        assert_eq!(report.retrieval_mode, "lexical_v0");
        assert_eq!(report.hit_count, 1);
        let hit = &report.hits[0];
        assert_eq!(hit.target_kind, RecallTargetKind::ItemRevision);
        assert_eq!(hit.id, item);
        assert_eq!(
            hit.revision_id.as_deref(),
            Some(revision.revision_id.as_str())
        );
        assert!(hit.score > 0);
        assert!(hit.snippet.chars().count() <= 320);
        let markdown = render_recall_markdown(&report);
        assert!(markdown.contains("max_sensitivity: public"));
        assert!(markdown.contains("kind: item_revision"));
        assert!(markdown.contains("sensitivity: public"));
        assert!(!markdown.contains("Public"));
        assert!(!markdown.contains("ItemRevision"));
        let json = serde_json::to_string(&report).unwrap();
        assert!(markdown.contains(&hit.id));
        assert!(markdown.contains(&hit.title));
        assert!(markdown.contains("lexical_v0"));
        assert!(markdown.contains(&hit.score.to_string()));
        assert!(json.contains(&hit.id));
    }
    #[test]
    fn recall_ceiling_excludes_restricted_analysis_content() {
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Public);
        let mut restricted = request(item);
        restricted.body_markdown = "body RESTRICTED_ANALYSIS_SENTINEL".into();
        restricted.requested_sensitivity = Some(Sensitivity::Restricted);
        let record = prepare_record(root.path(), &restricted).unwrap();
        apply_record(root.path(), &record).unwrap();
        let report = recall(
            root.path(),
            &RecallQuery {
                query: "body".into(),
                max_sensitivity: Sensitivity::Public,
                limit: 10,
            },
        )
        .unwrap();
        let repeated = recall(
            root.path(),
            &RecallQuery {
                query: "body".into(),
                max_sensitivity: Sensitivity::Public,
                limit: 10,
            },
        )
        .unwrap();
        assert_eq!(report, repeated);
        let json = serde_json::to_string(&report).unwrap();
        let markdown = render_recall_markdown(&report);
        assert!(!json.contains("RESTRICTED_ANALYSIS_SENTINEL"));
        assert!(!markdown.contains("RESTRICTED_ANALYSIS_SENTINEL"));
        assert!(!json.contains(&record.analysis_id));
        assert!(report
            .hits
            .iter()
            .all(|hit| hit.target_kind == RecallTargetKind::ItemRevision));
    }
    #[test]
    fn recall_ceiling_excludes_restricted_item_content() {
        let root = tempdir().unwrap();
        let item = ingest_one(root.path(), Sensitivity::Restricted);
        let report = recall(
            root.path(),
            &RecallQuery {
                query: "title".into(),
                max_sensitivity: Sensitivity::Public,
                limit: 10,
            },
        )
        .unwrap();
        let json = serde_json::to_string(&report).unwrap();
        assert!(report.hits.is_empty());
        assert_eq!(report.hit_count, 0);
        assert!(!json.contains(&item));
        assert!(!json.contains("Title"));
        assert!(!json.contains("body"));
    }
}
