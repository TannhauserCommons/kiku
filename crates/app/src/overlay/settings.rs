//! The settings window: a movable window with a tab per kind of setting, drawn by the panel's renderer
//! on a surface of its own. The panel's window owns it: its messages are handled there, like the panel's.

use windows::{
    core::{w, Result},
    Win32::{
        Foundation::{HWND, LRESULT, POINT, RECT},
        Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, ScreenToClient, MONITORINFO, MONITOR_DEFAULTTONEAREST},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT},
            WindowsAndMessaging::{
                CreateWindowExW, GetWindowRect, IsWindowVisible, LoadCursorW, SetForegroundWindow, SetWindowLongPtrW,
                SetWindowPos, ShowWindow, GWLP_USERDATA, HTCAPTION, HTCLIENT, IDC_ARROW, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOW,
                WNDCLASSW, WNDPROC, WS_EX_LAYERED, WS_POPUP,
            },
        },
    },
};

use super::{
    app_icon::register_class,
    render::Surface,
    view::{Tab, Target, View},
};

/// The window's size in DIPs.
pub const SIZE: (f32, f32) = (560.0, 620.0);

pub struct SettingsWindow {
    pub hwnd: HWND,
    pub view: View,
    pub surface: Surface,
    tracking_mouse: bool,
    /// Placed on a screen yet; after that it opens where it was left.
    placed: bool,
}

impl SettingsWindow {
    /// Creates the window hidden, owned by the panel so it stays in front of it and goes with it.
    /// `proc` handles its messages; the panel's handle is kept in its user data.
    pub fn new(panel: HWND, proc: WNDPROC) -> Result<Self> {
        let hwnd = create_window(panel, proc)?;
        Ok(Self { hwnd, view: View::new(), surface: Surface::default(), tracking_mouse: false, placed: false })
    }

    pub fn is_open(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd) }.as_bool()
    }

    /// Shows the window in front, centred on the panel's screen the first time.
    pub fn open(&mut self, panel: HWND) {
        if !self.placed {
            self.placed = self.centre_on(panel).is_ok();
        }
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    pub fn close(&mut self) {
        let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        (self.view.hovered, self.view.open_dropdown) = (None, None);
    }

    /// Where the window is on screen, in pixels.
    pub fn bounds(&self) -> RECT {
        let mut bounds = RECT::default();
        let _ = unsafe { GetWindowRect(self.hwnd, &mut bounds) };
        bounds
    }

    pub fn scale(&self) -> f32 {
        (unsafe { GetDpiForWindow(self.hwnd) }) as f32 / 96.0
    }

    /// Window coordinates of a client point, in DIPs.
    pub fn to_dips(&self, client: POINT) -> (f32, f32) {
        let scale = self.scale();
        (client.x as f32 / scale, client.y as f32 / scale)
    }

    pub fn target_at(&self, client: POINT) -> Option<Target> {
        let (x, y) = self.to_dips(client);
        self.view.regions.target_at(x, y)
    }

    /// The title bar drags the window; everything else is clickable.
    pub fn hit_test(&self, screen: POINT) -> LRESULT {
        let mut client = screen;
        let _ = unsafe { ScreenToClient(self.hwnd, &mut client) };
        LRESULT(if self.target_at(client) == Some(Target::Drag) { HTCAPTION } else { HTCLIENT } as isize)
    }

    /// Asks Windows for a mouse-leave message once the pointer leaves the window.
    pub fn track_mouse(&mut self) {
        if !self.tracking_mouse {
            let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
            self.tracking_mouse = unsafe { TrackMouseEvent(&mut track) }.is_ok();
        }
    }

    pub fn mouse_left(&mut self) {
        self.tracking_mouse = false;
        self.view.hovered = None;
    }

    /// Opens `tab` at its top.
    pub fn show_tab(&mut self, tab: Tab) {
        self.view.tab = tab;
        self.view.open_dropdown = None;
        self.view.scrolls.clear();
    }

    fn centre_on(&self, panel: HWND) -> Result<()> {
        let monitor = unsafe { MonitorFromWindow(panel, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = unsafe { GetMonitorInfoW(monitor, &mut info) };
        let (mut dpi, mut dpi_y) = (96, 96);
        unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y)? };
        let scale = dpi as f32 / 96.0;
        let (width, height) = ((SIZE.0 * scale).round() as i32, (SIZE.1 * scale).round() as i32);
        let work = info.rcWork;
        let (x, y) = ((work.left + work.right - width) / 2, (work.top + work.bottom - height) / 2);
        unsafe { SetWindowPos(self.hwnd, None, x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE) }
    }
}

fn create_window(panel: HWND, proc: WNDPROC) -> Result<HWND> {
    let instance = unsafe { GetModuleHandleW(None)? };
    let class = WNDCLASSW {
        lpfnWndProc: proc,
        hInstance: instance.into(),
        lpszClassName: w!("KikuSettings"),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        ..Default::default()
    };
    register_class(&class)?;
    let hwnd = unsafe {
        CreateWindowExW(WS_EX_LAYERED, w!("KikuSettings"), w!("Kiku settings"), WS_POPUP, 0, 0, 0, 0, Some(panel), None, Some(instance.into()), None)?
    };
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, panel.0 as isize) };
    Ok(hwnd)
}
