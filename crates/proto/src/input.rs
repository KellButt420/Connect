//! Remote input, sent by the client on its own uni-directional stream (reliable and ordered,
//! so a key-up can never be lost or overtake its key-down), framed like [`control`](crate::control)
//! messages.
//!
//! The host checks every event against its current [`Permissions`](crate::Permissions) before
//! acting on it; see [`InputEvent::device`].

use serde::{Deserialize, Serialize};

use crate::DeviceClass;

/// Gamepads one client can drive at once (XInput's limit).
pub const MAX_GAMEPADS: u8 = 4;

/// Largest relative mouse step accepted in one event; anything bigger is clamped.
pub const MAX_MOUSE_STEP: i32 = 4096;

/// One notch of a mouse wheel, as Windows counts it.
pub const WHEEL_NOTCH: i32 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// The side button usually mapped to "back".
    Back,
    /// The side button usually mapped to "forward".
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    /// Pointer position on the shared screen: 0 is the left/top edge, 65535 the right/bottom.
    MouseMoveTo { x: u16, y: u16 },
    /// Raw relative motion, for games that capture the mouse.
    MouseMoveBy { dx: i32, dy: i32 },
    MouseButton { button: MouseButton, down: bool },
    /// Wheel motion in [`WHEEL_NOTCH`] units; positive `dy` scrolls up, positive `dx` right.
    MouseWheel { dx: i32, dy: i32 },
    /// A key by its position on the keyboard: a PS/2 set-1 scancode, with `0xE000` set for
    /// extended keys (arrows, right Ctrl, …). The host's keyboard layout decides what it types.
    Key { scancode: u16, down: bool },
    /// The full state of gamepad slot `pad`, sent whenever it changes.
    Gamepad { pad: u8, state: GamepadState },
    /// The gamepad in slot `pad` was unplugged.
    GamepadRemoved { pad: u8 },
}

impl InputEvent {
    /// The permission that must be on for the host to act on this event.
    pub const fn device(&self) -> DeviceClass {
        match self {
            InputEvent::MouseMoveTo { .. }
            | InputEvent::MouseMoveBy { .. }
            | InputEvent::MouseButton { .. }
            | InputEvent::MouseWheel { .. } => DeviceClass::Mouse,
            InputEvent::Key { .. } => DeviceClass::Keyboard,
            InputEvent::Gamepad { .. } | InputEvent::GamepadRemoved { .. } => DeviceClass::Gamepad,
        }
    }

    /// Whether the event is well formed: a real scancode, a gamepad slot in range.
    pub const fn is_valid(&self) -> bool {
        match *self {
            InputEvent::Key { scancode, .. } => {
                let high = scancode >> 8;
                scancode & 0xff != 0 && (high == 0 || high == 0xE0)
            }
            InputEvent::Gamepad { pad, .. } | InputEvent::GamepadRemoved { pad } => pad < MAX_GAMEPADS,
            _ => true,
        }
    }
}

/// A gamepad in Xbox (XInput) layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GamepadState {
    /// [`GamepadState`] button bits, e.g. [`GamepadState::A`].
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    /// Sticks: -32768 is fully left/down, 32767 fully right/up.
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
}

impl GamepadState {
    /// Nothing pressed, sticks centred.
    pub const NEUTRAL: GamepadState =
        GamepadState { buttons: 0, left_trigger: 0, right_trigger: 0, left_x: 0, left_y: 0, right_x: 0, right_y: 0 };

    // Bit values match XInput's XINPUT_GAMEPAD_* constants.
    pub const DPAD_UP: u16 = 0x0001;
    pub const DPAD_DOWN: u16 = 0x0002;
    pub const DPAD_LEFT: u16 = 0x0004;
    pub const DPAD_RIGHT: u16 = 0x0008;
    pub const START: u16 = 0x0010;
    pub const BACK: u16 = 0x0020;
    pub const LEFT_THUMB: u16 = 0x0040;
    pub const RIGHT_THUMB: u16 = 0x0080;
    pub const LEFT_SHOULDER: u16 = 0x0100;
    pub const RIGHT_SHOULDER: u16 = 0x0200;
    pub const GUIDE: u16 = 0x0400;
    pub const A: u16 = 0x1000;
    pub const B: u16 = 0x2000;
    pub const X: u16 = 0x4000;
    pub const Y: u16 = 0x8000;
}

/// PS/2 set-1 scancodes for keys the client may need to refer to by name.
pub mod scancode {
    pub const LEFT_CTRL: u16 = 0x1D;
    pub const LEFT_SHIFT: u16 = 0x2A;
    pub const LEFT_ALT: u16 = 0x38;
    pub const C: u16 = 0x2E;
    pub const V: u16 = 0x2F;
    pub const X: u16 = 0x2D;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control;

    #[test]
    fn round_trips_through_control_framing() {
        let events = [
            InputEvent::MouseMoveTo { x: 0, y: u16::MAX },
            InputEvent::MouseMoveBy { dx: -3, dy: 7 },
            InputEvent::MouseButton { button: MouseButton::Back, down: true },
            InputEvent::MouseWheel { dx: 0, dy: -WHEEL_NOTCH },
            InputEvent::Key { scancode: 0xE048, down: false },
            InputEvent::Gamepad {
                pad: 3,
                state: GamepadState { buttons: GamepadState::A | GamepadState::Y, left_x: -32768, ..GamepadState::NEUTRAL },
            },
            InputEvent::GamepadRemoved { pad: 0 },
        ];
        for event in events {
            let bytes = control::encode(&event).unwrap();
            assert!(bytes.len() < 24, "{event:?} encodes to {} bytes", bytes.len());
            assert_eq!(control::decode::<InputEvent>(&bytes[4..]).unwrap(), event);
        }
    }

    #[test]
    fn events_map_to_their_permission() {
        assert_eq!(InputEvent::MouseWheel { dx: 0, dy: 1 }.device(), DeviceClass::Mouse);
        assert_eq!(InputEvent::Key { scancode: 0x1E, down: true }.device(), DeviceClass::Keyboard);
        assert_eq!(InputEvent::GamepadRemoved { pad: 0 }.device(), DeviceClass::Gamepad);
    }

    #[test]
    fn validation() {
        assert!(InputEvent::Key { scancode: 0x1E, down: true }.is_valid());
        assert!(InputEvent::Key { scancode: 0xE04D, down: true }.is_valid());
        assert!(!InputEvent::Key { scancode: 0, down: true }.is_valid());
        assert!(!InputEvent::Key { scancode: 0x1234, down: true }.is_valid());
        assert!(!InputEvent::Gamepad { pad: MAX_GAMEPADS, state: GamepadState::NEUTRAL }.is_valid());
    }
}
