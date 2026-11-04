//! Local controller testing and staged layout edits. Nothing here sends remote input.
use super::{
    ControllerManager, ControllerView,
    mapping::{BITS, ControllerProfile, LABELS, SavedProfiles, Source},
};
use crate::gui::theme;
use connect_proto::input::GamepadState;
use eframe::egui::{self, Ui};

#[derive(Default)]
pub struct Editor {
    selected: Option<usize>,
    draft: Option<ControllerProfile>,
    original: Option<ControllerProfile>,
    reset: bool,
    binding: Option<(usize, u64)>,
    pending: Option<Source>,
    notice: String,
    preview: GamepadState,
}

impl Editor {
    fn select(&mut self, pad: &ControllerView, saved: &SavedProfiles) {
        self.selected = Some(pad.id);
        self.draft = Some(
            saved
                .find(&pad.automatic_profile.key)
                .unwrap_or_else(|| pad.automatic_profile.clone()),
        );
        self.original = self.draft.clone();
        self.reset = false;
        self.binding = None;
        self.pending = None;
        self.preview = GamepadState::NEUTRAL;
    }
    fn dirty(&self) -> bool {
        self.reset || self.draft != self.original
    }
}

pub fn show(
    ui: &mut Ui,
    manager: &ControllerManager,
    saved: &mut SavedProfiles,
    editor: &mut Editor,
) {
    let snapshot = manager.snapshot();
    theme::section_title(ui, "Gamepads");
    ui.label(theme::caption("USB controllers on this PC. Test and remap before connecting; changes apply only after Apply. The host receives an Xbox controller."));
    for error in saved.errors() {
        ui.colored_label(
            theme::BAD,
            format!("A saved controller profile was ignored: {error}"),
        );
    }
    if let Some(error) = snapshot.error {
        ui.colored_label(theme::BAD, error);
        return;
    }
    if editor
        .selected
        .is_some_and(|id| !snapshot.devices.iter().any(|p| p.id == id))
    {
        *editor = Editor {
            notice: "Controller disconnected; unsaved edits were discarded.".into(),
            ..Default::default()
        };
        manager.cancel_capture();
    }
    if snapshot.devices.is_empty() {
        ui.label("No controller detected. Connect a USB controller.");
        return;
    }
    if editor.selected.is_none() {
        editor.select(&snapshot.devices[0], saved);
    }
    let mut selected = editor.selected.unwrap();
    ui.add_enabled_ui(!editor.dirty() && editor.binding.is_none(), |ui| {
        egui::ComboBox::from_id_salt("usb-controller")
            .selected_text(
                snapshot
                    .devices
                    .iter()
                    .find(|p| p.id == selected)
                    .map_or("Controller", |p| p.name.as_str()),
            )
            .show_ui(ui, |ui| {
                for pad in &snapshot.devices {
                    ui.selectable_value(
                        &mut selected,
                        pad.id,
                        format!("{} (device {})", pad.name, pad.id + 1),
                    );
                }
            });
    });
    let pad = snapshot.devices.iter().find(|p| p.id == selected).unwrap();
    if editor.selected != Some(selected) {
        manager.cancel_capture();
        editor.select(pad, saved);
    }
    ui.label(if pad.custom {
        "Saved custom layout"
    } else {
        "Automatic layout"
    });
    if let Some(slot) = pad.slot {
        ui.label(format!("Forwarding slot {}", slot + 1));
    }
    if snapshot
        .devices
        .iter()
        .position(|p| p.id == pad.id)
        .is_some_and(|i| i >= 4)
        && pad.slot.is_none()
    {
        ui.label("Only four controllers can be forwarded. This controller waits for a free slot.");
    }
    if editor.dirty() {
        ui.label("Unsaved changes — Apply or Cancel before selecting another controller.");
    }

    if let Some((target, token)) = editor.binding
        && let Some(capture) = snapshot
            .capture
            .as_ref()
            .filter(|c| c.id == pad.id && c.target == target && c.token == token)
    {
        ui.label(format!("{}: {}", LABELS[target], capture.message));
        if let Some(source) = capture.result {
            editor.pending = Some(source);
        }
    }
    if let (Some((target, _)), Some(source)) = (editor.binding, editor.pending) {
        let conflicts = editor.draft.as_ref().unwrap().conflicts(target, source);
        if conflicts.is_empty() {
            editor.draft.as_mut().unwrap().bind(target, Some(source));
            editor.binding = None;
            editor.pending = None;
            manager.cancel_capture();
        } else {
            ui.label(format!(
                "That input is assigned to {}. Reassigning clears those bindings.",
                conflicts
                    .iter()
                    .map(|&i| LABELS[i])
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            if ui.button("Reassign and clear previous bindings").clicked() {
                editor.draft.as_mut().unwrap().bind(target, Some(source));
                editor.binding = None;
                editor.pending = None;
                manager.cancel_capture();
            }
        }
    }
    if editor.binding.is_some() && ui.button("Cancel rebind").clicked() {
        manager.cancel_capture();
        editor.binding = None;
        editor.pending = None;
    }
    let profile = editor.draft.as_ref().unwrap();
    editor.preview = if editor.reset {
        pad.automatic
    } else {
        profile.map(&pad.input, editor.preview, pad.automatic)
    };
    ui.add_space(6.0);
    ui.label("Xbox output preview");
    ui.horizontal_wrapped(|ui| {
        for (i, bit) in BITS.iter().enumerate() {
            ui.label(format!(
                "{} {}",
                LABELS[i],
                if editor.preview.buttons & bit != 0 {
                    "[pressed]"
                } else {
                    "[released]"
                }
            ));
        }
    });
    ui.add(
        egui::ProgressBar::new(editor.preview.left_trigger as f32 / 255.0)
            .text(format!("LT: {} / 255", editor.preview.left_trigger)),
    );
    ui.add(
        egui::ProgressBar::new(editor.preview.right_trigger as f32 / 255.0)
            .text(format!("RT: {} / 255", editor.preview.right_trigger)),
    );
    ui.label(format!(
        "Sticks: left ({}, {}), right ({}, {})",
        editor.preview.left_x,
        editor.preview.left_y,
        editor.preview.right_x,
        editor.preview.right_y
    ));
    ui.label(theme::caption("For axis-backed controls, move sticks/triggers once, release them, then choose Rebind. Pull triggers fully and release to capture their range."));
    egui::Grid::new("usb-controller-bindings")
        .num_columns(4)
        .max_col_width(140.0)
        .striped(true)
        .show(ui, |ui| {
            for (target, label) in LABELS.iter().enumerate() {
                ui.label(*label);
                ui.add(
                    egui::Label::new(
                        editor.draft.as_ref().unwrap().bindings[target]
                            .map_or("Unbound".into(), Source::label),
                    )
                    .wrap(),
                );
                if ui
                    .add_enabled(editor.binding.is_none(), theme::secondary("Rebind").small())
                    .clicked()
                {
                    editor.reset = false;
                    editor.binding = Some((target, manager.capture(pad.id, target)));
                    editor.pending = None;
                }
                if ui
                    .add_enabled(editor.binding.is_none(), theme::secondary("Unbind").small())
                    .clicked()
                {
                    editor.reset = false;
                    editor.draft.as_mut().unwrap().bind(target, None);
                }
                ui.end_row();
            }
        });
    if let Err(error) = editor.draft.as_ref().unwrap().validate() {
        ui.colored_label(theme::BAD, error);
    }
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                editor.dirty() && editor.binding.is_none(),
                theme::primary("Apply"),
            )
            .clicked()
        {
            let profile = editor.draft.as_ref().unwrap();
            if editor.reset || profile.validate().is_ok() {
                saved.set(
                    profile.key.clone(),
                    (!editor.reset).then(|| profile.clone()),
                );
                manager.profiles(saved.clone());
                editor.select(pad, saved);
                editor.notice = "Controller layout saved.".into();
            }
        }
        if ui
            .add_enabled(
                editor.dirty() || editor.binding.is_some(),
                theme::secondary("Cancel"),
            )
            .clicked()
        {
            manager.cancel_capture();
            editor.select(pad, saved);
            editor.notice = "Edits cancelled.".into();
        }
        if ui.button("Reset to automatic").clicked() {
            manager.cancel_capture();
            editor.draft = Some(pad.automatic_profile.clone());
            editor.reset = true;
            editor.binding = None;
            editor.pending = None;
        }
    });
    if !editor.notice.is_empty() {
        ui.label(&editor.notice);
    }
    egui::CollapsingHeader::new("Controller diagnostics").show(ui, |ui| {
        let key = &pad.automatic_profile.key;
        ui.label(format!(
            "Platform: {}; VID/PID: {:?}/{:?}; UUID: {:02x?}",
            key.platform, key.vendor, key.product, key.uuid
        ));
        ui.label(format!("Last source event: {}", pad.last_event));
        for (code, value) in &pad.input.buttons {
            ui.monospace(format!(
                "Button {code:#x}: pressed={}, value={:.3}",
                value.pressed, value.value
            ));
        }
        for (code, value) in &pad.input.axes {
            ui.monospace(format!("Axis {code:#x}: {value:.3}"));
        }
    });
}
