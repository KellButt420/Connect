//! Injects input with `SendInput` (mouse, keyboard) and the ViGEmBus driver (virtual Xbox 360
//! gamepads). Keys go in as scancodes, which games reading DirectInput or Raw Input also see.
//!
//! Windows won't let a normal program send input to one running as administrator, so those
//! windows ignore the remote user unless Connect itself runs as administrator.

use std::sync::Arc;

use connect_proto::{
    DeviceClass,
    input::{GamepadState, InputEvent, MAX_GAMEPADS, MouseButton},
};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use vigem_client::{Client, TargetId, XButtons, XGamepad, Xbox360Wired};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    SendInput, VIRTUAL_KEY,
};

use super::{GAMEPAD_DRIVER_URL, Unavailable};

const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;

pub(super) fn gamepad_support() -> Result<(), String> {
    Client::connect().map(drop).map_err(describe)
}

fn describe(e: vigem_client::Error) -> String {
    match e {
        vigem_client::Error::BusNotFound => {
            format!("Gamepads need the free ViGEmBus driver on the host PC: {GAMEPAD_DRIVER_URL}")
        }
        other => format!("The host's gamepad driver (ViGEmBus) failed: {other}"),
    }
}

/// Applies events until the session's sender is dropped, then unplugs its gamepads.
pub(super) fn run(mut events: mpsc::UnboundedReceiver<InputEvent>, unavailable: mpsc::UnboundedSender<Unavailable>) {
    let mut gamepads = Gamepads::default();
    let mut blocked_reported = false;
    while let Some(event) = events.blocking_recv() {
        match event {
            InputEvent::Gamepad { pad, state } => {
                if let Err(reason) = gamepads.update(pad, state) {
                    let _ = unavailable.send(Unavailable { device: DeviceClass::Gamepad, reason });
                }
            }
            InputEvent::GamepadRemoved { pad } => gamepads.remove(pad),
            other => {
                if !send(other) && !blocked_reported {
                    // Typically the foreground window belongs to an elevated program.
                    debug!("Windows blocked injected input");
                    blocked_reported = true;
                }
            }
        }
    }
}

/// `false` if Windows refused the input.
fn send(event: InputEvent) -> bool {
    let input = match event {
        InputEvent::MouseMoveTo { x, y } => {
            // Absolute coordinates without MOUSEEVENTF_VIRTUALDESK span the primary monitor,
            // which is the one being captured.
            mouse(x.into(), y.into(), 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE)
        }
        InputEvent::MouseMoveBy { dx, dy } => mouse(dx, dy, 0, MOUSEEVENTF_MOVE),
        InputEvent::MouseButton { button, down } => {
            let (flags, data) = match (button, down) {
                (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
                (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1),
                (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
                (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2),
            };
            mouse(0, 0, data, flags)
        }
        InputEvent::MouseWheel { dx, dy } => {
            let mut ok = true;
            if dy != 0 {
                ok &= submit(&[mouse(0, 0, dy as u32, MOUSEEVENTF_WHEEL)]);
            }
            if dx != 0 {
                ok &= submit(&[mouse(0, 0, dx as u32, MOUSEEVENTF_HWHEEL)]);
            }
            return ok;
        }
        InputEvent::Key { scancode, down } => {
            let mut flags = KEYEVENTF_SCANCODE;
            if scancode & 0xFF00 == 0xE000 {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            key(scancode & 0xFF, flags)
        }
        InputEvent::Gamepad { .. } | InputEvent::GamepadRemoved { .. } => return true,
    };
    submit(&[input])
}

fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn key(scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn submit(inputs: &[INPUT]) -> bool {
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    sent as usize == inputs.len()
}

/// Virtual Xbox 360 pads, plugged in on first use and unplugged when dropped.
#[derive(Default)]
struct Gamepads {
    /// `None` until the first gamepad event; `Err` once connecting to the driver failed.
    client: Option<Result<Arc<Client>, String>>,
    pads: [Option<Xbox360Wired<Arc<Client>>>; MAX_GAMEPADS as usize],
}

impl Gamepads {
    /// Applies `state` to slot `pad`. `Err` (once per session) if gamepads can't work here.
    fn update(&mut self, pad: u8, state: GamepadState) -> Result<(), String> {
        let first_try = self.client.is_none();
        let client = match self.client.get_or_insert_with(|| Client::connect().map(Arc::new).map_err(describe)) {
            Ok(client) => client.clone(),
            Err(reason) if first_try => {
                warn!("gamepad input unavailable: {reason}");
                return Err(reason.clone());
            }
            Err(_) => return Ok(()),
        };
        let slot = &mut self.pads[pad as usize];
        if slot.is_none() {
            let mut target = Xbox360Wired::new(client, TargetId::XBOX360_WIRED);
            if let Err(e) = target.plugin().and_then(|()| target.wait_ready()) {
                warn!(pad, "couldn't plug in a virtual gamepad: {e}");
                return Ok(());
            }
            debug!(pad, "virtual gamepad plugged in");
            *slot = Some(target);
        }
        let report = XGamepad {
            buttons: XButtons(state.buttons),
            left_trigger: state.left_trigger,
            right_trigger: state.right_trigger,
            thumb_lx: state.left_x,
            thumb_ly: state.left_y,
            thumb_rx: state.right_x,
            thumb_ry: state.right_y,
        };
        if let Some(target) = slot
            && let Err(e) = target.update(&report)
        {
            debug!(pad, "gamepad update failed: {e}");
        }
        Ok(())
    }

    fn remove(&mut self, pad: u8) {
        if let Some(slot) = self.pads.get_mut(pad as usize) {
            *slot = None; // dropping unplugs it
        }
    }
}
