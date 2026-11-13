//! AI insights (`ai_insights_v1`): 重点标注 (highlights), 金句 (quotes) and
//! 事实核查 (fact check) for one item, produced on request (`insight_request`)
//! and read with the item (`library_item_read.ai_insights`). Every item points
//! at a block (`block_index`, the agent's Markdown split, see `blocks`) and
//! quotes an exact substring of it. Unknown values from a newer agent are kept;
//! a malformed section never fails the read it rides on.

use crate::dto::MinutesState;
use serde::{Deserialize, Deserializer, Serialize};

pub const INSIGHTS_FEATURE: &str = "ai_insights_v1";

/// State of one insight kind; the same states as AI minutes.
pub type InsightState = MinutesState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightKind {
    Highlights,
    Quotes,
    FactCheck,
}

impl InsightKind {
    pub const ALL: [InsightKind; 3] = [InsightKind::Highlights, InsightKind::Quotes, InsightKind::FactCheck];

    pub fn label(self) -> &'static str {
        crate::tx(match self {
            InsightKind::Highlights => "重点标注",
            InsightKind::Quotes => "金句",
            InsightKind::FactCheck => "事实核查",
        })
    }
}

/// Which kinds the agent can produce. `None` in `Capabilities`: not advertised.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InsightKinds {
    pub highlights: bool,
    pub quotes: bool,
    pub fact_check: bool,
}

impl InsightKinds {
    pub fn has(&self, kind: InsightKind) -> bool {
        match kind {
            InsightKind::Highlights => self.highlights,
            InsightKind::Quotes => self.quotes,
            InsightKind::FactCheck => self.fact_check,
        }
    }

    /// `ai_insights_v1` object: per kind a bool or `{available}`, or a `kinds`
    /// list. A feature without an object (or an unreadable one) offers all kinds;
    /// the agent still answers `no_insight_provider` when it cannot.
    pub fn from_agent(value: Option<&serde_json::Value>) -> Self {
        let all = Self { highlights: true, quotes: true, fact_check: true };
        let Some(v) = value.filter(|v| v.is_object()) else { return all };
        if let Some(list) = v.get("kinds").and_then(|k| k.as_array()) {
            let has = |name: &str| list.iter().any(|k| k.as_str() == Some(name));
            return Self { highlights: has("highlights"), quotes: has("quotes"), fact_check: has("fact_check") };
        }
        let one = |name: &str| match v.get(name) {
            None => true,
            Some(serde_json::Value::Bool(b)) => *b,
            Some(o) => o.get("available").and_then(|a| a.as_bool()).unwrap_or(true),
        };
        Self { highlights: one("highlights"), quotes: one("quotes"), fact_check: one("fact_check") }
    }
}

/// `bold | highlight | positive | negative`; others kept and shown plainly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum HighlightStyle {
    Bold,
    Highlight,
    Positive,
    Negative,
    Other(String),
}

impl From<String> for HighlightStyle {
    fn from(s: String) -> Self {
        match s.as_str() {
            "bold" => Self::Bold,
            "highlight" => Self::Highlight,
            "positive" => Self::Positive,
            "negative" => Self::Negative,
            _ => Self::Other(s),
        }
    }
}

impl From<HighlightStyle> for String {
    fn from(s: HighlightStyle) -> Self {
        match s {
            HighlightStyle::Bold => "bold".into(),
            HighlightStyle::Highlight => "highlight".into(),
            HighlightStyle::Positive => "positive".into(),
            HighlightStyle::Negative => "negative".into(),
            HighlightStyle::Other(s) => s,
        }
    }
}

/// A highlight term on one side (highlights v2): `start..end` are UTF-8 byte
/// offsets into that side's `split_blocks(...)[block]` raw text, and `text` is
/// exactly those bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermSpan {
    pub text: String,
    pub block: usize,
    pub start: usize,
    pub end: usize,
}

/// Which side of the text a highlight is placed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Original,
    Translation,
}

/// `sides` of succeeded highlights (v2).
pub const SIDES_ORIGINAL_ONLY: &str = "original_only";
pub const SIDES_BOTH: &str = "original_and_translation";
/// `unavailable` reason: the translation changed since the highlights were made.
pub const TRANSLATION_CHANGED: &str = "insight_translation_changed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightItem {
    pub block_index: usize,
    /// v1: a quote found verbatim in the original block. v2: the original
    /// term, or the translated one when there is no original span.
    pub quote: String,
    pub style: HighlightStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// v2 spans; a malformed span reads as absent (and is then not drawn).
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub original: Option<TermSpan>,
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub translation: Option<TermSpan>,
}

impl HighlightItem {
    /// A v1 item (no spans), placed by finding `quote` in the original.
    pub fn v1(block_index: usize, quote: &str, style: &str, label: Option<&str>) -> Self {
        Self { block_index, quote: quote.into(), style: style.to_string().into(), label: label.map(str::to_string), original: None, translation: None }
    }

    /// Carries v2 spans (then only spans are drawn, never a search for `quote`).
    pub fn has_spans(&self) -> bool {
        self.original.is_some() || self.translation.is_some()
    }

    pub fn span(&self, side: Side) -> Option<&TermSpan> {
        match side {
            Side::Original => self.original.as_ref(),
            Side::Translation => self.translation.as_ref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteItem {
    pub block_index: usize,
    pub quote: String,
    #[serde(default)]
    pub translation_zh: Option<String>,
    #[serde(default)]
    pub why: String,
}

/// `supported | disputed | incorrect | unverifiable | opinion`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Verdict {
    Supported,
    Disputed,
    Incorrect,
    Unverifiable,
    Opinion,
    Other(String),
}

impl From<String> for Verdict {
    fn from(s: String) -> Self {
        match s.as_str() {
            "supported" => Self::Supported,
            "disputed" => Self::Disputed,
            "incorrect" => Self::Incorrect,
            "unverifiable" => Self::Unverifiable,
            "opinion" => Self::Opinion,
            _ => Self::Other(s),
        }
    }
}

impl From<Verdict> for String {
    fn from(v: Verdict) -> Self {
        match v {
            Verdict::Supported => "supported".into(),
            Verdict::Disputed => "disputed".into(),
            Verdict::Incorrect => "incorrect".into(),
            Verdict::Unverifiable => "unverifiable".into(),
            Verdict::Opinion => "opinion".into(),
            Verdict::Other(s) => s,
        }
    }
}

impl Verdict {
    pub fn label(&self) -> String {
        match self {
            Verdict::Supported => crate::tx("支持").into(),
            Verdict::Disputed => crate::tx("存疑").into(),
            Verdict::Incorrect => crate::tx("有误").into(),
            Verdict::Unverifiable => crate::tx("无法核实").into(),
            Verdict::Opinion => crate::tx("观点").into(),
            Verdict::Other(s) => s.clone(),
        }
    }
}

/// Confidence label: `high | medium | low`, others as given.
pub fn confidence_label(confidence: &str) -> String {
    match confidence {
        "high" => crate::tx("置信度高").into(),
        "medium" => crate::tx("置信度中").into(),
        "low" => crate::tx("置信度低").into(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactItem {
    pub block_index: usize,
    pub quote: String,
    pub verdict: Verdict,
    #[serde(default)]
    pub explanation: String,
    #[serde(default)]
    pub confidence: String,
}

/// One kind: `{state, reason, items, dropped?, basis?}`. Items count only when
/// the state is `succeeded`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsightSection<T> {
    pub state: InsightState,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    /// Items the agent dropped (e.g. a quote it could not locate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<u32>,
    /// `model_knowledge` for fact checks made without the web.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    /// Succeeded highlights (v2): `original_only` | `original_and_translation`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sides: Option<String>,
}

impl<T> InsightSection<T> {
    pub fn new(state: InsightState) -> Self {
        Self { state, reason: None, items: vec![], dropped: None, basis: None, sides: None }
    }

    pub fn is_pending(&self) -> bool {
        matches!(self.state, MinutesState::Queued | MinutesState::Running)
    }

    /// Items, only when the agent says the kind succeeded.
    pub fn done(&self) -> Option<&[T]> {
        (self.state == MinutesState::Succeeded).then_some(self.items.as_slice())
    }
}

fn lenient<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiInsights {
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub highlights: Option<InsightSection<HighlightItem>>,
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub quotes: Option<InsightSection<QuoteItem>>,
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub fact_check: Option<InsightSection<FactItem>>,
}

impl AiInsights {
    /// `(state, reason)` of one kind; `None` when the agent did not report it.
    pub fn state(&self, kind: InsightKind) -> Option<(&InsightState, Option<&str>)> {
        match kind {
            InsightKind::Highlights => self.highlights.as_ref().map(|s| (&s.state, s.reason.as_deref())),
            InsightKind::Quotes => self.quotes.as_ref().map(|s| (&s.state, s.reason.as_deref())),
            InsightKind::FactCheck => self.fact_check.as_ref().map(|s| (&s.state, s.reason.as_deref())),
        }
    }

    pub fn is_pending(&self, kind: InsightKind) -> bool {
        self.state(kind).is_some_and(|(s, _)| matches!(s, MinutesState::Queued | MinutesState::Running))
    }
}

/// Why an insight failed, in plain words; unknown codes as given.
pub fn insight_reason_label(reason: Option<&str>) -> String {
    match reason {
        None => crate::tx("代理未说明原因").into(),
        Some("no_insight_provider") => crate::tx("还没有配置分析模型").into(),
        Some(TRANSLATION_CHANGED) => crate::tx("译文已更新，重点标注的位置已过期，需要重新生成").into(),
        Some(other) => crate::minutes_reason_label(Some(other)),
    }
}
