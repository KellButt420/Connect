//! First run: name this PC and generate the account (User-ID) before anything else.

use std::path::PathBuf;

use connect_host::VideoSource;
use connect_net::{self as net, iroh::Endpoint};
use eframe::egui::{self, RichText, TextEdit};
use tokio::runtime::Handle;

use super::{settings::Settings, theme};

pub struct Welcome {
    runtime: Handle,
    config_dir: PathBuf,
    pub settings: Settings,
    /// Set once the account exists; the next screen shows its User-ID.
    created: Option<Endpoint>,
    /// Install the built-in gamepad driver along with the account (Windows only).
    setup_gamepads: bool,
    error: Option<String>,
}

impl Welcome {
    pub fn new(runtime: Handle, config_dir: PathBuf, settings: Settings) -> Self {
        Self {
            runtime,
            config_dir,
            settings,
            created: None,
            setup_gamepads: VideoSource::SCREEN_SUPPORTED && connect_host::gamepad_support().is_err(),
            error: None,
        }
    }

    /// Once the user carries on into the app: the new account's endpoint, the settings, and
    /// whether to set up gamepad support.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<(Endpoint, Settings, bool)> {
        let mut done = false;
        egui::CentralPanel::default().frame(theme::content()).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.12).min(80.0));
                ui.set_max_width(540.0);
                match &self.created {
                    None => self.ask(ui),
                    Some(endpoint) => done = created(ui, endpoint),
                }
            });
        });
        if done {
            let endpoint = self.created.take().expect("shown above");
            return Some((endpoint, std::mem::take(&mut self.settings), self.setup_gamepads));
        }
        None
    }

    fn ask(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Welcome to Connect").size(28.0).strong().color(theme::ACCENT));
        ui.add_space(4.0);
        ui.label(RichText::new("Play on another PC as if you were sitting at it, or let a friend play on yours.").size(15.0));
        ui.add_space(18.0);

        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::section_title(ui, "Name this PC");
            ui.label(RichText::new("Shown to the people you connect with.").weak());
            ui.add(TextEdit::singleline(&mut self.settings.device_name).desired_width(f32::INFINITY));
        });
        ui.add_space(6.0);
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::section_title(ui, "Your account");
            ui.label(
                "Generating an account creates your User-ID: a key that lives only on this PC. \
                 There's no email or password.",
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Nobody can connect without your approval, and you choose what they may use: \
                     mouse, keyboard, gamepad.",
                )
                .weak(),
            );
        });
        if VideoSource::SCREEN_SUPPORTED && connect_host::gamepad_support().is_err() {
            ui.add_space(6.0);
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.checkbox(&mut self.setup_gamepads, "Set up gamepad support");
                ui.label(
                    RichText::new(
                        "Lets people who connect use their controller in your games. It's built in; \
                         Windows asks for permission once.",
                    )
                    .weak()
                    .small(),
                );
            });
        }
        ui.add_space(14.0);

        let named = !self.settings.device_name.trim().is_empty();
        let button = theme::primary("Generate Account").min_size(egui::vec2(220.0, 40.0));
        if ui.add_enabled(named, button).clicked() {
            self.settings.device_name = self.settings.device_name.trim().to_owned();
            match self.generate() {
                Ok(endpoint) => self.created = Some(endpoint),
                Err(e) => self.error = Some(format!("Couldn't create the account: {e:#}")),
            }
        }
        if let Some(error) = &self.error {
            ui.add_space(6.0);
            ui.label(RichText::new(error).color(theme::BAD));
        }
    }

    fn generate(&self) -> anyhow::Result<Endpoint> {
        let key = net::create_identity(&self.config_dir)?;
        self.runtime.block_on(net::bind_endpoint(key))
    }
}

/// The new User-ID, with a way to copy it. True when the user continues.
fn created(ui: &mut egui::Ui, endpoint: &Endpoint) -> bool {
    ui.label(RichText::new("Your account is ready").size(28.0).strong().color(theme::ACCENT));
    ui.add_space(18.0);
    theme::card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        theme::section_title(ui, "Your User-ID");
        let id = net::format_user_id(&endpoint.id());
        let groups: Vec<&str> = id.split(' ').collect();
        for row in groups.chunks(4) {
            ui.label(RichText::new(row.join(" ")).monospace().size(20.0));
        }
        ui.add_space(6.0);
        if ui.add(theme::primary("Copy User-ID")).clicked() {
            ui.ctx().copy_text(endpoint.id().to_string());
        }
        ui.add_space(6.0);
        let how = if VideoSource::SCREEN_SUPPORTED {
            "Give it to whoever should connect to this PC. To play on someone else's PC, use their User-ID."
        } else {
            "To play on someone else's PC, use their User-ID. (This PC can connect to others; hosting needs Windows.)"
        };
        ui.label(RichText::new(how).weak());
    });
    ui.add_space(14.0);
    ui.add(theme::primary("Continue").min_size(egui::vec2(220.0, 40.0))).clicked()
}
