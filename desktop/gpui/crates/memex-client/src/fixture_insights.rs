//! Synthetic AI insights for the fixture backend. Every quote is an exact
//! substring of the block it names (the agent's split, `blocks::split_blocks`);
//! the explanations are invented and say so.

use crate::blocks::split_blocks;
use crate::dto::MinutesState;
use crate::insights::*;
use std::time::{Duration, Instant};

const QUEUED: Duration = Duration::from_millis(400);
const DONE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub(crate) struct Insight<T> {
    state: MinutesState,
    reason: Option<String>,
    started: Option<Instant>,
    items: Vec<T>,
    /// Highlights only: `sides` of the items (`None` for the other kinds).
    sides: Option<&'static str>,
}

impl<T: Clone> Insight<T> {
    fn new(state: MinutesState, items: Vec<T>) -> Self {
        Self { state, reason: None, started: None, items, sides: None }
    }

    fn advance(&mut self) {
        let Some(started) = self.started else { return };
        if started.elapsed() >= DONE {
            self.started = None;
            self.state = MinutesState::Succeeded;
        } else if started.elapsed() >= QUEUED {
            self.state = MinutesState::Running;
        }
    }

    fn request(&mut self) -> bool {
        if matches!(self.state, MinutesState::NotRequested | MinutesState::Failed | MinutesState::Unavailable) {
            self.state = MinutesState::Queued;
            self.reason = None;
            self.started = Some(Instant::now());
            return true;
        }
        false
    }

    fn wire(&self, basis: Option<&str>) -> InsightSection<T> {
        let done = self.state == MinutesState::Succeeded;
        InsightSection {
            state: self.state.clone(),
            reason: self.reason.clone(),
            items: if done { self.items.clone() } else { vec![] },
            dropped: None,
            basis: basis.map(str::to_string),
            sides: self.sides.filter(|_| done).map(str::to_string),
        }
    }
}

/// How an item's synthetic highlights start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Start {
    NotRequested,
    /// Done, marked on both sides when the translation pairs with the original.
    Ready,
    /// Done before the translation existed: `original_only`.
    ReadyOriginalOnly,
    /// The translation changed after they were made: `unavailable`.
    Stale,
}

#[derive(Debug, Clone)]
pub(crate) struct Insights {
    highlights: Insight<HighlightItem>,
    quotes: Insight<QuoteItem>,
    fact_check: Insight<FactItem>,
    /// Highlights with translation spans (when the translation pairs) and without.
    both: Vec<HighlightItem>,
    original_only: Vec<HighlightItem>,
    pairs: bool,
}

impl Insights {
    /// Synthetic insights of `markdown` (and its eventual `translation`).
    pub(crate) fn new(markdown: &str, translation: Option<&str>, english: bool, start: Start, failed_fact_check: bool) -> Self {
        let (q, f) = synthesize(markdown, english);
        let pairs = translation.is_some_and(|t| split_blocks(t).len() == split_blocks(markdown).len());
        let both = highlight_terms(markdown, translation.filter(|_| pairs), english);
        let original_only = highlight_terms(markdown, None, english);
        let ready = matches!(start, Start::Ready | Start::ReadyOriginalOnly);
        let state = if ready { MinutesState::Succeeded } else { MinutesState::NotRequested };
        let mut fact_check = Insight::new(state.clone(), f);
        if failed_fact_check {
            fact_check.state = MinutesState::Failed;
            fact_check.reason = Some("provider_timeout".into());
        }
        let mut highlights = Insight::new(state.clone(), vec![]);
        match start {
            Start::Ready if pairs => (highlights.items, highlights.sides) = (both.clone(), Some(SIDES_BOTH)),
            Start::Ready | Start::ReadyOriginalOnly => (highlights.items, highlights.sides) = (original_only.clone(), Some(SIDES_ORIGINAL_ONLY)),
            Start::Stale => {
                highlights.state = MinutesState::Unavailable;
                highlights.reason = Some(TRANSLATION_CHANGED.into());
            }
            Start::NotRequested => {}
        }
        Self { highlights, quotes: Insight::new(state, q), fact_check, both, original_only, pairs }
    }

    pub(crate) fn advance(&mut self) {
        self.highlights.advance();
        self.quotes.advance();
        self.fact_check.advance();
    }

    /// `translated`: the item's translation has succeeded now, so new
    /// highlights can be marked on it too (when its blocks pair).
    pub(crate) fn request(&mut self, kinds: &[InsightKind], translated: bool) {
        for kind in kinds {
            match kind {
                InsightKind::Highlights => {
                    if self.highlights.request() {
                        let both = translated && self.pairs;
                        self.highlights.items = if both { self.both.clone() } else { self.original_only.clone() };
                        self.highlights.sides = Some(if both { SIDES_BOTH } else { SIDES_ORIGINAL_ONLY });
                    }
                }
                InsightKind::Quotes => _ = self.quotes.request(),
                InsightKind::FactCheck => _ = self.fact_check.request(),
            }
        }
    }

    /// `v1`: as an agent before highlights v2 (no spans, no `sides`).
    pub(crate) fn wire(&self, v1: bool) -> AiInsights {
        let mut highlights = self.highlights.wire(None);
        if v1 {
            highlights.sides = None;
            for item in &mut highlights.items {
                (item.original, item.translation) = (None, None);
            }
        }
        AiInsights { highlights: Some(highlights), quotes: Some(self.quotes.wire(None)), fact_check: Some(self.fact_check.wire(Some("model_knowledge"))) }
    }
}

/// Highlight terms per style: (original term, its translation), tried in
/// order; the first whose original term is in a block is used. English terms
/// have 1–4 words, Chinese ones 2–8 characters, as the agent requires.
const EN_TERMS: [(&str, &[(&str, &str)]); 4] = [
    ("bold", &[("Retrieval-augmented generation", "检索增强生成"), ("recurrent memory slot", "循环记忆槽")]),
    ("highlight", &[("search step", "检索步骤"), ("read before every answer", "回答前先读取")]),
    ("positive", &[("passages it relies on", "所依据的段落"), ("written once per passage", "每读一段写入一次")]),
    ("negative", &[("only to test", "仅用于检验"), ("a real experiment", "真实实验")]),
];
const ZH_TERMS: [(&str, &[(&str, &str)]); 4] = [
    ("bold", &[("大规模语言模型", "")]),
    ("highlight", &[("检索增强生成", "")]),
    ("positive", &[("研究热点", "")]),
    ("negative", &[("标点挤压", "")]),
];
/// After an emoji on both sides, to exercise byte offsets past a 4-byte character.
const EMOJI_TERM: (&str, &str) = ("emoji", "表情的显示");
const LABELS: [&str; 4] = ["要点（合成）", "关键信息（合成）", "正面（合成）", "负面（合成）"];

fn span(raw: &str, block: usize, term: &str, taken: &[TermSpan]) -> Option<TermSpan> {
    if term.is_empty() {
        return None;
    }
    raw.match_indices(term).map(|(at, _)| TermSpan { text: term.into(), block, start: at, end: at + term.len() }).find(|s| {
        !taken.iter().any(|t| t.block == block && t.start < s.end && s.start < t.end)
    })
}

/// v2 highlights: terms of the original blocks, with their translated terms
/// when a paired `translation` is given.
fn highlight_terms(markdown: &str, translation: Option<&str>, english: bool) -> Vec<HighlightItem> {
    let blocks = split_blocks(markdown);
    let translated = translation.map(|t| (t, split_blocks(t)));
    let terms = if english { &EN_TERMS } else { &ZH_TERMS };
    let mut out: Vec<HighlightItem> = vec![];
    let paragraphs = blocks.iter().enumerate().filter(|(_, b)| !b.passthrough && !markdown[b.start..b.end].starts_with('#'));
    for (p, (index, b)) in paragraphs.enumerate().take(14) {
        let raw = &markdown[b.start..b.end];
        let raw_t = translated.as_ref().and_then(|(t, tb)| tb.get(index).map(|x| &t[x.start..x.end]));
        // The first two paragraphs carry all four styles; later ones one each.
        let picks: Vec<usize> = match p {
            0 => vec![0, 1],
            1 => vec![2, 3],
            _ => vec![p % 4],
        };
        let mut wanted: Vec<(usize, &str, &str)> = vec![];
        for s in picks {
            if let Some((o, t)) = terms[s].1.iter().find(|(o, _)| raw.contains(o)) {
                wanted.push((s, o, t));
            }
        }
        if english && p == 0 && raw.contains(EMOJI_TERM.0) {
            wanted.push((1, EMOJI_TERM.0, EMOJI_TERM.1));
        }
        for (s, o, t) in wanted {
            let taken_o: Vec<TermSpan> = out.iter().filter_map(|h| h.original.clone()).collect();
            let taken_t: Vec<TermSpan> = out.iter().filter_map(|h| h.translation.clone()).collect();
            let original = span(raw, index, o, &taken_o);
            let translation = raw_t.and_then(|rt| span(rt, index, t, &taken_t));
            let Some(quote) = original.as_ref().or(translation.as_ref()).map(|x| x.text.clone()) else { continue };
            out.push(HighlightItem { block_index: index, quote, style: terms[s].0.to_string().into(), label: Some(LABELS[s].into()), original, translation });
        }
    }
    out
}

type Synth = (Vec<QuoteItem>, Vec<FactItem>);

fn synthesize(markdown: &str, english: bool) -> Synth {
    let blocks = split_blocks(markdown);
    let paragraphs: Vec<(usize, &str)> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (i, &markdown[b.start..b.end]))
        .filter(|(i, t)| !blocks[*i].passthrough && !t.starts_with('#'))
        .collect();
    let find = |phrase: &str, nth: usize| paragraphs.iter().filter(|(_, t)| t.contains(phrase)).nth(nth).map(|(i, _)| *i);
    let mut quotes = vec![];
    let mut facts = vec![];
    if english {
        for (phrase, zh, why) in [
            ("Retrieval-augmented generation pairs a language model with a search step, so that an answer can point to the passages it relies on.", "检索增强生成把语言模型与检索步骤结合起来，使回答能够指向它所依据的段落。", "（合成）一句话讲清了方法的核心。"),
            ("a recurrent memory slot that is written once per passage and read before every answer", "一个每读一段写入一次、每次回答前先读取的循环记忆槽", "（合成）把新方法压缩成一个可记住的画面。"),
            ("Nothing here reports a real experiment; it only gives the reader a long English text to lay out.", "这里没有任何真实实验，只为读者提供一篇较长的英文文本。", "（合成）坦白交代了这段文字的用途。"),
        ] {
            if let Some(i) = find(phrase, 0) {
                quotes.push(QuoteItem { block_index: i, quote: phrase.into(), translation_zh: Some(zh.into()), why: why.into() });
            }
        }
        for (phrase, nth, verdict, explanation, confidence) in [
            ("Retrieval-augmented generation pairs a language model with a search step", 0, "supported", "（合成）与检索增强生成的通常定义一致。", "high"),
            ("so that an answer can point to the passages it relies on", 1, "disputed", "（合成）检索能提供出处，但不保证回答忠实于出处。", "medium"),
            ("The text exists only to test bilingual layout", 2, "incorrect", "（合成示例）为演示「有误」样式而标注，并非真实判断。", "low"),
            ("Nothing here reports a real experiment", 0, "unverifiable", "（合成）文中没有可供核对的实验数据。", "medium"),
            ("it only gives the reader a long English text to lay out", 1, "opinion", "（合成）这是作者对文本用途的说明，属于观点。", "high"),
        ] {
            if let Some(i) = find(phrase, nth).or_else(|| find(phrase, 0)) {
                facts.push(FactItem { block_index: i, quote: phrase.into(), verdict: verdict.to_string().into(), explanation: explanation.into(), confidence: confidence.into() });
            }
        }
    } else {
        for (phrase, why) in [
            ("如何在长文档场景下有效地进行检索增强生成成为研究热点", "（合成）点明了全文要回答的问题。"),
            ("本段用于检验中文换行、标点挤压与 emoji 🌹 的显示", "（合成）交代了这段合成文字的用途。"),
        ] {
            if let Some(i) = find(phrase, 0) {
                quotes.push(QuoteItem { block_index: i, quote: phrase.into(), translation_zh: None, why: why.into() });
            }
        }
        for (phrase, nth, verdict, explanation, confidence) in [
            ("大规模语言模型在知识密集型任务中的广泛应用", 0, "supported", "（合成）与公开资料的一般描述一致。", "high"),
            ("如何在长文档场景下有效地进行检索增强生成成为研究热点", 1, "opinion", "（合成）“研究热点”是作者的判断。", "medium"),
            ("本段用于检验中文换行", 2, "unverifiable", "（合成）无法从外部核实。", "low"),
        ] {
            if let Some(i) = find(phrase, nth).or_else(|| find(phrase, 0)) {
                facts.push(FactItem { block_index: i, quote: phrase.into(), verdict: verdict.to_string().into(), explanation: explanation.into(), confidence: confidence.into() });
            }
        }
    }
    (quotes, facts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{FixtureBackend, SHOWCASE_ITEM};
    use crate::Client;

    /// Every quote is an exact substring of the agent's block it names.
    fn check(markdown: &str, ai: &AiInsights) -> (usize, usize, usize) {
        let blocks = split_blocks(markdown);
        let in_block = |i: usize, q: &str| blocks.get(i).is_some_and(|b| markdown[b.start..b.end].contains(q));
        let h = ai.highlights.as_ref().and_then(|s| s.done()).unwrap();
        let q = ai.quotes.as_ref().and_then(|s| s.done()).unwrap();
        let f = ai.fact_check.as_ref().and_then(|s| s.done()).unwrap();
        for item in h {
            assert!(in_block(item.block_index, &item.quote), "highlight {item:?}");
        }
        for item in q {
            assert!(in_block(item.block_index, &item.quote), "quote {item:?}");
        }
        for item in f {
            assert!(in_block(item.block_index, &item.quote), "fact {item:?}");
        }
        assert_eq!(ai.fact_check.as_ref().unwrap().basis.as_deref(), Some("model_knowledge"));
        (h.len(), q.len(), f.len())
    }

    #[test]
    fn showcase_insights_quote_their_blocks_and_cover_every_style_and_verdict() {
        let client = Client::new(FixtureBackend::new(30));
        let detail = client.item_detail(SHOWCASE_ITEM).unwrap();
        let ai = detail.analysis.ai_insights.clone().unwrap();
        let (h, q, f) = check(&detail.canonical.markdown, &ai);
        assert!(h >= 4 && q == 3 && f == 5);
        let styles: std::collections::BTreeSet<String> = ai.highlights.unwrap().items.into_iter().map(|i| String::from(i.style)).collect();
        assert_eq!(styles.into_iter().collect::<Vec<_>>(), ["bold", "highlight", "negative", "positive"]);
        let verdicts: Vec<String> = ai.fact_check.unwrap().items.into_iter().map(|i| String::from(i.verdict)).collect();
        assert_eq!(verdicts, ["supported", "disputed", "incorrect", "unverifiable", "opinion"]);
        assert!(ai.quotes.unwrap().items.iter().all(|q| q.translation_zh.is_some()), "an English source has Chinese translations");
    }

    #[test]
    fn chinese_insights_quote_their_blocks_without_translations() {
        let markdown = crate::fixture::markdown_for(8, "合成中文条目");
        let ai = Insights::new(&markdown, None, false, Start::Ready, false).wire(false);
        let (h, q, f) = check(&markdown, &ai);
        assert!(h >= 4 && q >= 1 && f >= 1);
        assert!(ai.quotes.unwrap().items.iter().all(|q| q.translation_zh.is_none()));
        // Not requested: nothing is shown as done.
        let waiting = Insights::new(&markdown, None, false, Start::NotRequested, false).wire(false);
        assert!(waiting.quotes.unwrap().done().is_none());
    }
}
