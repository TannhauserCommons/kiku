//! Running a macro's steps in the app in front: the [`Context`] that fills its templates and the
//! [`Executor`] that pastes, presses and types through the lent clipboard and the keyboard.

use std::{
    cell::{Cell, OnceCell, RefCell},
    env,
    ffi::OsString,
    fs,
    io::Read,
    os::windows::{io::AsRawHandle, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{atomic::Ordering, mpsc, Arc, Mutex, PoisonError},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use kiku_core::{
    actions::{clipboard_text, Private},
    captions::Captions,
    controls::Controls,
    keybinds::Key,
    log,
    macros::{local_now, read_text_file, Context, Executor, HowMuch, KikuRequest, KikuState, Mode, NaiveDateTime, OpenTarget, Transcript, TranscriptLine, Variables, Who},
    words::Output,
};
use windows::{
    core::{w, BOOL, HSTRING, PCWSTR},
    Win32::{
        Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, HWND, LPARAM},
        System::{
            DataExchange::GetClipboardSequenceNumber,
            Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ},
            JobObjects::{AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject},
        },
        UI::{
            Shell::ShellExecuteW,
            WindowsAndMessaging::{
                AllowSetForegroundWindow, EnumWindows, GetForegroundWindow, GetWindow, GetWindowTextLengthW, GetWindowTextW, IsIconic, IsWindowVisible,
                SetForegroundWindow, ShowWindow, ASFW_ANY, GW_OWNER, SW_RESTORE, SW_SHOWNORMAL,
            },
        },
    },
};

use super::{clipboard, give_back, keys, lend_clipboard, plain_error, share, Lent};
use crate::{
    processes::{snapshot, Process},
};

/// How long a copy of the selection is waited for, in ticks of `COPY_TICK`.
const COPY_TICKS: u32 = 30;
const COPY_TICK: Duration = Duration::from_millis(10);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The most of a program's output that is read.
const MAX_OUTPUT: u64 = 1 << 20;
const RUN_POLL: Duration = Duration::from_millis(20);
/// How long a program's output is waited for once it has ended: an app it started may still hold the pipe.
const OUTPUT_GRACE: Duration = Duration::from_millis(500);
const START_MENU: &str = r"Microsoft\Windows\Start Menu\Programs";
const APP_PATHS: &str = r"Software\Microsoft\Windows\CurrentVersion\App Paths";
/// How deep a Start menu folder is searched.
const START_MENU_DEPTH: usize = 4;

/// What a run left for the panel to do once it ends.
pub struct Outcome {
    /// Characters of the text pasted, which undo takes back; `None` for any other step.
    pub erase: Option<usize>,
    /// How long the next step waits at least, from a `wait` step.
    pub delay: Duration,
    /// Messages for the panel; never private text.
    pub notices: Vec<String>,
    /// A paste couldn't be made: the steps still waiting are dropped.
    pub failed: bool,
    /// Kiku's own actions the steps asked for, in order, for the panel to perform.
    pub actions: Vec<KikuRequest>,
}

/// One run of steps. The clipboard lent to its pastes is `lent`, which the panel keeps between runs.
pub struct MacroRun<'a> {
    hwnd: HWND,
    controls: &'a Controls,
    captions: &'a Mutex<Captions>,
    variables: Arc<Variables>,
    lent: &'a RefCell<Option<Lent>>,
    app: OnceCell<String>,
    transcript: OnceCell<Transcript>,
    erase: Cell<Option<usize>>,
    delay: Cell<Duration>,
    notices: RefCell<Vec<String>>,
    failed: Cell<bool>,
    actions: RefCell<Vec<KikuRequest>>,
}

impl<'a> MacroRun<'a> {
    pub fn new(hwnd: HWND, controls: &'a Controls, captions: &'a Mutex<Captions>, lent: &'a RefCell<Option<Lent>>) -> Self {
        Self {
            hwnd,
            controls,
            captions,
            variables: controls.macros.variables(),
            lent,
            app: OnceCell::new(),
            transcript: OnceCell::new(),
            erase: Cell::new(None),
            delay: Cell::new(Duration::ZERO),
            notices: RefCell::default(),
            failed: Cell::new(false),
            actions: RefCell::default(),
        }
    }

    pub fn finish(self) -> Outcome {
        Outcome { erase: self.erase.get(), delay: self.delay.get(), notices: self.notices.take(), failed: self.failed.get(), actions: self.actions.take() }
    }

    /// Lends the clipboard with `put`, then pastes with `paste`; a failure is told and stops the rest.
    fn lend(&self, put: impl FnOnce(HWND) -> windows::core::Result<()>, paste: impl FnOnce()) {
        match lend_clipboard(self.hwnd, &mut self.lent.borrow_mut(), put) {
            Ok(()) => paste(),
            Err(error) => {
                self.notices.borrow_mut().push(format!("Couldn't paste what you said: {}", plain_error(&error)));
                self.failed.set(true);
            }
        }
    }

    /// Presses Ctrl+C and waits for the app in front to copy; whether it did.
    fn copy_selection(&self) -> bool {
        let before = unsafe { GetClipboardSequenceNumber() };
        keys::press_keys(&ctrl(b'C'));
        await_copy(before, &|| unsafe { GetClipboardSequenceNumber() }, &thread::sleep)
    }

    /// Gives back what a paste borrowed, so the clipboard is the user's again.
    fn take_back_clipboard(&self) {
        give_back(self.hwnd, &mut self.lent.borrow_mut());
    }
}

fn ctrl(letter: u8) -> [Key; 2] {
    [Key::Ctrl, Key::Other(u16::from(letter))]
}

impl Context for MacroRun<'_> {
    fn variable(&self, name: &str) -> Option<Private> {
        self.variables.get(name)
    }

    fn now(&self) -> NaiveDateTime {
        local_now()
    }

    fn clipboard(&self) -> Private {
        self.take_back_clipboard();
        Private::new(clipboard::text(self.hwnd).unwrap_or_default())
    }

    /// The selection, by copying it with the clipboard's contents put back after.
    fn selection(&self) -> Private {
        self.take_back_clipboard();
        let saved = clipboard::contents(self.hwnd);
        let copied = self.copy_selection();
        let text = if copied { clipboard::text(self.hwnd).unwrap_or_default() } else { String::new() };
        if let Err(error) = clipboard::restore(self.hwnd, &saved) {
            log::write(format!("couldn't put the clipboard back: {error}"));
        }
        Private::new(text)
    }

    fn app(&self) -> String {
        self.app.get_or_init(|| keys::focused_app().unwrap_or_default()).clone()
    }

    fn window(&self) -> String {
        let mut title = [0u16; 256];
        let length = unsafe { GetWindowTextW(GetForegroundWindow(), &mut title) }.max(0) as usize;
        String::from_utf16_lossy(&title[..length])
    }

    fn resolve(&self, path: &Path) -> PathBuf {
        self.controls.macros.folder().join(path)
    }

    fn read_file(&self, path: &Path) -> Result<Private, String> {
        read_text_file(path)
    }

    /// The captions are read in memory, once per run.
    fn transcript(&self, who: &Who, how_much: &HowMuch) -> Vec<TranscriptLine> {
        let transcript = self.transcript.get_or_init(|| Transcript::from_captions(&self.captions.lock().unwrap()));
        transcript.select(who, how_much, &self.app(), local_now())
    }

    fn apply_rules(&self, text: &str) -> String {
        self.controls.words.apply(text, &self.app(), Output::Dictation)
    }

    fn pick(&self, choices: &[&str]) -> usize {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.subsec_nanos());
        nanos as usize % choices.len().max(1)
    }

    fn state(&self) -> KikuState {
        let macros = self.controls.macros.macros();
        let mode = macros.modes().effective(&self.app(), &self.controls.mode());
        KikuState {
            dictating: self.controls.dictating.load(Ordering::Relaxed),
            captions: !self.controls.stopped.load(Ordering::Relaxed),
            mode: mode.name().to_owned(),
        }
    }
}

impl Executor for MacroRun<'_> {
    fn paste_text(&self, text: &Private) {
        self.lend(|hwnd| clipboard::lend(hwnd, &clipboard_text(text.reveal())), || keys::paste(None));
        self.erase.set((!self.failed.get()).then(|| text.reveal().chars().count()));
    }

    fn paste_files(&self, files: &[PathBuf]) {
        self.lend(|hwnd| clipboard::lend_files(hwnd, files), || keys::press_keys(&ctrl(b'V')));
    }

    fn press(&self, chords: &[Vec<Key>]) {
        keys::press_chords(chords);
    }

    fn copy(&self, text: Option<&Private>) {
        self.take_back_clipboard();
        match text {
            None => {
                self.copy_selection();
            }
            Some(text) => {
                if let Err(error) = clipboard::copy_private(self.hwnd, &clipboard_text(text.reveal())) {
                    self.notices.borrow_mut().push(format!("Couldn't copy: {}", plain_error(&error)));
                }
            }
        }
    }

    fn type_text(&self, text: &Private) {
        keys::type_text(text.reveal());
    }

    /// Delays the next step instead of sleeping: the panel's timer runs it.
    fn wait(&self, how_long: Duration) {
        self.delay.set(self.delay.get() + how_long);
    }

    fn notify(&self, message: &str) {
        self.notices.borrow_mut().push(message.to_owned());
    }

    fn cancelled(&self) -> bool {
        self.failed.get()
    }

    fn open(&self, target: &OpenTarget) -> Result<(), String> {
        allow_foreground();
        let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
        match target {
            OpenTarget::Url(url) => shell_open(&HSTRING::from(url.reveal())),
            OpenTarget::Path(path) => shell_open(&HSTRING::from(path.as_os_str())),
            OpenTarget::App(name) => shell_open(&HSTRING::from(find_app(name).ok_or("couldn't find an app by that name")?.as_os_str())),
        }
    }

    fn switch_to(&self, app: &str) -> Result<(), String> {
        let window = newest_window_of(app).ok_or("there is no window of that app to switch to")?;
        allow_foreground();
        bring_to_front(window).then_some(()).ok_or_else(|| "Windows wouldn't bring that app to the front".into())
    }

    fn run_command(&self, program: &Path, args: &[Private], timeout: Duration) -> Result<Private, String> {
        run_program(program, args, timeout)
    }

    fn ask_claude(&self, text: &Private) -> Result<(), String> {
        self.take_back_clipboard();
        match share::send(self.hwnd, "text from a macro", text.reveal(), true) {
            Ok(share::Sent::Clipboard) => {
                self.notify("Too long to prefill: paste into Claude with Ctrl+V");
                Ok(())
            }
            Ok(_) => Ok(()),
            Err(error) => Err(format!("couldn't send to Claude: {error}")),
        }
    }

    fn set_mode(&self, mode: &Mode) {
        self.controls.set_mode(mode.clone());
    }

    fn kiku(&self, request: &KikuRequest) {
        self.actions.borrow_mut().push(request.clone());
    }
}

/// Opens `target` as a double-click would, with no parameters: a link, a file, a folder or a shortcut.
fn shell_open(target: &HSTRING) -> Result<(), String> {
    // ShellExecute returns a value above 32 on success.
    let opened = unsafe { ShellExecuteW(None, w!("open"), target, None, None, SW_SHOWNORMAL) };
    if opened.0 as isize > 32 { Ok(()) } else { Err("Windows couldn't open it".into()) }
}

/// What an app's name stands for: a shortcut of the Start menu or an App Paths entry of that name, and
/// nothing the shell would find by itself.
fn find_app(name: &str) -> Option<PathBuf> {
    let menus = ["APPDATA", "ProgramData"].into_iter().filter_map(env::var_os).map(|folder| PathBuf::from(folder).join(START_MENU));
    let shortcut = menus.into_iter().find_map(|menu| find_shortcut(&menu, name, START_MENU_DEPTH));
    shortcut.or_else(|| [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE].into_iter().find_map(|hive| app_path(hive, name)))
}

/// The `.lnk` in `folder` or below it whose file name is `name`, any case.
fn find_shortcut(folder: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = fs::read_dir(folder).ok()?.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect();
    let named = |path: &&PathBuf| {
        path.is_file() && path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("lnk")) && path.file_stem().is_some_and(|stem| stem.eq_ignore_ascii_case(name))
    };
    entries.iter().find(named).cloned().or_else(|| {
        let below = depth.checked_sub(1)?;
        entries.iter().filter(|path| path.is_dir()).find_map(|folder| find_shortcut(folder, name, below))
    })
}

/// The program App Paths lists for `<name>.exe` in `hive`, if it is there.
fn app_path(hive: HKEY, name: &str) -> Option<PathBuf> {
    let key = HSTRING::from(format!(r"{APP_PATHS}\{name}.exe"));
    let mut buffer = [0u16; 1024];
    let mut bytes = size_of_val(&buffer) as u32;
    let read = unsafe { RegGetValueW(hive, &key, PCWSTR::null(), RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ, None, Some(buffer.as_mut_ptr().cast()), Some(&mut bytes)) };
    if read != ERROR_SUCCESS {
        return None;
    }
    let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
    let path = PathBuf::from(String::from_utf16_lossy(&buffer[..length]).trim_matches('"'));
    path.is_file().then_some(path)
}

/// The topmost visible window with a title and no owner whose app is `app` (as captions name apps).
fn newest_window_of(app: &str) -> Option<HWND> {
    struct Search<'a> {
        app: &'a str,
        processes: Vec<Process>,
        found: Option<HWND>,
    }
    unsafe extern "system" fn visit(window: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `Search` `newest_window_of` lent to `EnumWindows`, alive for the call.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let is_main = unsafe { IsWindowVisible(window).as_bool() && GetWindow(window, GW_OWNER).is_err() && GetWindowTextLengthW(window) > 0 };
        if is_main && keys::app_of(window, &search.processes).is_some_and(|name| name.eq_ignore_ascii_case(search.app)) {
            search.found = Some(window);
        }
        BOOL::from(search.found.is_none())
    }
    let mut search = Search { app, processes: snapshot(), found: None };
    // EnumWindows reports a stop as a failure, which is how a find ends it.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize)) };
    search.found
}

/// Makes Kiku the last to have had input, which Windows requires before a program may take the foreground
/// (Kiku's keyboard hook doesn't count).
fn allow_foreground() {
    keys::tap_mask_key();
}

/// Brings `window` to the front, restoring it if minimised; whether it was taken.
fn bring_to_front(window: HWND) -> bool {
    unsafe {
        if IsIconic(window).as_bool() {
            let _ = ShowWindow(window, SW_RESTORE);
        }
        SetForegroundWindow(window).as_bool()
    }
}

/// How a program is started: what runs, with which arguments, and the process creation flags (a hidden
/// window, no console).
#[derive(Debug, PartialEq)]
pub struct Launch {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    pub flags: u32,
}

/// How `program` (in `custom\scripts\`, or anywhere if allowed) is started with `args`, each one argument:
/// an `.exe` itself, `.bat`/`.cmd` by Rust's `Command`, which escapes arguments for `cmd`, `.ps1` through
/// PowerShell, `.py` through `py`.
pub fn launch(program: &Path, args: &[Private]) -> Result<Launch, String> {
    let args = args.iter().map(|arg| OsString::from(arg.reveal()));
    let extension = program.extension().map(|ext| ext.to_string_lossy().to_lowercase()).unwrap_or_default();
    let (exe, first): (&Path, Vec<OsString>) = match extension.as_str() {
        "exe" | "bat" | "cmd" => (program, vec![]),
        "ps1" => (Path::new("powershell.exe"), ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"].map(OsString::from).into_iter().chain([program.into()]).collect()),
        "py" => (Path::new("py"), vec![program.into()]),
        _ => return Err("macros can only run .exe, .bat, .cmd, .ps1 and .py files".into()),
    };
    Ok(Launch { exe: exe.to_owned(), args: first.into_iter().chain(args).collect(), flags: CREATE_NO_WINDOW })
}

/// Runs `program` with `args` in a hidden window and waits for it; at `timeout` it is ended with what it
/// started, but after a normal exit what it started keeps running. What it printed, at most 1 MiB, or why
/// it failed.
pub fn run_program(program: &Path, args: &[Private], timeout: Duration) -> Result<Private, String> {
    let launch = launch(program, args)?;
    if !program.is_file() {
        return Err("the program wasn't found".into());
    }
    let mut child = Command::new(&launch.exe)
        .args(&launch.args)
        .creation_flags(launch.flags)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "the program couldn't be started")?;
    // SAFETY: plain Win32 calls on a new job and the child's own handle; no job just means only the child is ended.
    let job = unsafe { CreateJobObjectW(None, None) }.ok();
    if let Some(job) = job {
        unsafe { AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())) }.ok();
    }
    let stdout = child.stdout.take().ok_or("the program's output couldn't be read")?;
    let output = Arc::new(Mutex::new(Vec::new()));
    let (reader_output, (finished, ended_reading)) = (Arc::clone(&output), mpsc::channel());
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        let mut stdout = stdout.take(MAX_OUTPUT);
        while let Ok(read @ 1..) = stdout.read(&mut chunk) {
            reader_output.lock().unwrap_or_else(PoisonError::into_inner).extend_from_slice(&chunk[..read]);
        }
        let _ = finished.send(());
    });
    let started = Instant::now();
    let ended = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < timeout => thread::sleep(RUN_POLL),
            Ok(None) => break Err(format!("the program timed out after {} s and was ended", timeout.as_secs_f32().ceil())),
            Err(_) => break Err("the program couldn't be waited for".into()),
        }
    };
    if ended.is_err() {
        let _ = child.kill();
        if let Some(job) = job {
            let _ = unsafe { TerminateJobObject(job, 1) };
        }
        let _ = child.wait();
    }
    if let Some(job) = job {
        let _ = unsafe { CloseHandle(job) };
    }
    let _ = ended_reading.recv_timeout(OUTPUT_GRACE);
    let output = output.lock().unwrap_or_else(PoisonError::into_inner).clone();
    match ended?.code() {
        Some(0) => Ok(Private::new(String::from_utf8_lossy(&output).into_owned())),
        Some(code) => Err(format!("the program exited with code {code}")),
        None => Err("the program was ended".into()),
    }
}

/// Waits for the app in front to copy, polling the clipboard's sequence number (`sequence`) against
/// `before` and sleeping a tick between; whether it copied in time.
pub fn await_copy(before: u32, sequence: &dyn Fn() -> u32, sleep: &dyn Fn(Duration)) -> bool {
    for _ in 0..COPY_TICKS {
        let copied = sequence() != before;
        sleep(COPY_TICK);
        if copied {
            return true;
        }
    }
    false
}
