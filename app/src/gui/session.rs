//! The remote screen, filling the window, with a toolbar at the top edge.

use eframe::egui::{self, Align, Color32, Key, Layout, Rect, RichText, Sense, TextureOptions, ViewportCommand};

use super::{App, ClientState, theme};
use crate::stats_line;

/// Height of the strip at the top that reveals the toolbar.
const TOOLBAR_HOVER: f32 = 48.0;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let Some(client) = &mut app.client else { return };
    let ClientState::Connected { host_name, .. } = &client.state else { return };

    if let Some(frame) = client.handle.take_frame() {
        let image = egui::ColorImage::from_rgba_unmultiplied([frame.width as usize, frame.height as usize], &frame.rgba);
        match &mut client.video {
            Some(texture) => texture.set(image, TextureOptions::LINEAR),
            None => client.video = Some(ctx.load_texture("remote-screen", image, TextureOptions::LINEAR)),
        }
    }

    let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    if ctx.input(|i| i.key_pressed(Key::F11)) {
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen));
    }
    if ctx.input(|i| i.key_pressed(Key::F10)) {
        client.show_stats = !client.show_stats;
    }

    let area = ui.max_rect();
    ui.painter().rect_filled(area, 0.0, Color32::BLACK);
    match &client.video {
        Some(texture) => {
            let rect = fit(texture.size_vec2(), area);
            let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
        }
        None => {
            ui.painter().text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                "Waiting for video…",
                egui::FontId::proportional(18.0),
                Color32::from_gray(160),
            );
        }
    }
    // Claim the area so clicks on the video don't fall through to anything else.
    ui.allocate_rect(area, Sense::hover());

    let pointer_near_top = ctx.input(|i| i.pointer.hover_pos().is_some_and(|p| p.y < area.top() + TOOLBAR_HOVER));
    let stats = client.handle.stats();
    if client.show_stats {
        let pos = area.left_bottom() + egui::vec2(12.0, -12.0);
        let galley = ui.painter().layout_no_wrap(stats_line(&stats), egui::FontId::monospace(13.0), Color32::WHITE);
        let bg = Rect::from_min_size(pos - egui::vec2(0.0, galley.size().y), galley.size()).expand(6.0);
        ui.painter().rect_filled(bg, 6.0, Color32::from_black_alpha(180));
        ui.painter().galley(bg.min + egui::vec2(6.0, 6.0), galley, Color32::WHITE);
    }

    if pointer_near_top || client.video.is_none() {
        let mut disconnect = false;
        let bar = Rect::from_min_size(area.left_top(), egui::vec2(area.width(), 40.0));
        ui.painter().rect_filled(bar, 0.0, Color32::from_black_alpha(200));
        let mut bar_ui = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(egui::vec2(12.0, 6.0))));
        bar_ui.horizontal_centered(|ui| {
            ui.label(RichText::new(host_name.as_str()).strong().color(Color32::WHITE));
            ui.label(RichText::new(stats.path.label()).color(theme::ACCENT));
            if let Some(rtt) = stats.rtt {
                ui.label(RichText::new(format!("{:.0} ms", rtt.as_secs_f32() * 1000.0)).weak());
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(theme::primary("Disconnect")).clicked() {
                    disconnect = true;
                }
                let label = if fullscreen { "Exit full screen (F11)" } else { "Full screen (F11)" };
                if ui.button(label).clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen));
                }
                if ui.selectable_label(client.show_stats, "Stats (F10)").clicked() {
                    client.show_stats = !client.show_stats;
                }
            });
        });
        if disconnect {
            client.handle.disconnect();
        }
    }
}

/// The largest rect with `size`'s aspect ratio that fits centred in `area`.
fn fit(size: egui::Vec2, area: Rect) -> Rect {
    let scale = (area.width() / size.x).min(area.height() / size.y);
    Rect::from_center_size(area.center(), size * scale)
}
