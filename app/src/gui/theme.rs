//! A dark look with one accent colour.

use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, RichText, Stroke, Theme};

pub const ACCENT: Color32 = Color32::from_rgb(99, 102, 241);
pub const GOOD: Color32 = Color32::from_rgb(52, 199, 89);
pub const BAD: Color32 = Color32::from_rgb(255, 99, 99);
const BG: Color32 = Color32::from_rgb(17, 18, 23);
const SIDEBAR: Color32 = Color32::from_rgb(12, 13, 17);
const CARD: Color32 = Color32::from_rgb(28, 30, 38);
const FIELD: Color32 = Color32::from_rgb(10, 11, 14);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, |style| {
        let v = &mut style.visuals;
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.extreme_bg_color = FIELD;
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = ACCENT.linear_multiply(0.6);
        v.window_corner_radius = CornerRadius::same(12);
        v.window_stroke = Stroke::new(1.0, Color32::from_gray(50));
        for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
            w.corner_radius = CornerRadius::same(8);
        }
        style.spacing.button_padding = egui::vec2(14.0, 7.0);
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    });
}

pub fn sidebar() -> Frame {
    Frame::new().fill(SIDEBAR).inner_margin(Margin::same(16))
}

pub fn content() -> Frame {
    Frame::new().fill(BG).inner_margin(Margin::symmetric(32, 24))
}

pub fn card() -> Frame {
    Frame::new().fill(CARD).corner_radius(CornerRadius::same(12)).inner_margin(Margin::same(18))
}

/// A filled call-to-action button.
pub fn primary(text: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()).fill(ACCENT)
}

pub fn page_title(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).size(26.0).strong());
    ui.label(RichText::new(subtitle).weak());
    ui.add_space(8.0);
}

pub fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).size(15.0).strong());
}
