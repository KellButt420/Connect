//! The remote screen, filling the window, with a toolbar at the top edge. While the window has
//! focus, its mouse, keyboard and gamepads drive the host, as far as the host allows.

use std::time::Instant;

use connect_proto::DeviceClass;
use eframe::egui::{self, Align, Color32, CursorGrab, Layout, Rect, RichText, Sense, TextureOptions, ViewportCommand};

use super::{App, ClientSession, ClientState, input::Hotkey, theme};
use crate::stats_line;

/// Height of the strip at the top that reveals the toolbar.
const TOOLBAR_HOVER: f32 = 48.0;
const TOOLBAR_HEIGHT: f32 = 40.0;
/// How near the window edge the hidden, captured pointer may drift before it's re-centred.
const RECENTRE_MARGIN: f32 = 80.0;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let Some(client) = &mut app.client else { return };
    let ClientState::Connected { host_name, permissions } = &client.state else { return };
    let (host_name, permissions) = (host_name.clone(), *permissions);

    if let Some(frame) = client.handle.take_frame() {
        let image = egui::ColorImage::from_rgba_unmultiplied([frame.width as usize, frame.height as usize], &frame.rgba);
        match &mut client.video {
            Some(texture) => texture.set(image, TextureOptions::LINEAR),
            None => client.video = Some(ctx.load_texture("remote-screen", image, TextureOptions::LINEAR)),
        }
    }

    let area = ui.max_rect();
    let video_rect = client.video.as_ref().map_or(area, |texture| fit(texture.size_vec2(), area));
    let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    let (events, focused, pointer) = ctx.input(|i| (i.events.clone(), i.focused, i.pointer.hover_pos()));
    let toolbar = Rect::from_min_size(area.left_top(), egui::vec2(area.width(), TOOLBAR_HEIGHT));
    let toolbar_shown = !client.input.captured
        && (client.video.is_none() || pointer.is_some_and(|p| p.y < area.top() + TOOLBAR_HOVER));

    // Input first, so it leaves with the least delay.
    let blocked = toolbar_shown.then_some(toolbar);
    for hotkey in client.input.process(&events, video_rect, blocked, pointer, permissions, &client.handle) {
        match hotkey {
            Hotkey::Fullscreen => ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen)),
            Hotkey::Stats => client.show_stats = !client.show_stats,
            Hotkey::MouseCapture => {
                let capture = !client.input.captured && permissions.allows(DeviceClass::Mouse);
                set_captured(&ctx, client, capture);
            }
        }
    }
    if client.input.captured {
        if !focused {
            set_captured(&ctx, client, false);
        } else if pointer.is_some_and(|p| !area.shrink(RECENTRE_MARGIN).contains(p)) {
            // Confined, the hidden pointer would stop at the window edge; raw motion carries on
            // regardless, but keep it clear of the edges so clicks never land outside.
            ctx.send_viewport_cmd(ViewportCommand::CursorPosition(area.center()));
        }
    }
    if let Some(gamepads) = &client.gamepads {
        gamepads.set_active(focused && permissions.allows(DeviceClass::Gamepad));
    }

    ui.painter().rect_filled(area, 0.0, Color32::BLACK);
    match &client.video {
        Some(texture) => {
            let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(texture.id(), video_rect, uv, Color32::WHITE);
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

    let stats = client.handle.stats();
    if client.show_stats {
        let pos = area.left_bottom() + egui::vec2(12.0, -12.0);
        let galley = ui.painter().layout_no_wrap(stats_line(&stats), egui::FontId::monospace(13.0), Color32::WHITE);
        let bg = Rect::from_min_size(pos - egui::vec2(0.0, galley.size().y), galley.size()).expand(6.0);
        ui.painter().rect_filled(bg, 6.0, Color32::from_black_alpha(180));
        ui.painter().galley(bg.min + egui::vec2(6.0, 6.0), galley, Color32::WHITE);
    }

    if let Some((text, until)) = &client.note {
        if Instant::now() < *until {
            let top = if toolbar_shown { toolbar.bottom() + 10.0 } else { area.top() + 14.0 };
            let galley = ui.painter().layout(text.clone(), egui::FontId::proportional(15.0), Color32::WHITE, 560.0);
            let bg = Rect::from_center_size(
                egui::pos2(area.center().x, top + galley.size().y / 2.0 + 8.0),
                galley.size() + egui::vec2(24.0, 16.0),
            );
            ui.painter().rect_filled(bg, 8.0, Color32::from_black_alpha(200));
            ui.painter().galley(bg.min + egui::vec2(12.0, 8.0), galley, Color32::WHITE);
        } else {
            client.note = None;
        }
    }

    if toolbar_shown {
        let mut disconnect = false;
        let mut capture = false;
        ui.painter().rect_filled(toolbar, 0.0, Color32::from_black_alpha(200));
        let mut bar_ui = ui.new_child(egui::UiBuilder::new().max_rect(toolbar.shrink2(egui::vec2(12.0, 6.0))));
        bar_ui.horizontal_centered(|ui| {
            ui.label(RichText::new(host_name.as_str()).strong().color(Color32::WHITE));
            ui.label(RichText::new(stats.path.label()).color(theme::ACCENT));
            if let Some(rtt) = stats.rtt {
                ui.label(RichText::new(format!("{:.0} ms", rtt.as_secs_f32() * 1000.0)).weak());
            }
            ui.separator();
            for device in DeviceClass::ALL {
                let allowed = permissions.allows(device);
                let text = RichText::new(device.label());
                let text = if allowed { text.color(theme::GOOD) } else { text.color(Color32::from_gray(110)).strikethrough() };
                ui.label(text).on_hover_text(if allowed { "The host lets you use this" } else { "The host has this turned off" });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(theme::primary("Disconnect")).clicked() {
                    disconnect = true;
                }
                let label = if fullscreen { "Exit full screen" } else { "Full screen" };
                if ui.button(label).on_hover_text("Ctrl+Alt+F").clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen));
                }
                if ui.selectable_label(client.show_stats, "Stats").on_hover_text("Ctrl+Alt+S").clicked() {
                    client.show_stats = !client.show_stats;
                }
                let mouse = permissions.allows(DeviceClass::Mouse);
                capture = ui
                    .add_enabled(mouse, egui::Button::new("Capture mouse"))
                    .on_hover_text("For games that turn the camera with the mouse. Ctrl+Alt+M releases it.")
                    .clicked();
            });
        });
        if capture {
            set_captured(&ctx, client, true);
        }
        if disconnect {
            client.handle.disconnect();
        }
    }
}

/// Captures the mouse for games (hidden, confined, raw motion) or gives it back.
pub(super) fn set_captured(ctx: &egui::Context, client: &mut ClientSession, captured: bool) {
    if client.input.captured == captured {
        return;
    }
    client.input.captured = captured;
    let grab = if captured { CursorGrab::Confined } else { CursorGrab::None };
    ctx.send_viewport_cmd(ViewportCommand::CursorGrab(grab));
    ctx.send_viewport_cmd(ViewportCommand::CursorVisible(!captured));
    if captured {
        client.note("Mouse captured. Press Ctrl+Alt+M to release it.", 4);
    }
}

/// The largest rect with `size`'s aspect ratio that fits centred in `area`.
fn fit(size: egui::Vec2, area: Rect) -> Rect {
    let scale = (area.width() / size.x).min(area.height() / size.y);
    Rect::from_center_size(area.center(), size * scale)
}
