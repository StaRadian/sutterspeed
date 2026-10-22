//! Light oscilloscope view: talks to the firmware's `scope` command, draws frames / the roll stream with a
//! fixed div grid, and offers the usual scope controls (time/div, trigger, run/stop/single, cursors,
//! automatic measurements).

use eframe::egui;
use egui::{Color32, RichText};
use egui_plot::{GridInput, GridMark, HLine, Line, LineStyle, Plot, PlotBounds, PlotPoint, PlotPoints, Points, Text, VLine};
use crate::icons::{edge_button, pulse_button, run_button};
use crate::knob::{knob_small_zero, knob, knob_small, SI_LUX, SI_RATE, SI_TIME};
use crate::pulse::PulseView;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// minimum trigger width steps for the wheel (0 = off)
const MINW_STEPS: [f64; 10] = [0.0, 5e-6, 10e-6, 20e-6, 50e-6, 100e-6, 200e-6, 500e-6, 1e-3, 2e-3];
const MINW_MAX: f64 = 2.5e-3;
/// shutter trigger: two stages of y += (x - y) / 8, about 40 us; room-light flicker of tens of kHz is gone
const PULSE_AVG_K: u32 = 3;
const N_MAX: f64 = 4000.0; // firmware SCOPE_NMAX
const RATE_STEPS: [f64; 13] = [100.0, 200.0, 500.0, 1e3, 2e3, 5e3, 10e3, 20e3, 50e3, 100e3, 200e3, 400e3, 400e3];
const SCAN_S: f64 = 2.5e-6; // one ADC scan
const DEC_MAX: u32 = 4096; // firmware SCOPE_DECMAX: slowest sample = mean of 4096 scans
const POST_MAX: u32 = 4_000_000; // firmware SCOPE_POSTMAX
const TRACE: Color32 = Color32::from_rgb(240, 210, 60);
const TRIG: Color32 = Color32::from_rgb(240, 140, 40);
const CUR: Color32 = Color32::from_rgb(110, 200, 240);
const ORANGE: Color32 = Color32::from_rgb(230, 140, 40);

const TDIV: [f64; 18] = [
    10e-6, 20e-6, 50e-6, 100e-6, 200e-6, 500e-6, 1e-3, 2e-3, 5e-3, 10e-3, 20e-3, 50e-3, 100e-3, 200e-3, 500e-3, 1.0, 2.0, 5.0,
];
const MA_STEPS: [f64; 10] = [2.0, 3.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0];
const LP_STEPS: [f64; 14] = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1e3, 2e3, 5e3, 10e3, 20e3];
const ROLL_FROM: f64 = 0.2; // screens this long and longer (20 ms/div up) roll live without a trigger; shorter ones
                            // free-run or trigger
const STREAM_DEC_MIN: u32 = 4; // the live stream goes up to 100 kS/s (USB)
const LIVE_MAX_PTS: f64 = 20000.0; // live display: samples per screen at most
const YDIV: [f64; 22] = [
    0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1e3, 2e3, 5e3, 1e4, 2e4, 5e4, 1e5, 2e5, 5e5, 1e6,
];

/// number with up to 3 decimals, trailing zeros removed
fn fmt_num(x: f64) -> String {
    let s = format!("{x:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

/// time split into (number, unit, seconds per unit); the unit follows `scale`
fn split_time(v: f64, scale: f64) -> (String, &'static str, f64) {
    let a = scale.abs();
    let (u, f) = if a < 1e-3 { ("us", 1e-6) } else if a < 1.0 { ("ms", 1e-3) } else { ("s", 1.0) };
    (fmt_num(v / f), u, f)
}

/// next 1-2-5 value up (dir > 0) or down from v
fn step125(list: &[f64], v: f64, dir: i32) -> f64 {
    let mut x = v;
    for _ in 0..dir.unsigned_abs() {
        x = if dir > 0 {
            *list.iter().find(|&&d| d > x * 1.0001).unwrap_or(&list[list.len() - 1])
        } else {
            *list.iter().rev().find(|&&d| d < x * 0.9999).unwrap_or(&list[0])
        };
    }
    x
}

pub fn fmt_time(s: f64) -> String {
    let a = s.abs();
    if a < 1e-3 { format!("{:.1} us", s * 1e6) } else if a < 1.0 { format!("{:.3} ms", s * 1e3) } else { format!("{s:.3} s") }
}

fn fmt_div(s: f64) -> String {
    let (n, u, _) = split_time(s, s);
    format!("{n} {u}")
}

fn fmt_lux(v: f64) -> String {
    let a = v.abs();
    if a >= 1e4 { format!("{:.1} klx", v / 1e3) } else if a >= 100.0 { format!("{v:.0} lx") } else if a >= 1.0 { format!("{v:.1} lx") } else { format!("{v:.2} lx") }
}

fn fmt_freq(f: f64) -> String {
    if f >= 1e3 { format!("{:.3} kHz", f / 1e3) } else { format!("{f:.2} Hz") }
}

/// decoded firmware sample -> counts
fn dec(e: i16) -> f64 {
    let e = e as i32;
    (if e < 16384 { e } else { 16384 + ((e - 16384) << 5) }) as f64
}

/// samples of a data line: 4 hex digits each (firmware <= 0.2) or 3 characters of 6 bits ('0' + n, 0.3+)
pub fn line_samples(s: &str, enc3: bool) -> Vec<i16> {
    let b = s.as_bytes();
    if enc3 {
        b.chunks_exact(3)
            .map(|c| (((c[0].wrapping_sub(b'0') as u16 & 63) << 12) | ((c[1].wrapping_sub(b'0') as u16 & 63) << 6) | (c[2].wrapping_sub(b'0') as u16 & 63)) as i16)
            .collect()
    } else {
        b.chunks_exact(4).filter_map(|c| u16::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok().map(|u| u as i16)).collect()
    }
}

fn kv(line: &str, key: &str) -> Option<f64> {
    line.split_whitespace().find_map(|t| t.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
}

struct Frame {
    lux: Vec<f64>,
    dt: f64,
    trig: f64, // index of the trigger sample in the frame (may be negative: trigger delay)
    #[allow(dead_code)] // kept for a future "auto" marker
    forced: bool,
}

#[derive(Default)]
struct Meas {
    max: f64,
    min: f64,
    mean: f64,
    period: Option<f64>,
    width: Option<f64>,
    rise: Option<f64>,
}

fn measure(t: &[f64], v: &[f64]) -> Option<Meas> {
    if v.len() < 2 {
        return None;
    }
    let max = v.iter().cloned().fold(f64::MIN, f64::max);
    let min = v.iter().cloned().fold(f64::MAX, f64::min);
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let mut m = Meas { max, min, mean, ..Default::default() };
    let pp = max - min;
    if pp <= 1e-9 {
        return Some(m);
    }
    let mid = (max + min) / 2.0;
    let (hi, lo) = (mid + 0.1 * pp, mid - 0.1 * pp);
    let at = |i: usize, lvl: f64| t[i - 1] + (t[i] - t[i - 1]) * (lvl - v[i - 1]) / (v[i] - v[i - 1]);
    let (mut ups, mut downs) = (Vec::new(), Vec::new());
    let mut low = v[0] < lo;
    let mut high = v[0] > hi;
    for i in 1..v.len() {
        if v[i] < lo {
            low = true;
        }
        if v[i] > hi {
            high = true;
        }
        if low && v[i - 1] < mid && v[i] >= mid {
            ups.push(at(i, mid));
            low = false;
        }
        if high && v[i - 1] >= mid && v[i] < mid {
            downs.push(at(i, mid));
            high = false;
        }
    }
    if ups.len() >= 2 {
        m.period = Some((ups[ups.len() - 1] - ups[0]) / (ups.len() - 1) as f64);
    }
    let widths: Vec<f64> = ups.iter().filter_map(|&u| downs.iter().find(|&&d| d > u).map(|&d| d - u)).collect();
    if !widths.is_empty() {
        m.width = Some(widths.iter().sum::<f64>() / widths.len() as f64);
    }
    // 10-90 % rise time of the first rising edge
    let (l10, l90) = (min + 0.1 * pp, min + 0.9 * pp);
    if let Some(&u) = ups.first() {
        let k = t.iter().position(|&x| x >= u).unwrap_or(0);
        let a = (1..=k).rev().find(|&i| v[i - 1] < l10 && v[i] >= l10).map(|i| at(i, l10));
        let b = (k.max(1)..v.len()).find(|&i| v[i - 1] < l90 && v[i] >= l90).map(|i| at(i, l90));
        if let (Some(a), Some(b)) = (a, b) {
            if b > a {
                m.rise = Some(b - a);
            }
        }
    }
    Some(m)
}

/// Points to draw for (t, v): all of them when they are few, otherwise per pixel column its first, min, max and
/// last value in time order (what the eye would see anyway, peaks included).
fn minmax_columns(t: &[f64], v: &[f64], unit: f64, x0: f64, x1: f64, cols: usize) -> Vec<[f64; 2]> {
    if t.len() <= 2 * cols || x1 <= x0 {
        return t.iter().zip(v).map(|(&a, &b)| [a * unit, b]).collect();
    }
    let mut out = Vec::with_capacity(4 * cols);
    let w = (x1 - x0) / cols as f64;
    let mut i = 0;
    while i < t.len() {
        let col = ((t[i] - x0) / w).floor();
        let (mut j, mut lo, mut hi) = (i, i, i);
        while j < t.len() && ((t[j] - x0) / w).floor() == col {
            if v[j] < v[lo] {
                lo = j;
            }
            if v[j] > v[hi] {
                hi = j;
            }
            j += 1;
        }
        let mut idx = [i, lo, hi, j - 1];
        idx.sort_unstable();
        let mut last = usize::MAX;
        for k in idx {
            if k != last {
                out.push([t[k] * unit, v[k]]);
                last = k;
            }
        }
        i = j;
    }
    out
}

fn nice_div(x: f64) -> f64 {
    *YDIV.iter().find(|&&d| d >= x).unwrap_or(&YDIV[YDIV.len() - 1])
}

/// a screen kept as it was: samples (time from the trigger, filtered, raw) and the time range shown
#[derive(Clone)]
struct Frozen {
    t: Vec<f64>,
    v: Vec<f64>,
    raw: Vec<f64>,
    x0: f64,
    x1: f64,
}

pub(crate) fn div_marks(input: GridInput, div: f64, origin: f64) -> Vec<GridMark> {
    let (a, b) = input.bounds;
    if div <= 0.0 || !(b > a) || (b - a) / div > 200.0 {
        return Vec::new();
    }
    let k0 = ((a - origin) / div).ceil() as i64;
    let k1 = ((b - origin) / div).floor() as i64;
    (k0..=k1).map(|k| GridMark { value: origin + k as f64 * div, step_size: div }).collect()
}

pub struct ScopeView {
    tdiv_v: f64, // s/div, any value
    pub pulse_mode: bool,
    n: f64, // samples per frame (16 .. 4000): fewer = a shorter record and less USB traffic
    enc3: bool, // current frame uses 3-character samples
    data_ver: u64,
    #[allow(clippy::type_complexity)]
    vis_cache: std::cell::RefCell<Option<((u64, u64, u64, bool, u8, usize, usize), std::rc::Rc<(Vec<f64>, Vec<f64>, Vec<f64>)>, Option<Option<std::rc::Rc<Meas>>>)>>,
    trig_on: bool, // trigger on: frames aligned on the trigger; off: the live (rolling) stream
    maxp_s: f64, // pulse mode: longest pulse
    pulse: PulseView,
    dec_manual: Option<u32>, // sample = mean of this many scans, set by hand (None = from the time base)
    applied_tdiv: f64,
    last_cmd: String, // configuration the board has now
    delay: f64, // screen centre relative to the trigger, s (+ = later than the trigger)
    level_lx: f64,
    falling: bool,
    mode: u8, // 0 auto, 1 normal, 2 single
    filt: u8, // screen: 0 none, 1 moving average, 2 low-pass
    ma_n: f64,  // moving average: samples
    lp_hz: f64, // low-pass cutoff
    show_raw: bool,
    running: bool,
    auto_y: bool,     // automatic scale (lx/div)
    auto_ypos: bool,  // automatic centre
    ydiv_v: f64, // lx/div, any value
    yoff: f64,
    cpl: f64,
    frame: Option<Frame>,
    frame_at: Option<Instant>,
    frames: VecDeque<Instant>,
    collecting: Option<(Vec<i16>, f64, f64, bool)>, // samples, dt, trigger index, forced
    roll: VecDeque<f64>,
    cur_a: Option<f64>,
    cur_b: Option<f64>,
    note: String,
    y_target: Option<(f64, f64)>,
    y_fast_until: Option<Instant>, // quick glide after a click back to auto
    trig_lag: f64, // samples the board's trigger filter lags behind
    lvl_show_until: Option<Instant>,
    frozen: Option<Frozen>,
    min_w: f64, // trigger only on light that stays past the level this long (s); 0 = off // shutter measurement armed from the scope: its last screen, until a pulse arrives // trigger off: level line shown briefly after a change
    roll_key: (bool, u32), // live stream the roll buffer belongs to (roll, scans per sample)
    pulse_prev: Option<(u8, bool)>, // mode and trigger before the shutter measurement // automatic vertical scale: (lx/div, centre) the view glides to
    level_dragging: bool,
    pos_dragging: bool,
    pos_commit: bool,
    level_commit: bool,
}

impl ScopeView {
    pub fn new() -> Self {
        ScopeView {
            tdiv_v: 10e-3, // room light flicker
            pulse_mode: false,
            n: N_MAX,
            enc3: false,
            data_ver: 0,
            vis_cache: Default::default(),
            trig_on: false,
            maxp_s: 40.0,
            pulse: PulseView::default(),
            dec_manual: None,
            applied_tdiv: 10e-3,
            last_cmd: String::new(),
            delay: 0.0,
            level_lx: 20.0,
            falling: false,
            mode: 0,
            filt: 0,
            ma_n: 9.0,
            lp_hz: 100.0,
            show_raw: true,
            running: true,
            auto_y: true,
            auto_ypos: true,
            ydiv_v: 10.0,
            yoff: 0.0,
            cpl: 15.02,
            frame: None,
            frame_at: None,
            frames: VecDeque::new(),
            collecting: None,
            roll: VecDeque::new(),
            cur_a: None,
            cur_b: None,
            note: String::new(),
            y_target: None,
            y_fast_until: None,
            trig_lag: 0.0,
            roll_key: (false, 0),
            lvl_show_until: None,
            frozen: None,
            min_w: 0.0,
            pulse_prev: None,
            level_dragging: false,
            pos_dragging: false,
            pos_commit: false,
            level_commit: false,
        }
    }

    fn tdiv(&self) -> f64 {
        self.tdiv_v
    }
    fn window(&self) -> f64 {
        10.0 * self.tdiv()
    }
    /// live (rolling) display instead of triggered frames
    fn is_roll(&self) -> bool {
        if self.pulse_mode {
            return false;
        }
        !self.trig_possible()
    }

    /// the trigger works up to 10 ms/div; slower time bases roll live
    fn trig_possible(&self) -> bool {
        self.window() < ROLL_FROM
    }

    /// Trigger ON in the roll range (the time base was turned there): switch it off, back to AUTO.
    fn drop_trigger_if_rolling(&mut self, out: &mut Vec<String>) {
        if self.trig_on && !self.pulse_mode && !self.trig_possible() {
            self.trig_on = false;
            self.mode = 0;
            self.running = true;
            self.apply(out);
        }
    }
    /// (scans per sample, samples captured after the trigger, sample interval)
    fn geom(&self) -> (u32, u32, f64) {
        let w = self.window();
        if self.is_roll() {
            let cap = (w / LIVE_MAX_PTS / SCAN_S).ceil() as u32; // keep the screen light
            let auto = (w / 1000.0 / SCAN_S).round().max(1.0) as u32;
            let d = self.dec_manual.unwrap_or(auto).max(STREAM_DEC_MIN).max(cap).clamp(1, DEC_MAX);
            return (d, 0, SCAN_S * d as f64);
        }
        // auto: the slowest rate whose record (N_MAX samples) still covers the screen
        let d = self.dec_manual.unwrap_or(((w / (self.n * SCAN_S) - 1e-6).ceil() as u32).max(1)).clamp(1, DEC_MAX);
        let dt = SCAN_S * d as f64;
        // the frame (N samples) is centred on the screen centre
        let post = (self.delay / dt + self.n / 2.0).round().clamp(0.0, POST_MAX as f64) as u32;
        (d, post, dt)
    }

    /// samples per frame: always the most the board holds
    fn n_eff(&self) -> f64 {
        if self.is_roll() { N_MAX } else { self.n }
    }

    /// limits of the screen-centre position: the screen must stay inside the frame
    fn delay_range(&self) -> (f64, f64) {
        let dt = SCAN_S * self.geom().0 as f64;
        let n = self.n_eff();
        let half = (self.window() / 2.0).min((n - 1.0) * dt / 2.0);
        (-(n - 1.0) * dt + half, (POST_MAX as f64 - n / 2.0) * dt)
    }

    fn clamp_delay(&mut self) {
        let (a, b) = self.delay_range();
        self.delay = self.delay.clamp(a, b);
    }

    pub fn config_cmd(&self) -> String {
        let (dec, post, _) = self.geom();
        // the board's trigger filter fires 2^k - 1 samples after the real crossing: end the record that much
        // earlier, so it still covers the screen once the viewer moves the trigger back onto the crossing
        let post = post.saturating_sub((1u32 << self.trig_filter_k()) - 1);
        let trig = (self.trig_on && self.trig_possible()) || self.pulse_mode; // the shutter measurement always triggers
        // minimum width in samples of the capture (2.5 us in the shutter measurement)
        let minw = if trig && self.min_w > 0.0 {
            let dt = if self.pulse_mode { SCAN_S } else { self.geom().2 };
            ((self.min_w / dt).ceil() as u32).clamp(2, if self.pulse_mode { 1000 } else { 100_000 })
        } else {
            0
        };
        format!(
            "scope set {dec} {} {post} {} {} {} {} {} {} {} {minw}",
            self.n_eff().round() as u32,
            if trig { (self.level_lx * self.cpl).round() as i64 } else { 0 }, // free run ignores them
            if self.falling && trig && !self.pulse_mode { 'f' } else { 'r' }, // shutter: a bright pulse
            if trig { ['a', 'n', 's'][self.mode as usize] } else { 'f' }, // f: free run (no trigger)
            (self.is_roll() && !self.pulse_mode) as u8,
            self.pulse_mode as u8,
            (self.maxp_s * 1000.0).round() as u32,
            self.trig_filter_k()
        )
    }

    /// Board-side trigger smoothing (y += (x - y) / 2^k) that matches the screen filter, so the trigger
    /// fires on the filtered trace. 0 = raw.
    fn trig_filter_k(&self) -> u32 {
        if !(self.trig_on || self.pulse_mode) || self.is_roll() {
            return 0;
        }
        if self.pulse_mode && self.filt == 0 {
            return PULSE_AVG_K; // the shutter trigger always looks at a short average (what the screen shows)
        }
        let dt = self.geom().2;
        let a = match self.filt {
            1 => 2.0 / (self.ma_n.round() + 1.0),
            2 => 1.0 - (-2.0 * std::f64::consts::PI * self.lp_hz * dt).exp(),
            _ => return 0,
        };
        if !(a > 0.0) {
            return 0;
        }
        let k = (1.0 / a).log2().round().clamp(0.0, 15.0) as u32;
        // shutter: the average's delay (2 x 2^k samples) must stay inside the 436 samples kept before the trigger
        if self.pulse_mode { k.clamp(PULSE_AVG_K, 7) } else { k }
    }

    /// Commands to (re)start the scope after connecting or switching to the scope view.
    pub fn start_cmds(&mut self) -> Vec<String> {
        self.last_cmd = self.config_cmd();
        let mut v = vec!["lux off".into(), "scope on".into(), self.last_cmd.clone()];
        if !self.running {
            v.push("scope stop".into());
        }
        v
    }

    /// Send the configuration if it differs from what the board has (a knob turned against its limit, or a
    /// change that does not alter the capture, restarts nothing). The old frame stays on screen until a new one.
    fn apply(&mut self, out: &mut Vec<String>) {
        let cmd = self.config_cmd();
        if cmd == self.last_cmd {
            return;
        }
        self.apply_force(out);
    }

    /// Send the configuration even if unchanged (SINGLE: arm once more).
    fn apply_force(&mut self, out: &mut Vec<String>) {
        let cmd = self.config_cmd();
        self.applied_tdiv = self.tdiv_v;
        // keep what is on screen: only a different live stream (roll on / off, sample interval) starts it anew;
        // trigger level, edge or mode do not touch the data
        let roll_key = (self.is_roll(), self.geom().0);
        if roll_key != self.roll_key {
            self.roll.clear();
            self.roll_key = roll_key;
        }
        if !level_only(&cmd, &self.last_cmd) {
            self.collecting = None; // the board restarts: the rest of a frame in flight is stale
        }
        self.trig_lag = ((1u32 << self.trig_filter_k()) - 1) as f64;
        self.last_cmd = cmd.clone();
        out.push(cmd);
        if !self.running {
            out.push("scope stop".into());
        }
    }

    /// Returns true when the line belonged to the scope protocol.
    pub fn on_line(&mut self, ln: &str) -> bool {
        self.pulse.set_filter((self.filt, self.ma_n, self.lp_hz));
        if self.pulse.on_line(ln) {
            if ln.starts_with("#PEND") && self.mode == 2 {
                self.running = false;
            }
            return true;
        }
        if let Some((buf, dt, pre, forced)) = &mut self.collecting {
            if ln.starts_with("#SEND") {
                let (buf, dt, trig, forced) = (std::mem::take(buf), *dt, *pre, *forced);
                self.collecting = None;
                if buf.len() >= 16 {
                    let cpl = self.cpl;
                    self.frame = Some(Frame { lux: buf.into_iter().map(|e| dec(e) / cpl).collect(), dt, trig, forced });
                    self.data_ver += 1;
                    let now = Instant::now();
                    self.frame_at = Some(now);
                    self.frames.push_back(now);
                    while self.frames.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(1)) {
                        self.frames.pop_front();
                    }
                    if self.mode == 2 {
                        self.running = false;
                    }
                }
            } else {
                buf.extend(line_samples(ln.trim(), self.enc3));
            }
            return true;
        }
        if ln.starts_with("#SCOPE") {
            self.enc3 = ln.contains("enc=3");
            let dec = kv(ln, "dec").unwrap_or_else(|| (1u64 << kv(ln, "sh").unwrap_or(0.0) as u32) as f64);
            let dt = kv(ln, "dt_us").unwrap_or(2.5) * 1e-6 * dec;
            if let Some(c) = kv(ln, "cpl") {
                self.cpl = c;
            }
            let pre = kv(ln, "trig").or_else(|| kv(ln, "pre")).unwrap_or(0.0) - self.trig_lag;
            let forced = kv(ln, "forced").unwrap_or(0.0) != 0.0;
            self.collecting = Some((Vec::with_capacity(kv(ln, "n").unwrap_or(2000.0) as usize), dt, pre, forced));
            return true;
        }
        if let Some((h, e3)) = ln.strip_prefix("#R ").map(|h| (h, false)).or_else(|| ln.strip_prefix("#r ").map(|h| (h, true))) {
            if self.is_roll() && self.running {
                let cpl = self.cpl;
                self.roll.extend(line_samples(h.trim(), e3).into_iter().map(|e| dec(e) / cpl));
                self.data_ver += 1;
                let (_, _, dt) = self.geom();
                let keep = (self.window() / dt).ceil() as usize + 2;
                while self.roll.len() > keep {
                    self.roll.pop_front();
                }
            }
            return true;
        }
        ln.starts_with("scope ")
    }

    pub fn simulate(&mut self) {
        let (_, post, dt) = self.geom();
        let n = self.n_eff() as usize;
        let trig = (n - 1) as f64 - post as f64;
        let lux: Vec<f64> = (0..n)
            .map(|i| {
                let t = (i as f64 - trig) * dt;
                20.0 + 15.0 * (2.0 * std::f64::consts::PI * 120.0 * t - std::f64::consts::FRAC_PI_2).sin().abs().powf(1.5) - 3.0
            })
            .collect();
        self.frame = Some(Frame { lux, dt, trig, forced: false });
        self.data_ver += 1;
        self.frame_at = Some(Instant::now());
    }

    /// screen filter over a whole record with sample interval dt
    fn filtered(&self, x: &[f64], dt: f64) -> Vec<f64> {
        Self::filter_with((self.filt, self.ma_n, self.lp_hz), x, dt)
    }

    /// (kind 0 off / 1 average / 2 low-pass, average points, cutoff Hz) over a record with sample interval dt
    pub(crate) fn filter_with((kind, ma_n, lp_hz): (u8, f64, f64), x: &[f64], dt: f64) -> Vec<f64> {
        match kind {
            1 => {
                let n = ma_n.round().max(1.0) as usize;
                let h = n / 2;
                let mut pre = vec![0.0; x.len() + 1];
                for (i, &v) in x.iter().enumerate() {
                    pre[i + 1] = pre[i] + v;
                }
                (0..x.len())
                    .map(|i| {
                        let (a, b) = (i.saturating_sub(h), (i + h + 1).min(x.len()));
                        (pre[b] - pre[a]) / (b - a) as f64
                    })
                    .collect()
            }
            2 => {
                // single-pole low-pass run forward and backward: no delay, 2nd-order roll-off
                let a = 1.0 - (-2.0 * std::f64::consts::PI * lp_hz * dt).exp();
                let mut y = x.to_vec();
                if let Some(&f) = x.first() {
                    let mut acc = f;
                    for v in y.iter_mut() {
                        acc += a * (*v - acc);
                        *v = acc;
                    }
                    let mut acc = *y.last().unwrap();
                    for v in y.iter_mut().rev() {
                        acc += a * (*v - acc);
                        *v = acc;
                    }
                }
                y
            }
            _ => x.to_vec(),
        }
    }

    fn sample_dt(&self) -> f64 {
        match (&self.frame, self.is_roll()) {
            (Some(f), false) => f.dt,
            _ => self.geom().2,
        }
    }

    /// visible samples as (time from trigger in s, filtered lux, raw lux)
    fn visible3_calc(&self) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let w = self.window();
        let (mut t, mut v, mut r) = (Vec::new(), Vec::new(), Vec::new());
        if self.is_roll() {
            let (_, _, dt) = self.geom();
            let raw: Vec<f64> = self.roll.iter().copied().collect();
            let fl = self.filtered(&raw, dt);
            let n = raw.len();
            for i in 0..n {
                let ti = -((n - 1 - i) as f64) * dt;
                if ti >= -w {
                    t.push(ti);
                    v.push(fl[i]);
                    r.push(raw[i]);
                }
            }
        } else if let Some(f) = &self.frame {
            let (a, b) = (self.delay - w / 2.0, self.delay + w / 2.0);
            let fl = self.filtered(&f.lux, f.dt);
            for (i, &x) in f.lux.iter().enumerate() {
                let ti = (i as f64 - f.trig) * f.dt;
                if ti >= a - f.dt && ti <= b + f.dt {
                    t.push(ti);
                    v.push(fl[i]);
                    r.push(x);
                }
            }
        }
        (t, v, r)
    }

    /// what decides the visible data: new data, the window, the screen filter
    fn vis_key(&self) -> (u64, u64, u64, bool, u8, usize, usize) {
        (self.data_ver, self.tdiv_v.to_bits(), self.delay.to_bits(), self.is_roll(), self.filt, self.ma_n.to_bits() as usize, self.lp_hz.to_bits() as usize)
    }

    /// visible samples (time, filtered, raw), computed once per change of data or view
    fn visible3(&self) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let key = self.vis_key();
        if let Some((k, d, _)) = &*self.vis_cache.borrow() {
            if *k == key {
                return (**d).clone();
            }
        }
        let d = std::rc::Rc::new(self.visible3_calc());
        *self.vis_cache.borrow_mut() = Some((key, d.clone(), None));
        (*d).clone()
    }

    /// automatic measurements of the visible (filtered) data, cached like visible3
    fn measured(&self) -> Option<std::rc::Rc<Meas>> {
        let key = self.vis_key();
        if let Some((k, _, Some(m))) = &*self.vis_cache.borrow() {
            if *k == key {
                return m.clone();
            }
        }
        let (t, v) = self.visible();
        let m = measure(&t, &v).map(std::rc::Rc::new);
        if let Some((k, _, mm)) = &mut *self.vis_cache.borrow_mut() {
            if *k == key {
                *mm = Some(m.clone());
            }
        }
        m
    }

    fn visible(&self) -> (Vec<f64>, Vec<f64>) {
        let (t, v, _) = self.visible3();
        (t, v)
    }


    // ---------------------------------------------------------------- controls panel

    pub fn controls(&mut self, ui: &mut egui::Ui, connected: bool) -> Vec<String> {
        let mut out = Vec::new();
        self.drop_trigger_if_rolling(&mut out);
        ui.add_space(4.0);
        ui.add_enabled_ui(connected, |ui| {
            ui.horizontal(|ui| {
                let run_click = run_button(ui, self.running, connected).clicked();
                if run_click && self.pulse_mode {
                    if self.running {
                        self.running = false; // stop waiting, the last result stays
                        out.push("scope stop".into());
                    } else {
                        // like after SINGLE: run goes back to the running scope (AUTO)
                        let (_, t) = self.pulse_prev.take().unwrap_or((0, false));
                        self.pulse_mode = false;
                        self.mode = 0;
                        self.trig_on = t;
                        self.running = true;
                        self.apply_force(&mut out);
                    }
                } else if run_click {
                    self.running = !self.running;
                    if self.running && self.mode == 2 {
                        self.mode = 0;
                        self.apply(&mut out);
                    } else {
                        out.push(if self.running { "scope run".into() } else { "scope stop".into() });
                    }
                }
                // trigger mode: the current one is lit
                for (m, txt, hint) in [
                    (0u8, "AUTO", "Refreshes even without a trigger"),
                    (1u8, "NORMAL", "Refreshes only on a trigger"),
                    (2u8, "SINGLE", "Captures one trigger, then stops"),
                ] {
                    // NORMAL and SINGLE wait for a trigger: with the trigger off only AUTO (free running) is there
                    let usable = m == 0 || self.trig_on || self.pulse_mode;
                    let on = if self.pulse_mode { m == 2 } else { self.mode == m };
                    let b = egui::Button::new(RichText::new(txt).strong().color(if on { Color32::WHITE } else { Color32::from_gray(200) }))
                        .fill(if on { TRIG } else { Color32::from_gray(55) })
                        .min_size(egui::vec2(64.0, 30.0));
                    let r = ui.add_enabled(usable, b).on_hover_text(hint).on_disabled_hover_text(format!("{hint}\n(turn the trigger on first)"));
                    if r.clicked() {
                        self.pulse_mode = false;
                        self.mode = m;
                        self.running = true;
                        if m == 2 { self.apply_force(&mut out) } else { self.apply(&mut out) }
                    }
                }
            });
        });
        ui.separator();


        // ---- horizontal
        ui.label(RichText::new("HORIZONTAL").strong());
        let before = (self.tdiv_v, self.delay, self.dec_manual);
        let mut commit = false;
        let mut knob_active = false;
        let roll = self.is_roll();
        ui.horizontal(|ui| {
            let k = knob(ui, "tdiv", "TIME/DIV", self.tdiv_v, "s/div", SI_TIME, TRACE, true,
                "Turn: any time base (clockwise = faster)\nWheel: 1-2-5 steps\nClick: 10 ms/div");
            if k.turn != 0.0 {
                self.tdiv_v *= 2f64.powf(-k.turn / 16.0);
            }
            if k.wheel != 0 {
                self.tdiv_v = step125(&TDIV, self.tdiv_v, -k.wheel);
            }
            if k.clicked {
                self.tdiv_v = 10e-3;
            }
            if let Some(v) = k.typed.filter(|v| *v > 0.0) {
                self.tdiv_v = v;
            }
            self.tdiv_v = self.tdiv_v.clamp(TDIV[0], TDIV[TDIV.len() - 1]);
            knob_active |= k.active;
            commit |= k.released;

            let k = knob_small(ui, "hpos", "POSITION", self.delay, "s", SI_TIME, TRACE, !roll, "Turn / wheel: move the trace left / right\nClick: trigger to the centre");
            let t = k.turn + 2.0 * k.wheel as f64;
            if t != 0.0 {
                self.delay -= t * self.tdiv_v * 0.1;
                self.clamp_delay();
            }
            if k.clicked {
                self.delay = 0.0;
            }
            if let Some(v) = k.typed {
                self.delay = v;
                self.clamp_delay();
            }
            knob_active |= k.active;
            commit |= k.released || k.wheel != 0;

            // sample rate = 400 kS/s / (scans per sample), any integer 1 .. 4096
            let (dec, _, _) = self.geom();
            let rate = 1.0 / (SCAN_S * dec as f64);
            let hint = "Turn: sample rate (400 kS/s .. 98 S/s, 400 kS/s divided by an integer)\n\
                        the time base follows, so the record (samples / rate) fills the screen\nWheel: 1-2-5 steps\nClick: 10 ms/div".to_string();
            let k = knob_small(ui, "srate", "RATE", rate, "S/s", SI_RATE, TRACE, true, &hint);
            let mut r = rate;
            if k.turn != 0.0 {
                r *= 2f64.powf(k.turn / 16.0);
            }
            if k.wheel != 0 {
                r = step125(&RATE_STEPS, r, k.wheel);
            }
            if let Some(v) = k.typed.filter(|v| *v > 0.0) {
                r = v;
            }
            // rate and time base are one setting (record = samples / rate fills the 10 divisions)
            if r != rate {
                let dec = ((1.0 / SCAN_S / r).round() as u32).clamp(1, DEC_MAX);
                let span = if self.is_roll() { 1000.0 } else { self.n };
                self.tdiv_v = (span * dec as f64 * SCAN_S / 10.0).clamp(TDIV[0], TDIV[TDIV.len() - 1]);
                self.dec_manual = None;
            }
            if k.clicked {
                self.dec_manual = None;
                self.tdiv_v = 10e-3;
            }
            knob_active |= k.active;
            commit |= k.released || k.wheel != 0 || k.clicked || k.typed.is_some();
        });
        let moved = (self.tdiv_v != before.0 || self.delay != before.1) && !knob_active && !self.pos_dragging;
        if moved || self.dec_manual != before.2 && !knob_active || commit || self.pos_commit {
            self.pos_commit = false;
            self.clamp_delay();
            self.apply(&mut out);
        }
        ui.separator();

        // ---- vertical | trigger, side by side; the columns share the panel width
        let avail = ui.available_width();
        let vw = ((avail - 30.0) * 0.54).max(120.0);
        let tw = (avail - 30.0 - vw).max(100.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(vw); // the AUTO button sits at the column's right edge
                ui.horizontal(|ui| {
                    ui.label(RichText::new("VERTICAL").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let auto = self.auto_y && self.auto_ypos;
                    let (txt, fill) = if auto { ("AUTO", ORANGE) } else { ("MANUAL", Color32::from_gray(70)) };
                    if ui
                        .add(egui::Button::new(RichText::new(txt).strong().color(Color32::WHITE)).fill(fill).min_size(egui::vec2(60.0, 20.0)))
                        .on_hover_text("AUTO: scale and position follow the signal (smoothly)\nMANUAL: scale and position stay where they are set\n(turning a knob or dragging the trace: MANUAL, clicking a knob: AUTO)")
                        .clicked()
                    {
                        self.auto_y = !auto;
                        self.y_fast_until = Some(Instant::now() + Duration::from_millis(600));
                        self.auto_ypos = !auto;
                    }
                    });
                });
                ui.horizontal(|ui| {
                    let k = knob(ui, "ydiv", "LX/DIV", self.ydiv_v, "lx/div", SI_LUX, TRACE, true,
                        "Turn: any scale (clockwise = zoom in)\nWheel: 1-2-5 steps\nClick: AUTO");
                    if k.turn != 0.0 {
                        self.auto_y = false;
                        self.auto_ypos = false;
                        self.ydiv_v *= 2f64.powf(-k.turn / 16.0);
                    }
                    if k.wheel != 0 {
                        self.auto_y = false;
                        self.auto_ypos = false;
                        self.ydiv_v = step125(&YDIV, self.ydiv_v, -k.wheel);
                    }
                    if k.clicked {
                        self.auto_y = true;
                        self.auto_ypos = true;
                        self.y_fast_until = Some(Instant::now() + Duration::from_millis(600));
                    }
                    if let Some(v) = k.typed.filter(|v| *v > 0.0) {
                        self.auto_y = false;
                        self.auto_ypos = false;
                        self.ydiv_v = v;
                    }
                    self.ydiv_v = self.ydiv_v.clamp(0.01, 1e6);

                    let k = knob_small(ui, "ypos", "POSITION", self.yoff, "lx", SI_LUX, TRACE, true, "Turn / wheel: move the trace up / down\nClick: AUTO");
                    let t = k.turn + 2.0 * k.wheel as f64;
                    if t != 0.0 {
                        self.auto_ypos = false;
                        self.auto_y = false;
                        self.yoff -= t * self.ydiv_v * 0.1;
                    }
                    if k.clicked {
                        self.auto_ypos = true;
                        self.auto_y = true;
                        self.y_fast_until = Some(Instant::now() + Duration::from_millis(600));
                    }
                    if let Some(v) = k.typed {
                        self.auto_ypos = false;
                        self.auto_y = false;
                        self.yoff = v;
                    }
                });
            });
            // short divider: a separator here would stretch to the bottom of the panel
            let (r, _) = ui.allocate_exact_size(egui::vec2(9.0, 110.0), egui::Sense::hover());
            ui.painter().vline(r.center().x, r.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
            ui.vertical(|ui| {
                ui.set_width(tw); // the ON / OFF button sits at the column's right edge
                let before_on = self.trig_on;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("TRIGGER").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (txt, fill) = if self.trig_on { ("ON", TRIG) } else { ("OFF", Color32::from_gray(70)) };
                    let b = egui::Button::new(RichText::new(txt).strong().color(Color32::WHITE)).fill(fill).min_size(egui::vec2(44.0, 20.0));
                    if ui
                        .add_enabled(self.trig_possible() || self.pulse_mode, b)
                        .on_hover_text("ON: the trace is held on the trigger\nOFF: free running")
                        .on_disabled_hover_text("No trigger at 20 ms/div and slower: the trace rolls live\n(10 ms/div or faster for the trigger)")
                        .clicked()
                    {
                        if self.pulse_mode {
                            // OFF ends the shutter measurement (it cannot run without its trigger)
                            self.pulse_prev = None;
                            self.pulse_mode = false;
                            self.trig_on = false;
                            self.running = true;
                        } else {
                            self.trig_on = !self.trig_on;
                        }
                        if !self.trig_on {
                            self.mode = 0; // without a trigger only AUTO (free running) makes sense
                            self.running = true;
                        }
                    }
                    });
                });
                if self.trig_on != before_on {
                    self.clamp_delay();
                    self.apply(&mut out);
                }
                let before = (self.level_lx, self.falling, self.mode);
                let mut pulse_arm = false;
                let mut lvl_commit = false;
                let mut lvl_active = false;
                ui.horizontal(|ui| {
                    let k = knob_small(ui, "lvl", "LEVEL", self.level_lx, "lx", SI_LUX, if self.trig_on { TRACE } else { Color32::GRAY }, true, "Turn / wheel: trigger level\nClick: 50 % of the signal");
                    let t = k.turn + 2.0 * k.wheel as f64;
                    if t != 0.0 {
                        self.level_lx += t * self.ydiv_v * 0.05;
                        self.level_lx = (self.level_lx * 1000.0).round() / 1000.0;
                    }
                    if k.clicked {
                        if let Some(m) = self.measured() {
                            self.level_lx = ((m.max + m.min) / 2.0 * 100.0).round() / 100.0;
                        }
                    }
                    if let Some(v) = k.typed {
                        self.level_lx = v;
                    }
                    lvl_active = k.active;
                    lvl_commit = k.released || k.clicked || k.typed.is_some() || k.wheel != 0;
                    ui.vertical(|ui| {
                        let acc = if self.trig_on { TRIG } else { Color32::from_gray(110) };
                        // in the shutter measurement neither edge is selected; picking one goes back to the scope
                        let pm = self.pulse_mode;
                        for (rising, sel) in [(true, !self.falling && !pm), (false, self.falling && !pm)] {
                            if edge_button(ui, rising, sel, acc).clicked() {
                                self.falling = !rising;
                                if pm {
                                    let (m, t) = self.pulse_prev.take().unwrap_or((0, true));
                                    self.pulse_mode = false;
                                    self.mode = m;
                                    self.trig_on = t;
                                    self.running = true;
                                    pulse_arm = true; // re-send the configuration
                                }
                            }
                        }
                        // shutter measurement: one pulse, single shot
                        if pulse_button(ui, self.pulse_mode, TRIG).clicked() {
                            if !self.pulse_mode {
                                // keep the scope screen standing (dim) until the pulse comes, like SINGLE
                                let (t, v, raw) = self.visible3();
                                let w = self.window();
                                let (x0, x1) = if self.is_roll() { (-w, 0.0) } else { (self.delay - w / 2.0, self.delay + w / 2.0) };
                                self.frozen = Some(Frozen { t, v, raw, x0, x1 });
                                self.pulse_prev = Some((self.mode, self.trig_on));
                                self.pulse_mode = true;
                                self.trig_on = true;
                                self.mode = 2;
                            }
                            self.running = true;
                            self.pulse.stale = true; // the result shown is from before: dim until the next one
                            pulse_arm = true;
                        }
                    });
                });
                if pulse_arm {
                    self.apply_force(&mut out);
                }
                if !self.trig_on && (self.level_lx != before.0 || self.falling != before.1 || lvl_active) {
                    self.lvl_show_until = Some(Instant::now() + Duration::from_millis(2000));
                }
                let changed = (self.level_lx, self.falling, self.mode) != before && !lvl_active && !self.level_dragging;
                if changed || lvl_commit || self.level_commit {
                    self.level_commit = false;
                    if self.mode == 2 && before.2 != 2 {
                        self.running = true;
                    }
                    self.apply(&mut out);
                }
            });
        });
        ui.separator();

        // ---- filter
        let dt = self.sample_dt();
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
            ui.label(RichText::new("FILTER").strong());
            for (f, txt, hint) in [
                (0u8, "OFF", "No filter"),
                (1u8, "AVG", "Moving average over N points (less noise)"),
                (2u8, "LPF", "Low-pass: damps changes faster than the cutoff (no delay)"),
            ] {
                let on = self.filt == f;
                let b = egui::Button::new(RichText::new(txt).strong().color(if on { Color32::WHITE } else { Color32::from_gray(200) }))
                    .fill(if on { ORANGE } else { Color32::from_gray(55) })
                    .min_size(egui::vec2(48.0, 20.0));
                if ui.add(b).on_hover_text(hint).clicked() {
                    self.filt = f;
                }
            }
            });
            ui.add_space(8.0);
            ui.vertical(|ui| {
            match self.filt {
                1 => {
                    let k = knob_small(ui, "ma", "POINTS", self.ma_n.round(), "pts", &[("", 1.0)], TRACE, true,
                        "Moving average points (2 .. 5000)\nTurn / wheel: adjust, click: 9, click the number: type it");
                    if k.turn != 0.0 {
                        self.ma_n *= 2f64.powf(k.turn / 16.0);
                    }
                    if k.wheel != 0 {
                        self.ma_n = step125(&MA_STEPS, self.ma_n, k.wheel);
                    }
                    if k.clicked {
                        self.ma_n = 9.0;
                    }
                    if let Some(v) = k.typed {
                        self.ma_n = v;
                    }
                    self.ma_n = self.ma_n.clamp(2.0, 5000.0);
                    ui.label(RichText::new(format!("= {} wide", fmt_time(self.ma_n.round() * dt))).weak());
                }
                2 => {
                    let k = knob_small(ui, "lp", "CUTOFF", self.lp_hz, "Hz", &[("k", 1e3), ("", 1.0)], TRACE, true,
                        "Low-pass cutoff\nTurn / wheel: adjust, click: 100 Hz, click the number: type it (e.g. 2.5k)");
                    if k.turn != 0.0 {
                        self.lp_hz *= 2f64.powf(k.turn / 16.0);
                    }
                    if k.wheel != 0 {
                        self.lp_hz = step125(&LP_STEPS, self.lp_hz, k.wheel);
                    }
                    if k.clicked {
                        self.lp_hz = 100.0;
                    }
                    if let Some(v) = k.typed.filter(|v| *v > 0.0) {
                        self.lp_hz = v;
                    }
                    self.lp_hz = self.lp_hz.clamp(0.1, 200e3);
                    if self.lp_hz > 0.5 / dt {
                        ui.label(RichText::new(format!("above the Nyquist limit ({}): no effect", fmt_freq(0.5 / dt))).small().color(ORANGE));
                    }
                }
                _ => {}
            }
            if self.filt != 0 {
                ui.checkbox(&mut self.show_raw, "Show raw (dim)");
            }
            });
            // trigger minimum width: light that goes back across the level sooner is passed over
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                let k = knob_small_zero(ui, "minw", "MIN WIDTH", self.min_w, "s", SI_TIME, TRIG, true,
                    "Trigger only on light that stays past the level at least this long:\nshorter flashes and flicker are passed over\nTurn / wheel: adjust, click: off (HOLDOFF), click the value: type it",
                    "HOLDOFF");
                if k.turn != 0.0 {
                    self.min_w = if self.min_w == 0.0 { if k.turn > 0.0 { 10e-6 } else { 0.0 } } else { self.min_w * 2f64.powf(k.turn / 16.0) };
                    if self.min_w < 2.5e-6 {
                        self.min_w = 0.0;
                    }
                }
                if k.wheel != 0 {
                    let i = MINW_STEPS.iter().position(|&s| s >= self.min_w * 0.999).unwrap_or(MINW_STEPS.len() - 1) as i32;
                    self.min_w = MINW_STEPS[(i + k.wheel).clamp(0, MINW_STEPS.len() as i32 - 1) as usize];
                }
                if k.clicked {
                    self.min_w = 0.0;
                }
                if let Some(v) = k.typed.filter(|v| *v >= 0.0) {
                    self.min_w = v;
                }
                self.min_w = self.min_w.clamp(0.0, MINW_MAX);
            });
        });
        // filter (trigger filter) and minimum width go to the board when they change
        self.apply(&mut out);
        ui.separator();

        out
    }

    /// sample-count bar for the top bar: drag left / right (or type); applied when let go
    pub fn sample_count_ui(&mut self, ui: &mut egui::Ui) -> Vec<String> {
        let mut out = Vec::new();
        let r = ui
            .add(egui::DragValue::new(&mut self.n).range(16.0..=N_MAX).speed(8.0).max_decimals(0).suffix(" pts"))
            .on_hover_text("Samples per frame (16 .. 4000)\nDrag left / right or click to type\nFewer samples: a shorter record and less USB traffic");
        self.n = self.n.round().clamp(16.0, N_MAX);
        if r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged()) {
            self.clamp_delay();
            self.apply(&mut out);
        }
        // the screen filter also sets the board's trigger smoothing: re-send when that changes
        if self.trig_on {
            self.apply(&mut out);
        }
        out
    }

    /// lower part of the panel (scrolls when the window is small): measurements, cursors
    pub fn controls_rest(&mut self, ui: &mut egui::Ui) -> Vec<String> {
        let out = Vec::new();
        if self.pulse_mode {
            self.pulse.panel(ui); // shutter result and statistics instead of the scope measurements
            return out;
        }
        // ---- measurements
        ui.label(RichText::new("MEASUREMENTS").strong());
        let (t, v) = self.visible();
        egui::Grid::new("meas").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
            match self.measured().as_deref() {
                Some(m) => {
                    let row = |ui: &mut egui::Ui, k: &str, val: String| {
                        ui.label(k);
                        ui.label(RichText::new(val).monospace());
                        ui.end_row();
                    };
                    row(ui, "Max", fmt_lux(m.max));
                    row(ui, "Min", fmt_lux(m.min));
                    row(ui, "Mean", fmt_lux(m.mean));
                    row(ui, "P-P", fmt_lux(m.max - m.min));
                    let fl = if m.max + m.min > 0.0 { 100.0 * (m.max - m.min) / (m.max + m.min) } else { 0.0 };
                    row(ui, "Flicker", format!("{fl:.1} %"));
                    row(ui, "Freq", m.period.map(|p| fmt_freq(1.0 / p)).unwrap_or("-".into()));
                    row(ui, "Period", m.period.map(fmt_time).unwrap_or("-".into()));
                    row(ui, "+Width", m.width.map(fmt_time).unwrap_or("-".into()));
                    row(ui, "Duty", match (m.width, m.period) { (Some(w), Some(p)) => format!("{:.1} %", 100.0 * w / p), _ => "-".into() });
                    row(ui, "Rise 10-90", m.rise.map(fmt_time).unwrap_or("-".into()));
                }
                None => {
                    ui.label(RichText::new("no data").weak());
                    ui.end_row();
                }
            }
        });
        ui.separator();

        // ---- cursors: A and B each switched on (placed on the screen) and off (removed)
        ui.label(RichText::new("CURSORS").strong());
        ui.label(RichText::new("drag the ▼▲ arrows · click = move A, right click = move B").small().weak());
        let (sx0, sx1) = if self.is_roll() { (-self.window(), 0.0) } else { (self.delay - self.window() / 2.0, self.delay + self.window() / 2.0) };
        let val_at = |x: f64| -> Option<f64> {
            let i = t.iter().position(|&ti| ti >= x)?;
            if i == 0 { return v.first().copied(); }
            let f = (x - t[i - 1]) / (t[i] - t[i - 1]);
            Some(v[i - 1] + f * (v[i] - v[i - 1]))
        };
        egui::Grid::new("cur").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
            for (name, frac) in [("A", 1.0 / 3.0), ("B", 2.0 / 3.0)] {
                let cur = if name == "A" { &mut self.cur_a } else { &mut self.cur_b };
                let on = cur.is_some();
                let (txt, fill) = if on { (format!("{name} ON"), CUR) } else { (format!("{name} OFF"), Color32::from_gray(70)) };
                if ui
                    .add(egui::Button::new(RichText::new(txt).strong().color(Color32::WHITE)).fill(fill).min_size(egui::vec2(58.0, 20.0)))
                    .on_hover_text(format!("ON: cursor {name} appears on the screen\nOFF: cursor {name} is removed"))
                    .clicked()
                {
                    *cur = if on { None } else { Some(sx0 + frac * (sx1 - sx0)) };
                }
                let c = *cur;
                ui.label(RichText::new(match c {
                    Some(x) => format!("{}  {}", fmt_time(x), val_at(x).map(fmt_lux).unwrap_or_default()),
                    None => "-".into(),
                }).monospace());
                ui.end_row();
            }
            if let (Some(a), Some(b)) = (self.cur_a, self.cur_b) {
                let d = (b - a).abs();
                ui.label("ΔT");
                ui.label(RichText::new(format!("{}  ({})", fmt_time(d), if d > 0.0 { fmt_freq(1.0 / d) } else { "-".into() })).monospace());
                ui.end_row();
                if let (Some(va), Some(vb)) = (val_at(a), val_at(b)) {
                    ui.label("ΔV");
                    ui.label(RichText::new(fmt_lux(vb - va)).monospace());
                    ui.end_row();
                }
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.add_enabled(!v.is_empty(), egui::Button::new("Save CSV")).on_hover_text("Save the trace on screen").clicked() {
                self.export(&t, &v);
            }
        });
        if !self.note.is_empty() {
            ui.label(RichText::new(&self.note).small().weak());
        }
        out
    }

    fn export(&mut self, t: &[f64], v: &[f64]) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default();
        let path = dir.join(format!("sutterspeed_scope_{secs}.csv"));
        let mut s = String::from("time_s,lux\n");
        for (a, b) in t.iter().zip(v) {
            s += &format!("{a:.9},{b:.3}\n");
        }
        self.note = match std::fs::write(&path, s) {
            Ok(_) => format!("Saved: {}", path.display()),
            Err(e) => format!("Save failed: {e}"),
        };
    }

    // ---------------------------------------------------------------- screen

    pub fn plot(&mut self, ui: &mut egui::Ui) -> Vec<String> {
        let mut out = Vec::new();
        self.drop_trigger_if_rolling(&mut out);
        if !self.pulse_mode || !self.pulse.stale {
            self.frozen = None;
        }
        if self.pulse_mode && self.frozen.is_none() {
            self.pulse.set_filter((self.filt, self.ma_n, self.lp_hz));
            // same vertical scale as the scope view (no rescaling)
            let (y0, y1) = (self.yoff - 4.0 * self.ydiv_v, self.yoff + 4.0 * self.ydiv_v);
            let gdiv = match (self.auto_y, self.y_target) {
                (true, Some((td, _))) => td,
                _ => self.ydiv_v,
            };
            self.pulse.plot(ui, self.running, self.level_lx, self.filt != 0, (y0, y1, gdiv));
            return out;
        }
        let w = self.window();
        let tdiv = self.tdiv();
        let (unit, uname) = if tdiv < 1e-3 { (1e6, "us") } else if tdiv < 1.0 { (1e3, "ms") } else { (1.0, "s") };
        let frozen = self.frozen.clone();
        let (x0, x1) = match &frozen {
            Some(f) => (f.x0, f.x1),
            None if self.is_roll() => (-w, 0.0),
            None => (self.delay - w / 2.0, self.delay + w / 2.0),
        };
        let (t, v, raw) = match frozen {
            Some(f) => (f.t, f.v, f.raw),
            None => self.visible3(),
        };
        // waiting for a trigger (SINGLE armed, or the shutter measurement): the trace on screen is the old one
        let dim = self.running && (self.pulse_mode || (self.mode == 2 && self.trig_on && !self.is_roll()));
        let trace = if dim { TRACE.gamma_multiply(0.35) } else { TRACE };

        // vertical scale
        let (ydiv, yc) = if self.auto_y || self.auto_ypos {
            let (lo, hi) = if v.is_empty() {
                (0.0, 10.0)
            } else {
                let lo = v.iter().cloned().fold(f64::MAX, f64::min);
                let hi = v.iter().cloned().fold(f64::MIN, f64::max);
                if self.trig_on { (lo.min(self.level_lx), hi.max(self.level_lx)) } else { (lo, hi) }
            };
            // new target only when the signal leaves the view or shrinks to under a third of it (hysteresis)
            let (td, tc) = self.y_target.unwrap_or((self.ydiv_v, self.yoff));
            let (vlo, vhi) = (tc - 4.0 * td, tc + 4.0 * td);
            let need = self.y_target.is_none() || lo < vlo || hi > vhi || (hi - lo) < (vhi - vlo) / 3.0;
            if need {
                let d = if self.auto_y {
                    let half = if self.auto_ypos { (hi - lo) / 2.0 } else { (hi - self.yoff).abs().max((self.yoff - lo).abs()) };
                    nice_div((half * 2.0 * 1.3 / 8.0).max(0.05))
                } else {
                    self.ydiv_v
                };
                let c = if self.auto_ypos { ((hi + lo) / 2.0 / (d / 2.0)).round() * (d / 2.0) } else { self.yoff };
                self.y_target = Some((d, c));
            }
            let (td, tc) = self.y_target.unwrap();
            // glide: about 90 % of the way in 0.5 s
            let dt = ui.input(|i| i.stable_dt).clamp(0.0, 0.1) as f64;
            // following the signal: slow glide; just switched back to auto (knob click / AUTO): 5x faster
            let fast = self.y_fast_until.is_some_and(|t| Instant::now() < t);
            let kd = 1.0 - (-dt / if fast { 0.044 } else { 0.66 }).exp(); // scale: slow
            let kc = 1.0 - (-dt / if fast { 0.044 } else { 0.22 }).exp(); // position
            let d = if self.auto_y { self.ydiv_v + (td - self.ydiv_v) * kd } else { self.ydiv_v };
            let c = if self.auto_ypos { self.yoff + (tc - self.yoff) * kc } else { self.yoff };
            if (d - td).abs() > td * 1e-3 || (c - tc).abs() > td * 1e-3 {
                ui.ctx().request_repaint();
            }
            (d, c)
        } else {
            self.y_target = None;
            (self.ydiv_v, self.yoff)
        };
        if self.auto_y {
            self.ydiv_v = ydiv;
        }
        if self.auto_ypos {
            self.yoff = yc;
        }
        let (y0, y1) = (yc - 4.0 * ydiv, yc + 4.0 * ydiv);
        // grid and labels follow the round target division (anchored at 0) while the view glides to it
        let gdiv = match (self.auto_y, self.y_target) {
            (true, Some((td, _))) => td,
            _ => ydiv,
        };

        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}/div", fmt_div(tdiv))).strong().color(TRACE));
            ui.label(RichText::new(format!("{}/div", fmt_lux(gdiv))).strong().color(TRACE));
            if self.pulse_mode {
                ui.label(RichText::new(format!("WAITING FOR A PULSE  (light above {})", fmt_lux(self.level_lx))).color(TRIG).strong());
            } else if self.trig_on {
                ui.label(RichText::new(format!("Trigger {} {}", if self.falling { "↘" } else { "↗" }, fmt_lux(self.level_lx))).color(TRIG));
            }
            ui.label(RichText::new("Wheel: time base · Shift+wheel: scale").small().weak());
        });

        let cols = (ui.available_width() as usize).max(100);
        let pts: Vec<[f64; 2]> = minmax_columns(&t, &v, unit, x0, x1, cols);
        let raw_pts: Option<Vec<[f64; 2]>> = (self.filt != 0 && self.show_raw).then(|| minmax_columns(&t, &raw, unit, x0, x1, cols));
        let level = self.level_lx;
        let roll = self.is_roll() && self.frozen.is_none();
        let trig = (self.trig_on && !roll) || self.pulse_mode;
        // trigger off and the level just changed: show it faintly, fading out
        let lvl_fade = match self.lvl_show_until {
            Some(t) if !self.trig_on => {
                let left = t.saturating_duration_since(Instant::now()).as_secs_f32();
                if left > 0.0 {
                    ui.ctx().request_repaint();
                    Some((left / 0.6).min(1.0))
                } else {
                    self.lvl_show_until = None;
                    None
                }
            }
            _ => None,
        };
        let (ca, cb) = (self.cur_a, self.cur_b);
        let xdiv = tdiv * unit;
        let resp = Plot::new("scope")
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .show_x(true)
            .show_y(true)
            .x_grid_spacer(move |g| div_marks(g, xdiv, 0.0))
            .y_grid_spacer(move |g| div_marks(g, gdiv, 0.0))
            .x_axis_formatter(move |m, _| format!("{} {uname}", fmt_num(m.value)))
            .y_axis_formatter(|m, _| format!("{} lx", fmt_num(m.value)))
            .show(ui, |p| {
                p.set_plot_bounds(PlotBounds::from_min_max([x0 * unit, y0], [x1 * unit, y1]));
                if let Some(rp) = raw_pts {
                    p.line(Line::new("raw", PlotPoints::from(rp)).color(trace.gamma_multiply(0.25)).width(1.0));
                }
                if pts.len() <= 300 {
                    // few samples on screen: show where they really are
                    p.points(Points::new("samples", PlotPoints::from(pts.clone())).color(trace).radius(2.5));
                }
                p.line(Line::new("light", PlotPoints::from(pts)).color(trace).width(1.5));
                if trig {
                    p.hline(HLine::new("trigger", level).color(TRIG).style(LineStyle::dashed_dense()).width(1.5));
                    // level marker at the right edge, like a bench scope
                    let ly = level.clamp(y0 + 0.05 * ydiv, y1 - 0.05 * ydiv);
                    p.text(Text::new("tlvl", PlotPoint::new(x1 * unit, ly), RichText::new("◀").color(TRIG).size(20.0).strong()).anchor(egui::Align2::RIGHT_CENTER));
                    if 0.0 < x0 {
                        p.text(Text::new("tl", PlotPoint::new(x0 * unit, y1 - 0.3 * ydiv), RichText::new("◀ T").color(TRIG).strong()).anchor(egui::Align2::LEFT_CENTER));
                    } else if 0.0 > x1 {
                        p.text(Text::new("tr", PlotPoint::new(x1 * unit, y1 - 0.3 * ydiv), RichText::new("T ▶").color(TRIG).strong()).anchor(egui::Align2::RIGHT_CENTER));
                    } else {
                        p.vline(VLine::new("t0", 0.0).color(TRIG.gamma_multiply(0.5)).width(1.0));
                    }
                }
                if let Some(a) = lvl_fade {
                    let c = TRIG.gamma_multiply(0.75 * a);
                    p.hline(HLine::new("trigger", level).color(c).style(LineStyle::dashed_dense()).width(1.5));
                    let ly = level.clamp(y0 + 0.05 * ydiv, y1 - 0.05 * ydiv);
                    p.text(Text::new("tlvl", PlotPoint::new(x1 * unit, ly), RichText::new("◀").color(c).size(20.0).strong()).anchor(egui::Align2::RIGHT_CENTER));
                }
                for (c, n) in [(ca, "A"), (cb, "B")] {
                    if let Some(x) = c {
                        p.vline(VLine::new(n, x * unit).color(CUR).style(LineStyle::dashed_loose()).width(1.2));
                        // arrow centred on the line, the name beside it on the side facing the middle of the screen
                        let gap = 8.0 * p.transform().dvalue_dpos()[0].abs();
                        let left_half = x < (x0 + x1) / 2.0;
                        let lx = x * unit + if left_half { gap } else { -gap };
                        for (y, arrow, tag, centre, side) in [
                            (y1, "▼", "t", egui::Align2::CENTER_TOP, if left_half { egui::Align2::LEFT_TOP } else { egui::Align2::RIGHT_TOP }),
                            (y0, "▲", "b", egui::Align2::CENTER_BOTTOM, if left_half { egui::Align2::LEFT_BOTTOM } else { egui::Align2::RIGHT_BOTTOM }),
                        ] {
                            p.text(Text::new(format!("{n}{tag}a"), PlotPoint::new(x * unit, y), RichText::new(arrow).color(CUR).size(15.0).strong()).anchor(centre));
                            p.text(Text::new(format!("{n}{tag}n"), PlotPoint::new(lx, y), RichText::new(n).color(CUR).size(15.0).strong()).anchor(side));
                        }
                    }
                }
                p.pointer_coordinate()
            });
        // the trigger arrow at the right edge: grab it and drag up / down to move the level
        let mut arrow_drag = false;
        if trig {
            let ly = level.clamp(y0 + 0.05 * ydiv, y1 - 0.05 * ydiv);
            let pos = resp.transform.position_from_point(&PlotPoint::new(x1 * unit, ly));
            let hit = egui::Rect::from_min_max(egui::pos2(pos.x - 26.0, pos.y - 13.0), egui::pos2(pos.x + 2.0, pos.y + 13.0));
            let a = ui.interact(hit, ui.id().with("trig_arrow"), egui::Sense::click_and_drag());
            let a = a.on_hover_text("Drag to move the trigger level").on_hover_cursor(egui::CursorIcon::ResizeVertical);
            if a.dragged() {
                if let Some(p) = a.interact_pointer_pos() {
                    let v = resp.transform.value_from_position(p).y;
                    self.level_lx = (v * 100.0).round() / 100.0;
                    self.level_dragging = true;
                }
                arrow_drag = true;
            }
            if a.drag_stopped() {
                self.level_dragging = false;
                self.apply(&mut out);
            }
        }
        // cursor arrows (top and bottom of each cursor line): drag left / right to move the cursor
        for (which, c) in [(0, ca), (1, cb)] {
            let Some(x) = c else { continue };
            for (edge, yy, dy) in [(0, y1, 10.0f32), (1, y0, -10.0f32)] {
                let pos = resp.transform.position_from_point(&PlotPoint::new(x * unit, yy));
                let hit = egui::Rect::from_center_size(egui::pos2(pos.x, pos.y + dy), egui::vec2(28.0, 22.0));
                let a = ui.interact(hit, ui.id().with(("cursor_arrow", which, edge)), egui::Sense::click_and_drag());
                let a = a.on_hover_text("Drag to move the cursor").on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                if a.dragged() {
                    if let Some(p) = a.interact_pointer_pos() {
                        let nx = (resp.transform.value_from_position(p).x / unit).clamp(x0, x1);
                        if which == 0 { self.cur_a = Some(nx) } else { self.cur_b = Some(nx) }
                    }
                    arrow_drag = true;
                }
            }
        }
        let r = &resp.response;
        // drag = horizontal and vertical position (like the scope's position knobs); the trace follows the pointer
        if !arrow_drag && r.dragged_by(egui::PointerButton::Primary) {
            let d = r.drag_delta();
            if d != egui::Vec2::ZERO && (self.auto_y || self.auto_ypos) {
                // the trace is placed by hand now: the vertical scale and position stop following the signal
                self.auto_y = false;
                self.auto_ypos = false;
                self.y_target = None;
            }
            if !roll && d.x != 0.0 {
                self.delay -= d.x as f64 * resp.transform.dvalue_dpos()[0] / unit;
                self.clamp_delay();
                self.pos_dragging = true;
            }
            if d.y != 0.0 {
                self.yoff -= d.y as f64 * resp.transform.dvalue_dpos()[1];
            }
        }
        if !roll && r.drag_stopped() {
            self.pos_dragging = false;
            self.pos_commit = true;
        }
        if let Some(pc) = resp.inner {
            let ctrl = ui.input(|i| i.modifiers.ctrl);
            if r.clicked() && ctrl {
                self.level_lx = (pc.y * 100.0).round() / 100.0;
                self.apply(&mut out);
            } else if r.clicked() && self.cur_a.is_some() {
                self.cur_a = Some(pc.x / unit); // a cursor that is on moves where the plot is clicked
            } else if r.secondary_clicked() && self.cur_b.is_some() {
                self.cur_b = Some(pc.x / unit);
            }
        }
        if r.hovered() {
            let (dy, shift) = ui.input(|i| {
                let d: f32 = i.raw.events.iter().map(|e| match e {
                    egui::Event::MouseWheel { delta, .. } => delta.y + delta.x,
                    _ => 0.0,
                }).sum();
                (d, i.modifiers.shift)
            });
            if dy != 0.0 {
                let dir = if dy > 0.0 { -1 } else { 1 }; // wheel up = zoom in
                if shift {
                    self.auto_y = false;
                    self.auto_ypos = false;
                    self.ydiv_v = step125(&YDIV, self.ydiv_v, dir);
                } else {
                    let old = self.tdiv_v;
                    self.tdiv_v = step125(&TDIV, self.tdiv_v, dir);
                    if old != self.tdiv_v {
                        self.clamp_delay();
                        self.apply(&mut out);
                    }
                }
            }
        }
        out
    }
}

/// Two "scope set" commands differing only in trigger level / edge (the board updates those in place,
/// outside the shutter measurement).
fn level_only(a: &str, b: &str) -> bool {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.split(' ').collect(), b.split(' ').collect());
    a != b
        && a.len() == b.len()
        && a.len() > 9
        && a[9] == "0"
        && a.iter().zip(&b).enumerate().all(|(i, (x, y))| i == 5 || i == 6 || x == y)
}
