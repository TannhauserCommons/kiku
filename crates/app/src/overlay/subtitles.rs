//! Captions mode's windows: the lines, which always let clicks through, and the || tab beside them that
//! drags them (and, double-clicked, brings the panel back). Both show while there is something to read and fade out after a few quiet seconds.
//!
//! The panel's window owns this: its timers and the tab's mouse messages arrive there.

use windows::{
    core::{w, Result},
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::Dialogs::{ChooseColorW, CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW},
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, GetCursorPos, GetWindowLongPtrW, KillTimer, LoadCursorW, SendMessageW, SetCursor,
                SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_USERDATA, IDC_SIZEALL, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
                SW_HIDE, SW_SHOWNOACTIVATE, CS_DBLCLKS, WINDOW_EX_STYLE, WM_APP, WM_DPICHANGED, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
                WM_MOUSEMOVE, WM_SETCURSOR,
                WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    },
};

use super::app_icon::register_class;
use super::render::{Renderer, Speaker, SubtitleLine, SubtitleScene, Surface, DRAG_TAB};
use kiku_core::{
    captions::{Captions, YOU_SECTION},
    subtitles::{newest, Mark, SubtitleSettings},
    theme::Themes,
};

/// A subtitle window's mouse or DPI message, passed on to the panel; `wParam` holds the message.
pub const WM_SUBTITLES: u32 = WM_APP + 6;
/// Fades the lines out once nothing has changed for this long. Set on the panel, so its id must differ
/// from the panel's own timers.
pub const QUIET_TIMER: usize = 8;
const QUIET_MS: u32 = 4000;
/// Steps the fade, which takes `FADE_STEPS` of these.
pub const FADE_TIMER: usize = 9;
const FADE_STEP_MS: u32 = 30;
const FADE_STEPS: u8 = 10;
/// Defined in the `UI::Controls` bindings' mouse-tracking part, which this module doesn't otherwise need.
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Space between the drag tab and the pills, in DIPs.
const TAB_GAP: f32 = 6.0;
/// The lines take this share of their screen's width.
const SCREEN_SHARE: f32 = 2.0 / 3.0;
/// Where the lines sit by default: this share of the screen's height above the taskbar.
const BOTTOM_LIFT: f32 = 0.06;

/// What a message from the tab asks the panel to do.
#[derive(Clone, Copy, PartialEq)]
pub enum TabEvent {
    Nothing,
    /// A drag put the lines somewhere new, to be saved.
    Moved,
    /// Double-clicked: back to the transcript panel.
    ShowPanel,
}

pub struct Subtitles {
    pub settings: SubtitleSettings,
    /// The app's theme, which colours the lines and the tab; the panel and settings window draw with it too.
    pub themes: Themes,
    /// Hidden with the hide shortcut or the tray.
    pub hidden: bool,
    panel: HWND,
    window: HWND,
    tab: HWND,
    surface: Surface,
    tab_surface: Surface,
    /// Lines no newer than this stay hidden: set when captions mode starts and when the lines fade out.
    shown_from: Mark,
    /// Each section's revision at the last frame, to tell when something was said.
    seen: Mark,
    /// Says how to get back to the transcript, until something is said or it fades.
    hint: bool,
    /// Both windows' opacity, lowered step by step as they fade.
    alpha: u8,
    shown: bool,
    /// The bottom-centre of the lines window, in screen pixels.
    anchor: POINT,
    /// The lines window's size in DIPs, and its monitor's DPI.
    size: (f32, f32),
    dpi: f32,
    /// The tab's top-left, in screen pixels.
    tab_at: POINT,
    tab_hovered: bool,
    /// While the tab is dragged: the pointer's offset from the anchor.
    dragging: Option<POINT>,
}

impl Subtitles {
    pub fn new(panel: HWND, settings: SubtitleSettings, themes: Themes) -> Result<Self> {
        let window = create_window(panel, WS_EX_TRANSPARENT, None)?;
        let tab = create_window(panel, WINDOW_EX_STYLE(0), Some(window))?;
        Ok(Self {
            settings,
            themes,
            hidden: false,
            panel,
            window,
            tab,
            surface: Surface::default(),
            tab_surface: Surface::default(),
            shown_from: Mark::default(),
            seen: Mark::default(),
            hint: false,
            alpha: u8::MAX,
            shown: false,
            anchor: POINT::default(),
            size: (0.0, 0.0),
            dpi: 96.0,
            tab_at: POINT::default(),
            tab_hovered: false,
            dragging: None,
        })
    }

    /// Switches captions mode on, showing only what is said from now on, after a hint on how to leave.
    pub fn start(&mut self, captions: &Captions, renderer: &Renderer) -> Result<()> {
        self.settings.on = true;
        self.hidden = false;
        self.shown_from = Mark::now(captions);
        self.seen = self.shown_from.clone();
        self.hint = true;
        self.wake();
        self.place(renderer)
    }

    pub fn stop(&mut self) {
        self.settings.on = false;
        self.end_drag();
        self.show(false);
        self.kill_timers();
    }

    /// Hiding keeps captions mode on; showing again says how to leave it.
    pub fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
        if hidden {
            self.end_drag();
            self.show(false);
            self.kill_timers();
        } else {
            self.hint = true;
            self.wake();
        }
    }

    /// Draws the newest lines; anything said since the last frame keeps them up for another quiet spell.
    pub fn draw(&mut self, renderer: &mut Renderer, captions: &Captions, hint: &str) -> Result<()> {
        if self.hidden {
            return Ok(());
        }
        let now = Mark::now(captions);
        if now != self.seen {
            self.seen = now;
            self.wake();
        }
        let newest = newest(captions, &self.shown_from, self.settings.rows as usize);
        self.hint &= newest.is_empty();
        let mut lines: Vec<SubtitleLine> = newest
            .iter()
            .map(|&(section, caption)| SubtitleLine {
                text: &caption.text,
                speaker: if section == YOU_SECTION { Speaker::You } else { Speaker::App(captions.sections[section].exe_path.as_deref()) },
            })
            .collect();
        if self.hint {
            lines.push(SubtitleLine { text: hint, speaker: Speaker::Kiku });
        }
        if lines.is_empty() {
            self.show(false);
            return Ok(());
        }
        let look = self.settings.look(&self.themes);
        let opacity = self.settings.opacity;
        let scene = SubtitleScene { lines: &lines, look: &look, opacity, themes: self.themes, rows: self.settings.rows as usize, width: self.size.0, height: self.size.1, dpi: self.dpi };
        let (origin, scale) = (self.origin(), self.dpi / 96.0);
        let (frame, block) = renderer.render_subtitles(&mut self.surface, &scene)?;
        frame.present(self.window, origin, self.alpha)?;
        let tab_at = POINT {
            x: origin.x + ((block.left - TAB_GAP - DRAG_TAB.0) * scale).round() as i32,
            y: origin.y + (((block.top + block.bottom - DRAG_TAB.1) / 2.0) * scale).round() as i32,
        };
        if tab_at.x != self.tab_at.x || tab_at.y != self.tab_at.y {
            self.tab_at = tab_at;
            unsafe { SetWindowPos(self.tab, None, tab_at.x, tab_at.y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)? };
        }
        self.draw_tab(renderer)?;
        self.show(true);
        Ok(())
    }

    /// Steps the fade once the lines have been quiet long enough; at the end they are gone for good.
    /// Without auto-hide only the hint goes. Returns true when there is a new frame to draw.
    pub fn on_timer(&mut self, timer: usize, captions: &Captions) -> Result<bool> {
        match timer {
            QUIET_TIMER => {
                unsafe { KillTimer(Some(self.panel), QUIET_TIMER).ok() }.unwrap_or_default();
                if !self.settings.auto_hide {
                    return Ok(std::mem::take(&mut self.hint));
                }
                // Pointing at the tab holds them up.
                if self.shown && !self.tab_hovered && self.dragging.is_none() {
                    unsafe { SetTimer(Some(self.panel), FADE_TIMER, FADE_STEP_MS, None) };
                }
            }
            FADE_TIMER => {
                self.alpha = self.alpha.saturating_sub(u8::MAX / FADE_STEPS + 1);
                if self.alpha > 0 {
                    return self.present_alpha().map(|()| false);
                }
                self.kill_timers();
                self.show(false);
                self.shown_from = Mark::now(captions);
                self.seen = self.shown_from.clone();
                self.hint = false;
                self.alpha = u8::MAX;
            }
            _ => {}
        }
        Ok(false)
    }

    /// Handles a message from the tab or the lines window.
    pub fn on_message(&mut self, message: u32, renderer: &mut Renderer) -> Result<TabEvent> {
        match message {
            WM_MOUSEMOVE => {
                if !self.tab_hovered {
                    self.tab_hovered = true;
                    let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.tab, dwHoverTime: 0 };
                    unsafe { TrackMouseEvent(&mut track)? };
                    self.wake();
                    self.present_alpha()?;
                    self.draw_tab(renderer)?;
                }
                if let Some(offset) = self.dragging {
                    let cursor = cursor();
                    self.move_to(POINT { x: cursor.x - offset.x, y: cursor.y - offset.y })?;
                }
            }
            WM_MOUSELEAVE => {
                self.tab_hovered = false;
                self.draw_tab(renderer)?;
                if self.dragging.is_none() {
                    self.wake();
                }
            }
            WM_LBUTTONDOWN => {
                unsafe { SetCapture(self.tab) };
                let cursor = cursor();
                self.dragging = Some(POINT { x: cursor.x - self.anchor.x, y: cursor.y - self.anchor.y });
            }
            WM_LBUTTONUP if self.dragging.is_some() => {
                self.end_drag();
                self.settings.anchor = Some((self.anchor.x, self.anchor.y));
                // Dropped on another screen, the lines take that screen's width and scale.
                self.place(renderer)?;
                self.wake();
                return Ok(TabEvent::Moved);
            }
            WM_LBUTTONDBLCLK => return Ok(TabEvent::ShowPanel),
            WM_DPICHANGED => self.place(renderer)?,
            _ => {}
        }
        Ok(TabEvent::Nothing)
    }

    /// Sizes and moves the lines window: two-thirds of its screen wide and tall enough for the rows,
    /// where it was dragged to, or bottom-centre of the panel's screen if that spot is on no screen.
    pub fn place(&mut self, renderer: &Renderer) -> Result<()> {
        let on_a_screen = |point: &POINT| !unsafe { MonitorFromPoint(*point, MONITOR_DEFAULTTONULL) }.is_invalid();
        let saved = self.settings.anchor.map(|(x, y)| POINT { x, y }).filter(on_a_screen);
        self.anchor = saved.unwrap_or_else(|| default_anchor(self.panel));
        let monitor = unsafe { MonitorFromPoint(self.anchor, MONITOR_DEFAULTTONEAREST) };
        let (mut dpi, mut dpi_y) = (96, 96);
        unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y)? };
        self.dpi = dpi as f32;
        let work = monitor_info(monitor).rcWork;
        let width = (work.right - work.left) as f32 * SCREEN_SHARE / (self.dpi / 96.0);
        self.size = (width, renderer.subtitle_height(&self.settings.look(&self.themes), self.settings.rows as usize)?);
        let (origin, (width, height)) = (self.origin(), self.pixels());
        unsafe { SetWindowPos(self.window, None, origin.x, origin.y, width, height, SWP_NOZORDER | SWP_NOACTIVATE) }
    }

    fn draw_tab(&mut self, renderer: &mut Renderer) -> Result<()> {
        renderer.render_drag_tab(&mut self.tab_surface, self.tab_hovered, &self.settings.look(&self.themes), &self.themes, self.dpi)?.present(self.tab, self.tab_at, self.alpha)
    }

    /// Pushes both frames again at the current opacity, with no drawing.
    fn present_alpha(&mut self) -> Result<()> {
        let (origin, alpha) = (self.origin(), self.alpha);
        if let Some(frame) = self.surface.frame() {
            frame.present(self.window, origin, alpha)?;
        }
        if let Some(frame) = self.tab_surface.frame() {
            frame.present(self.tab, self.tab_at, alpha)?;
        }
        Ok(())
    }

    /// Back to full opacity, fading again after a quiet spell.
    fn wake(&mut self) {
        self.alpha = u8::MAX;
        unsafe {
            let _ = KillTimer(Some(self.panel), FADE_TIMER);
            SetTimer(Some(self.panel), QUIET_TIMER, QUIET_MS, None);
        }
    }

    fn kill_timers(&self) {
        for timer in [QUIET_TIMER, FADE_TIMER] {
            let _ = unsafe { KillTimer(Some(self.panel), timer) };
        }
    }

    fn end_drag(&mut self) {
        if self.dragging.take().is_some() {
            unsafe { ReleaseCapture().ok() }.unwrap_or_default();
        }
    }

    /// Moves both windows so the lines' bottom-centre is at `anchor`.
    fn move_to(&mut self, anchor: POINT) -> Result<()> {
        let (dx, dy) = (anchor.x - self.anchor.x, anchor.y - self.anchor.y);
        self.anchor = anchor;
        self.tab_at = POINT { x: self.tab_at.x + dx, y: self.tab_at.y + dy };
        let origin = self.origin();
        unsafe {
            SetWindowPos(self.window, None, origin.x, origin.y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)?;
            SetWindowPos(self.tab, None, self.tab_at.x, self.tab_at.y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)
        }
    }

    fn show(&mut self, shown: bool) {
        if self.shown != shown {
            self.shown = shown;
            for window in [self.window, self.tab] {
                let _ = unsafe { ShowWindow(window, if shown { SW_SHOWNOACTIVATE } else { SW_HIDE }) };
            }
        }
    }

    /// The lines window's size in pixels, as its frames are drawn.
    fn pixels(&self) -> (i32, i32) {
        let scale = self.dpi / 96.0;
        ((self.size.0 * scale).round() as i32, (self.size.1 * scale).round() as i32)
    }

    /// The lines window's top-left in screen pixels.
    fn origin(&self) -> POINT {
        let (width, height) = self.pixels();
        POINT { x: self.anchor.x - width / 2, y: self.anchor.y - height }
    }
}

/// Asks for a colour with Windows' colour picker, starting at `rgb` (`0xRRGGBB`); `None` if cancelled.
pub fn pick_colour(owner: HWND, rgb: u32) -> Option<u32> {
    let mut custom = [COLORREF(0xffffff); 16];
    let mut choice = CHOOSECOLORW {
        lStructSize: size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: COLORREF(swap_red_blue(rgb)),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    unsafe { ChooseColorW(&mut choice) }.as_bool().then(|| swap_red_blue(choice.rgbResult.0))
}

/// Windows colours are `0x00BBGGRR`.
fn swap_red_blue(colour: u32) -> u32 {
    (colour & 0xff) << 16 | (colour & 0xff00) | (colour >> 16) & 0xff
}

/// Bottom-centre of the panel's screen, a little above the taskbar.
fn default_anchor(panel: HWND) -> POINT {
    let info = monitor_info(unsafe { MonitorFromWindow(panel, MONITOR_DEFAULTTONEAREST) });
    let (work, screen) = (info.rcWork, info.rcMonitor);
    POINT { x: (work.left + work.right) / 2, y: work.bottom - ((screen.bottom - screen.top) as f32 * BOTTOM_LIFT) as i32 }
}

fn monitor_info(monitor: HMONITOR) -> MONITORINFO {
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = unsafe { GetMonitorInfoW(monitor, &mut info) };
    info
}

fn cursor() -> POINT {
    let mut point = POINT::default();
    let _ = unsafe { GetCursorPos(&mut point) };
    point
}

/// A hidden layered window that stays on top and never takes focus; `owner` keeps the tab above the lines.
fn create_window(panel: HWND, style: WINDOW_EX_STYLE, owner: Option<HWND>) -> Result<HWND> {
    let instance = unsafe { GetModuleHandleW(None)? };
    let class = WNDCLASSW {
        style: CS_DBLCLKS,
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        lpszClassName: w!("KikuSubtitles"),
        ..Default::default()
    };
    // Registering again fails harmlessly.
    register_class(&class)?;
    let window = unsafe {
        CreateWindowExW(
            style | WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("KikuSubtitles"),
            w!("Kiku captions"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            owner,
            None,
            Some(instance.into()),
            None,
        )?
    };
    unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, panel.0 as isize) };
    Ok(window)
}

/// Passes the mouse and DPI messages on to the panel, which keeps captions mode's state.
extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_MOUSEMOVE | WM_MOUSELEAVE | WM_DPICHANGED => {
            let panel = HWND(unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as _);
            unsafe { SendMessageW(panel, WM_SUBTITLES, Some(WPARAM(message as usize)), Some(lparam)) }
        }
        WM_SETCURSOR => {
            unsafe { SetCursor(LoadCursorW(None, IDC_SIZEALL).ok()) };
            LRESULT(1)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::swap_red_blue;

    #[test]
    fn windows_colours_swap_red_and_blue() {
        assert_eq!(swap_red_blue(0x112233), 0x332211);
        assert_eq!(swap_red_blue(swap_red_blue(0xc2e2f6)), 0xc2e2f6);
    }
}
