//! Transport-independent admission, translation, and Store orchestration.

mod comments;
pub use comments::{Comments, CommentsState, MAX_COMMENTS_BYTES};

use anyhow::{bail, Result};
use html5ever::{
    buffer_queue::BufferQueue,
    tendril::StrTendril,
    tokenizer::{
        states::{Rawtext, Rcdata, ScriptData},
        EndTag, StartTag, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
    },
    LocalName,
};
use memex_core::{DraftAttachmentRef, IngestReceipt, ItemDraft, ItemMetadata, VerifiedCapture};
use lots_scout_extract::{
    AmpPodcastExtractor, LinkresearcherArticleExtractor, WeixinArticleExtractor,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{cell::RefCell, path::Path};

pub const ADMISSION_POLICY_VERSION: &str = "capture-admission-v4";
const MAX_HTML_BYTES: usize = 8 * 1024 * 1024;
const MAX_HTML_ELEMENTS: usize = 100_000;
const MAX_HTML_DEPTH: usize = 256;
const MAX_TAG_ATTRIBUTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionClass {
    Article,
    NeedsPreview,
    InteractiveLoginRequired,
    ChallengeDetected,
    UnsupportedContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionDecision {
    pub class: AdmissionClass,
    pub reason_code: &'static str,
    pub artifact_sha256: String,
    pub policy_version: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewApproval {
    pub artifact_sha256: String,
    pub policy_version: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionContext {
    pub http_status: Option<u16>,
    pub preview_approval: Option<PreviewApproval>,
}

pub struct CaptureSubmission {
    pub capture: VerifiedCapture,
    pub metadata: ItemMetadata,
    pub attachments: Vec<DraftAttachmentRef>,
}

#[derive(Debug)]
pub struct RejectedAdmission(pub AdmissionDecision);

impl std::fmt::Display for RejectedAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "capture admission rejected: {}", self.0.reason_code)
    }
}
impl std::error::Error for RejectedAdmission {}

pub struct PreparedCapture {
    capture: VerifiedCapture,
    draft: ItemDraft,
    decision: AdmissionDecision,
}

impl PreparedCapture {
    pub fn artifact_bytes(&self) -> &[u8] {
        &self.capture.blobs[0].bytes
    }
    pub fn artifact_sha256(&self) -> &str {
        &self.decision.artifact_sha256
    }
    pub fn admission(&self) -> &AdmissionDecision {
        &self.decision
    }
    /// Blobs backed by the capture spool rather than Store-local image attachments.
    pub fn source_blobs(&self) -> impl Iterator<Item = &memex_core::VerifiedBlob> {
        self.capture
            .blobs
            .iter()
            .filter(|blob| blob.source_backend_ref.is_some())
    }
}

pub fn prepare_capture(
    submission: CaptureSubmission,
    context: AdmissionContext,
) -> Result<PreparedCapture> {
    let CaptureSubmission {
        capture,
        metadata,
        attachments,
    } = submission;
    let decision = decide(&capture, context.http_status);
    let admitted = decision.class == AdmissionClass::Article
        || (decision.class == AdmissionClass::NeedsPreview
            && context.preview_approval.as_ref().is_some_and(|approval| {
                approval.artifact_sha256 == decision.artifact_sha256
                    && approval.policy_version == decision.policy_version
            }));
    if !admitted {
        return Err(RejectedAdmission(decision).into());
    }

    let mut draft: ItemDraft = lots_scout_extract::extract(&capture)?.into();
    draft.metadata = metadata;
    draft.attachments.extend(attachments);
    draft.attachments.sort();
    draft.attachments.dedup();
    if draft.attachments.len() > 128 {
        bail!("too many attachments")
    }
    // Readability drops a lone embedded-player link, so the reader and AI would never see the
    // video. Put each YouTube watch link at the top of the body unless it is already there.
    let videos = draft
        .attachments
        .iter()
        .filter_map(|attachment| match attachment {
            DraftAttachmentRef::Remote { url, .. }
                if url.starts_with("https://www.youtube.com/watch?v=")
                    && !draft.content_markdown.contains(url.as_str()) =>
            {
                Some(format!("[YouTube: {url}]({url})\n\n"))
            }
            _ => None,
        })
        .collect::<String>();
    if !videos.is_empty() {
        draft.content_markdown = format!("{videos}{}", draft.content_markdown);
    }
    Ok(PreparedCapture {
        capture,
        draft,
        decision,
    })
}

pub fn commit_capture(library: &Path, prepared: PreparedCapture) -> Result<IngestReceipt> {
    memex_store::ingest(library, &prepared.capture, prepared.draft)
}

/// Explicit webpage collection preserves existing memberships and refuses changed bodies.
/// Admission and translation are identical for the loopback transport and offline CLI.
pub fn commit_web_capture(
    library: &Path,
    prepared: PreparedCapture,
    target_group_id: Option<&str>,
) -> Result<memex_store::WebCaptureReceipt> {
    memex_store::ingest_web_capture(library, &prepared.capture, prepared.draft, target_group_id)
}

/// Optional quality checking never bypasses admission and never changes raw Evidence.
/// Failed Analysis persistence does not undo or misreport an already durable Store commit.
pub fn commit_capture_checked(
    library: &Path,
    mut prepared: PreparedCapture,
    configured: bool,
    model: Option<&dyn memex_analysis::capture_quality::CaptureQualityModel>,
) -> Result<(IngestReceipt, serde_json::Value)> {
    use memex_analysis::capture_quality::*;
    if configured {
        prepared.capture.capture.sensitivity = prepared
            .capture
            .capture
            .sensitivity
            .max(memex_core::Sensitivity::Internal);
        prepared.draft.sensitivity = prepared
            .draft
            .sensitivity
            .max(memex_core::Sensitivity::Internal);
    }
    let digest = |bytes: &[u8]| format!("sha256:{:x}", Sha256::digest(bytes));
    let adapter = prepared.capture.capture.adapter_id.clone();
    let external_id = prepared.capture.capture.external_capture_id.clone();
    let key = capture_quality_key(&adapter, &external_id);
    let previously_committed =
        memex_store::lookup_ingest_receipt(library, &adapter, &external_id, None)?.is_some();
    if previously_committed && !configured {
        // Validate legacy replay before binding its previously absent options.
        // A rejected payload must not poison the successful capture's identity.
        let replay = memex_store::ingest(library, &prepared.capture, prepared.draft.clone())?;
        if let Ok(Some(result)) = materialize_capture_quality(library, &adapter, &external_id) {
            if result["reason"] == "capture_identity_conflict" {
                // The canonical unchecked winner is quality=off. Preserve the
                // rejected reservation as evidence, but do not let it own replay.
                return Ok((
                    replay,
                    serde_json::json!({"state":"not_requested","reason":null,"analysis_id":null}),
                ));
            }
        }
    }
    let input = digest(&serde_json::to_vec(&(
        &prepared.capture.source,
        &prepared.capture.capture,
        prepared
            .capture
            .blobs
            .iter()
            .map(|blob| (&blob.descriptor, &blob.blob_id, &blob.source_backend_ref))
            .collect::<Vec<_>>(),
        &prepared.draft,
        configured,
    ))?);
    let pre = digest(&serde_json::to_vec(&prepared.draft)?);
    let frozen = freeze_capture_quality(
        library,
        CaptureQualityRequest {
            capture_key: &key,
            input_sha256: &input,
            source_sha256: prepared.artifact_sha256(),
            draft_sha256: &pre,
            translator_id: &prepared.draft.translator_id,
            translator_version: &prepared.draft.translator_version,
            created_at: &prepared.capture.capture.captured_at,
            source_markdown: &prepared.draft.content_markdown,
            configured,
            previously_committed,
        },
        model,
    )?;
    if let Some(markdown) = frozen.corrected_markdown() {
        prepared.draft.content_markdown = markdown.into();
    }
    let post = digest(&serde_json::to_vec(&prepared.draft)?);
    if configured {
        let store_input = memex_store::ingest_input_sha256(&prepared.capture, &prepared.draft)?;
        prepare_quality_materialization(library, &adapter, &external_id, &post, &store_input)?;
    }
    let receipt = commit_capture(library, prepared)?;
    if !configured {
        return Ok((receipt, frozen.receipt()));
    }
    let quality = materialize_capture_quality(library, &adapter, &external_id)
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            serde_json::json!({
                "state":"pending","reason":"analysis_persistence_pending","analysis_id":null
            })
        });
    Ok((receipt, quality))
}

/// Read-only admission. It performs no translation, staging, or Store writes.
pub fn admission_decision(capture: &VerifiedCapture, status: Option<u16>) -> AdmissionDecision {
    let digest = capture
        .blobs
        .first()
        .map(|blob| format!("{:x}", Sha256::digest(&blob.bytes)))
        .unwrap_or_default();
    let (class, reason_code) = if matches!(status, Some(401 | 403 | 407)) {
        (
            AdmissionClass::InteractiveLoginRequired,
            "http_authentication_required",
        )
    } else if capture.blobs.first().is_none_or(|blob| {
        blob.descriptor.sha256 != digest
            || blob.descriptor.byte_size != blob.bytes.len() as u64
            || blob.blob_id != format!("sha256:{digest}")
    }) {
        (
            AdmissionClass::UnsupportedContent,
            "primary_artifact_integrity_invalid",
        )
    } else if LinkresearcherArticleExtractor::supports(capture) {
        match LinkresearcherArticleExtractor::article_html(capture) {
            Ok(html) => classify_html(capture, &html),
            Err(_) => (
                AdmissionClass::UnsupportedContent,
                "invalid_source_article_response",
            ),
        }
    } else if capture.blobs.first().is_none_or(|b| {
        !matches!(
            b.descriptor.media_type.as_str(),
            "text/html" | "application/xhtml+xml"
        )
    }) {
        (
            AdmissionClass::UnsupportedContent,
            "primary_artifact_not_html",
        )
    } else if capture.blobs[0].bytes.len() > MAX_HTML_BYTES {
        (
            AdmissionClass::UnsupportedContent,
            "primary_artifact_too_large",
        )
    } else if std::str::from_utf8(&capture.blobs[0].bytes).is_err() {
        (
            AdmissionClass::UnsupportedContent,
            "primary_artifact_not_utf8",
        )
    } else if WeixinArticleExtractor::supports_url(&capture.capture.source_url) {
        // The article shell carries hidden QR dialogs and buttons outside the
        // body. Page-level credential and challenge signals are still checked
        // on the whole document; only the article region is classified.
        let html = std::str::from_utf8(&capture.blobs[0].bytes).expect("validated UTF-8");
        let raw = inspect_html(html);
        if raw.limit_exceeded {
            (AdmissionClass::UnsupportedContent, "html_structure_limit_exceeded")
        } else if raw.credential_control || raw.content_editable {
            (AdmissionClass::InteractiveLoginRequired, "interactive_authentication_detected")
        } else if raw.visible_markers.challenge || WeixinArticleExtractor::is_verification_page(html) {
            (AdmissionClass::ChallengeDetected, "automated_access_challenge_detected")
        } else {
            match WeixinArticleExtractor::article_html(capture) {
                Ok(article) => classify_html(capture, &article),
                Err(_) => (AdmissionClass::UnsupportedContent, "weixin_article_body_missing"),
            }
        }
    } else if AmpPodcastExtractor::supports(capture) {
        // Cropping navigation must never crop page-level credential/challenge
        // signals. Reuse the full inspector, including textarea/OTP and all
        // contenteditable states, before inspecting the complete main region.
        let raw = inspect_html(std::str::from_utf8(&capture.blobs[0].bytes).expect("validated UTF-8"));
        if raw.limit_exceeded {
            (AdmissionClass::UnsupportedContent, "html_structure_limit_exceeded")
        } else if raw.credential_control || raw.content_editable {
            (AdmissionClass::InteractiveLoginRequired, "interactive_authentication_detected")
        } else if raw.visible_markers.challenge {
            (AdmissionClass::ChallengeDetected, "automated_access_challenge_detected")
        } else { match AmpPodcastExtractor::article_html(capture) {
            Ok(html) => classify_html(capture, &html),
            Err(_) => (AdmissionClass::UnsupportedContent, "invalid_podcast_article"),
        } }
    } else {
        classify_html(
            capture,
            std::str::from_utf8(&capture.blobs[0].bytes).expect("validated UTF-8"),
        )
    };
    AdmissionDecision {
        class,
        reason_code,
        artifact_sha256: digest,
        policy_version: if WeixinArticleExtractor::supports_url(&capture.capture.source_url) {
            "capture-admission-v4-weixin-v2"
        } else { ADMISSION_POLICY_VERSION },
    }
}

fn decide(capture: &VerifiedCapture, status: Option<u16>) -> AdmissionDecision {
    admission_decision(capture, status)
}

const LONG_FORM_TEXT_CHARS: usize = 3_000;

fn classify_html(capture: &VerifiedCapture, html: &str) -> (AdmissionClass, &'static str) {
    // Interactive checks intentionally precede generic challenge checks (QR pages often say verify).
    let inspection = inspect_html(html);
    if inspection.limit_exceeded {
        return (
            AdmissionClass::UnsupportedContent,
            "html_structure_limit_exceeded",
        );
    }
    let markers = &inspection.visible_markers;
    // Login walls are short. Words like "log in" inside a long article body (a transcript, a
    // how-to) are content, not a wall; credential controls still block regardless of length.
    let long_form_article =
        inspection.article_has_text && inspection.visible_text_chars >= LONG_FORM_TEXT_CHARS;
    let strong_interactive = inspection.credential_control
        || (!long_form_article && (markers.login || markers.qr || markers.jike));
    if strong_interactive {
        return (
            AdmissionClass::InteractiveLoginRequired,
            "interactive_authentication_detected",
        );
    }
    if markers.challenge || (WeixinArticleExtractor::supports_url(&capture.capture.source_url)
        && WeixinArticleExtractor::is_verification_page(html)) {
        return (
            AdmissionClass::ChallengeDetected,
            "automated_access_challenge_detected",
        );
    }
    if inspection.prohibited_snapshot_element || inspection.content_editable {
        return (
            AdmissionClass::InteractiveLoginRequired,
            "interactive_control_detected",
        );
    }
    if !inspection.visible_has_text {
        return (
            AdmissionClass::UnsupportedContent,
            "html_has_no_visible_content",
        );
    }
    let weixin_body = WeixinArticleExtractor::supports_url(&capture.capture.source_url)
        && inspection.weixin_has_text;
    let article = inspection.article_has_text;
    if weixin_body {
        (AdmissionClass::Article, "supported_weixin_article_body")
    } else if WeixinArticleExtractor::supports_url(&capture.capture.source_url) {
        (AdmissionClass::UnsupportedContent, "weixin_article_body_missing")
    } else if article {
        (AdmissionClass::Article, "semantic_article_container")
    } else {
        (
            AdmissionClass::NeedsPreview,
            "generic_html_requires_preview",
        )
    }
}

#[derive(Default)]
struct MarkerState {
    tail: String,
    login: bool,
    qr: bool,
    jike: bool,
    challenge: bool,
    storage: bool,
}

impl MarkerState {
    fn observe_text(&mut self, value: &str) {
        let lower = value.to_lowercase();
        let mut combined = String::with_capacity(self.tail.len() + lower.len());
        combined.push_str(&self.tail);
        combined.push_str(&lower);
        self.observe_lower(&combined);
        let mut tail = combined.chars().rev().take(32).collect::<Vec<_>>();
        tail.reverse();
        self.tail = tail.into_iter().collect();
    }

    fn observe_value(&mut self, value: &str) {
        self.observe_lower(&value.to_lowercase());
    }

    fn observe_lower(&mut self, value: &str) {
        self.login |= ["login", "sign in", "log in", "登录", "扫码"]
            .iter()
            .any(|marker| value.contains(marker));
        self.qr |= ["qrcode", "二维码"]
            .iter()
            .any(|marker| value.contains(marker))
            || value
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|part| part == "qr");
        self.jike |= ["即刻app", "jike app"]
            .iter()
            .any(|marker| value.contains(marker));
        self.challenge |= [
            "captcha",
            "cf-challenge",
            "challenge-form",
            "verify you are human",
            "人机验证",
        ]
        .iter()
        .any(|marker| value.contains(marker));
        self.storage |= ["cookie", "localstorage", "sessionstorage"]
            .iter()
            .any(|marker| value.contains(marker));
    }
}

struct HtmlFrame {
    name: LocalName,
    article: bool,
    weixin: bool,
    suppressed: bool,
}

#[derive(Default)]
struct HtmlInspection {
    elements: usize,
    stack: Vec<HtmlFrame>,
    article_depth: usize,
    weixin_depth: usize,
    suppressed_depth: usize,
    visible_has_text: bool,
    visible_text_chars: usize,
    article_has_text: bool,
    weixin_has_text: bool,
    credential_control: bool,
    prohibited_snapshot_element: bool,
    content_editable: bool,
    active_content: bool,
    limit_exceeded: bool,
    visible_markers: MarkerState,
    all_markers: MarkerState,
}

#[derive(Default)]
struct HtmlInspector(RefCell<HtmlInspection>);

fn void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn form_associated_element(name: &str) -> bool {
    matches!(
        name,
        "button"
            | "datalist"
            | "fieldset"
            | "form"
            | "input"
            | "label"
            | "object"
            | "option"
            | "output"
            | "select"
            | "textarea"
    )
}

fn credential_attribute(name: &str, value: &str) -> bool {
    let value = value.trim().to_lowercase();
    match name {
        "type" => value == "password",
        "autocomplete" => value
            .split_ascii_whitespace()
            .any(|token| matches!(token, "current-password" | "new-password" | "one-time-code")),
        "name" | "id" => {
            value.contains("password")
                || value.contains("otp")
                || value.contains("one-time")
                || value.contains("one_time")
                || value.contains("verification-code")
                || value.contains("verification_code")
        }
        _ => false,
    }
}

impl TokenSink for HtmlInspector {
    type Handle = ();

    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<Self::Handle> {
        match token {
            Token::TagToken(tag) => {
                let name = tag.name.as_ref();
                let raw_result = if tag.kind == StartTag {
                    match name {
                        "script" => TokenSinkResult::RawData(ScriptData),
                        "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => {
                            TokenSinkResult::RawData(Rawtext)
                        }
                        "title" | "textarea" => TokenSinkResult::RawData(Rcdata),
                        "plaintext" => TokenSinkResult::Plaintext,
                        _ => TokenSinkResult::Continue,
                    }
                } else {
                    TokenSinkResult::Continue
                };
                let mut state = self.0.borrow_mut();
                match tag.kind {
                    StartTag => {
                        state.elements = state.elements.saturating_add(1);
                        state.limit_exceeded |= state.elements > MAX_HTML_ELEMENTS
                            || tag.attrs.len() > MAX_TAG_ATTRIBUTES
                            || tag.had_duplicate_attributes;
                        let is_control = form_associated_element(name);
                        state.prohibited_snapshot_element |= is_control;
                        state.active_content |= matches!(name, "embed" | "iframe" | "script");
                        let mut weixin = false;
                        for attribute in &tag.attrs {
                            let attribute_name = attribute.name.local.as_ref();
                            let attribute_value = attribute.value.as_ref();
                            // Metadata/script attributes are not an interactive
                            // page surface (e.g. an article abstract mentions QR).
                            if state.suppressed_depth == 0
                                && !matches!(name, "meta" | "link" | "script" | "style")
                            {
                                state.visible_markers.observe_value(attribute_value);
                            }
                            state.all_markers.observe_value(attribute_value);
                            state.content_editable |= attribute_name == "contenteditable"
                                && !attribute_value.eq_ignore_ascii_case("false");
                            state.active_content |=
                                attribute_name.starts_with("on") && attribute_name.len() > 2;
                            if is_control && credential_attribute(attribute_name, attribute_value) {
                                state.credential_control = true;
                            }
                            weixin |= attribute_name == "id" && attribute_value == "js_content";
                        }
                        if !void_element(name) {
                            if state.stack.len() >= MAX_HTML_DEPTH {
                                state.limit_exceeded = true;
                            } else {
                                let frame = HtmlFrame {
                                    name: tag.name.clone(),
                                    article: name == "article",
                                    weixin,
                                    suppressed: matches!(
                                        name,
                                        "iframe"
                                            | "noembed"
                                            | "noframes"
                                            | "noscript"
                                            | "plaintext"
                                            | "script"
                                            | "style"
                                            | "template"
                                            | "textarea"
                                            | "title"
                                            | "xmp"
                                    ),
                                };
                                state.article_depth += usize::from(frame.article);
                                state.weixin_depth += usize::from(frame.weixin);
                                state.suppressed_depth += usize::from(frame.suppressed);
                                state.stack.push(frame);
                            }
                        }
                    }
                    EndTag => {
                        if let Some(index) =
                            state.stack.iter().rposition(|frame| frame.name == tag.name)
                        {
                            let removed = state.stack.drain(index..).collect::<Vec<_>>();
                            for frame in removed {
                                state.article_depth -= usize::from(frame.article);
                                state.weixin_depth -= usize::from(frame.weixin);
                                state.suppressed_depth -= usize::from(frame.suppressed);
                            }
                        }
                    }
                }
                raw_result
            }
            Token::CharacterTokens(text) => {
                let mut state = self.0.borrow_mut();
                state.all_markers.observe_text(text.as_ref());
                if state.suppressed_depth == 0 {
                    state.visible_markers.observe_text(text.as_ref());
                    let chars = text
                        .chars()
                        .filter(|character| !character.is_whitespace())
                        .count();
                    if chars > 0 {
                        state.visible_has_text = true;
                        state.visible_text_chars = state.visible_text_chars.saturating_add(chars);
                        state.article_has_text |= state.article_depth > 0;
                        state.weixin_has_text |= state.weixin_depth > 0;
                    }
                }
                TokenSinkResult::Continue
            }
            Token::CommentToken(text) => {
                self.0.borrow_mut().all_markers.observe_text(text.as_ref());
                TokenSinkResult::Continue
            }
            Token::NullCharacterToken => {
                self.0.borrow_mut().limit_exceeded = true;
                TokenSinkResult::Continue
            }
            _ => TokenSinkResult::Continue,
        }
    }
}

fn inspect_html(html: &str) -> HtmlInspection {
    let tokenizer = Tokenizer::new(HtmlInspector::default(), TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from(html));
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink.0.into_inner()
}

pub fn validate_inert_dom_snapshot(html: &str) -> Result<()> {
    if html.len() > MAX_HTML_BYTES {
        bail!("inert_dom_rejected")
    }
    let inspection = inspect_html(html);
    if inspection.limit_exceeded
        || inspection.prohibited_snapshot_element
        || inspection.content_editable
        || inspection.active_content
        || inspection.all_markers.storage
    {
        bail!("inert_dom_rejected")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weixin_admission_ignores_the_page_shell_but_not_page_level_walls() {
        let body = "<p>正文段落，说明为什么要保留证据。</p>".repeat(200);
        let shell = |extra: &str| format!(
            "<html><body><div class='qrcode-con' style='display:none'><img class='jump_wx_qrcode_img' alt='跳转二维码'>\
             <p>微信扫一扫可打开此内容</p></div><button>关注</button><button>分享</button>{extra}\
             <h1 id='activity-name'>合成文章</h1><div id='js_content'>{body}</div></body></html>");
        let decide_for = |html: &str| {
            let mut input = submission(html, "text/html");
            input.capture.capture.source_url = "https://mp.weixin.qq.com/s/synthetic-shell".into();
            let decision = admission_decision(&input.capture, Some(200));
            (decision.class, decision.reason_code, decision.policy_version)
        };
        let (class, reason, policy) = decide_for(&shell(""));
        assert_eq!((class, reason), (AdmissionClass::Article, "supported_weixin_article_body"));
        assert_eq!(policy, "capture-admission-v4-weixin-v2");
        assert_eq!(
            decide_for(&shell("<form><input type='password'></form>")).0,
            AdmissionClass::InteractiveLoginRequired
        );
        let verify = "<html><body><div id='js_verify'>环境异常，完成验证后即可继续</div></body></html>";
        assert_eq!(decide_for(verify).0, AdmissionClass::ChallengeDetected);
        let no_body = shell("").replace("js_content", "js_other");
        assert_eq!(decide_for(&no_body).1, "weixin_article_body_missing");
        // A hidden body is not an article even though its text is in the DOM.
        let hidden = shell("").replace("<div id='js_content'>", "<div id='js_content' style='visibility: hidden'>");
        assert_eq!(decide_for(&hidden).1, "weixin_article_body_missing");
    }
    use memex_core::{BlobDescriptor, CaptureEnvelope, Sensitivity, SourceProfile, VerifiedBlob};
    use std::fs;

    fn submission(html: &str, media_type: &str) -> CaptureSubmission {
        let digest = format!("{:x}", Sha256::digest(html.as_bytes()));
        CaptureSubmission {
            capture: VerifiedCapture {
                source: SourceProfile {
                    schema_version: 1,
                    source_id: "s".into(),
                    name: "test".into(),
                    source_kind: "web".into(),
                    canonical_uris: vec![],
                    evidence_roles: vec![],
                    default_sensitivity: Sensitivity::Public,
                    preferred_extractor: None,
                },
                capture: CaptureEnvelope {
                    schema_version: 1,
                    adapter_id: "test".into(),
                    adapter_version: "1".into(),
                    external_capture_id: "c".into(),
                    source_id: "s".into(),
                    captured_at: "2026-01-01T00:00:00Z".into(),
                    capture_method: "test".into(),
                    sensitivity: Sensitivity::Public,
                    route_summary: None,
                    title: None,
                    source_url: "https://example.test/page".into(),
                    canonical_url: None,
                    artifacts: vec![],
                },
                blobs: vec![VerifiedBlob {
                    descriptor: BlobDescriptor {
                        schema_version: 1,
                        locator: "memory".into(),
                        media_type: media_type.into(),
                        byte_size: html.len() as u64,
                        sha256: digest.clone(),
                        source_url: None,
                    },
                    blob_id: format!("sha256:{digest}"),
                    source_backend_ref: None,
                    bytes: html.as_bytes().to_vec(),
                }],
            },
            metadata: ItemMetadata::default(),
            attachments: vec![],
        }
    }

    fn rejection(html: &str, context: AdmissionContext) -> AdmissionDecision {
        let library = tempfile::tempdir().unwrap();
        let error = prepare_capture(submission(html, "text/html"), context)
            .err()
            .unwrap();
        let decision = error.downcast::<RejectedAdmission>().unwrap().0;
        assert_eq!(fs::read_dir(library.path()).unwrap().count(), 0);
        decision
    }

    #[test]
    fn nested_comments_remain_evidence_not_article_or_quality_input() {
        use memex_analysis::capture_quality::*;
        struct Check;
        impl CaptureQualityModel for Check {
            fn descriptor(&self) -> CaptureQualityDescriptor {
                CaptureQualityDescriptor {
                    model_id: "synthetic".into(),
                    adapter_id: "synthetic".into(),
                    adapter_version: "1".into(),
                    configuration_sha256: format!("sha256:{}", "a".repeat(64)),
                }
            }
            fn check(
                &self,
                input: CaptureQualityInput<'_>,
            ) -> std::result::Result<String, CaptureQualityFailure> {
                assert!(!input.source_markdown.contains("COMMENT-ONLY"));
                Ok(input.source_markdown.into())
            }
        }
        let html = "<article id='js_content'><h1>Synthetic</h1><p>ARTICLE-ONLY 正文。</p><section itemprop='comment'><p>COMMENT-ONLY 独立评论。</p></section></article>";
        for url in [
            "https://mp.weixin.qq.com/s/synthetic-comments",
            "https://example.test/page",
        ] {
            let library = tempfile::tempdir().unwrap();
            let mut input = submission(html, "text/html");
            input.capture.source.source_id = "source_comments_test".into();
            input.capture.capture.source_id = "source_comments_test".into();
            input.capture.capture.source_url = url.into();
            input.capture.blobs[0].source_backend_ref = Some(format!(
                "agent-cas:sha256:{}",
                input.capture.blobs[0].descriptor.sha256
            ));
            let prepared = prepare_capture(input, AdmissionContext::default())
                .unwrap()
                .with_comments(Comments::extract(html).unwrap(), "agent-cas")
                .unwrap();
            assert_eq!(prepared.artifact_bytes(), html.as_bytes());
            assert!(std::str::from_utf8(
                &prepared
                    .source_blobs()
                    .find(|blob| blob.descriptor.locator == "comments_html_v1")
                    .unwrap()
                    .bytes
            )
            .unwrap()
            .contains("COMMENT-ONLY"));
            let (receipt, quality) =
                commit_capture_checked(library.path(), prepared, true, Some(&Check)).unwrap();
            let (_, revision) =
                memex_store::read_item(library.path(), &receipt.item_ids[0]).unwrap();
            assert!(revision.content_markdown.contains("ARTICLE-ONLY"));
            assert!(!revision.content_markdown.contains("COMMENT-ONLY"));
            assert_eq!(quality["state"], "passed");
        }
    }

    #[test]
    fn quality_materialization_recovers_without_recapture_or_model_and_keeps_frozen_prompt() {
        use memex_analysis::capture_quality::*;
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Check(AtomicUsize);
        impl CaptureQualityModel for Check {
            fn descriptor(&self) -> CaptureQualityDescriptor {
                CaptureQualityDescriptor {
                    model_id: "synthetic".into(),
                    adapter_id: "test".into(),
                    adapter_version: "1".into(),
                    configuration_sha256: format!("sha256:{}", "a".repeat(64)),
                }
            }
            fn check(
                &self,
                input: CaptureQualityInput<'_>,
            ) -> std::result::Result<String, CaptureQualityFailure> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(format!("{}\n", input.source_markdown))
            }
        }
        let library = tempfile::tempdir().unwrap();
        let mut input = submission(
            "<article><p>Original synthetic article.</p></article>",
            "text/html",
        );
        input.capture.source.source_id = "source_quality_test".into();
        input.capture.capture.source_id = "source_quality_test".into();
        input.capture.blobs[0].source_backend_ref = Some(format!(
            "agent-cas:sha256:{}",
            input.capture.blobs[0].descriptor.sha256
        ));
        let adapter = input.capture.capture.adapter_id.clone();
        let external_id = input.capture.capture.external_capture_id.clone();
        let prepared = prepare_capture(input, AdmissionContext::default()).unwrap();
        let model = Check(AtomicUsize::new(0));
        std::fs::create_dir(library.path().join("analysis")).unwrap();
        // A temporary I/O failure at the real Analysis persistence boundary.
        std::fs::write(library.path().join("analysis/records"), b"blocked").unwrap();
        let (receipt, quality) =
            commit_capture_checked(library.path(), prepared, true, Some(&model)).unwrap();
        assert_eq!(quality["state"], "pending");
        assert!(quality["analysis_id"].is_null());
        assert_eq!(memex_store::list_items(library.path()).unwrap().len(), 1);
        let (_, revision) = memex_store::read_item(library.path(), &receipt.item_ids[0]).unwrap();
        assert!(revision.content_markdown.ends_with("\n\n"));
        let check = library
            .path()
            .join("catalog/capture-checks")
            .join(capture_quality_key(&adapter, &external_id));
        // Persisted older-version fixture: recovery must not substitute today's prompt.
        let mut frozen: serde_json::Value =
            serde_json::from_slice(&std::fs::read(check.join("frozen.json")).unwrap()).unwrap();
        let historical_prompt = serde_json::json!({"id":"memex.capture-quality","version":"prior","sha256":format!("sha256:{}", "b".repeat(64))});
        frozen["prompt"] = historical_prompt.clone();
        frozen["formatting_policy"] = "historical-policy".into();
        std::fs::write(
            check.join("frozen.json"),
            serde_json::to_vec(&frozen).unwrap(),
        )
        .unwrap();
        std::fs::remove_file(library.path().join("analysis/records")).unwrap();
        let mut restarted = CaptureQualityReconciler::default();
        assert!(restarted.process_next(library.path()).unwrap());
        let recovered = capture_quality_result(library.path(), &adapter, &external_id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered["state"], "corrected");
        let id = recovered["analysis_id"].as_str().unwrap();
        let record: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                library
                    .path()
                    .join("analysis/records")
                    .join(format!("{id}.json")),
            )
            .unwrap(),
        )
        .unwrap();
        let provenance: serde_json::Value =
            serde_json::from_str(record["body_markdown"].as_str().unwrap()).unwrap();
        assert_eq!(provenance["prompt"], historical_prompt);
        assert_eq!(provenance["formatting_policy"], "historical-policy");
        assert_eq!(record["inputs"][0]["revision_id"], receipt.revision_ids[0]);
        // Crash after Analysis durability but before the completion marker.
        std::fs::remove_file(check.join("result.json")).unwrap();
        assert!(CaptureQualityReconciler::default()
            .process_next(library.path())
            .unwrap());
        assert_eq!(
            capture_quality_result(library.path(), &adapter, &external_id)
                .unwrap()
                .unwrap(),
            recovered
        );
        assert_eq!(
            memex_analysis::list_records(library.path()).unwrap().len(),
            1
        );
        assert_eq!(model.0.load(Ordering::SeqCst), 1);
        assert_eq!(memex_store::list_items(library.path()).unwrap().len(), 1);

        // Both persistence windows terminate honestly if metadata advances first.
        for already_materialized in [false, true] {
            let library = tempfile::tempdir().unwrap();
            let mut input = submission(
                "<article><p>Synthetic revision change.</p></article>",
                "text/html",
            );
            input.capture.source.source_id = "source_revision_test".into();
            input.capture.capture.source_id = "source_revision_test".into();
            input.capture.blobs[0].source_backend_ref = Some(format!(
                "agent-cas:sha256:{}",
                input.capture.blobs[0].descriptor.sha256
            ));
            let adapter = input.capture.capture.adapter_id.clone();
            let external_id = input.capture.capture.external_capture_id.clone();
            let check = library
                .path()
                .join("catalog/capture-checks")
                .join(capture_quality_key(&adapter, &external_id));
            if !already_materialized {
                std::fs::create_dir(library.path().join("analysis")).unwrap();
                std::fs::write(library.path().join("analysis/records"), b"blocked").unwrap();
            }
            let prepared = prepare_capture(input, AdmissionContext::default()).unwrap();
            let (receipt, _) =
                commit_capture_checked(library.path(), prepared, true, Some(&model)).unwrap();
            if already_materialized {
                std::fs::remove_file(check.join("result.json")).unwrap();
            } else {
                std::fs::remove_file(library.path().join("analysis/records")).unwrap();
            }
            memex_store::revise_item_metadata(
                library.path(),
                &receipt.item_ids[0],
                memex_store::ItemMetadataUpdate {
                    expected_revision_id: receipt.revision_ids[0].clone(),
                    title: "Updated title".into(),
                    manual_tags: vec![],
                },
            )
            .unwrap();
            assert!(CaptureQualityReconciler::default()
                .process_next(library.path())
                .unwrap());
            let result = capture_quality_result(library.path(), &adapter, &external_id)
                .unwrap()
                .unwrap();
            assert_eq!(result["state"], "unavailable");
            assert_eq!(result["reason"], "capture_revision_changed");
            assert!(result["analysis_id"].is_null());
        }
    }

    #[test]
    fn legacy_unchecked_capture_can_replay_off_but_cannot_acquire_quality() {
        let library = tempfile::tempdir().unwrap();
        let prepare = || {
            let mut input = submission(
                "<article><p>Legacy synthetic article.</p></article>",
                "text/html",
            );
            input.capture.source.source_id = "source_legacy_test".into();
            input.capture.capture.source_id = "source_legacy_test".into();
            input.capture.blobs[0].source_backend_ref = Some(format!(
                "agent-cas:sha256:{}",
                input.capture.blobs[0].descriptor.sha256
            ));
            prepare_capture(input, AdmissionContext::default()).unwrap()
        };
        let original = commit_capture(library.path(), prepare()).unwrap();
        let mut wrong = prepare();
        wrong.draft.title = "Different payload".into();
        assert!(commit_capture_checked(library.path(), wrong, false, None)
            .unwrap_err()
            .is::<memex_store::IngestConflict>());
        let upgraded = commit_capture_checked(library.path(), prepare(), true, None).unwrap_err();
        assert_eq!(upgraded.to_string(), "request_identity_conflict");
        let (replay, quality) =
            commit_capture_checked(library.path(), prepare(), false, None).unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(original.item_ids, replay.item_ids);
        assert_eq!(quality["state"], "not_requested");
    }

    #[test]
    fn competing_unchecked_commit_cannot_receive_another_payloads_quality() {
        use memex_analysis::capture_quality::*;
        struct Competitor {
            library: std::path::PathBuf,
            prepared: std::sync::Mutex<Option<PreparedCapture>>,
        }
        impl CaptureQualityModel for Competitor {
            fn descriptor(&self) -> CaptureQualityDescriptor {
                CaptureQualityDescriptor {
                    model_id: "synthetic".into(),
                    adapter_id: "test".into(),
                    adapter_version: "1".into(),
                    configuration_sha256: format!("sha256:{}", "a".repeat(64)),
                }
            }
            fn check(
                &self,
                input: CaptureQualityInput<'_>,
            ) -> std::result::Result<String, CaptureQualityFailure> {
                // Deterministic interleaving: unchecked A commits after B's lookup,
                // before B freezes its result or reaches the Store commit.
                commit_capture(&self.library, self.prepared.lock().unwrap().take().unwrap())
                    .unwrap();
                Ok(input.source_markdown.into())
            }
        }
        let library = tempfile::tempdir().unwrap();
        let prepare = |title: &str| {
            let mut input = submission(
                "<article><p>Synthetic competing capture.</p></article>",
                "text/html",
            );
            input.capture.source.source_id = "source_competing_test".into();
            input.capture.capture.source_id = "source_competing_test".into();
            input.capture.blobs[0].source_backend_ref = Some(format!(
                "agent-cas:sha256:{}",
                input.capture.blobs[0].descriptor.sha256
            ));
            let mut result = prepare_capture(input, AdmissionContext::default()).unwrap();
            result.draft.title = title.into();
            result
        };
        let b = prepare("Payload B");
        let adapter = b.capture.capture.adapter_id.clone();
        let external_id = b.capture.capture.external_capture_id.clone();
        let model = Competitor {
            library: library.path().into(),
            prepared: std::sync::Mutex::new(Some(prepare("Payload A"))),
        };
        assert!(
            commit_capture_checked(library.path(), b, true, Some(&model))
                .unwrap_err()
                .is::<memex_store::IngestConflict>()
        );
        assert!(CaptureQualityReconciler::default()
            .process_next(library.path())
            .unwrap());
        assert_eq!(
            capture_quality_result(library.path(), &adapter, &external_id)
                .unwrap()
                .unwrap()["reason"],
            "capture_identity_conflict"
        );
        assert!(memex_analysis::list_records(library.path())
            .unwrap()
            .is_empty());
        let items = memex_store::list_items(library.path()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].1.title, "Payload A");
        let (replayed, quality) =
            commit_capture_checked(library.path(), prepare("Payload A"), false, None).unwrap();
        assert!(replayed.idempotent_replay);
        assert_eq!(replayed.item_ids, vec![items[0].0.item_id.clone()]);
        assert_eq!(quality["state"], "not_requested");
        assert!(!CaptureQualityReconciler::default()
            .process_next(library.path())
            .unwrap());
    }

    #[test]
    fn youtube_attachments_lead_the_body_once_and_other_attachments_do_not() {
        let video = "https://www.youtube.com/watch?v=tEse6n4hEJQ";
        let remote = |url: &str| DraftAttachmentRef::Remote {
            url: url.into(),
            media_type: Some("text/html".into()),
            title: None,
        };
        let mut with_video = submission("<article><p>Episode notes.</p></article>", "text/html");
        with_video.attachments = vec![remote("https://example.invalid/paper.pdf"), remote(video)];
        let body = prepare_capture(with_video, AdmissionContext::default())
            .unwrap()
            .draft
            .content_markdown;
        assert!(body.starts_with(&format!("[YouTube: {video}]({video})\n\nEpisode notes.")));
        assert!(!body.contains("paper.pdf"));
        assert_eq!(body.matches(video).count(), 2);

        let html = format!("<article><p>Watch <a href='{video}'>here</a>.</p></article>");
        let mut already = submission(&html, "text/html");
        already.attachments = vec![remote(video)];
        let body = prepare_capture(already, AdmissionContext::default())
            .unwrap()
            .draft
            .content_markdown;
        assert!(!body.starts_with("[YouTube"));
        assert_eq!(body.matches(video).count(), 1);
    }

    #[test]
    fn login_words_in_a_long_article_are_content_but_short_pages_stay_walls() {
        let body = "Transcript paragraph about building software together. ".repeat(80);
        let long = format!(
            "<article><p>{body}</p><p>I logged in with the login it gave me.</p></article>"
        );
        assert!(
            prepare_capture(submission(&long, "text/html"), AdmissionContext::default()).is_ok()
        );
        // Short page with the same words is still treated as a login wall.
        assert_eq!(
            rejection(
                "<article><p>Please log in to continue reading.</p></article>",
                AdmissionContext::default()
            )
            .class,
            AdmissionClass::InteractiveLoginRequired
        );
        // Long text outside any article container does not get the exemption.
        assert_eq!(
            rejection(
                &format!("<div><p>{body}</p><p>Log in</p></div>"),
                AdmissionContext::default()
            )
            .class,
            AdmissionClass::InteractiveLoginRequired
        );
        // A credential control blocks regardless of length.
        assert_eq!(
            rejection(
                &format!("<article><p>{body}</p><form><input type=password></form></article>"),
                AdmissionContext::default()
            )
            .class,
            AdmissionClass::InteractiveLoginRequired
        );
    }

    #[test]
    fn metadata_mentions_do_not_imply_login_or_supply_missing_article_text() {
        let meta =
            "<head><meta name=description content='An article about login and 二维码'></head>";
        assert_eq!(
            rejection(
                &format!("{meta}<body><div id=app></div></body>"),
                AdmissionContext::default()
            )
            .class,
            AdmissionClass::UnsupportedContent
        );
        assert!(prepare_capture(
            submission(
                &format!("{meta}<article><p>Visible article.</p></article>"),
                "text/html"
            ),
            AdmissionContext::default()
        )
        .is_ok());
        assert_eq!(
            rejection(
                &format!("{meta}<form><input type=password></form>"),
                AdmissionContext::default()
            )
            .class,
            AdmissionClass::InteractiveLoginRequired
        );
        assert_eq!(
            rejection(
                &format!("{meta}<div class=cf-challenge>Wait</div>"),
                AdmissionContext::default()
            )
            .class,
            AdmissionClass::ChallengeDetected
        );
        let readonly = "<article><span contenteditable=false>Read only.</span><img src='https://cdn.test/qrv6-asset-OQRN09'><p>Ordinary article.</p></article>";
        assert!(prepare_capture(
            submission(readonly, "text/html"),
            AdmissionContext::default()
        )
        .is_ok());
        assert!(validate_inert_dom_snapshot(readonly).is_ok());
        for value in ["true", "plaintext-only", ""] {
            let editable =
                format!("<article><div contenteditable='{value}'>User input</div></article>");
            assert!(validate_inert_dom_snapshot(&editable).is_err());
            assert_eq!(
                rejection(&editable, AdmissionContext::default()).class,
                AdmissionClass::InteractiveLoginRequired
            );
        }
        assert_eq!(
            rejection(
                "<article><img class=qr-code><p>Scan to continue</p></article>",
                AdmissionContext::default()
            )
            .class,
            AdmissionClass::InteractiveLoginRequired
        );
    }

    #[test]
    fn podcast_body_ignores_navigation_but_keeps_raw_evidence_and_admission_gates() {
        let html = "<nav><button>Log in</button><a href='https://youtu.be/PROMOTION01'>Promotion</a></nav><main><h1>Episode fixture</h1><iframe src='https://www.youtube-nocookie.com/embed/abcdefghijk'></iframe><div class=transcript><p>FIRST transcript paragraph.</p><p>LAST transcript paragraph.</p></div></main>";
        let make = |html: &str| {
            let mut s = submission(html, "text/html");
            s.capture.capture.source_url = "https://ampcode.com/podcast/season-2/episode-1".into();
            s.capture.blobs[0].source_backend_ref = Some(format!("agent-cas:sha256:{}", s.capture.blobs[0].descriptor.sha256));
            s.capture.capture.artifacts = vec![s.capture.blobs[0].descriptor.clone()];
            s
        };
        let prepared = prepare_capture(make(html), AdmissionContext::default()).unwrap();
        assert_eq!(prepared.artifact_bytes(), html.as_bytes());
        assert_eq!(lots_scout_extract::podcast_video_id(html).unwrap().as_deref(), Some("abcdefghijk"));
        let library = tempfile::tempdir().unwrap();
        let receipt = commit_capture(library.path(), prepared).unwrap();
        let (_, revision) = memex_store::read_item(library.path(), &receipt.item_ids[0]).unwrap();
        assert_eq!(revision.title, "Episode fixture");
        assert!(revision.content_markdown.contains("FIRST transcript paragraph."));
        assert!(revision.content_markdown.contains("LAST transcript paragraph."));
        assert!(!revision.content_markdown.contains("Promotion"));
        assert!(!revision.content_markdown.contains("Log in"));
        for bad in [
            html.replace("<nav>", "<nav><input type=password>"),
            html.replace("<nav>", "<textarea autocomplete=one-time-code></textarea><nav>"),
            html.replace("<main>", "<main class=cf-challenge>"),
            html.replace("<main>", "<main contenteditable=''>"),
            html.replace("<main>", "<main contenteditable=plaintext-only>"),
            html.replace("<h1>", "<p>Verify you are human</p><h1>"),
            html.replace("<h1>", "<p>Log in to continue</p><h1>"),
            html.replace("<main>", "<main hidden>"),
            html.replace("class=transcript", "class=transcript style='display:none'"),
            html.replace("FIRST transcript paragraph.", "Log in to continue"),
            html.replace("FIRST transcript paragraph.", "Verify you are human"),
            html.replace("FIRST transcript paragraph.", "<button>Continue</button>"),
            html.replace("class=transcript", "class=other"),
            html.replace("</main>", "<div class=transcript>Second ambiguous body</div></main>"),
        ] {
            assert!(prepare_capture(make(&bad), AdmissionContext::default()).is_err());
        }
        let mut foreign = make(html);
        foreign.capture.capture.source_url = "https://example.test/podcast/episode-1".into();
        assert!(prepare_capture(foreign, AdmissionContext::default()).is_err());
        assert!(prepare_capture(make(html), AdmissionContext { http_status: Some(403), preview_approval: None }).is_err());
        let ambiguous = html.replace("</main>", "<a href='https://youtu.be/PROMOTION01'>Second video</a></main>");
        assert_eq!(lots_scout_extract::podcast_video_id(&ambiguous).unwrap_err().to_string(), "podcast_video_ambiguous");
        assert!(lots_scout_extract::podcast_video_id(&html.replace("</main>", "</main><main>Other</main>")).is_err());
    }

    #[test]
    fn linkresearcher_public_response_is_identity_bound_raw_evidence() {
        let id = "11111111-2222-3333-4444-555555555555";
        let url = format!("https://www.linkresearcher.com/theses/{id}");
        let make = |payload: serde_json::Value| {
            let mut s = submission(&payload.to_string(), "application/json");
            s.capture.capture.source_url = url.clone();
            s.capture.capture.canonical_url = Some(url.clone());
            s.capture.blobs[0].descriptor.source_url =
                LinkresearcherArticleExtractor::api_url(&url);
            s.capture.blobs[0].source_backend_ref = Some(format!(
                "agent-cas:sha256:{}",
                s.capture.blobs[0].descriptor.sha256
            ));
            s.capture.capture.artifacts = vec![s.capture.blobs[0].descriptor.clone()];
            s
        };
        let payload = serde_json::json!({
            "id":id, "title":"Public fixture", "content":"<p>Unused legacy content</p>",
            "wholeHtmlContent":"<span class=custom-entry-btn contenteditable=false>↵</span><p>FIRST article paragraph.</p><aside itemprop=comment>COMMENT-ONLY</aside><p>LAST paragraph.</p><span class=custom-delete-btn contenteditable=false>X factor remains</span>",
            "relevant":[{"title":"NOT-ARTICLE"}]
        });
        let raw = payload.to_string().into_bytes();
        let prepared = prepare_capture(make(payload.clone()), AdmissionContext::default()).unwrap();
        assert_eq!(prepared.artifact_bytes(), raw);
        let comments = prepared.extract_comments().unwrap();
        assert_eq!(comments.state, CommentsState::Captured);
        assert!(std::str::from_utf8(comments.html.as_ref().unwrap())
            .unwrap()
            .contains("COMMENT-ONLY"));
        let prepared = prepared.with_comments(comments, "agent-cas").unwrap();
        assert_eq!(prepared.artifact_bytes(), raw);
        assert_eq!(prepared.source_blobs().count(), 3);
        let library = tempfile::tempdir().unwrap();
        let (receipt, quality) =
            commit_capture_checked(library.path(), prepared, false, None).unwrap();
        assert_eq!(quality["state"], "not_requested");
        let (_, revision) = memex_store::read_item(library.path(), &receipt.item_ids[0]).unwrap();
        assert!(revision
            .content_markdown
            .contains("FIRST article paragraph."));
        assert!(revision.content_markdown.contains("LAST paragraph."));
        assert!(!revision.content_markdown.contains("COMMENT-ONLY"));
        assert!(!revision.content_markdown.contains("Unused legacy"));
        assert!(!revision.content_markdown.contains("NOT-ARTICLE"));
        assert!(!revision.content_markdown.contains('↵'));
        assert!(revision.content_markdown.contains("X factor remains"));
        let again = prepare_capture(make(payload.clone()), AdmissionContext::default()).unwrap();
        let comments = again.extract_comments().unwrap();
        let again = again.with_comments(comments, "agent-cas").unwrap();
        assert!(
            commit_capture_checked(library.path(), again, false, None)
                .unwrap()
                .0
                .idempotent_replay
        );
        let mut changed_body = payload.clone();
        changed_body["wholeHtmlContent"] = "<p>Different source response.</p>".into();
        let changed = prepare_capture(make(changed_body), AdmissionContext::default()).unwrap();
        assert!(commit_capture_checked(library.path(), changed, false, None).is_err());
        let mut wrong_blob_url = make(payload.clone());
        wrong_blob_url.capture.blobs[0].descriptor.source_url =
            Some("https://other.test/api".into());
        assert_eq!(
            admission_decision(&wrong_blob_url.capture, Some(200)).class,
            AdmissionClass::UnsupportedContent
        );
        let mut multiple = make(payload.clone());
        multiple
            .capture
            .blobs
            .push(multiple.capture.blobs[0].clone());
        assert_eq!(
            admission_decision(&multiple.capture, Some(200)).class,
            AdmissionClass::UnsupportedContent
        );
        for (field, value, expected) in [
            (
                "id",
                "99999999-2222-3333-4444-555555555555",
                AdmissionClass::UnsupportedContent,
            ),
            (
                "wholeHtmlContent",
                "<form><input type=password></form>",
                AdmissionClass::InteractiveLoginRequired,
            ),
            (
                "wholeHtmlContent",
                "<div class=cf-challenge>Wait</div>",
                AdmissionClass::ChallengeDetected,
            ),
            (
                "wholeHtmlContent",
                "<script>not visible</script>",
                AdmissionClass::UnsupportedContent,
            ),
        ] {
            let mut changed = payload.clone();
            changed[field] = value.into();
            assert_eq!(
                admission_decision(&make(changed).capture, Some(200)).class,
                expected
            );
        }
        assert_eq!(
            admission_decision(&make(payload.clone()).capture, Some(403)).class,
            AdmissionClass::InteractiveLoginRequired
        );
        let mut wrong_origin = make(payload);
        wrong_origin.capture.capture.source_url =
            "https://other.test/theses/11111111-2222-3333-4444-555555555555".into();
        assert_eq!(
            admission_decision(&wrong_origin.capture, Some(200)).class,
            AdmissionClass::UnsupportedContent
        );
    }

    #[test]
    fn semantic_article_is_admitted_and_generic_is_not() {
        assert!(prepare_capture(
            submission("<article><p>body</p></article>", "text/html"),
            AdmissionContext::default()
        )
        .is_ok());
        assert_eq!(
            rejection(
                "<main><h1>Readable title</h1><p>Readable body</p></main>",
                AdmissionContext::default()
            )
            .class,
            AdmissionClass::NeedsPreview
        );
    }

    #[test]
    fn preview_approval_is_bound_to_digest_and_policy() {
        let digest = submission("<main>body</main>", "text/html").capture.blobs[0]
            .descriptor
            .sha256
            .clone();
        let approved = AdmissionContext {
            http_status: None,
            preview_approval: Some(PreviewApproval {
                artifact_sha256: digest,
                policy_version: ADMISSION_POLICY_VERSION.into(),
            }),
        };
        assert!(prepare_capture(submission("<main>body</main>", "text/html"), approved).is_ok());
        assert_eq!(
            rejection(
                "<main>body</main>",
                AdmissionContext {
                    http_status: None,
                    preview_approval: Some(PreviewApproval {
                        artifact_sha256: "0".repeat(64),
                        policy_version: ADMISSION_POLICY_VERSION.into()
                    })
                }
            )
            .class,
            AdmissionClass::NeedsPreview
        );
    }

    #[test]
    fn authentication_statuses_and_forms_are_interactive() {
        for status in [401, 403, 407] {
            assert_eq!(
                rejection(
                    "<article>body</article>",
                    AdmissionContext {
                        http_status: Some(status),
                        preview_approval: None
                    }
                )
                .class,
                AdmissionClass::InteractiveLoginRequired
            );
        }
        for html in [
            "<form><input type=\"password\"></form>",
            "<form><input type = password></form>",
            "<form>login <input name=otp></form>",
            "<form><input name=\"verification code\"></form>",
            "<article><input type = \"password\"><p>body</p></article>",
            "<article><input type=password><p>body</p></article>",
            "<article><input TYPE=PASSWORD><p>body</p></article>",
            "<article><input data-marker='>' type='password'><p>body</p></article>",
            "<article><input type=pass&#119;ord><p>body</p></article>",
            "<article><input autocomplete='one-time-code'><p>body</p></article>",
            "<article><input data-marker='>' autocomplete=one&#45;time-code><p>body</p></article>",
            "<article><input id=otp_code><p>body</p></article>",
            "<article><input type=text><p>body</p></article>",
            "<article><textarea>standalone value</textarea><p>body</p></article>",
            "<article><select><option>standalone value</option></select><p>body</p></article>",
            "<article><div contenteditable>editable value</div><p>body</p></article>",
        ] {
            assert_eq!(
                rejection(html, AdmissionContext::default()).class,
                AdmissionClass::InteractiveLoginRequired
            );
        }
    }

    #[test]
    fn tokenizer_ignores_fake_articles_and_bounds_malformed_tags() {
        for html in [
            "<!-- <article>comment-only fake body</article> -->",
            "<script>const fake = '<article>script-only fake body</article>';</script>",
        ] {
            let decision = rejection(html, AdmissionContext::default());
            assert_eq!(decision.class, AdmissionClass::UnsupportedContent);
            assert_eq!(decision.reason_code, "html_has_no_visible_content");
        }

        let mut malformed = String::from("<article>");
        malformed.push_str(&"<input ".repeat(20_000));
        malformed.push_str("><p>visible body</p></article>");
        let decision = rejection(&malformed, AdmissionContext::default());
        assert_eq!(decision.class, AdmissionClass::UnsupportedContent);
        assert_eq!(decision.reason_code, "html_structure_limit_exceeded");
    }

    #[test]
    fn inert_dom_rejects_controls_editability_and_active_or_storage_content() {
        validate_inert_dom_snapshot("<article>inert body</article>").unwrap();
        for html in [
            "<input>",
            "<textarea>secret</textarea>",
            "<select><option>secret</option></select>",
            "<div contenteditable>secret</div>",
            "<input type=password value=secret>",
            "<input data-marker='>' type=pass&#119;ord value=secret>",
            "<script>document.cookie</script>",
            "<!-- localStorage -->",
        ] {
            assert!(validate_inert_dom_snapshot(html).is_err(), "{html}");
        }
    }

    #[test]
    fn challenge_non_html_and_jike_are_never_overridden() {
        assert_eq!(
            rejection("<div>captcha</div>", AdmissionContext::default()).class,
            AdmissionClass::ChallengeDetected
        );
        let error = prepare_capture(
            submission("plain", "text/plain"),
            AdmissionContext::default(),
        )
        .err()
        .unwrap();
        assert_eq!(
            error.downcast::<RejectedAdmission>().unwrap().0.class,
            AdmissionClass::UnsupportedContent
        );
        let jike = "<html>即刻App 二维码登录 verify</html>";
        assert_eq!(
            rejection(jike, AdmissionContext::default()).class,
            AdmissionClass::InteractiveLoginRequired
        );

        let mut malformed = submission("<article>body</article>", "text/html");
        malformed.capture.blobs[0].descriptor.sha256 = "0".repeat(64);
        let decision = admission_decision(&malformed.capture, None);
        assert_eq!(decision.class, AdmissionClass::UnsupportedContent);
        assert_eq!(decision.reason_code, "primary_artifact_integrity_invalid");
    }
}
