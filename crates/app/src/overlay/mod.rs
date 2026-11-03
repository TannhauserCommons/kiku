//! The always-on-top caption panel and its tray icon.
//!
//! The panel is a normal clickable window that can be locked to let clicks pass through.
//! The captions key (Ctrl+Win by default, changeable in settings): hold to dictate into the focused app;
//! with V paste what you said (again to reach further back), Z undo that, S stop/start, C lock/unlock,
//! H hide/show, Q quit, B save a miss, T switch between the panel and captions mode.
//! Each app's header folds it (▸/▾), and double-clicked shows it alone; ✕ hides it from the panel and
//! ⊘ blocks it from being captured.

pub mod app_icon;
mod clipboard;
mod field;
mod finder;
mod folds;
mod icons;
mod keys;
mod selection;
mod share;
mod render;
#[cfg(test)]
mod render_bench;
mod rows;
mod scene;
mod settings;
mod subtitles;
mod tray;
pub mod usage;
mod view;

pub use keys::focused_app;

use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    sync::{atomic::Ordering, mpsc::Sender, Arc, Mutex},
    time::{Duration, Instant},
};

use windows::{
    core::{w, Result, HSTRING, PCWSTR},
    Win32::{
        Foundation::{GetLastError, D2DERR_RECREATE_TARGET, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::ScreenToClient,
        System::{
            Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED},
            DataExchange::GetClipboardSequenceNumber,
            LibraryLoader::GetModuleHandleW,
            Threading::CreateMutexW,
        },
        UI::{
            Shell::{FileOpenDialog, IFileOpenDialog, ShellExecuteW, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH},
            HiDpi::{GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
            Input::KeyboardAndMouse::{
                GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, VIRTUAL_KEY,
                VK_CONTROL, VK_ESCAPE, VK_RETURN,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, RegisterWindowMessageW, GetMessageW, GetWindowLongPtrW,
                GetWindowRect, IsWindowVisible, KillTimer, PostMessageW, PostQuitMessage, SetForegroundWindow, SetTimer,
                SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW, TranslateMessage,
                GWLP_USERDATA, GWL_EXSTYLE, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, MINMAXINFO, MSG,
                SPI_GETWORKAREA, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOWNORMAL, SW_SHOWNOACTIVATE,
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WM_APP, WM_DESTROY, WM_DPICHANGED, WM_GETMINMAXINFO,
                WA_INACTIVE, WM_ACTIVATE, WM_CHAR, WM_CLOSE, WM_ENDSESSION, WM_DISPLAYCHANGE, WM_KEYDOWN, WM_LBUTTONDBLCLK, WNDPROC, CS_DBLCLKS, WM_NCMOUSEMOVE, WM_SETCURSOR, IDC_ARROW, IDC_HAND, IDC_IBEAM, IDC_SIZEALL, GetCursorPos, LoadCursorW, SetCursor, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_MOUSEWHEEL, WM_NCHITTEST, WM_RBUTTONUP,
                WM_SIZE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    },
};

use kiku_core::{
    captions::{Caption, Captions, YOU_SECTION},
    controls::{Controls, Input, SaveRequest, THRESHOLD_MIN_DB},
    keybinds::{Command, Key, Keybinds},
    log,
    catalog::{self, Choice},
    download::{self, Downloads, Target as Download},
    hardware::Hardware,
    imports,
    settings::{BlockAnswer, Opacity, Settings, MIN_TEXT_OPACITY, MIN_WINDOW_OPACITY},
    subtitles::SubtitleSettings,
    theme::Themes,
    transcripts,
    update::{self, Release},
    words::{Output, WordSettings, ASSISTANT_INSTRUCTIONS},
};
use app_icon::register_class;
use finder::{example_sql, line_text, table_text, Reply, Request, Searcher, TextBox};
use folds::auto_fold_after;
use render::{fraction_at, threshold_at, usage_text, Meter, Panel, Renderer, Surface, HANDLE_STRIP, TITLE_HEIGHT};
use settings::SettingsWindow;
use subtitles::{Subtitles, TabEvent, WM_SUBTITLES};
use tray::{MenuChoice, MenuState, Tray, WM_TRAY};
use usage::{Usage, UsageMeter};
use selection::{Selection, TextPoint};
use view::{
    Action, BlockPrompt, Corner, Dropdown, ImportStatus, LineRef, Notice, Rebind, Screen, Scroll, ScrollArea, SetupStep, Slider, Tab, Target,
    UpdateCheck, View,
};

/// Custom messages: `WM_APP + 2` is the tray's, `+ 3` the captions key's and `+ 6` captions mode's windows'.
const WM_REFRESH: u32 = WM_APP + 1;
/// Dictated text is waiting to be typed.
const WM_TYPE: u32 = WM_APP + 4;
/// Draws the frame asked for since the last one.
const WM_DRAW: u32 = WM_APP + 5;
/// The history thread has answers waiting.
const WM_HISTORY: u32 = WM_APP + 7;
/// Another start of Kiku asks the running one to show itself.
const WM_SHOW: u32 = WM_APP + 8;
/// An update's installer is running and waits for Kiku to quit.
const WM_QUIT_FOR_UPDATE: u32 = WM_APP + 9;
/// Defined in the `UI::Controls` bindings, which this module doesn't otherwise need.
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_NCMOUSELEAVE: u32 = 0x02A2;
const NOTICE_TIMER: usize = 1;
const NOTICE_MS: u32 = 3000;
/// Dictation starts once the captions key has been held this long, so quick Ctrl+Win shortcuts
/// (Ctrl+Win+D, Ctrl+Win+arrows) never start it.
const DICTATE_TIMER: usize = 3;
const DICTATE_AFTER_MS: u32 = 250;
/// Setting a shortcut keeps every key from other apps, so it gives up after this long.
const REBIND_TIMER: usize = 5;
const REBIND_MS: u32 = 10_000;
/// Pasting what you said again within this long reaches one line further back.
const GROW_WITHIN: Duration = Duration::from_secs(5);
/// Gives the clipboard back after a paste, once the app has had time to read it, even behind a long erase.
const CLIPBOARD_TIMER: usize = 6;
const CLIPBOARD_MS: u32 = 1000;
/// Refreshes the CPU/GPU figures in the footer.
const USAGE_TIMER: usize = 4;
const USAGE_MS: u32 = 2000;
/// Takes down a "Live" badge, or folds an app, once it has been quiet long enough.
const LIVE_TIMER: usize = 7;
/// Shows the hovered button's tooltip once the pointer has rested on it this long.
const TIP_TIMER: usize = 10;
const TIP_MS: u32 = 400;
const ERROR_NOTICE_MS: u32 = 6000;
/// How close to an edge, in DIPs, the pointer resizes the panel.
const RESIZE_BORDER: f32 = 6.0;
/// Moves the level meters while the settings window shows them (~15 fps).
const METER_TIMER: usize = 2;
const METER_MS: u32 = 66;
/// How fast a meter falls back when the input goes quiet, per meter tick.
const METER_FALL_DB: f32 = 4.0;
const METER_INPUTS: [Input; 2] = [Input::Mic, Input::Computer];
const WHEEL_STEP: f32 = 48.0;
const SCREEN_MARGIN: i32 = 24;
/// Default and minimum panel sizes, in DIPs, not counting the drag handle strip.
const DEFAULT_SIZE: (f32, f32) = (560.0, 420.0);
const MIN_SIZE: (f32, f32) = (340.0, 200.0);

/// The text this app last typed into another app, so it can be undone or replaced by more.
#[derive(Clone, Copy)]
struct Typed {
    erase: keys::Erase,
    /// How many of your lines it holds, for growing a paste; 0 for dictation.
    lines: usize,
    at: Instant,
}

/// What was on the clipboard before a paste borrowed it.
struct Lent {
    contents: clipboard::Contents,
    /// The clipboard's sequence number once the paste's text was on it; a new one means something was copied since.
    sequence: u32,
}

/// Lets other threads ask the overlay to redraw.
#[derive(Clone, Copy)]
pub struct Handle(isize);

impl Handle {
    /// Schedules a redraw with the latest captions.
    pub fn refresh(&self) {
        unsafe { PostMessageW(Some(HWND(self.0 as _)), WM_REFRESH, WPARAM(0), LPARAM(0)).ok() };
    }

    /// Asks the overlay to type the dictated text queued in [`Controls`].
    pub fn type_queued(&self) {
        unsafe { PostMessageW(Some(HWND(self.0 as _)), WM_TYPE, WPARAM(0), LPARAM(0)).ok() };
    }

    fn history_answered(&self) {
        unsafe { PostMessageW(Some(HWND(self.0 as _)), WM_HISTORY, WPARAM(0), LPARAM(0)).ok() };
    }

    fn quit_for_update(&self) {
        unsafe { PostMessageW(Some(HWND(self.0 as _)), WM_QUIT_FOR_UPDATE, WPARAM(0), LPARAM(0)).ok() };
    }
}

/// What the panel starts with besides the shared state.
pub struct Setup {
    /// The shortcuts and how things look.
    pub settings: Settings,
    pub hardware: Hardware,
    /// No settings were saved yet: setup opens.
    pub first_run: bool,
}

/// Shows the overlay, or captions mode if it was on, and runs its message loop on this thread until the
/// user quits. `settings` gives the shortcuts and how things look; `wake_discovery` asks for a discovery
/// pass once an app is blocked or unblocked. `on_ready` is called once the window exists, with a handle
/// for requesting redraws.
pub fn run(
    captions: Arc<Mutex<Captions>>,
    controls: Arc<Controls>,
    setup: Setup,
    wake_discovery: Sender<()>,
    on_ready: impl FnOnce(Handle),) -> Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)?;
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let hwnd = create_window()?;
    // Without a tray icon (e.g. Explorer restarting) the panel still hides and returns via Ctrl+Win+H.
    let tray = Tray::add(hwnd).inspect_err(|error| log::write(format!("no tray icon: {error}"))).ok();
    let (settings, handle) = (setup.settings, Handle(hwnd.0 as isize));
    let mut window = Window {
        hwnd,
        captions,
        controls,
        dragging: None,
        hardware: setup.hardware,
        downloads: Downloads::start(move || handle.refresh()),
        import: Arc::default(),
        meter_db: [THRESHOLD_MIN_DB; 2],
        frame_pending: false,
        renderer: Renderer::new()?,
        panel_surface: Surface::default(),
        subtitles: Subtitles::new(hwnd, settings.subtitles, settings.themes)?,
        settings: SettingsWindow::new(hwnd, WNDPROC::Some(settings_proc))?,
        view: View::new(),
        searcher: Searcher::start(move || handle.history_answered()),
        tray,
        tracking_mouse: false,
        tracking_title_bar: false,
        typed: None,
        lent: None,
        keybinds: settings.keybinds,
        auto_fold: settings.auto_fold_minutes,
        block_answer: settings.block_answer,
        opacity: settings.opacity,
        locked_opacity: settings.locked_opacity,
        wake_discovery,
        captured: Vec::new(),
        captured_down: Vec::new(),
        selecting: false,
        _keys: None,
        usage_meter: UsageMeter::new(),
        usage: None,
        collapsed_from: None,
        restore_height: None,
        taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
    };
    window.view.has_tray = window.tray.is_some();
    if setup.first_run {
        window.start_setup();
    } else if download::fallback().is_none() && !download::runnable(window.controls.model().wanted()) {
        // Nothing to caption with: straight to where models are downloaded.
        window.settings.show_tab(Tab::Listening);
        window.settings.open(hwnd);
        window.watch_meters();
    }
    let in_subtitles = window.subtitles.settings.on;
    if in_subtitles {
        window.start_subtitles();
    }
    let shared = Box::new(Shared { window: RefCell::new(window), redraw_pending: Cell::new(false) });
    // Without the hook the buttons and tray menu still work.
    let hook_keybinds = shared.window.borrow().keybinds.clone();
    match keys::install(hwnd, hook_keybinds) {
        Ok(hook) => shared.window.borrow_mut()._keys = Some(hook),
        Err(error) => log::write(format!("the Ctrl+Win captions key is unavailable: {error}")),
    }
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(shared) as isize) };
    unsafe { SetTimer(Some(hwnd), USAGE_TIMER, USAGE_MS, None) };
    Handle(hwnd.0 as isize).refresh();
    if !in_subtitles {
        // The return value is the previous visibility, not an error.
        let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
    }
    on_ready(Handle(hwnd.0 as isize));

    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

struct Window {
    hwnd: HWND,
    captions: Arc<Mutex<Captions>>,
    controls: Arc<Controls>,
    /// The settings window's slider being dragged, if any.
    dragging: Option<Slider>,
    hardware: Hardware,
    downloads: Downloads,
    /// How importing a model folder goes; set from the thread trying it.
    import: Arc<Mutex<ImportStatus>>,
    /// What each meter shows: new levels arrive ~10 times a second, so it holds and falls gradually.
    meter_db: [f32; 2],
    /// A WM_DRAW is on its way.
    frame_pending: bool,
    renderer: Renderer,
    panel_surface: Surface,
    /// Captions mode, which shows instead of the panel while on.
    subtitles: Subtitles,
    settings: SettingsWindow,
    view: View,
    /// Runs the history screen's queries.
    searcher: Searcher,
    tray: Option<Tray>,
    tracking_mouse: bool,
    tracking_title_bar: bool,
    /// A drag over caption text is selecting it.
    selecting: bool,
    typed: Option<Typed>,
    /// The clipboard as it was before a paste, until it is given back.
    lent: Option<Lent>,
    keybinds: Keybinds,
    /// Minutes an app is quiet before the panel folds it; 0 never.
    auto_fold: u32,
    block_answer: BlockAnswer,
    /// The panel's opacity while unlocked, and while locked.
    opacity: Opacity,
    locked_opacity: Opacity,
    wake_discovery: Sender<()>,
    /// While setting the captions key: every key pressed so far, and the ones still down.
    captured: Vec<Key>,
    captured_down: Vec<u16>,
    /// The keyboard hook, removed with the window.
    _keys: Option<keys::Hook>,
    usage_meter: UsageMeter,
    usage: Option<Usage>,
    /// The panel's height in pixels before it collapsed; `Some` while collapsed.
    collapsed_from: Option<i32>,
    /// The height to grow back to on the next redraw after expanding.
    restore_height: Option<i32>,
    /// Sent by Explorer when it restarts, which drops every tray icon.
    taskbar_created: u32,
}

impl Drop for Window {
    // Quitting right after a paste still gives the clipboard back.
    fn drop(&mut self) {
        self.give_back_clipboard();
    }
}

/// The window state as stored on the window, plus redraws requested while it was busy.
struct Shared {
    window: RefCell<Window>,
    redraw_pending: Cell<bool>,
}

impl Window {
    fn handle(&mut self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match message {
            WM_REFRESH => {
                if let Some(notice) = self.controls.take_notice() {
                    if !self.is_visible() || self.subtitles.settings.on {
                        self.tray.iter().for_each(|tray| tray.balloon(&notice));
                    }
                    self.notify(notice, true);
                }
                if let Some(release) = self.controls.take_update() {
                    self.offer_update(release);
                }
                if let Some(found) = self.controls.take_checked() {
                    self.settings.view.update_check = match found {
                        Ok(true) => None,
                        Ok(false) => Some(UpdateCheck::Latest),
                        Err(why) => Some(UpdateCheck::Failed(why)),
                    };
                }
                self.redraw();
            }
            WM_QUIT_FOR_UPDATE => {
                self.downloads.pause_all();
                self.quit();
            }
            WM_SIZE => self.redraw(),
            // Logging off or shutting down: no question; the queue is kept and resumes next time.
            WM_ENDSESSION if wparam.0 != 0 => self.downloads.pause_all(),
            WM_DRAW => self.draw(),
            WM_DPICHANGED => {
                move_to_suggested(self.hwnd, lparam);
                self.redraw();
            }
            keys::WM_CAPTIONS_KEY => self.on_captions_key(keys::KeyEvent::from_message(wparam, lparam)),
            WM_TYPE => self.type_dictation(),
            WM_HISTORY => self.receive_history(),
            WM_KEYDOWN if self.view.finder.typing.is_some() => self.on_typing_key(VIRTUAL_KEY(wparam.0 as u16)),
            WM_CHAR => {
                if let Some(field) = self.typing_field() {
                    field.type_unit(wparam.0 as u16);
                    self.after_typing();
                }
            }
            WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => self.stop_typing(),
            WM_NCHITTEST => return Some(self.hit_test(lparam)),
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => match self.target_at_client(lparam) {
                Some(Target::Header(index)) if message == WM_LBUTTONDBLCLK => self.solo(index),
                Some(Target::Click(action)) => self.press(Some(action)),
                Some(Target::Card) => {}
                _ => self.start_selecting(lparam),
            },
            WM_MOUSEMOVE if self.selecting => self.extend_selection(lparam),
            WM_MOUSEMOVE => self.on_mouse_move(lparam),
            // The title bar is a non-client area, so it gets its own mouse messages.
            WM_NCMOUSEMOVE => self.on_title_bar_hover(wparam.0 as u32 == HTCAPTION),
            WM_NCMOUSELEAVE => {
                self.tracking_title_bar = false;
                self.on_title_bar_hover(false);
            }
            WM_SETCURSOR if (lparam.0 & 0xFFFF) as u32 == HTCAPTION => {
                unsafe { SetCursor(LoadCursorW(None, IDC_SIZEALL).ok()) };
                return Some(LRESULT(1));
            }
            WM_SETCURSOR if (lparam.0 & 0xFFFF) as u32 == HTCLIENT => {
                unsafe { SetCursor(LoadCursorW(None, self.client_cursor()).ok()) };
                return Some(LRESULT(1));
            }
            WM_MOUSELEAVE => {
                self.tracking_mouse = false;
                self.press(None);
                self.set_hovered(None, None, None);
            }
            WM_LBUTTONUP if self.selecting => self.finish_selecting(),
            WM_LBUTTONUP => {
                self.press(None);
                if let Some(Target::Click(action)) = self.target_at_client(lparam) {
                    self.perform(action);
                }
            }
            WM_MOUSEWHEEL => self.on_wheel(wparam, lparam),
            WM_GETMINMAXINFO => self.limit_size(lparam),
            WM_TIMER if wparam.0 == METER_TIMER => self.update_meters(),
            WM_TIMER if wparam.0 == REBIND_TIMER => self.finish_rebinding(None),
            WM_TIMER if wparam.0 == CLIPBOARD_TIMER => self.give_back_clipboard(),
            WM_TIMER if wparam.0 == DICTATE_TIMER => {
                unsafe { KillTimer(Some(self.hwnd), DICTATE_TIMER).ok() }.unwrap_or_default();
                self.controls.dictating.store(true, Ordering::Relaxed);
                self.redraw();
            }
            // Sampled only while the panel's footer shows it.
            WM_TIMER if wparam.0 == USAGE_TIMER && self.is_visible() && !self.view.collapsed => {
                self.usage_meter.watch_helper(self.controls.helper_process());
                let usage = self.usage_meter.sample();
                if self.usage.map(usage_text) != Some(usage_text(usage)) {
                    self.redraw();
                }
                self.usage = Some(usage);
            }
            WM_TIMER if wparam.0 == LIVE_TIMER => {
                unsafe { KillTimer(Some(self.hwnd), LIVE_TIMER).ok() }.unwrap_or_default();
                self.redraw();
            }
            WM_TIMER if wparam.0 == TIP_TIMER => {
                unsafe { KillTimer(Some(self.hwnd), TIP_TIMER).ok() }.unwrap_or_default();
                self.view.tip = self.view.hovered.is_some();
                self.settings.view.tip = self.settings.view.hovered.is_some();
                self.redraw();
            }
            WM_TIMER if wparam.0 == NOTICE_TIMER => {
                unsafe { KillTimer(Some(self.hwnd), NOTICE_TIMER).ok() };
                (self.view.notice, self.settings.view.notice) = (None, None);
                self.redraw();
            }
            WM_TIMER if matches!(wparam.0, subtitles::QUIET_TIMER | subtitles::FADE_TIMER) => {
                let captions = self.captions.lock().unwrap();
                match self.subtitles.on_timer(wparam.0, &captions) {
                    Ok(true) => {
                        drop(captions);
                        self.redraw();
                    }
                    Ok(false) => {}
                    Err(error) => log::write(format!("captions mode: {error}")),
                }
            }
            WM_SUBTITLES => self.on_subtitle_message(wparam.0 as u32),
            WM_DISPLAYCHANGE if self.subtitles.settings.on => {
                self.place_subtitles();
                self.redraw();
            }
            WM_TRAY => self.on_tray(lparam.0 as u32),
            WM_SHOW => self.set_display_visible(true),
            message if message == self.taskbar_created => {
                self.tray = Tray::add(self.hwnd).inspect_err(|error| log::write(format!("no tray icon: {error}"))).ok();
                self.view.has_tray = self.tray.is_some();
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }

    fn perform(&mut self, action: Action) {
        // Any click outside an open drop-down closes it, and anything but a second press keeps a session.
        if !matches!(action, Action::ToggleDropdown(_)) {
            (self.settings.view.open_dropdown, self.view.open_dropdown) = (None, None);
        }
        if !matches!(action, Action::DeleteSession(_)) {
            self.view.finder.deleting = None;
        }
        if !matches!(action, Action::TypeTry | Action::ClearTry) {
            self.settings.view.typing_try = false;
        }
        // Only a second press of Delete deletes.
        let confirming = std::mem::take(&mut self.settings.view.confirm_delete);
        match action {
            Action::SaveAll => self.save(SaveRequest::All, "every app"),
            Action::SaveSection(index) => {
                let label = self.captions.lock().unwrap().sections[index].label.clone();
                self.save(SaveRequest::Section(index), &label);
            }
            Action::CopySection(index) => self.copy_section(index),
            Action::ToggleFold(index) => {
                let captions = self.captions.lock().unwrap();
                let folded = self.view.folds.folded(index, &captions.sections[index], auto_fold_after(self.auto_fold), Instant::now());
                drop(captions);
                self.view.folds.toggle(index, folded);
            }
            Action::Hide(index) => {
                self.view.folds.hide(index);
                self.view.hovered_header = None;
            }
            Action::ShowHidden => self.view.folds.show_hidden(),
            Action::AutoFold(minutes) => {
                self.auto_fold = minutes;
                self.save_settings();
            }
            Action::Block(index) if index != YOU_SECTION => match self.block_answer {
                BlockAnswer::Ask => self.view.blocking = Some(BlockPrompt { section: index, dont_ask: false }),
                answer => self.block(index, answer == BlockAnswer::Always),
            },
            Action::Block(_) => {}
            Action::BlockFor(always) => {
                if let Some(prompt) = self.view.blocking.take() {
                    if prompt.dont_ask {
                        self.block_answer = if always { BlockAnswer::Always } else { BlockAnswer::Session };
                    }
                    self.block(prompt.section, always);
                }
            }
            Action::CancelBlock => self.view.blocking = None,
            Action::DontAskToBlock => {
                if let Some(prompt) = &mut self.view.blocking {
                    prompt.dont_ask = !prompt.dont_ask;
                }
            }
            Action::Unblock(index) => {
                if let Some(app) = self.controls.blocked().get(index) {
                    self.controls.unblock(&app.name);
                    self.save_settings();
                    let _ = self.wake_discovery.send(());
                    self.notify(format!("Captioning {} again", app.name), false);
                }
            }
            Action::Pin(index) if index != YOU_SECTION => {
                let mut captions = self.captions.lock().unwrap();
                let (label, pinned) = (captions.sections[index].label.clone(), captions.is_pinned(index));
                captions.pin(&label, !pinned);
                drop(captions);
                self.save_settings();
            }
            Action::Pin(_) => {}
            Action::Unpin(index) => {
                let mut captions = self.captions.lock().unwrap();
                if let Some(label) = captions.pinned.get(index).cloned() {
                    captions.pin(&label, false);
                }
                drop(captions);
                self.save_settings();
            }
            Action::SetBlockAnswer(answer) => {
                self.block_answer = answer;
                self.save_settings();
            }
            Action::WordScope { transforms, scope } => {
                let mut settings = self.controls.words.settings();
                *if transforms { &mut settings.transforms } else { &mut settings.rules } = scope;
                self.controls.words.set_settings(settings);
                self.save_settings();
            }
            Action::WordHotwords(on) => {
                self.controls.words.set_settings(WordSettings { hotwords: on, ..self.controls.words.settings() });
                self.save_settings();
            }
            Action::RemoveWordEntry(index) => {
                let words = self.controls.words.words();
                if let Some(Err(error)) = words.entries().get(index).map(|entry| self.controls.words.remove(entry)) {
                    self.notify(format!("Couldn't change words.txt: {}", plain_error(&error)), true);
                }
            }
            Action::OpenWords => self.open_words(),
            Action::CopyWordInstructions => self.copy(ASSISTANT_INSTRUCTIONS, "Copied: paste it to your assistant, then say what you want".into()),
            Action::TypeTry => self.settings.view.typing_try = true,
            Action::ClearTry => self.settings.view.try_words.clear(),
            Action::CopyLine(line) => {
                if let Some(text) = self.caption_text(line) {
                    self.copy(&text, "Copied the line".into());
                }
            }
            Action::ShareLine(line) => {
                let label = self.captions.lock().unwrap().sections[line.section].label.clone();
                if let Some(text) = self.caption_text(line) {
                    self.share(&format!("a line from {label}"), &text);
                }
            }
            Action::ShareSection(index) => {
                let (label, text) = {
                    let captions = self.captions.lock().unwrap();
                    let section = &captions.sections[index];
                    (section.label.clone(), transcripts::transcript_text(std::slice::from_ref(section)))
                };
                self.share(&format!("the {label} transcript"), &text);
            }
            Action::ShareAll => {
                let text = {
                    let captions = self.captions.lock().unwrap();
                    let sections: Vec<_> = captions.visible().map(|(_, s)| s.clone()).collect();
                    transcripts::transcript_text(&sections)
                };
                self.share("everything said so far (every app, and me as You)", &text);
            }
            Action::ShareSelection => {
                if let Some(selection) = self.view.selection {
                    let text = selection.text(&self.captions.lock().unwrap(), &self.view.regions.sections);
                    self.share("a selection", &text);
                }
            }
            // Pressed on the list, History goes back to the captions; from a past session, back to the list.
            Action::ShowSaved if matches!(self.view.screen, Screen::SavedList) => self.leave_history(),
            // Back from a session to what the list showed, scrolled as it was.
            Action::ShowSaved if matches!(self.view.screen, Screen::SavedFile { .. }) => self.view.screen = Screen::SavedList,
            Action::ShowSaved => {
                self.view.screen = Screen::SavedList;
                self.searcher.ask(Request::Sessions);
                self.find();
            }
            Action::ShowSettings if self.settings.is_open() => self.close_settings(),
            Action::ShowSettings => {
                self.settings.open(self.hwnd);
                self.watch_meters();
            }
            Action::CloseSettings if self.settings.view.setup.is_some() => self.finish_setup(),
            Action::CloseSettings => self.close_settings(),
            Action::ShowShortcuts => {
                if !self.settings.is_open() {
                    self.settings.open(self.hwnd);
                }
                return self.perform(Action::ShowTab(Tab::Shortcuts));
            }
            Action::ShowTab(tab) => {
                if self.settings.view.rebinding.is_some() {
                    self.finish_rebinding(None);
                }
                self.settings.show_tab(tab);
                self.controls.words.refresh();
                self.watch_meters();
            }
            Action::ToggleStop => self.toggle_stop(),
            Action::Rebind(rebind) => self.start_rebinding(rebind),
            Action::ToggleDropdown(dropdown) => {
                let view = &mut self.settings.view;
                if dropdown == Dropdown::Font && view.fonts.is_empty() {
                    match self.renderer.font_families() {
                        Ok(fonts) => view.fonts = fonts,
                        Err(error) => self.notify(format!("Couldn't list the fonts: {error}"), true),
                    }
                }
                // History's drop-down is the panel's; the rest are the settings window's.
                let view = if dropdown == Dropdown::Who { &mut self.view } else { &mut self.settings.view };
                if view.open_dropdown == Some(dropdown) {
                    view.open_dropdown = None;
                } else if dropdown == Dropdown::Font {
                    view.open_font_list(&self.subtitles.settings.font);
                } else {
                    view.scrolls.remove(&ScrollArea::Dropdown(dropdown));
                    view.open_dropdown = Some(dropdown);
                }
            }
            Action::SetAutoSave(auto) => {
                self.controls.auto_save.store(auto, Ordering::Relaxed);
                self.save_settings();
            }
            Action::SaveMicOnly(mic_only) => {
                self.controls.save_mic_only.store(mic_only, Ordering::Relaxed);
                self.save_settings();
            }
            Action::KeepDays(days) => {
                // The decoder deletes lines older than this at its next poll.
                self.controls.keep_days.store(days, Ordering::Relaxed);
                self.save_settings();
            }
            Action::ResetKeys => {
                self.keybinds = Keybinds::default();
                keys::set_keybinds(self.keybinds.clone());
                self.save_settings();
                self.notify(format!("Shortcuts reset: hold {} to dictate", self.keybinds.hold_name()), false);
            }
            Action::PickModel(model) => {
                self.controls.request_model(Choice { model, ..self.controls.model().wanted() });
                self.save_settings();
            }
            Action::PickDevice(device) => {
                self.controls.request_model(Choice { device, ..self.controls.model().wanted() });
                self.save_settings();
            }
            // The GPU helper holds the pack's DLLs open while a model runs on the GPU.
            Action::Download(Download::Component(_)) | Action::Delete(Download::Component(_)) if self.controls.helper_process().is_some() => {
                self.notify("Switch the model to the CPU first: the GPU pack is in use".into(), true);
            }
            Action::Download(target) => self.downloads.download(target),
            Action::PauseDownload(target) => self.downloads.pause(target),
            Action::CancelDownload(target) => self.downloads.cancel(target),
            Action::Delete(target @ Download::Model(model, _)) if model.imported() => self.delete(target),
            Action::Delete(target) if confirming == Some(target) => self.delete(target),
            Action::Delete(target) => self.settings.view.confirm_delete = Some(target),
            Action::Import => self.import_model(),
            Action::OpenSetup => self.start_setup(),
            Action::UseRecommended => {
                let recommended = self.hardware.recommend();
                self.controls.request_model(Choice { model: recommended.model, device: recommended.variant.device });
                self.save_settings();
            }
            Action::SetupNext => self.next_setup_step(),
            Action::SetupBack => self.settings.view.setup = self.settings.view.setup.and_then(SetupStep::back).or(self.settings.view.setup),
            Action::SkipSetup => self.finish_setup(),
            Action::QuitAndPause => {
                self.downloads.pause_all();
                self.view.quit_prompt = false;
                return self.quit();
            }
            Action::KeepDownloading => self.view.quit_prompt = false,
            Action::InstallUpdate => self.install_update(),
            Action::UpdateLater => self.view.update = None,
            Action::CheckUpdates(on) => {
                self.controls.check_updates.store(on, Ordering::Relaxed);
                self.save_settings();
            }
            Action::CheckUpdatesNow => self.check_for_update(),
            Action::DictationOnly(on) => {
                self.controls.dictation_only.store(on, Ordering::Relaxed);
                self.save_settings();
                let _ = self.wake_discovery.send(());
            }
            Action::ShowSubtitles(on) => self.set_subtitles(on),
            Action::SubtitleRows(rows) => self.restyle(|settings| settings.rows = rows),
            Action::SetTheme(theme) => self.retheme(|themes| themes.choose(theme)),
            Action::SubtitleAutoHide(on) => self.restyle(|settings| settings.auto_hide = on),
            Action::SetColour(paint, rgb) => self.retheme(|themes| themes.edit(|colours| paint.set(colours, rgb))),
            Action::MoreColours(paint) => {
                let shown = paint.colour(self.subtitles.themes.colours());
                if let Some(rgb) = subtitles::pick_colour(self.settings.hwnd, shown) {
                    self.retheme(|themes| themes.edit(|colours| paint.set(colours, rgb)));
                }
            }
            Action::SubtitleFont(index) => {
                if let Some(font) = self.settings.view.fonts.get(index).cloned() {
                    self.restyle(|settings| settings.font = font);
                }
            }
            Action::SubtitleSize(size) => self.restyle(|settings| settings.size = size),
            Action::ShowLive => self.leave_history(),
            Action::OpenSaved(index) => {
                if let Some(session) = self.view.saved.get(index) {
                    self.searcher.ask(Request::Open { session: session.id, line: None });
                }
            }
            Action::DeleteSession(index) => {
                let Some(session) = self.view.saved.get(index).map(|session| session.id) else { return };
                let finder = &mut self.view.finder;
                match finder.deleting.take() {
                    Some(pressed) if pressed == session => self.searcher.ask(Request::DeleteSession(session)),
                    _ => finder.deleting = Some(session),
                }
            }
            Action::ShowSql(on) => {
                self.stop_typing();
                self.view.finder.sql_mode = on;
            }
            Action::Type(text_box) => self.start_typing(text_box),
            Action::ClearSearch => {
                self.view.finder.search.clear();
                self.find();
            }
            Action::ClearFontSearch => {
                self.settings.view.font_search.clear();
                self.settings.view.search_fonts();
            }
            Action::SetPeriod(period) => {
                self.view.finder.period = period;
                self.find();
            }
            Action::SetWho(index) => {
                let finder = &mut self.view.finder;
                if let Some(who) = finder.who_choices().into_iter().nth(index) {
                    finder.who = who;
                    self.find();
                }
            }
            Action::OpenResult(index) => {
                if let Some(line) = self.view.finder.found.lines.get(index) {
                    self.searcher.ask(Request::Open { session: line.session, line: Some(line.id) });
                }
            }
            Action::CopyResult(index) => {
                if let Some(text) = self.view.finder.found.lines.get(index).map(line_text) {
                    self.copy(&text, "Copied the line".into());
                }
            }
            Action::ShareResult(index) => {
                if let Some(line) = self.view.finder.found.lines.get(index) {
                    let (what, text) = (format!("a line {} said, from Kiku's history", line.speaker), line_text(line));
                    self.share(&what, &text);
                }
            }
            Action::CopyResults | Action::ShareResults => self.export_results(action == Action::ShareResults),
            Action::RunSql => self.run_sql(),
            Action::Example(index) => {
                self.view.finder.sql.set(&example_sql(index));
                self.run_sql();
            }
            Action::ToggleLock => self.toggle_lock(),
            // Without a tray icon a hidden panel could not be found again, so it collapses instead.
            Action::Minimize if self.tray.is_some() => self.set_visible(false),
            Action::Minimize => self.toggle_collapsed(),
            Action::Close => return self.quit(),
        }
        self.redraw();
    }

    fn close_settings(&mut self) {
        if self.settings.view.rebinding.is_some() {
            self.finish_rebinding(None);
        }
        self.settings.close();
        self.watch_meters();
    }

    /// Keeps the level meters moving with a timer while the settings window shows them.
    fn watch_meters(&mut self) {
        let shown = self.settings.is_open() && self.settings.view.tab == Tab::Listening;
        unsafe {
            if shown {
                SetTimer(Some(self.hwnd), METER_TIMER, METER_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), METER_TIMER);
            }
        }
    }

    /// The settings window's mouse, keyboard and placement messages.
    fn handle_settings(&mut self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match message {
            WM_NCHITTEST => return Some(self.settings.hit_test(point_from(lparam))),
            WM_SETCURSOR if (lparam.0 & 0xFFFF) as u32 == HTCLIENT => {
                let mut point = POINT::default();
                let _ = unsafe { GetCursorPos(&mut point) };
                let _ = unsafe { ScreenToClient(self.settings.hwnd, &mut point) };
                let cursor = if matches!(self.settings.target_at(point), Some(Target::Click(_))) { IDC_HAND } else { IDC_ARROW };
                unsafe { SetCursor(LoadCursorW(None, cursor).ok()) };
                return Some(LRESULT(1));
            }
            WM_LBUTTONDOWN => match self.settings.target_at(point_from(lparam)) {
                Some(Target::Slider(slider)) => {
                    self.dragging = Some(slider);
                    unsafe { SetCapture(self.settings.hwnd) };
                    self.drag_slider(lparam);
                }
                Some(Target::Click(action)) => {
                    self.settings.view.pressed = Some(action);
                    self.redraw();
                }
                // An open list's field and the space between its rows, or between a palette's colours.
                Some(Target::Card) => {}
                _ => {
                    (self.settings.view.open_dropdown, self.settings.view.typing_try) = (None, false);
                    self.redraw();
                }
            },
            WM_MOUSEMOVE if self.dragging.is_some() => self.drag_slider(lparam),
            WM_MOUSEMOVE => {
                self.settings.track_mouse();
                self.settings.view.pointer = self.settings.to_dips(point_from(lparam));
                let hovered = match self.settings.target_at(point_from(lparam)) {
                    Some(Target::Click(action)) => Some(action),
                    _ => None,
                };
                if self.settings.view.hovered != hovered {
                    (self.settings.view.hovered, self.settings.view.tip) = (hovered, false);
                    self.wait_for_tip(hovered);
                    self.redraw();
                }
            }
            WM_MOUSELEAVE => {
                self.settings.mouse_left();
                self.settings.view.pressed = None;
                self.redraw();
            }
            WM_LBUTTONUP if self.dragging.is_some() => {
                self.dragging = None;
                unsafe { ReleaseCapture().ok() }.unwrap_or_default();
                self.save_settings();
                // The panel showed a locked opacity while it was dragged.
                self.redraw();
            }
            WM_LBUTTONUP => {
                self.settings.view.pressed = None;
                self.redraw();
                if let Some(Target::Click(action)) = self.settings.target_at(point_from(lparam)) {
                    self.perform(action);
                }
            }
            WM_MOUSEWHEEL => {
                let mut point = point_from(lparam);
                let _ = unsafe { ScreenToClient(self.settings.hwnd, &mut point) };
                let (x, y) = self.settings.to_dips(point);
                if self.settings.view.scroll_at(x, y, -wheel_notches(wparam) * WHEEL_STEP) {
                    self.redraw();
                }
            }
            WM_DPICHANGED => {
                move_to_suggested(self.settings.hwnd, lparam);
                self.redraw();
            }
            WM_CHAR if self.settings.view.open_dropdown == Some(Dropdown::Font) => {
                let view = &mut self.settings.view;
                view.font_search.type_unit(wparam.0 as u16);
                view.search_fonts();
                self.redraw();
            }
            WM_KEYDOWN if self.settings.view.open_dropdown == Some(Dropdown::Font) => self.font_list_key(VIRTUAL_KEY(wparam.0 as u16)),
            WM_CHAR if self.settings.view.typing_try => {
                self.settings.view.try_words.type_unit(wparam.0 as u16);
                self.redraw();
            }
            WM_KEYDOWN if self.settings.view.typing_try => self.try_box_key(VIRTUAL_KEY(wparam.0 as u16)),
            WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE && self.settings.view.typing_try => {
                self.settings.view.typing_try = false;
                self.redraw();
            }
            WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => self.close_settings(),
            WM_CLOSE => self.close_settings(),
            _ => return None,
        }
        Some(LRESULT(0))
    }

    /// A key pressed while the font list is open: editing keys edit its search, Enter picks the first match,
    /// and Escape clears the search, or closes the list when there is none.
    fn font_list_key(&mut self, key: VIRTUAL_KEY) {
        let (ctrl, hwnd) = (ctrl_held(), self.hwnd);
        let view = &mut self.settings.view;
        let typed = view.font_search.text.clone();
        if !view.font_search.edit(key, ctrl, || clipboard::text(hwnd)) {
            match key {
                // With no query typed, the list shows the font in use, so Enter keeps it.
                VK_RETURN if typed.is_empty() => view.open_dropdown = None,
                VK_RETURN => match view.font_matches.first() {
                    Some(&font) => return self.perform(Action::SubtitleFont(font)),
                    None => return,
                },
                VK_ESCAPE if typed.is_empty() => view.open_dropdown = None,
                VK_ESCAPE => view.font_search.clear(),
                _ => return,
            }
        }
        if view.font_search.text != typed {
            view.search_fonts();
        }
        self.redraw();
    }

    /// A key pressed while typing in the Words tab's Try box: editing keys edit it, and Escape empties it, then leaves it.
    fn try_box_key(&mut self, key: VIRTUAL_KEY) {
        let (ctrl, hwnd) = (ctrl_held(), self.hwnd);
        let view = &mut self.settings.view;
        if !view.try_words.edit(key, ctrl, || clipboard::text(hwnd)) {
            match key {
                VK_ESCAPE if !view.try_words.text.is_empty() => view.try_words.clear(),
                VK_ESCAPE => view.typing_try = false,
                _ => return,
            }
        }
        self.redraw();
    }

    /// Opens `words.txt` in the user's editor, made with a short header if it is missing.
    fn open_words(&mut self) {
        let opened = self.controls.words.create().map(|path| unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(path.as_os_str()), None, None, SW_SHOWNORMAL) });
        match opened {
            // ShellExecute returns a value above 32 on success.
            Ok(instance) if instance.0 as isize > 32 => {}
            Ok(_) => self.notify("Couldn't open words.txt: no app opens text files".into(), true),
            Err(error) => self.notify(format!("Couldn't make words.txt: {}", plain_error(&error)), true),
        }
    }

    /// Sets what the dragged slider sets; it is saved when the drag ends.
    fn drag_slider(&mut self, lparam: LPARAM) {
        let Some(slider) = self.dragging else { return };
        let Some(track) = self.settings.view.regions.rect_of(Target::Slider(slider)) else { return };
        let (x, _) = self.settings.to_dips(point_from(lparam));
        let changed = match slider {
            Slider::Threshold(input) => {
                let before = self.controls.threshold_db(input);
                self.controls.set_threshold_db(input, threshold_at(&track, x));
                self.controls.threshold_db(input) != before
            }
            Slider::PillOpacity => {
                let percent = (fraction_at(&track, x) * 100.0).round() as u8;
                std::mem::replace(&mut self.subtitles.settings.opacity, percent) != percent
            }
            Slider::WindowOpacity { locked } => {
                let percent = MIN_WINDOW_OPACITY + (fraction_at(&track, x) * (100 - MIN_WINDOW_OPACITY) as f32).round() as u8;
                std::mem::replace(&mut self.opacity_mut(locked).window, percent) != percent
            }
            Slider::TextOpacity { locked } => {
                let percent = MIN_TEXT_OPACITY + (fraction_at(&track, x) * (100 - MIN_TEXT_OPACITY) as f32).round() as u8;
                std::mem::replace(&mut self.opacity_mut(locked).text, percent) != percent
            }
        };
        if changed {
            self.redraw();
        }
    }

    fn opacity_mut(&mut self, locked: bool) -> &mut Opacity {
        if locked { &mut self.locked_opacity } else { &mut self.opacity }
    }

    /// The panel's opacity now: the locked one while locked, or while its slider is dragged, to preview it.
    fn shown_opacity(&self) -> Opacity {
        let dragging_locked = matches!(self.dragging, Some(Slider::WindowOpacity { locked: true } | Slider::TextOpacity { locked: true }));
        if self.view.locked || dragging_locked { self.locked_opacity } else { self.opacity }
    }

    /// Takes the latest levels; redraws only if a meter moved.
    fn update_meters(&mut self) {
        let shown = self.meter_db;
        for (level, input) in self.meter_db.iter_mut().zip(METER_INPUTS) {
            // Below the slider's range a meter looks the same, so quiet noise doesn't redraw it.
            *level = self.controls.take_level(input).max(*level - METER_FALL_DB).max(THRESHOLD_MIN_DB);
        }
        if self.meter_db != shown {
            self.redraw();
        }
    }

    fn meters(&self) -> [Meter; 2] {
        [0, 1].map(|i| Meter { input: METER_INPUTS[i], threshold_db: self.controls.threshold_db(METER_INPUTS[i]), level_db: self.meter_db[i] })
    }

    fn save_settings(&mut self) {
        let settings = Settings {
            mic_threshold_db: self.controls.threshold_db(Input::Mic),
            computer_threshold_db: self.controls.threshold_db(Input::Computer),
            // The pick is saved even before it finishes loading; startup falls back if it can't load.
            model: self.controls.model().wanted().model.id.clone(),
            device: self.controls.model().wanted().device,
            keep_days: self.controls.keep_days.load(Ordering::Relaxed),
            auto_save: self.controls.auto_save.load(Ordering::Relaxed),
            save_mic_only: self.controls.save_mic_only.load(Ordering::Relaxed),
            keybinds: self.keybinds.clone(),
            themes: self.subtitles.themes,
            subtitles: self.subtitles.settings.clone(),
            auto_fold_minutes: self.auto_fold,
            block_answer: self.block_answer,
            blocked: self.controls.blocked().into_iter().filter(|app| app.always).map(|app| app.name).collect(),
            pinned: self.captions.lock().unwrap().pinned.clone(),
            check_updates: self.controls.check_updates.load(Ordering::Relaxed),
            opacity: self.opacity,
            locked_opacity: self.locked_opacity,
            dictation_only: self.controls.dictation_only.load(Ordering::Relaxed),
            words: self.controls.words.settings(),
        };
        if let Err(error) = settings.save() {
            self.notify(format!("Couldn't save settings: {}", plain_error(&error)), true);
        }
    }

    /// Closing is posted rather than done here, so the window is destroyed outside this handler. While a
    /// download runs it first asks whether to pause it until next time.
    fn quit(&mut self) {
        if self.downloads.active().is_some() {
            self.view.quit_prompt = true;
            if self.subtitles.settings.on {
                self.set_subtitles(false);
            }
            if !self.is_visible() {
                self.set_visible(true);
            }
            if self.view.collapsed {
                self.toggle_collapsed();
            }
            return self.redraw();
        }
        unsafe { PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)).ok() }.unwrap_or_default();
    }

    /// Checks for a newer version on a thread of its own; one found is offered over the panel.
    fn check_for_update(&mut self) {
        if self.settings.view.update_check == Some(UpdateCheck::Checking) {
            return;
        }
        self.settings.view.update_check = Some(UpdateCheck::Checking);
        let (controls, handle) = (self.controls.clone(), Handle(self.hwnd.0 as isize));
        std::thread::spawn(move || {
            let found = update::check_now().map(|release| release.map(|release| controls.offer_update(release)).is_some());
            if let Err(why) = &found {
                log::write(format!("Update check failed: {why}"));
            }
            controls.post_checked(found);
            handle.refresh();
        });
    }

    /// Asks over the panel whether to install a newer version, with a balloon while the panel is hidden.
    fn offer_update(&mut self, release: Release) {
        if !self.is_visible() || self.subtitles.settings.on {
            let text = format!("Kiku {} is out. Open Kiku to update.", release.version);
            self.tray.iter().for_each(|tray| tray.balloon(&text));
        }
        self.view.update = Some(release);
    }

    /// Downloads the update offered on a thread of its own. Once it checks out its installer starts and
    /// Kiku quits; the installer waits for that, and opens Kiku again when it is done.
    fn install_update(&mut self) {
        let Some(release) = self.view.update.take() else { return };
        self.notify(format!("Downloading Kiku {}…", release.version), false);
        let (controls, handle) = (self.controls.clone(), Handle(self.hwnd.0 as isize));
        std::thread::spawn(move || match update::download(&release).and_then(|installer| run_installer(&installer)) {
            Ok(()) => handle.quit_for_update(),
            Err(error) => {
                log::write(format!("Update to {} failed: {error}", release.version));
                controls.post_notice(format!("Couldn't update: {error}"));
                handle.refresh();
            }
        });
    }

    /// Opens the settings window on setup's first step, with the model recommended for this PC picked
    /// unless one that runs is already.
    fn start_setup(&mut self) {
        let wanted = self.controls.model().wanted();
        if !Download::Model(wanted.model, wanted.variant()).complete() {
            let recommended = self.hardware.recommend();
            self.controls.request_model(Choice { model: recommended.model, device: recommended.variant.device });
        }
        self.settings.show_tab(Tab::General);
        self.settings.view.setup = Some(SetupStep::ALL[0]);
        self.settings.open(self.hwnd);
        self.watch_meters();
    }

    /// Goes on to the next step; leaving the model step downloads the model picked if it isn't here.
    fn next_setup_step(&mut self) {
        let Some(step) = self.settings.view.setup else { return };
        if self.settings.view.rebinding.is_some() {
            self.finish_rebinding(None);
        }
        if step == SetupStep::Model {
            let wanted = self.controls.model().wanted();
            let target = Download::Model(wanted.model, wanted.variant());
            if !target.complete() {
                self.downloads.download(target);
            }
        }
        match step.next() {
            Some(next) => {
                self.settings.view.setup = Some(next);
                self.settings.view.scrolls.clear();
            }
            None => self.finish_setup(),
        }
    }

    /// Ends setup, done or skipped: settings are saved, so it doesn't open again.
    fn finish_setup(&mut self) {
        self.settings.view.setup = None;
        self.save_settings();
        self.close_settings();
    }

    /// Deletes a model's files or the GPU pack, or forgets an imported model.
    fn delete(&mut self, target: Download) {
        let deleted = match target {
            Download::Model(model, variant) => catalog::delete(model, variant).map(|_| model.name.clone()),
            Download::Component(component) => catalog::delete_component(component).map(|_| component.name.clone()),
        };
        match deleted {
            Ok(name) => self.notify(format!("Deleted {name}"), false),
            Err(error) => self.notify(format!("Couldn't delete: {}", plain_error(&error)), true),
        }
    }

    /// Asks for a folder and tries the model in it on a thread of its own; what it found shows under
    /// the models in settings.
    fn import_model(&mut self) {
        let Some(dir) = pick_folder(self.settings.hwnd) else { return };
        *self.import.lock().unwrap() = ImportStatus::Trying;
        let (status, overlay) = (Arc::clone(&self.import), Handle(self.hwnd.0 as isize));
        std::thread::spawn(move || {
            let found = match imports::import(&dir) {
                Ok(import) => ImportStatus::Imported(format!(
                    "Added {} ({}). It heard: \u{201c}{}\u{201d}. Pick it above to use it.",
                    import.model.name,
                    import.model.backend.description(),
                    import.heard
                )),
                Err(error) => ImportStatus::Failed(error),
            };
            *status.lock().unwrap() = found;
            overlay.refresh();
        });
        self.redraw();
    }

    /// Stops capturing and captioning a section's app, until Kiku quits or `always`; what it said stays.
    fn block(&mut self, section: usize, always: bool) {
        let name = self.captions.lock().unwrap().sections[section].label.clone();
        self.controls.block(&name, always);
        self.view.hovered_header = None;
        self.save_settings();
        let _ = self.wake_discovery.send(());
        let until = if always { "" } else { " until Kiku quits" };
        self.notify(format!("Blocked {name}{until}: Settings > Apps unblocks it"), false);
    }

    /// Saves to the history database; the decoder, which owns it, does the writing.
    fn save(&mut self, request: SaveRequest, what: &str) {
        self.controls.request_save(request);
        self.notify(format!("Saved {what} to History"), false);
    }

    fn copy_section(&mut self, index: usize) {
        let text = {
            let captions = self.captions.lock().unwrap();
            captions.sections[index].captions.iter().map(|c| c.display_text()).collect::<Vec<_>>().join("\n")
        };
        let count = text.lines().count();
        self.copy(&text, format!("Copied {count} line{}", if count == 1 { "" } else { "s" }));
    }

    /// A caption as shown, e.g. "You: on my way".
    fn caption_text(&self, line: LineRef) -> Option<String> {
        self.caption(line).map(|caption| caption.display_text())
    }

    fn caption(&self, line: LineRef) -> Option<Caption> {
        let captions = self.captions.lock().unwrap();
        let lines = &captions.sections[line.section].captions;
        Some(lines[line.position(lines)?].clone())
    }

    /// Pressing on caption text starts a selection; pressing anywhere else clears it.
    fn start_selecting(&mut self, lparam: LPARAM) {
        let (x, y) = self.to_dips(point_from(lparam));
        self.view.selection = self.text_point(x, y, false).map(|point| Selection { anchor: point, focus: point });
        self.selecting = self.view.selection.is_some();
        if self.selecting {
            unsafe { SetCapture(self.hwnd) };
        }
        self.redraw();
    }

    fn extend_selection(&mut self, lparam: LPARAM) {
        let (x, y) = self.to_dips(point_from(lparam));
        let point = self.text_point(x, y, true);
        if let (Some(selection), Some(point)) = (self.view.selection.as_mut(), point) {
            if selection.focus != point {
                selection.focus = point;
                self.redraw();
            }
        }
    }

    /// Copies what was selected; a press without a drag selected nothing.
    fn finish_selecting(&mut self) {
        self.selecting = false;
        unsafe { ReleaseCapture().ok() }.unwrap_or_default();
        match self.view.selection.filter(|s| !s.is_empty()) {
            Some(selection) => {
                let text = selection.text(&self.captions.lock().unwrap(), &self.view.regions.sections);
                self.copy(&text, "Copied the selection".into());
            }
            None => self.view.selection = None,
        }
        self.redraw();
    }

    /// The place in caption text under a panel point. While dragging, a point between captions
    /// counts as the `nearest` caption.
    fn text_point(&self, x: f32, y: f32, nearest: bool) -> Option<TextPoint> {
        let region = if nearest { self.view.regions.nearest_caption(x, y)? } else { self.view.regions.caption_at(x, y)? };
        let caption = self.caption(region.line)?;
        let y = y.clamp(region.visible.top, region.visible.bottom - 0.5);
        let offset = self.renderer.text_offset(&caption, region.width, x - region.x, y - region.y).ok()?;
        Some(TextPoint { line: region.line, offset })
    }

    fn share(&mut self, what: &str, text: &str) {
        match share::send(self.hwnd, what, text) {
            Ok(share::Sent::Clipboard) => self.notify("Too long to prefill: paste into Claude with Ctrl+V".into(), false),
            Ok(_) => self.notify("Opened a new Claude chat".into(), false),
            Err(error) => self.notify(format!("Couldn't send to Claude: {error}"), true),
        }
    }

    fn copy(&mut self, text: &str, done: String) {
        match clipboard::copy(self.hwnd, text) {
            Ok(()) => self.notify(done, false),
            Err(error) => self.notify(format!("Couldn't copy: {}", plain_error(&error)), true),
        }
    }

    /// Types what you said into the focused app. Pressed again soon after, it replaces that
    /// with one more of your lines in front, reaching further back each time.
    fn paste_said(&mut self) {
        let (erase, lines) = match self.typed {
            Some(typed) if typed.lines > 0 && typed.at.elapsed() < GROW_WITHIN => (Some(typed.erase), typed.lines + 1),
            _ => (None, 1),
        };
        let Some(said) = self.captions.lock().unwrap().said(lines) else {
            return self.notify("Nothing heard from your mic yet".into(), false);
        };
        let said = self.controls.words.apply_to_caption(&said, &keys::focused_app().unwrap_or_default());
        if let Err(error) = self.paste(erase, &said) {
            return self.notify(format!("Couldn't paste: {}", plain_error(&error)), true);
        }
        self.typed = Some(Typed { erase: keys::Erase::Pasted(said.chars().count()), lines, at: Instant::now() });
    }

    /// Pastes `text` through the clipboard after taking back `erase`: one paste, where typing it
    /// would send every character as keystrokes that each app and keyboard hook has to work through.
    fn paste(&mut self, erase: Option<keys::Erase>, text: &str) -> Result<()> {
        // A paste still holding the clipboard keeps what was there before it.
        let contents = match self.lent.take() {
            Some(lent) => lent.contents,
            None => clipboard::contents(self.hwnd),
        };
        let lent = clipboard::lend(self.hwnd, text);
        self.lent = Some(Lent { contents, sequence: unsafe { GetClipboardSequenceNumber() } });
        unsafe { SetTimer(Some(self.hwnd), CLIPBOARD_TIMER, CLIPBOARD_MS, None) };
        lent?;
        keys::paste(erase);
        Ok(())
    }

    /// Puts back what was on the clipboard before a paste, unless something has been copied since.
    fn give_back_clipboard(&mut self) {
        unsafe { KillTimer(Some(self.hwnd), CLIPBOARD_TIMER).ok() }.unwrap_or_default();
        let unchanged = |lent: &Lent| lent.sequence == unsafe { GetClipboardSequenceNumber() };
        if let Some(lent) = self.lent.take().filter(unchanged) {
            if let Err(error) = clipboard::restore(self.hwnd, &lent.contents) {
                log::write(format!("couldn't put the clipboard back: {error}"));
            }
        }
    }

    /// Types dictated lines as they finish.
    fn type_dictation(&mut self) {
        let lines = self.controls.take_typing();
        let app = if lines.is_empty() { String::new() } else { keys::focused_app().unwrap_or_default() };
        for line in lines {
            let line = self.controls.words.apply(&line, &app, Output::Dictation);
            if line.is_empty() {
                continue;
            }
            let text = dictated(&line);
            keys::type_text(&text);
            self.typed = Some(Typed { erase: keys::Erase::Typed(text.chars().count()), lines: 0, at: Instant::now() });
        }
    }

    /// Erases what this app last typed.
    fn undo_typing(&mut self) {
        if let Some(typed) = self.typed.take() {
            keys::erase(typed.erase);
        }
    }

    /// Asks for the first page of what the search and filters find, if anything is set.
    fn find(&mut self) {
        self.view.scrolls.remove(&ScrollArea::Results);
        if let Some(request) = self.view.finder.search() {
            self.searcher.ask(request);
        }
    }

    /// Asks for the next page of results once they are scrolled near their end.
    fn find_more_near_end(&mut self) {
        let Some(&(_, _, extent)) = self.view.regions.scroll_areas.iter().find(|(_, area, _)| *area == ScrollArea::Results) else { return };
        if self.view.scroll(ScrollArea::Results).top(extent) + 2.0 * extent.viewport >= extent.content
            && let Some(request) = self.view.finder.more()
        {
            self.searcher.ask(request);
        }
    }

    fn run_sql(&mut self) {
        let finder = &mut self.view.finder;
        if finder.sql.text.trim().is_empty() {
            return;
        }
        finder.running = true;
        self.searcher.ask(Request::Sql(finder.sql.text.clone()));
        self.view.scrolls.remove(&ScrollArea::Table);
    }

    /// Copies or shares what the history screen found: the SQL's rows, or every line the search finds.
    fn export_results(&mut self, share: bool) {
        let finder = &self.view.finder;
        if !finder.sql_mode {
            if let Some(request) = finder.export(share) {
                self.searcher.ask(request);
            }
            return;
        }
        let Some(Ok(table)) = &finder.table else { return };
        let (text, rows) = (table_text(table), table.rows.len());
        match share {
            true => self.share(&format!("{rows} rows a query found in Kiku's history"), &text),
            false => self.copy(&text, format!("Copied {rows} row{}", plural(rows))),
        }
    }

    /// Takes in what the history thread answered.
    fn receive_history(&mut self) {
        let replies: Vec<Reply> = self.searcher.replies().collect();
        for reply in replies {
            match reply {
                Reply::Sessions(sessions, speakers) => (self.view.saved, self.view.finder.speakers) = (sessions, speakers),
                Reply::Found { search, page, total } => self.view.finder.take_page(search, page, total),
                Reply::Table(table) => (self.view.finder.table, self.view.finder.running) = (Some(table), false),
                // Only while still on the list it was opened from.
                Reply::Opened { session, title, rows, focus } if matches!(self.view.screen, Screen::SavedList) => {
                    self.stop_typing();
                    match focus {
                        Some(_) => self.view.scrolls.remove(&ScrollArea::SavedFile),
                        None => self.view.scrolls.insert(ScrollArea::SavedFile, Scroll::at_top()),
                    };
                    self.view.screen = Screen::SavedFile { session, title, lines: rows, focus };
                }
                Reply::Opened { .. } => {}
                Reply::Exported { text, lines, share: true } => self.share(&format!("{lines} lines found in Kiku's history"), &text),
                Reply::Exported { text, lines, share: false } => self.copy(&text, format!("Copied {lines} line{}", plural(lines))),
                Reply::Deleted(lines) => {
                    self.notify(format!("Deleted the session's {lines} line{}", plural(lines)), false);
                    self.searcher.ask(Request::Sessions);
                    self.find();
                }
                Reply::Failed(error) => self.notify(format!("Couldn't read the history: {error}"), true),
            }
        }
        self.redraw();
    }

    fn leave_history(&mut self) {
        self.stop_typing();
        self.view.screen = Screen::Live;
    }

    /// Lets the panel take the keyboard, which it otherwise never does, so keys go to a history box.
    fn start_typing(&mut self, text_box: TextBox) {
        self.view.finder.typing = Some(text_box);
        unsafe {
            let style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) & !(WS_EX_NOACTIVATE.0 as isize);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, style);
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    /// Gives the keyboard back: the panel no longer takes it when clicked.
    fn stop_typing(&mut self) {
        if self.view.finder.typing.take().is_some() {
            unsafe { SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) | WS_EX_NOACTIVATE.0 as isize) };
            self.redraw();
        }
    }

    /// The history box being typed in.
    fn typing_field(&mut self) -> Option<&mut field::Field> {
        let finder = &mut self.view.finder;
        match finder.typing? {
            TextBox::Search => Some(&mut finder.search),
            TextBox::Sql => Some(&mut finder.sql),
        }
    }

    /// Editing keys for the box being typed in; Escape empties it, then leaves it, and Enter runs SQL.
    fn on_typing_key(&mut self, key: VIRTUAL_KEY) {
        let (ctrl, hwnd) = (ctrl_held(), self.hwnd);
        let sql = self.view.finder.typing == Some(TextBox::Sql);
        let Some(field) = self.typing_field() else { return };
        if !field.edit(key, ctrl, || clipboard::text(hwnd)) {
            match key {
                VK_ESCAPE if !field.text.is_empty() => field.clear(),
                VK_ESCAPE => return self.stop_typing(),
                VK_RETURN if sql => return self.perform(Action::RunSql),
                _ => return,
            }
        }
        self.after_typing();
    }

    /// Searches again for what the search box holds now.
    fn after_typing(&mut self) {
        let finder = &self.view.finder;
        if finder.typing == Some(TextBox::Search) && finder.text_changed() {
            self.find();
        }
        self.redraw();
    }

    /// Shows a short message in the panel's title bar and the settings window's.
    fn notify(&mut self, text: String, error: bool) {
        self.view.notice = Some(Notice { text, error });
        self.settings.view.notice = self.view.notice.clone();
        unsafe { SetTimer(Some(self.hwnd), NOTICE_TIMER, if error { ERROR_NOTICE_MS } else { NOTICE_MS }, None) };
    }

    fn on_captions_key(&mut self, event: keys::KeyEvent) {
        if let keys::KeyEvent::Captured { vk, down } = event {
            return self.on_captured(vk, down);
        }
        if event == keys::KeyEvent::Held {
            if self.controls.stopped.load(Ordering::Relaxed) {
                self.notify(format!("Captioning is stopped: {} starts it", self.keybinds.shortcut(Command::Stop)), false);
            } else {
                unsafe { SetTimer(Some(self.hwnd), DICTATE_TIMER, DICTATE_AFTER_MS, None) };
            }
            return self.redraw();
        }
        // Anything else ends dictation; what was said so far is still typed.
        unsafe { KillTimer(Some(self.hwnd), DICTATE_TIMER).ok() }.unwrap_or_default();
        self.controls.dictating.store(false, Ordering::Relaxed);
        match event {
            keys::KeyEvent::Command(Command::Paste) => self.paste_said(),
            keys::KeyEvent::Command(Command::Undo) => self.undo_typing(),
            keys::KeyEvent::Command(Command::Stop) => self.toggle_stop(),
            keys::KeyEvent::Command(Command::Lock) => self.toggle_lock(),
            keys::KeyEvent::Command(Command::Hide) => self.set_display_visible(!self.display_visible()),
            keys::KeyEvent::Command(Command::Subtitles) => self.set_subtitles(!self.subtitles.settings.on),
            keys::KeyEvent::Command(Command::Quit) => return self.quit(),
            keys::KeyEvent::Command(Command::SaveMiss) => {
                self.controls.save_miss.store(true, Ordering::Relaxed);
                self.notify("Saved the last 30 s of audio to the misses folder".into(), false);
            }
            _ => {}
        }
        self.redraw();
    }

    /// Waits for the keys of the shortcut picked in settings. Every key goes here and to no app until done.
    fn start_rebinding(&mut self, rebind: Rebind) {
        self.settings.view.rebinding = Some(rebind);
        (self.captured, self.captured_down) = (Vec::new(), Vec::new());
        keys::set_capturing(true);
        unsafe { SetTimer(Some(self.hwnd), REBIND_TIMER, REBIND_MS, None) };
    }

    /// The captions key is every key held together, taken once all are let go; a command takes the
    /// first non-modifier key. Escape on its own cancels.
    fn on_captured(&mut self, vk: u16, down: bool) {
        let Some(rebind) = self.settings.view.rebinding else { return };
        const ESCAPE: u16 = 0x1B;
        if down && vk == ESCAPE && self.captured.is_empty() {
            return self.finish_rebinding(None);
        }
        let key = Key::from_vk(vk);
        match rebind {
            Rebind::Command(command) if down && !key.is_modifier() => {
                let result = self.keybinds.set_command(command, key);
                self.finish_rebinding(Some(result.map(|()| format!("{} is now {}", command.short(), self.keybinds.shortcut(command)))));
            }
            Rebind::Hold if down => {
                if !self.captured.contains(&key) {
                    self.captured.push(key);
                }
                if !self.captured_down.contains(&vk) {
                    self.captured_down.push(vk);
                }
                self.redraw();
            }
            Rebind::Hold => {
                self.captured_down.retain(|&v| v != vk);
                if self.captured_down.is_empty() && !self.captured.is_empty() {
                    let result = self.keybinds.set_hold(std::mem::take(&mut self.captured));
                    self.finish_rebinding(Some(result.map(|()| format!("Hold {} to dictate", self.keybinds.hold_name()))));
                }
            }
            Rebind::Command(_) => {}
        }
    }

    /// Ends rebinding: `None` when cancelled, else what happened.
    fn finish_rebinding(&mut self, result: Option<std::result::Result<String, String>>) {
        keys::set_capturing(false);
        unsafe { KillTimer(Some(self.hwnd), REBIND_TIMER).ok() }.unwrap_or_default();
        self.settings.view.rebinding = None;
        self.captured.clear();
        match result {
            Some(Ok(done)) => {
                keys::set_keybinds(self.keybinds.clone());
                self.save_settings();
                self.notify(done, false);
            }
            Some(Err(why)) => self.notify(format!("Not changed: {why}"), true),
            None => {}
        }
        self.redraw();
    }

    fn on_tray(&mut self, mouse_message: u32) {
        match mouse_message {
            WM_LBUTTONUP => self.set_display_visible(!self.display_visible()),
            WM_RBUTTONUP => {
                let state = MenuState {
                    visible: self.display_visible(),
                    subtitles: self.subtitles.settings.on,
                    locked: self.view.locked,
                    stopped: self.controls.stopped.load(Ordering::Relaxed),
                };
                match self.tray.as_ref().and_then(|tray| tray.menu(state)) {
                    Some(MenuChoice::ShowOrHide) => self.set_display_visible(!state.visible),
                    Some(MenuChoice::ToggleSubtitles) => self.set_subtitles(!state.subtitles),
                    Some(MenuChoice::ToggleLock) => self.toggle_lock(),
                    Some(MenuChoice::ToggleStop) => self.toggle_stop(),
                    Some(MenuChoice::Quit) => return self.quit(),
                    None => {}
                }
                self.redraw();
            }
            _ => {}
        }
    }

    /// Shows captions mode instead of the panel, or the panel again; saving, dictation and the
    /// shortcuts work the same either way.
    fn set_subtitles(&mut self, on: bool) {
        if on == self.subtitles.settings.on {
            return;
        }
        if on {
            self.set_visible(false);
            self.start_subtitles();
        } else {
            self.subtitles.stop();
            self.set_visible(true);
        }
        self.save_settings();
        self.redraw();
    }

    fn start_subtitles(&mut self) {
        let captions = self.captions.lock().unwrap();
        if let Err(error) = self.subtitles.start(&captions, &self.renderer) {
            log::write(format!("captions mode: {error}"));
        }
    }

    fn place_subtitles(&mut self) {
        if let Err(error) = self.subtitles.place(&self.renderer) {
            log::write(format!("captions mode: {error}"));
        }
    }

    /// Changes captions mode's settings and saves them.
    fn restyle(&mut self, change: impl FnOnce(&mut SubtitleSettings)) {
        change(&mut self.subtitles.settings);
        self.save_settings();
        if self.subtitles.settings.on {
            self.place_subtitles();
        }
    }

    /// Changes the app's theme and saves it.
    fn retheme(&mut self, change: impl FnOnce(&mut Themes)) {
        change(&mut self.subtitles.themes);
        self.save_settings();
    }

    /// The tab's mouse messages: a drag that ends saves where the captions went, a double-click
    /// brings the panel back.
    fn on_subtitle_message(&mut self, message: u32) {
        match self.subtitles.on_message(message, &mut self.renderer) {
            Ok(TabEvent::Moved) => {
                self.save_settings();
                self.redraw();
            }
            Ok(TabEvent::ShowPanel) => self.set_subtitles(false),
            Ok(TabEvent::Nothing) if message == WM_DPICHANGED => self.redraw(),
            Ok(TabEvent::Nothing) => {}
            Err(error) => log::write(format!("captions mode: {error}")),
        }
    }

    fn toggle_stop(&self) {
        self.controls.stopped.fetch_xor(true, Ordering::Relaxed);
    }

    /// Edges and the corner grip resize the window, the title bar drags it, everything else is clickable.
    fn hit_test(&self, lparam: LPARAM) -> LRESULT {
        let mut point = point_from(lparam);
        let _ = unsafe { ScreenToClient(self.hwnd, &mut point) };
        if let Some(edge) = self.edge_at(point) {
            return LRESULT(edge as isize);
        }
        LRESULT(match self.target_at(point) {
            Some(Target::Drag) => HTCAPTION,
            Some(Target::Resize(Corner::TopLeft)) => HTTOPLEFT,
            Some(Target::Resize(Corner::BottomRight)) => HTBOTTOMRIGHT,
            _ => HTCLIENT,
        } as isize)
    }

    /// A hand over things that can be clicked, the arrow elsewhere.
    fn client_cursor(&self) -> PCWSTR {
        let mut point = POINT::default();
        let _ = unsafe { GetCursorPos(&mut point) };
        let _ = unsafe { ScreenToClient(self.hwnd, &mut point) };
        let (x, y) = self.to_dips(point);
        match self.target_at(point) {
            Some(Target::Click(_)) => IDC_HAND,
            Some(Target::Card) => IDC_ARROW,
            _ if self.view.regions.caption_at(x, y).is_some() => IDC_IBEAM,
            _ => IDC_ARROW,
        }
    }

    /// The resize hit-test code for a point near an edge or corner, if any.
    fn edge_at(&self, client: POINT) -> Option<u32> {
        let mut bounds = RECT::default();
        unsafe { GetWindowRect(self.hwnd, &mut bounds).ok()? };
        let (x, y) = self.to_dips(client);
        let scale = self.scale();
        let width = (bounds.right - bounds.left) as f32 / scale - HANDLE_STRIP;
        let height = (bounds.bottom - bounds.top) as f32 / scale;
        // Left of the panel is the drag handle strip, not an edge.
        let (left, right) = ((0.0..RESIZE_BORDER).contains(&x), x > width - RESIZE_BORDER);
        let (top, bottom) = (y < RESIZE_BORDER, y > height - RESIZE_BORDER);
        Some(match (left, right, top, bottom) {
            (true, _, true, _) => HTTOPLEFT,
            (_, true, true, _) => HTTOPRIGHT,
            (true, _, _, true) => HTBOTTOMLEFT,
            (_, true, _, true) => HTBOTTOMRIGHT,
            (true, ..) => HTLEFT,
            (_, true, ..) => HTRIGHT,
            (_, _, true, _) => HTTOP,
            (_, _, _, true) => HTBOTTOM,
            _ => return None,
        })
    }

    fn on_title_bar_hover(&mut self, over: bool) {
        if over && !self.tracking_title_bar {
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE | TME_NONCLIENT,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            self.tracking_title_bar = unsafe { TrackMouseEvent(&mut track) }.is_ok();
        }
        if self.view.drag_hover != over {
            self.view.drag_hover = over;
            self.redraw();
        }
    }

    fn on_mouse_move(&mut self, lparam: LPARAM) {
        self.on_title_bar_hover(false);
        if !self.tracking_mouse {
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            self.tracking_mouse = unsafe { TrackMouseEvent(&mut track) }.is_ok();
        }
        let hovered = match self.target_at_client(lparam) {
            Some(Target::Click(action)) => Some(action),
            _ => None,
        };
        let (x, y) = self.to_dips(point_from(lparam));
        self.view.pointer = (x, y);
        let (line, header) = (self.view.regions.line_at(x, y), self.view.regions.header_at(x, y));
        self.set_hovered(hovered, line, header);
    }

    fn set_hovered(&mut self, hovered: Option<Action>, line: Option<LineRef>, header: Option<usize>) {
        if self.view.hovered != hovered {
            self.view.tip = false;
            self.wait_for_tip(hovered);
        }
        let view = &mut self.view;
        if (view.hovered, view.hovered_line, view.hovered_header) != (hovered, line, header) {
            (view.hovered, view.hovered_line, view.hovered_header) = (hovered, line, header);
            self.redraw();
        }
    }

    /// Draws again once the pointer has rested on a button long enough for its tooltip.
    fn wait_for_tip(&self, hovered: Option<Action>) {
        if hovered.is_some() {
            unsafe { SetTimer(Some(self.hwnd), TIP_TIMER, TIP_MS, None) };
        }
    }

    /// Pushes in the button the mouse went down on, or lets it out.
    fn press(&mut self, action: Option<Action>) {
        if self.view.pressed != action {
            self.view.pressed = action;
            self.redraw();
        }
    }

    /// Shows a section alone by folding the others, or puts the folds back if it already is.
    fn solo(&mut self, index: usize) {
        let captions = self.captions.lock().unwrap();
        let shown: Vec<usize> = captions.visible().map(|(index, _)| index).filter(|&index| !self.view.folds.is_hidden(index)).collect();
        drop(captions);
        self.view.folds.solo(index, shown);
        self.redraw();
    }

    fn on_wheel(&mut self, wparam: WPARAM, lparam: LPARAM) {
        let mut point = point_from(lparam);
        let _ = unsafe { ScreenToClient(self.hwnd, &mut point) };
        let (x, y) = self.to_dips(point);
        if self.view.scroll_at(x, y, -wheel_notches(wparam) * WHEEL_STEP) {
            self.find_more_near_end();
            self.redraw();
        }
    }

    /// Asks for a frame. Every request made before it is drawn shares it, so a burst of caption
    /// updates and mouse moves draws once.
    fn redraw(&mut self) {
        if !self.frame_pending {
            self.frame_pending = true;
            unsafe { PostMessageW(Some(self.hwnd), WM_DRAW, WPARAM(0), LPARAM(0)).ok() }.unwrap_or_default();
        }
    }

    fn draw(&mut self) {
        self.frame_pending = false;
        match self.draw_windows() {
            // The graphics device was lost (driver update, remote desktop); start over with a new one.
            Err(error) if error.code() == D2DERR_RECREATE_TARGET => match Renderer::new() {
                Ok(renderer) => {
                    self.renderer = renderer;
                    self.draw();
                }
                Err(error) => log::write(format!("overlay renderer lost: {error}")),
            },
            Err(error) => log::write(format!("overlay redraw failed: {error}")),
            Ok(()) => {}
        }
    }

    fn draw_subtitles(&mut self) -> Result<()> {
        let hint = format!("Captions mode  ·  {} or double-click the tab beside them for the transcript", self.keybinds.shortcut(Command::Subtitles));
        let captions = self.captions.lock().unwrap();
        self.subtitles.draw(&mut self.renderer, &captions, &hint)
    }

    /// Draws captions mode or the panel, whichever shows, and the settings window if it is open. A panel
    /// hidden in the tray isn't drawn; showing it again redraws.
    fn draw_windows(&mut self) -> Result<()> {
        if self.subtitles.settings.on {
            self.draw_subtitles()?;
        }
        let panel_shows = !self.subtitles.settings.on && self.is_visible();
        if !panel_shows && !self.settings.is_open() {
            return Ok(());
        }
        let mut bounds = RECT::default();
        unsafe { GetWindowRect(self.hwnd, &mut bounds)? };
        let scale = self.scale();
        let blocked = self.controls.blocked();
        let import = self.import.lock().unwrap().clone();
        let words = self.controls.words.words();
        let captions = self.captions.lock().unwrap();
        self.schedule_next_change(&captions);
        let height = match self.view.collapsed {
            true => (TITLE_HEIGHT * scale).round() as i32,
            false => panel_shows.then(|| self.restore_height.take()).flatten().unwrap_or(bounds.bottom - bounds.top),
        };
        let panel = Panel {
            captions: &captions,
            view: &self.view,
            stopped: self.controls.stopped.load(Ordering::Relaxed),
            dictating: self.controls.dictating.load(Ordering::Relaxed),
            keybinds: &self.keybinds,
            captured: &self.captured,
            model: self.controls.model(),
            hardware: &self.hardware,
            downloads: &self.downloads,
            import: &import,
            auto_save: self.controls.auto_save.load(Ordering::Relaxed),
            save_mic_only: self.controls.save_mic_only.load(Ordering::Relaxed),
            keep_days: self.controls.keep_days.load(Ordering::Relaxed),
            check_updates: self.controls.check_updates.load(Ordering::Relaxed),
            dictation_only: self.controls.dictation_only.load(Ordering::Relaxed),
            auto_fold: self.auto_fold,
            blocked: &blocked,
            block_answer: self.block_answer,
            themes: self.subtitles.themes,
            shown_opacity: self.shown_opacity(),
            opacity: self.opacity,
            locked_opacity: self.locked_opacity,
            subtitles: &self.subtitles.settings,
            words: &words,
            word_settings: self.controls.words.settings(),
            settings_open: self.settings.is_open(),
            usage: self.usage,
            meters: self.meters(),
            width: (bounds.right - bounds.left) as f32 / scale - HANDLE_STRIP,
            height: height as f32 / scale,
            dpi: scale * 96.0,
        };
        let mut panel_regions = None;
        if panel_shows {
            let (frame, regions) = self.renderer.render(&mut self.panel_surface, &panel)?;
            frame.present(self.hwnd, POINT { x: bounds.left, y: bounds.top }, u8::MAX)?;
            panel_regions = Some(regions);
        }
        let mut settings_regions = None;
        if self.settings.is_open() {
            let (bounds, scale) = (self.settings.bounds(), self.settings.scale());
            let (width, height) = ((bounds.right - bounds.left) as f32 / scale, (bounds.bottom - bounds.top) as f32 / scale);
            let settings = Panel { view: &self.settings.view, width, height, dpi: scale * 96.0, ..panel };
            let (frame, regions) = self.renderer.render_settings(&mut self.settings.surface, &settings)?;
            frame.present(self.settings.hwnd, POINT { x: bounds.left, y: bounds.top }, u8::MAX)?;
            settings_regions = Some(regions);
        }
        drop(captions);
        if let Some(regions) = panel_regions {
            if let Some((area, top)) = regions.revealed {
                self.view.scrolls.insert(area, Scroll::at(top));
            }
            self.view.regions = regions;
        }
        if let Some(regions) = settings_regions {
            self.settings.view.regions = regions;
        }
        Ok(())
    }

    /// Draws again when the next "Live" badge is due to go out or the next quiet app folds; nothing else
    /// changes with time alone.
    fn schedule_next_change(&self, captions: &Captions) {
        let now = Instant::now();
        let badges = captions.visible().filter_map(|(_, section)| section.speaking_until()).filter(|&until| until > now);
        let next = badges.chain(self.view.folds.next_auto_fold(captions, auto_fold_after(self.auto_fold), now)).min();
        unsafe {
            match next {
                Some(until) => _ = SetTimer(Some(self.hwnd), LIVE_TIMER, (until - now).as_millis() as u32 + 1, None),
                None => _ = KillTimer(Some(self.hwnd), LIVE_TIMER),
            }
        }
    }

    /// Locked: clicks pass through to whatever is underneath. Unlocked: the panel is clickable.
    fn toggle_lock(&mut self) {
        self.view.locked = !self.view.locked;
        // A click-through window gets no mouse-leave, so clear the hover now.
        (self.view.hovered, self.view.hovered_header) = (None, None);
        unsafe {
            let style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) ^ WS_EX_TRANSPARENT.0 as isize;
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, style);
        }
    }

    fn toggle_collapsed(&mut self) {
        match self.collapsed_from.take() {
            Some(height) => self.restore_height = Some(height),
            None => {
                let mut bounds = RECT::default();
                let _ = unsafe { GetWindowRect(self.hwnd, &mut bounds) };
                self.collapsed_from = Some(bounds.bottom - bounds.top);
            }
        }
        self.view.collapsed = self.collapsed_from.is_some();
    }

    fn set_visible(&mut self, visible: bool) {
        let _ = unsafe { ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE }) };
        if visible {
            self.redraw();
        }
    }

    fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd) }.as_bool()
    }

    /// Whether the mode's display (the panel, or captions mode) is up rather than hidden.
    fn display_visible(&self) -> bool {
        if self.subtitles.settings.on { !self.subtitles.hidden } else { self.is_visible() }
    }

    fn set_display_visible(&mut self, visible: bool) {
        if self.subtitles.settings.on {
            self.subtitles.set_hidden(!visible);
            self.redraw();
        } else {
            self.set_visible(visible);
        }
    }

    fn limit_size(&self, lparam: LPARAM) {
        let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
        let scale = self.scale();
        info.ptMinTrackSize = POINT { x: ((MIN_SIZE.0 + HANDLE_STRIP) * scale) as i32, y: (MIN_SIZE.1 * scale) as i32 };
    }

    fn target_at_client(&self, lparam: LPARAM) -> Option<Target> {
        self.target_at(point_from(lparam))
    }

    fn target_at(&self, client: POINT) -> Option<Target> {
        let (x, y) = self.to_dips(client);
        self.view.regions.target_at(x, y)
    }

    /// Panel coordinates of a client point; the drag handle strip is at negative x.
    fn to_dips(&self, client: POINT) -> (f32, f32) {
        let scale = self.scale();
        (client.x as f32 / scale - HANDLE_STRIP, client.y as f32 / scale)
    }

    fn scale(&self) -> f32 {
        (unsafe { GetDpiForWindow(self.hwnd) }) as f32 / 96.0
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Windows suggests a new rectangle when a window moves to a monitor with another DPI.
fn move_to_suggested(hwnd: HWND, lparam: LPARAM) {
    let suggested = unsafe { &*(lparam.0 as *const RECT) };
    let (width, height) = (suggested.right - suggested.left, suggested.bottom - suggested.top);
    unsafe {
        let _ = SetWindowPos(hwnd, None, suggested.left, suggested.top, width, height, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Ctrl is held down now.
fn ctrl_held() -> bool {
    (unsafe { GetKeyState(VK_CONTROL.0 as i32) }) < 0
}

/// How far a mouse wheel message turned the wheel, in notches (positive = away from the user).
fn wheel_notches(wparam: WPARAM) -> f32 {
    (wparam.0 >> 16) as u16 as i16 as f32 / 120.0
}

/// A short form of an OS error, e.g. "access denied" rather than "Access is denied. (os error 5)".
fn plain_error(error: &dyn std::error::Error) -> String {
    let text = error.to_string();
    let text = text.split(" (os error").next().unwrap_or(&text);
    text.trim_end_matches('.').replace("Access is denied", "access denied")
}

/// The folder picked in Windows' folder dialog, if one was.
fn pick_folder(owner: HWND) -> Option<PathBuf> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        dialog.SetOptions(dialog.GetOptions().ok()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        dialog.SetTitle(w!("Pick the folder with your model's files")).ok()?;
        // Fails when cancelled.
        dialog.Show(Some(owner)).ok()?;
        let name = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0 as _));
        path
    }
}

extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let default = || unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    let shared = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Shared;
    // Null until `run` attaches the state, which is after window creation messages.
    let Some(state) = (unsafe { shared.as_ref() }) else { return default() };
    match message {
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(shared));
            }
            return default();
        }
        _ => {}
    }

    state.run(message, |window| window.handle(message, wparam, lparam)).unwrap_or_else(default)
}

/// The settings window's messages, handled by the panel's window state, which its user data leads to.
extern "system" fn settings_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let default = || unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    let panel = HWND(unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as _);
    // Null while the window is being created, and once the panel is gone.
    let shared = unsafe { GetWindowLongPtrW(panel, GWLP_USERDATA) } as *mut Shared;
    let Some(state) = (unsafe { shared.as_ref() }) else { return default() };
    state.run(message, |window| window.handle_settings(message, wparam, lparam)).unwrap_or_else(default)
}

impl Shared {
    /// Runs `handler` on the window state. Win32 calls made by a handler (a tray menu, moving a window)
    /// can send messages back before it returns; those get default handling and a redraw afterwards.
    fn run(&self, message: u32, handler: impl FnOnce(&mut Window) -> Option<LRESULT>) -> Option<LRESULT> {
        let Ok(mut window) = self.window.try_borrow_mut() else {
            if matches!(message, WM_REFRESH | WM_DRAW) {
                self.redraw_pending.set(true);
            }
            return None;
        };
        let handled = handler(&mut window);
        if self.redraw_pending.replace(false) {
            window.frame_pending = false;
            window.redraw();
        }
        handled
    }
}

/// Starts an update's installer: no questions, only its progress, and no restart of Windows.
fn run_installer(installer: &Path) -> std::result::Result<(), String> {
    let started = std::process::Command::new(installer).args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]).spawn();
    started.map(drop).map_err(|e| format!("couldn't start the installer: {e}"))
}

/// One Kiku per sign-in: returns whether this process should run, after asking an already running
/// Kiku to show itself if there is one. The mutex stays held until the process exits.
pub fn claim_single_instance() -> bool {
    let claimed = unsafe { CreateMutexW(None, false, w!("Local\\Kiku")) };
    if claimed.is_err() || unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return true;
    }
    if let Ok(panel) = unsafe { FindWindowW(w!("KikuOverlay"), PCWSTR::null()) } {
        let _ = unsafe { PostMessageW(Some(panel), WM_SHOW, WPARAM(0), LPARAM(0)) };
    }
    false
}

/// Mouse messages pack signed 16-bit coordinates into `lParam`.
fn point_from(lparam: LPARAM) -> POINT {
    POINT { x: lparam.0 as u16 as i16 as i32, y: (lparam.0 >> 16) as u16 as i16 as i32 }
}

fn create_window() -> Result<HWND> {
    let instance = unsafe { GetModuleHandleW(None)? };
    let class = WNDCLASSW {
        style: CS_DBLCLKS,
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        lpszClassName: w!("KikuOverlay"),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        ..Default::default()
    };
    register_class(&class)?;

    let scale = unsafe { GetDpiForSystem() } as f32 / 96.0;
    let (width, height) = (((DEFAULT_SIZE.0 + HANDLE_STRIP) * scale) as i32, (DEFAULT_SIZE.1 * scale) as i32);
    let mut work_area = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work_area as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    let (x, y) = (work_area.right - width - SCREEN_MARGIN, work_area.bottom - height - SCREEN_MARGIN);
    unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("KikuOverlay"),
            w!("Kiku"),
            WS_POPUP,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

/// A dictated line as typed: a space after it, so the next line or dictation doesn't run on.
fn dictated(line: &str) -> String {
    format!("{line} ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictated_lines_end_with_a_space_so_the_next_one_does_not_run_on() {
        assert_eq!(dictated("That's exactly."), "That's exactly. ");
        let typed = ["That's exactly", "And also."].map(dictated).concat();
        assert_eq!(typed, "That's exactly And also. ");
    }
}
