//! Rotary knob widget: grab it and turn (circle the pointer around it) or use the mouse wheel; click to reset.
//! The indicator follows the pointer, so the knob visibly rotates while it is being used. The value under it
//! can be typed (number only: the unit next to it is fixed).

use eframe::egui::{self, Color32, RichText, Sense, Stroke, Vec2};
use std::f32::consts::{PI, TAU};

pub struct KnobOut {
    /// turn clockwise since the last frame from dragging, in units of 7.5 degrees (48 per revolution)
    pub turn: f64,
    /// mouse-wheel notches clockwise (wheel up) since the last frame
    pub wheel: i32,
    /// clicked without dragging: reset to the default
    pub clicked: bool,
    /// the user let go of the knob: time to apply the value
    pub released: bool,
    /// being dragged right now
    pub active: bool,
    /// value typed into the field, in base units (an SI prefix like m / u / k may be typed with it)
    pub typed: Option<f64>,
}

const UNIT_ANGLE: f32 = PI / 24.0; // 7.5 degrees per unit
const WHEEL_ANGLE: f32 = PI / 6.0; // 30 degrees per wheel notch (drawing only)

#[allow(clippy::too_many_arguments)]
pub fn knob(ui: &mut egui::Ui, id: &str, label: &str, value: f64, unit: &str, si: &[(&str, f64)], accent: Color32, enabled: bool, hint: &str) -> KnobOut {
    knob_sized(ui, id, label, value, unit, si, accent, enabled, hint, false)
}

/// smaller knob for the secondary controls (positions, sample count, level)
#[allow(clippy::too_many_arguments)]
pub fn knob_small(ui: &mut egui::Ui, id: &str, label: &str, value: f64, unit: &str, si: &[(&str, f64)], accent: Color32, enabled: bool, hint: &str) -> KnobOut {
    knob_sized(ui, id, label, value, unit, si, accent, enabled, hint, true)
}

#[allow(clippy::too_many_arguments)]
fn knob_sized(ui: &mut egui::Ui, id: &str, label: &str, value: f64, unit: &str, si: &[(&str, f64)], accent: Color32, enabled: bool, hint: &str, small: bool) -> KnobOut {
    let (num, prefix) = si_split(value, si);
    let mut out = KnobOut { turn: 0.0, wheel: 0, clicked: false, released: false, active: false, typed: None };
    let key = egui::Id::new(("knob", id));
    ui.vertical(|ui| {
        ui.set_width(if small { 66.0 } else { 84.0 });
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(label).small().strong().color(if enabled { accent } else { Color32::GRAY }));
            let sense = if enabled { Sense::click_and_drag() } else { Sense::hover() };
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(if small { 36.0 } else { 52.0 }), sense);
            let resp = resp.on_hover_text(hint);
            let c = rect.center();

            // ---- drag: the angle of the pointer around the centre drives the knob
            let gkey = key.with("grab");
            let mut dang = 0.0f32;
            if enabled && resp.dragged() {
                out.active = true;
                if let Some(pos) = resp.interact_pointer_pos() {
                    let v = pos - c;
                    if v.length() > 6.0 {
                        let a = v.x.atan2(-v.y); // 0 = up, clockwise positive
                        if let Some(pa) = ui.data(|d| d.get_temp::<f32>(gkey)) {
                            let mut d = a - pa;
                            if d > PI {
                                d -= TAU;
                            } else if d < -PI {
                                d += TAU;
                            }
                            dang = d;
                        }
                        ui.data_mut(|d| d.insert_temp(gkey, a));
                    }
                }
            } else {
                ui.data_mut(|d| d.remove::<f32>(gkey));
            }
            out.turn = (dang / UNIT_ANGLE) as f64;

            // ---- wheel
            if enabled && resp.hovered() {
                let w: f32 = ui.input(|i| {
                    i.raw.events.iter().map(|e| match e {
                        egui::Event::MouseWheel { delta, .. } => (delta.y + delta.x).signum(),
                        _ => 0.0,
                    }).sum()
                });
                out.wheel = w as i32;
            }
            let mut ang: f32 = ui.data(|d| d.get_temp(key).unwrap_or(0.0));
            ang = (ang + dang + out.wheel as f32 * WHEEL_ANGLE).rem_euclid(TAU);
            ui.data_mut(|d| d.insert_temp(key, ang));
            out.clicked = enabled && resp.clicked();
            out.released = resp.drag_stopped();

            // ---- drawing
            let p = ui.painter();
            let r = if small { 14.0 } else { 21.0 };
            let hot = out.active || resp.hovered();
            let ring = if !enabled { Color32::from_gray(70) } else if hot { accent } else { Color32::from_gray(110) };
            for k in 0..24 {
                let a = k as f32 * TAU / 24.0;
                let dir = Vec2::new(a.sin(), -a.cos());
                let (r0, r1) = if k % 6 == 0 { (r + 1.5, r + if small { 3.5 } else { 5.0 }) } else { (r + 2.0, r + if small { 2.8 } else { 3.5 }) };
                p.line_segment([c + dir * r0, c + dir * r1], Stroke::new(1.0, Color32::from_gray(90)));
            }
            p.circle_filled(c + Vec2::new(0.0, 1.5), r, Color32::from_black_alpha(120)); // shadow
            p.circle_filled(c, r, Color32::from_gray(if hot { 52 } else { 44 }));
            for k in 0..16 {
                // knurled edge: ridges that turn with the knob
                let a = ang + k as f32 * TAU / 16.0;
                let dir = Vec2::new(a.sin(), -a.cos());
                p.line_segment([c + dir * (r - 4.0), c + dir * (r - 0.5)], Stroke::new(1.0, Color32::from_gray(70)));
            }
            p.circle_stroke(c, r, Stroke::new(1.5, ring));
            p.circle_filled(c, r * 0.62, Color32::from_gray(if hot { 62 } else { 54 }));
            let dir = Vec2::new(ang.sin(), -ang.cos());
            let col = if enabled { accent } else { Color32::GRAY };
            p.line_segment([c + dir * (r * 0.25), c + dir * (r - 3.0)], Stroke::new(if small { 2.2 } else { 3.0 }, col));
            p.circle_filled(c + dir * (r - 3.0), if small { 2.0 } else { 2.5 }, col);

            // ---- value, centred under the knob: click it to type a new one. The number and its SI prefix
            // (m, u, k ...) are editable, the unit (s/div, lx, S/s) stays.
            let ekey = key.with("edit");
            let mut editing: Option<String> = ui.data(|d| d.get_temp(ekey));
            let mut finish: Option<Option<f64>> = None; // Some(typed or None for cancel): leave edit mode
            if let Some(buf) = &mut editing {
                let ew = if small { 40.0 } else { 48.0 };
                let uw = unit.chars().count() as f32 * 6.5;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.add_space(((ui.available_width() - ew - uw - 2.0) / 2.0).max(0.0));
                    let r = ui.add(egui::TextEdit::singleline(buf).desired_width(ew).font(egui::TextStyle::Monospace));
                    if !r.has_focus() && !r.lost_focus() {
                        r.request_focus();
                    }
                    ui.label(RichText::new(unit).monospace().small());
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        finish = Some(None);
                    } else if r.lost_focus() {
                        finish = Some(si_parse(buf, unit, si));
                    }
                });
            } else {
                let mut job = egui::text::LayoutJob::default();
                let mono = egui::FontId::monospace(11.0);
                let fg = if enabled { Color32::from_gray(230) } else { Color32::GRAY };
                job.append(&format!("{num} {prefix}"), 0.0, egui::TextFormat { font_id: mono.clone(), color: fg, underline: Stroke::new(1.0, fg), ..Default::default() });
                job.append(unit, 0.0, egui::TextFormat { font_id: mono, color: Color32::from_gray(150), ..Default::default() });
                let r = ui.add(egui::Label::new(job).sense(if enabled { Sense::click() } else { Sense::hover() }));
                if enabled && r.on_hover_text("Click to type (a number with a prefix like m, u, k. Enter = apply, Esc = cancel)").clicked() {
                    editing = Some(format!("{num}{prefix}"));
                }
            }
            if let Some(t) = finish {
                out.typed = t;
                editing = None;
            }
            ui.data_mut(|d| match editing {
                Some(b) => {
                    d.insert_temp(ekey, b);
                }
                None => d.remove::<String>(ekey),
            });
        });
    });
    out
}

pub const SI_TIME: &[(&str, f64)] = &[("", 1.0), ("m", 1e-3), ("u", 1e-6), ("n", 1e-9)];
pub const SI_LUX: &[(&str, f64)] = &[("k", 1e3), ("", 1.0)];
pub const SI_RATE: &[(&str, f64)] = &[("k", 1e3), ("", 1.0)];

/// value -> (number, prefix) with the largest allowed prefix that keeps the number >= 1
fn si_split(v: f64, si: &[(&str, f64)]) -> (String, String) {
    let a = v.abs();
    if a == 0.0 {
        return ("0".into(), si.iter().find(|x| x.1 == 1.0).map(|x| x.0).unwrap_or("").to_string());
    }
    let (p, f) = si.iter().copied().filter(|(_, f)| a >= *f * 0.9995).max_by(|x, y| x.1.total_cmp(&y.1))
        .unwrap_or_else(|| *si.iter().min_by(|x, y| x.1.total_cmp(&y.1)).unwrap());
    let x = v / f;
    let s = if x.abs() >= 100.0 { format!("{x:.1}") } else if x.abs() >= 10.0 { format!("{x:.2}") } else { format!("{x:.3}") };
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    (if s == "-0" { "0".into() } else { s }, p.to_string())
}

/// "5m", "200 u", "1.5k", "3", "10ms/div" -> base units; without a prefix the number is taken as it is
fn si_parse(t: &str, unit: &str, si: &[(&str, f64)]) -> Option<f64> {
    let mut t = t.trim().replace(',', ".").replace('µ', "u").replace(' ', "");
    if let Some(s) = t.strip_suffix(unit) {
        t = s.to_string();
    }
    let (num, mul) = match t.chars().last() {
        Some(c) if c.is_ascii_alphabetic() => {
            let p = c.to_string();
            let f = si.iter().find(|(q, _)| *q == p).map(|x| x.1).or(match c {
                'n' => Some(1e-9),
                'u' => Some(1e-6),
                'm' => Some(1e-3),
                'k' | 'K' => Some(1e3),
                'M' => Some(1e6),
                _ => None,
            })?;
            (t[..t.len() - 1].to_string(), f)
        }
        _ => (t.clone(), 1.0),
    };
    num.parse::<f64>().ok().map(|v| v * mul).filter(|v| v.is_finite())
}
