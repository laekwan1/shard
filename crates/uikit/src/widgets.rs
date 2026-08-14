//! Custom widgets shared by both apps.

use egui::{Color32, Response, Sense, Stroke, Ui, Vec2};
use std::f32::consts::TAU;

/// A large circular power button.
///
/// The whole interface reduces to this one control, so it carries the state
/// rather than a label elsewhere: the ring and glyph light up, and a halo
/// grows behind it. The transition is animated because an instant colour flip
/// gives no sense that something was switched.
pub fn power_button(ui: &mut Ui, on: bool, accent: Color32, diameter: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::click());
    let t = ui.ctx().animate_bool_with_time(response.id, on, 0.35);

    if !ui.is_rect_visible(rect) {
        return response;
    }
    // Keep animating while the transition is in flight.
    if t > 0.0 && t < 1.0 {
        ui.ctx().request_repaint();
    }

    let painter = ui.painter();
    let center = rect.center();
    let radius = diameter * 0.5 - 6.0;
    let hovered = response.hovered();

    let dim = Color32::from_rgb(96, 104, 120);
    let ring = blend(dim, accent, t);
    let lift = if hovered { 1.0 } else { 0.0 };

    // Halo: a few soft rings rather than a real blur, which egui has no
    // primitive for. Three is enough to read as a glow at this size.
    for i in 0..3 {
        let spread = 8.0 + i as f32 * 9.0;
        let alpha = (t * (26.0 - i as f32 * 7.0) + lift * 6.0).clamp(0.0, 255.0) as u8;
        if alpha > 0 {
            painter.circle_filled(center, radius + spread, with_alpha(accent, alpha));
        }
    }

    // Body.
    let body = blend(Color32::from_rgb(24, 27, 34), blend(Color32::from_rgb(24, 27, 34), accent, 0.10), t);
    painter.circle_filled(center, radius, body);
    painter.circle_stroke(center, radius, Stroke::new(2.0 + t, ring));

    // Power glyph: an arc open at the top, plus a stem through the gap.
    let glyph_radius = radius * 0.42;
    let gap = TAU * 0.16;
    let start = -TAU / 4.0 + gap;
    let end = -TAU / 4.0 + TAU - gap;
    let steps = 48;
    let arc: Vec<egui::Pos2> = (0..=steps)
        .map(|i| {
            let angle = start + (end - start) * (i as f32 / steps as f32);
            center + Vec2::new(angle.cos(), angle.sin()) * glyph_radius
        })
        .collect();
    let glyph = Stroke::new(3.0, blend(dim, accent, t));
    painter.add(egui::Shape::line(arc, glyph));
    painter.line_segment(
        [
            center + Vec2::new(0.0, -glyph_radius * 1.30),
            center + Vec2::new(0.0, -glyph_radius * 0.18),
        ],
        glyph,
    );

    response
}

/// A quiet text button for secondary actions.
pub fn ghost_button(ui: &mut Ui, text: &str) -> Response {
    let widget = egui::Button::new(egui::RichText::new(text).color(crate::theme::MUTED))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::NONE);
    ui.add(widget)
}

/// The two things the main screen offers besides the switch.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Settings,
    Browser,
    /// A play mark in a frame: what has been saved, ready to watch.
    Library,
}

/// A small drawn button.
///
/// Drawn rather than loaded, for the same reason the tray icon is: an image
/// file is a second place to remember to edit, and it has to be made again for
/// every screen density. These are shapes, so they are sharp at any size and
/// follow the theme's colours without a second copy in another palette.
pub fn icon_button(ui: &mut Ui, glyph: Glyph, tooltip: &str) -> Response {
    let size = 34.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response.on_hover_text(tooltip);
    }

    let lit = ui.ctx().animate_bool_with_time(response.id, response.hovered(), 0.15);
    let colour = blend(crate::theme::MUTED, Color32::from_rgb(226, 232, 240), lit);
    let painter = ui.painter();
    let centre = rect.center();
    let radius = size * 0.32;
    let stroke = Stroke::new(1.6, colour);

    match glyph {
        Glyph::Settings => {
            painter.circle_stroke(centre, radius * 0.45, stroke);
            // Eight spokes around the hub read as a gear at this size; teeth
            // drawn as polygons turn to mush below about twenty pixels.
            for step in 0..8 {
                let angle = TAU * step as f32 / 8.0;
                let direction = Vec2::new(angle.cos(), angle.sin());
                painter.line_segment(
                    [centre + direction * radius * 0.72, centre + direction * radius * 1.15],
                    stroke,
                );
            }
        }
        Glyph::Browser => {
            let frame = egui::Rect::from_center_size(centre, Vec2::new(size * 0.62, size * 0.52));
            painter.rect_stroke(frame, 3.0, stroke, egui::StrokeKind::Middle);
            // A title bar with two dots: the shorthand everything uses for a
            // window, and distinct from the gear at a glance.
            let bar = frame.top() + frame.height() * 0.3;
            painter.line_segment(
                [egui::pos2(frame.left(), bar), egui::pos2(frame.right(), bar)],
                stroke,
            );
            for offset in [0.18f32, 0.34] {
                painter.circle_filled(
                    egui::pos2(frame.left() + frame.width() * offset, (frame.top() + bar) / 2.0),
                    1.1,
                    colour,
                );
            }
        }
        Glyph::Library => {
            // A frame with a solid play mark in it. The frame says "a screen",
            // the filled triangle says "press to watch" — and the fill is what
            // tells it apart from the browser's outline at a glance.
            let frame = egui::Rect::from_center_size(centre, Vec2::new(size * 0.62, size * 0.52));
            painter.rect_stroke(frame, 3.0, stroke, egui::StrokeKind::Middle);
            let mark = radius * 0.52;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(centre.x - mark * 0.55, centre.y - mark),
                    egui::pos2(centre.x - mark * 0.55, centre.y + mark),
                    egui::pos2(centre.x + mark * 0.9, centre.y),
                ],
                colour,
                Stroke::NONE,
            ));
        }
    }
    response.on_hover_text(tooltip)
}

/// One number with a caption under it, for the handful of figures worth showing
/// on the main screen.
pub fn stat(ui: &mut Ui, value: &str, caption: &str, accent: Color32) {
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(value).size(22.0).color(accent).strong());
        ui.label(egui::RichText::new(caption).size(11.0).color(crate::theme::MUTED));
    });
}

fn blend(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    Color32::from_rgb(mix(from.r(), to.r()), mix(from.g(), to.g()), mix(from.b(), to.b()))
}

fn with_alpha(colour: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), alpha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_moves_between_the_endpoints() {
        let a = Color32::from_rgb(0, 0, 0);
        let b = Color32::from_rgb(100, 200, 255);
        assert_eq!(blend(a, b, 0.0), a);
        assert_eq!(blend(a, b, 1.0), b);
        let mid = blend(a, b, 0.5);
        assert!(mid.r() > 40 && mid.r() < 60);
    }

    #[test]
    fn blend_clamps_out_of_range_factors() {
        let a = Color32::from_rgb(10, 10, 10);
        let b = Color32::from_rgb(200, 200, 200);
        assert_eq!(blend(a, b, -1.0), a);
        assert_eq!(blend(a, b, 5.0), b);
    }

    #[test]
    fn alpha_produces_a_translucent_version_of_the_colour() {
        let base = Color32::from_rgb(200, 100, 50);
        let faded = with_alpha(base, 64);
        assert_eq!(faded.a(), 64);
        // Color32 stores premultiplied channels, so they scale with alpha —
        // but the relationship between them, and so the hue, is preserved.
        assert!(faded.r() > faded.g() && faded.g() > faded.b());
        assert!(faded.r() < base.r());
    }
}
