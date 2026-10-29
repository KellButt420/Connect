//! The desktop app: Share / Connect / Settings pages, the Allow/Deny prompt, and the session view.

mod pages;
mod prompt;
mod session;
mod settings;
mod theme;

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use connect_client::{ClientEvent, ClientHandle, Stage};
use connect_host::{ConnectionRequest, HostConfig, HostEvent, HostHandle, VideoSource};
use connect_net::{
    self as net,
    iroh::{Endpoint, EndpointId, SecretKey},
};
use connect_proto::Permissions;
use eframe::egui::{self, ViewportCommand};
use tokio::{runtime::Runtime, sync::mpsc};

use settings::Settings;

pub fn run(runtime: Runtime, key: SecretKey) -> Result<()> {
    // Lets the UI thread start tasks on the runtime.
    let _guard = runtime.enter();
    let endpoint = runtime.block_on(net::bind_endpoint(key))?;
    // wgpu (Direct3D 12 / Vulkan / Metal) normally; plain OpenGL for machines where that fails,
    // such as old GPUs and some VMs.
    let renderer = match std::env::var("CONNECT_RENDERER").as_deref() {
        Ok("glow" | "opengl") => eframe::Renderer::Glow,
        _ => eframe::Renderer::Wgpu,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Connect")
            .with_inner_size([980.0, 640.0])
            .with_min_inner_size([760.0, 520.0]),
        renderer,
        ..Default::default()
    };
    eframe::run_native("Connect", options, Box::new(|cc| Ok(Box::new(App::new(cc, endpoint)))))
        .map_err(|e| anyhow::anyhow!("couldn't open the window: {e}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Share,
    Connect,
    Settings,
}

pub struct App {
    endpoint: Endpoint,
    online: Arc<AtomicBool>,
    settings: Settings,
    host: HostHandle,
    host_events: mpsc::UnboundedReceiver<HostEvent>,
    /// Connection requests waiting for Allow/Deny, oldest first.
    requests: VecDeque<Request>,
    /// Who is viewing this PC right now.
    hosting: Option<Hosting>,
    page: Page,
    connect_input: String,
    client: Option<ClientSession>,
    /// The latest thing worth telling the user, such as why a session ended.
    notice: Option<Notice>,
}

struct Request {
    inner: ConnectionRequest,
    arrived: Instant,
    always_allow: bool,
}

struct Hosting {
    peer: EndpointId,
    client_name: String,
    since: Instant,
}

struct Notice {
    text: String,
    is_error: bool,
}

/// This PC connecting to another one.
struct ClientSession {
    handle: ClientHandle,
    events: mpsc::Receiver<ClientEvent>,
    host_id: EndpointId,
    started: Instant,
    state: ClientState,
    video: Option<egui::TextureHandle>,
    show_stats: bool,
}

enum ClientState {
    Connecting,
    WaitingForApproval,
    Connected { host_name: String, permissions: Permissions },
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, endpoint: Endpoint) -> Self {
        theme::apply(&cc.egui_ctx);
        let settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default();

        let online = Arc::new(AtomicBool::new(false));
        tokio::spawn({
            let (endpoint, online, ctx) = (endpoint.clone(), online.clone(), cc.egui_ctx.clone());
            async move {
                endpoint.online().await;
                online.store(true, Ordering::Relaxed);
                ctx.request_repaint();
            }
        });

        let (host, events) = connect_host::start(
            endpoint.clone(),
            HostConfig {
                accepting: settings.accepting,
                permissions: settings.permissions,
                trusted: settings.trusted_ids(),
                ..HostConfig::new(settings.device_name.clone())
            },
        );

        Self {
            endpoint,
            online,
            settings,
            host,
            host_events: forward_host_events(events, cc.egui_ctx.clone()),
            requests: VecDeque::new(),
            hosting: None,
            // Only Windows can host; elsewhere the host just turns callers away with the reason.
            page: if VideoSource::SCREEN_SUPPORTED { Page::Share } else { Page::Connect },
            connect_input: String::new(),
            client: None,
            notice: None,
        }
    }

    fn notify(&mut self, text: impl Into<String>, is_error: bool) {
        self.notice = Some(Notice { text: text.into(), is_error });
    }

    fn on_host_event(&mut self, event: HostEvent) {
        match event {
            HostEvent::Request(inner) => {
                self.requests.push_back(Request { inner, arrived: Instant::now(), always_allow: false })
            }
            HostEvent::SessionStarted { peer, client_name } => {
                self.hosting = Some(Hosting { peer, client_name, since: Instant::now() });
            }
            HostEvent::SessionEnded { reason, .. } => {
                self.hosting = None;
                self.notify(reason, false);
            }
        }
    }

    fn start_connect(&mut self, host_id: EndpointId, ctx: &egui::Context) {
        if self.client.is_some() {
            return;
        }
        let ctx = ctx.clone();
        let (handle, events) = connect_client::connect(
            self.endpoint.clone(),
            host_id,
            self.settings.device_name.clone(),
            move || ctx.request_repaint(),
        );
        self.client = Some(ClientSession {
            handle,
            events,
            host_id,
            started: Instant::now(),
            state: ClientState::Connecting,
            video: None,
            show_stats: false,
        });
        self.notice = None;
    }

    fn poll_client(&mut self, ctx: &egui::Context) {
        let Some(client) = &mut self.client else { return };
        let mut ended = None;
        while let Ok(event) = client.events.try_recv() {
            match event {
                ClientEvent::Stage(Stage::Connecting) => client.state = ClientState::Connecting,
                ClientEvent::Stage(Stage::WaitingForApproval) => client.state = ClientState::WaitingForApproval,
                ClientEvent::Accepted { host_name, permissions, .. } => {
                    self.settings.add_recent(client.host_id, &host_name);
                    client.state = ClientState::Connected { host_name, permissions };
                }
                ClientEvent::PermissionsChanged(p) => {
                    if let ClientState::Connected { permissions, .. } = &mut client.state {
                        *permissions = p;
                    }
                }
                ClientEvent::VideoFormat { .. } => {}
                ClientEvent::Ended(reason) => ended = Some(reason),
            }
        }
        if let Some(reason) = ended {
            let is_error = !matches!(reason, connect_client::EndReason::Disconnected);
            self.client = None;
            self.page = Page::Connect;
            self.notify(reason.to_string(), is_error);
            if ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
            }
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.host_events.try_recv() {
            self.on_host_event(event);
        }
        let before = self.requests.len();
        self.requests.retain(|r| r.inner.is_pending());
        if self.requests.len() < before {
            self.notify("A connection request expired or was withdrawn.", false);
        }
        self.poll_client(ctx);

        // Countdown on the prompt, and the elapsed timers while connecting or hosting.
        let waiting = self.client.as_ref().is_some_and(|c| !matches!(c.state, ClientState::Connected { .. }));
        if !self.requests.is_empty() || waiting {
            ctx.request_repaint_after(Duration::from_millis(250));
        } else if self.hosting.is_some() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.client.as_ref().is_some_and(|c| matches!(c.state, ClientState::Connected { .. })) {
            session::show(self, ui);
        } else {
            egui::Panel::left("nav").exact_size(210.0).resizable(false).frame(theme::sidebar()).show(ui, |ui| {
                pages::nav(self, ui);
            });
            egui::CentralPanel::default().frame(theme::content()).show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match self.page {
                    Page::Share => pages::share(self, ui),
                    Page::Connect => pages::connect(self, ui),
                    Page::Settings => pages::settings(self, ui),
                });
            });
        }
        prompt::show(self, ui.ctx());
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
    }
}

/// Passes host events to the UI, waking it and bringing the window forward for a request,
/// even when it's minimised.
fn forward_host_events(mut events: mpsc::Receiver<HostEvent>, ctx: egui::Context) -> mpsc::UnboundedReceiver<HostEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if matches!(event, HostEvent::Request(_)) {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
                ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(egui::UserAttentionType::Critical));
            }
            if tx.send(event).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    });
    rx
}
