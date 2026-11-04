//! UI state of the overlay that isn't the transcript itself: which screen is shown, scroll positions, toggles.

use std::collections::{HashMap, VecDeque};

use super::{
    field::Field,
    finder::{Finder, Period, TextBox},
    folds::Folds,
    render::font_list_scroll,
    selection::Selection,
};

use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

use kiku_core::{
    captions::{Caption, StreamId},
    catalog::Model,
    controls::Input,
    download::Target as Download,
    engine::Device,
    keybinds::Command,
    history::Session,
    settings::{BlockAnswer, YourLines},
    theme::{Colours, Theme},
    update::Release,
    words::Scope,
};

/// What the panel is showing.
pub enum Screen {
    Live,
    /// History: past sessions, or what the search or SQL found.
    SavedList,
    /// One past session's lines, from the history database, as rows to read, and the row of a line
    /// opened from the search, which starts in view.
    SavedFile { session: i64, title: String, lines: Vec<String>, focus: Option<usize> },
}

/// A tab of the settings window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    General,
    Listening,
    Apps,
    Words,
    Appearance,
    Shortcuts,
}

/// A step of the setup the settings window walks through on first run: each has a default, so Next
/// all the way through works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupStep {
    Keys,
    Model,
    Gpu,
    Saving,
}

impl SetupStep {
    pub const ALL: [SetupStep; 4] = [SetupStep::Keys, SetupStep::Model, SetupStep::Gpu, SetupStep::Saving];

    pub fn name(self) -> &'static str {
        match self {
            SetupStep::Keys => "Keys",
            SetupStep::Model => "Model",
            SetupStep::Gpu => "GPU",
            SetupStep::Saving => "Saving",
        }
    }

    /// The step after this one, or `None` after the last.
    pub fn next(self) -> Option<SetupStep> {
        Self::ALL.iter().position(|&s| s == self).and_then(|i| Self::ALL.get(i + 1).copied())
    }

    pub fn back(self) -> Option<SetupStep> {
        Self::ALL.iter().position(|&s| s == self).and_then(|i| i.checked_sub(1)).map(|i| Self::ALL[i])
    }
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::General, Tab::Listening, Tab::Apps, Tab::Words, Tab::Appearance, Tab::Shortcuts];

    pub fn name(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Listening => "Listening",
            Tab::Apps => "Apps",
            Tab::Words => "Words",
            Tab::Appearance => "Appearance",
            Tab::Shortcuts => "Shortcuts",
        }
    }
}

/// Something the user can click.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Manual save mode's buttons: save every app, or one, to the history database.
    SaveAll,
    SaveSection(usize),
    CopySection(usize),
    /// Folds a section to one row, or opens it, by hand.
    ToggleFold(usize),
    /// Takes an app off the panel; the "N hidden" chip brings every hidden app back.
    Hide(usize),
    ShowHidden,
    /// Fold quiet apps after this many minutes; 0 never does.
    AutoFold(u32),
    /// Stops capturing an app: asks first, unless an answer was remembered.
    Block(usize),
    /// The prompt's answers: block until Kiku quits (false) or always, or don't.
    BlockFor(bool),
    CancelBlock,
    /// The prompt's "Don't ask again" box.
    DontAskToBlock,
    /// Unblocks an app, by its place in the blocked list.
    Unblock(usize),
    /// Pins or unpins the app at a section index: a pinned app gets every line you say.
    Pin(usize),
    /// Unpins an app, by its place in the pinned list.
    Unpin(usize),
    SetBlockAnswer(BlockAnswer),
    SetYourLines(YourLines),
    /// Where word rules (false) or transforms (true) apply.
    WordScope { transforms: bool, scope: Scope },
    /// Whether the model listens for the file's `@` words.
    WordHotwords(bool),
    /// Removes an entry from the word rules file, by its place in the file's entries.
    RemoveWordEntry(usize),
    /// Opens the word rules file, made first if missing.
    OpenWords,
    /// Copies how to edit the word rules file, for an assistant.
    CopyWordInstructions,
    /// Keys go to one of the Words tab's boxes until it is left; and emptying it.
    TypeWord(WordBox),
    ClearWord(WordBox),
    /// Opens the Words tab's correction card with a line's text, or the selection, as what was heard.
    CorrectLine(LineRef),
    CorrectSelection,
    /// Whether the correction's rule applies only in the app its line came from.
    CorrectOnlyApp(bool),
    /// Adds the correction as a rule (heard => should be), or its right word as one to listen for.
    AddRule,
    AddHotword,
    CopyLine(LineRef),
    /// Scrolls a section back down to its newest line, which it follows again.
    ScrollToLatest(usize),
    /// The ✨ buttons: open a Claude chat about a line, an app, everything, or the selection.
    ShareLine(LineRef),
    ShareSection(usize),
    ShareAll,
    ShareSelection,
    /// Opens or closes a drop-down list.
    ToggleDropdown(Dropdown),
    /// Picks auto (true) or manual save mode.
    SetAutoSave(bool),
    /// In auto save mode, save only your mic (true) or everything.
    SaveMicOnly(bool),
    /// Keep auto-saved transcripts this many days; 0 keeps them forever.
    KeepDays(u32),
    /// Starts waiting for the keys of a shortcut in settings.
    Rebind(Rebind),
    ResetKeys,
    /// Captions mode (true) or the transcript panel.
    ShowSubtitles(bool),
    /// The app's theme, for the panel, settings and captions mode; Custom is made if there is none.
    SetTheme(Theme),
    /// One of the theme's colours, from the palette or any from Windows' colour picker; a preset's,
    /// changed, makes the Custom theme.
    SetColour(Paint, u32),
    MoreColours(Paint),
    /// Captions mode's settings: rows shown, fading when quiet, font (by index in [`View::fonts`]) and text size.
    SubtitleRows(u8),
    SubtitleAutoHide(bool),
    SubtitleFont(usize),
    SubtitleSize(u16),
    ShowSaved,
    /// Opens the settings window, or closes it if open.
    ShowSettings,
    /// Opens the settings window on its Shortcuts tab.
    ShowShortcuts,
    CloseSettings,
    ShowTab(Tab),
    ToggleStop,
    PickModel(&'static Model),
    PickDevice(Device),
    /// Starts, resumes or retries a download: a model's files, or the GPU pack.
    Download(Download),
    PauseDownload(Download),
    CancelDownload(Download),
    /// Deletes downloaded files (a second press confirms), or forgets an imported model.
    Delete(Download),
    /// Picks a folder with a model and tries it.
    Import,
    /// Opens the setup in the settings window.
    OpenSetup,
    /// Setup's model step: picks the model recommended for this PC.
    UseRecommended,
    SetupNext,
    SetupBack,
    SkipSetup,
    /// The question asked on quitting while a download runs.
    QuitAndPause,
    KeepDownloading,
    /// The update offer's answers: download and install it now, or not until it is offered again.
    InstallUpdate,
    UpdateLater,
    /// Check for updates once a day (true) or never.
    CheckUpdates(bool),
    /// Checks for a newer version now.
    CheckUpdatesNow,
    /// Only dictation (true), or captions and dictation.
    DictationOnly(bool),
    ShowLive,
    OpenSaved(usize),
    /// Deletes a past session, by its place in the list, when pressed a second time.
    DeleteSession(usize),
    /// History's SQL mode (true) or its search.
    ShowSql(bool),
    /// Keys go to this box, until it is left.
    Type(TextBox),
    ClearSearch,
    ClearFontSearch,
    SetPeriod(Period),
    /// Whose lines the search looks at, by index in [`Finder::who_choices`].
    SetWho(usize),
    /// A search result, by its place in the results: open its session there, copy it, or ask Claude about it.
    OpenResult(usize),
    CopyResult(usize),
    ShareResult(usize),
    /// Everything found: by the search, or the SQL's rows.
    CopyResults,
    ShareResults,
    RunSql,
    /// Runs an example query, by index in [`super::finder::EXAMPLES`].
    Example(usize),
    ToggleLock,
    Minimize,
    Close,
}

/// A drop-down list in the settings window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dropdown {
    Font,
    /// History's choice of whose lines to search.
    Who,
    /// The colour palette under one of the theme's swatches.
    Colour(Paint),
}

/// Which of the theme's colours a picked colour paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Paint {
    Background,
    Accent,
    /// Captions mode's pills and text.
    Pill,
    Text,
}

impl Paint {
    pub const ALL: [Paint; 4] = [Paint::Background, Paint::Accent, Paint::Pill, Paint::Text];

    pub fn name(self) -> &'static str {
        match self {
            Paint::Background => "Background",
            Paint::Accent => "Accent",
            Paint::Pill => "Caption pill",
            Paint::Text => "Caption text",
        }
    }

    pub fn colour(self, mut colours: Colours) -> u32 {
        *self.field(&mut colours)
    }

    pub fn set(self, colours: &mut Colours, rgb: u32) {
        *self.field(colours) = rgb;
    }

    fn field(self, colours: &mut Colours) -> &mut u32 {
        match self {
            Paint::Background => &mut colours.background,
            Paint::Accent => &mut colours.accent,
            Paint::Pill => &mut colours.pill,
            Paint::Text => &mut colours.text,
        }
    }
}

/// Which shortcut is being set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rebind {
    Hold,
    Command(Command),
}

/// A caption line in a section. Lines are named by stream and id, which survive older lines being dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LineRef {
    pub section: usize,
    pub stream: StreamId,
    pub id: u64,
}

impl LineRef {
    pub fn of(section: usize, caption: &Caption) -> Self {
        Self { section, stream: caption.stream, id: caption.id }
    }

    pub fn position(&self, captions: &VecDeque<Caption>) -> Option<usize> {
        captions.iter().rposition(|c| (c.stream, c.id) == (self.stream, self.id))
    }
}

/// What a point on the panel does when clicked or dragged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Click(Action),
    /// Moves the window.
    Drag,
    /// Resizes the window from a corner grip.
    Resize(Corner),
    /// A section's header; double-clicking it shows the section alone.
    Header(usize),
    /// A prompt and the panel behind it: clicks there do nothing.
    Card,
    /// Dragging along it sets a value.
    Slider(Slider),
}

/// A check for updates asked for by hand: running, or what it found when nothing newer is out.
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateCheck {
    Checking,
    Latest,
    Failed(String),
}

/// What a slider sets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slider {
    /// An input's sensitivity threshold.
    Threshold(Input),
    /// Captions mode's pill opacity.
    PillOpacity,
    /// The opacity of the panel's background, while locked or unlocked.
    WindowOpacity { locked: bool },
    /// The opacity of the panel's text, while locked or unlocked.
    TextOpacity { locked: bool },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Corner {
    TopLeft,
    BottomRight,
}

/// A scrollable area, identified so its position survives redraws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScrollArea {
    Section(usize),
    SavedList,
    SavedFile,
    /// What the search found, and the SQL's rows.
    Results,
    Table,
    Settings,
    Dropdown(Dropdown),
}

/// Interactive regions from the last drawn frame, in DIPs.
#[derive(Default)]
pub struct Regions {
    pub targets: Vec<(D2D_RECT_F, Target)>,
    pub scroll_areas: Vec<(D2D_RECT_F, ScrollArea, Extent)>,
    pub captions: Vec<CaptionRegion>,
    /// The sections whose lines show, in reading order, for selecting across them.
    pub sections: Vec<usize>,
    /// An area with no scroll position yet, drawn from where it shows a row it was opened at; it keeps that position.
    pub revealed: Option<(ScrollArea, f32)>,
}

/// Where a caption's text was drawn, for selecting it with the mouse.
#[derive(Clone, Copy, Debug)]
pub struct CaptionRegion {
    pub line: LineRef,
    /// The part on screen.
    pub visible: D2D_RECT_F,
    /// The whole row on screen, gutter included, which hovers the line and shows its buttons.
    pub row: D2D_RECT_F,
    /// Top-left of the text, which may be scrolled above `visible`, and its wrapping width.
    pub x: f32,
    pub y: f32,
    pub width: f32,
}

impl Regions {
    pub fn target_at(&self, x: f32, y: f32) -> Option<Target> {
        self.targets.iter().rev().find(|(rect, _)| contains(rect, x, y)).map(|(_, target)| *target)
    }

    /// Where `target` was drawn under a point.
    pub fn rect_at(&self, target: Target, x: f32, y: f32) -> Option<D2D_RECT_F> {
        self.targets.iter().rev().find(|(rect, t)| *t == target && contains(rect, x, y)).map(|(rect, _)| *rect)
    }

    /// Where `target` was drawn, e.g. to map a slider drag to a value.
    pub fn rect_of(&self, target: Target) -> Option<D2D_RECT_F> {
        self.targets.iter().find(|(_, t)| *t == target).map(|(rect, _)| *rect)
    }

    pub fn caption_at(&self, x: f32, y: f32) -> Option<CaptionRegion> {
        self.captions.iter().find(|c| contains(&c.visible, x, y)).copied()
    }

    /// The section whose header is under a point, buttons included.
    pub fn header_at(&self, x: f32, y: f32) -> Option<usize> {
        self.targets.iter().find_map(|(rect, target)| match target {
            Target::Header(index) if contains(rect, x, y) => Some(*index),
            _ => None,
        })
    }

    /// The line whose row is under a point, including the gutter where its buttons go.
    pub fn line_at(&self, x: f32, y: f32) -> Option<LineRef> {
        self.captions.iter().find(|c| contains(&c.row, x, y)).map(|c| c.line)
    }

    /// The caption nearest a point, for a drag that strays between lines or sections.
    pub fn nearest_caption(&self, x: f32, y: f32) -> Option<CaptionRegion> {
        let distance = |c: &CaptionRegion| {
            let dy = (c.visible.top - y).max(y - c.visible.bottom).max(0.0);
            let dx = (c.visible.left - x).max(x - c.visible.right).max(0.0);
            dy * 1000.0 + dx
        };
        self.captions.iter().min_by(|a, b| distance(a).total_cmp(&distance(b))).copied()
    }

    /// The innermost scroll area at a point, e.g. an open drop-down list over the settings.
    pub fn scroll_area_at(&self, x: f32, y: f32) -> Option<(ScrollArea, Extent)> {
        self.scroll_areas.iter().rev().find(|(rect, ..)| contains(rect, x, y)).map(|(_, area, extent)| (*area, *extent))
    }
}

/// How much content a scroll area holds versus how much fits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extent {
    pub content: f32,
    pub viewport: f32,
}

impl Extent {
    /// The furthest down the content can be scrolled: its end in view.
    pub fn max_top(&self) -> f32 {
        (self.content - self.viewport).max(0.0)
    }
}

/// A scroll position. Following areas stick to the newest (bottom) content.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scroll {
    top: f32,
    follow: bool,
}

impl Default for Scroll {
    fn default() -> Self {
        Self { top: 0.0, follow: true }
    }
}

impl Scroll {
    /// A position that starts at the top and stays put, for reading saved files.
    pub fn at_top() -> Self {
        Self::at(0.0)
    }

    /// A position `top` DIPs down that stays put; past the end it shows the end.
    pub fn at(top: f32) -> Self {
        Self { top: top.max(0.0), follow: false }
    }

    /// The top of the visible window into the content.
    pub fn top(&self, extent: Extent) -> f32 {
        if self.follow { extent.max_top() } else { self.top.min(extent.max_top()) }
    }

    /// Scrolls by `delta` DIPs (positive = down). Reaching the bottom resumes following.
    pub fn scroll_by(&mut self, delta: f32, extent: Extent) {
        self.top = (self.top(extent) + delta).clamp(0.0, extent.max_top());
        self.follow = self.top >= extent.max_top();
    }
}

/// A text box of the Words tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WordBox {
    Try,
    Heard,
    Fix,
}

/// A misheard word being corrected: what was heard, what it should be, and the app its line came from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Correction {
    pub heard: Field,
    pub fix: Field,
    /// As word rules name it: "You" for your lines.
    pub app: Option<String>,
    /// The rule applies only in `app`.
    pub only_app: bool,
}

/// The question asked before blocking an app.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockPrompt {
    pub section: usize,
    /// "Don't ask again" is ticked: the answer is kept for every app.
    pub dont_ask: bool,
}

/// How importing a model folder is going.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ImportStatus {
    #[default]
    Idle,
    Trying,
    Imported(String),
    Failed(String),
}

/// A short message in the title bar; errors are drawn in red and stay longer.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

/// A window's UI state: the panel's, or the settings window's.
pub struct View {
    pub screen: Screen,
    /// The settings window's open tab.
    pub tab: Tab,
    pub locked: bool,
    /// Shrunk to its title bar; how "–" minimizes when there is no tray icon to hide into.
    pub collapsed: bool,
    pub has_tray: bool,
    pub hovered: Option<Action>,
    /// The pointer has rested on `hovered` long enough to show its tooltip.
    pub tip: bool,
    /// Where the pointer is, in DIPs, so a tooltip goes under the button it is on.
    pub pointer: (f32, f32),
    /// The button the mouse went down on, drawn pushed in while the pointer stays on it.
    pub pressed: Option<Action>,
    /// The caption under the pointer, which shows its copy button.
    pub hovered_line: Option<LineRef>,
    /// The section header under the pointer, which shows its buttons.
    pub hovered_header: Option<usize>,
    pub folds: Folds,
    pub blocking: Option<BlockPrompt>,
    /// Asking whether to quit while a download runs.
    pub quit_prompt: bool,
    /// A newer version, offered over the panel.
    pub update: Option<Release>,
    /// Where a check asked for by hand is.
    pub update_check: Option<UpdateCheck>,
    /// Files waiting for a second press of Delete.
    pub confirm_delete: Option<Download>,
    /// The settings window is walking through setup, at this step.
    pub setup: Option<SetupStep>,
    /// The pointer is over the title bar, where dragging moves the panel.
    pub drag_hover: bool,
    /// A short message replacing the status, e.g. after saving.
    pub notice: Option<Notice>,
    /// Past sessions for the history screen, newest first.
    pub saved: Vec<Session>,
    /// The history screen's search and SQL.
    pub finder: Finder,
    pub selection: Option<Selection>,
    pub open_dropdown: Option<Dropdown>,
    /// Fonts for captions mode, the preset ones first; listed when its font drop-down first opens.
    pub fonts: Vec<String>,
    /// What is typed in the font list's search field, and the fonts matching it as indices into `fonts`.
    pub font_search: Field,
    pub font_matches: Vec<usize>,
    /// The Words tab's Try box, its correction, and which of their boxes keys go to.
    pub try_words: Field,
    pub correction: Correction,
    pub typing_word: Option<WordBox>,
    /// The shortcut waiting for keys to be pressed, if any.
    pub rebinding: Option<Rebind>,
    pub scrolls: HashMap<ScrollArea, Scroll>,
    pub regions: Regions,
}

impl View {
    pub fn new() -> Self {
        Self {
            screen: Screen::Live,
            tab: Tab::General,
            locked: false,
            collapsed: false,
            has_tray: false,
            hovered: None,
            tip: false,
            pointer: (0.0, 0.0),
            pressed: None,
            hovered_line: None,
            hovered_header: None,
            folds: Folds::default(),
            blocking: None,
            quit_prompt: false,
            update: None,
            update_check: None,
            confirm_delete: None,
            setup: None,
            drag_hover: false,
            notice: None,
            saved: Vec::new(),
            finder: Finder::default(),
            selection: None,
            rebinding: None,
            open_dropdown: None,
            fonts: Vec::new(),
            font_search: Field::default(),
            font_matches: Vec::new(),
            try_words: Field::default(),
            correction: Correction::default(),
            typing_word: None,
            scrolls: HashMap::new(),
            regions: Regions::default(),
        }
    }

    pub fn scroll(&self, area: ScrollArea) -> Scroll {
        self.scrolls.get(&area).copied().unwrap_or_else(|| first_scroll(area))
    }

    pub fn word_box(&self, which: WordBox) -> &Field {
        match which {
            WordBox::Try => &self.try_words,
            WordBox::Heard => &self.correction.heard,
            WordBox::Fix => &self.correction.fix,
        }
    }

    pub fn word_box_mut(&mut self, which: WordBox) -> &mut Field {
        match which {
            WordBox::Try => &mut self.try_words,
            WordBox::Heard => &mut self.correction.heard,
            WordBox::Fix => &mut self.correction.fix,
        }
    }

    /// Scrolls the innermost scroll area at a point by `delta` DIPs (positive = down); returns whether it moved.
    pub fn scroll_at(&mut self, x: f32, y: f32, delta: f32) -> bool {
        let Some((area, extent)) = self.regions.scroll_area_at(x, y) else { return false };
        let scroll = self.scrolls.entry(area).or_insert_with(|| first_scroll(area));
        let before = *scroll;
        scroll.scroll_by(delta, extent);
        *scroll != before
    }

    /// Opens the font list with nothing searched, scrolled to the font `chosen`.
    pub fn open_font_list(&mut self, chosen: &str) {
        self.open_dropdown = Some(Dropdown::Font);
        self.font_search.clear();
        self.search_fonts();
        let row = self.fonts.iter().position(|font| font == chosen).unwrap_or(0);
        self.scrolls.insert(ScrollArea::Dropdown(Dropdown::Font), font_list_scroll(row));
    }

    /// Lists the fonts whose names hold what is typed in the search, ignoring case, from the top of the list.
    pub fn search_fonts(&mut self) {
        let needle = self.font_search.text.to_lowercase();
        self.font_matches = (0..self.fonts.len()).filter(|&i| needle.is_empty() || self.fonts[i].to_lowercase().contains(&needle)).collect();
        self.scrolls.insert(ScrollArea::Dropdown(Dropdown::Font), Scroll::at_top());
    }
}

/// Where an area starts before it is scrolled: settings and history, newest first, read from the top; the
/// rest follow their newest lines.
fn first_scroll(area: ScrollArea) -> Scroll {
    match area {
        ScrollArea::Settings | ScrollArea::SavedList | ScrollArea::Results | ScrollArea::Table => Scroll::at_top(),
        _ => Scroll::default(),
    }
}

fn contains(rect: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXTENT: Extent = Extent { content: 500.0, viewport: 100.0 };

    #[test]
    fn following_scroll_shows_the_newest_content() {
        assert_eq!(Scroll::default().top(EXTENT), 400.0);
    }

    #[test]
    fn scrolling_up_stops_following_and_reaching_bottom_resumes() {
        let mut scroll = Scroll::default();
        scroll.scroll_by(-150.0, EXTENT);
        assert_eq!(scroll.top(EXTENT), 250.0);
        assert_eq!(scroll.top(Extent { content: 600.0, ..EXTENT }), 250.0, "new content must not move a scrolled view");
        scroll.scroll_by(1000.0, EXTENT);
        assert_eq!(scroll.top(Extent { content: 600.0, ..EXTENT }), 500.0);
    }

    #[test]
    fn the_font_search_ignores_case_and_opening_the_list_clears_it() {
        let mut view = View::new();
        view.fonts = ["Arial", "Open Sans", "Segoe UI", "Noto Sans"].map(str::to_owned).into();
        view.font_search.set("sAnS");
        view.search_fonts();
        assert_eq!(view.font_matches, [1, 3]);
        view.open_font_list("Segoe UI");
        assert_eq!((view.font_search.text.as_str(), view.font_matches.len()), ("", 4));
    }

    #[test]
    fn the_button_gutter_hovers_a_line_but_selects_no_text() {
        let line = LineRef { section: 1, stream: 1, id: 7 };
        let visible = D2D_RECT_F { left: 0.0, top: 0.0, right: 100.0, bottom: 20.0 };
        let row = D2D_RECT_F { right: 156.0, ..visible };
        let regions = Regions { captions: vec![CaptionRegion { line, visible, row, x: 0.0, y: 0.0, width: 100.0 }], ..Default::default() };
        assert_eq!(regions.line_at(140.0, 10.0), Some(line));
        assert!(regions.caption_at(140.0, 10.0).is_none());
        assert_eq!(regions.line_at(50.0, 10.0), Some(line));
    }
}
