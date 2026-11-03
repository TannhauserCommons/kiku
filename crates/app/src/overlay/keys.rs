//! The captions key (Ctrl+Win by default; Windows never sees Fn): held alone it dictates, held with
//! another key it runs a command. The keys are set in settings. Also types and pastes into the focused app.
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
                KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_BACK, VK_INSERT, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN,
                VK_RCONTROL, VK_RETURN, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_Z,
            },
            WindowsAndMessaging::{
                CallNextHookEx, GetClassNameW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, PeekMessageW, PostMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
                KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_UP, MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WM_APP, WM_QUIT,
            },
        },
    },
};

use crate::{
    processes::{root_of, snapshot},
    sessions::display_name,
};
use kiku_core::keybinds::{Command, Key, Keybinds};

/// Posted to the overlay with a [`KeyEvent`] packed by [`KeyEvent::to_message`].
pub const WM_CAPTIONS_KEY: u32 = WM_APP + 3;
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
}

impl KeyEvent {
    fn to_message(self) -> (WPARAM, LPARAM) {
        match self {
            KeyEvent::Held => (WPARAM(0), LPARAM(0)),
            KeyEvent::Released => (WPARAM(1), LPARAM(0)),
            KeyEvent::Other => (WPARAM(2), LPARAM(0)),
            KeyEvent::Command(command) => (WPARAM(3), LPARAM(Command::ALL.iter().position(|&c| c == command).unwrap_or(0) as isize)),
            KeyEvent::Captured { vk, down } => (WPARAM(if down { 4 } else { 5 }), LPARAM(vk as isize)),
        }
    }

    pub fn from_message(wparam: WPARAM, lparam: LPARAM) -> Self {
        match wparam.0 {
            0 => KeyEvent::Held,
            1 => KeyEvent::Released,
            2 => KeyEvent::Other,
            3 => KeyEvent::Command(Command::ALL[lparam.0 as usize % Command::ALL.len()]),
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
    let mut context = context();
    context.capturing = on;
    context.state = State::default();
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

/// The physical state of the captions key's keys; keys we inject ourselves are ignored.
#[derive(Default)]
struct State {
    down: Vec<Key>,
    /// The captions key was held, so letting go of a Win or Alt in it must not open a menu.
    used: bool,
    /// A command key whose key-up must be swallowed like its key-down was.
    swallowed: Option<u32>,
}

impl State {
    fn held(&self, binds: &Keybinds) -> bool {
        binds.hold.iter().all(|key| self.down.contains(key))
    }
}

/// What the hook works from, shared with the overlay's thread, which changes the bindings.
#[derive(Default)]
struct Context {
    /// The overlay window, as a number since a window handle can't cross threads.
    window: isize,
    binds: Keybinds,
    capturing: bool,
    state: State,
}

static CONTEXT: LazyLock<Mutex<Context>> = LazyLock::new(Mutex::default);

fn context() -> MutexGuard<'static, Context> {
    CONTEXT.lock().unwrap_or_else(PoisonError::into_inner)
}

extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if !key.flags.contains(LLKHF_INJECTED) && on_key(&mut context(), key.vkCode, !key.flags.contains(LLKHF_UP)) {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Updates the key state and tells the overlay what changed. Returns true to swallow the key.
fn on_key(context: &mut Context, vk: u32, down: bool) -> bool {
    let Context { window, binds, capturing, state } = context;
    let post = |event| post(*window, event);
    if *capturing {
        post(KeyEvent::Captured { vk: vk as u16, down });
        return true;
    }
    let key = Key::from_vk(vk as u16);
    let was_held = state.held(binds);
    let swallow = if binds.hold.contains(&key) {
        if !down && state.used && matches!(key, Key::Win | Key::Alt) {
            send(&[key_input(MASK_KEY, false), key_input(MASK_KEY, true)]);
        }
        state.down.retain(|k| *k != key);
        if down {
            state.down.push(key);
        }
        // A spare key used as the captions key (Caps Lock, F13) does nothing else.
        !key.is_modifier()
    } else if !down && state.swallowed == Some(vk) {
        state.swallowed = None;
        true
    } else if was_held && down {
        match binds.command_for(key) {
            Some(command) => {
                post(KeyEvent::Command(command));
                state.swallowed = Some(vk);
                true
            }
            None => {
                post(KeyEvent::Other);
                false
            }
        }
    } else {
        false
    };
    let held = state.held(binds);
    if held && !was_held {
        state.used = true;
        post(KeyEvent::Held);
    } else if was_held && !held {
        post(KeyEvent::Released);
    }
    if state.down.is_empty() {
        state.used = false;
    }
    swallow
}

fn post(window: isize, event: KeyEvent) {
    let (wparam, lparam) = event.to_message();
    let _ = unsafe { PostMessageW(Some(HWND(window as _)), WM_CAPTIONS_KEY, wparam, lparam) };
}

/// Text this app put into the focused app, to be taken back by [`erase`] or a growing [`paste`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Erase {
    /// This many characters, typed as keystrokes.
    Typed(usize),
    /// This many characters, pasted at once.
    Pasted(usize),
}

/// Types `text` into the focused app.
pub fn type_text(text: &str) {
    press(
        None,
        text.encode_utf16().flat_map(|unit| match unit {
            0x0A => [key_input(VK_RETURN, false), key_input(VK_RETURN, true)],
            _ => [unicode(unit, false), unicode(unit, true)],
        }),
    );
}

/// Takes back what this app last put into the focused app.
pub fn erase(what: Erase) {
    press(Some(what), []);
}

/// Pastes the clipboard into the focused app with Shift+Insert, which terminals take too (unlike
/// Ctrl+V), after taking back `erase`.
pub fn paste(erase: Option<Erase>) {
    let insert = |up| {
        let flags = if up { KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP } else { KEYEVENTF_EXTENDEDKEY };
        INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_INSERT, dwFlags: flags, ..Default::default() } } }
    };
    press(erase, [key_input(VK_LSHIFT, false), insert(false), insert(true), key_input(VK_LSHIFT, true)]);
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

/// A paste is one edit that Ctrl+Z undoes. Typed text, and anything in a terminal, takes a Backspace per character.
fn erase_keys(what: Erase) -> Vec<INPUT> {
    match what {
        Erase::Pasted(_) if !terminal_in_front() => {
            vec![key_input(VK_LCONTROL, false), key_input(VK_Z, false), key_input(VK_Z, true), key_input(VK_LCONTROL, true)]
        }
        Erase::Typed(chars) | Erase::Pasted(chars) => {
            (0..chars).flat_map(|_| [key_input(VK_BACK, false), key_input(VK_BACK, true)]).collect()
        }
    }
}

/// The app being typed into, named as its captions would be, for its word rules.
pub fn focused_app() -> Option<String> {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid)) };
    root_of(pid, &snapshot()).map(|root| display_name(&root.exe))
}

fn terminal_in_front() -> bool {
    let mut class = [0u16; 64];
    let length = unsafe { GetClassNameW(GetForegroundWindow(), &mut class) }.max(0) as usize;
    TERMINALS.contains(&String::from_utf16_lossy(&class[..length]).as_str())
}

fn send(inputs: &[INPUT]) {
    unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
}

fn key_input(key: VIRTUAL_KEY, up: bool) -> INPUT {
    let flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: key, dwFlags: flags, ..Default::default() } } }
}

fn unicode(unit: u16, up: bool) -> INPUT {
    let flags = if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE };
    INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wScan: unit, dwFlags: flags, ..Default::default() } } }
}
