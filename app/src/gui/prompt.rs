//! The Allow / Deny prompt for incoming connections.

use connect_host::Decision;
use connect_net as net;
use eframe::egui::{self, Align, Layout, RichText};

use super::{App, theme};

pub fn show(app: &mut App, ctx: &egui::Context) {
    let timeout = connect_proto::APPROVAL_TIMEOUT;
    let Some(request) = app.requests.front_mut() else { return };
    let remaining = timeout.saturating_sub(request.arrived.elapsed()).as_secs();

    let mut decision = None;
    egui::Modal::new(egui::Id::unique("connection-request")).frame(theme::card()).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.label(RichText::new("Connection request").size(20.0).strong());
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!("{} wants to connect to this PC and see your screen.", request.inner.client_name))
                .size(15.0),
        );
        ui.add_space(4.0);
        ui.label(RichText::new("User-ID").weak().small());
        ui.label(RichText::new(net::format_user_id(&request.inner.peer)).monospace());
        ui.label(
            RichText::new("The User-ID is verified. The name is whatever they chose, so check the ID if unsure.")
                .weak()
                .small(),
        );
        ui.add_space(6.0);
        ui.checkbox(&mut request.always_allow, "Always allow this device");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Denying automatically in {remaining} s")).weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(theme::primary("Allow")).clicked() {
                    decision = Some(Decision::Allow);
                }
                if ui.button("Deny").clicked() {
                    decision = Some(Decision::Deny);
                }
            });
        });
    });

    if let Some(decision) = decision {
        let request = app.requests.pop_front().expect("shown above");
        if decision == Decision::Allow && request.always_allow {
            app.settings.trust(request.inner.peer, &request.inner.client_name);
            app.host.set_trusted(request.inner.peer, true);
        }
        request.inner.respond(decision);
    }
}
