//! Host gamepad input (a PS4/DualShock 4 or any standard pad), read
//! through gilrs and mapped to the 4004/482 joystick buttons.
//!
//! gilrs is polled once per render frame ([`App::advance`]): events
//! are drained to keep the internal state current, then the button
//! state of every connected pad is snapshotted. The D-pad and the
//! left stick (with a dead zone) drive the four directions; all face
//! buttons and shoulders act as FIRE — forgiving for games that
//! expect a held fire button. Each pad is assigned to a joystick
//! connector (GPIO/0 or GPIO/1) in the Joystick panel; the assignment
//! persists in the settings, keyed by the pad's UUID.

use std::collections::BTreeMap;

use gilrs::ev::Axis;
use pmd85_core::joystick::Button as JoyButton;

/// Left-stick dead zone: below this deflection the axis reads
/// centered (resting sticks sit around ±0.05 and drift with age).
const STICK_DEADZONE: f32 = 0.4;

/// The gamepad buttons that act as FIRE.
const FIRE_BUTTONS: [gilrs::Button; 8] = [
    gilrs::Button::South,
    gilrs::Button::East,
    gilrs::Button::West,
    gilrs::Button::North,
    gilrs::Button::LeftTrigger,
    gilrs::Button::LeftTrigger2,
    gilrs::Button::RightTrigger,
    gilrs::Button::RightTrigger2,
];

/// A connected gamepad, as seen by the Joystick panel.
#[derive(Clone, Debug)]
pub struct GamepadView {
    /// Human-readable name from the driver.
    pub name: String,
    /// Mapped joystick button bits currently held on this pad.
    pub mask: u8,
    /// Raw left-stick axes (x, y) for the debug readout.
    pub stick: (f32, f32),
}

/// The gilrs context and a snapshot of the connected pads.
pub struct GamepadInput {
    /// `None` when the gamepad backend failed to initialize (no udev
    /// on this system, for instance): the app then simply sees no
    /// pads, and the Joystick panel reports it.
    gilrs: Option<gilrs::Gilrs>,
    /// Currently connected pads, keyed by UUID (hex, 32 chars) —
    /// stable across reconnects, unlike the numeric pad id.
    pads: BTreeMap<String, GamepadView>,
    /// Why gilrs is unavailable, for the panel.
    error: Option<String>,
}

impl GamepadInput {
    pub fn new() -> Self {
        let (gilrs, error) = match gilrs::Gilrs::new() {
            Ok(gilrs) => (Some(gilrs), None),
            // Not fatal: keyboard and mouse still work.
            Err(e) => {
                let error = format!("{e}");
                log::warn!("gamepad support unavailable: {error}");
                (None, Some(error))
            }
        };
        GamepadInput {
            gilrs,
            pads: BTreeMap::new(),
            error,
        }
    }

    /// Pump gilrs (draining events keeps the polled state current),
    /// snapshot the connected pads and report pads that connected or
    /// disconnected since the last poll. The returned strings are
    /// user-facing notifications.
    pub fn poll(&mut self) -> Vec<String> {
        let mut changes = Vec::new();
        let Some(gilrs) = self.gilrs.as_mut() else {
            return changes;
        };
        while gilrs.next_event().is_some() {}

        // A pad already plugged in at startup never generates a
        // Connected event, so the snapshot is built from the
        // connected set and diffed against the previous one.
        let mut now = BTreeMap::new();
        for (_id, pad) in gilrs.gamepads() {
            now.insert(
                uuid_key(&pad),
                GamepadView {
                    name: pad.name().to_owned(),
                    mask: mapped_buttons(&pad),
                    stick: (pad.value(Axis::LeftStickX), pad.value(Axis::LeftStickY)),
                },
            );
        }
        for (key, pad) in &now {
            if !self.pads.contains_key(key) {
                changes.push(format!("Gamepad connected: {}", pad.name));
            }
        }
        for (key, pad) in &self.pads {
            if !now.contains_key(key) {
                changes.push(format!("Gamepad disconnected: {}", pad.name));
            }
        }
        self.pads = now;
        changes
    }

    /// The connected pads with their current state, by UUID.
    pub fn pads(&self) -> impl Iterator<Item = (&String, &GamepadView)> {
        self.pads.iter()
    }

    /// Why the gamepad backend is unavailable, if it is.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// The stable config key of a pad: its UUID in hex.
pub fn uuid_key(pad: &gilrs::Gamepad) -> String {
    format!("{:032x}", u128::from_be_bytes(pad.uuid()))
}

/// Map a gamepad's current state to joystick button bits.
fn mapped_buttons(pad: &gilrs::Gamepad) -> u8 {
    let mut mask = 0u8;
    // The D-pad...
    if pad.is_pressed(gilrs::Button::DPadDown) {
        mask |= JoyButton::Down.bit();
    }
    if pad.is_pressed(gilrs::Button::DPadUp) {
        mask |= JoyButton::Up.bit();
    }
    if pad.is_pressed(gilrs::Button::DPadRight) {
        mask |= JoyButton::Right.bit();
    }
    if pad.is_pressed(gilrs::Button::DPadLeft) {
        mask |= JoyButton::Left.bit();
    }
    // ...and the left stick (gilrs normalizes Y so up is positive).
    let (x, y) = (pad.value(Axis::LeftStickX), pad.value(Axis::LeftStickY));
    if y < -STICK_DEADZONE {
        mask |= JoyButton::Down.bit();
    }
    if y > STICK_DEADZONE {
        mask |= JoyButton::Up.bit();
    }
    if x > STICK_DEADZONE {
        mask |= JoyButton::Right.bit();
    }
    if x < -STICK_DEADZONE {
        mask |= JoyButton::Left.bit();
    }
    for &button in &FIRE_BUTTONS {
        if pad.is_pressed(button) {
            mask |= JoyButton::Fire.bit();
            break;
        }
    }
    mask
}
