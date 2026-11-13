//! The AI conversation's own surface: a rose-tinted, slightly raised tone with a
//! rose rule at the split edge, so the article (porcelain / charcoal) and the
//! chat never read as one page. One table for the pane, its header strip, the
//! bubbles and the composer zone; the phone's chat drawer mirrors these values.

use gpui_kit::*;

fn c(h: f32, s: f32, l: f32, a: f32) -> Hsla {
    hsla(h / 360., s / 100., l / 100., a)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChatSurface {
    /// Pane and composer-zone background.
    pub surface: Hsla,
    /// Header strip 「对话 · n」.
    pub header: Hsla,
    /// 1 px rule where the chat meets the article (pane top, composer top, divider line).
    pub edge: Hsla,
    /// Top colour of the 8 px inset shade under the header (fades to transparent).
    pub inset: Hsla,
    /// MemeX's reply bubble and its hairline.
    pub reply: Hsla,
    pub reply_border: Hsla,
    /// The user's question bubble.
    pub question: Hsla,
    /// The composer field on the surface, and its idle border.
    pub field: Hsla,
    pub field_border: Hsla,
}

pub const BUBBLE_RADIUS: f32 = 10.;
pub const FIELD_RADIUS: f32 = 12.;
pub const INSET_HEIGHT: f32 = 8.;

pub fn chat_surface(dark: bool) -> ChatSurface {
    if dark {
        ChatSurface {
            surface: c(345., 12., 14.5, 1.),
            header: c(345., 13., 18., 1.),
            edge: c(349., 24., 34., 1.),
            inset: c(0., 0., 0., 0.28),
            reply: c(345., 9., 19., 1.),
            reply_border: c(345., 10., 25., 1.),
            question: c(349., 22., 25., 1.),
            field: c(222., 9., 10., 1.),
            field_border: c(345., 10., 28., 1.),
        }
    } else {
        ChatSurface {
            surface: c(349., 33., 97., 1.),
            header: c(349., 30., 94., 1.),
            edge: c(349., 26., 80., 1.),
            inset: c(349., 30., 40., 0.08),
            reply: c(0., 0., 100., 1.),
            reply_border: c(349., 22., 90., 1.),
            question: c(349., 38., 91., 1.),
            field: c(0., 0., 100., 1.),
            field_border: c(349., 20., 86., 1.),
        }
    }
}

/// Dark when the active theme's background is dark (the MemeX palette's rule).
pub fn for_theme(background: Hsla) -> ChatSurface {
    chat_surface(background.l < 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chat must stand apart from the reader background in both themes:
    /// a different tone and a rule darker (light) / lighter (dark) than the surface.
    #[::core::prelude::v1::test]
    fn the_chat_surface_differs_from_the_reader_in_both_themes() {
        let reader_light = c(220., 20., 99., 1.);
        let reader_dark = c(222., 9., 11., 1.);
        let light = chat_surface(false);
        let dark = chat_surface(true);
        assert!(light.surface.l < reader_light.l && light.surface.h != reader_light.h);
        assert!(dark.surface.l > reader_dark.l && dark.surface.h != reader_dark.h);
        assert!(light.edge.l < light.header.l && light.header.l < light.surface.l);
        assert!(dark.edge.l > dark.header.l && dark.header.l > dark.surface.l);
    }
}
