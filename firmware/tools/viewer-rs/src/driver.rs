//! WinUSB for the ST system bootloader (USB 0483:DF11). That device carries no Microsoft OS descriptors, so on a PC
//! without STM32CubeProgrammer it shows up without a driver. Windows has WinUSB built in (winusb.inf, "WinUsb
//! Device"): the viewer binds it to the device the way Device Manager's "pick from a list" does. That needs
//! administrator rights, so the viewer starts itself elevated (`--install-driver`; Windows asks once).
//! Our own bootloader (1209:0002) needs none of this: Windows binds WinUSB from its descriptors.

use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};

const ROM_HWID: &str = "USB\\VID_0483&PID_DF11";

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn from_wide(b: &[u16]) -> String {
    String::from_utf16_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())])
}

/// present USB devices; `f` gets the device set and each device with its first hardware ID and its service
fn each_device(mut f: impl FnMut(HDEVINFO, &mut SP_DEVINFO_DATA, &str, &str) -> bool) -> Result<bool, String> {
    let usb = wide("USB");
    unsafe {
        let set = SetupDiGetClassDevsW(std::ptr::null(), usb.as_ptr(), std::ptr::null_mut(), DIGCF_PRESENT | DIGCF_ALLCLASSES);
        if set == -1 {
            return Err(format!("cannot list the USB devices (error {})", GetLastError()));
        }
        let prop = |di: &SP_DEVINFO_DATA, p| {
            let mut b = [0u16; 512];
            let ok = SetupDiGetDeviceRegistryPropertyW(set, di, p, std::ptr::null_mut(), b.as_mut_ptr().cast(), 1024, std::ptr::null_mut());
            if ok != 0 { from_wide(&b) } else { String::new() }
        };
        let mut found = false;
        for i in 0.. {
            let mut di: SP_DEVINFO_DATA = std::mem::zeroed();
            di.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiEnumDeviceInfo(set, i, &mut di) == 0 {
                break;
            }
            let (hwid, service) = (prop(&di, SPDRP_HARDWAREID), prop(&di, SPDRP_SERVICE));
            if f(set, &mut di, &hwid, &service) {
                found = true;
                break;
            }
        }
        SetupDiDestroyDeviceInfoList(set);
        Ok(found)
    }
}

/// the ST bootloader is plugged in but has no WinUSB driver
pub fn rom_needs_driver() -> bool {
    each_device(|_, _, hwid, service| hwid.to_ascii_uppercase().starts_with(ROM_HWID) && !service.eq_ignore_ascii_case("WinUSB"))
        .unwrap_or(false)
}

/// Start this program elevated with `--install-driver` and wait for it (Windows shows its permission prompt).
pub fn install_elevated() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let (verb, args) = (wide("runas"), wide("--install-driver"));
    unsafe {
        let mut sei: SHELLEXECUTEINFOW = std::mem::zeroed();
        sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        sei.lpVerb = verb.as_ptr();
        sei.lpFile = file.as_ptr();
        sei.lpParameters = args.as_ptr();
        sei.nShow = 0; // SW_HIDE
        if ShellExecuteExW(&mut sei) == 0 || sei.hProcess.is_null() {
            return Err(match GetLastError() {
                1223 => "the driver installation was cancelled".into(), // ERROR_CANCELLED: "No" in the prompt
                e => format!("cannot start the driver installation (error {e})"),
            });
        }
        WaitForSingleObject(sei.hProcess, INFINITE);
        let mut code = 1u32;
        GetExitCodeProcess(sei.hProcess, &mut code);
        CloseHandle(sei.hProcess);
        match code {
            0 => Ok(()),
            2 => Err("the ST bootloader is no longer plugged in".into()),
            c => Err(format!("installing WinUSB failed (code {c})")),
        }
    }
}

/// `--install-driver` (elevated): bind Windows' own WinUSB driver to the ST bootloader. Exit code 0 = done.
pub fn install_now() -> i32 {
    let mut result = Err(2);
    let _ = each_device(|set, di, hwid, service| {
        if !hwid.to_ascii_uppercase().starts_with(ROM_HWID) {
            return false;
        }
        result = if service.eq_ignore_ascii_case("WinUSB") { Ok(()) } else { unsafe { bind_winusb(set, di) } };
        true
    });
    match result {
        Ok(()) => 0,
        Err(c) => c,
    }
}

unsafe fn bind_winusb(set: HDEVINFO, di: &mut SP_DEVINFO_DATA) -> Result<(), i32> {
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    let inf = wide(&format!("{windir}\\INF\\winusb.inf"));
    if inf.len() > 260 {
        return Err(3);
    }
    // only winusb.inf, its "WinUsb Device" model included (it is not offered automatically)
    let mut p: SP_DEVINSTALL_PARAMS_W = std::mem::zeroed();
    p.cbSize = std::mem::size_of::<SP_DEVINSTALL_PARAMS_W>() as u32;
    if SetupDiGetDeviceInstallParamsW(set, di, &mut p) == 0 {
        return Err(4);
    }
    p.Flags |= DI_ENUMSINGLEINF;
    p.FlagsEx |= DI_FLAGSEX_ALLOWEXCLUDEDDRVS;
    p.DriverPath[..inf.len()].copy_from_slice(&inf);
    if SetupDiSetDeviceInstallParamsW(set, di, &p) == 0 {
        return Err(5);
    }
    // a device without a driver has no class; the "WinUsb Device" model is in the USBDevice class
    let mut drv = first_driver(set, di);
    if drv.is_none() {
        SetupDiDestroyDriverInfoList(set, di, SPDIT_CLASSDRIVER);
        let class = wide("{88BAE032-5A81-49F0-BC3D-A4FF138216D6}");
        SetupDiSetDeviceRegistryPropertyW(set, di, SPDRP_CLASSGUID, class.as_ptr().cast(), (class.len() * 2) as u32);
        drv = first_driver(set, di);
    }
    let Some(mut drv) = drv else { return Err(6) };
    if SetupDiSetSelectedDriverW(set, di, &mut drv) == 0 {
        return Err(7);
    }
    let mut reboot = 0;
    if DiInstallDevice(std::ptr::null_mut(), set, di, &drv, 0, &mut reboot) == 0 {
        return Err(10_000 + GetLastError() as i32);
    }
    Ok(())
}

unsafe fn first_driver(set: HDEVINFO, di: &mut SP_DEVINFO_DATA) -> Option<SP_DRVINFO_DATA_V2_W> {
    if SetupDiBuildDriverInfoList(set, di, SPDIT_CLASSDRIVER) == 0 {
        return None;
    }
    let mut d: SP_DRVINFO_DATA_V2_W = std::mem::zeroed();
    d.cbSize = std::mem::size_of::<SP_DRVINFO_DATA_V2_W>() as u32;
    (0..).map_while(|i| (SetupDiEnumDriverInfoW(set, di, SPDIT_CLASSDRIVER, i, &mut d) != 0).then_some(d)).find(|d| {
        from_wide(&d.Description).to_ascii_lowercase().contains("winusb")
    })
}
