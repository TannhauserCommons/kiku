//! The captions key (Ctrl+Win by default; Windows never sees Fn): held alone it dictates, held with
//! another key it runs a command. The keys are set in settings. Also pastes into the focused app.
//!
//! A low-level keyboard hook sees every key press system-wide. It runs on a thread of its own that
//! only tracks the captions key and posts to the overlay window, so a busy panel never slows typing;
//! Windows drops hooks that take too long.

use std::{
    sync::{mpsc, LazyLock, Mutex, MutexGuard, PoisonError},
    thread::{self, JoinHandle},
};

use windows::{
    core::Result,
    Win32::{
        Foundation::{E_FAIL, HWND, LPARAM, LRESULT, WPARAM},
        System::Threading::{GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_HIGHEST},
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY,
                KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_INSERT, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU,
                VK_RCONTROL, VK_RETURN, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_Z,
            },
            WindowsAndMessaging::{
                CallNextHookEx, GetClassNameW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, PeekMessageW, PostMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
                KBDLLHOOKSTRUCT, LLKHF_UP, MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WM_APP, WM_QUIT,
            },
        },
    },
};

use crate::{
    processes::{root_of, snapshot, Process},
    sessions::display_name,
};
use kiku_core::{
    actions::{key_strokes, sequence_strokes, KeyStroke},
    keybinds::{Command, Hotkey, Key, Keybinds},
};

/// Posted to the overlay with a [`KeyEvent`] packed by [`KeyEvent::to_message`].
pub const WM_CAPTIONS_KEY: u32 = WM_APP + 3;
/// Set in `dwExtraInfo` of every key Kiku sends, so its keyboard hook lets those through untouched and counts
/// keys any other program sends (a Stream Deck, AutoHotkey) as typed.
const OWN_KEYS: usize = 0x4B49_4B55;
/// An unassigned key. Pressing it before a held Win or Alt is let go stops the Start menu or a
/// window menu from opening, as AutoHotkey does.
const MASK_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);
/// Window classes of consoles and terminals, where Ctrl+Z means end-of-file or suspend rather than undo.
const TERMINALS: [&str; 6] = [
    "ConsoleWindowClass",            // Windows console host
    "CASCADIA_HOSTING_WINDOW_CLASS", // Windows Terminal
    "mintty",                        // Git Bash, Cygwin, MSYS2
    "VirtualConsoleClass",           // ConEmu, Cmder
    "org.wezfurlong.wezterm",        // WezTerm
    "PuTTY",
];

/// What happened with the captions key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KeyEvent {
    /// All of the captions key's keys are down.
    Held,
    /// One of them was let go.
    Released,
    /// Another key was pressed with them, e.g. Ctrl+Win+D; it goes to Windows as usual.
    Other,
    Command(Command),
    /// While rebinding, any key, with its virtual-key code; no app sees it.
    Captured { vk: u16, down: bool },
    /// A macro's shortcut: the `index`th of those last handed to [`set_macro_hotkeys`], which bumped
    /// `generation`, so the overlay can ignore one pressed before the macros changed.
    Macro { generation: u32, index: usize },
}

impl KeyEvent {
    fn to_message(self) -> (WPARAM, LPARAM) {
        match self {
            KeyEvent::Held => (WPARAM(0), LPARAM(0)),
            KeyEvent::Released => (WPARAM(1), LPARAM(0)),
            KeyEvent::Other => (WPARAM(2), LPARAM(0)),
            KeyEvent::Command(command) => (WPARAM(3), LPARAM(Command::ALL.iter().position(|&c| c == command).unwrap_or(0) as isize)),
            KeyEvent::Captured { vk, down } => (WPARAM(if down { 4 } else { 5 }), LPARAM(vk as isize)),
            KeyEvent::Macro { generation, index } => (WPARAM(6), LPARAM(((u64::from(generation) << 32) | index as u64) as isize)),
        }
    }

    pub fn from_message(wparam: WPARAM, lparam: LPARAM) -> Self {
        match wparam.0 {
            0 => KeyEvent::Held,
            1 => KeyEvent::Released,
            2 => KeyEvent::Other,
            3 => KeyEvent::Command(Command::ALL[lparam.0 as usize % Command::ALL.len()]),
            6 => {
                let packed = lparam.0 as u64;
                KeyEvent::Macro { generation: (packed >> 32) as u32, index: (packed & u64::from(u32::MAX)) as usize }
            }
            down => KeyEvent::Captured { vk: lparam.0 as u16, down: down == 4 },
        }
    }
}

/// The hook's thread; dropping it removes the hook.
pub struct Hook {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Hook {
    fn drop(&mut self) {
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts watching the keyboard on a thread of its own, posting [`KeyEvent`]s to `window`.
pub fn install(window: HWND, keybinds: Keybinds) -> Result<Hook> {
    *context() = Context { window: window.0 as isize, binds: keybinds, ..Default::default() };
    let (started, thread_id) = mpsc::channel();
    let thread = thread::Builder::new().name("keyboard hook".into()).spawn(move || run_hook(&started))?;
    let thread_id = thread_id.recv().unwrap_or(Err(E_FAIL.into()))?;
    Ok(Hook { thread_id, thread: Some(thread) })
}

pub fn set_keybinds(keybinds: Keybinds) {
    let mut context = context();
    context.binds = keybinds;
    context.state = State::default();
}

/// While on, every key press goes to the overlay as [`KeyEvent::Captured`] and to no app, so it can be bound.
pub fn set_capturing(on: bool) {
    context().set_capturing(on);
}

/// The macros' shortcuts the hook catches from now on, posted as [`KeyEvent::Macro`] by their place here;
/// returns the generation they carry.
pub fn set_macro_hotkeys(hotkeys: Vec<Hotkey>) -> u32 {
    context().set_macro_hotkeys(hotkeys)
}

/// Runs the hook until [`Hook`] is dropped. Windows calls a low-level hook on the thread that set it,
/// while that thread waits for messages, so this thread does nothing else.
fn run_hook(started: &mpsc::Sender<Result<u32>>) {
    // Ahead of the decoder's busy threads, so no key press waits for a turn on the CPU.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
    let mut message = MSG::default();
    // Creates the thread's message queue, so the WM_QUIT that ends it can't be posted too early.
    let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
    let hook = match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0) } {
        Ok(hook) => hook,
        Err(error) => return drop(started.send(Err(error))),
    };
    let _ = started.send(Ok(unsafe { GetCurrentThreadId() }));
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {}
    let _ = unsafe { UnhookWindowsHookEx(hook) };
}

/// The physical state of the modifiers and the captions key's keys; keys Kiku sends itself are ignored.
#[derive(Default)]
struct State {
    down: Vec<Key>,
    /// The captions key was held or a macro's shortcut pressed, so letting go of a Win or Alt in it must not
    /// open a menu.
    used: bool,
    /// A command or macro key whose key-up must be swallowed like its key-down was.
    swallowed: Option<u32>,
}

impl State {
    fn held(&self, binds: &Keybinds) -> bool {
        binds.hold.iter().all(|key| self.down.contains(key))
    }
}

/// What the hook works from, shared with the overlay's thread, which changes the bindings.
#[derive(Default)]
pub(super) struct Context {
    /// The overlay window, as a number since a window handle can't cross threads.
    window: isize,
    binds: Keybinds,
    capturing: bool,
    state: State,
    /// The macros' shortcuts, by the place a [`KeyEvent::Macro`] names, and which handing of them this is.
    macros: Vec<Hotkey>,
    generation: u32,
}

impl Context {
    /// The hook's state with `binds` and nothing held, for the tests.
    #[cfg(test)]
    pub(super) fn new(binds: Keybinds) -> Context {
        Context { binds, ..Default::default() }
    }

    pub(super) fn set_capturing(&mut self, on: bool) {
        self.capturing = on;
        self.state = State::default();
    }

    /// Catches `hotkeys` from now on; returns the new generation their events carry.
    pub(super) fn set_macro_hotkeys(&mut self, hotkeys: Vec<Hotkey>) -> u32 {
        self.macros = hotkeys;
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }
}

/// What the hook does with one key: whether to swallow it, what to tell the overlay, and whether to press the
/// mask key first (letting go of a Win or Alt that was part of a shortcut).
#[derive(Debug, Default, PartialEq)]
pub(super) struct Outcome {
    pub swallow: bool,
    pub events: Vec<KeyEvent>,
    pub mask: bool,
}

/// What the hook does with `key`; keys Kiku sent itself (tagged in `dwExtraInfo`) are let through untouched.
pub(super) fn handle(context: &mut Context, key: &KBDLLHOOKSTRUCT) -> Outcome {
    let mut outcome = Outcome::default();
    if key.dwExtraInfo == OWN_KEYS {
        return outcome;
    }
    let (vk, down) = (key.vkCode, !key.flags.contains(LLKHF_UP));
    let Context { binds, capturing, state, macros, generation, .. } = context;
    if *capturing {
        outcome.events.push(KeyEvent::Captured { vk: vk as u16, down });
        outcome.swallow = true;
        return outcome;
    }
    let key = Key::from_vk(vk as u16);
    let was_held = state.held(binds);
    let in_hold = binds.hold.contains(&key);
    if key.is_modifier() || in_hold {
        outcome.mask = !down && state.used && matches!(key, Key::Win | Key::Alt);
        state.down.retain(|k| *k != key);
        if down {
            state.down.push(key);
        }
    }
    if in_hold {
        // A spare key used as the captions key (Caps Lock, F13) does nothing else.
        outcome.swallow = !key.is_modifier();
    } else if !down && state.swallowed == Some(vk) {
        state.swallowed = None;
        outcome.swallow = true;
    } else if let Some(index) = down.then(|| macro_at(macros, &state.down, key)).flatten() {
        // Holding the key repeats it, which runs the macro once.
        if state.swallowed != Some(vk) {
            outcome.events.push(KeyEvent::Macro { generation: *generation, index });
        }
        (state.swallowed, state.used, outcome.swallow) = (Some(vk), true, true);
    } else if was_held && down {
        match binds.command_for(key) {
            Some(command) => {
                outcome.events.push(KeyEvent::Command(command));
                state.swallowed = Some(vk);
                outcome.swallow = true;
            }
            None => outcome.events.push(KeyEvent::Other),
        }
    }
    let held = state.held(binds);
    if held && !was_held {
        state.used = true;
        outcome.events.push(KeyEvent::Held);
    } else if was_held && !held {
        outcome.events.push(KeyEvent::Released);
    }
    if state.down.is_empty() {
        state.used = false;
    }
    outcome
}

/// The place of the shortcut that is `key` with exactly the modifiers in `down` held.
fn macro_at(macros: &[Hotkey], down: &[Key], key: Key) -> Option<usize> {
    let held: Vec<Key> = down.iter().copied().filter(|key| key.is_modifier()).collect();
    macros.iter().position(|hotkey| match hotkey.keys.split_last() {
        Some((last, modifiers)) => *last == key && modifiers.len() == held.len() && modifiers.iter().all(|modifier| held.contains(modifier)),
        None => false,
    })
}

/// Forgets modifiers Windows says are up: the hook misses key-ups while input is on the secure desktop
/// (Ctrl+Alt+Del, UAC, the lock screen), which would leave them held for good. `current` is the key being
/// handled, whose state Windows hasn't updated yet.
fn drop_released_modifiers(state: &mut State, current: Key) {
    state.down.retain(|key| {
        if *key == current {
            return true;
        }
        let vk = match key {
            Key::Ctrl => VK_CONTROL,
            Key::Alt => VK_MENU,
            Key::Shift => VK_SHIFT,
            Key::Win => return unsafe { GetAsyncKeyState(VK_LWIN.0 as i32) < 0 || GetAsyncKeyState(VK_RWIN.0 as i32) < 0 },
            Key::Other(_) => return true,
        };
        (unsafe { GetAsyncKeyState(vk.0 as i32) }) < 0
    });
}

static CONTEXT: LazyLock<Mutex<Context>> = LazyLock::new(Mutex::default);

fn context() -> MutexGuard<'static, Context> {
    CONTEXT.lock().unwrap_or_else(PoisonError::into_inner)
}

extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let (outcome, window) = {
            let mut context = context();
            // Kiku's own keys lift held modifiers for a moment (a paste while dictating), which must not read as let go.
            if key.dwExtraInfo != OWN_KEYS {
                drop_released_modifiers(&mut context.state, Key::from_vk(key.vkCode as u16));
            }
            (handle(&mut context, key), context.window)
        };
        if outcome.mask {
            tap_mask_key();
        }
        for event in outcome.events {
            post(window, event);
        }
        if outcome.swallow {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn post(window: isize, event: KeyEvent) {
    let (wparam, lparam) = event.to_message();
    let _ = unsafe { PostMessageW(Some(HWND(window as _)), WM_CAPTIONS_KEY, wparam, lparam) };
}

/// A paste this app put into the focused app, this many characters long, to be taken back by [`erase`] or
/// a growing [`paste`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Erase(pub usize);

/// Takes back what this app last put into the focused app.
pub fn erase(what: Erase) {
    press(Some(what), []);
}

/// Pastes the clipboard into the focused app with Shift+Insert, which terminals take too (unlike
/// Ctrl+V), after taking back `erase`.
pub fn paste(erase: Option<Erase>) {
    let insert = |up| stroke_input(VK_INSERT, up, true);
    press(erase, [key_input(VK_LSHIFT, false), insert(false), insert(true), key_input(VK_LSHIFT, true)]);
}

/// Presses `keys` in the focused app as a shortcut, with the same care for held modifiers as [`paste`].
pub fn press_keys(keys: &[Key]) {
    press(None, inputs(&key_strokes(keys)));
}

/// Presses `chords` one after another, as [`press_keys`] does one.
pub fn press_chords(chords: &[Vec<Key>]) {
    press(None, inputs(&sequence_strokes(chords)));
}

/// Types `text` key by key (Unicode, so any character). Only for a macro's `type` step: a paste is
/// one edit, this is a keystroke per character for every app and keyboard hook to work through.
pub fn type_text(text: &str) {
    let unicode = |unit: u16, up| {
        let flags = if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE };
        INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wScan: unit, dwFlags: flags, dwExtraInfo: OWN_KEYS, ..Default::default() } } }
    };
    let text = text.replace("\r\n", "\n");
    let strokes: Vec<INPUT> = text.chars().flat_map(|c| match c {
        '\n' => vec![key_input(VK_RETURN, false), key_input(VK_RETURN, true)],
        c => c.encode_utf16(&mut [0; 2]).iter().flat_map(|&unit| [unicode(unit, false), unicode(unit, true)]).collect(),
    }).collect();
    press(None, strokes);
}

/// Sends `keys` after taking back `erase`. Held modifiers are let go for the moment, so a held
/// captions key doesn't turn the keys into shortcuts.
fn press(erase: Option<Erase>, keys: impl IntoIterator<Item = INPUT>) {
    let modifiers = [VK_LCONTROL, VK_RCONTROL, VK_LWIN, VK_RWIN, VK_LMENU, VK_RMENU, VK_LSHIFT, VK_RSHIFT];
    let held: Vec<VIRTUAL_KEY> = modifiers.into_iter().filter(|&k| unsafe { GetAsyncKeyState(k.0 as i32) } < 0).collect();
    let mut inputs = vec![key_input(MASK_KEY, false), key_input(MASK_KEY, true)];
    inputs.extend(held.iter().map(|&k| key_input(k, true)));
    inputs.extend(erase.map_or_else(Vec::new, erase_keys));
    inputs.extend(keys);
    inputs.extend(held.iter().map(|&k| key_input(k, false)));
    send(&inputs);
}

/// A paste is one edit that Ctrl+Z undoes; in a terminal it takes a Backspace per character.
fn erase_keys(Erase(chars): Erase) -> Vec<INPUT> {
    if terminal_in_front() {
        (0..chars).flat_map(|_| [key_input(VK_BACK, false), key_input(VK_BACK, true)]).collect()
    } else {
        vec![key_input(VK_LCONTROL, false), key_input(VK_Z, false), key_input(VK_Z, true), key_input(VK_LCONTROL, true)]
    }
}

/// The app being typed into, named as its captions would be, for its word rules.
pub fn focused_app() -> Option<String> {
    app_of(unsafe { GetForegroundWindow() }, &snapshot())
}

/// The app that owns `window`, named as its captions would be, among `processes`.
pub fn app_of(window: HWND, processes: &[Process]) -> Option<String> {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    root_of(pid, processes).map(|root| display_name(&root.exe))
}

fn terminal_in_front() -> bool {
    let mut class = [0u16; 64];
    let length = unsafe { GetClassNameW(GetForegroundWindow(), &mut class) }.max(0) as usize;
    TERMINALS.contains(&String::from_utf16_lossy(&class[..length]).as_str())
}

/// The inputs sending `strokes`.
pub fn inputs(strokes: &[KeyStroke]) -> Vec<INPUT> {
    strokes.iter().map(|stroke| stroke_input(VIRTUAL_KEY(stroke.vk), stroke.up, stroke.extended)).collect()
}

/// Kiku's own no-op key press: once it is the last input, Windows lets Kiku bring a window to the front.
pub(super) fn tap_mask_key() {
    send(&[key_input(MASK_KEY, false), key_input(MASK_KEY, true)]);
}

fn send(inputs: &[INPUT]) {
    unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
}

fn key_input(key: VIRTUAL_KEY, up: bool) -> INPUT {
    stroke_input(key, up, false)
}

fn stroke_input(key: VIRTUAL_KEY, up: bool, extended: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: key, dwFlags: flags, dwExtraInfo: OWN_KEYS, ..Default::default() } } }
}

