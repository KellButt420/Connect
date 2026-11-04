//! One local controller reader for Settings and the current viewing session.
pub(super) mod mapping;
mod routing;
pub(super) mod settings_ui;

use connect_client::ClientHandle;
use connect_proto::input::GamepadState;
use eframe::egui;
use gilrs::ev::filter::{Jitter, axis_dpad_to_button, deadzone};
use gilrs::{Axis, Button, Event, EventType, Filter, Gamepad, Gilrs, GilrsBuilder};
use mapping::{Capture, ControllerKey, ControllerProfile, SavedProfiles, Source, SourceSnapshot};
use routing::Router;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(20);
const BUTTONS: [Button; 17] = [
    Button::South,
    Button::East,
    Button::West,
    Button::North,
    Button::LeftTrigger,
    Button::RightTrigger,
    Button::Select,
    Button::Start,
    Button::Mode,
    Button::LeftThumb,
    Button::RightThumb,
    Button::DPadUp,
    Button::DPadDown,
    Button::DPadLeft,
    Button::DPadRight,
    Button::LeftTrigger2,
    Button::RightTrigger2,
];

#[derive(Clone)]
pub struct ControllerView {
    pub id: usize,
    pub name: String,
    pub automatic_profile: ControllerProfile,
    pub input: SourceSnapshot,
    pub automatic: GamepadState,
    pub output: GamepadState,
    pub custom: bool,
    pub slot: Option<u8>,
    pub last_event: String,
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub devices: Vec<ControllerView>,
    pub error: Option<String>,
    pub capture: Option<CaptureView>,
}

#[derive(Clone)]
pub struct CaptureView {
    pub token: u64,
    pub id: usize,
    pub target: usize,
    pub message: String,
    pub result: Option<Source>,
}

enum Command {
    Attach(ClientHandle),
    Detach,
    Active(bool),
    Visible(bool),
    Profiles(SavedProfiles),
    Capture {
        token: u64,
        id: usize,
        target: usize,
    },
    CancelCapture,
}

pub struct ControllerManager {
    commands: mpsc::Sender<Command>,
    state: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    active: AtomicBool,
    visible: AtomicBool,
    capture_token: AtomicU64,
}

impl ControllerManager {
    pub fn start(ctx: egui::Context, profiles: SavedProfiles) -> Self {
        let state = Arc::new(Mutex::new(Snapshot::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let (s, stopping) = (state.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("connect-gamepads".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(rx, &s, &stopping, &ctx, profiles)
                }));
                let error = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e),
                    Err(_) => Some(
                        "Controller reader stopped unexpectedly. Restart Connect to retry.".into(),
                    ),
                };
                if let Some(error) = error {
                    tracing::warn!("{error}");
                    *s.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Snapshot {
                        error: Some(error),
                        ..Default::default()
                    };
                    ctx.request_repaint();
                }
            });
        let (thread, error) = match thread {
            Ok(t) => (Some(t), None),
            Err(e) => (None, Some(format!("Couldn't start controller reader: {e}"))),
        };
        if let Some(error) = error {
            state.lock().unwrap().error = Some(error);
        }
        Self {
            commands: tx,
            state,
            stop,
            thread,
            active: AtomicBool::new(false),
            visible: AtomicBool::new(false),
            capture_token: AtomicU64::new(0),
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub fn attach(&self, host: ClientHandle) {
        self.active.store(false, Ordering::Relaxed);
        let _ = self.commands.send(Command::Attach(host));
    }
    pub fn detach(&self) {
        self.active.store(false, Ordering::Relaxed);
        let _ = self.commands.send(Command::Detach);
    }
    pub fn set_active(&self, active: bool) {
        if self.active.swap(active, Ordering::Relaxed) != active {
            let _ = self.commands.send(Command::Active(active));
        }
    }
    pub fn set_visible(&self, visible: bool) {
        if self.visible.swap(visible, Ordering::Relaxed) != visible {
            let _ = self.commands.send(Command::Visible(visible));
        }
    }
    pub fn profiles(&self, profiles: SavedProfiles) {
        let _ = self.commands.send(Command::Profiles(profiles));
    }
    pub fn capture(&self, id: usize, target: usize) -> u64 {
        let token = self.capture_token.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.commands.send(Command::Capture { token, id, target });
        token
    }
    pub fn cancel_capture(&self) {
        let _ = self.commands.send(Command::CancelCapture);
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for ControllerManager {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Unplugs controllers even if the reader unwinds.
#[derive(Default)]
struct Session {
    host: Option<ClientHandle>,
    router: Router,
    active: bool,
}
impl Session {
    fn detach(&mut self) {
        if let Some(host) = self.host.take() {
            for e in self.router.clear() {
                host.send_input(e);
            }
        }
        self.active = false;
    }
    fn forward(&mut self, devices: &mut BTreeMap<usize, Pad>) {
        if let Some(host) = &self.host {
            let states: Vec<_> = devices.iter().map(|(&id, p)| (id, p.view.output)).collect();
            for e in self.router.update(&states, self.active) {
                host.send_input(e);
            }
        }
        for (&id, p) in devices {
            p.view.slot = self.router.slot(id);
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.detach();
    }
}
struct Pad {
    view: ControllerView,
    profile: Option<ControllerProfile>,
}

fn read(pad: &Gamepad<'_>) -> GamepadState {
    let buttons = BUTTONS[..15]
        .iter()
        .zip(mapping::BITS)
        .filter(|(button, _)| {
            pad.is_pressed(**button)
                || button.is_dpad()
                    && button
                        .to_nec()
                        .is_some_and(|code| pad.state().is_pressed(code))
        })
        .fold(0, |all, (_, bit)| all | bit);
    let trigger = |button| {
        (pad.button_data(button)
            .map_or(0.0, |d| d.value())
            .clamp(0.0, 1.0)
            * 255.0)
            .round() as u8
    };
    let stick = |axis| {
        let value = pad.value(axis).clamp(-1.0, 1.0);
        (value * if value < 0.0 { 32768.0 } else { 32767.0 }).round() as i16
    };
    GamepadState {
        buttons,
        left_trigger: trigger(Button::LeftTrigger2),
        right_trigger: trigger(Button::RightTrigger2),
        left_x: stick(Axis::LeftStickX),
        left_y: stick(Axis::LeftStickY),
        right_x: stick(Axis::RightStickX),
        right_y: stick(Axis::RightStickY),
    }
}

fn automatic_profile(pad: &Gamepad<'_>) -> ControllerProfile {
    let uuid = pad.uuid();
    let mut p = ControllerProfile::empty(ControllerKey {
        platform: std::env::consts::OS.into(),
        uuid,
        vendor: pad.vendor_id(),
        product: pad.product_id(),
        fallback_name: if uuid == [0; 16] {
            pad.os_name().into()
        } else {
            String::new()
        },
    });
    for (i, button) in BUTTONS.iter().enumerate() {
        p.bindings[i] = pad
            .button_code(*button)
            .map(|c| Source::Button { code: c.into_u32() });
    }
    let generated_dpad = BUTTONS[11..15].iter().all(|b| {
        b.to_nec()
            .is_some_and(|c| pad.axis_or_btn_name(c).is_none())
    });
    for (i, axis, full) in [
        (11, Axis::DPadY, 1.0),
        (12, Axis::DPadY, -1.0),
        (13, Axis::DPadX, -1.0),
        (14, Axis::DPadX, 1.0),
    ] {
        if p.bindings[i].is_none()
            && let Some(code) = pad.axis_code(axis)
        {
            p.bindings[i] = if generated_dpad {
                BUTTONS[i]
                    .to_nec()
                    .map(|c| Source::Button { code: c.into_u32() })
            } else {
                Some(Source::Axis {
                    code: code.into_u32(),
                    rest: 0.0,
                    full,
                })
            };
        }
    }
    p
}

fn refresh(
    gilrs: &Gilrs,
    devices: &mut BTreeMap<usize, Pad>,
    profiles: &SavedProfiles,
    event: Option<&Event>,
    diagnostics: bool,
) {
    devices.retain(|id, _| gilrs.gamepads().any(|(other, _)| usize::from(other) == *id));
    for (id, pad) in gilrs.gamepads() {
        let id = usize::from(id);
        let p = devices.entry(id).or_insert_with(|| {
            let automatic_profile = automatic_profile(&pad);
            let profile = profiles.find(&automatic_profile.key);
            Pad {
                view: ControllerView {
                    id,
                    name: pad.os_name().into(),
                    automatic_profile,
                    input: SourceSnapshot::default(),
                    automatic: GamepadState::NEUTRAL,
                    output: GamepadState::NEUTRAL,
                    custom: profile.is_some(),
                    slot: None,
                    last_event: String::new(),
                },
                profile,
            }
        });
        for (code, button) in pad.state().buttons() {
            // Keep pressed flags and analogue values separate; press+change notifications
            // must not create a false full-trigger spike.
            p.view
                .input
                .buttons
                .entry(code.into_u32())
                .or_default()
                .pressed = button.is_pressed();
        }
        for (code, axis) in pad.state().axes() {
            p.view.input.axes.insert(code.into_u32(), axis.value());
        }
        if let Some(event) = event.filter(|e| usize::from(e.id) == id) {
            if let EventType::ButtonChanged(_, value, code) = event.event {
                p.view
                    .input
                    .buttons
                    .entry(code.into_u32())
                    .or_default()
                    .value = value;
            }
            if diagnostics {
                p.view.last_event = format!("{:?}", event.event);
            }
        }
        p.view.automatic = read(&pad);
        p.view.output = p.profile.as_ref().map_or(p.view.automatic, |profile| {
            profile.map(&p.view.input, p.view.output, p.view.automatic)
        });
    }
}

fn run(
    commands: mpsc::Receiver<Command>,
    state: &Mutex<Snapshot>,
    stop: &AtomicBool,
    ctx: &egui::Context,
    mut profiles: SavedProfiles,
) -> Result<(), String> {
    // Process the standard filters once per backend event. gilrs' blocking filtered loop can
    // otherwise keep consuming jitter indefinitely without servicing focus/stop commands.
    let mut gilrs = GilrsBuilder::new().with_default_filters(false).set_update_state(false).build()
        .map_err(|e| format!("Controllers unavailable: {e}. On Linux, check USB detection and access to /dev/input/event devices."))?;
    let mut devices = BTreeMap::new();
    let mut session = Session::default();
    let mut capture: Option<(u64, usize, usize, Capture)> = None;
    let mut capture_view = None;
    let mut visible = false;
    let mut dirty = true;
    let mut force = true;
    let mut published = Instant::now();
    refresh(&gilrs, &mut devices, &profiles, None, false);
    while !stop.load(Ordering::Relaxed) {
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Attach(host) => {
                    session.detach();
                    session.host = Some(host);
                    capture = None;
                    capture_view = None;
                }
                Command::Detach => session.detach(),
                Command::Active(active) => session.active = active,
                Command::Visible(now) => {
                    visible = now;
                    if !now {
                        capture = None;
                        capture_view = None;
                    }
                }
                Command::Profiles(now) => {
                    profiles = now;
                    for pad in devices.values_mut() {
                        pad.profile = profiles.find(&pad.view.automatic_profile.key);
                        pad.view.custom = pad.profile.is_some();
                    }
                    refresh(&gilrs, &mut devices, &profiles, None, visible);
                }
                Command::Capture { token, id, target }
                    if session.host.is_none() && target < mapping::LABELS.len() =>
                {
                    capture = Some((token, id, target, Capture::default()));
                    capture_view = None;
                }
                Command::Capture { .. } => {}
                Command::CancelCapture => {
                    capture = None;
                    capture_view = None;
                }
            }
            session.forward(&mut devices);
            dirty = true;
            force = true;
        }
        if let Some((token, id, target, machine)) = &mut capture {
            if let Some(pad) = devices.get(id) {
                let result = machine.update(&pad.view.input, pad.view.automatic);
                let mut view = CaptureView {
                    token: *token,
                    id: *id,
                    target: *target,
                    message: machine.message().into(),
                    result: None,
                };
                if let Some(result) = result {
                    match result {
                        Ok(source) => {
                            view.result = Some(source);
                            view.message = "Input captured.".into();
                        }
                        Err(e) => view.message = e,
                    }
                    force = true;
                }
                let done = view.result.is_some();
                capture_view = Some(view);
                if done {
                    capture = None;
                }
            } else {
                capture_view = Some(CaptureView {
                    token: *token,
                    id: *id,
                    target: *target,
                    message: "Controller disconnected. Cancel and select a connected controller."
                        .into(),
                    result: None,
                });
                capture = None;
                force = true;
            }
        }
        if dirty && (force || visible && published.elapsed() >= Duration::from_millis(33)) {
            *state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Snapshot {
                devices: devices.values().map(|p| p.view.clone()).collect(),
                error: None,
                capture: capture_view.clone(),
            };
            published = Instant::now();
            dirty = false;
            force = false;
            if visible {
                ctx.request_repaint();
            }
        }
        let event = gilrs
            .next_event_blocking(Some(POLL))
            .filter_ev(&axis_dpad_to_button, &mut gilrs)
            .filter_ev(&Jitter::new(), &mut gilrs)
            .filter_ev(&deadzone, &mut gilrs);
        if let Some(event) = event.filter(|e| !e.is_dropped()) {
            gilrs.update(&event);
            force |= matches!(event.event, EventType::Connected | EventType::Disconnected);
            refresh(&gilrs, &mut devices, &profiles, Some(&event), visible);
            session.forward(&mut devices);
            dirty = true;
        }
    }
    Ok(())
}
