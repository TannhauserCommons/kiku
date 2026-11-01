//! The notification-area ("hidden icons") entry that brings the hidden overlay back.

use windows::{
    core::{w, Result},
    Win32::{
        Foundation::{HWND, POINT},
        UI::{
            Shell::{Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW},
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos, SetForegroundWindow, TrackPopupMenu,
                MF_STRING, SM_CXSMICON, TPM_NONOTIFY, TPM_RETURNCMD, WM_APP,
            },
        },
    },
};

use super::app_icon::app_icon_for;

/// Sent to the overlay window when the tray icon is clicked; `lParam` holds the mouse message.
pub const WM_TRAY: u32 = WM_APP + 2;

/// Choices in the tray icon's right-click menu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MenuChoice {
    ShowOrHide,
    ToggleSubtitles,
    ToggleLock,
    ToggleStop,
    Quit,
}

/// What the menu labels should reflect.
#[derive(Clone, Copy, Debug)]
pub struct MenuState {
    pub visible: bool,
    /// Captions mode is on instead of the panel.
    pub subtitles: bool,
    pub locked: bool,
    pub stopped: bool,
}

/// The tray icon, at the small icon size; removed when dropped.
pub struct Tray {
    data: NOTIFYICONDATAW,
}

impl Tray {
    pub fn add(hwnd: HWND) -> Result<Self> {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: app_icon_for(SM_CXSMICON)?,
            ..Default::default()
        };
        let tip: Vec<u16> = "Kiku".encode_utf16().collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        // Made first, so that dropping it frees the icon if adding fails.
        let tray = Self { data };
        unsafe { Shell_NotifyIconW(NIM_ADD, &tray.data).ok()? };
        Ok(tray)
    }

    /// Pops a notification from the icon, for news while the panel is hidden.
    pub fn balloon(&self, text: &str) {
        let mut data = NOTIFYICONDATAW { uFlags: NIF_INFO, dwInfoFlags: NIIF_INFO, ..self.data };
        copy_truncated(&mut data.szInfoTitle, "Kiku");
        copy_truncated(&mut data.szInfo, text);
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    }

    /// Shows the right-click menu at the cursor and returns what was picked.
    pub fn menu(&self, state: MenuState) -> Option<MenuChoice> {
        let items = [
            (MenuChoice::ShowOrHide, if state.visible { w!("Hide") } else { w!("Show") }),
            (MenuChoice::ToggleSubtitles, if state.subtitles { w!("Show the transcript panel") } else { w!("Show captions (subtitles)") }),
            (MenuChoice::ToggleLock, if state.locked { w!("Unlock") } else { w!("Lock (click-through)") }),
            (MenuChoice::ToggleStop, if state.stopped { w!("Start captioning") } else { w!("Stop captioning") }),
            (MenuChoice::Quit, w!("Quit")),
        ];
        unsafe {
            let menu = CreatePopupMenu().ok()?;
            // Menu ids start at 1, since 0 means nothing was picked.
            for (id, (_, label)) in items.iter().enumerate() {
                AppendMenuW(menu, MF_STRING, id + 1, *label).ok()?;
            }
            let mut cursor = POINT::default();
            GetCursorPos(&mut cursor).ok()?;
            // Without this the menu would not close when clicking elsewhere.
            let _ = SetForegroundWindow(self.data.hWnd);
            let picked = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY, cursor.x, cursor.y, None, self.data.hWnd, None);
            let _ = DestroyMenu(menu);
            (picked.0 as usize).checked_sub(1).and_then(|index| items.get(index)).map(|(choice, _)| *choice)
        }
    }
}

/// `text` into a fixed, nul-terminated UTF-16 buffer, cut to fit.
fn copy_truncated(buffer: &mut [u16], text: &str) {
    let text: Vec<u16> = text.encode_utf16().take(buffer.len() - 1).collect();
    buffer[..text.len()].copy_from_slice(&text);
    buffer[text.len()] = 0;
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data);
            let _ = DestroyIcon(self.data.hIcon);
        }
    }
}
