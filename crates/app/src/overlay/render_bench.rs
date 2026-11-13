//! Frames drawn off-screen with long transcripts, in captions mode and in the settings window: that drawing only what changed
//! gives the same pixels as drawing everything, and what a frame costs, pushed to a layered window that
//! is never shown (run with `cargo test --release -p kiku render_cost -- --ignored --nocapture`).

use std::{fs, path::PathBuf, time::{Duration, Instant}};

use windows::{
    core::w,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::{GetCurrentObject, GetDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, OBJ_BITMAP},
        System::{
            Com::{CoInitializeEx, COINIT_APARTMENTTHREADED},
            LibraryLoader::GetModuleHandleW,
            Threading::{GetCurrentProcess, GetCurrentThread},
            WindowsProgramming::{QueryProcessCycleTime, QueryThreadCycleTime},
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, RegisterClassW, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
        },
    },
};

use super::{
    dictation_card::DictationCard,
    finder::{Found, TextBox, Who},
    macro_tab::{MacroAction, MacroBox, MacroTab, StepKind},
    palette::Palette,
    render::{Frame, Icon, Meter, Panel, Renderer, Speaker, SubtitleLine, SubtitleScene, Surface},
    usage::Usage,
    view::{Action, BlockPrompt, Correction, Dropdown, ImportStatus, LineRef, Screen, Scroll, ScrollArea, SetupStep, Tab, Target, View, WordBox},
};
use kiku_core::{
    captions::Captions,
    dictation::{DictationKey, DictationSettings},
    controls::{Blocked, Input, ModelState},
    engine::{Device, Line},
    history::{Session, StoredLine, Table},
    keybinds::{Key, Keybinds},
    macros::{CommandPolicy, KikuAction, Macros, Mode, Variables},
    catalog::{self, Choice},
    download::Downloads,
    hardware::{Gpu, Hardware},
    settings::{BlockAnswer, EngineWhere, LabelStyle, Labels, Opacity, YourLines},
    voices::book::{Place, VoiceInfo, VoiceScope},
    subtitles::SubtitleSettings,
    theme::{Theme, Themes},
    update::Release,
    words::{WordSettings, Words},
};

const WORDS: [&str; 16] =
    ["so", "the", "meeting", "moved", "to", "Thursday", "because", "everyone", "was", "travelling", "and", "we", "couldn't", "agree", "on", "anything"];

/// Ten thousand lines from an app and ten thousand from you, so both sections are full.
fn long_captions() -> Captions {
    let (mut captions, now) = (Captions::default(), Instant::now());
    let app = captions.add_app("Discord", None);
    for id in 0..10_000u64 {
        captions.update_app(app, 1, &[line(id, id as usize)], now);
        captions.update_mic(2, &[line(id, id as usize + 7)], Some("Discord"));
    }
    captions
}

fn line(id: u64, seed: usize) -> Line {
    let count = 4 + seed * 7919 % 36;
    let text = (0..count).map(|i| WORDS[(seed + i * 31) % WORDS.len()]).collect::<Vec<_>>().join(" ");
    Line { id, text, complete: true, voice: None }
}

/// CPU cycles this thread has run, which unlike its CPU time isn't counted in 15.6 ms ticks.
fn cycles() -> u64 {
    let mut cycles = 0;
    unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles).unwrap() };
    cycles
}

/// CPU cycles every thread of this process has run, e.g. Direct2D's own.
fn process_cycles() -> u64 {
    let mut cycles = 0;
    unsafe { QueryProcessCycleTime(GetCurrentProcess(), &mut cycles).unwrap() };
    cycles
}

/// How many cycles this thread runs per second while it never waits.
fn cycles_per_second() -> f64 {
    let (start, wall) = (cycles(), Instant::now());
    while wall.elapsed() < Duration::from_millis(200) {}
    (cycles() - start) as f64 / wall.elapsed().as_secs_f64()
}

/// Prints the average wall time, this thread's CPU time and the whole process's per call of `frame`.
fn measure(name: &str, frames: u32, mut frame: impl FnMut(u32)) {
    let rate = cycles_per_second();
    let (wall, thread, process) = (Instant::now(), cycles(), process_cycles());
    for index in 0..frames {
        frame(index);
    }
    let per = |seconds: f64| seconds * 1000.0 / frames as f64;
    let (thread, process) = ((cycles() - thread) as f64 / rate, (process_cycles() - process) as f64 / rate);
    println!("{name:<36} wall {:>7.3} ms   thread {:>7.3} ms   process {:>7.3} ms", per(wall.elapsed().as_secs_f64()), per(thread), per(process));
}

/// A layered window like the panel's, never shown, so frames can be pushed to it without anyone seeing.
fn hidden_window() -> HWND {
    extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }
    let instance = unsafe { GetModuleHandleW(None).unwrap() };
    let class = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance.into(), lpszClassName: w!("KikuBench"), ..Default::default() };
    unsafe {
        RegisterClassW(&class);
        let style = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        CreateWindowExW(style, w!("KikuBench"), w!(""), WS_POPUP, 0, 0, 100, 100, None, None, Some(instance.into()), None).unwrap()
    }
}

static SUBTITLES: std::sync::LazyLock<SubtitleSettings> = std::sync::LazyLock::new(SubtitleSettings::default);
static HARDWARE: std::sync::LazyLock<Hardware> = std::sync::LazyLock::new(Hardware::default);
static DOWNLOADS: std::sync::LazyLock<Downloads> = std::sync::LazyLock::new(|| Downloads::start(|| {}));
static WORD_RULES: std::sync::LazyLock<Words> = std::sync::LazyLock::new(|| Words::parse(SNAPSHOT_WORDS));
static MACROS: std::sync::LazyLock<Macros> = std::sync::LazyLock::new(|| Macros::parse("", &WORD_RULES, &Variables::default()));
/// A macros file with a macro of two phrases, a shortcut and one turned off with three steps.
const SNAPSHOT_MACROS: &str = "[[macro]]\nname = \"Paste my email\"\nsay = [\"paste my email\", \"my email\"]\nsteps = [ { paste = \"me@example.com\" } ]\n\n[[macro]]\nname = \"New tab\"\nsay = \"open a tab\"\nsteps = [ { press = \"ctrl+t\" } ]\n\n[[macro]]\nname = \"Sign off\"\nsay = \"sign off\"\nhotkey = \"Ctrl+Alt+S\"\nlisten = true\nenabled = false\nsteps = [ { paste = \"Thanks,\\nTannhauser\" }, { wait = 200 }, { notify = \"Signed off\" } ]\n";
/// More macros for the snapshots: one that runs a program, asks Claude and changes mode, and one with a shortcut only.
const SNAPSHOT_MORE_MACROS: &str = "\n[[macro]]\nname = \"Search the web\"\nsay = \"search for {words}\"\nsteps = [ { run = \"search.ps1\", args = [\"{words}\", \"--quiet\"], output = \"paste\" }, { ask_claude = \"Summarise {clipboard}\" }, { mode = \"commands\" } ]\n\n[[macro]]\nname = \"Reopen tab\"\nhotkey = \"Ctrl+Alt+R\"\nsteps = [ { press = \"ctrl+shift+t\" } ]\n";
/// What the Dictating badge's status line says in the snapshots.
const DICTATING_HINT: &str = "Ctrl+Win or \"that's all Kiku\" stops";

/// A word rules file with an entry of each kind (two actions everywhere, one in Discord), sections and a line it skips.
const SNAPSHOT_WORDS: &str = "# mine\nkicku => Kiku\num =>\n@ Tannhauser\n@ Lull :2.8\n! capitalise\n> select all => press ctrl+a\n> paste email => paste me@example.com\nnot a rule\n[Discord]\ngg => good game\n> paste address => paste 1 Main St\\n2nd floor\n! no-trailing-punctuation\n";

pub(super) fn panel<'a>(captions: &'a Captions, view: &'a View, keybinds: &'a Keybinds, width: f32) -> Panel<'a> {
    let choice = Choice { model: catalog::by_id_or_default(catalog::DEFAULT_ID), device: Device::Cpu };
    let meter = |input| Meter { input, threshold_db: -50.0, level_db: -30.0 };
    Panel {
        captions,
        view,
        stopped: false,
        dictating: false,
        latched: false,
        stop_hint: "",
        dictation_key: DictationKey::Hold,
        keybinds,
        captured: &[],
        model: ModelState { active: choice, pending: None, failed: None, failure: None, loaded: true, gpu_fallback: None, remote: None },
        hardware: &HARDWARE,
        downloads: &DOWNLOADS,
        import: &ImportStatus::Idle,
        auto_save: true,
        save_mic_only: false,
        keep_days: 30,
        check_updates: true,
        voices_apps: true,
        voices_mic: false,
        speaker_model: true,
        auto_fold: 2,
        blocked: &[],
        block_answer: BlockAnswer::Ask,
        your_lines: YourLines::default(),
        labels: Labels::default(),
        recognise_across_apps: false,
        saved_voices: Vec::new(),
        themes: Themes::default(),
        dictation_only: false,
        engine_where: EngineWhere::ThisPc,
        host_on: false,
        hosted_code: None,
        engine_host: String::new(),
        shown_opacity: Opacity::default(),
        opacity: Opacity::default(),
        locked_opacity: Opacity::default(),
        subtitles: &SUBTITLES,
        words: &WORD_RULES,
        macros: &MACROS,
        word_settings: WordSettings::default(),
        settings_open: false,
        usage: None,
        meters: [meter(Input::Mic), meter(Input::Computer)],
        width,
        height: 420.0,
        dpi: 144.0,
        now: Instant::now(),
        mode: Mode::Normal,
    }
}

#[test]
#[ignore = "benchmark"]
fn render_cost() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let window = hidden_window();
    let (mut renderer, mut surface) = (Renderer::new().unwrap(), Surface::default());
    let mut captions = long_captions();
    let keybinds = Keybinds::default();
    let mut view = View::new();
    let mut draw = |panel: Panel| {
        let (frame, _) = renderer.render(&mut surface, &panel).unwrap();
        frame.present(window, POINT::default(), u8::MAX).unwrap();
    };
    let usage = |frame: u32| Some(Usage { cpu_percent: frame as f32 / 10.0, gpu_percent: 0.0, gpu_memory_bytes: 0 });

    measure("live: first frame", 1, |_| draw(panel(&captions, &view, &keybinds, 560.0)));
    measure("live: nothing changed", 200, |_| draw(panel(&captions, &view, &keybinds, 560.0)));
    measure("live: footer figures changed", 200, |i| draw(Panel { usage: usage(i), ..panel(&captions, &view, &keybinds, 560.0) }));
    measure("live: last line revised", 200, |i| {
        captions.update_mic(2, &[Line { id: 9_999, text: format!("still talking {i}"), complete: false, voice: None }], Some("Discord"));
        draw(panel(&captions, &view, &keybinds, 560.0));
    });
    measure("live: a new line (oldest dropped)", 200, |i| {
        captions.update_mic(2, &[line(20_000 + i as u64, i as usize)], Some("Discord"));
        draw(panel(&captions, &view, &keybinds, 560.0));
    });
    measure("live: resized each frame", 50, |i| draw(panel(&captions, &view, &keybinds, 560.0 + (i % 2) as f32 * 40.0)));
    let palette_macros = Macros::parse(&many_macros(200), &WORD_RULES, &Variables::default());
    view.palette = Some(Palette::open(&palette_macros, "Discord", &Mode::Normal));
    measure("live: macro palette open, 200 macros", 50, |_| draw(panel(&captions, &view, &keybinds, 560.0)));
    view.palette = None;
    measure("live: Dictating badge showing", 200, |_| draw(Panel { dictating: true, latched: true, stop_hint: DICTATING_HINT, ..panel(&captions, &view, &keybinds, 560.0) }));

    let lines = (0..10_000).map(|i| format!("[14:{:02}] Discord: {}", i % 60, line(i, i as usize).text)).collect();
    view.screen = Screen::SavedFile { session: 1, title: "Discord  ·  Oct 30, 14:05".into(), lines, focus: None };
    measure("history: first frame", 1, |_| draw(panel(&captions, &view, &keybinds, 560.0)));
    measure("history: nothing changed", 50, |_| draw(panel(&captions, &view, &keybinds, 560.0)));
    measure("history: resized each frame", 50, |i| draw(panel(&captions, &view, &keybinds, 560.0 + (i % 2) as f32 * 40.0)));

    let (mut settings, mut surface) = (View::new(), Surface::default());
    settings.tab = Tab::Listening;
    let mut draw = |panel: Panel| {
        let (frame, _) = renderer.render_settings(&mut surface, &settings_window(panel)).unwrap();
        frame.present(window, POINT::default(), u8::MAX).unwrap();
    };
    measure("settings: nothing changed", 200, |_| draw(panel(&captions, &settings, &keybinds, 560.0)));
    measure("settings: meters moved", 200, |i| {
        let meter = |input| Meter { input, threshold_db: -50.0, level_db: -60.0 + (i % 40) as f32 };
        draw(Panel { meters: [meter(Input::Mic), meter(Input::Computer)], ..panel(&captions, &settings, &keybinds, 560.0) });
    });

    // The Macros tab beside the Words tab, each with 500 entries.
    let many_words = Words::parse(&(0..500).map(|i| format!("word{i} => replacement {i}\n")).collect::<String>());
    settings.tab = Tab::Words;
    measure("settings: words tab, 500 entries", 50, |_| draw(Panel { words: &many_words, ..panel(&captions, &settings, &keybinds, 560.0) }));
    let folder = macros_folder(&many_macros(500));
    (settings.tab, settings.macros) = (Tab::Macros, MacroTab::open(&folder));
    measure("settings: macros tab, 500 macros", 50, |_| draw(panel(&captions, &settings, &keybinds, 560.0)));
    let _ = fs::remove_dir_all(folder);
    let programs = (0..50).map(|i| format!("[[macro]]\nname = \"Program {i}\"\nsay = \"run program {i}\"\nsteps = [ {{ run = \"tool{i}.ps1\", args = [\"{{words}}\"] }} ]\n\n")).collect::<String>();
    let folder = macros_folder(&(many_macros(500) + &programs));
    (settings.tab, settings.macros) = (Tab::Macros, MacroTab::open(&folder));
    settings.macros.set_policy(CommandPolicy { enabled: true, anywhere: true });
    measure("settings: macros tab, 50 commands to approve", 50, |_| draw(panel(&captions, &settings, &keybinds, 560.0)));
    let _ = fs::remove_dir_all(folder);

    settings.tab = Tab::General;
    measure("settings: general tab with the Dictation card", 200, |_| draw(panel(&captions, &settings, &keybinds, 560.0)));

    let (look, mut surface) = (SubtitleSettings::default().look(&Themes::default()), Surface::default());
    let mut draw = |text: &str| {
        let first = line(1, 1);
        let lines = [SubtitleLine::new(&first.text, Speaker::App(None)), SubtitleLine::new(text, Speaker::You)];
        let scene = subtitles(&renderer, &lines, &look, 92, 144.0);
        renderer.render_subtitles(&mut surface, &scene).unwrap().0.present(window, POINT::default(), u8::MAX).unwrap();
    };
    measure("captions mode: first frame", 1, |_| draw("so the meeting"));
    measure("captions mode: nothing changed", 200, |_| draw("so the meeting"));
    measure("captions mode: last line grew a word", 200, |i| draw(&format!("so the meeting moved {i}")));
}

/// A `macros.toml` of `count` macros of two steps each.
fn many_macros(count: usize) -> String {
    (0..count).map(|i| format!("[[macro]]\nname = \"Macro {i}\"\nsay = \"run macro {i}\"\nsteps = [ {{ paste = \"text {i}\" }}, {{ press = \"ctrl+a\" }} ]\n\n")).collect()
}

/// A temporary folder holding a `macros.toml` of `text`.
fn macros_folder(text: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("kiku-macros-bench-{}-{}", std::process::id(), text.len()));
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("macros.toml"), text).unwrap();
    folder
}

/// `panel` sized as the settings window.
pub(super) fn settings_window(panel: Panel) -> Panel {
    Panel { width: super::settings::SIZE.0, height: super::settings::SIZE.1, ..panel }
}

/// Captions mode two-thirds of a 1920-pixel screen wide at 144 DPI, showing `lines` in Night.
fn subtitles<'a>(renderer: &Renderer, lines: &'a [SubtitleLine<'a>], look: &'a kiku_core::subtitles::Look, opacity: u8, dpi: f32) -> SubtitleScene<'a> {
    let height = renderer.subtitle_height(look, 2).unwrap();
    SubtitleScene { lines, labels: Labels::default(), look, opacity, themes: Themes::default(), rows: 2, width: 853.0, height, dpi }
}

/// The bitmap selected into `dc`, as top-down BGRA pixels.
fn pixels(dc: HDC, width: i32, height: i32) -> Vec<u32> {
    let header = BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: width, biHeight: -height, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() };
    let mut info = BITMAPINFO { bmiHeader: header, ..Default::default() };
    let mut pixels = vec![0u32; (width * height) as usize];
    unsafe {
        let bitmap = HBITMAP(GetCurrentObject(dc, OBJ_BITMAP).0);
        GetDIBits(dc, bitmap, 0, height as u32, Some(pixels.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
    }
    pixels
}

/// Whether two frames match. A gradient can round a channel one level differently depending on where
/// the band being drawn starts, and a button's plate on a card on the window stacks two of them, which
/// no one can see; anything drawn wrongly differs by more.
fn same_pixels(a: &[u32], b: &[u32]) -> bool {
    let close = |a: u32, b: u32| (0..4).all(|byte| ((a >> (8 * byte)) as u8).abs_diff((b >> (8 * byte)) as u8) <= 2);
    a.len() == b.len() && a.iter().zip(b).all(|(&a, &b)| close(a, b))
}

#[test]
fn drawing_only_what_changed_matches_drawing_everything() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let keybinds = Keybinds::default();
    let usage = |cpu_percent| Some(Usage { cpu_percent, gpu_percent: 0.0, gpu_memory_bytes: 0 });
    for dpi in [96.0, 120.0, 144.0, 168.0] {
        let (mut captions, mut view, mut partly, mut surface) = (long_captions(), View::new(), Renderer::new().unwrap(), Surface::default());
        for step in 0..6 {
            match step {
                1 => view.hovered_line = Some(LineRef { section: 1, stream: 1, id: 9_999 }),
                2 => {
                    let text = "a revised last line, long enough to wrap onto a second row of the panel".into();
                    captions.update_mic(2, &[Line { id: 9_999, text, complete: false, voice: None }], Some("Discord"));
                }
                3 => captions.update_app(1, 1, &[line(10_000, 3)], Instant::now()),
                4 => view.hovered_line = None,
                _ => {}
            }
            let panel = Panel { usage: usage(step as f32), dpi, ..panel(&captions, &view, &keybinds, 560.0) };
            let (frame, _) = partly.render(&mut surface, &panel).unwrap();
            let partial = pixels(frame.dc, frame.width, frame.height);
            let (mut wholly, mut fresh) = (Renderer::new().unwrap(), Surface::default());
            let (frame, _) = wholly.render(&mut fresh, &panel).unwrap();
            assert!(same_pixels(&partial, &pixels(frame.dc, frame.width, frame.height)), "step {step} at {dpi} DPI left different pixels");
        }
    }
}

/// Folding changes how many rows each section measures, which with 10,000 lines can move rows by a
/// fraction of a pixel between renderers, so these steps use a shorter transcript.
#[test]
fn folding_hiding_and_prompting_drawn_only_where_changed_match_drawing_everything() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let keybinds = Keybinds::default();
    let (mut captions, now) = (Captions::default(), Instant::now());
    let app = captions.add_app("Discord", None);
    for id in 0..60u64 {
        captions.update_app(app, 1, &[line(id, id as usize)], now);
        captions.update_mic(2, &[line(id, id as usize + 7)], Some("Discord"));
    }
    for dpi in [96.0, 120.0, 144.0, 168.0] {
        let (mut view, mut partly, mut surface) = (View::new(), Renderer::new().unwrap(), Surface::default());
        for step in 0..7 {
            match step {
                1 => view.hovered_header = Some(app),
                2 => view.folds.toggle(app, false),
                3 => view.blocking = Some(BlockPrompt { section: app, dont_ask: true }),
                4 => (view.blocking, view.folds) = (None, Default::default()),
                5 => view.folds.hide(app),
                6 => view.folds.show_hidden(),
                _ => {}
            }
            let panel = Panel { dpi, ..panel(&captions, &view, &keybinds, 560.0) };
            let (frame, _) = partly.render(&mut surface, &panel).unwrap();
            let partial = pixels(frame.dc, frame.width, frame.height);
            let (mut wholly, mut fresh) = (Renderer::new().unwrap(), Surface::default());
            let (frame, _) = wholly.render(&mut fresh, &panel).unwrap();
            assert!(same_pixels(&partial, &pixels(frame.dc, frame.width, frame.height)), "step {step} at {dpi} DPI left different pixels");
        }
    }
}

#[test]
fn a_folded_app_is_one_row_and_its_space_goes_to_the_open_ones() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds, mut view) = (long_captions(), Keybinds::default(), View::new());
    let (mut renderer, mut surface) = (Renderer::new().unwrap(), Surface::default());
    let you_viewport = |regions: &super::view::Regions| {
        regions.scroll_areas.iter().find(|(_, area, _)| *area == ScrollArea::Section(0)).map(|(_, _, extent)| extent.viewport).unwrap()
    };
    let (_, open) = renderer.render(&mut surface, &panel(&captions, &view, &keybinds, 560.0)).unwrap();
    assert_eq!(open.sections, [1, 0]);

    view.folds.toggle(1, false);
    let (_, folded) = renderer.render(&mut surface, &panel(&captions, &view, &keybinds, 560.0)).unwrap();
    assert_eq!(folded.sections, [0], "a folded app's lines aren't shown or selectable");
    assert!(folded.scroll_areas.iter().all(|(_, area, _)| *area != ScrollArea::Section(1)));
    assert!(you_viewport(&folded) > you_viewport(&open) + 100.0, "the folded app's space went to you");
    assert!(folded.targets.iter().any(|(_, target)| *target == Target::Click(Action::ToggleFold(1))), "the chevron opens it again");
}

#[test]
fn header_buttons_show_only_while_the_header_is_hovered_and_you_cannot_be_hidden() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds, mut view) = (long_captions(), Keybinds::default(), View::new());
    let (mut renderer, mut surface) = (Renderer::new().unwrap(), Surface::default());
    let has = |regions: &super::view::Regions, action| regions.targets.iter().any(|(_, target)| *target == Target::Click(action));
    let (_, regions) = renderer.render(&mut surface, &panel(&captions, &view, &keybinds, 560.0)).unwrap();
    assert!(has(&regions, Action::ToggleFold(1)) && !has(&regions, Action::CopySection(1)) && !has(&regions, Action::Hide(1)));

    let header = regions.targets.iter().find(|(_, target)| *target == Target::Header(1)).unwrap().0;
    view.hovered_header = regions.header_at(header.right - 4.0, header.top + 4.0);
    let (_, regions) = renderer.render(&mut surface, &panel(&captions, &view, &keybinds, 560.0)).unwrap();
    assert!(has(&regions, Action::CopySection(1)) && has(&regions, Action::Hide(1)));

    view.hovered_header = Some(0);
    let (_, regions) = renderer.render(&mut surface, &panel(&captions, &view, &keybinds, 560.0)).unwrap();
    assert!(has(&regions, Action::CopySection(0)) && !has(&regions, Action::Hide(0)) && !has(&regions, Action::Block(0)));
}

#[test]
fn a_blocked_app_leaves_the_panel_and_the_prompt_keeps_the_captions_from_being_clicked() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds, mut view) = (long_captions(), Keybinds::default(), View::new());
    let (mut renderer, mut surface) = (Renderer::new().unwrap(), Surface::default());
    let blocked = [Blocked { name: "Discord".into(), always: false }];
    let (_, regions) = renderer.render(&mut surface, &Panel { blocked: &blocked, ..panel(&captions, &view, &keybinds, 560.0) }).unwrap();
    assert_eq!(regions.sections, [0]);
    assert!(regions.targets.iter().all(|(_, target)| *target != Target::Header(1)));

    view.blocking = Some(BlockPrompt { section: 1, dont_ask: false });
    let (_, regions) = renderer.render(&mut surface, &panel(&captions, &view, &keybinds, 560.0)).unwrap();
    for action in [Action::BlockFor(false), Action::BlockFor(true), Action::CancelBlock, Action::DontAskToBlock] {
        assert!(regions.targets.iter().any(|(_, target)| *target == Target::Click(action)), "{action:?}");
    }
    let caption = regions.captions.first().unwrap().visible;
    assert_eq!(regions.target_at(caption.left + 1.0, caption.top + 1.0), Some(Target::Card));
}

#[test]
fn the_apps_tab_lists_blocked_apps_to_unblock() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds, mut view) = (Captions::default(), Keybinds::default(), View::new());
    let (mut renderer, mut surface) = (Renderer::new().unwrap(), Surface::default());
    view.tab = Tab::Apps;
    let blocked = [Blocked { name: "Discord".into(), always: true }, Blocked { name: "Opera".into(), always: false }];
    let settings = settings_window(Panel { blocked: &blocked, ..panel(&captions, &view, &keybinds, 560.0) });
    let (_, regions) = renderer.render_settings(&mut surface, &settings).unwrap();
    let unblock: Vec<Target> = regions.targets.iter().map(|(_, target)| *target).filter(|t| matches!(t, Target::Click(Action::Unblock(_)))).collect();
    assert_eq!(unblock, [Target::Click(Action::Unblock(0)), Target::Click(Action::Unblock(1))]);
}

#[test]
fn the_apps_tab_has_a_switch_for_speakers_in_apps_and_at_the_mic() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds, mut view) = (Captions::default(), Keybinds::default(), View::new());
    let (mut renderer, mut surface) = (Renderer::new().unwrap(), Surface::default());
    view.tab = Tab::Apps;
    let settings = settings_window(Panel { voices_mic: true, ..panel(&captions, &view, &keybinds, 560.0) });
    let (_, regions) = renderer.render_settings(&mut surface, &settings).unwrap();
    let clicks: Vec<Target> = regions.targets.iter().map(|(_, target)| *target).filter(|t| matches!(t, Target::Click(Action::SetVoices(..)))).collect();
    assert_eq!(clicks, [Target::Click(Action::SetVoices(false, false)), Target::Click(Action::SetVoices(true, false))]);
}

#[test]
fn every_label_style_drawn_only_where_changed_matches_drawing_everything() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let keybinds = Keybinds::default();
    for style in LabelStyle::ALL {
        let (mut captions, now) = (speaker_captions(), Instant::now());
        // An icon is loaded as it is first wanted, which frames drawn at different moments would show differently.
        captions.sections.iter_mut().for_each(|section| section.exe_path = None);
        let (mut view, mut partly, mut surface) = (View::new(), Renderer::new().unwrap(), Surface::default());
        for step in 0..6 {
            match step {
                1 => captions.update_app(1, 1, &[Line { id: 9, text: "A third voice joins and says enough to wrap onto a second row of text in the panel.".into(), complete: true, voice: Some(3) }], now),
                2 => captions.set_names(1, &Place::App("Discord".into()), [(1, "Maya".to_owned())].into()),
                3 => view.hovered_line = captions.sections[1].captions.get(1).map(|caption| LineRef::of(1, caption)),
                4 => {
                    view.naming = captions.sections[1].captions.get(2).map(|caption| LineRef::of(1, caption));
                    view.finder.typing = Some(TextBox::Name);
                    view.name.set("Ma");
                }
                5 => captions.update_app(1, 1, &[Line { id: 9, text: "A third voice joins and says enough to wrap onto a second row of text in the panel, revised.".into(), complete: true, voice: Some(3) }], now),
                _ => {}
            }
            let panel = Panel { labels: Labels { you: style, speakers: style }, height: 1000.0, ..panel(&captions, &view, &keybinds, 560.0) };
            let (frame, _) = partly.render(&mut surface, &panel).unwrap();
            let partial = pixels(frame.dc, frame.width, frame.height);
            let (mut wholly, mut fresh) = (Renderer::new().unwrap(), Surface::default());
            let (frame, _) = wholly.render(&mut fresh, &panel).unwrap();
            let whole = pixels(frame.dc, frame.width, frame.height);
            let first = partial.iter().zip(&whole).position(|(a, b)| !same_pixels(&[*a], &[*b]));
            assert!(first.is_none(), "step {step} in {style:?} left different pixels, first at {:?} of width {}", first.map(|at| (at as i32 % frame.width, at as i32 / frame.width)), frame.width);
        }
    }
}

#[test]
fn a_trimmed_section_whose_first_line_joins_a_turn_draws_like_a_fresh_one() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let keybinds = Keybinds::default();
    for style in [LabelStyle::Blocks, LabelStyle::Turns, LabelStyle::Badges] {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("Discord", None);
        let said = |id: u64, voice| Line { id, text: format!("line {id} of a long call"), complete: true, voice: Some(voice) };
        let lines: Vec<Line> = (0..10_000).map(|id| said(id, u32::from(id >= 9_990))).collect();
        captions.update_app(app, 1, &lines, now);
        let (mut view, mut partly, mut surface) = (View::new(), Renderer::new().unwrap(), Surface::default());
        view.scrolls.insert(ScrollArea::Section(app), Scroll::at(0.0));
        for step in 0..2 {
            if step == 1 {
                captions.update_app(app, 1, &[said(10_000, 1)], now);
            }
            let panel = Panel { labels: Labels { you: style, speakers: style }, height: 640.0, ..panel(&captions, &view, &keybinds, 560.0) };
            let (frame, _) = partly.render(&mut surface, &panel).unwrap();
            let partial = pixels(frame.dc, frame.width, frame.height);
            let (mut wholly, mut fresh) = (Renderer::new().unwrap(), Surface::default());
            let (frame, _) = wholly.render(&mut fresh, &panel).unwrap();
            assert!(same_pixels(&partial, &pixels(frame.dc, frame.width, frame.height)), "step {step} in {style:?} left different pixels");
        }
    }
}

#[test]
fn clicking_a_speakers_name_names_them_in_every_label_style_but_never_the_first_voice_at_your_mic() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds, view) = (speaker_captions(), Keybinds::default(), View::new());
    for style in LabelStyle::ALL {
        let mut renderer = Renderer::new().unwrap();
        let panel = Panel { labels: Labels { you: style, speakers: style }, height: 640.0, ..panel(&captions, &view, &keybinds, 560.0) };
        let (_, regions) = renderer.render(&mut Surface::default(), &panel).unwrap();
        let named: Vec<LineRef> = regions.targets.iter().filter_map(|(_, target)| if let Target::Click(Action::NameVoice(line)) = target { Some(*line) } else { None }).collect();
        assert!(named.iter().any(|line| line.section == 1), "{style:?}: a Discord speaker can be named");
        let you = &captions.sections[0].captions;
        let first_voice = |line: &LineRef| you.iter().find(|caption| (caption.stream, caption.id) == (line.stream, line.id)).is_some_and(|caption| caption.voice == Some(0));
        assert!(named.iter().filter(|line| line.section == 0).all(|line| !first_voice(line)), "{style:?}: your first voice stays You");
    }
}

#[test]
fn the_apps_tab_lists_saved_voices_to_rename_scope_and_forget() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds, mut view) = (Captions::default(), Keybinds::default(), View::new());
    let (mut renderer, mut surface) = (Renderer::new().unwrap(), Surface::default());
    view.tab = Tab::Apps;
    let voice = |id, name: &str, scope, home: Option<&str>| VoiceInfo { id, name: name.into(), scope, home: home.map(str::to_owned) };
    let saved = vec![voice(0, "Maya", VoiceScope::Everywhere, Some("Discord")), voice(1, "Sam", VoiceScope::App("Edge".into()), Some("Edge")), voice(2, "Alex", VoiceScope::Everywhere, None)];
    let settings = settings_window(Panel { saved_voices: saved, ..panel(&captions, &view, &keybinds, 560.0) });
    let (_, regions) = renderer.render_settings(&mut surface, &settings).unwrap();
    let has = |action| regions.targets.iter().any(|(_, target)| *target == Target::Click(action));
    assert!(has(Action::RenameSaved(0)) && has(Action::ForgetSaved(2)) && has(Action::SetVoiceScope(1, false)) && has(Action::SetVoiceScope(0, true)));
    assert!(!has(Action::SetVoiceScope(2, true)), "a voice named at the mic has no app to be limited to");
    let empty = settings_window(panel(&captions, &view, &keybinds, 560.0));
    let texts = renderer.render_settings(&mut Surface::default(), &empty).map(|_| ()).is_ok();
    assert!(texts);
}

#[test]
fn settings_drawing_only_what_changed_matches_drawing_everything() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds) = (Captions::default(), Keybinds::default());
    for dpi in [96.0, 144.0] {
        let (mut view, mut partly, mut surface, mut themes) = (View::new(), Renderer::new().unwrap(), Surface::default(), Themes::default());
        for step in 0..11 {
            match step {
                1 => view.hovered = Some(Action::ShowTab(Tab::Listening)),
                2 => view.tab = Tab::Listening,
                7 => view.tab = Tab::Apps,
                3 => view.tab = Tab::Appearance,
                4 => view.open_dropdown = Some(Dropdown::Colour(super::view::Paint::Pill)),
                5 => (view.tab, view.open_dropdown) = (Tab::Shortcuts, None),
                6 => view.tab = Tab::General,
                8 => (view.tab, themes) = (Tab::Appearance, custom_themes()),
                9 => view.tab = Tab::Words,
                10 => (view.typing_word, view.hovered) = (Some(WordBox::Try), Some(Action::RemoveWordEntry(1))),
                _ => {}
            }
            let panel = settings_window(Panel { dpi, themes, ..panel(&captions, &view, &keybinds, 560.0) });
            let (frame, _) = partly.render_settings(&mut surface, &panel).unwrap();
            let partial = pixels(frame.dc, frame.width, frame.height);
            let (mut wholly, mut fresh) = (Renderer::new().unwrap(), Surface::default());
            let (frame, _) = wholly.render_settings(&mut fresh, &panel).unwrap();
            assert!(same_pixels(&partial, &pixels(frame.dc, frame.width, frame.height)), "step {step} at {dpi} DPI left different pixels");
        }
    }
}

#[test]
fn every_settings_tab_can_be_reached_from_the_window() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds, mut view) = (Captions::default(), Keybinds::default(), View::new());
    let (mut renderer, mut surface) = (Renderer::new().unwrap(), Surface::default());
    for tab in Tab::ALL {
        view.tab = tab;
        let (_, regions) = renderer.render_settings(&mut surface, &settings_window(panel(&captions, &view, &keybinds, 560.0))).unwrap();
        let tabs: Vec<Target> = regions.targets.iter().map(|(_, target)| *target).filter(|t| matches!(t, Target::Click(Action::ShowTab(_)))).collect();
        assert_eq!(tabs, Tab::ALL.map(|tab| Target::Click(Action::ShowTab(tab))));
        assert!(regions.targets.iter().any(|(_, target)| *target == Target::Click(Action::CloseSettings)));
    }
}

#[test]
fn subtitles_drawing_only_what_changed_matches_drawing_everything() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let wraps = "a line from an app long enough to wrap across more than two rows of captions, so the top of it scrolls away and only its last rows show";
    let line = SubtitleLine::new;
    let named = |text, label, voice| SubtitleLine { label: Some(label), voice: Some(voice), ..SubtitleLine::new(text, Speaker::App(None)) };
    let steps: [(Vec<SubtitleLine>, Theme, u8); 10] = [
        (vec![line("Captions mode  ·  Ctrl+Win+T for the transcript", Speaker::Kiku)], Theme::Night, 92),
        (vec![line("so the", Speaker::You)], Theme::Night, 92),
        (vec![line("so the meeting moved", Speaker::You)], Theme::Night, 92),
        (vec![line("so the meeting moved to Thursday.", Speaker::You), line("Sounds good.", Speaker::App(None))], Theme::Night, 92),
        (vec![line("so the meeting moved to Thursday.", Speaker::You), line("Sounds good.", Speaker::App(None))], Theme::Graphite, 92),
        (vec![line("Sounds good.", Speaker::App(None)), line(wraps, Speaker::App(None))], Theme::Graphite, 92),
        (vec![line("Sounds good.", Speaker::App(None)), line(wraps, Speaker::App(None))], Theme::Graphite, 30),
        (vec![named("Sounds good.", "Speaker 1", 0), named("Not for me.", "Speaker 2", 1)], Theme::Graphite, 92),
        (vec![named("Sounds good.", "Speaker 1", 0), named("Not for me, sorry.", "Speaker 2", 1)], Theme::Graphite, 92),
        (vec![named("Sounds good.", "Speaker 1", 0), named("Not for me, sorry.", "Speaker 2", 1), named("Hm.", "Mic 2", 1)], Theme::Night, 92),
    ];
    // Every DPI with the default label styles, and every style at one.
    let styles = LabelStyle::ALL.map(|style| (Labels { you: style, speakers: style }, vec![144.0]));
    let runs = std::iter::once((Labels::default(), vec![96.0, 120.0, 144.0, 168.0])).chain(styles);
    for (labels, dpis) in runs {
        for dpi in dpis {
            let (mut partly, mut surface) = (Renderer::new().unwrap(), Surface::default());
            for (step, (lines, theme, opacity)) in steps.iter().enumerate() {
                let look = SubtitleSettings::default().look(&Themes::from(*theme));
                let (frame, _) = partly.render_subtitles(&mut surface, &SubtitleScene { labels, ..subtitles(&partly, lines, &look, *opacity, dpi) }).unwrap();
                let partial = pixels(frame.dc, frame.width, frame.height);
                let (mut wholly, mut fresh) = (Renderer::new().unwrap(), Surface::default());
                let (frame, _) = wholly.render_subtitles(&mut fresh, &SubtitleScene { labels, ..subtitles(&wholly, lines, &look, *opacity, dpi) }).unwrap();
                assert!(same_pixels(&partial, &pixels(frame.dc, frame.width, frame.height)), "step {step} at {dpi} DPI in {labels:?} left different pixels");
            }
        }
    }
}

/// Writes PNGs of the panel, the settings window and captions mode to the folder named by `KIKU_SNAPSHOTS`, for
/// looking at the design (`cargo test -p kiku snapshots -- --ignored`).
#[test]
#[ignore = "writes images for review"]
fn snapshots() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let folder = std::path::PathBuf::from(std::env::var("KIKU_SNAPSHOTS").expect("KIKU_SNAPSHOTS names the folder for the images"));
    std::fs::create_dir_all(&folder).unwrap();
    let (captions, keybinds) = (snapshot_captions(), Keybinds::default());
    let usage = Some(Usage { cpu_percent: 0.4, gpu_percent: 0.0, gpu_memory_bytes: 0 });
    let mut renderer = Renderer::new().unwrap();
    let name = |theme: Theme| theme.name().to_lowercase();
    let mut custom_light = Themes::from(Theme::Meadow);
    custom_light.edit(|colours| (colours.background, colours.accent) = (0xf5f1e8, 0x8a5300));
    // Each preset, and Custom: dark with its own background and accent, and light.
    let customs = [("custom".to_owned(), custom_themes()), ("custom-light".to_owned(), custom_light)];
    let shown: Vec<(String, Themes)> = Theme::ALL.map(|theme| (name(theme), Themes::from(theme))).into_iter().chain(customs).collect();
    // The pointer resting on the History button, long enough for its tooltip.
    let mut tip = View::new();
    let (_, regions) = renderer.render(&mut Surface::default(), &panel(&captions, &tip, &keybinds, 560.0)).unwrap();
    let button = regions.rect_of(Target::Click(Action::ShowSaved)).unwrap();
    (tip.hovered, tip.tip, tip.pointer) = (Some(Action::ShowSaved), true, (button.left + 4.0, button.top + 4.0));
    // A see-through background under solid text.
    for theme in [Theme::Night, Theme::Day] {
        let (mut surface, view) = (Surface::default(), View::new());
        let shown = Panel { usage, themes: Themes::from(theme), height: 420.0, shown_opacity: Opacity { window: 30, text: 100 }, ..panel(&captions, &view, &keybinds, 560.0) };
        write_png(&folder.join(format!("panel-see-through-{}.png", name(theme))), renderer.render(&mut surface, &shown).unwrap().0);
    }
    // The macro palette over the live panel, and the title bar's chip for a mode that isn't normal.
    let palette_macros = Macros::parse(&[SNAPSHOT_MACROS, SNAPSHOT_MORE_MACROS].concat(), &WORD_RULES, &Variables::default());
    let mut asking = View::new();
    asking.palette = Some(Palette::open(&palette_macros, "Discord", &Mode::Normal));
    asking.hovered = Some(Action::RunFromPalette(1));
    for theme in [Theme::Night, Theme::Day] {
        for (shot, view, mode) in [("panel-macro-palette", &asking, Mode::Normal), ("panel-mode-chip", &View::new(), Mode::Commands)] {
            let mut surface = Surface::default();
            let shown = Panel { usage, themes: Themes::from(theme), height: 420.0, mode, ..panel(&captions, view, &keybinds, 560.0) };
            write_png(&folder.join(format!("{shot}-{}.png", name(theme))), renderer.render(&mut surface, &shown).unwrap().0);
        }
    }
    // Dictation: the General tab's card, and the badge while dictation stays on until told.
    let mut general = View::new();
    general.tab = Tab::General;
    general.dictation = DictationCard::open(&DictationSettings { key: DictationKey::Toggle, release_delay: Duration::from_millis(500), ..DictationSettings::default() });
    let idle = View::new();
    for theme in [Theme::Night, Theme::Day] {
        let themes = Themes::from(theme);
        save_settings_window(&mut renderer, &folder.join(format!("settings-general-dictation-{}.png", name(theme))), Panel { themes, ..panel(&captions, &general, &keybinds, 560.0) });
        let mut surface = Surface::default();
        let badge = Panel { usage, themes, height: 420.0, dictating: true, latched: true, stop_hint: DICTATING_HINT, ..panel(&captions, &idle, &keybinds, 560.0) };
        write_png(&folder.join(format!("panel-dictating-badge-{}.png", name(theme))), renderer.render(&mut surface, &badge).unwrap().0);
    }
    let mut save_panel = |name: &str, view: &View, themes: Themes, height: f32| {
        let mut surface = Surface::default();
        let (frame, _) = renderer.render(&mut surface, &Panel { usage, themes, height, ..panel(&captions, view, &keybinds, 560.0) }).unwrap();
        write_png(&folder.join(format!("{name}.png")), frame);
    };

    let mut live = View::new();
    live.hovered_header = Some(1);
    live.hovered_line = captions.sections[0].captions.back().map(|caption| LineRef::of(0, caption));
    let mut folded = View::new();
    folded.folds.toggle(1, false);
    folded.folds.hide(2);
    folded.hovered = Some(Action::ShowHidden);
    let mut block = View::new();
    block.blocking = Some(BlockPrompt { section: 1, dont_ask: true });
    let history = history_view();
    let mut deleting = history_view();
    (deleting.hovered, deleting.finder.deleting) = (Some(Action::DeleteSession(1)), Some(2));
    let mut found = history_view();
    search_found(&mut found);
    found.hovered = Some(Action::OpenResult(1));
    let mut who = history_view();
    search_found(&mut who);
    who.open_dropdown = Some(Dropdown::Who);
    let mut sql = history_view();
    sql_table(&mut sql);
    let mut opened = history_view();
    search_found(&mut opened);
    let rows = (0..60).map(|i| format!("[14:{:02}:00] Discord: {}", i, if i == 30 { "and the signing can wait until Monday" } else { "so the meeting moved to Thursday" })).collect();
    opened.screen = Screen::SavedFile { session: 2, title: "Discord  ·  Oct 30, 14:05".into(), lines: rows, focus: Some(30) };
    let mut scrolled = View::new();
    scrolled.scrolls.insert(ScrollArea::Section(1), Scroll::at(0.0));
    let mut collapsed = View::new();
    collapsed.collapsed = true;
    let mut quitting = View::new();
    quitting.quit_prompt = true;
    let mut updating = View::new();
    let (url, sha256) = ("https://downloads.example/kiku/Kiku-0.2.0-setup.exe".into(), "ab".repeat(32));
    updating.update = Some(Release { version: "0.2.0".into(), url, size: 20_000_000, sha256, moved_to: String::new(), notes: "Faster captions and fewer dropped words.".into() });
    for (theme, themes) in &shown {
        save_panel(&format!("panel-live-{theme}"), &live, *themes, 420.0);
    }
    for theme in [Theme::Night, Theme::Day] {
        let themes = Themes::from(theme);
        save_panel(&format!("panel-folded-{}", name(theme)), &folded, themes, 420.0);
        save_panel(&format!("panel-scrolled-up-{}", name(theme)), &scrolled, themes, 420.0);
        save_panel(&format!("panel-block-{}", name(theme)), &block, themes, 420.0);
        save_panel(&format!("panel-history-{}", name(theme)), &history, themes, 420.0);
        save_panel(&format!("panel-history-delete-{}", name(theme)), &deleting, themes, 420.0);
        save_panel(&format!("panel-history-found-{}", name(theme)), &found, themes, 420.0);
        save_panel(&format!("panel-history-who-{}", name(theme)), &who, themes, 420.0);
        save_panel(&format!("panel-history-sql-{}", name(theme)), &sql, themes, 420.0);
        save_panel(&format!("panel-history-opened-{}", name(theme)), &opened, themes, 420.0);
        save_panel(&format!("panel-collapsed-{}", name(theme)), &collapsed, themes, super::render::TITLE_HEIGHT);
        save_panel(&format!("panel-tooltip-{}", name(theme)), &tip, themes, 420.0);
        save_panel(&format!("panel-quit-{}", name(theme)), &quitting, themes, 420.0);
        save_panel(&format!("panel-update-{}", name(theme)), &updating, themes, 420.0);
    }

    let blocked = [Blocked { name: "Spotify".into(), always: true }];
    let mut save_settings = |name: &str, tab: Tab, dropdown: Option<Dropdown>, themes: Themes| {
        let mut view = View::new();
        (view.tab, view.open_dropdown) = (tab, dropdown);
        save_settings_window(&mut renderer, &folder.join(format!("settings-{name}.png")), Panel { blocked: &blocked, themes, ..panel(&captions, &view, &keybinds, 560.0) });
    };
    for (theme, themes) in &shown {
        save_settings(&format!("appearance-{theme}"), Tab::Appearance, None, *themes);
    }
    for theme in [Theme::Night, Theme::Day] {
        for (tab_name, tab, dropdown) in [
            ("general", Tab::General, None),
            ("listening", Tab::Listening, None),
            ("apps", Tab::Apps, None),
            ("words", Tab::Words, None),
            ("appearance-palette", Tab::Appearance, Some(Dropdown::Colour(super::view::Paint::Background))),
            ("shortcuts", Tab::Shortcuts, None),
        ] {
            save_settings(&format!("{tab_name}-{}", name(theme)), tab, dropdown, Themes::from(theme));
        }
    }
    // The Words tab trying a line, typed in, and with an empty file.
    let mut trying = View::new();
    (trying.tab, trying.typing_word) = (Tab::Words, Some(WordBox::Try));
    trying.try_words.set("um, gg kicku.");
    trying.scrolls.insert(ScrollArea::Settings, Scroll::at(440.0));
    // The Try box with a snippet's phrase said in a line.
    let mut trying_action = View::new();
    (trying_action.tab, trying_action.typing_word) = (Tab::Words, Some(WordBox::Try));
    trying_action.try_words.set("My email is paste email.");
    trying_action.scrolls.insert(ScrollArea::Settings, Scroll::at(440.0));
    // The Listening tab's remote cards: a host answering, an invalid code, and sharing with a code.
    let key = "A".repeat(43);
    let code = format!("kiku://192.168.1.20:7300/#{key}.{key}");
    let (mut running, mut invalid, mut sharing) = (View::new(), View::new(), View::new());
    for view in [&mut running, &mut invalid, &mut sharing] {
        view.tab = Tab::Listening;
        view.scrolls.insert(ScrollArea::Settings, Scroll::at(f32::MAX));
    }
    running.host_code.set(&code);
    // A saved code with its end deleted, being edited, and pasted garbage with a secret in it, not focused.
    invalid.host_code.set(&code[..code.len() - 8]);
    invalid.host_code.left(false);
    invalid.typing_word = Some(WordBox::Host);
    let mut garbage = View::new();
    garbage.tab = Tab::Listening;
    garbage.scrolls.insert(ScrollArea::Settings, Scroll::at(f32::MAX));
    garbage.host_code.set(&format!("{key}.{key} kiku://host/#{key}"));
    let on_host = ModelState { remote: Some("192.168.1.20:7300".into()), ..panel(&captions, &running, &keybinds, 560.0).model };
    for theme in [Theme::Night, Theme::Day] {
        let themes = Themes::from(theme);
        let host = Panel { themes, engine_where: EngineWhere::Host, ..panel(&captions, &running, &keybinds, 560.0) };
        save_settings_window(&mut renderer, &folder.join(format!("settings-remote-running-{}.png", name(theme))), Panel { model: on_host.clone(), ..host });
        let host = Panel { themes, engine_where: EngineWhere::Host, engine_host: code.clone(), ..panel(&captions, &invalid, &keybinds, 560.0) };
        save_settings_window(&mut renderer, &folder.join(format!("settings-remote-invalid-{}.png", name(theme))), host);
        let host = Panel { themes, engine_where: EngineWhere::Host, ..panel(&captions, &garbage, &keybinds, 560.0) };
        save_settings_window(&mut renderer, &folder.join(format!("settings-remote-garbage-{}.png", name(theme))), host);
        let hosting = Panel { themes, host_on: true, hosted_code: Some(code.clone()), ..panel(&captions, &sharing, &keybinds, 560.0) };
        save_settings_window(&mut renderer, &folder.join(format!("settings-remote-sharing-{}.png", name(theme))), hosting);
    }
    let mut entries = View::new();
    entries.tab = Tab::Words;
    entries.scrolls.insert(ScrollArea::Settings, Scroll::at(f32::MAX));
    // A line's misheard word being corrected, opened from Discord.
    let mut correcting = View::new();
    (correcting.tab, correcting.typing_word) = (Tab::Words, Some(WordBox::Fix));
    correcting.correction = Correction { app: Some("Discord".into()), only_app: true, ..Correction::default() };
    correcting.correction.heard.set("dictionary");
    correcting.correction.fix.set("fictionary");
    // A hotword being typed, with a strength of its own.
    let mut hotword = View::new();
    (hotword.tab, hotword.typing_word, hotword.new_strength) = (Tab::Words, Some(WordBox::Hotword), Some(2.5));
    hotword.new_word.set("Lull");
    for theme in [Theme::Night, Theme::Day] {
        let themes = Themes::from(theme);
        save_settings_window(&mut renderer, &folder.join(format!("settings-words-hotword-{}.png", name(theme))), Panel { themes, ..panel(&captions, &hotword, &keybinds, 560.0) });
        save_settings_window(&mut renderer, &folder.join(format!("settings-words-try-{}.png", name(theme))), Panel { themes, ..panel(&captions, &trying, &keybinds, 560.0) });
        save_settings_window(&mut renderer, &folder.join(format!("settings-words-correct-{}.png", name(theme))), Panel { themes, ..panel(&captions, &correcting, &keybinds, 560.0) });
        save_settings_window(&mut renderer, &folder.join(format!("settings-words-try-action-{}.png", name(theme))), Panel { themes, ..panel(&captions, &trying_action, &keybinds, 560.0) });
        save_settings_window(&mut renderer, &folder.join(format!("settings-words-actions-{}.png", name(theme))), Panel { themes, ..panel(&captions, &entries, &keybinds, 560.0) });
    }
    save_settings_window(&mut renderer, &folder.join("settings-words-entries-night.png"), panel(&captions, &entries, &keybinds, 560.0));
    let (empty, mut words_tab) = (Words::default(), View::new());
    words_tab.tab = Tab::Words;
    save_settings_window(&mut renderer, &folder.join("settings-words-empty-night.png"), Panel { words: &empty, ..panel(&captions, &words_tab, &keybinds, 560.0) });
    // The Macros tab: the list, a macro being edited, and the Add step menu open.
    let macros_dir = macros_folder(SNAPSHOT_MACROS);
    let macros_view = |setup: &dyn Fn(&mut MacroTab)| {
        let mut view = View::new();
        (view.tab, view.macros) = (Tab::Macros, MacroTab::open(&macros_dir));
        setup(&mut view.macros);
        view
    };
    let more_dir = macros_folder(&[SNAPSHOT_MACROS, SNAPSHOT_MORE_MACROS].concat());
    let programs_view = |setup: &dyn Fn(&mut MacroTab)| {
        let mut view = View::new();
        (view.tab, view.macros) = (Tab::Macros, MacroTab::open(&more_dir));
        view.macros.set_policy(CommandPolicy { enabled: true, anywhere: false });
        view.scrolls.insert(ScrollArea::Settings, Scroll::at(f32::MAX));
        setup(&mut view.macros);
        view
    };
    let macro_views = [
        ("approval", programs_view(&|_| {})),
        ("editor-run", programs_view(&|tab| tab.click(MacroAction::Edit(3)))),
        ("list", macros_view(&|_| {})),
        ("editor", macros_view(&|tab| {
            tab.set_settings(&Keybinds::default(), false, "computer");
            tab.click(MacroAction::Edit(2));
        })),
        ("shortcut-clash", macros_view(&|tab| {
            [MacroAction::Edit(2), MacroAction::RecordHotkey].into_iter().for_each(|action| tab.click(action));
            tab.record(&[Key::Ctrl, Key::Win, Key::Other(u16::from(b'V'))]);
            tab.click(MacroAction::Save);
        })),
        ("editor-kiku-step", macros_view(&kiku_steps)),
        ("editor-kiku", macros_view(&|tab| {
            kiku_steps(tab);
            tab.click(MacroAction::KikuPicker(1));
        })),
        ("add-step", macros_view(&|tab| [MacroAction::Edit(0), MacroAction::AddStepMenu].into_iter().for_each(|action| tab.click(action)))),
    ];
    for theme in [Theme::Night, Theme::Day] {
        for (shown, view) in &macro_views {
            save_settings_window(&mut renderer, &folder.join(format!("settings-macros-{shown}-{}.png", name(theme))), Panel { themes: Themes::from(theme), ..panel(&captions, view, &keybinds, 560.0) });
        }
    }
    let _ = fs::remove_dir_all(macros_dir);
    let _ = fs::remove_dir_all(more_dir);
    // The font list just opened on an installed font from the middle of the list, then searched.
    let mut fonts = View::new();
    (fonts.tab, fonts.fonts) = (Tab::Appearance, renderer.font_families().unwrap());
    let chosen = SubtitleSettings { font: fonts.fonts[fonts.fonts.len() / 2].clone(), ..Default::default() };
    fonts.open_font_list(&chosen.font);
    save_settings_window(&mut renderer, &folder.join("settings-font-open-night.png"), Panel { subtitles: &chosen, ..panel(&captions, &fonts, &keybinds, 560.0) });
    fonts.font_search.set("Sans");
    fonts.search_fonts();
    save_settings_window(&mut renderer, &folder.join("settings-font-search-night.png"), Panel { subtitles: &chosen, ..panel(&captions, &fonts, &keybinds, 560.0) });
    fonts.font_search.set("zzzz");
    fonts.search_fonts();
    save_settings_window(&mut renderer, &folder.join("settings-font-none-night.png"), Panel { subtitles: &chosen, ..panel(&captions, &fonts, &keybinds, 560.0) });

    // Setup and the Listening tab on a PC with an NVIDIA card, and setup's model step on a small laptop.
    let gpu = Gpu { name: "GeForce RTX 3090".into(), vram_mb: 24 * 1024, driver: "610.62".into(), cuda_driver: Some(13_030) };
    let desktop = Hardware { threads: 24, ram_mb: 32 * 1024, gpu: Some(gpu) };
    let laptop = Hardware { threads: 4, ram_mb: 8 * 1024, gpu: None };
    let failed = ImportStatus::Failed("This is a GGUF model (whisper.cpp or llama.cpp). Kiku runs sherpa-onnx and Moonshine models.".into());
    let mut save_setup = |name: &str, setup: Option<SetupStep>, hardware: &Hardware, import: &ImportStatus, theme: Theme| {
        let mut view = View::new();
        (view.tab, view.setup) = (Tab::Listening, setup);
        let settings = settings_window(Panel { hardware, import, settings_open: true, themes: Themes::from(theme), ..panel(&captions, &view, &keybinds, 560.0) });
        renderer.render_settings(&mut Surface::default(), &settings).unwrap();
        let mut surface = Surface::default();
        let (frame, _) = renderer.render_settings(&mut surface, &settings).unwrap();
        write_png(&folder.join(format!("{name}.png")), frame);
    };
    for theme in [Theme::Night, Theme::Day] {
        for step in SetupStep::ALL {
            save_setup(&format!("setup-{}-{}", step.name().to_lowercase(), name(theme)), Some(step), &desktop, &ImportStatus::Idle, theme);
        }
        save_setup(&format!("settings-listening-gpu-{}", name(theme)), None, &desktop, &failed, theme);
    }
    save_setup("setup-model-laptop-night", Some(SetupStep::Model), &laptop, &ImportStatus::Idle, Theme::Night);
    save_setup("setup-gpu-laptop-night", Some(SetupStep::Gpu), &laptop, &ImportStatus::Idle, Theme::Night);
    let small = Hardware { gpu: Some(Gpu { name: "GeForce GTX 1660".into(), vram_mb: 6 * 1024, ..desktop.gpu.clone().unwrap() }), ..desktop.clone() };
    save_setup("setup-model-small-gpu-night", Some(SetupStep::Model), &small, &ImportStatus::Idle, Theme::Night);
    save_setup("setup-gpu-small-gpu-night", Some(SetupStep::Gpu), &small, &ImportStatus::Idle, Theme::Night);

    let discord = discord_exe();
    let lines = [
        SubtitleLine::new("Okay so the build passed, but the installer still needs signing.", Speaker::App(discord.as_deref())),
        SubtitleLine::new("I can do that after lunch.", Speaker::You),
    ];
    let captions_shots = shown.iter().map(|(name, themes)| (name.clone(), *themes, 92)).chain([("day-see-through".to_owned(), Themes::from(Theme::Day), 35)]);
    for (name, themes, opacity) in captions_shots {
        let look = SubtitleSettings::default().look(&themes);
        let scene = SubtitleScene { themes, ..subtitles(&renderer, &lines, &look, opacity, 144.0) };
        let mut surface = Surface::default();
        let (frame, _) = renderer.render_subtitles(&mut surface, &scene).unwrap();
        write_png(&folder.join(format!("captions-{name}.png")), frame);
    }
    for theme in [Theme::Night, Theme::Day] {
        let themes = Themes::from(theme);
        let dictating = format!("Dictating  ·  {DICTATING_HINT}");
        let badge = [SubtitleLine::new(lines[0].text, lines[0].speaker), SubtitleLine::new(&dictating, Speaker::Kiku)];
        let look = SubtitleSettings::default().look(&themes);
        let scene = SubtitleScene { themes, ..subtitles(&renderer, &badge, &look, 92, 144.0) };
        write_png(&folder.join(format!("captions-dictating-badge-{}.png", name(theme))), renderer.render_subtitles(&mut Surface::default(), &scene).unwrap().0);
    }
    for (name, themes, hovered) in [("night", Themes::default(), false), ("day", Themes::from(Theme::Day), true), ("custom", custom_themes(), false)] {
        let mut surface = Surface::default();
        let frame = renderer.render_drag_tab(&mut surface, hovered, &SubtitleSettings::default().look(&themes), &themes, 144.0).unwrap();
        write_png(&folder.join(format!("drag-tab-{name}.png")), frame);
    }
    // Speakers told apart: three in a Discord call, you and a second voice at your mic; the Apps tab's switches.
    let spoken = speaker_captions();
    let speakers_view = {
        let mut view = View::new();
        view.hovered_header = Some(1);
        view
    };
    let names = [("night", Themes::default()), ("day", Themes::from(Theme::Day)), ("custom-light", custom_light)];
    let (heard, mut apps_tab) = (&spoken, View::new());
    apps_tab.tab = Tab::Apps;
    // A `Downloads` reads its queue from the models folder only as it starts, so the folder is lent for that.
    let models = std::env::temp_dir().join("kiku-speaker-snapshots");
    let _ = fs::remove_dir_all(&models);
    fs::create_dir_all(&models).unwrap();
    let before = std::env::var_os("KIKU_MODELS_DIR");
    // SAFETY: this ignored test runs alone, and the variable is put back before it goes on.
    unsafe { std::env::set_var("KIKU_MODELS_DIR", &models) };
    let missing = Downloads::start(|| {});
    fs::write(models.join("downloads.json"), format!("[\"{}\"]", catalog::SPEAKERS)).unwrap();
    let downloading = Downloads::start(|| {});
    unsafe {
        match before {
            Some(folder) => std::env::set_var("KIKU_MODELS_DIR", folder),
            None => std::env::remove_var("KIKU_MODELS_DIR"),
        }
    }
    // A renderer of its own: it remembers each section's row heights, which belong to one transcript.
    let mut speaker_renderer = Renderer::new().unwrap();
    for (theme, themes) in &names {
        let mut surface = Surface::default();
        let shown = Panel { usage, themes: *themes, height: 640.0, ..panel(heard, &speakers_view, &keybinds, 560.0) };
        write_png(&folder.join(format!("panel-speakers-{theme}.png")), speaker_renderer.render(&mut surface, &shown).unwrap().0);
        for (state, downloads) in [("missing", &missing), ("downloading", &downloading)] {
            let shown = Panel { themes: *themes, voices_mic: true, speaker_model: false, downloads, ..panel(heard, &apps_tab, &keybinds, 560.0) };
            save_settings_window(&mut renderer, &folder.join(format!("settings-apps-speakers-{state}-{theme}.png")), shown);
        }
    }
    let speaking = [
        SubtitleLine { label: Some("Speaker 1"), voice: Some(0), ..SubtitleLine::new("Okay so the build passed, but the installer still needs signing.", Speaker::App(discord.as_deref())) },
        SubtitleLine { label: Some("Speaker 2"), voice: Some(1), ..SubtitleLine::new("I think the certificate is still on the old laptop.", Speaker::App(discord.as_deref())) },
        SubtitleLine { label: Some("Speaker 3"), voice: Some(2), ..SubtitleLine::new("Check the shared drive first.", Speaker::App(discord.as_deref())) },
        SubtitleLine { label: Some("Mic 2"), voice: Some(1), ..SubtitleLine::new("Someone else at my desk says hi.", Speaker::You) },
    ];
    for (theme, themes) in &names {
        let look = SubtitleSettings::default().look(themes);
        let height = renderer.subtitle_height(&look, 4).unwrap();
        let scene = SubtitleScene { themes: *themes, rows: 4, height, ..subtitles(&renderer, &speaking, &look, 92, 144.0) };
        write_png(&folder.join(format!("captions-speakers-{theme}.png")), renderer.render_subtitles(&mut Surface::default(), &scene).unwrap().0);
    }
    // Named voices: the six label styles for the speakers of a call (Maya is named), your lines in two styles, the
    // naming box, the Apps tab's label choices and saved voices, and captions mode.
    let call = names_captions();
    let style_id = |style: LabelStyle| format!("{style:?}").to_lowercase();
    let mut names_view = View::new();
    names_view.hovered_line = call.sections[1].captions.get(3).map(|caption| LineRef::of(1, caption));
    let shot = |name: &str, theme: Theme, labels: Labels, view: &View| {
        let mut surface = Surface::default();
        let shown = Panel { usage, themes: Themes::from(theme), height: 700.0, labels, ..panel(&call, view, &keybinds, 560.0) };
        write_png(&folder.join(format!("{name}.png")), Renderer::new().unwrap().render(&mut surface, &shown).unwrap().0);
    };
    for style in LabelStyle::ALL {
        shot(&format!("panel-names-{}-night", style_id(style)), Theme::Night, Labels { speakers: style, ..Labels::default() }, &names_view);
    }
    for style in [LabelStyle::Turns, LabelStyle::Badges] {
        shot(&format!("panel-names-{}-day", style_id(style)), Theme::Day, Labels { speakers: style, ..Labels::default() }, &View::new());
    }
    for style in [LabelStyle::EveryLine, LabelStyle::Turns] {
        shot(&format!("panel-names-you-{}-night", style_id(style)), Theme::Night, Labels { you: style, ..Labels::default() }, &View::new());
    }
    let mut naming = View::new();
    naming.naming = call.sections[1].captions.front().map(|caption| LineRef::of(1, caption));
    naming.finder.typing = Some(TextBox::Name);
    naming.name.set("Ma");
    for theme in [Theme::Night, Theme::Day] {
        shot(&format!("panel-names-naming-{}", name(theme)), theme, Labels::default(), &naming);
    }
    let voice = |id, name: &str, scope, home: Option<&str>| VoiceInfo { id, name: name.into(), scope, home: home.map(str::to_owned) };
    let saved = vec![voice(0, "Maya", VoiceScope::Everywhere, Some("Discord")), voice(1, "Sam", VoiceScope::App("Discord".into()), Some("Discord"))];
    let nothing = Captions::default();
    let apps_tab = |top: f32, renaming: bool| {
        let mut view = View::new();
        view.tab = Tab::Apps;
        view.scrolls.insert(ScrollArea::Settings, Scroll::at(top));
        if renaming {
            (view.renaming, view.typing_word) = (Some(0), Some(WordBox::Rename));
            view.rename.set("Maya R");
        }
        view
    };
    for (theme, themes) in [("night", Themes::default()), ("day", Themes::from(Theme::Day))] {
        for (part, view) in [("labels", apps_tab(150.0, false)), ("voices", apps_tab(560.0, true))] {
            let shown = Panel { themes, saved_voices: saved.clone(), ..panel(&nothing, &view, &keybinds, 560.0) };
            save_settings_window(&mut Renderer::new().unwrap(), &folder.join(format!("settings-apps-{part}-{theme}.png")), shown);
        }
    }
    {
        let apps = apps_tab(0.0, false);
        let shown = Panel { themes: Themes::default(), ..panel(&nothing, &apps, &keybinds, 560.0) };
        save_settings_window(&mut Renderer::new().unwrap(), &folder.join("settings-apps-speakers-recognise-night.png"), shown);
    }
    let tagged = |text, label, tag: &'static str, voice, speaker| SubtitleLine { label: Some(label), tag: Some(tag.to_owned()), voice: Some(voice), ..SubtitleLine::new(text, speaker) };
    let spoken = [
        tagged("Okay so the build passed, but the installer still needs signing.", "Speaker 1", "1", 0, Speaker::App(discord.as_deref())),
        tagged("I think the certificate is still on the old laptop.", "Maya", "Maya", 1, Speaker::App(discord.as_deref())),
        tagged("Check the shared drive first.", "Speaker 3", "3", 2, Speaker::App(discord.as_deref())),
        tagged("Someone else at my desk says hi.", "Mic 2", "Mic 2", 1, Speaker::You),
    ];
    for style in [LabelStyle::EveryLine, LabelStyle::Tags, LabelStyle::Badges, LabelStyle::Turns] {
        let look = SubtitleSettings::default().look(&Themes::default());
        let height = renderer.subtitle_height(&look, 4).unwrap();
        let scene = SubtitleScene { rows: 4, height, labels: Labels { you: style, speakers: style }, ..subtitles(&renderer, &spoken, &look, 92, 144.0) };
        write_png(&folder.join(format!("captions-names-{}-night.png", style_id(style))), renderer.render_subtitles(&mut Surface::default(), &scene).unwrap().0);
    }
    write_png(&folder.join("icons.png"), icon_sheet(&mut renderer, &mut Surface::default()));
}

/// Writes the settings window drawn from `panel` to `path`.
fn save_settings_window(renderer: &mut Renderer, path: &std::path::Path, panel: Panel) {
    let settings = settings_window(Panel { settings_open: true, ..panel });
    // Twice, since a tab's height is measured as it is drawn.
    renderer.render_settings(&mut Surface::default(), &settings).unwrap();
    let mut surface = Surface::default();
    let (frame, _) = renderer.render_settings(&mut surface, &settings).unwrap();
    write_png(path, frame);
}

/// History's list of past sessions, as the panel shows it.
fn history_view() -> View {
    let mut view = View::new();
    view.screen = Screen::SavedList;
    let ago = |minutes: u64| std::time::SystemTime::now() - Duration::from_secs(minutes * 60);
    let session = |id, minutes, length, lines, apps: &str| Session { id, started: ago(minutes), ended: None, lines, apps: apps.into(), last_line: ago(minutes - length) };
    view.saved = vec![session(3, 50, 45, 48, "Discord,Edge"), session(2, 190, 80, 312, "Discord"), session(1, 1500, 0, 9, "")];
    let speaker = |id, name: &str, you| kiku_core::history::Speaker { id, name: name.into(), you, lines: 10, last_heard: None };
    view.finder.speakers = vec![speaker(2, "You", true), speaker(1, "Discord", false), speaker(3, "Edge", false)];
    view
}

/// A search for "signing" being typed, and what it found.
fn search_found(view: &mut View) {
    let ago = |minutes: u64| std::time::SystemTime::now() - Duration::from_secs(minutes * 60);
    let line = |id, minutes, speaker: &str, talking_to: Option<&str>, text: &str| StoredLine {
        id,
        session: 2,
        at: ago(minutes),
        speaker: speaker.into(),
        you: speaker == "You",
        talking_to: talking_to.map(Into::into),
        text: text.into(),
    };
    view.finder.search.set("signing");
    view.finder.typing = Some(TextBox::Search);
    view.finder.who = Who::Conversation("Discord".into());
    let lines = vec![
        line(9, 12, "You", Some("Discord"), "I can do the signing after lunch."),
        line(8, 13, "Discord", None, "Cool. So the build passed last night, but the installer still needs signing."),
        line(5, 140, "Discord", None, "Signing the driver needs the EV certificate, which lives on the old laptop in the office cupboard."),
        line(2, 1500, "You", None, "Remind me about the signing tomorrow."),
    ];
    view.finder.found = Found { total: lines.len(), lines, complete: true, loading: false };
}

/// SQL mode after running the first example.
fn sql_table(view: &mut View) {
    view.finder.sql_mode = true;
    view.finder.sql.set(&super::finder::example_sql(0));
    let row = |cells: [&str; 3]| cells.map(String::from).to_vec();
    let rows = vec![row(["Discord", "212", "2841"]), row(["You", "64", "702"]), row(["Edge", "31", "655"])];
    view.finder.table = Some(Ok(Table { columns: vec!["speaker".into(), "lines".into(), "words".into()], rows, more: false }));
}

/// Every icon at 16 and 20 DIPs, in light text on a dark card and dark text on a light one.
fn icon_sheet<'s>(renderer: &mut Renderer, surface: &'s mut Surface) -> &'s mut Frame {
    use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
    const CELL: f32 = 36.0;
    const COLUMNS: usize = 10;
    let color = |r: u8, g: u8, b: u8| D2D1_COLOR_F { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 };
    let rows = Icon::ALL.len().div_ceil(COLUMNS) as f32;
    let (card_width, card_height) = (COLUMNS as f32 * CELL + 24.0, 2.0 * rows * CELL + 24.0);
    let cards = [(color(0x1d, 0x26, 0x33), color(0xee, 0xf3, 0xf7)), (color(0xee, 0xf1, 0xf4), color(0x1d, 0x26, 0x33))];
    let size = (2.0 * card_width + 36.0, card_height + 24.0);
    renderer
        .render_on(surface, size, 0.0, 144.0, &Themes::default(), |renderer| {
            for (card, (back, ink)) in cards.into_iter().enumerate() {
                let left = 12.0 + card as f32 * (card_width + 12.0);
                renderer.fill_rounded(D2D_RECT_F { left, top: 12.0, right: left + card_width, bottom: 12.0 + card_height }, 10.0, back)?;
                for (block, side) in [16.0, 20.0].into_iter().enumerate() {
                    for (index, &icon) in Icon::ALL.iter().enumerate() {
                        let row = (block as f32 * rows + (index / COLUMNS) as f32) * CELL;
                        let x = left + 12.0 + (index % COLUMNS) as f32 * CELL + (CELL - side) / 2.0;
                        let y = 24.0 + row + (CELL - side) / 2.0;
                        renderer.icon(icon, D2D_RECT_F { left: x, top: y, right: x + side, bottom: y + side }, ink)?;
                    }
                }
            }
            Ok(())
        })
        .unwrap()
}

/// A Custom theme made from Night: plum with an amber accent, and lavender captions.
fn custom_themes() -> Themes {
    let mut themes = Themes::default();
    themes.edit(|colours| *colours = kiku_core::theme::Colours { background: 0x2a2140, accent: 0xf5c451, pill: 0xdccdf7, text: 0x2a2140 });
    themes
}

/// A short, realistic transcript: a Discord call, a browser video, and you.
fn snapshot_captions() -> Captions {
    let (mut captions, now) = (Captions::default(), Instant::now());
    let discord = captions.add_app("Discord", discord_exe());
    captions.set_stream_place(1, Place::App("Discord".into()));
    let edge = captions.add_app("Edge", Some(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe".into()));
    let said = |id, text: &str, complete| Line { id, text: text.into(), complete, voice: None };
    for (id, text) in ["Hey, can everyone hear me okay?", "Cool. So the build passed last night, but the installer still needs signing.", "Who has the certificate?"].iter().enumerate() {
        captions.update_app(discord, 1, &[said(id as u64, text, true)], now);
    }
    captions.update_app(discord, 1, &[said(3, "I think it's on the old", false)], now);
    captions.update_app(edge, 1, &[said(0, "In this video we'll look at how glass was made in the eighteenth century.", true)], now);
    captions.pin("Discord", true);
    for (id, text) in ["Yeah, loud and clear.", "I can do the signing after lunch."].iter().enumerate() {
        captions.update_mic(2, &[said(id as u64, text, true)], Some("Discord"));
    }
    captions
}

/// A Discord call of three voices, a video, and you with a second voice at your mic.
/// A call of three speakers in Discord, the second (Maya) named and speaking in turns of several lines, and your
/// mic with a second voice, which is copied into Discord.
fn names_captions() -> Captions {
    let (mut captions, now) = (Captions::default(), Instant::now());
    let discord = captions.add_app("Discord", discord_exe());
    let place = Place::App("Discord".into());
    captions.set_stream_place(1, place.clone());
    captions.set_stream_place(2, Place::Mic);
    let said = |id, (voice, text): (u32, &str)| Line { id, text: text.into(), complete: true, voice: Some(voice) };
    let call = [
        (0, "Okay so the build passed, but the installer still needs signing."),
        (0, "I can look at the certificate after lunch."),
        (1, "I think the certificate is still on the old laptop."),
        (1, "Check the shared drive first, there should be a copy there."),
        (1, "If not, I can export a new one tonight."),
        (2, "Sounds good to me."),
        (0, "Great, thanks both."),
    ];
    for (id, line) in call.into_iter().enumerate() {
        captions.update_app(discord, 1, &[said(id as u64, line)], now);
    }
    captions.set_names(discord, &place, [(1, "Maya".to_owned())].into());
    let mine = [(0, "Sure, I will start the build again."), (0, "It should only take a few minutes."), (1, "Someone else at my desk says hi."), (0, "Sorry about that.")];
    for (id, line) in mine.into_iter().enumerate() {
        captions.update_mic(2, &[said(id as u64, line)], Some("Discord"));
    }
    captions
}

fn speaker_captions() -> Captions {
    let (mut captions, now) = (Captions::default(), Instant::now());
    let discord = captions.add_app("Discord", discord_exe());
    let edge = captions.add_app("Edge", Some(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe".into()));
    let said = |id, voice, text: &str| Line { id, text: text.into(), complete: true, voice: Some(voice) };
    let call = [(0, "Okay so the build passed, but the installer still needs signing."), (1, "I think the certificate is still on the old laptop."), (2, "Check the shared drive first."), (1, "Found it, it was in the drive.")];
    for (id, (voice, text)) in call.iter().enumerate() {
        captions.update_app(discord, 1, &[said(id as u64, *voice, text)], now);
    }
    captions.update_app(edge, 1, &[said(0, 0, "In this video we'll look at how glass was made in the eighteenth century.")], now);
    for (id, (voice, text)) in [(0, "Great, then I can sign it after lunch."), (1, "Someone else at my desk says hi."), (0, "Sorry about that.")].iter().enumerate() {
        captions.update_mic(2, &[said(id as u64, *voice, text)], Some("Discord"));
    }
    captions
}

/// A new macro with two Kiku steps, the first Open settings on the words tab.
fn kiku_steps(tab: &mut MacroTab) {
    let steps = [MacroAction::New, MacroAction::AddStepMenu, MacroAction::AddStep(StepKind::Kiku), MacroAction::PickKiku { step: 0, action: KikuAction::OpenSettings }, MacroAction::Focus(MacroBox::Step(0))];
    steps.into_iter().for_each(|action| tab.click(action));
    tab.type_text("words");
    [MacroAction::AddStepMenu, MacroAction::AddStep(StepKind::Kiku)].into_iter().for_each(|action| tab.click(action));
}

fn discord_exe() -> Option<std::path::PathBuf> {
    let local = std::env::var("LOCALAPPDATA").ok()?;
    std::fs::read_dir(std::path::Path::new(&local).join("Discord")).ok()?.flatten().map(|entry| entry.path().join("Discord.exe")).find(|exe| exe.exists())
}

/// Saves the frame in `dc` as a PNG over a desktop-like backdrop, so its see-through parts show.
fn write_png(path: &std::path::Path, frame: &Frame) {
    let (width, height) = (frame.width, frame.height);
    use windows::Win32::{
        Graphics::Imaging::{CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory, WICBitmapEncoderNoCache},
        System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER},
    };
    let backdrop = |y: i32| {
        let t = y as f32 / height.max(1) as f32;
        let channel = |top: f32, bottom: f32| top + (bottom - top) * t;
        [channel(150.0, 70.0), channel(120.0, 80.0), channel(95.0, 70.0)]
    };
    let mut bytes = Vec::with_capacity((width * height * 4) as usize);
    for (index, pixel) in pixels(frame.dc, width, height).into_iter().enumerate() {
        let back = backdrop(index as i32 / width);
        let alpha = (pixel >> 24) as f32 / 255.0;
        for (shift, back) in [(0, back[0]), (8, back[1]), (16, back[2])] {
            bytes.push((((pixel >> shift) & 0xff) as f32 + back * (1.0 - alpha)).round() as u8);
        }
        bytes.push(u8::MAX);
    }
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
        let stream = wic.CreateStream().unwrap();
        stream.InitializeFromFilename(&windows::core::HSTRING::from(path.as_os_str()), 0x4000_0000).unwrap();
        let encoder = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).unwrap();
        encoder.Initialize(&stream, WICBitmapEncoderNoCache).unwrap();
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut()).unwrap();
        let frame = frame.unwrap();
        frame.Initialize(None).unwrap();
        frame.SetSize(width as u32, height as u32).unwrap();
        let mut format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut format).unwrap();
        frame.WritePixels(height as u32, width as u32 * 4, &bytes).unwrap();
        frame.Commit().unwrap();
        encoder.Commit().unwrap();
    }
}

#[test]
fn history_drawn_only_where_changed_matches_drawing_everything() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds) = (Captions::default(), Keybinds::default());
    for dpi in [96.0, 144.0] {
        let (mut view, mut partly, mut surface) = (history_view(), Renderer::new().unwrap(), Surface::default());
        for step in 0..8 {
            match step {
                1 => view.hovered = Some(Action::OpenSaved(1)),
                2 => (view.hovered, view.finder.deleting) = (Some(Action::DeleteSession(1)), Some(2)),
                3 => search_found(&mut view),
                4 => view.hovered = Some(Action::CopyResult(1)),
                5 => view.open_dropdown = Some(Dropdown::Who),
                6 => sql_table(&mut view),
                7 => {
                    let rows = (0..60).map(|i| format!("[14:{i:02}:00] Discord: the signing can wait")).collect();
                    view.screen = Screen::SavedFile { session: 2, title: "Discord".into(), lines: rows, focus: Some(40) };
                }
                _ => {}
            }
            let panel = Panel { dpi, ..panel(&captions, &view, &keybinds, 560.0) };
            let (frame, regions) = partly.render(&mut surface, &panel).unwrap();
            let partial = pixels(frame.dc, frame.width, frame.height);
            let (mut wholly, mut fresh) = (Renderer::new().unwrap(), Surface::default());
            let (frame, _) = wholly.render(&mut fresh, &panel).unwrap();
            assert!(same_pixels(&partial, &pixels(frame.dc, frame.width, frame.height)), "step {step} at {dpi} DPI left different pixels");
            if step == 7 {
                let (_, top) = regions.revealed.expect("opened at a line, it says where it scrolled to");
                assert!(top > 0.0, "the line is further down than the first screen");
            }
        }
    }
}

#[test]
fn history_offers_its_filters_results_and_sql() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds, mut view) = (Captions::default(), Keybinds::default(), history_view());
    let mut renderer = Renderer::new().unwrap();
    let mut targets = |view: &View| -> Vec<Action> {
        let (_, regions) = renderer.render(&mut Surface::default(), &panel(&captions, view, &keybinds, 560.0)).unwrap();
        regions.targets.iter().filter_map(|(_, target)| match target { Target::Click(action) => Some(*action), _ => None }).collect()
    };
    let shown = targets(&view);
    for action in [Action::Type(TextBox::Search), Action::ShowSql(true), Action::ToggleDropdown(Dropdown::Who), Action::OpenSaved(2)] {
        assert!(shown.contains(&action), "{action:?}");
    }
    search_found(&mut view);
    view.open_dropdown = Some(Dropdown::Who);
    let shown = targets(&view);
    assert!(shown.contains(&Action::OpenResult(3)) && shown.contains(&Action::ClearSearch) && shown.contains(&Action::CopyResults));
    assert_eq!(shown.iter().filter(|action| matches!(action, Action::SetWho(_))).count(), 6, "everyone, you, and each app alone and with you");
    sql_table(&mut view);
    let shown = targets(&view);
    assert!(shown.contains(&Action::RunSql) && shown.contains(&Action::Example(0)) && shown.contains(&Action::ShowSql(false)));
}

#[test]
fn pointing_at_a_lines_button_gutter_shows_its_buttons() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds, mut view) = (long_captions(), Keybinds::default(), View::new());
    let (mut renderer, mut surface) = (Renderer::new().unwrap(), Surface::default());
    let (_, regions) = renderer.render(&mut surface, &panel(&captions, &view, &keybinds, 560.0)).unwrap();
    let shown = regions.captions.last().copied().unwrap();
    let (x, y) = (shown.row.right - 12.0, shown.row.top + 4.0);
    assert!(regions.caption_at(x, y).is_none(), "the gutter holds no text to select");

    view.hovered_line = regions.line_at(x, y);
    assert_eq!(view.hovered_line, Some(shown.line));
    let (_, regions) = renderer.render(&mut surface, &panel(&captions, &view, &keybinds, 560.0)).unwrap();
    assert_eq!(regions.target_at(x, y), Some(Target::Click(Action::ShareLine(shown.line))));
}

