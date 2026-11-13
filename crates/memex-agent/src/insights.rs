//! AI insights (`ai_insights_v1`): highlights, golden quotes and a model-knowledge fact check,
//! generated only on an explicit `insight_request`.
//!
//! Anchoring: the source is the revision's canonical `content_markdown` split by
//! `markdown_blocks::split_blocks` (blank-line paragraphs; fenced code, tables, image lines and
//! pure-URL lines are passthrough blocks). `block_index` is the index into that `Vec<Block>`,
//! i.e. the same split the bilingual reader uses to pair source and translated blocks. Every
//! item's `quote` must be an exact substring of `blocks[block_index].text`; passthrough blocks
//! are never anchored. Items that fail any check are dropped (never corrected) and counted.
//!
//! Highlights (v2) are key terms. When the revision has a validated Chinese translation (whose
//! `split_blocks` has the same block count and passthrough layout), each numbered block is sent
//! with its translated block in the same request, and an item may carry a span on either side:
//! both sides of one item always share `block_index`, each must be an exact, short, sentence-free
//! substring of its own side's block, and a rejected span is dropped (never truncated).

use crate::ipc::{Command, Response};
use crate::markdown_blocks::{self, Block};
use crate::provider_generation::{
    document_context, fits_whole, saved_route_model, ProviderRoute, TextGenerator,
    DOCUMENT_CONTEXT_BYTES,
};
use crate::queue::Queue;
use anyhow::{Context, Result};
use memex_analysis::{
    InsightKind, InsightModel, InsightModelInput, InsightOutput, InsightQueue,
    SourceClassification, TranslationModelDescriptor, TranslationModelError,
};
use memex_core::{ItemRevision, Sensitivity};
use memex_models::Purpose;
use memex_provider::generation::GenerationError;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::ops::Range;
use std::path::Path;

pub const FACT_CHECK_BASIS: &str = "model_knowledge";
/// Per-section source text when the numbered document does not fit as a whole.
const SECTION_BYTES: usize = 64 * 1024;
const MAX_COMPLETION_TOKENS: u32 = 16_000;
const MAX_HIGHLIGHTS: usize = 200;
const HIGHLIGHTS_PER_1000_WORDS: usize = 8;
const MAX_QUOTES: usize = 10;
const MAX_FACT_CHECKS: usize = 20;
const SECTION_START: &str = "<<<ANNOTATE THESE NUMBERED BLOCKS>>>";
const SECTION_END: &str = "<<<END NUMBERED BLOCKS>>>";

/// The model route of one kind: every insight kind (like minutes) uses the summary route;
/// fact check no longer borrows the chat route. The model itself comes from model settings.
pub(crate) fn purpose(kind: InsightKind) -> Purpose {
    match kind {
        InsightKind::Highlights | InsightKind::Quotes | InsightKind::FactCheck => Purpose::Summary,
    }
}

// ---- wire items ------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighlightStyle {
    Bold,
    Highlight,
    Positive,
    Negative,
}

/// Bounds of one highlight term: CJK-bearing text by characters, other text by words.
const MAX_TERM_CJK_CHARS: usize = 8;
const MAX_TERM_WORDS: usize = 4;
const MAX_TERM_CHARS: usize = 80;

/// One anchored highlight span: `start..end` are UTF-8 byte offsets into
/// `split_blocks(side)[block].text` (`block` equals the item's `block_index`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub text: String,
    pub block: usize,
    pub start: usize,
    pub end: usize,
}

/// Wire item. `block_index`/`quote`/`style`/`label` keep the v1 shape for existing readers:
/// `quote` is the original term, or the translated term when the item has no original span.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Highlight {
    pub block_index: usize,
    pub quote: String,
    pub style: HighlightStyle,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub original: Option<Span>,
    #[serde(default)]
    pub translation: Option<Span>,
}

/// One model highlight (`quote` is accepted for `original`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HighlightReply {
    pub block_index: usize,
    #[serde(default, alias = "quote")]
    pub original: Option<String>,
    #[serde(default)]
    pub translation: Option<String>,
    pub style: HighlightStyle,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenQuote {
    pub block_index: usize,
    pub quote: String,
    #[serde(default)]
    pub translation_zh: Option<String>,
    pub why: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Supported,
    Disputed,
    Incorrect,
    Unverifiable,
    Opinion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactCheck {
    pub block_index: usize,
    pub quote: String,
    pub verdict: Verdict,
    pub explanation: String,
    pub confidence: Confidence,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    items: Vec<T>,
}

fn contract(kind: InsightKind, limit: usize, bilingual: bool) -> String {
    let shape = match kind {
        InsightKind::Highlights if bilingual => {
            r#"{"items":[{"block_index":0,"original":"exact term or null","translation":"exact 译文词 or null","style":"bold|highlight|positive|negative","label":null}]}"#
        }
        InsightKind::Highlights => {
            r#"{"items":[{"block_index":0,"original":"exact term","translation":null,"style":"bold|highlight|positive|negative","label":null}]}"#
        }
        InsightKind::Quotes => {
            r#"{"items":[{"block_index":0,"quote":"exact sentence","translation_zh":null,"why":"..."}]}"#
        }
        InsightKind::FactCheck => {
            r#"{"items":[{"block_index":0,"quote":"exact claim","verdict":"supported|disputed|incorrect|unverifiable|opinion","explanation":"...","confidence":"high|medium|low"}]}"#
        }
    };
    let translated = if bilingual {
        " Each numbered block [[B<n>]] is followed by its Chinese translation [[T<n>]]; a translation term must come from [[T<n>]] of the same n."
    } else {
        ""
    };
    format!(
        "Output contract: the user message gives the source document as read-only context, then, between {SECTION_START} and {SECTION_END}, the blocks to work on, each introduced by its number as [[B<n>]].{translated} Use only those numbered blocks. Return at most {limit} items. Reply with only one JSON object of exactly this form and nothing else (no code fence): {shape}"
    )
}

// ---- validation ----------------------------------------------------------------------

fn char_count(text: &str) -> usize {
    text.chars().count()
}

fn is_cjk(character: char) -> bool {
    matches!(character as u32, 0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff)
}

/// Word units of the translatable text: whitespace-separated tokens without CJK, plus one unit
/// per two CJK characters (CJK text has no spaces).
pub(crate) fn word_units(blocks: &[Block<'_>]) -> usize {
    blocks
        .iter()
        .filter(|block| !block.passthrough)
        .map(|block| {
            let cjk = block.text.chars().filter(|c| is_cjk(*c)).count();
            let words = block
                .text
                .split_whitespace()
                .filter(|word| !word.chars().any(is_cjk))
                .count();
            words + cjk / 2
        })
        .sum()
}

pub(crate) fn highlight_cap(blocks: &[Block<'_>]) -> usize {
    (word_units(blocks) * HIGHLIGHTS_PER_1000_WORDS / 1000)
        .clamp(HIGHLIGHTS_PER_1000_WORDS, MAX_HIGHLIGHTS)
}

/// Byte range of `quote` in its block, when it is anchored verbatim in an allowed block.
fn anchor(
    blocks: &[Block<'_>],
    allowed: &Range<usize>,
    index: usize,
    quote: &str,
    max_chars: usize,
) -> Option<Range<usize>> {
    let block = blocks.get(index).filter(|_| allowed.contains(&index))?;
    if block.passthrough || quote.trim().is_empty() || char_count(quote) > max_chars {
        return None;
    }
    let start = block.text.find(quote)?;
    Some(start..start + quote.len())
}

/// A key term, never a sentence: trimmed, one line, no sentence punctuation, within bounds.
fn term_ok(term: &str) -> bool {
    let words = term.split_whitespace().count();
    let sentence = term.contains(['。', '，', '！', '？', '；', '!', '?', ';'])
        || term.contains(". ")
        // "Apple Inc." is a term; "The ministry announced a tax." is a sentence.
        || (term.ends_with('.') && words > 3);
    let short = if term.chars().any(is_cjk) {
        (2..=MAX_TERM_CJK_CHARS).contains(&char_count(term))
    } else {
        words <= MAX_TERM_WORDS && char_count(term) <= MAX_TERM_CHARS
    };
    !term.is_empty()
        && term.trim() == term
        && !term.chars().any(char::is_control)
        && !sentence
        && short
}

/// The span of `term` in block `index` of one side, outside the spans already taken there.
fn term_span(
    blocks: &[Block<'_>],
    allowed: &Range<usize>,
    index: usize,
    term: &str,
    taken: &[Span],
) -> Option<Span> {
    if !term_ok(term) {
        return None;
    }
    let range = anchor(blocks, allowed, index, term, MAX_TERM_CHARS)?;
    let overlaps = taken
        .iter()
        .any(|seen| seen.block == index && seen.start < range.end && range.start < seen.end);
    (!overlaps).then(|| Span {
        text: term.to_owned(),
        block: index,
        start: range.start,
        end: range.end,
    })
}

fn short_text(text: &str, max_chars: usize) -> bool {
    !text.trim().is_empty()
        && char_count(text) <= max_chars
        && !text.chars().any(|c| c.is_control() && c != '\n')
}

fn has_url(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("http://") || lower.contains("https://") || lower.contains("www.")
}

/// Accumulates validated items across sections; `dropped` counts every rejected model item.
#[derive(Default)]
pub(crate) struct Collected {
    highlights: Vec<Highlight>,
    quotes: Vec<GoldenQuote>,
    fact_checks: Vec<FactCheck>,
    pub dropped: u32,
}

impl Collected {
    /// `translated`: the translation's blocks (same count and layout as `blocks`), if any.
    /// Every offered span that fails is counted once; an item with no valid span is omitted.
    pub(crate) fn add_highlights(
        &mut self,
        blocks: &[Block<'_>],
        translated: Option<&[Block<'_>]>,
        allowed: &Range<usize>,
        items: Vec<HighlightReply>,
    ) {
        for item in items {
            let label_ok = item
                .label
                .as_deref()
                .is_none_or(|label| short_text(label, 40) && !label.contains('\n'));
            let offered =
                usize::from(item.original.is_some()) + usize::from(item.translation.is_some());
            if !label_ok || offered == 0 {
                self.dropped += offered.max(1) as u32;
                continue;
            }
            let taken_original: Vec<Span> = self
                .highlights
                .iter()
                .filter_map(|h| h.original.clone())
                .collect();
            let taken_translation: Vec<Span> = self
                .highlights
                .iter()
                .filter_map(|h| h.translation.clone())
                .collect();
            let original = item.original.as_deref().and_then(|term| {
                term_span(blocks, allowed, item.block_index, term, &taken_original)
            });
            // Pairing is by block: a translation term is looked up only in the same block.
            let translation = item.translation.as_deref().and_then(|term| {
                term_span(
                    translated?,
                    allowed,
                    item.block_index,
                    term,
                    &taken_translation,
                )
            });
            let kept = usize::from(original.is_some()) + usize::from(translation.is_some());
            self.dropped += (offered - kept) as u32;
            let Some(quote) = original
                .as_ref()
                .or(translation.as_ref())
                .map(|span| span.text.clone())
            else {
                continue;
            };
            self.highlights.push(Highlight {
                block_index: item.block_index,
                quote,
                style: item.style,
                label: item.label,
                original,
                translation,
            });
        }
    }

    fn add_quotes(
        &mut self,
        blocks: &[Block<'_>],
        allowed: &Range<usize>,
        items: Vec<GoldenQuote>,
        chinese_source: bool,
    ) {
        for mut item in items {
            if chinese_source {
                // A Chinese source needs no rendering; any offered one is not shown.
                item.translation_zh = None;
            }
            let translation_ok = item
                .translation_zh
                .as_deref()
                .is_none_or(|text| short_text(text, 1000) && text.chars().any(is_cjk));
            let duplicate = self
                .quotes
                .iter()
                .any(|seen| seen.block_index == item.block_index && seen.quote == item.quote);
            if anchor(blocks, allowed, item.block_index, &item.quote, 600).is_some()
                && translation_ok
                && short_text(&item.why, 60)
                && !duplicate
            {
                self.quotes.push(item);
            } else {
                self.dropped += 1;
            }
        }
    }

    fn add_fact_checks(
        &mut self,
        blocks: &[Block<'_>],
        allowed: &Range<usize>,
        items: Vec<FactCheck>,
    ) {
        for item in items {
            let duplicate = self
                .fact_checks
                .iter()
                .any(|seen| seen.block_index == item.block_index && seen.quote == item.quote);
            if anchor(blocks, allowed, item.block_index, &item.quote, 600).is_some()
                && short_text(&item.explanation, 200)
                && !has_url(&item.explanation)
                && !duplicate
            {
                self.fact_checks.push(item);
            } else {
                self.dropped += 1;
            }
        }
    }

    /// Applies the total bounds (excess items are dropped and counted) in reading order.
    pub(crate) fn finish(mut self, kind: InsightKind, blocks: &[Block<'_>]) -> InsightOutput {
        let items = match kind {
            InsightKind::Highlights => {
                self.highlights.sort_by_key(|item| {
                    let original = item.original.as_ref().map(|span| span.start);
                    let translation = item.translation.as_ref().map(|span| span.start);
                    (item.block_index, original.is_none(), original, translation)
                });
                let cap = highlight_cap(blocks);
                if self.highlights.len() > cap {
                    self.dropped += (self.highlights.len() - cap) as u32;
                    self.highlights.truncate(cap);
                }
                serde_json::to_value(self.highlights)
            }
            InsightKind::Quotes => {
                if self.quotes.len() > MAX_QUOTES {
                    self.dropped += (self.quotes.len() - MAX_QUOTES) as u32;
                    self.quotes.truncate(MAX_QUOTES);
                }
                serde_json::to_value(self.quotes)
            }
            InsightKind::FactCheck => {
                if self.fact_checks.len() > MAX_FACT_CHECKS {
                    self.dropped += (self.fact_checks.len() - MAX_FACT_CHECKS) as u32;
                    self.fact_checks.truncate(MAX_FACT_CHECKS);
                }
                serde_json::to_value(self.fact_checks)
            }
        };
        InsightOutput {
            items: items.unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
            dropped: self.dropped,
            translation_used: false,
        }
    }
}

fn parse<T: DeserializeOwned>(output: &str) -> Option<Vec<T>> {
    let trimmed = output.trim();
    let trimmed = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|rest| rest.trim_end().strip_suffix("```"))
        .unwrap_or(trimmed)
        .trim();
    serde_json::from_str::<Envelope<T>>(trimmed)
        .ok()
        .map(|envelope| envelope.items)
}

// ---- requests ------------------------------------------------------------------------

/// Numbered blocks; with `translated`, each numbered block is followed by its translation.
fn numbered(blocks: &[Block<'_>], translated: Option<&[Block<'_>]>, range: Range<usize>) -> String {
    blocks[range.clone()]
        .iter()
        .zip(range)
        .map(|(block, index)| match translated {
            _ if block.passthrough => block.text.to_owned(),
            Some(translated) => {
                format!(
                    "[[B{index}]]\n{}\n[[T{index}]]\n{}",
                    block.text, translated[index].text
                )
            }
            None => format!("[[B{index}]]\n{}", block.text),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The translation's blocks when they pair one-to-one with the source blocks.
pub(crate) fn paired_translation<'a>(
    blocks: &[Block<'_>],
    translation: Option<&'a str>,
) -> Option<Vec<Block<'a>>> {
    let translated = markdown_blocks::split_blocks(translation?);
    (translated.len() == blocks.len()
        && translated
            .iter()
            .zip(blocks)
            .all(|(translated, block)| translated.passthrough == block.passthrough))
    .then_some(translated)
}

/// Block ranges sent per request: the whole document when the numbered text fits as context,
/// otherwise consecutive sections of about `SECTION_BYTES`, each after the shared (excerpt)
/// document prefix. Deterministic for one document.
pub(crate) fn sections(
    title: &str,
    markdown: &str,
    blocks: &[Block<'_>],
    translated: Option<&[Block<'_>]>,
) -> Vec<Range<usize>> {
    let whole = numbered(blocks, translated, 0..blocks.len());
    if fits_whole(title, &whole) {
        return vec![0..blocks.len()];
    }
    let mut out = Vec::new();
    let mut start = 0;
    let mut size = 0;
    for (index, block) in blocks.iter().enumerate() {
        let bytes =
            block.text.len() + translated.map_or(0, |translated| translated[index].text.len());
        if index > start && size + bytes > SECTION_BYTES {
            out.push(start..index);
            start = index;
            size = 0;
        }
        size += bytes;
    }
    if start < blocks.len() {
        out.push(start..blocks.len());
    }
    debug_assert!(markdown.len() + title.len() > DOCUMENT_CONTEXT_BYTES || !out.is_empty());
    out
}

fn request_text(
    title: &str,
    markdown: &str,
    blocks: &[Block<'_>],
    translated: Option<&[Block<'_>]>,
    range: &Range<usize>,
) -> String {
    let whole = range.start == 0 && range.end == blocks.len();
    let numbered = numbered(blocks, translated, range.clone());
    if whole && fits_whole(title, &numbered) {
        // The numbered whole document is itself the context.
        format!("{SECTION_START}\nTitle: {title}\n\n{numbered}\n{SECTION_END}")
    } else {
        format!(
            "{}{SECTION_START}\n{numbered}\n{SECTION_END}",
            document_context(title, markdown)
        )
    }
}

pub(crate) struct ProviderInsightModel {
    descriptor: TranslationModelDescriptor,
    generator: Box<dyn TextGenerator>,
}

impl ProviderInsightModel {
    pub(crate) fn new(route: ProviderRoute) -> Self {
        Self {
            descriptor: route.descriptor,
            generator: route.generator,
        }
    }

    /// One request; a malformed reply is re-requested once within the attempt.
    fn items<T: DeserializeOwned>(
        &mut self,
        system: &str,
        input: &str,
    ) -> std::result::Result<Vec<T>, TranslationModelError> {
        for _ in 0..2 {
            let output = self
                .generator
                .generate(system, input, MAX_COMPLETION_TOKENS)
                .map_err(|error| match error {
                    GenerationError::OutcomeUnknown => TranslationModelError::OutcomeUnknown,
                    GenerationError::InvalidRequest => TranslationModelError::InputUnsupported,
                    GenerationError::CredentialScopeMismatch | GenerationError::Rejected => {
                        TranslationModelError::ProviderFailure
                    }
                    GenerationError::InvalidResponse
                    | GenerationError::Incomplete
                    | GenerationError::ModelMismatch => TranslationModelError::MalformedOutput,
                })?;
            if let Some(items) = parse(&output) {
                return Ok(items);
            }
        }
        Err(TranslationModelError::MalformedOutput)
    }
}

impl InsightModel for ProviderInsightModel {
    fn descriptor(&self) -> TranslationModelDescriptor {
        self.descriptor.clone()
    }

    fn generate(
        &mut self,
        input: InsightModelInput<'_>,
    ) -> std::result::Result<InsightOutput, TranslationModelError> {
        let kind = input.kind();
        let (title, markdown) = (input.title(), input.markdown());
        let blocks = markdown_blocks::split_blocks(markdown);
        let chinese = input.source_language() == "zh-CN";
        let translated = match kind {
            InsightKind::Highlights => paired_translation(&blocks, input.translation()),
            InsightKind::Quotes | InsightKind::FactCheck => None,
        };
        let translated = translated.as_deref();
        let limit = match kind {
            InsightKind::Highlights => highlight_cap(&blocks),
            InsightKind::Quotes => MAX_QUOTES,
            InsightKind::FactCheck => MAX_FACT_CHECKS,
        };
        let system = format!(
            "{}\n\nSource language: {}.\n\n{}",
            input.prompt(),
            input.source_language(),
            contract(kind, limit, translated.is_some())
        );
        let mut collected = Collected::default();
        for range in sections(title, markdown, &blocks, translated) {
            if blocks[range.clone()].iter().all(|block| block.passthrough) {
                continue;
            }
            let request = request_text(title, markdown, &blocks, translated, &range);
            match kind {
                InsightKind::Highlights => {
                    let items = self.items(&system, &request)?;
                    collected.add_highlights(&blocks, translated, &range, items);
                }
                InsightKind::Quotes => {
                    let items = self.items(&system, &request)?;
                    collected.add_quotes(&blocks, &range, items, chinese);
                }
                InsightKind::FactCheck => {
                    let items = self.items(&system, &request)?;
                    collected.add_fact_checks(&blocks, &range, items);
                }
            }
        }
        let mut output = collected.finish(kind, &blocks);
        output.translation_used = translated.is_some();
        Ok(output)
    }
}

// ---- IPC -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsightProjection {
    /// not_requested | queued | running | succeeded | failed | unavailable
    pub state: String,
    pub reason: Option<String>,
    pub items: Option<serde_json::Value>,
    /// Model items rejected by validation (not anchored verbatim, overlap, bounds, ...).
    pub dropped: u32,
    /// Only on `fact_check`: `model_knowledge` (no web retrieval).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    /// Only on succeeded `highlights`: `original_and_translation` | `original_only` (no
    /// usable Chinese translation existed when they were generated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sides: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiInsights {
    pub highlights: InsightProjection,
    pub quotes: InsightProjection,
    pub fact_check: InsightProjection,
}

impl AiInsights {
    /// Keeps states but removes item arrays (a `library_item_read` frame would overflow).
    pub(crate) fn without_items(mut self) -> Self {
        for view in [&mut self.highlights, &mut self.quotes, &mut self.fact_check] {
            if view.items.take().is_some() {
                view.reason = Some("insights_too_large_for_read".into());
            }
        }
        self
    }
}

pub(crate) fn projection(library: &Path, revision: &ItemRevision) -> Result<AiInsights> {
    let view = |kind| -> Result<InsightProjection> {
        let view = memex_analysis::insight_view(library, revision, kind)?;
        Ok(InsightProjection {
            state: view.state,
            reason: view.reason,
            items: view.items,
            dropped: view.dropped,
            basis: (kind == InsightKind::FactCheck).then(|| FACT_CHECK_BASIS.into()),
            sides: view.sides,
        })
    };
    Ok(AiInsights {
        highlights: view(InsightKind::Highlights)?,
        quotes: view(InsightKind::Quotes)?,
        fact_check: view(InsightKind::FactCheck)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsightsCapability {
    pub highlights: bool,
    pub quotes: bool,
    pub fact_check: bool,
    pub fact_check_basis: String,
}

pub(crate) fn capability(routes: &crate::provider_generation::PurposeRoutes) -> InsightsCapability {
    InsightsCapability {
        highlights: routes.summary.is_some(),
        quotes: routes.summary.is_some(),
        fact_check: routes.summary.is_some(),
        fact_check_basis: FACT_CHECK_BASIS.into(),
    }
}

pub(crate) fn dispatch(
    q: &mut Queue,
    library: &Path,
    c: Command,
) -> Result<std::result::Result<Response, Command>> {
    let Command::InsightRequest {
        item_id,
        max_sensitivity,
        kinds,
    } = c
    else {
        return Ok(Err(c));
    };
    let snapshot = q.model_settings().ok();
    let has_route = |purpose| {
        snapshot
            .as_ref()
            .and_then(|snapshot| saved_route_model(q.database_path(), snapshot, purpose))
            .is_some()
    };
    let routes = [(Purpose::Summary, has_route(Purpose::Summary))];
    let mut response = Response::empty_ok();
    response.ai_insights = Some(request(
        library,
        &item_id,
        max_sensitivity,
        &kinds,
        |purpose| routes.iter().any(|(p, saved)| *p == purpose && *saved),
    )?);
    Ok(Ok(response))
}

/// Validates the kinds, checks routes (all requested kinds must be routable, or nothing is
/// enqueued), enqueues/re-queues and returns the full projection.
pub(crate) fn request(
    library: &Path,
    item_id: &str,
    max_sensitivity: Sensitivity,
    kinds: &[String],
    routed: impl Fn(Purpose) -> bool,
) -> Result<AiInsights> {
    let mut parsed: Vec<InsightKind> = Vec::new();
    for kind in kinds {
        let kind = InsightKind::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == kind)
            .context("invalid_insight_request")?;
        if parsed.contains(&kind) {
            anyhow::bail!("invalid_insight_request")
        }
        parsed.push(kind);
    }
    if parsed.is_empty() || parsed.len() > 3 {
        anyhow::bail!("invalid_insight_request")
    }
    let (_, revision) = memex_store::read_item_at_sensitivity(library, item_id, max_sensitivity)?
        .filter(|(item, revision)| item.sensitivity.max(revision.sensitivity) <= max_sensitivity)
        .context("library_resource_not_found")?;
    if parsed.iter().any(|kind| !routed(purpose(*kind))) {
        anyhow::bail!("no_insight_provider")
    }
    let mut insights = projection(library, &revision)?;
    let applicable = matches!(
        memex_analysis::classify_source(&revision.title, &revision.content_markdown),
        SourceClassification::Chinese | SourceClassification::English | SourceClassification::Other
    ) && markdown_blocks::split_blocks(&revision.content_markdown)
        .iter()
        .any(|block| !block.passthrough);
    if !applicable {
        for kind in &parsed {
            let view = match kind {
                InsightKind::Highlights => &mut insights.highlights,
                InsightKind::Quotes => &mut insights.quotes,
                InsightKind::FactCheck => &mut insights.fact_check,
            };
            if view.state == "not_requested" {
                view.reason = Some("insights_not_applicable".into());
            }
        }
        return Ok(insights);
    }
    let mut queue = InsightQueue::open(library)?;
    for kind in parsed {
        queue.request(&revision, kind)?;
    }
    projection(library, &revision)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# Title line\n\nThe company reported revenue of 12 billion dollars in 2025. Critics say the growth is fragile.\n\n```\ncode block\n```\n\nThe new policy helps small farmers. It also raises costs for consumers.";

    fn blocks() -> Vec<Block<'static>> {
        markdown_blocks::split_blocks(DOC)
    }

    #[test]
    fn every_insight_kind_including_fact_check_uses_the_summary_route() {
        for kind in InsightKind::ALL {
            assert_eq!(purpose(kind), Purpose::Summary, "{}", kind.as_str());
        }
        let only_summary = crate::provider_generation::PurposeRoutes {
            translation: None,
            summary: Some("smart".into()),
            classification: None,
            chat: None,
        };
        let available = capability(&only_summary);
        assert!(available.highlights && available.quotes && available.fact_check);
        let only_chat = crate::provider_generation::PurposeRoutes {
            summary: None,
            chat: Some("chat".into()),
            ..only_summary
        };
        assert!(!capability(&only_chat).fact_check);
    }

    #[test]
    fn exact_substring_overlap_range_and_passthrough_rules_drop_items() {
        let blocks = blocks();
        assert_eq!(blocks.len(), 4);
        assert!(blocks[2].passthrough);
        let mut collected = Collected::default();
        let item = |block_index, quote: &str, style| HighlightReply {
            block_index,
            original: Some(quote.into()),
            translation: None,
            style,
            label: None,
        };
        collected.add_highlights(
            &blocks,
            None,
            &(0..blocks.len()),
            vec![
                item(1, "12 billion dollars", HighlightStyle::Highlight),
                // Overlaps the first one in the same block → dropped.
                item(1, "12 billion", HighlightStyle::Bold),
                // Fabricated (not verbatim) → dropped, never corrected.
                item(1, "revenue of twelve billion dollars", HighlightStyle::Bold),
                // Out of range and passthrough → dropped.
                item(9, "anything", HighlightStyle::Bold),
                item(2, "code block", HighlightStyle::Bold),
                item(3, "helps small farmers", HighlightStyle::Positive),
                item(3, "raises costs for consumers", HighlightStyle::Negative),
                HighlightReply {
                    label: Some("这是一个远远超过四十个汉字长度上限的标注说明文字，因此整条标注都应当被丢弃而不是被截断处理掉才对".into()),
                    ..item(1, "Critics say", HighlightStyle::Negative)
                },
            ],
        );
        let output = collected.finish(InsightKind::Highlights, &blocks);
        let items: Vec<Highlight> = serde_json::from_value(output.items).unwrap();
        assert_eq!(output.dropped, 5);
        assert_eq!(
            items
                .iter()
                .map(|item| item.quote.as_str())
                .collect::<Vec<_>>(),
            [
                "12 billion dollars",
                "helps small farmers",
                "raises costs for consumers"
            ]
        );
        for item in &items {
            assert!(blocks[item.block_index].text.contains(&item.quote));
            let span = item.original.as_ref().unwrap();
            assert_eq!(&blocks[span.block].text[span.start..span.end], item.quote);
            assert!(item.translation.is_none());
        }
    }

    #[test]
    fn totals_are_bounded_and_chinese_quotes_carry_no_translation() {
        let blocks = blocks();
        // A few dozen word units → the minimum cap of 8 highlights.
        assert_eq!(highlight_cap(&blocks), 8);
        let mut collected = Collected::default();
        let words: Vec<&str> = blocks[1].text.split(' ').collect();
        collected.add_highlights(
            &blocks,
            None,
            &(0..blocks.len()),
            words
                .iter()
                .filter(|word| blocks[1].text.matches(**word).count() == 1)
                .map(|word| HighlightReply {
                    block_index: 1,
                    original: Some((*word).into()),
                    translation: None,
                    style: HighlightStyle::Highlight,
                    label: None,
                })
                .collect(),
        );
        let output = collected.finish(InsightKind::Highlights, &blocks);
        assert_eq!(output.items.as_array().unwrap().len(), 8);
        assert!(output.dropped > 0);

        let chinese = "这是一篇中文文章。作者认为，读书的意义在于改变自己。\n\n第二段继续讨论。";
        let blocks = markdown_blocks::split_blocks(chinese);
        let mut collected = Collected::default();
        collected.add_quotes(
            &blocks,
            &(0..blocks.len()),
            (0..12)
                .map(|_| GoldenQuote {
                    block_index: 0,
                    quote: "作者认为，读书的意义在于改变自己。".into(),
                    translation_zh: Some("无需译文".into()),
                    why: "核心观点".into(),
                })
                .collect(),
            true,
        );
        let output = collected.finish(InsightKind::Quotes, &blocks);
        let items: Vec<GoldenQuote> = serde_json::from_value(output.items).unwrap();
        assert_eq!(items.len(), 1, "duplicates dropped");
        assert_eq!(output.dropped, 11);
        assert!(items[0].translation_zh.is_none());
    }

    #[test]
    fn long_documents_are_sectioned_after_the_shared_document_prefix() {
        let paragraph = |index: usize| format!("Paragraph {index}: {}", "word ".repeat(1000));
        let long = (0..40).map(paragraph).collect::<Vec<_>>().join("\n\n");
        let blocks = markdown_blocks::split_blocks(&long);
        let parts = sections("Long", &long, &blocks, None);
        assert!(parts.len() > 1);
        assert_eq!(parts.first().unwrap().start, 0);
        assert_eq!(parts.last().unwrap().end, blocks.len());
        assert!(parts.windows(2).all(|pair| pair[0].end == pair[1].start));
        let prefix = document_context("Long", &long);
        for range in &parts {
            let text = request_text("Long", &long, &blocks, None, range);
            assert!(text.starts_with(&prefix));
            assert!(text.len() <= memex_provider::generation::MAX_INPUT_BYTES);
        }
        let short = markdown_blocks::split_blocks(DOC);
        assert_eq!(sections("Title", DOC, &short, None), vec![0..short.len()]);
        assert!(request_text("Title", DOC, &short, None, &(0..short.len())).contains("[[B3]]"));
    }

    // Five blocks; blocks 3 and 4 share vocabulary so block pairing is observable.
    const PAIRED: &str = "# Farm policy\n\nThe ministry announced a carbon tax on fertilizer.\n\n```\ncode block\n```\n\nThe carbon tax helps small farmers.\n\nCritics expect higher food prices.";
    const PAIRED_ZH: &str = "# 农业政策\n\n该部宣布对化肥征收碳税。\n\n```\ncode block\n```\n\n碳税有助于小农户。\n\n批评者预计食品价格上涨，并称碳税会伤害小农户。";

    fn reply(
        block_index: usize,
        original: Option<&str>,
        translation: Option<&str>,
    ) -> HighlightReply {
        HighlightReply {
            block_index,
            original: original.map(Into::into),
            translation: translation.map(Into::into),
            style: HighlightStyle::Highlight,
            label: None,
        }
    }

    fn collect(items: Vec<HighlightReply>) -> (Vec<Highlight>, u32) {
        let blocks = markdown_blocks::split_blocks(PAIRED);
        let translated = paired_translation(&blocks, Some(PAIRED_ZH)).expect("paired layout");
        let mut collected = Collected::default();
        collected.add_highlights(&blocks, Some(&translated), &(0..blocks.len()), items);
        let output = collected.finish(InsightKind::Highlights, &blocks);
        (
            serde_json::from_value(output.items).unwrap(),
            output.dropped,
        )
    }

    #[test]
    fn sentence_length_highlights_are_rejected_and_short_terms_kept() {
        let (items, dropped) = collect(vec![
            // A whole sentence (verbatim, but 8+ words / sentence punctuation) → dropped.
            reply(
                1,
                Some("The ministry announced a carbon tax on fertilizer."),
                None,
            ),
            reply(1, None, Some("该部宣布对化肥征收碳税。")),
            // A long Chinese clause with a comma, verbatim in the translation → dropped.
            reply(
                4,
                None,
                Some("批评者预计食品价格上涨，并称碳税会伤害小农户"),
            ),
            // Key terms → kept.
            reply(1, Some("carbon tax"), Some("碳税")),
            reply(4, Some("higher food prices"), Some("食品价格上涨")),
        ]);
        assert_eq!(dropped, 3);
        let pairs: Vec<_> = items
            .iter()
            .map(|item| {
                (
                    item.original.as_ref().map(|span| span.text.as_str()),
                    item.translation.as_ref().map(|span| span.text.as_str()),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            [
                (Some("carbon tax"), Some("碳税")),
                (Some("higher food prices"), Some("食品价格上涨"))
            ]
        );
        assert!(term_ok("one"));
        assert!(term_ok("one two three four"));
        assert!(!term_ok("one two three four five"));
        assert!(!term_ok("碳"));
        assert!(term_ok("碳税"));
        assert!(term_ok("一二三四五六七八"));
        assert!(!term_ok("一二三四五六七八九"));
    }

    #[test]
    fn a_chinese_term_missing_from_the_translation_is_dropped_but_its_original_kept() {
        let (items, dropped) = collect(vec![
            // "化肥税" is not a substring of the translated block (it says "对化肥征收碳税").
            reply(1, Some("fertilizer"), Some("化肥税")),
            // Neither side verbatim → the item disappears, both spans counted.
            reply(1, Some("fertiliser"), Some("肥料")),
        ]);
        assert_eq!(dropped, 3);
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.quote, "fertilizer");
        assert!(item.translation.is_none());
        assert_eq!(item.original.as_ref().unwrap().text, "fertilizer");
    }

    #[test]
    fn pairing_is_by_block_so_a_block_3_term_never_maps_into_block_4() {
        let blocks = markdown_blocks::split_blocks(PAIRED);
        let translated = markdown_blocks::split_blocks(PAIRED_ZH);
        assert!(!translated[3].text.contains("批评者") && translated[4].text.contains("批评者"));
        let (items, dropped) = collect(vec![
            // "批评者" exists only in translated block 4: offered for block 3 → dropped.
            reply(3, Some("small farmers"), Some("批评者")),
            // The same term for block 4 is anchored in block 4.
            reply(4, Some("Critics"), Some("批评者")),
        ]);
        assert_eq!(dropped, 1);
        assert_eq!(items[0].block_index, 3);
        assert!(items[0].translation.is_none());
        let span = items[1].translation.as_ref().unwrap();
        assert_eq!((items[1].block_index, span.block), (4, 4));
        assert_eq!(&translated[4].text[span.start..span.end], "批评者");
        let original = items[1].original.as_ref().unwrap();
        assert_eq!(&blocks[4].text[original.start..original.end], "Critics");
        // A translation-only item keeps the legacy `quote` on the translated term.
        let (items, _) = collect(vec![reply(3, None, Some("小农户"))]);
        assert_eq!(items[0].quote, "小农户");
        assert!(items[0].original.is_none());
        // Without a translation, every translation span is dropped.
        let mut collected = Collected::default();
        collected.add_highlights(
            &blocks,
            None,
            &(0..blocks.len()),
            vec![reply(4, Some("Critics"), Some("批评者"))],
        );
        let output = collected.finish(InsightKind::Highlights, &blocks);
        assert_eq!(output.dropped, 1);
        assert!(output.items[0]["translation"].is_null());
        // A translation whose blocks do not pair one-to-one is not used at all.
        assert!(paired_translation(&blocks, Some("只有一段译文。")).is_none());
        let paired = paired_translation(&blocks, Some(PAIRED_ZH)).unwrap();
        let text = request_text(
            "Farm policy",
            PAIRED,
            &blocks,
            Some(&paired),
            &(0..blocks.len()),
        );
        assert!(text
            .contains("[[B3]]\nThe carbon tax helps small farmers.\n[[T3]]\n碳税有助于小农户。"));
        assert!(
            !text.contains("[[T2]]"),
            "passthrough blocks are not numbered"
        );
    }
}
