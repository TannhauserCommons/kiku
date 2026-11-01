//! Shows everything the keyboards send for 20 s, to find out whether a key like Fn reaches Windows.
//! Watches three layers: the low-level keyboard hook (what shortcuts see), raw keyboard input, and raw
//! reports from every other HID collection (media keys, vendor channels such as a keyboard's Fn).
//! Usage: key_probe, then press the key alone a few times. Run it from a folder without a Low integrity
//! label (not the agent workspace), or Windows hides input meant for other apps.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use windows::{
    core::w,
    Win32::{
        Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::{
                GetRawInputData, GetRawInputDeviceInfoW, GetRawInputDeviceList, RegisterRawInputDevices, HRAWINPUT, RAWINPUT,
                RAWINPUTDEVICE, RAWINPUTDEVICELIST, RAWINPUTHEADER, RIDEV_INPUTSINK, RIDI_DEVICEINFO, RID_DEVICE_INFO, RID_INPUT,
                RIM_TYPEHID, RIM_TYPEKEYBOARD,
            },
            WindowsAndMessaging::{
                CallNextHookEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW, SetTimer,
                SetWindowsHookExW, KBDLLHOOKSTRUCT, LLKHF_UP, MSG, WH_KEYBOARD_LL, WINDOW_EX_STYLE, WM_INPUT, WM_TIMER, WNDCLASSW,
                WS_OVERLAPPED,
            },
        },
    },
};

const WATCH_FOR: Duration = Duration::from_secs(20);

fn main() -> windows::core::Result<()> {
    let window = hidden_window()?;
    let collections = hid_collections();
    let devices: Vec<RAWINPUTDEVICE> = collections
        .iter()
        .map(|&(page, usage)| RAWINPUTDEVICE { usUsagePage: page, usUsage: usage, dwFlags: RIDEV_INPUTSINK, hwndTarget: window })
        .collect();
    unsafe { RegisterRawInputDevices(&devices, size_of::<RAWINPUTDEVICE>() as u32)? };
    let _hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0)? };
    println!("Watching for {} s. Press the key alone a few times, slowly.\n", WATCH_FOR.as_secs());

    let start = Instant::now();
    unsafe { SetTimer(None, 0, 500, None) };
    let mut last_report: HashMap<isize, Vec<u8>> = HashMap::new();
    let mut message = MSG::default();
    while start.elapsed() < WATCH_FOR && unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        match message.message {
            WM_INPUT => print_raw(HRAWINPUT(message.lParam.0 as _), &mut last_report),
            WM_TIMER => {}
            _ => unsafe {
                DispatchMessageW(&message);
            },
        }
    }
    println!("\nDone.");
    Ok(())
}

/// Every (usage page, usage) of the keyboards and other HID devices attached, listed as they are found.
fn hid_collections() -> Vec<(u16, u16)> {
    let mut count = 0;
    unsafe { GetRawInputDeviceList(None, &mut count, size_of::<RAWINPUTDEVICELIST>() as u32) };
    let mut list = vec![RAWINPUTDEVICELIST::default(); count as usize];
    unsafe { GetRawInputDeviceList(Some(list.as_mut_ptr()), &mut count, size_of::<RAWINPUTDEVICELIST>() as u32) };
    let mut collections = HashSet::from([(0x01, 0x06)]);
    for device in &list {
        let mut info = RID_DEVICE_INFO { cbSize: size_of::<RID_DEVICE_INFO>() as u32, ..Default::default() };
        let mut size = info.cbSize;
        unsafe { GetRawInputDeviceInfoW(Some(device.hDevice), RIDI_DEVICEINFO, Some(&mut info as *mut _ as _), &mut size) };
        if device.dwType == RIM_TYPEHID {
            let hid = unsafe { info.Anonymous.hid };
            println!("HID device {:04x}:{:04x}  page {:#06x} usage {:#06x}", hid.dwVendorId, hid.dwProductId, hid.usUsagePage, hid.usUsage);
            collections.insert((hid.usUsagePage, hid.usUsage));
        }
    }
    let mut sorted: Vec<_> = collections.into_iter().collect();
    sorted.sort();
    sorted
}

fn print_raw(input: HRAWINPUT, last_report: &mut HashMap<isize, Vec<u8>>) {
    let mut size = 0;
    let header = size_of::<RAWINPUTHEADER>() as u32;
    unsafe { GetRawInputData(input, RID_INPUT, None, &mut size, header) };
    let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
    unsafe { GetRawInputData(input, RID_INPUT, Some(buffer.as_mut_ptr() as _), &mut size, header) };
    let raw = unsafe { &*(buffer.as_ptr() as *const RAWINPUT) };
    let device = raw.header.hDevice;
    if raw.header.dwType == RIM_TYPEKEYBOARD.0 {
        let key = unsafe { raw.data.keyboard };
        let state = if key.Flags & 1 != 0 { "up" } else { "down" };
        println!("raw keyboard   vkey {:#04x}  scan {:#04x}  flags {:#x}  {state}", key.VKey, key.MakeCode, key.Flags);
    } else {
        let hid = unsafe { &raw.data.hid };
        let bytes = unsafe { std::slice::from_raw_parts(hid.bRawData.as_ptr(), (hid.dwSizeHid * hid.dwCount) as usize) }.to_vec();
        // Some devices stream reports continuously; only changes are interesting.
        if last_report.insert(device_id(device), bytes.clone()).as_ref() != Some(&bytes) {
            let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
            println!("raw HID        device {:#x}  report {}", device_id(device), hex.join(" "));
        }
    }
}

fn device_id(device: HANDLE) -> isize {
    device.0 as isize
}

extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let state = if key.flags.contains(LLKHF_UP) { "up" } else { "down" };
        println!("hook           vk {:#04x}  scan {:#04x}  flags {:#x}  {state}", key.vkCode, key.scanCode, key.flags.0);
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn hidden_window() -> windows::core::Result<HWND> {
    let instance = unsafe { GetModuleHandleW(None)? };
    let class = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance.into(), lpszClassName: w!("KeyProbe"), ..Default::default() };
    unsafe { RegisterClassW(&class) };
    unsafe { CreateWindowExW(WINDOW_EX_STYLE(0), w!("KeyProbe"), w!("Key probe"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(instance.into()), None) }
}

extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
