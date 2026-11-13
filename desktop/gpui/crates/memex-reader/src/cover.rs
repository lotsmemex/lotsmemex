//! The item's cover (`item_cover_v1`) above the text: its own block in the
//! list's lead row, never a text block, so marks, selections and reading
//! positions keep the canonical Markdown's byte coordinates. Loading, a missing
//! cover and any failure show nothing: no placeholder, no broken image.
use super::*;
use std::{io::Cursor, sync::Arc};

/// A decoded, verified cover of the item on screen.
#[derive(Clone)]
pub(super) struct ShownCover {
    item_id: String,
    version: String,
    image: Arc<Image>,
    /// Width / height of the original.
    ratio: f32,
}

#[derive(Default)]
pub(super) struct CoverSlot {
    pub(super) shown: Option<ShownCover>,
    ticket: u64,
}

/// Same limits as the Store: decoded rasters only, at most 20,000,000 pixels.
fn decode(bytes: &[u8]) -> Option<(Arc<Image>, f32)> {
    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let (w, h) = reader.into_dimensions().ok()?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > 20_000_000 { return None }
    let decoded = image::load_from_memory(bytes).ok()?;
    let mut png = Cursor::new(Vec::new());
    decoded.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some((Arc::new(Image::from_bytes(ImageFormat::Png, png.into_inner())), w as f32 / h as f32))
}

impl Reader {
    /// Stops any cover read; a cover of another item than `item` is dropped
    /// (a reload of the same item keeps it until the new read answers).
    pub(super) fn cover_reset(&mut self, item: Option<&str>) {
        self.cover.ticket += 1;
        if self.cover.shown.as_ref().is_some_and(|c| Some(c.item_id.as_str()) != item) { self.cover.shown = None; }
    }

    /// Reads and decodes the item's original cover off the UI thread. Without
    /// `item_cover_v1` the client sends nothing; every failure stays silent.
    pub(super) fn cover_load(&mut self, item_id: String, cx: &mut Context<Self>) {
        self.cover.ticket += 1;
        let ticket = self.cover.ticket;
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move {
            let cover = client.cover_original(&item_id).ok().flatten()?;
            let (image, ratio) = decode(&cover.bytes)?;
            Some(ShownCover { item_id, version: cover.cover.version, image, ratio })
        });
        cx.spawn(async move |this, cx| {
            let shown = task.await;
            this.update(cx, |this, cx| {
                if this.cover.ticket != ticket { return; }
                this.cover.shown = shown.filter(|s| this.model.active_item() == Some(s.item_id.as_str()));
                this.list.remeasure_items(0..LEAD);
                cx.notify();
            }).ok();
        }).detach();
    }

    /// The list's lead row: the cover, then the AI 纪要 card.
    pub(super) fn lead_row(&self, serif: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let cover = self.cover.shown.clone().filter(|c| self.model.active_item() == Some(c.item_id.as_str()));
        let card = self.minutes_card(serif, cx);
        if cover.is_none() { return card.unwrap_or_else(|| div().into_any_element()); }
        v_flex().w_full().children(cover.map(|c| cover_element(&c, cx.theme().background.l < 0.5))).children(card).into_any_element()
    }

    /// Development and tests: the version and aspect ratio of the cover on screen.
    pub fn dev_cover(&self) -> Option<(String, f32)> {
        self.cover.shown.as_ref().map(|c| (c.version.clone(), c.ratio))
    }

    /// Development and tests: rows of the reading list, the lead row included.
    pub fn dev_list_len(&self) -> usize { self.list.item_count() }

    /// Development and tests: what the renderer's source map makes of a
    /// selection of `term` in the first original block that shows it.
    pub fn dev_selection_source(&self, term: &str) -> Option<Range<usize>> {
        let markdown = &self.model.detail()?.canonical.markdown;
        self.model.original().iter().find_map(|b| {
            let at = b.text.find(term)?;
            let raw = markdown.get(b.start..b.end)?;
            crate::selection::source_bytes(raw, b.start, b.skip, b.kind == BlockKind::Code, at..at + term.len())
        })
    }
}

/// A champagne mat with soft corners around the picture, in its own proportion,
/// never wider than the text column nor taller than `MAX_COVER_HEIGHT`.
const MAX_COVER_HEIGHT: f32 = 420.;

fn cover_element(c: &ShownCover, dark: bool) -> AnyElement {
    let (mat, edge, shadow) = if dark {
        (hsla(24. / 360., 0.14, 0.17, 1.), hsla(30. / 360., 0.22, 0.30, 1.), hsla(0., 0., 0., 0.35))
    } else {
        (hsla(32. / 360., 0.40, 0.97, 1.), hsla(28. / 360., 0.32, 0.86, 1.), hsla(350. / 360., 0.30, 0.45, 0.12))
    };
    // The picture fills a box of its own ratio: an image's intrinsic height
    // would otherwise win over a relative width.
    let picture = div().relative().w_full().aspect_ratio(c.ratio)
        .child(img(c.image.clone()).absolute().top_0().left_0().size_full().rounded(px(12.)).object_fit(ObjectFit::Cover).when(dark, |i| i.opacity(0.9)));
    div().w_full().pb_5().child(
        div()
            .id("reader-cover")
            .w_full()
            .max_w(px(MAX_COVER_HEIGHT * c.ratio + 14.))
            .p(px(6.))
            .rounded(px(18.))
            .bg(mat)
            .border_1()
            .border_color(edge)
            .shadow(vec![BoxShadow { color: shadow, offset: point(px(0.), px(8.)), blur_radius: px(22.), spread_radius: px(0.), inset: false }])
            .child(picture),
    ).into_any_element()
}
