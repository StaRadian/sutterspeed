//! Pulse measurement view: the firmware captures a light pulse of any length as a rising-edge window, a
//! falling-edge window (both 2.5 us/sample) and a min/max envelope. The open time (shutter speed) is the time
//! from the light rising through the trigger level to it falling back through it, on the screen filter's output
//! when a filter is set.

use crate::scope::fmt_time;
use eframe::egui;
use egui::{Color32, RichText};
use egui_plot::{HLine, Line, LineStyle, Plot, PlotPoints, VLine};

const GREEN: Color32 = Color32::from_rgb(70, 180, 100);
const ORANGE: Color32 = Color32::from_rgb(230, 140, 40);
const RED: Color32 = Color32::from_rgb(220, 70, 60);
const BLUE: Color32 = Color32::from_rgb(90, 160, 230);
const TRACE: Color32 = Color32::from_rgb(240, 210, 60);

const NOMINAL: [f64; 19] = [
    1.0 / 8000.0, 1.0 / 4000.0, 1.0 / 2000.0, 1.0 / 1000.0, 1.0 / 500.0, 1.0 / 250.0, 1.0 / 125.0, 1.0 / 60.0, 1.0 / 30.0,
    1.0 / 15.0, 1.0 / 8.0, 1.0 / 4.0, 1.0 / 2.0, 1.0, 2.0, 4.0, 8.0, 15.0, 30.0,
];

pub fn speed_text(s: f64) -> String {
    if s >= 0.95 { format!("{s:.2} s") } else { format!("1/{:.1}", 1.0 / s) }
}

fn nominal_text(s: f64) -> String {
    if s >= 0.95 { format!("{s:.0} s") } else { format!("1/{:.0}", 1.0 / s) }
}

fn nearest_nominal(s: f64) -> f64 {
    *NOMINAL.iter().min_by(|a, b| (s / **a).log2().abs().total_cmp(&(s / **b).log2().abs())).unwrap()
}

fn ev_color(ev: f64) -> Color32 {
    let e = ev.abs();
    if e <= 1.0 / 3.0 { GREEN } else if e <= 2.0 / 3.0 { ORANGE } else { RED }
}

fn dec(e: i16) -> f64 {
    let e = e as i32;
    (if e < 16384 { e } else { 16384 + ((e - 16384) << 5) }) as f64
}


fn kv(line: &str, key: &str) -> Option<f64> {
    line.split_whitespace().find_map(|t| t.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
}

/// crossings of `lvl` (interpolated indices), upward or downward
fn crossings(a: &[f64], lvl: f64, up: bool) -> Vec<f64> {
    let s = if up { 1.0 } else { -1.0 };
    (1..a.len())
        .filter_map(|i| {
            let (x0, x1) = (s * (a[i - 1] - lvl), s * (a[i] - lvl));
            (x0 < 0.0 && x1 >= 0.0).then(|| (i - 1) as f64 + x0 / (x0 - x1))
        })
        .collect()
}

#[derive(Clone)]
pub struct Shot {
    pub seq: u32,
    pub width: f64, // s, rising to falling through the trigger level
    pub nominal: f64,
    pub ev: f64,
    pub clipped: bool,
    pub timeout: bool,
}

/// screen filter: (kind 0 off / 1 average / 2 low-pass, average points, cutoff Hz)
pub type Filt = (u8, f64, f64);

struct Capture {
    dt: f64,
    pre: usize, // index of the trigger sample in the rising window
    fall_start: f64,
    rise_raw: Vec<f64>, // lux, as sampled
    fall_raw: Vec<f64>,
    level: f64, // trigger level, lux
    clipped: bool,
    timeout: bool,
    seq: u32,
    rise: Vec<f64>, // lux, through the screen filter: what is measured and drawn
    fall: Vec<f64>,
    env: Vec<(f64, f64)>,
    env_bucket: f64,
    t_rise: Option<f64>, // crossing index in `rise`
    t_fall: Option<f64>, // crossing index in `fall`
    shot: Option<Shot>,
}

#[derive(Default)]
struct Raw {
    lines: Vec<String>, // as received, saved for checking
    head: String,
    head2: String,
    rise: Vec<i16>,
    fall: Vec<i16>,
    env: Vec<i16>,
    part: u8, // 0 rise, 1 fall, 2 env
}

#[derive(Default)]
pub struct PulseView {
    raw: Option<Raw>,
    last: Option<Capture>,
    pub shots: Vec<Shot>,
    seq: u32,
    note: String,
    filt: Filt,
    /// armed again: what is shown is the previous pulse (drawn dim until the next one)
    pub stale: bool,
}

impl Capture {
    /// Filter and time the pulse. The board triggers on an average, a little after the light itself passed the
    /// level: rising = the last upward crossing of the level up to the trigger (earlier ones are flicker around
    /// the level before the pulse), falling = the first downward crossing after the rising one.
    fn analyze(&mut self, f: Filt) {
        let filt = |x: &[f64]| if f.0 == 0 { x.to_vec() } else { crate::scope::ScopeView::filter_with(f, x, self.dt) };
        let (rise, fall) = (filt(&self.rise_raw), filt(&self.fall_raw));
        let (dt, pre, fs) = (self.dt, self.pre as f64, self.fall_start);
        let up = crossings(&rise, self.level, true);
        let tr = up.iter().copied().filter(|&t| t <= pre).last().or_else(|| up.first().copied());
        // falling, in trigger-relative samples: after the rising crossing
        let tf = tr.and_then(|r| crossings(&fall, self.level, false).into_iter().find(|&t| fs + t > r - pre));
        let shot = match (tr, tf) {
            (Some(r), Some(fc)) if fs + fc > r - pre => {
                let width = (fs + fc - (r - pre)) * dt;
                let nominal = nearest_nominal(width);
                Some(Shot { seq: self.seq, width, nominal, ev: (width / nominal).log2(), clipped: self.clipped, timeout: self.timeout })
            }
            _ => None,
        };
        (self.rise, self.fall, self.t_rise, self.t_fall, self.shot) = (rise, fall, tr, tf, shot);
    }
}

impl PulseView {
    /// Returns true when the line belonged to a pulse block.
    pub fn on_line(&mut self, ln: &str) -> bool {
        if ln.starts_with("#PULSE") {
            self.raw = Some(Raw { head: ln.to_string(), lines: vec![ln.to_string()], ..Default::default() });
            return true;
        }
        let Some(r) = &mut self.raw else { return false };
        r.lines.push(ln.to_string());
        if ln.starts_with("#P ") {
            r.head2 = ln.to_string();
        } else if ln.starts_with("#F") {
            r.part = 1;
        } else if ln.starts_with("#E") {
            r.part = 2;
        } else if ln.starts_with("#PEND") {
            let r = self.raw.take().unwrap();
            self.finish(r);
        } else {
            let v: Vec<i16> = crate::scope::line_samples(ln.trim(), r.head.contains("enc=3"));
            match r.part {
                0 => r.rise.extend(v),
                1 => r.fall.extend(v),
                _ => r.env.extend(v),
            }
        }
        true
    }

    fn finish(&mut self, r: Raw) {
        let h = &r.head;
        let h2 = &r.head2;
        let cpl = kv(h, "cpl").unwrap_or(15.02);
        let dt = kv(h, "dt_us").unwrap_or(2.5) * 1e-6;
        let pre = kv(h, "pre").unwrap_or(0.0) as usize;
        let fall_start = kv(h, "fstart").unwrap_or(0.0);
        let level = kv(h2, "lvl").unwrap_or(0.0) / cpl;
        let eb = kv(h2, "eb").unwrap_or(1.0);
        let clipped = kv(h2, "clip").unwrap_or(0.0) != 0.0;
        let timeout = kv(h2, "to").unwrap_or(0.0) != 0.0;
        // the last capture as received, for checking the measurement by hand
        if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
            let _ = std::fs::write(dir.join("sutterspeed_last_pulse.txt"), r.lines.join("\n"));
        }
        let rise: Vec<f64> = r.rise.iter().map(|&e| dec(e) / cpl).collect();
        let fall: Vec<f64> = r.fall.iter().map(|&e| dec(e) / cpl).collect();
        let env: Vec<(f64, f64)> = r.env.chunks(2).filter(|c| c.len() == 2).map(|c| (dec(c[0]) / cpl, dec(c[1]) / cpl)).collect();
        if rise.len() < pre.max(30) {
            return;
        }
        let mut c = Capture {
            dt, pre, fall_start, rise_raw: rise, fall_raw: fall, level, clipped, timeout, seq: self.seq + 1,
            rise: Vec::new(), fall: Vec::new(), env, env_bucket: eb, t_rise: None, t_fall: None, shot: None,
        };
        c.analyze(self.filt);
        self.stale = false;
        if let Some(s) = &c.shot {
            self.seq = s.seq;
            if !s.timeout {
                self.shots.push(s.clone());
            }
        }
        self.last = Some(c);
    }

    /// Filter setting of the scope; a change re-measures the last pulse with it.
    pub fn set_filter(&mut self, f: Filt) {
        if f == self.filt {
            return;
        }
        self.filt = f;
        if let Some(c) = &mut self.last {
            c.analyze(f);
            // the list shows the last shot as it is measured now
            if let (Some(s), Some(l)) = (&c.shot, self.shots.last_mut()) {
                if l.seq == s.seq {
                    *l = s.clone();
                }
            }
        }
    }

    // ---------------------------------------------------------------- right panel

    pub fn panel(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("SHUTTER").strong());
        match self.last.as_ref().and_then(|c| c.shot.clone()) {
            Some(s) => {
                egui::Grid::new("pres").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, k: &str, v: String| {
                        ui.label(k);
                        ui.label(RichText::new(v).monospace());
                        ui.end_row();
                    };
                    row(ui, "Open time", fmt_time(s.width));
                    row(ui, "Speed", speed_text(s.width));
                    row(ui, "Nominal", nominal_text(s.nominal));
                    row(ui, "Error", format!("{:+.1} %  {:+.2} EV", (s.width / s.nominal - 1.0) * 100.0, s.ev));
                });
                if s.clipped {
                    ui.label(RichText::new("Too bright: the sensor saturated").color(RED).strong());
                }
                if s.timeout {
                    ui.label(RichText::new("Longer than the max length: taken as a room-light change").color(ORANGE));
                }
            }
            None => {
                ui.label(RichText::new(if self.last.is_some() { "The level crossings were not found" } else { "no pulse yet" }).weak());
            }
        }
        ui.separator();
        ui.label(RichText::new("STATISTICS").strong());
        let n = self.shots.len();
        if n > 0 {
            let v: Vec<f64> = self.shots.iter().map(|s| s.width).collect();
            let mean = v.iter().sum::<f64>() / n as f64;
            let sd = if n > 1 { (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt() } else { 0.0 };
            let mn = v.iter().cloned().fold(f64::MAX, f64::min);
            let mx = v.iter().cloned().fold(f64::MIN, f64::max);
            egui::Grid::new("pstat").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                ui.label("Count");
                ui.label(format!("{n}"));
                ui.end_row();
                ui.label("Mean");
                ui.label(RichText::new(format!("{}  ({})", fmt_time(mean), speed_text(mean))).monospace());
                ui.end_row();
                ui.label("Std dev");
                ui.label(RichText::new(format!("{}  ({:.2} %)", fmt_time(sd), 100.0 * sd / mean)).monospace());
                ui.end_row();
                ui.label("Range");
                ui.label(RichText::new(format!("{} .. {}", fmt_time(mn), fmt_time(mx))).monospace());
                ui.end_row();
            });
            egui::ScrollArea::vertical().id_salt("pshots").max_height(160.0).stick_to_bottom(true).show(ui, |ui| {
                egui::Grid::new("pshotg").striped(true).num_columns(4).show(ui, |ui| {
                    for h in ["#", "Measured", "Nominal", "Error"] {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();
                    for s in &self.shots {
                        ui.label(format!("{}", s.seq));
                        ui.label(speed_text(s.width));
                        ui.label(nominal_text(s.nominal));
                        ui.label(RichText::new(format!("{:+.2} EV", s.ev)).color(ev_color(s.ev)));
                        ui.end_row();
                    }
                });
            });
        } else {
            ui.label(RichText::new("no shots yet").weak());
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(n > 0, egui::Button::new("Clear")).clicked() {
                self.shots.clear();
            }
            if ui.add_enabled(n > 0, egui::Button::new("Save CSV")).clicked() {
                self.export();
            }
        });
        if !self.note.is_empty() {
            ui.label(RichText::new(&self.note).small().weak());
        }
    }

    fn export(&mut self) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default();
        let path = dir.join(format!("sutterspeed_pulses_{secs}.csv"));
        let mut s = String::from("seq,width_s,speed,nominal,ev,clipped\n");
        for x in &self.shots {
            s += &format!(
                "{},{:.9},{},{},{:.3},{}\n",
                x.seq, x.width, speed_text(x.width), nominal_text(x.nominal), x.ev, x.clipped
            );
        }
        self.note = match std::fs::write(&path, s) {
            Ok(_) => format!("Saved: {}", path.display()),
            Err(e) => format!("Save failed: {e}"),
        };
    }

    // ---------------------------------------------------------------- graph

    /// The whole pulse in one graph: the min/max envelope from start to end, the opening and closing at full
    /// resolution on top, the trigger level and its rising / falling crossings marked, the open time written above.
    pub fn plot(&self, ui: &mut egui::Ui, armed: bool, level: f64, filtered: bool, (y0, y1, ydiv): (f64, f64, f64)) {
        ui.horizontal(|ui| {
            if armed {
                let how = if filtered { "filtered" } else { "40 us average" };
                ui.label(RichText::new(format!("WAITING FOR A PULSE  (light above {level:.0} lx, {how})")).color(ORANGE).strong());
            }
            if let Some(s) = self.last.as_ref().and_then(|c| c.shot.as_ref()) {
                ui.label(RichText::new(speed_text(s.width)).size(26.0).strong());
                ui.label(RichText::new(fmt_time(s.width)).size(20.0).color(TRACE));
                ui.label(RichText::new(format!("{}  {:+.2} EV", nominal_text(s.nominal), s.ev)).size(16.0).color(ev_color(s.ev)));
            }
        });
        let Some(c) = &self.last else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Shutter measurement: fire the shutter with a bright light in front of the lens.\nThe pulse is measured from the moment the light passes the trigger level.").weak());
            });
            return;
        };
        let mut end = (c.fall_start + c.fall.len() as f64) * c.dt;
        let mut start = -(c.pre as f64) * c.dt;
        if let (Some(r), Some(f)) = (c.t_rise, c.t_fall) {
            let (a, b) = ((r - c.pre as f64) * c.dt, (c.fall_start + f) * c.dt);
            let m = 0.2 * (b - a);
            (start, end) = ((a - m).max(start), (b + m).min(end));
        }
        let span = end - start;
        let (unit, un) = if span < 2e-3 { (1e6, "us") } else if span < 2.0 { (1e3, "ms") } else { (1.0, "s") };
        let (mut hi, mut lo) = (Vec::new(), Vec::new());
        for (i, &(a, b)) in c.env.iter().enumerate() {
            let t0 = i as f64 * c.env_bucket * c.dt * unit;
            let t1 = t0 + c.env_bucket * c.dt * unit;
            hi.extend([[t0, b], [t1, b]]);
            lo.extend([[t0, a], [t1, a]]);
        }
        let rise: Vec<[f64; 2]> = c.rise.iter().enumerate().map(|(i, &v)| [(i as f64 - c.pre as f64) * c.dt * unit, v]).collect();
        let fall: Vec<[f64; 2]> = c.fall.iter().enumerate().map(|(i, &v)| [(c.fall_start + i as f64) * c.dt * unit, v]).collect();
        let rise_raw: Vec<[f64; 2]> = c.rise_raw.iter().enumerate().map(|(i, &v)| [(i as f64 - c.pre as f64) * c.dt * unit, v]).collect();
        let fall_raw: Vec<[f64; 2]> = c.fall_raw.iter().enumerate().map(|(i, &v)| [(c.fall_start + i as f64) * c.dt * unit, v]).collect();
        let t_open = c.t_rise.map(|r| (r - c.pre as f64) * c.dt * unit);
        let t_close = c.t_fall.map(|f| (c.fall_start + f) * c.dt * unit);
        let top = c.env.iter().map(|e| e.1).chain(c.rise.iter().copied()).fold(c.level, f64::max);
        let (x0, x1) = (start * unit, end * unit);
        let tr = if self.stale { TRACE.gamma_multiply(0.35) } else { TRACE };
        let ty = (y1 - 0.4 * ydiv).min(top + 0.4 * ydiv).max(y0 + 0.4 * ydiv);
        Plot::new("pulse_all")
            .height(ui.available_height())
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .y_grid_spacer(move |g| crate::scope::div_marks(g, ydiv, 0.0))
            .x_axis_formatter(move |m, _| format!("{} {un}", (m.value * 1000.0).round() / 1000.0))
            .y_axis_formatter(|m, _| format!("{} lx", (m.value * 1000.0).round() / 1000.0))
            .show(ui, |p| {
                p.set_plot_bounds(egui_plot::PlotBounds::from_min_max([x0, y0], [x1, y1]));
                p.line(Line::new("max", PlotPoints::from(hi)).color(tr.gamma_multiply(0.45)));
                p.line(Line::new("min", PlotPoints::from(lo)).color(tr.gamma_multiply(0.45)));
                if self.filt.0 != 0 {
                    for (nm, raw) in [("opening raw", rise_raw), ("closing raw", fall_raw)] {
                        p.line(Line::new(nm, PlotPoints::from(raw)).color(tr.gamma_multiply(0.25)).width(1.0));
                    }
                }
                p.line(Line::new("opening", PlotPoints::from(rise)).color(tr).width(1.5));
                p.line(Line::new("closing", PlotPoints::from(fall)).color(tr).width(1.5));
                p.hline(HLine::new("trigger level", c.level).color(ORANGE).style(LineStyle::dashed_dense()));
                if let (Some(a), Some(b)) = (t_open, t_close) {
                    p.vline(VLine::new("rising", a).color(BLUE).width(1.5));
                    p.vline(VLine::new("falling", b).color(BLUE).width(1.5));
                    let w = (b - a) / unit;
                    p.text(
                        egui_plot::Text::new("w", egui_plot::PlotPoint::new((a + b) / 2.0, ty), RichText::new(format!("<- {} ->", fmt_time(w))).color(BLUE).size(15.0).strong()),
                    );
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a flat pulse of `width_n` samples with uniform noise +-amp and a 44.7 kHz flicker +-flick, in the two
    /// windows the board sends
    fn measure_fl(width_n: usize, base: f64, top: f64, amp: f64, flick: f64, level: f64, f: Filt) -> Capture {
        let (pre, win) = (436usize, 1744usize);
        let mut seed = 12345u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            ((seed >> 8) as f64 / (1u32 << 24) as f64 - 0.5) * 2.0 * amp
        };
        let lvl = |rel: i64| {
            let fl = flick * (2.0 * std::f64::consts::PI * 44.7e3 * rel as f64 * 2.5e-6).sin();
            fl + if rel >= 0 && (rel as usize) < width_n { top } else { base }
        };
        let rise: Vec<f64> = (0..win).map(|i| lvl(i as i64 - pre as i64) + noise()).collect();
        let fcount = (width_n + 40 + 436).max(1307);
        let fall_n = fcount.min(win);
        let fs = (fcount - fall_n) as f64;
        let fall: Vec<f64> = (0..fall_n).map(|i| lvl(fs as i64 + i as i64) + noise()).collect();
        let mut c = Capture {
            dt: 2.5e-6, pre, fall_start: fs, rise_raw: rise, fall_raw: fall, level, clipped: false,
            timeout: false, seq: 1, rise: vec![], fall: vec![], env: vec![], env_bucket: 16.0, t_rise: None, t_fall: None,
            shot: None,
        };
        c.analyze(f);
        c
    }

    fn measure(width_n: usize, base: f64, top: f64, amp: f64, level: f64, f: Filt) -> Capture {
        measure_fl(width_n, base, top, amp, 0.0, level, f)
    }

    #[test]
    fn flicker_crossing_the_level() {
        // room light 23 lx flickering +-80 lx (crosses the 85 lx level every 22 us), pulse to 1000 lx
        for n in [50usize, 400, 1600, 40000] {
            let w = measure_fl(n, 23.0, 1000.0, 5.0, 80.0, 85.0, (0, 1.0, 1e3)).shot.expect("measured").width;
            let want = n as f64 * 2.5e-6;
            assert!((w - want).abs() <= 2.5e-6, "n {n}: {w} vs {want}");
        }
    }

    #[test]
    fn rising_to_falling() {
        // room light 120 lx +-30, pulse to 1000 lx, level 400 lx: the noise never reaches the level
        for n in [50usize, 400, 1600, 4000, 40000] {
            let w = measure(n, 120.0, 1000.0, 30.0, 400.0, (0, 1.0, 1e3)).shot.expect("measured").width;
            let want = n as f64 * 2.5e-6;
            assert!((w - want).abs() <= 2.5e-6, "n {n}: {w} vs {want}");
        }
    }

    #[test]
    fn filter_for_a_noisy_level() {
        // noise +-150 lx around a level 100 lx above the room light: the 9-point average makes the crossings clean
        let w = measure(1600, 120.0, 1000.0, 150.0, 220.0, (1, 9.0, 1e3)).shot.expect("measured").width;
        assert!((w / 4e-3 - 1.0).abs() < 0.005, "{w}");
    }
}
