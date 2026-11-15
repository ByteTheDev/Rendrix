//! Desktop gamepad polling (`gilrs`) → W3C "standard" layout for `navigator.getGamepads()`.
//!
//! Android and iOS have no gilrs backend; the web driver reads the browser Gamepad API instead.

use crate::session::GamepadState;
use gilrs::{Axis, Button, Gilrs};

pub struct Pads {
    gilrs: Gilrs,
}

/// W3C standard button order: 0-3 face, 4/5 bumpers, 6/7 triggers, 8 select, 9 start, 10/11 sticks,
/// 12-15 d-pad, 16 mode.
const BUTTONS: [Button; 17] = [
    Button::South,
    Button::East,
    Button::West,
    Button::North,
    Button::LeftTrigger,
    Button::RightTrigger,
    Button::LeftTrigger2,
    Button::RightTrigger2,
    Button::Select,
    Button::Start,
    Button::LeftThumb,
    Button::RightThumb,
    Button::DPadUp,
    Button::DPadDown,
    Button::DPadLeft,
    Button::DPadRight,
    Button::Mode,
];

impl Pads {
    pub fn new() -> Option<Pads> {
        Gilrs::new().ok().map(|gilrs| Pads { gilrs })
    }

    /// Drain OS events and snapshot every pad slot as `(index, state)`; `None` = not connected.
    pub fn poll(&mut self) -> Vec<(usize, Option<GamepadState>)> {
        while self.gilrs.next_event().is_some() {}
        let mut out: Vec<(usize, Option<GamepadState>)> = Vec::new();
        for (id, pad) in self.gilrs.gamepads() {
            let index = usize::from(id);
            if !pad.is_connected() {
                out.push((index, None));
                continue;
            }
            let buttons = BUTTONS.iter().map(|b| pad.button_data(*b).map(|d| d.value()).unwrap_or(0.0)).collect();
            // DOM sticks report up as negative Y; gilrs reports up as positive.
            let axes = vec![pad.value(Axis::LeftStickX), -pad.value(Axis::LeftStickY), pad.value(Axis::RightStickX), -pad.value(Axis::RightStickY)];
            out.push((index, Some(GamepadState { id: pad.name().to_string(), buttons, axes })));
        }
        out
    }
}
