//! Sutterspeed light oscilloscope (Rust / egui).
//!
//! Pick the port (or leave it on automatic) and the board's light level is shown like on an oscilloscope:
//! time/div, trigger level / edge / mode, run/stop/single, cursors and automatic measurements.
//!
//!     sutterspeed-viewer            automatic port (USB 1209:0001)
//!     sutterspeed-viewer COM7       start on this port
//!     sutterspeed-viewer --sim      no hardware: show a synthetic flickering light
#![windows_subsystem = "windows"]

mod dfu;
mod icons;
mod knob;
mod pulse;
mod scope;

use eframe::egui;
use icons::{icon_button, Icon};
use egui::{Color32, RichText};
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

const VID: u16 = 0x1209;
const PID: u16 = 0x0001;
const GREEN: Color32 = Color32::from_rgb(70, 180, 100);
const ORANGE: Color32 = Color32::from_rgb(230, 140, 40);
const FRAME_MIN: Duration = Duration::from_micros(16_000); // frame cap ~60 fps

// ------------------------------------------------------------------ serial link (background thread)

enum Ctl {
    Connect(Option<String>), // None = automatic
    Disconnect,
    Send(String),
}

enum Ev {
    Connected(String),
    Status(String),
    Disconnected,
    Line(String),
}

#[derive(Clone, PartialEq)]
struct PortInfo {
    name: String,
    board: bool,
}

fn list_ports() -> Vec<PortInfo> {
    let mut v: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let board = matches!(&p.port_type, serialport::SerialPortType::UsbPort(u) if u.vid == VID && u.pid == PID);
            PortInfo { name: p.port_name, board }
        })
        .collect();
    v.sort_by(|a, b| b.board.cmp(&a.board).then(a.name.cmp(&b.name)));
    v
}

/// A control message received while not connected.
fn idle_ctl(c: Ctl, target: &mut Option<Option<String>>, send: &dyn Fn(Ev)) {
    match c {
        Ctl::Connect(p) => *target = Some(p),
        Ctl::Disconnect => {
            *target = None;
            send(Ev::Disconnected);
        }
        Ctl::Send(_) => {}
    }
}

fn link(ev: Sender<Ev>, ctl: Receiver<Ctl>, wake: egui::Context) {
    let send = |e: Ev| {
        let _ = ev.send(e);
        wake.request_repaint();
    };
    let mut target: Option<Option<String>> = None;
    loop {
        let Some(want) = target.clone() else {
            match ctl.recv() {
                Ok(c) => idle_ctl(c, &mut target, &send),
                Err(_) => return,
            }
            continue;
        };
        let name = match want.or_else(|| list_ports().into_iter().find(|p| p.board).map(|p| p.name)) {
            Some(n) => n,
            None => {
                send(Ev::Status("Looking for the board... (check the USB cable)".into()));
                if let Ok(c) = ctl.recv_timeout(Duration::from_millis(700)) {
                    idle_ctl(c, &mut target, &send);
                }
                continue;
            }
        };
        let mut port = match serialport::new(&name, 115200).timeout(Duration::from_millis(20)).open() {
            Ok(p) => p,
            Err(e) => {
                send(Ev::Status(format!("{name}: cannot open: {e}")));
                if let Ok(c) = ctl.recv_timeout(Duration::from_millis(1000)) {
                    idle_ctl(c, &mut target, &send);
                }
                continue;
            }
        };
        let _ = port.write_data_terminal_ready(true); // the firmware only talks while DTR is set
        send(Ev::Connected(name.clone()));
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        'conn: loop {
            while let Ok(c) = ctl.try_recv() {
                match c {
                    Ctl::Send(s) => {
                        let _ = port.write_all(format!("{s}\r").as_bytes());
                    }
                    Ctl::Disconnect => {
                        target = None;
                        send(Ev::Disconnected);
                        break 'conn;
                    }
                    Ctl::Connect(p) => {
                        target = Some(p);
                        break 'conn;
                    }
                }
            }
            match port.read(&mut chunk) {
                Ok(0) => thread::sleep(Duration::from_millis(2)), // nothing yet: do not spin
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    let mut start = 0;
                    while let Some(p) = buf[start..].iter().position(|&b| b == b'\n') {
                        let ln = &buf[start..start + p];
                        let _ = ev.send(Ev::Line(String::from_utf8_lossy(ln).trim().to_string()));
                        start += p + 1;
                    }
                    if start > 0 {
                        buf.drain(..start);
                        wake.request_repaint_after(Duration::from_millis(33)); // data arrives ~200x/s: redraw at most ~30 fps
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => {
                    send(Ev::Status(format!("{name} disconnected: {e} - reconnecting")));
                    thread::sleep(Duration::from_millis(500));
                    break 'conn;
                }
            }
        }
    }
}

// ------------------------------------------------------------------ app

struct App {
    tx: Option<Sender<Ctl>>,
    rx: Option<Receiver<Ev>>,
    ports: Vec<PortInfo>,
    ports_at: Instant,
    choice: Option<String>, // None = automatic
    want_conn: bool,
    connected: Option<String>,
    status: String,
    log: Vec<String>,
    input: String,
    scope: scope::ScopeView,
    upd: Update,
    dbg_frames: u32, // SSV_FPS=1: frames per second in the window title
    dbg_at: Instant,
    dbg_ui: f64, // time spent in ui() this second, ms
    last_frame: Instant,
}

#[derive(Default)]
struct Update {
    open: bool,
    path: Option<std::path::PathBuf>,
    image: Option<Result<dfu::Image, String>>,
    rx: Option<Receiver<dfu::Progress>>,
    progress: f32,
    msg: String,
    result: Option<Result<String, String>>,
}

/// firmware next to the viewer (tools/ -> ../build/sutterspeed.hex) or in the same folder
fn default_firmware() -> Option<std::path::PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    [dir.join("sutterspeed.hex"), dir.join("../build/sutterspeed.hex")].into_iter().find(|p| p.exists())
}

impl App {
    fn new(cc: &eframe::CreationContext, port: Option<String>, sim: bool) -> Self {
        install_korean_font(&cc.egui_ctx);
        let mut app = App {
            tx: None,
            rx: None,
            ports: list_ports(),
            ports_at: Instant::now(),
            choice: port.clone(),
            want_conn: !sim,
            connected: None,
            status: String::new(),
            log: Vec::new(),
            input: String::new(),
            scope: scope::ScopeView::new(),
            upd: Update::default(),
            dbg_frames: 0,
            dbg_at: Instant::now(),
            dbg_ui: 0.0,
            last_frame: Instant::now(),
        };
        if sim {
            app.status = "Simulation (no board)".into();
            app.scope.simulate();
            return app;
        }
        let (etx, erx) = mpsc::channel();
        let (ctx_, crx) = mpsc::channel();
        let wake = cc.egui_ctx.clone();
        thread::spawn(move || link(etx, crx, wake));
        let _ = ctx_.send(Ctl::Connect(port));
        app.tx = Some(ctx_);
        app.rx = Some(erx);
        app
    }

    fn ctl(&self, c: Ctl) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(c);
        }
    }

    fn send(&mut self, c: &str) {
        if self.connected.is_some() {
            self.ctl(Ctl::Send(c.to_string()));
            self.push_log(format!("> {c}"));
        }
    }

    fn push_log(&mut self, s: String) {
        self.log.push(s);
        if self.log.len() > 2000 {
            self.log.drain(..500);
        }
    }

    fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        let evs: Vec<Ev> = rx.try_iter().collect();
        for e in evs {
            match e {
                Ev::Connected(n) => {
                    self.status = format!("{n} connected");
                    self.connected = Some(n);
                    for c in self.scope.start_cmds() {
                        self.send(&c);
                    }
                }
                Ev::Status(s) => {
                    self.connected = None;
                    self.status = s;
                }
                Ev::Disconnected => {
                    self.connected = None;
                    self.status = "Not connected".into();
                }
                Ev::Line(ln) => {
                    let echo = ln.starts_with("scope ");
                    if !self.scope.on_line(&ln) || echo {
                        // shot lines of the (paused) shutter measurement are not used here
                        if !ln.is_empty() && !ln.starts_with('#') {
                            self.push_log(ln);
                        }
                    }
                }
            }
        }
    }

    fn port_text(&self, name: &str) -> String {
        name.to_string()
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Port").strong());
            if self.connected.is_none() && self.ports_at.elapsed() > Duration::from_secs(2) {
                self.ports = list_ports();
                self.ports_at = Instant::now();
            }
            // shows the port in use; until one is picked the board is found by itself
            let sel = match (&self.connected, &self.choice) {
                (Some(n), _) => self.port_text(n),
                (None, Some(n)) if self.want_conn => format!("{n}  (connecting...)"),
                (None, Some(n)) => n.clone(),
                (None, None) if self.want_conn => "Looking for the board...".to_string(),
                (None, None) => "Select port".to_string(),
            };
            let current = self.connected.clone().or(self.choice.clone());
            let ports: Vec<PortInfo> = self.ports.iter().filter(|p| p.board).cloned().collect(); // sutterspeed boards only
            let mut pick = None;
            let r = egui::ComboBox::from_id_salt("port").width(200.0).selected_text(sel).show_ui(ui, |ui| {
                if ports.is_empty() {
                    ui.label(RichText::new("No sutterspeed board (check the USB connection)").weak());
                }
                for p in &ports {
                    let txt = p.name.clone();
                    if ui.selectable_label(current.as_deref() == Some(p.name.as_str()), txt).clicked() {
                        pick = Some(p.name.clone());
                    }
                }
            });
            if r.response.clicked() {
                self.ports = list_ports(); // opening the list re-reads the ports (shown from the next frame)
                self.ports_at = Instant::now();
            }
            if let Some(n) = pick {
                if self.connected.as_deref() != Some(n.as_str()) {
                    self.choice = Some(n);
                    self.want_conn = true;
                    self.ctl(Ctl::Connect(self.choice.clone()));
                }
            }
            // the icon shows the link as it is: a plain plug while connected, crossed out while not (also while the
            // cable is out and it reconnects by itself)
            let (icon, hint, accent) = match (self.connected.is_some(), self.want_conn) {
                (true, _) => (Icon::Plug, "Connected\nclick to disconnect", ORANGE),
                (false, true) => (Icon::Unplug, "Not connected: reconnects when the board is back\nclick to stop", ORANGE),
                (false, false) => (Icon::Unplug, "Not connected\nclick to connect", GREEN),
            };
            if icon_button(ui, icon, hint, self.tx.is_some(), accent).clicked() {
                self.want_conn = !self.want_conn;
                if self.want_conn {
                    self.ctl(Ctl::Connect(self.choice.clone()));
                } else {
                    self.ctl(Ctl::Disconnect);
                }
            }
            ui.separator();
            let col = if self.connected.is_some() { GREEN } else if self.want_conn { ORANGE } else { Color32::GRAY };
            ui.label(RichText::new(&self.status).color(col).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Icon::Firmware, "Firmware update (USB)\nwrite a .hex / .bin file to the board", true, GREEN).clicked() {
                    self.upd.open = true;
                    if self.upd.path.is_none() {
                        if let Some(p) = default_firmware() {
                            self.upd.image = Some(dfu::load(&p));
                            self.upd.path = Some(p);
                        }
                    }
                }
                let hint = if self.connected.is_some() {
                    "Zero\ncover the sensor completely, then click (learns the dark reading as 0 lx)"
                } else {
                    "Zero (board not connected)"
                };
                if icon_button(ui, Icon::Zero, hint, self.connected.is_some(), GREEN).clicked() {
                    self.send("scope off");
                    self.send("zero");
                    for c in self.scope.start_cmds() {
                        self.send(&c);
                    }
                }
                // sample count, left of the zero icon (right-to-left layout)
                let cmds = self.scope.sample_count_ui(ui);
                ui.label("Samples");
                for c in cmds {
                    self.send(&c);
                }
            });
        });
    }

    fn console(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Board messages / console").default_open(false).show(ui, |ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.input).hint_text("Command, then Enter (help)").desired_width(f32::INFINITY));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.input.trim().is_empty() {
                let c = self.input.trim().to_string();
                self.send(&c);
                self.input.clear();
                r.request_focus();
            }
            egui::ScrollArea::vertical().id_salt("log").max_height(160.0).stick_to_bottom(true).show(ui, |ui| {
                for l in &self.log {
                    ui.add(egui::Label::new(RichText::new(l).monospace().size(11.0)).wrap());
                }
            });
        });
    }
}

impl App {
    fn set_firmware(&mut self, p: std::path::PathBuf) {
        self.upd.image = Some(dfu::load(&p));
        self.upd.path = Some(p);
        self.upd.result = None;
    }

    fn start_update(&mut self, ctx: &egui::Context) {
        let Some(Ok(img)) = self.upd.image.take() else { return };
        let img_copy = dfu::Image { addr: img.addr, data: img.data.clone() };
        self.upd.image = Some(Ok(img));
        if self.connected.is_some() {
            self.send("dfu"); // the board restarts into the ST USB bootloader
            self.want_conn = false;
            self.ctl(Ctl::Disconnect);
        } else if !dfu::dfu_present() {
            self.upd.result = Some(Err("No board connected (needs the COM port or DFU mode)".into()));
            return;
        }
        let (tx, rx) = mpsc::channel();
        let wake = ctx.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            dfu::flash(&img_copy, &|p| {
                let _ = tx.send(p);
                wake.request_repaint();
            });
        });
        self.upd.rx = Some(rx);
        self.upd.progress = 0.0;
        self.upd.msg = "Switching the board to update mode...".into();
        self.upd.result = None;
    }

    fn update_window(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.upd.rx {
            let evs: Vec<dfu::Progress> = rx.try_iter().collect();
            for e in evs {
                match e {
                    dfu::Progress::Step(f, m) => {
                        self.upd.progress = f;
                        self.upd.msg = m;
                    }
                    dfu::Progress::Done(r) => {
                        self.upd.rx = None;
                        if r.is_ok() {
                            self.upd.progress = 1.0;
                        }
                        self.upd.result = Some(r);
                        self.want_conn = true; // reconnect to the (new) firmware
                        self.ctl(Ctl::Connect(self.choice.clone()));
                    }
                }
            }
        }
        let busy = self.upd.rx.is_some();
        if !self.upd.open {
            return;
        }
        if !busy {
            let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
            if let Some(p) = dropped.into_iter().next() {
                self.set_firmware(p);
            }
        }
        let mut open = self.upd.open;
        let mut start = false;
        let mut pick = false;
        egui::Window::new("Firmware update (USB)")
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Writes new firmware (.hex / .bin) over USB. No ST-LINK needed.");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui.button("Choose file...").clicked() {
                            pick = true;
                        }
                    });
                    ui.label(match &self.upd.path {
                        Some(p) => RichText::new(p.display().to_string()).monospace(),
                        None => RichText::new("Choose a file or drop it on this window").weak(),
                    });
                });
                match &self.upd.image {
                    Some(Ok(img)) => {
                        ui.label(format!("{1} bytes at 0x{0:08X} ({2:.1} KB / 64 KB)", img.addr, img.data.len(), img.data.len() as f64 / 1024.0));
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(e).color(Color32::from_rgb(220, 70, 60)));
                    }
                    None => {}
                }
                ui.add_space(6.0);
                let ready = matches!(self.upd.image, Some(Ok(_))) && !busy;
                if ui.add_enabled(ready, egui::Button::new(RichText::new("Start update").strong()).min_size(egui::vec2(140.0, 28.0))).clicked() {
                    start = true;
                }
                if busy || self.upd.progress > 0.0 {
                    ui.add(egui::ProgressBar::new(self.upd.progress).show_percentage());
                    ui.label(&self.upd.msg);
                }
                match &self.upd.result {
                    Some(Ok(m)) => {
                        ui.label(RichText::new(m).color(GREEN).strong());
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(format!("Failed: {e}")).color(Color32::from_rgb(220, 70, 60)).strong());
                        ui.label(RichText::new("The board stays in update mode (DFU): just try again.").small().weak());
                    }
                    None => {}
                }
                if busy {
                    ui.label(RichText::new("Do not unplug the USB cable while updating.").color(ORANGE));
                }
            });
        if pick {
            if let Some(p) = rfd::FileDialog::new().add_filter("firmware", &["hex", "bin"]).pick_file() {
                self.set_firmware(p);
            }
        }
        if start {
            self.start_update(ctx);
        }
        self.upd.open = open || busy; // stay open while flashing
    }
}

fn install_korean_font(ctx: &egui::Context) {
    for f in ["C:\\Windows\\Fonts\\malgun.ttf", "C:\\Windows\\Fonts\\gulim.ttc"] {
        if let Ok(bytes) = std::fs::read(f) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert("kr".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push("kr".into()); // fallback after the default fonts
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // at most ~60 frames per second: a fast mouse (500-1000 Hz) would otherwise make the UI redraw for every
        // event (vsync is not honoured on every system) and flood the main thread; input is merged into the next frame
        let since = self.last_frame.elapsed();
        if since < FRAME_MIN {
            thread::sleep(FRAME_MIN - since);
        }
        self.last_frame = Instant::now();
        let t_ui = Instant::now();
        self.poll();
        self.dbg_frames += 1;
        if self.dbg_at.elapsed() >= Duration::from_secs(1) {
            if std::env::var_os("SSV_FPS").is_some() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(format!("fps {} ui {:.2} ms", self.dbg_frames, self.dbg_ui / self.dbg_frames.max(1) as f64)));
            }
            self.dbg_frames = 0;
            self.dbg_ui = 0.0;
            self.dbg_at = Instant::now();
        }
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("console").show(ui, |ui| self.console(ui));
        let on = self.connected.is_some();
        let mut cmds = Vec::new();
        egui::Panel::right("scopectl").resizable(false).default_size(330.0).show(ui, |ui| {
            cmds.extend(self.scope.controls(ui, on)); // fixed: run / trigger mode, horizontal, vertical, trigger
            egui::ScrollArea::vertical().show(ui, |ui| cmds.extend(self.scope.controls_rest(ui)));
        });
        egui::CentralPanel::default().show(ui, |ui| cmds.extend(self.scope.plot(ui)));
        let ctx = ui.ctx().clone();
        self.update_window(&ctx);
        for c in cmds {
            self.send(&c);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        self.dbg_ui += t_ui.elapsed().as_secs_f64() * 1e3;
    }
}

/// `sutterspeed-viewer --flash firmware.hex`: update without the window (for scripts)
fn flash_cli(path: Option<&String>) -> i32 {
    let Some(path) = path else {
        println!("usage: sutterspeed-viewer --flash <firmware.hex|bin>");
        return 2;
    };
    let img = match dfu::load(std::path::Path::new(path)) {
        Ok(i) => i,
        Err(e) => {
            println!("error: {e}");
            return 1;
        }
    };
    if !dfu::dfu_present() {
        let Some(port) = list_ports().into_iter().find(|p| p.board) else {
            println!("error: board not found (neither COM port nor DFU bootloader)");
            return 1;
        };
        match serialport::new(&port.name, 115200).timeout(Duration::from_millis(200)).open() {
            Ok(mut p) => {
                let _ = p.write_data_terminal_ready(true);
                thread::sleep(Duration::from_millis(200));
                let _ = p.write_all(b"dfu\r");
                thread::sleep(Duration::from_millis(300));
            }
            Err(e) => {
                println!("error: {}: {e}", port.name);
                return 1;
            }
        }
    }
    let ok = std::cell::Cell::new(false);
    dfu::flash(&img, &|p| match p {
        dfu::Progress::Step(f, m) => println!("{:3.0}%  {m}", f * 100.0),
        dfu::Progress::Done(Ok(m)) => {
            println!("{m}");
            ok.set(true);
        }
        dfu::Progress::Done(Err(e)) => println!("error: {e}"),
    });
    if ok.get() { 0 } else { 1 }
}

fn main() -> eframe::Result {
    // DirectX 12 presents through the desktop compositor like any Windows app (Vulkan / OpenGL windows can make
    // a multi-monitor, high-refresh desktop stutter)
    if std::env::var_os("WGPU_BACKEND").is_none() {
        std::env::set_var("WGPU_BACKEND", "dx12");
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|a| a == "--flash") {
        std::process::exit(flash_cli(args.get(i + 1)));
    }
    let sim = args.iter().any(|a| a == "--sim");
    let port = args.into_iter().find(|a| !a.starts_with("--"));
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("sutterspeed light scope"),
        renderer: eframe::Renderer::Wgpu, // DirectX presentation: OpenGL windows made the whole desktop stutter on
        // a multi-monitor / high-refresh NVIDIA setup
        ..Default::default()
    };
    eframe::run_native("sutterspeed light scope", opts, Box::new(move |cc| Ok(Box::new(App::new(cc, port, sim)))))
}
