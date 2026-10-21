//! Firmware update over USB: the board's `dfu` command restarts it into the STM32 system bootloader
//! (USB 0483:DF11, WinUSB driver), which is then programmed with the DfuSe protocol (ST extensions of DFU 1.1):
//! erase the pages, write 1 KB blocks, read them back, start the new firmware.

use nusb::transfer::{Control, ControlType, Recipient};
use std::time::{Duration, Instant};

const DFU_VID: u16 = 0x0483;
const DFU_PID: u16 = 0xDF11;
const FLASH_BASE: u32 = 0x0800_0000;
const FLASH_SIZE: u32 = 64 * 1024; // STM32C071G8
const PAGE: u32 = 2048;
const CHUNK: usize = 1024;
const TO: Duration = Duration::from_secs(5);

// DFU requests and states
const DNLOAD: u8 = 1;
const UPLOAD: u8 = 2;
const GETSTATUS: u8 = 3;
const CLRSTATUS: u8 = 4;
const ABORT: u8 = 6;
const ST_IDLE: u8 = 2;
const ST_DNBUSY: u8 = 4;
const ST_DNIDLE: u8 = 5;
const ST_UPIDLE: u8 = 9;
const ST_ERROR: u8 = 10;

pub enum Progress {
    Step(f32, String), // 0..1, what is happening
    Done(Result<String, String>),
}

/// Firmware image: start address + contiguous bytes.
pub struct Image {
    pub addr: u32,
    pub data: Vec<u8>,
}

pub fn load(path: &std::path::Path) -> Result<Image, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read the file: {e}"))?;
    let is_hex = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("hex"));
    let img = if is_hex { parse_hex(&String::from_utf8_lossy(&bytes))? } else { Image { addr: FLASH_BASE, data: bytes } };
    if img.data.is_empty() {
        return Err("empty file".into());
    }
    if img.addr < FLASH_BASE || img.addr + img.data.len() as u32 > FLASH_BASE + FLASH_SIZE {
        return Err(format!("outside the flash (0x08000000, 64 KB): 0x{:08X} + {} B", img.addr, img.data.len()));
    }
    // sanity check of the vector table: initial stack pointer in RAM, reset handler in flash
    if img.addr == FLASH_BASE && img.data.len() >= 8 {
        let sp = u32::from_le_bytes(img.data[0..4].try_into().unwrap());
        let pc = u32::from_le_bytes(img.data[4..8].try_into().unwrap());
        if sp & 0xFFF0_0000 != 0x2000_0000 || pc & 0xFFF0_0000 != 0x0800_0000 {
            return Err("does not look like STM32 firmware (vector table)".into());
        }
    }
    Ok(img)
}

fn parse_hex(text: &str) -> Result<Image, String> {
    let mut base = 0u32;
    let mut recs: Vec<(u32, Vec<u8>)> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let b = l
            .strip_prefix(':')
            .and_then(|h| (0..h.len() / 2).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok()).collect::<Option<Vec<u8>>>())
            .ok_or(format!("HEX line {}: bad format", n + 1))?;
        if b.len() < 5 || b.len() != 5 + b[0] as usize || b.iter().fold(0u8, |a, &x| a.wrapping_add(x)) != 0 {
            return Err(format!("HEX line {}: bad checksum / length", n + 1));
        }
        let off = u16::from_be_bytes([b[1], b[2]]) as u32;
        let d = &b[4..4 + b[0] as usize];
        match b[3] {
            0 => recs.push((base + off, d.to_vec())),
            1 => break,
            2 => base = (u16::from_be_bytes([d[0], d[1]]) as u32) << 4,
            4 => base = (u16::from_be_bytes([d[0], d[1]]) as u32) << 16,
            _ => {}
        }
    }
    let lo = recs.iter().map(|r| r.0).min().ok_or("no data in the HEX file")?;
    let hi = recs.iter().map(|r| r.0 + r.1.len() as u32).max().unwrap();
    let mut data = vec![0xFFu8; (hi - lo) as usize];
    for (a, d) in recs {
        let o = (a - lo) as usize;
        data[o..o + d.len()].copy_from_slice(&d);
    }
    Ok(Image { addr: lo, data })
}

struct Dfu {
    intf: nusb::Interface,
}

impl Dfu {
    fn ctl(request: u8, value: u16) -> Control {
        Control { control_type: ControlType::Class, recipient: Recipient::Interface, request, value, index: 0 }
    }

    fn out(&self, request: u8, value: u16, data: &[u8]) -> Result<(), String> {
        self.intf.control_out_blocking(Self::ctl(request, value), data, TO).map(|_| ()).map_err(|e| format!("USB transfer failed: {e}"))
    }

    fn status(&self) -> Result<(u8, u8, u32), String> {
        let mut b = [0u8; 6];
        self.intf.control_in_blocking(Self::ctl(GETSTATUS, 0), &mut b, TO).map_err(|e| format!("reading the status failed: {e}"))?;
        Ok((b[0], b[4], u32::from_le_bytes([b[1], b[2], b[3], 0])))
    }

    /// GETSTATUS until the bootloader has finished the last request.
    fn wait(&self) -> Result<u8, String> {
        let t0 = Instant::now();
        loop {
            let (status, state, poll) = self.status()?;
            if state == ST_ERROR || status != 0 {
                return Err(format!("bootloader error (status {status}, state {state})"));
            }
            if state != ST_DNBUSY {
                return Ok(state);
            }
            if t0.elapsed() > Duration::from_secs(10) {
                return Err("the bootloader does not answer".into());
            }
            std::thread::sleep(Duration::from_millis(poll.clamp(1, 200) as u64));
        }
    }

    fn to_idle(&self) -> Result<(), String> {
        let (_, state, _) = self.status()?;
        if state == ST_ERROR {
            self.out(CLRSTATUS, 0, &[])?;
        } else if state != ST_IDLE {
            self.out(ABORT, 0, &[])?;
        }
        Ok(())
    }

    fn command(&self, cmd: u8, addr: u32) -> Result<(), String> {
        let mut b = vec![cmd];
        b.extend_from_slice(&addr.to_le_bytes());
        self.out(DNLOAD, 0, &b)?;
        self.wait()?;
        Ok(())
    }

    fn write(&self, addr: u32, data: &[u8]) -> Result<(), String> {
        self.command(0x21, addr)?; // set address pointer
        self.out(DNLOAD, 2, data)?;
        let st = self.wait()?;
        if st != ST_DNIDLE && st != ST_IDLE {
            return Err(format!("unexpected state after writing ({st})"));
        }
        Ok(())
    }

    fn read(&self, addr: u32, len: usize) -> Result<Vec<u8>, String> {
        self.command(0x21, addr)?;
        self.out(ABORT, 0, &[])?; // back to idle so UPLOAD starts at the pointer
        let mut b = vec![0u8; len];
        let n = self.intf.control_in_blocking(Self::ctl(UPLOAD, 2), &mut b, TO).map_err(|e| format!("read failed: {e}"))?;
        b.truncate(n);
        let (_, st, _) = self.status()?;
        if st == ST_UPIDLE {
            self.out(ABORT, 0, &[])?;
        }
        Ok(b)
    }

    fn leave(&self) {
        // set the pointer to the start, then a zero-length download: the bootloader jumps to the firmware
        let _ = self.command(0x21, FLASH_BASE);
        let _ = self.out(DNLOAD, 0, &[]);
        let _ = self.status(); // the device usually disappears here
    }
}

pub fn dfu_present() -> bool {
    nusb::list_devices().map(|mut it| it.any(|d| d.vendor_id() == DFU_VID && d.product_id() == DFU_PID)).unwrap_or(false)
}

fn open(wait: Duration) -> Result<Dfu, String> {
    let t0 = Instant::now();
    loop {
        if let Ok(mut it) = nusb::list_devices() {
            if let Some(di) = it.find(|d| d.vendor_id() == DFU_VID && d.product_id() == DFU_PID) {
                let dev = di.open().map_err(|e| format!("cannot open the bootloader: {e} (check the STM32 Bootloader driver)"))?;
                let intf = dev.claim_interface(0).map_err(|e| format!("cannot claim the interface: {e}"))?;
                let _ = intf.set_alt_setting(0); // alt 0 = internal flash
                return Ok(Dfu { intf });
            }
        }
        if t0.elapsed() > wait {
            return Err("the bootloader (USB 0483:DF11) did not show up".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Erase, write, verify, start. Runs on a worker thread; `report` gets the progress.
pub fn flash(img: &Image, report: &dyn Fn(Progress)) {
    let step = |f: f32, s: &str| report(Progress::Step(f, s.to_string()));
    let r = (|| -> Result<String, String> {
        step(0.0, "Waiting for the bootloader...");
        let d = open(Duration::from_secs(10))?;
        d.to_idle()?;

        // pad to a multiple of 8 bytes (flash is programmed in double words)
        let mut data = img.data.clone();
        while data.len() % 8 != 0 {
            data.push(0xFF);
        }
        let end = img.addr + data.len() as u32;
        let first = img.addr & !(PAGE - 1);
        let pages: Vec<u32> = (first..end).step_by(PAGE as usize).collect();
        for (i, &p) in pages.iter().enumerate() {
            step(0.25 * i as f32 / pages.len() as f32, &format!("Erasing 0x{p:08X}"));
            d.command(0x41, p)?;
        }
        let chunks: Vec<(u32, &[u8])> = data.chunks(CHUNK).enumerate().map(|(i, c)| (img.addr + (i * CHUNK) as u32, c)).collect();
        for (i, &(a, c)) in chunks.iter().enumerate() {
            step(0.25 + 0.5 * i as f32 / chunks.len() as f32, &format!("Writing 0x{a:08X}"));
            d.write(a, c)?;
        }
        for (i, &(a, c)) in chunks.iter().enumerate() {
            step(0.75 + 0.23 * i as f32 / chunks.len() as f32, &format!("Verifying 0x{a:08X}"));
            let back = d.read(a, c.len())?;
            if back != c {
                return Err(format!("verify failed: block 0x{a:08X} differs"));
            }
        }
        step(0.99, "Starting the new firmware...");
        d.leave();
        Ok(format!("Done: {} bytes written and verified (0x{:08X})", data.len(), img.addr))
    })();
    report(Progress::Done(r));
}
