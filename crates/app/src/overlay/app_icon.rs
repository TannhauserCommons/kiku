//! Kiku's own icon, embedded in the exe from `assets/icon/kiku.ico` by `build.rs`. Everything that shows
//! Kiku's icon (windows, the tray, the title bars' mark) loads it from here.

use windows::{
    core::{Result, PCWSTR},
    Win32::{
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi},
            WindowsAndMessaging::{
                DestroyIcon, LoadImageW, RegisterClassExW, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON, SM_CXSMICON,
                SYSTEM_METRICS_INDEX, WNDCLASSEXW, WNDCLASSW,
            },
        },
    },
};

/// The icon's resource id: `winresource`'s `set_icon` embeds it as 1, which Windows also takes as the exe's icon.
const RESOURCE_ID: usize = 1;

/// Kiku's icon `size` pixels square: the embedded size that matches, or the nearest one scaled.
/// The caller owns it, to free with `DestroyIcon` unless it lives as long as Kiku.
pub fn app_icon(size: i32) -> Result<HICON> {
    let module = unsafe { GetModuleHandleW(None)? };
    let icon = unsafe { LoadImageW(Some(module.into()), PCWSTR(RESOURCE_ID as _), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)? };
    Ok(HICON(icon.0))
}

/// Kiku's icon at Windows' size for `metric` at the system DPI: `SM_CXSMICON` for small icons (the
/// tray, title bars), `SM_CXICON` for large ones (Alt+Tab).
pub fn app_icon_for(metric: SYSTEM_METRICS_INDEX) -> Result<HICON> {
    app_icon(unsafe { GetSystemMetricsForDpi(metric, GetDpiForSystem()) })
}

/// Registers `class` with Kiku's icon, large and small. Registering a class again fails harmlessly.
pub fn register_class(class: &WNDCLASSW) -> Result<()> {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: class.style,
        lpfnWndProc: class.lpfnWndProc,
        cbClsExtra: class.cbClsExtra,
        cbWndExtra: class.cbWndExtra,
        hInstance: class.hInstance,
        hIcon: app_icon_for(SM_CXICON)?,
        hCursor: class.hCursor,
        hbrBackground: class.hbrBackground,
        lpszMenuName: class.lpszMenuName,
        lpszClassName: class.lpszClassName,
        hIconSm: app_icon_for(SM_CXSMICON)?,
    };
    // A registered class keeps its icons for as long as Kiku runs; a repeat registration has no use for them.
    if unsafe { RegisterClassExW(&class) } == 0 {
        unsafe {
            let _ = DestroyIcon(class.hIcon);
            let _ = DestroyIcon(class.hIconSm);
        }
    }
    Ok(())
}
