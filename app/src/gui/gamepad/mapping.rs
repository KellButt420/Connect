//! Viewer-local controller layouts. Native codes belong to one platform/device model.

use std::collections::BTreeMap;

use connect_proto::input::GamepadState;
use serde::{Deserialize, Serialize};

pub const LABELS: [&str; 17] = [
    "A",
    "B",
    "X",
    "Y",
    "LB",
    "RB",
    "Back",
    "Start",
    "Guide",
    "Left stick click",
    "Right stick click",
    "D-pad up",
    "D-pad down",
    "D-pad left",
    "D-pad right",
    "LT",
    "RT",
];
pub const BITS: [u16; 15] = [
    GamepadState::A,
    GamepadState::B,
    GamepadState::X,
    GamepadState::Y,
    GamepadState::LEFT_SHOULDER,
    GamepadState::RIGHT_SHOULDER,
    GamepadState::BACK,
    GamepadState::START,
    GamepadState::GUIDE,
    GamepadState::LEFT_THUMB,
    GamepadState::RIGHT_THUMB,
    GamepadState::DPAD_UP,
    GamepadState::DPAD_DOWN,
    GamepadState::DPAD_LEFT,
    GamepadState::DPAD_RIGHT,
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerKey {
    pub platform: String,
    pub uuid: [u8; 16],
    pub vendor: Option<u16>,
    pub product: Option<u16>,
    pub fallback_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Source {
    Button { code: u32 },
    Axis { code: u32, rest: f32, full: f32 },
}

impl Source {
    pub fn conflicts(self, other: Self) -> bool {
        match (self, other) {
            (Self::Button { code: a }, Self::Button { code: b }) => a == b,
            (
                Self::Axis {
                    code: a,
                    rest: ar,
                    full: af,
                },
                Self::Axis {
                    code: b,
                    rest: br,
                    full: bf,
                },
            ) => a == b && (af - ar).signum() == (bf - br).signum(),
            _ => false,
        }
    }

    pub fn value(self, input: &SourceSnapshot) -> f32 {
        let value = match self {
            Self::Button { code } => input.buttons.get(&code).map_or(0.0, |v| v.value),
            Self::Axis { code, rest, full } => input
                .axes
                .get(&code)
                .map_or(0.0, |v| (v - rest) / (full - rest)),
        };
        if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Button { code } => format!("Button {code:#x}"),
            Self::Axis { code, rest, full } => format!("Axis {code:#x} ({rest:.2} → {full:.2})"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ButtonValue {
    pub pressed: bool,
    pub value: f32,
}

#[derive(Clone, Debug, Default)]
pub struct SourceSnapshot {
    pub buttons: BTreeMap<u32, ButtonValue>,
    pub axes: BTreeMap<u32, f32>,
}

/// Runs on the reader thread, so even short taps between GUI frames are captured.
#[derive(Default)]
pub struct Capture {
    baseline: Option<SourceSnapshot>,
    candidates: Vec<Source>,
}

impl Capture {
    pub fn message(&self) -> &'static str {
        if self.baseline.is_none() {
            "Release all controls to start."
        } else if self.candidates.is_empty() {
            "Press one control fully, then release it."
        } else {
            "Release the control to finish. For LT/RT, pull fully first."
        }
    }

    pub fn update(
        &mut self,
        input: &SourceSnapshot,
        automatic: GamepadState,
    ) -> Option<Result<Source, String>> {
        if self.baseline.is_none() {
            let neutral = automatic.buttons == 0
                && automatic.left_trigger < 20
                && automatic.right_trigger < 20
                && [
                    automatic.left_x,
                    automatic.left_y,
                    automatic.right_x,
                    automatic.right_y,
                ]
                .iter()
                .all(|v| i32::from(*v).abs() < 8000)
                && input.buttons.values().all(|v| !v.pressed && v.value < 0.2);
            if neutral {
                self.baseline = Some(input.clone());
            }
            return None;
        }
        let baseline = self.baseline.as_mut().unwrap();
        for (&code, value) in &input.buttons {
            let source = Source::Button { code };
            if (value.pressed || value.value >= 0.75) && !self.candidates.contains(&source) {
                self.candidates.push(source);
            }
        }
        for (&code, &value) in &input.axes {
            // First observation establishes rest; don't guess a trigger's range from its label.
            let rest = *baseline.axes.entry(code).or_insert(value);
            if let Some(Source::Axis { full, .. }) = self
                .candidates
                .iter_mut()
                .find(|s| matches!(s, Source::Axis { code: c, rest: r, full: f } if *c == code && (*f - *r).signum() == (value - rest).signum()))
            {
                if (value - rest).abs() > (*full - rest).abs() {
                    *full = value;
                }
            } else if (value - rest).abs() >= 0.55 {
                self.candidates.push(Source::Axis {
                    code,
                    rest,
                    full: value,
                });
            }
        }
        if self.candidates.is_empty() {
            return None;
        }
        let released = self.candidates.iter().all(|s| match *s {
            Source::Button { code } => input
                .buttons
                .get(&code)
                .is_none_or(|v| !v.pressed && v.value < 0.25),
            Source::Axis { .. } => s.value(input) < 0.2,
        });
        if !released {
            return None;
        }
        let result = if self.candidates.len() == 1 {
            Ok(self.candidates[0])
        } else {
            Err("More than one control moved. Release everything and try again.".into())
        };
        *self = Self::default();
        self.baseline = Some(input.clone());
        Some(result)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControllerProfile {
    pub version: u32,
    pub key: ControllerKey,
    pub bindings: [Option<Source>; 17],
}

impl ControllerProfile {
    pub fn empty(key: ControllerKey) -> Self {
        Self {
            version: 1,
            key,
            bindings: [None; 17],
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("Unsupported controller profile version".into());
        }
        for (i, source) in self.bindings.iter().enumerate() {
            if let Some(Source::Axis { rest, full, .. }) = source
                && (!rest.is_finite()
                    || !full.is_finite()
                    || (full - rest).abs() < 0.1
                    || rest.abs() > 1.01
                    || full.abs() > 1.01)
            {
                return Err(format!("Invalid axis range for {}", LABELS[i]));
            }
            if let Some(source) = source
                && self.bindings[..i]
                    .iter()
                    .flatten()
                    .any(|other| source.conflicts(*other))
            {
                return Err(format!("{} shares an input with another button", LABELS[i]));
            }
        }
        Ok(())
    }

    pub fn conflicts(&self, target: usize, source: Source) -> Vec<usize> {
        self.bindings
            .iter()
            .enumerate()
            .filter_map(|(i, other)| {
                (i != target && other.is_some_and(|s| source.conflicts(s))).then_some(i)
            })
            .collect()
    }

    /// Caller presents any displaced bindings before accepting a reassignment.
    pub fn bind(&mut self, target: usize, source: Option<Source>) {
        if let Some(source) = source {
            for i in self.conflicts(target, source) {
                self.bindings[i] = None;
            }
        }
        self.bindings[target] = source;
    }

    pub fn map(
        &self,
        input: &SourceSnapshot,
        previous: GamepadState,
        automatic: GamepadState,
    ) -> GamepadState {
        let mut out = GamepadState {
            buttons: 0,
            left_trigger: 0,
            right_trigger: 0,
            ..automatic
        };
        for (i, bit) in BITS.iter().enumerate() {
            let down = self.bindings[i].is_some_and(|source| match source {
                Source::Button { code } => input.buttons.get(&code).is_some_and(|v| v.pressed),
                Source::Axis { .. } => {
                    source.value(input)
                        >= if previous.buttons & bit != 0 {
                            0.65
                        } else {
                            0.75
                        }
                }
            });
            if down {
                out.buttons |= bit;
            }
        }
        out.left_trigger =
            (self.bindings[15].map_or(0.0, |s| s.value(input)) * 255.0).round() as u8;
        out.right_trigger =
            (self.bindings[16].map_or(0.0, |s| s.value(input)) * 255.0).round() as u8;
        out
    }
}

/// Each profile is separately encoded so an invalid enum/range cannot discard other settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedProfiles {
    pub entries: Vec<String>,
}

impl SavedProfiles {
    pub fn decoded(&self) -> impl Iterator<Item = Result<ControllerProfile, String>> + '_ {
        self.entries.iter().map(|text| {
            let p: ControllerProfile = ron::from_str(text).map_err(|e| e.to_string())?;
            p.validate()?;
            Ok(p)
        })
    }

    pub fn errors(&self) -> Vec<String> {
        self.decoded().filter_map(Result::err).collect()
    }

    pub fn find(&self, key: &ControllerKey) -> Option<ControllerProfile> {
        self.decoded()
            .filter_map(Result::ok)
            .find(|p| &p.key == key)
    }

    pub fn set(&mut self, key: ControllerKey, profile: Option<ControllerProfile>) {
        self.entries
            .retain(|s| ron::from_str::<ControllerProfile>(s).map_or(true, |p| p.key != key));
        if let Some(profile) = profile {
            self.entries
                .push(ron::to_string(&profile).expect("validated controller profile"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ControllerKey {
        ControllerKey {
            platform: "linux".into(),
            uuid: [1; 16],
            vendor: Some(1),
            product: Some(2),
            fallback_name: String::new(),
        }
    }

    #[test]
    fn physical_a_and_lb_replace_the_wrong_x_and_lt_routes() {
        let mut profile = ControllerProfile::empty(key());
        // The driver calls physical A "X" (code 7), physical LB "LT" (code 9).
        profile.bindings[2] = Some(Source::Button { code: 7 });
        profile.bindings[15] = Some(Source::Button { code: 9 });
        profile.bind(0, Some(Source::Button { code: 7 }));
        profile.bind(4, Some(Source::Button { code: 9 }));
        let mut input = SourceSnapshot::default();
        input.buttons.insert(
            7,
            ButtonValue {
                pressed: true,
                value: 1.0,
            },
        );
        input.buttons.insert(
            9,
            ButtonValue {
                pressed: true,
                value: 1.0,
            },
        );
        let output = profile.map(&input, GamepadState::NEUTRAL, GamepadState::NEUTRAL);
        assert_eq!(
            output.buttons,
            GamepadState::A | GamepadState::LEFT_SHOULDER
        );
        assert_eq!(output.left_trigger, 0);
        assert_eq!(
            profile.map(&SourceSnapshot::default(), output, GamepadState::NEUTRAL),
            GamepadState::NEUTRAL
        );
    }

    #[test]
    fn analog_triggers_axis_dpad_hysteresis_and_sticks_are_preserved() {
        let mut p = ControllerProfile::empty(key());
        p.bindings[15] = Some(Source::Axis {
            code: 1,
            rest: -1.0,
            full: 1.0,
        });
        p.bindings[16] = Some(Source::Button { code: 2 });
        p.bindings[11] = Some(Source::Axis {
            code: 3,
            rest: 0.0,
            full: 1.0,
        });
        p.bindings[13] = Some(Source::Axis {
            code: 4,
            rest: 0.0,
            full: -1.0,
        });
        let mut input = SourceSnapshot::default();
        input.axes.extend([(1, 0.0), (3, 1.0), (4, -1.0)]);
        input.buttons.insert(
            2,
            ButtonValue {
                pressed: false,
                value: 0.25,
            },
        );
        let automatic = GamepadState {
            left_y: 32767,
            right_x: -32768,
            ..GamepadState::NEUTRAL
        };
        let out = p.map(&input, GamepadState::NEUTRAL, automatic);
        assert_eq!((out.left_trigger, out.right_trigger), (128, 64));
        assert_eq!(out.buttons, GamepadState::DPAD_UP | GamepadState::DPAD_LEFT);
        assert_eq!((out.left_y, out.right_x), (32767, -32768));
        input.axes.insert(3, 0.70);
        assert_ne!(
            p.map(&input, out, automatic).buttons & GamepadState::DPAD_UP,
            0
        );
        input.axes.insert(3, 0.60);
        assert_eq!(
            p.map(&input, out, automatic).buttons & GamepadState::DPAD_UP,
            0
        );
    }

    #[test]
    fn corrupt_profiles_are_isolated_and_opposite_axis_halves_are_allowed() {
        let mut p = ControllerProfile::empty(key());
        p.bindings[0] = Some(Source::Axis {
            code: 3,
            rest: 0.0,
            full: 1.0,
        });
        p.bindings[1] = Some(Source::Axis {
            code: 3,
            rest: 0.0,
            full: -1.0,
        });
        assert!(p.validate().is_ok());
        let good = ron::to_string(&p).unwrap();
        let mut saved = SavedProfiles {
            entries: vec![good, "broken".into()],
            ..Default::default()
        };
        assert!(saved.find(&key()).is_some());
        assert_eq!(saved.errors().len(), 1);
        p.bindings[0] = Some(Source::Axis {
            code: 3,
            rest: 0.0,
            full: 0.0,
        });
        assert!(p.validate().is_err());
        saved.set(key(), None);
        assert!(saved.find(&key()).is_none());
        let mut windows = key();
        windows.platform = "windows".into();
        assert_ne!(windows, key());
    }

    #[test]
    fn capture_ignores_held_controls_drift_and_rejects_ambiguous_presses() {
        let mut capture = Capture::default();
        let mut input = SourceSnapshot::default();
        input.buttons.insert(
            1,
            ButtonValue {
                pressed: true,
                value: 1.0,
            },
        );
        assert!(
            capture
                .update(
                    &input,
                    GamepadState {
                        buttons: GamepadState::X,
                        ..GamepadState::NEUTRAL
                    }
                )
                .is_none()
        );
        input.buttons.clear();
        input.axes.insert(3, 0.02);
        capture.update(&input, GamepadState::NEUTRAL);
        input.axes.insert(3, 0.04);
        assert!(capture.update(&input, GamepadState::NEUTRAL).is_none());
        input.buttons.insert(
            1,
            ButtonValue {
                pressed: true,
                value: 1.0,
            },
        );
        capture.update(&input, GamepadState::NEUTRAL);
        input.buttons.insert(
            2,
            ButtonValue {
                pressed: true,
                value: 1.0,
            },
        );
        capture.update(&input, GamepadState::NEUTRAL);
        input.buttons.clear();
        assert!(
            capture
                .update(&input, GamepadState::NEUTRAL)
                .unwrap()
                .is_err()
        );
        input.buttons.insert(
            2,
            ButtonValue {
                pressed: true,
                value: 1.0,
            },
        );
        capture.update(&input, GamepadState::NEUTRAL);
        input.buttons.clear();
        assert_eq!(
            capture
                .update(&input, GamepadState::NEUTRAL)
                .unwrap()
                .unwrap(),
            Source::Button { code: 2 }
        );
    }

    #[test]
    fn capture_calibrates_negative_rest_trigger_from_full_pull_and_release() {
        let mut capture = Capture::default();
        let mut input = SourceSnapshot::default();
        input.axes.insert(1, -1.0);
        capture.update(&input, GamepadState::NEUTRAL);
        input.axes.insert(1, 0.5);
        capture.update(&input, GamepadState::NEUTRAL);
        input.axes.insert(1, 1.0);
        capture.update(&input, GamepadState::NEUTRAL);
        input.axes.insert(1, -1.0);
        assert_eq!(
            capture
                .update(&input, GamepadState::NEUTRAL)
                .unwrap()
                .unwrap(),
            Source::Axis {
                code: 1,
                rest: -1.0,
                full: 1.0
            }
        );
    }

    #[test]
    fn capture_rejects_both_directions_of_an_axis_in_one_attempt() {
        let mut capture = Capture::default();
        let mut input = SourceSnapshot::default();
        input.axes.insert(1, 0.0);
        capture.update(&input, GamepadState::NEUTRAL);
        input.axes.insert(1, 0.6);
        capture.update(&input, GamepadState::NEUTRAL);
        input.axes.insert(1, -1.0);
        capture.update(&input, GamepadState::NEUTRAL);
        input.axes.insert(1, 0.0);
        assert!(
            capture
                .update(&input, GamepadState::NEUTRAL)
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn every_digital_destination_unbind_and_independent_triggers_have_correct_ranges() {
        let mut p = ControllerProfile::empty(key());
        let mut input = SourceSnapshot::default();
        let expected = [
            0x1000, 0x2000, 0x4000, 0x8000, 0x0100, 0x0200, 0x0020, 0x0010, 0x0400, 0x0040, 0x0080,
            0x0001, 0x0002, 0x0004, 0x0008,
        ];
        for (i, bit) in expected.into_iter().enumerate() {
            p.bindings = [None; 17];
            p.bind(i, Some(Source::Button { code: 10 }));
            input.buttons.insert(
                10,
                ButtonValue {
                    pressed: true,
                    value: 1.0,
                },
            );
            assert_eq!(
                p.map(&input, GamepadState::NEUTRAL, GamepadState::NEUTRAL)
                    .buttons,
                bit
            );
        }
        p.bind(15, Some(Source::Button { code: 10 }));
        p.bind(
            16,
            Some(Source::Axis {
                code: 20,
                rest: 0.0,
                full: -1.0,
            }),
        );
        for (value, want) in [(0.0, 0), (-0.5, 128), (-1.0, 255)] {
            input.axes.insert(20, value);
            let out = p.map(&input, GamepadState::NEUTRAL, GamepadState::NEUTRAL);
            assert_eq!((out.left_trigger, out.right_trigger), (255, want));
        }
        p.bind(15, None);
        assert_eq!(
            p.map(
                &input,
                GamepadState::NEUTRAL,
                GamepadState {
                    left_trigger: 255,
                    ..GamepadState::NEUTRAL
                }
            )
            .left_trigger,
            0
        );
        assert_eq!(
            p.map(
                &SourceSnapshot::default(),
                GamepadState::NEUTRAL,
                GamepadState::NEUTRAL
            ),
            GamepadState::NEUTRAL
        );
    }
}
