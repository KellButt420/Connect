//! Session-local slots and ordered reports, independent of the device backend and GUI.
use connect_proto::input::{GamepadState, InputEvent, MAX_GAMEPADS};

#[derive(Default)]
pub struct Router {
    slots: [Option<usize>; MAX_GAMEPADS as usize],
    sent: [Option<GamepadState>; MAX_GAMEPADS as usize],
}

impl Router {
    pub fn slot(&self, id: usize) -> Option<u8> {
        self.slots
            .iter()
            .position(|s| *s == Some(id))
            .map(|i| i as u8)
    }
    pub fn clear(&mut self) -> Vec<InputEvent> {
        let events = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, id)| id.map(|_| InputEvent::GamepadRemoved { pad: i as u8 }))
            .collect();
        *self = Self::default();
        events
    }
    pub fn update(&mut self, states: &[(usize, GamepadState)], active: bool) -> Vec<InputEvent> {
        let mut events = Vec::new();
        for (slot, id) in self.slots.iter_mut().enumerate() {
            if id.is_some_and(|id| !states.iter().any(|(other, _)| id == *other)) {
                *id = None;
                self.sent[slot] = None;
                events.push(InputEvent::GamepadRemoved { pad: slot as u8 });
            }
        }
        for &(id, state) in states {
            let slot = self.slot(id).or_else(|| {
                self.slots.iter().position(Option::is_none).map(|s| {
                    self.slots[s] = Some(id);
                    s as u8
                })
            });
            let Some(slot) = slot else { continue };
            let state = if active { state } else { GamepadState::NEUTRAL };
            if self.sent[slot as usize] != Some(state) {
                self.sent[slot as usize] = Some(state);
                events.push(InputEvent::Gamepad { pad: slot, state });
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taps_focus_changes_removal_and_replacement_release_state() {
        let mut router = Router::default();
        let down = GamepadState {
            buttons: GamepadState::A,
            ..GamepadState::NEUTRAL
        };
        assert_eq!(
            router.update(&[(10, down)], true),
            vec![InputEvent::Gamepad {
                pad: 0,
                state: down
            }]
        );
        assert_eq!(
            router.update(&[(10, GamepadState::NEUTRAL)], true),
            vec![InputEvent::Gamepad {
                pad: 0,
                state: GamepadState::NEUTRAL
            }]
        );
        router.update(&[(10, down)], true);
        assert_eq!(
            router.update(&[(10, down)], false),
            vec![InputEvent::Gamepad {
                pad: 0,
                state: GamepadState::NEUTRAL
            }]
        );
        assert_eq!(
            router.update(&[(10, down)], true),
            vec![InputEvent::Gamepad {
                pad: 0,
                state: down
            }]
        );
        assert_eq!(router.clear(), vec![InputEvent::GamepadRemoved { pad: 0 }]);
        assert_eq!(router.slot(10), None);
        assert_eq!(
            router.update(&[(20, down)], true),
            vec![InputEvent::Gamepad {
                pad: 0,
                state: down
            }]
        );
        assert_eq!(
            router.update(&[], true),
            vec![InputEvent::GamepadRemoved { pad: 0 }]
        );
    }

    #[test]
    fn four_slots_are_stable_and_fifth_waits_for_a_free_slot() {
        let mut router = Router::default();
        let pads: Vec<_> = (0..5).map(|id| (id, GamepadState::NEUTRAL)).collect();
        assert_eq!(router.update(&pads, true).len(), 4);
        assert_eq!(router.slot(4), None);
        router.update(&pads[1..], true);
        assert_eq!(router.slot(1), Some(1));
        assert_eq!(router.slot(4), Some(0));
    }
}
