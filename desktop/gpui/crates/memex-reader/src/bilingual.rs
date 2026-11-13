//! Reading modes and the Bi-Translation style 对照 layout.
//!
//! The agent's translated document keeps the original's blank-line blocks in the
//! same order, with code, images and URLs passed through unchanged. 对照 pairs
//! blocks by index: each original block is followed by its translation, and a
//! passthrough block is shown once. When the counts differ the pairing cannot be
//! trusted, so the layout falls back to the translation alone and says so instead
//! of putting a translation under the wrong paragraph.

use crate::model::{Block, BlockKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    Original,
    /// 对照: original and translation interleaved block by block.
    #[default]
    Bilingual,
    Translation,
}

/// One rendered row; indexes point into the original or translation blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Original(usize),
    Translation(usize),
    /// Original block `i` followed by translation block `i`.
    Pair(usize),
    /// A passthrough block (code, image, URL, unchanged text), shown once.
    Shared(usize),
    /// Row `i` of a book chapter's paragraph index (`ReaderModel::aligned`).
    Aligned(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub rows: Vec<Row>,
    /// What is actually shown (对照 falls back to 译文 on a block mismatch).
    pub shown: Mode,
    /// 对照 was asked for but the block counts differ.
    pub mismatch: bool,
    /// The chosen mode needs a translation and there is none.
    pub missing_translation: bool,
}

/// A block the translation carries over unchanged.
pub fn is_passthrough(original: &Block, translation: &Block) -> bool {
    original.text == translation.text || (original.kind == translation.kind && matches!(original.kind, BlockKind::Image | BlockKind::Code))
}

/// Index pairs, or `None` when the block counts differ.
pub fn pair(original: &[Block], translation: &[Block]) -> Option<Vec<Row>> {
    if original.len() != translation.len() {
        return None;
    }
    Some(original.iter().zip(translation).enumerate().map(|(i, (o, t))| if is_passthrough(o, t) { Row::Shared(i) } else { Row::Pair(i) }).collect())
}

pub fn layout(mode: Mode, original: &[Block], translation: &[Block]) -> Layout {
    let originals = || (0..original.len()).map(Row::Original).collect();
    let translations = || (0..translation.len()).map(Row::Translation).collect();
    let missing = translation.is_empty();
    match mode {
        Mode::Original => Layout { rows: originals(), shown: Mode::Original, mismatch: false, missing_translation: false },
        // The original stays readable while 对照 waits for a translation.
        Mode::Bilingual if missing => Layout { rows: originals(), shown: Mode::Original, mismatch: false, missing_translation: true },
        Mode::Bilingual => match pair(original, translation) {
            Some(rows) => Layout { rows, shown: Mode::Bilingual, mismatch: false, missing_translation: false },
            None => Layout { rows: translations(), shown: Mode::Translation, mismatch: true, missing_translation: false },
        },
        Mode::Translation => Layout { rows: translations(), shown: Mode::Translation, mismatch: false, missing_translation: missing },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::blocks;

    const EN: &str = "# Title\n\nFirst paragraph.\n\n```text\nquery -> answer\n```\n\n![Figure 1](a.png)\n\nhttps://example.invalid/x\n\nLast paragraph.\n";
    const ZH: &str = "# 标题\n\n第一段。\n\n```text\nquery -> answer\n```\n\n![Figure 1](a.png)\n\nhttps://example.invalid/x\n\n最后一段。\n";

    #[test]
    fn blocks_pair_by_index_and_passthrough_blocks_show_once() {
        let (o, t) = (blocks(EN), blocks(ZH));
        let l = layout(Mode::Bilingual, &o, &t);
        assert_eq!((l.shown, l.mismatch), (Mode::Bilingual, false));
        assert_eq!(l.rows, vec![Row::Pair(0), Row::Pair(1), Row::Shared(2), Row::Shared(3), Row::Shared(4), Row::Pair(5)]);
        assert_eq!(o[2].kind, BlockKind::Code);
        // Each pair puts the translation of the same block right after it.
        let Row::Pair(i) = l.rows[5] else { unreachable!() };
        assert_eq!((o[i].text.as_str(), t[i].text.as_str()), ("Last paragraph.", "最后一段。"));
    }

    #[test]
    fn a_block_count_mismatch_shows_the_translation_instead_of_mispairing() {
        let o = blocks(EN);
        let t = blocks(&ZH.replace("\n\n最后一段。\n", "\n"));
        assert_eq!(pair(&o, &t), None);
        let l = layout(Mode::Bilingual, &o, &t);
        assert_eq!((l.shown, l.mismatch), (Mode::Translation, true));
        assert_eq!(l.rows, (0..t.len()).map(Row::Translation).collect::<Vec<_>>());
    }

    #[test]
    fn modes_without_a_translation_keep_the_original_or_report_it_missing() {
        let o = blocks(EN);
        let l = layout(Mode::Bilingual, &o, &[]);
        assert!(l.missing_translation && l.shown == Mode::Original && l.rows.len() == o.len());
        let l = layout(Mode::Translation, &o, &[]);
        assert!(l.missing_translation && l.rows.is_empty());
        assert_eq!(layout(Mode::Original, &o, &blocks(ZH)).rows, (0..o.len()).map(Row::Original).collect::<Vec<_>>());
    }
}
