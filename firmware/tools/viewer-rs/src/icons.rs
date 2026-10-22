//! Small vector icon buttons (drawn with the painter, so they do not depend on a font), with a hover hint.

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

#[derive(Clone, Copy)]
pub enum Icon {
    Plug,     // connected
    Unplug,   // not connected
    Zero,     // dark-offset calibration
    Firmware, // firmware update
}

pub fn icon_button(ui: &mut egui::Ui, icon: Icon, hint: &str, enabled: bool, accent: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(28.0), if enabled { Sense::click() } else { Sense::hover() });
    let resp = resp.on_hover_text(hint);
    let p = ui.painter();
    let hot = enabled && resp.hovered();
    if hot {
        p.rect_filled(rect, CornerRadius::same(5), Color32::from_gray(60));
    }
    let col = if !enabled { Color32::from_gray(90) } else if hot { accent } else { Color32::from_gray(200) };
    let st = Stroke::new(1.6, col);
    let c = rect.center();
    let at = |x: f32, y: f32| Pos2::new(c.x + x, c.y + y);
    match icon {
        Icon::Plug | Icon::Unplug => {
            // plug body with two prongs and a cable
            let body = Rect::from_min_max(at(-5.0, -3.0), at(5.0, 4.0));
            p.rect_stroke(body, CornerRadius::same(2), st, StrokeKind::Middle);
            p.line_segment([at(-2.5, -3.0), at(-2.5, -8.0)], st);
            p.line_segment([at(2.5, -3.0), at(2.5, -8.0)], st);
            p.line_segment([at(0.0, 4.0), at(0.0, 9.0)], st);
            if let Icon::Unplug = icon {
                p.line_segment([at(-8.0, 8.0), at(8.0, -8.0)], Stroke::new(2.0, Color32::from_rgb(220, 90, 70)));
            }
        }
        Icon::Zero => {
            // sensor covered: circle with a filled dark half and a "0" mark
            p.circle_stroke(c, 8.0, st);
            p.circle_stroke(c, 3.5, st);
            p.line_segment([at(-10.0, 0.0), at(-6.0, 0.0)], st);
            p.line_segment([at(6.0, 0.0), at(10.0, 0.0)], st);
            p.line_segment([at(0.0, -10.0), at(0.0, -6.0)], st);
            p.line_segment([at(0.0, 6.0), at(0.0, 10.0)], st);
        }
        Icon::Firmware => {
            // chip with pins and an upward arrow
            let body = Rect::from_min_max(at(-6.0, -6.0), at(6.0, 6.0));
            p.rect_stroke(body, CornerRadius::same(1), st, StrokeKind::Middle);
            for k in [-3.0, 0.0, 3.0] {
                p.line_segment([at(k, -6.0), at(k, -9.0)], st);
                p.line_segment([at(k, 6.0), at(k, 9.0)], st);
                p.line_segment([at(-6.0, k), at(-9.0, k)], st);
                p.line_segment([at(6.0, k), at(9.0, k)], st);
            }
            p.line_segment([at(0.0, 3.5), at(0.0, -3.5)], st);
            p.line_segment([at(0.0, -3.5), at(-2.5, -1.0)], st);
            p.line_segment([at(0.0, -3.5), at(2.5, -1.0)], st);
        }
    }
    resp
}

/// RUN / STOP as one button showing the current state: a green triangle while running, a red square when stopped
pub fn run_button(ui: &mut egui::Ui, running: bool, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(44.0, 30.0), if enabled { Sense::click() } else { Sense::hover() });
    let resp = resp.on_hover_text(if running { "Running (click: stop)" } else { "Stopped (click: run)" });
    let p = ui.painter();
    let hot = enabled && resp.hovered();
    p.rect_filled(rect, CornerRadius::same(5), Color32::from_gray(if hot { 64 } else { 48 }));
    let c = rect.center();
    let at = |x: f32, y: f32| Pos2::new(c.x + x, c.y + y);
    if !enabled {
        p.rect_filled(Rect::from_center_size(c, Vec2::splat(12.0)), CornerRadius::same(1), Color32::from_gray(90));
    } else if running {
        let g = Color32::from_rgb(70, 200, 100);
        p.add(egui::Shape::convex_polygon(vec![at(-6.0, -8.0), at(9.0, 0.0), at(-6.0, 8.0)], g, Stroke::NONE));
    } else {
        let r = Color32::from_rgb(225, 70, 60);
        p.rect_filled(Rect::from_center_size(c, Vec2::splat(14.0)), CornerRadius::same(2), r);
    }
    resp
}

/// Trigger edge as a pulse-edge icon: rising (low -> step up -> high) or falling; the chosen one is lit
pub fn edge_button(ui: &mut egui::Ui, rising: bool, selected: bool, accent: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 24.0), Sense::click());
    let resp = resp.on_hover_text(if rising { "Rising edge" } else { "Falling edge" });
    let p = ui.painter();
    let hot = resp.hovered();
    let bg = if selected { accent } else if hot { Color32::from_gray(64) } else { Color32::from_gray(48) };
    p.rect_filled(rect, CornerRadius::same(4), bg);
    let col = if selected { Color32::WHITE } else { Color32::from_gray(200) };
    let st = Stroke::new(1.8, col);
    let c = rect.center();
    let at = |x: f32, y: f32| Pos2::new(c.x + x, c.y + y);
    let (y_start, y_end) = if rising { (6.0, -6.0) } else { (-6.0, 6.0) };
    p.line_segment([at(-11.0, y_start), at(0.0, y_start)], st);
    p.line_segment([at(0.0, y_start), at(0.0, y_end)], st);
    p.line_segment([at(0.0, y_end), at(11.0, y_end)], st);
    // arrow head on the edge
    let dir = if rising { -1.0 } else { 1.0 };
    p.line_segment([at(0.0, dir * 2.5), at(-3.0, dir * 2.5 - dir * 3.5)], st);
    p.line_segment([at(0.0, dir * 2.5), at(3.0, dir * 2.5 - dir * 3.5)], st);
    resp
}

/// Shutter measurement: a single light pulse (up, then down); lit while the mode is on
pub fn pulse_button(ui: &mut egui::Ui, selected: bool, accent: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 24.0), Sense::click());
    let resp = resp.on_hover_text("Shutter measurement (single)\nwaits for one light pulse and shows it from start to end\nclick again for the next one, ▶ goes back to the running scope");
    let p = ui.painter();
    let hot = resp.hovered();
    let bg = if selected { accent } else if hot { Color32::from_gray(64) } else { Color32::from_gray(48) };
    p.rect_filled(rect, CornerRadius::same(4), bg);
    let col = if selected { Color32::WHITE } else { Color32::from_gray(200) };
    let st = Stroke::new(1.8, col);
    let c = rect.center();
    let at = |x: f32, y: f32| Pos2::new(c.x + x, c.y + y);
    p.line_segment([at(-12.0, 6.0), at(-5.0, 6.0)], st);
    p.line_segment([at(-5.0, 6.0), at(-5.0, -6.0)], st);
    p.line_segment([at(-5.0, -6.0), at(5.0, -6.0)], st);
    p.line_segment([at(5.0, -6.0), at(5.0, 6.0)], st);
    p.line_segment([at(5.0, 6.0), at(12.0, 6.0)], st);
    // up and down arrow heads on the two edges
    p.line_segment([at(-5.0, -1.0), at(-7.5, 1.5)], st);
    p.line_segment([at(-5.0, -1.0), at(-2.5, 1.5)], st);
    p.line_segment([at(5.0, 1.0), at(2.5, -1.5)], st);
    p.line_segment([at(5.0, 1.0), at(7.5, -1.5)], st);
    resp
}
