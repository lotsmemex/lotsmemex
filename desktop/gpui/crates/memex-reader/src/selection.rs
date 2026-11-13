//! Styled Markdown participates in GPUI's window selection without replacing
//! links, AI styling or the virtualized reader with a second Markdown renderer.
use gpui_kit::*;
use gpui_kit::base::{TextSelection, TextSelectionHandle, TextSelectionRegistration, TextSelectionRun};
use std::{cell::RefCell, collections::BTreeMap, ops::Range, rc::Rc};

#[derive(Clone)]
pub struct SelectedPart {
    pub source: Option<Range<usize>>,
    pub translation: bool,
    /// The selection is in an AI chapter translation: `source` is then a byte
    /// range of that chapter's complete AI translation Markdown.
    pub ai: bool,
    pub text: String,
    pub order: u64,
    /// Window position of the selection's first character (the 浮动条 sits above it).
    pub at: Option<Point<Pixels>>,
}
pub type Selection = Rc<RefCell<BTreeMap<String, SelectedPart>>>;

#[cfg(feature = "dev-snapshot")]
thread_local! { static GEOMETRY: RefCell<BTreeMap<String, (SharedString, TextLayout)>> = RefCell::new(BTreeMap::new()); }
#[cfg(feature = "dev-snapshot")]
pub(crate) fn selection_points(term: &str) -> Option<(Point<Pixels>, Point<Pixels>)> {
    GEOMETRY.with(|all| all.borrow().values().find_map(|(text, layout)| {
        let start = text.rfind(term)?;
        let mut a = layout.position_for_index(start)?;
        let mut b = layout.position_for_index(start + term.len())?;
        a.y += layout.line_height() / 2.; b.y += layout.line_height() / 2.;
        Some((a, b))
    }))
}

pub struct SelectableRich {
    pub id: String,
    pub text: SharedString,
    pub interactive: InteractiveText,
    pub layout: TextLayout,
    pub source: String,
    pub block_start: usize,
    pub skip: usize,
    pub shown_start: usize,
    pub code: bool,
    pub translation: bool,
    pub ai: bool,
    /// A display-only replacement of the speaker label: its range in the shown
    /// `text` and the length of the label it stands for.
    pub relabel: Option<(Range<usize>, usize)>,
    pub order: u64,
    pub selected: Selection,
}

/// Where shown position `p` lands once `label` (shown bytes) is displayed as
/// `new_len` bytes; a position inside the label moves to its end.
pub(crate) fn shown_to_display(p: usize, label: &Range<usize>, new_len: usize) -> usize {
    if p <= label.start { p } else if p >= label.end { p - label.len() + new_len } else { label.start + new_len }
}

/// A displayed selection back in shown coordinates, around a relabelled speaker
/// (`label`: its displayed range, `orig_len`: the length of the real label). The
/// replaced name maps to no source bytes: a selection reaching into it starts
/// after it, one inside it maps to nothing.
pub(crate) fn display_to_shown(range: Range<usize>, label: &Range<usize>, orig_len: usize) -> Option<Range<usize>> {
    let back = |p: usize| if p <= label.start { p } else if p >= label.end { p - label.len() + orig_len } else { label.start + orig_len };
    let start = if range.start < label.end && range.end > label.start { range.start.max(label.end) } else { range.start };
    (start < range.end).then(|| back(start)..back(range.end))
}

/// The source map of one rendered block: shown bytes `shown` of the block whose
/// raw Markdown `raw` starts at byte `block_start` of its document, as a byte
/// range of that document (`None` when it maps to no source bytes).
pub(crate) fn source_bytes(raw: &str, block_start: usize, skip: usize, code: bool, shown: Range<usize>) -> Option<Range<usize>> {
    let r = if code { Some(shown) } else { crate::inline::Inline::parse(raw, skip).source_range(raw, shown) }?;
    Some(block_start + r.start..block_start + r.end)
}

impl IntoElement for SelectableRich { type Element = Self; fn into_element(self) -> Self { self } }
impl Element for SelectableRich {
    type RequestLayoutState = (TextSelectionHandle, <InteractiveText as Element>::RequestLayoutState);
    type PrepaintState = <InteractiveText as Element>::PrepaintState;
    fn id(&self) -> Option<ElementId> { Some(ElementId::Name(self.id.clone().into())) }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> { None }
    fn request_layout(&mut self, id: Option<&GlobalElementId>, inspector: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        let handle = window.with_element_state(id.unwrap(), |retained: Option<TextSelectionHandle>, _| {
            let handle = retained.unwrap_or_else(|| TextSelectionHandle::new(self.text.clone(), cx));
            (handle.clone(), handle)
        });
        let (layout, state) = self.interactive.request_layout(id, inspector, window, cx);
        (layout, (handle, state))
    }
    fn prepaint(&mut self, id: Option<&GlobalElementId>, inspector: Option<&InspectorElementId>, bounds: Bounds<Pixels>, state: &mut Self::RequestLayoutState, window: &mut Window, cx: &mut App) -> Self::PrepaintState {
        let hitbox = self.interactive.prepaint(id, inspector, bounds, &mut state.1, window, cx);
        state.0.register(TextSelectionRegistration::new(hitbox.clone(), bounds).with_document_order(self.order)
            .with_text_bounds(vec![bounds]).with_rendered_element(&state.0, window, cx), window, cx);
        hitbox
    }
    fn paint(&mut self, id: Option<&GlobalElementId>, inspector: Option<&InspectorElementId>, bounds: Bounds<Pixels>, state: &mut Self::RequestLayoutState, hitbox: &mut Self::PrepaintState, window: &mut Window, cx: &mut App) {
        #[cfg(feature = "dev-snapshot")]
        GEOMETRY.with(|all| { all.borrow_mut().insert(self.id.clone(), (self.text.clone(), self.layout.clone())); });
        let projection = state.0.update_runs(&[TextSelectionRun::new(self.text.clone(), self.layout.clone(), bounds).with_document_order(self.order)], cx);
        self.interactive.paint(id, inspector, bounds, &mut state.1, hitbox, window, cx);
        let selected = self.selected.clone();
        let hovered = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, _| {
            if phase.capture() && event.button == MouseButton::Left && hovered.is_hovered(window) { selected.borrow_mut().clear(); }
        });
        if let Some(range) = projection.ranges().first().and_then(Clone::clone) {
            let local = match &self.relabel { Some((label, n)) => display_to_shown(range.clone(), label, *n), None => Some(range.clone()) };
            let source = local.and_then(|r| source_bytes(&self.source, self.block_start, self.skip, self.code, self.shown_start + r.start..self.shown_start + r.end));
            let at = self.layout.position_for_index(range.start);
            self.selected.borrow_mut().insert(self.id.clone(), SelectedPart { source, translation: self.translation, ai: self.ai, text: self.text[range.clone()].to_string(), order: self.order, at });
            if let (Some(start), Some(end)) = (self.layout.position_for_index(range.start), self.layout.position_for_index(range.end)) {
                let bounds = self.layout.bounds();
                let height = self.layout.line_height();
                let mut y = start.y;
                while y <= end.y {
                    let left = if y == start.y { start.x } else { bounds.left() };
                    let right = if y == end.y { end.x } else { bounds.right() };
                    window.paint_quad(fill(Bounds::from_corners(point(left, y), point(right, y + height)), hsla(0.59, 0.7, 0.55, 0.23)));
                    y += height;
                }
            }
        } else if TextSelection::has_selection(window, cx) {
            self.selected.borrow_mut().remove(&self.id);
        }
    }
}
