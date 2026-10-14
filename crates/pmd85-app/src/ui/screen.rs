//! The main view: the emulated CRT screen, integer-scaled and centered
//! on a dark bezel.

use crate::app::App;
use crate::ui::theme::Theme;
use egui_phosphor::regular as icon;
use pmd85_core::vram::{HEIGHT, WIDTH};

pub fn draw(ui: &mut egui::Ui, app: &mut App) {
    let theme = app.active_theme_data();
    let c = |field: &[u8; 4]| Theme::color(field);
    egui::CentralPanel::default_margins()
        .frame(
            egui::Frame::new()
                .fill(c(&theme.bg))
                .inner_margin(egui::Margin::same(12)),
        )
        .show(ui, |ui| {
            let Some(tex) = &app.screen else {
                return;
            };
            let tex = tex.clone();

            // Integer scale of 288x256 into the available space.
            let avail = ui.available_size();
            let max_scale_x = (avail.x / WIDTH as f32).floor();
            let max_scale_y = (avail.y / HEIGHT as f32).floor();
            let scale = max_scale_x.min(max_scale_y).max(1.0).floor();
            // Feedback for the CRT pass (scanline strength); the
            // next decode reads it.
            app.ui.screen_scale = scale;
            let image_size = egui::vec2(WIDTH as f32 * scale, HEIGHT as f32 * scale);

            // Bezel: slightly larger than the image, drawn as a flat
            // dark frame with a hairline accent.
            let bezel = 8.0;
            let outer = image_size + egui::vec2(2.0 * bezel, 2.0 * bezel);

            let (outer_rect, _) = ui.allocate_exact_size(outer, egui::Sense::hover());
            // Center within leftovers when the available space is not
            // an exact multiple.
            let leftover = avail - outer;
            let shift = egui::vec2(
                (leftover.x / 2.0).floor().max(0.0),
                (leftover.y / 2.0).floor().max(0.0),
            );
            let outer_rect = outer_rect.translate(shift);
            let inner_rect =
                egui::Rect::from_min_size(outer_rect.min + egui::vec2(bezel, bezel), image_size);

            // Soft glow behind the bezel, then the bezel itself.
            ui.painter().rect_filled(
                outer_rect,
                egui::CornerRadius::same(3),
                c(&theme.screen_bezel),
            );
            ui.painter().rect_stroke(
                outer_rect,
                egui::CornerRadius::same(3),
                egui::Stroke::new(1.0, c(&theme.panel_border)),
                egui::StrokeKind::Inside,
            );

            // The image itself senses clicks: egui's Image defaults to
            // hover-only, and the double-click fullscreen toggle
            // needs a click-sensed response.
            let image_resp = ui.put(
                inner_rect,
                // maintain_aspect_ratio(false): the CRT texture is
                // 2x the height (two texels per emulated line for
                // scanline resolution), and the rect already has the
                // emulated screen's aspect — the image must stretch
                // into it, not letterbox to its own 1:2 aspect.
                egui::Image::from_texture(&tex)
                    .fit_to_exact_size(image_size)
                    .maintain_aspect_ratio(false)
                    .sense(egui::Sense::click()),
            );
            // Double-click toggles fullscreen (except over the
            // fullscreen button's corner, where clicks belong to it).
            let corner = fullscreen_button_zone(outer_rect);
            if image_resp.double_clicked() && !ui.rect_contains_pointer(corner) {
                app.toggle_fullscreen();
            }

            // Fullscreen toggle, video-player style: a semi-
            // transparent button revealed when the pointer nears the
            // top-right corner of the screen (the zone is larger than
            // the button so the reveal does not flicker on the
            // button's own hover).
            if ui.rect_contains_pointer(corner) {
                let btn = fullscreen_button_rect(outer_rect);
                let resp = ui.interact(
                    btn,
                    egui::Id::new("screen-fullscreen-button"),
                    egui::Sense::click(),
                );
                let panel = c(&theme.panel);
                let bg = if resp.hovered() {
                    panel
                } else {
                    egui::Color32::from_rgba_unmultiplied(
                        panel.r(),
                        panel.g(),
                        panel.b(),
                        190,
                    )
                };
                ui.painter()
                    .rect_filled(btn, egui::CornerRadius::same(3), bg);
                if resp.hovered() {
                    ui.painter().rect_stroke(
                        btn,
                        egui::CornerRadius::same(3),
                        egui::Stroke::new(1.0, c(&theme.accent)),
                        egui::StrokeKind::Inside,
                    );
                }
                let glyph = if app.fullscreen {
                    icon::ARROWS_IN_SIMPLE
                } else {
                    icon::ARROWS_OUT_SIMPLE
                };
                ui.painter().text(
                    btn.center(),
                    egui::Align2::CENTER_CENTER,
                    glyph,
                    egui::FontId::proportional(14.0),
                    c(&theme.text),
                );
                if resp.clicked() {
                    app.toggle_fullscreen();
                }
                resp.on_hover_text("Fullscreen (Alt+Enter; Esc leaves)");
            }
        });
}

/// The pointer region that reveals the fullscreen button: the
/// top-right corner of the screen.
fn fullscreen_button_zone(screen: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(
        screen.right_top() + egui::vec2(-76.0, 0.0),
        egui::vec2(76.0, 42.0),
    )
}

/// The button itself, tucked into that corner.
fn fullscreen_button_rect(screen: egui::Rect) -> egui::Rect {
    let zone = fullscreen_button_zone(screen);
    egui::Rect::from_min_size(zone.right_top() + egui::vec2(-34.0, 6.0), egui::vec2(28.0, 24.0))
}
