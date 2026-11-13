//! MemeX palette (approved v4): cool porcelain-white / charcoal neutrals with a
//! single muted dusty-rose accent. No beige or warm-grey bases; no saturated blue.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

fn c(h: f32, s: f32, l: f32) -> Hsla {
    hsla(h / 360., s / 100., l / 100., 1.)
}

/// Switches GPUI Kit's mode, then overrides its colors with the MemeX palette.
pub fn apply(dark: bool, cx: &mut App) {
    Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    Theme::update(cx, |t| {
        let (bg, fg, border, muted, muted_fg, panel, rose, rose_soft, rose_fg) = if dark {
            (c(222., 9., 11.), c(220., 12., 90.), c(222., 8., 20.), c(222., 8., 16.), c(220., 7., 62.), c(222., 9., 9.), c(349., 34., 67.), c(349., 20., 21.), c(222., 9., 11.))
        } else {
            (c(220., 20., 99.), c(222., 12., 18.), c(222., 12., 90.), c(222., 14., 96.), c(222., 7., 45.), c(222., 16., 97.), c(349., 26., 58.), c(349., 38., 94.), c(0., 0., 100.))
        };
        t.background = bg;
        t.foreground = fg;
        t.border = border;
        t.muted = muted;
        t.muted_foreground = muted_fg;
        t.secondary = muted;
        t.secondary_foreground = fg;
        t.secondary_hover = border;
        t.accent = rose_soft;
        t.accent_foreground = fg;
        t.primary = rose;
        t.primary_hover = rose.opacity(0.9);
        t.primary_active = rose.opacity(0.8);
        t.primary_foreground = rose_fg;
        t.button_primary = rose;
        t.button_primary_hover = rose.opacity(0.9);
        t.button_primary_active = rose.opacity(0.8);
        t.button_primary_foreground = rose_fg;
        t.ring = rose.opacity(0.6);
        t.caret = rose;
        t.selection = rose.opacity(0.25);
        t.link = rose;
        t.switch = rose;
        t.colors.list = bg;
        t.list_even = muted.opacity(0.5);
        t.list_active = rose_soft;
        t.list_active_border = rose.opacity(0.5);
        t.list_hover = muted;
        t.table = bg;
        t.table_even = muted.opacity(0.6);
        t.table_head = panel;
        t.table_head_foreground = muted_fg;
        t.table_active = rose_soft;
        t.table_active_border = rose.opacity(0.5);
        t.table_hover = muted;
        t.table_row_border = border.opacity(0.5);
        t.sidebar = panel;
        t.sidebar_border = border;
        t.sidebar_foreground = fg;
        t.sidebar_accent = rose_soft;
        t.sidebar_accent_foreground = fg;
        t.sidebar_primary = rose;
        t.sidebar_primary_foreground = rose_fg;
        t.title_bar = panel;
        t.title_bar_border = border;
        t.tab_bar = panel;
        t.tab_active = bg;
        t.popover = bg;
        t.popover_foreground = fg;
        t.input = border;
        t.font_family = "PingFang SC".into();
    });
}
