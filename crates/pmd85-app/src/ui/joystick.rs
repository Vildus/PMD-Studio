//! The 4004/482 joystick panel: live state of both connectors, the
//! enable switch, mouse control (like the keyboard panel, holding
//! the pointer on a cap presses the button, sliding slides the
//! press) and the per-gamepad connector assignment.
//!
//! The caps light up from the *effective* emulated state — what the
//! running program would read — so gamepad input, mouse input and the
//! driver-enable state all show: while the program has not opened the
//! driver transistor (PC4/PC0), a held button cannot pull its line
//! low, and the panel says so.

use egui::{CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind};
use pmd85_core::joystick::Button;

use crate::app::App;
use crate::config::GamepadPort;
use crate::gamepad::GamepadView;
use crate::ui::theme::Theme;

/// Cap size in points.
const CAP: f32 = 30.0;
/// Gap between caps.
const GAP: f32 = 3.0;
/// Gap between the cross and the fire button.
const FIRE_GAP: f32 = 16.0;
/// Margin between the two sticks.
const STICK_MARGIN: f32 = 28.0;
/// Height reserved for the connector heading above the cross.
const HEADING: f32 = 18.0;

/// The caps of one stick, in (row, column) cells of the cross.
const CROSS: [(Button, usize, usize); 5] = [
    (Button::Up, 0, 1),
    (Button::Left, 1, 0),
    (Button::Down, 2, 1),
    (Button::Right, 1, 2),
    (Button::Fire, 1, 4),
];

/// The panel body, drawn inside its dock tab.
pub fn ui(ui: &mut egui::Ui, app: &mut App) {
    let theme = app.active_theme_data();

    let mut enabled = app.settings.joystick_enabled;
    if ui
        .checkbox(&mut enabled, "Joystick input enabled")
        .changed()
    {
        app.settings.joystick_enabled = enabled;
        app.persist();
    }
    ui.add_space(4.0);

    // Both sticks side by side, in one interaction area: a press
    // slides between them like the keyboard caps slide between keys.
    let stick_w = stick_width();
    let total = egui::vec2(
        2.0 * stick_w + STICK_MARGIN,
        HEADING + stick_height(),
    );
    let (rect, _) = ui.allocate_exact_size(total, Sense::click());
    let diagram = |port: usize| rect.min + egui::vec2(port as f32 * (stick_w + STICK_MARGIN), HEADING);
    for port in [0usize, 1] {
        paint_stick(ui.painter(), diagram(port), app, port, &theme);
    }
    let held = if ui.input(|i| i.pointer.primary_down()) {
        ui.input(|i| i.pointer.interact_pos()).and_then(|pos| hit_button(rect.min, pos))
    } else {
        None
    };
    apply_pointer(app, held);

    // What the program currently sees: whether the driver transistor
    // is open (the caps above show the buttons regardless).
    for port in [0usize, 1] {
        let readable = app.machine.bus.joystick_readable(port);
        ui.label(
            egui::RichText::new(if readable {
                format!(
                    "Joystick {port} (\u{b7} {}) readable: driver enabled",
                    connector_name(port)
                )
            } else {
                format!(
                    "Joystick {port} (\u{b7} {}) not readable: the program has \
                     not opened the driver (PC{})",
                    connector_name(port),
                    if port == 0 { 4 } else { 0 }
                )
            })
            .size(10.0)
            .color(Theme::color(&theme.text_weak)),
        );
    }

    ui.add_space(6.0);
    gamepads(ui, app, &theme);
}

/// Width of one stick's diagram.
fn stick_width() -> f32 {
    4.0 * CAP + 3.0 * GAP + FIRE_GAP
}

/// Height of one stick's diagram.
fn stick_height() -> f32 {
    3.0 * CAP + 2.0 * GAP
}

/// The rect of a cap, relative to the stick's top-left corner.
fn cap_rect(origin: Pos2, button: Button) -> Rect {
    let (row, col) = CROSS
        .iter()
        .find(|(b, _, _)| *b == button)
        .map(|(_, r, c)| (*r, *c))
        .expect("all five buttons are in the cross");
    let step = CAP + GAP;
    let x = if col <= 2 {
        origin.x + col as f32 * step
    } else {
        // The fire button sits right of the cross.
        origin.x + 3.0 * step + FIRE_GAP - GAP
    };
    Rect::from_min_size(
        egui::pos2(x, origin.y + row as f32 * step),
        egui::vec2(CAP, CAP),
    )
}

fn paint_stick(
    p: &egui::Painter,
    origin: Pos2,
    app: &App,
    port: usize,
    theme: &Theme,
) {
    let c = |f: &[u8; 4]| Theme::color(f);
    let stick = &app.machine.bus.joysticks[port];
    // The engraved arrows; fire is the round red cap.
    let glyphs = [
        (Button::Up, "\u{2191}"),
        (Button::Left, "\u{2190}"),
        (Button::Down, "\u{2193}"),
        (Button::Right, "\u{2192}"),
    ];
    for (button, glyph) in glyphs {
        paint_cap(p, cap_rect(origin, button), glyph, stick.is_pressed(button), theme);
    }
    let fire_rect = cap_rect(origin, Button::Fire);
    let pressed = stick.is_pressed(Button::Fire);
    let fill = if pressed {
        c(&theme.danger)
    } else {
        c(&theme.widget)
    };
    let stroke = if pressed {
        c(&theme.danger)
    } else {
        c(&theme.panel_border)
    };
    p.rect_filled(fire_rect, CornerRadius::same((CAP / 2.0) as u8), fill);
    p.rect_stroke(
        fire_rect,
        CornerRadius::same((CAP / 2.0) as u8),
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );
    p.text(
        fire_rect.center(),
        egui::Align2::CENTER_CENTER,
        "FIRE",
        egui::FontId::monospace(8.0),
        if pressed {
            c(&theme.text)
        } else {
            c(&theme.text_weak)
        },
    );

    // The connector heading above the cross.
    p.text(
        egui::pos2(origin.x, origin.y - HEADING + 2.0),
        egui::Align2::LEFT_TOP,
        format!("Joystick {port} \u{b7} {}", connector_name(port)),
        egui::FontId::monospace(10.0),
        c(&theme.text_weak),
    );
}

fn paint_cap(p: &egui::Painter, rect: Rect, glyph: &str, pressed: bool, theme: &Theme) {
    let c = |f: &[u8; 4]| Theme::color(f);
    let fill = if pressed {
        c(&theme.accent_dim)
    } else {
        c(&theme.widget)
    };
    let stroke = if pressed {
        c(&theme.accent)
    } else {
        c(&theme.panel_border)
    };
    p.rect_filled(rect, CornerRadius::same(2), fill);
    p.rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::monospace(13.0),
        if pressed {
            c(&theme.accent)
        } else {
            c(&theme.text)
        },
    );
}

/// The (connector, button) of the cap under `pos`, if any.
fn hit_button(origin: Pos2, pos: Pos2) -> Option<(usize, Button)> {
    for port in [0usize, 1] {
        let stick = origin
            + egui::vec2(port as f32 * (stick_width() + STICK_MARGIN), HEADING);
        for &(button, _, _) in &CROSS {
            if cap_rect(stick, button).contains(pos) {
                return Some((port, button));
            }
        }
    }
    None
}

/// Apply the cap currently under a held pointer button (`None`: the
/// button is up or the pointer left the caps). Holding a cap presses
/// its joystick button, sliding across caps slides the press. The
/// merge with the gamepad state happens in [`App::advance`].
fn apply_pointer(app: &mut App, held: Option<(usize, Button)>) {
    app.ui.mouse_joystick = held;
}

/// Release a pointer-held button (the panel is closing or being
/// hidden; nothing will call [`apply_pointer`] again).
pub fn release_pointer(app: &mut App) {
    apply_pointer(app, None);
}

/// The connector a joystick index is wired to.
fn connector_name(port: usize) -> &'static str {
    match port {
        0 => "GPIO/0, K3, port 4Ch",
        _ => "GPIO/1, K4, port 4Dh",
    }
}

/// The connected gamepads with their connector assignment.
fn gamepads(ui: &mut egui::Ui, app: &mut App, theme: &Theme) {
    ui.label(
        egui::RichText::new("Gamepads")
            .size(11.0)
            .color(Theme::color(&theme.text_weak)),
    );
    if let Some(error) = app.gamepads.error() {
        ui.label(
            egui::RichText::new(format!("Gamepad backend unavailable: {error}"))
                .size(10.0)
                .color(Theme::color(&theme.danger)),
        );
        return;
    }

    // A snapshot: the settings map is edited while iterating.
    let pads: Vec<(String, GamepadView)> = app
        .gamepads
        .pads()
        .map(|(uuid, pad)| (uuid.clone(), pad.clone()))
        .collect();
    if pads.is_empty() {
        ui.label(
            egui::RichText::new(
                "No gamepad connected. Buttons: D-pad and left stick move, \
                 Cross/Circle/Square/Triangle and the shoulders fire.",
            )
            .size(10.0)
            .color(Theme::color(&theme.text_weak)),
        );
        return;
    }

    for (uuid, pad) in pads {
        egui::Grid::new(uuid.clone())
            .num_columns(3)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                ui.label(&pad.name);
                let old = app
                    .settings
                    .gamepad_ports
                    .get(&uuid)
                    .copied()
                    .unwrap_or_default();
                let mut port = old;
                let selected = match port {
                    GamepadPort::Gpio0 => "GPIO/0 \u{b7} joystick 0",
                    GamepadPort::Gpio1 => "GPIO/1 \u{b7} joystick 1",
                    GamepadPort::Off => "off",
                };
                egui::ComboBox::from_id_salt(&uuid)
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut port, GamepadPort::Gpio0, "GPIO/0 \u{b7} joystick 0");
                        ui.selectable_value(&mut port, GamepadPort::Gpio1, "GPIO/1 \u{b7} joystick 1");
                        ui.selectable_value(&mut port, GamepadPort::Off, "off");
                    });
                if port != old {
                    // The map holds only assignments that differ from
                    // the default (GPIO/0).
                    app.settings.gamepad_ports.insert(uuid.clone(), port);
                    app.persist();
                }
                ui.label(
                    egui::RichText::new(format!(
                        "stick {:+.2} / {:+.2}",
                        pad.stick.0, pad.stick.1
                    ))
                    .size(10.0)
                    .color(Theme::color(&theme.text_weak)),
                );
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cross geometry: every cap is hittable at its own center,
    /// on both sticks.
    #[test]
    fn hit_button_finds_caps() {
        for port in [0usize, 1] {
            let origin =
                egui::Pos2::ZERO + egui::vec2(port as f32 * (stick_width() + STICK_MARGIN), HEADING);
            for &(button, _, _) in &CROSS {
                let center = cap_rect(origin, button).center();
                assert_eq!(
                    hit_button(egui::Pos2::ZERO, center),
                    Some((port, button)),
                    "cap {button:?} of stick {port} is not hit at its center"
                );
            }
        }
        // Between the sticks there is no cap...
        let between = egui::pos2(stick_width() + STICK_MARGIN / 2.0, HEADING + CAP);
        assert_eq!(hit_button(egui::Pos2::ZERO, between), None);
        // ...nor above the crosses, around the whole block, or in the
        // cross centers.
        assert_eq!(
            hit_button(egui::Pos2::ZERO, egui::pos2(0.0, HEADING + CAP)),
            None
        );
        assert_eq!(hit_button(egui::Pos2::ZERO, egui::pos2(-50.0, -50.0)), None);
        assert_eq!(
            hit_button(egui::Pos2::ZERO, egui::pos2(5000.0, 5000.0)),
            None
        );
    }

    /// Holding a cap presses its joystick button once the emulation
    /// advances; disabling the input path releases it; the pointer
    /// going up releases it.
    #[test]
    fn clicking_caps_presses_and_releases_buttons() {
        let mut app = crate::ui::tests::test_app();
        app.advance();

        apply_pointer(&mut app, Some((0, Button::Fire)));
        assert_eq!(app.ui.mouse_joystick, Some((0, Button::Fire)));
        app.advance();
        assert!(app.machine.bus.joysticks[0].is_pressed(Button::Fire));

        // The other stick is not affected.
        assert!(!app.machine.bus.joysticks[1].is_pressed(Button::Fire));

        // The master switch gates everything.
        app.settings.joystick_enabled = false;
        app.advance();
        assert!(!app.machine.bus.joysticks[0].is_pressed(Button::Fire));
        app.settings.joystick_enabled = true;

        // Releasing the pointer releases the button.
        apply_pointer(&mut app, None);
        app.advance();
        assert!(!app.machine.bus.joysticks[0].is_pressed(Button::Fire));

        // And the panel being closed releases it too.
        apply_pointer(&mut app, Some((1, Button::Left)));
        release_pointer(&mut app);
        app.advance();
        assert!(!app.machine.bus.joysticks[1].is_pressed(Button::Left));
    }
}
