//! The Macros tab's drawing: the macros listed with a search, and the editor with the phrases, the step list
//! and the Add step menu. A step's text shows only in the editor, which is where it is being written.

use windows::{core::Result, Win32::Graphics::Direct2D::Common::D2D_RECT_F};

use kiku_core::{
    actions::escape,
    macros::{Command, RunOutput},
};

use super::{
    design::{CONTROL, GAP, PADDING, RADIUS_CARD, RADIUS_CONTROL, SETTING_ROW},
    clipped, heading_control, height_of,
    history::BoxRole,
    list_top, metrics, rect, height, width, Emphasis, Panel, Renderer, RowButton, Tone, BUTTON, FOOTER_HEIGHT, LIST_ROW, SECTION_GAP, MODEL_ROW, TITLE_HEIGHT,
};
use crate::overlay::{
    field::Field,
    macro_tab::{EditedStep, Editor, MacroAction, MacroBox, Phrase, Recording, StepKind},
    palette::Palette,
    render::Icon,
    view::{Action, Extent, Regions, ScrollArea, Target},
};

/// Macros the file couldn't read that the tab names; the log has every one.
const SKIPPED_SHOWN: usize = 3;
/// The room for a step's kind before its box.
const KIND_WIDTH: f32 = 112.0;

impl Renderer {
    /// Returns the y below the last row.
    pub(super) fn draw_macro_settings(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let tab = &panel.view.macros;
        let y = match tab.editor() {
            Some(editor) => self.draw_macro_editor(editor, panel, content, y, regions)?,
            None => self.draw_macro_list(panel, content, y, regions)?,
        };
        match tab.problem().filter(|_| tab.shortcut_problem().is_none()) {
            Some(problem) => self.draw_problem(problem, content, y),
            None => Ok(y),
        }
    }

    /// A search over the macros: each one's name and phrases, whether it is on, and its buttons.
    fn draw_macro_list(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let tab = &panel.view.macros;
        let (right, middle) = heading_control(content, y);
        let left = self.draw_text_button("New macro", Some(Icon::Plus), Action::Macro(MacroAction::New), Emphasis::Primary, right, middle, panel, regions)?;
        self.draw_text_button("Ask Claude", Some(Icon::Sparkles), Action::AskForMacro, Emphasis::Secondary, left - 8.0, middle, panel, regions)?;
        let y = self.draw_heading_with_control("Macros", content, y)?;
        let search = rect(content.left, y, content.right, y + CONTROL);
        let mut y = self.draw_macro_box(tab.search(), "Search by name or phrase", MacroBox::Search, Some(Icon::Search), search, panel, regions)?;
        y = self.draw_heading("Wake word for macros said without the key", content, y + SECTION_GAP)?;
        y = self.draw_macro_box(tab.wake_field(), "e.g. computer (none by default)", MacroBox::Wake, None, rect(content.left, y, content.right, y), panel, regions)?;
        y = self.draw_hint("Macros that listen run only when a line starts with this word. Empty: any line that is a macro's phrase runs it.", content, y)? + SECTION_GAP;
        let listed = tab.listed();
        if listed.is_empty() {
            let hint = if tab.search().text.trim().is_empty() {
                "No macros yet. New macro makes one: a phrase you say that runs steps (paste text, press keys, attach files) where you dictate."
            } else {
                "No macro has that in its name or phrases."
            };
            y = self.draw_hint(hint, content, y)?;
        } else {
            y = self.draw_card(content, y + 8.0, |renderer, inner, mut y| {
                for &(at, known) in &listed {
                    let state = if known.enabled { ("On", Emphasis::Primary) } else { ("Off", Emphasis::Secondary) };
                    let delete = if tab.armed() == Some(at) { ("Press Delete again", Emphasis::Danger) } else { ("Delete", Emphasis::Ghost) };
                    let buttons: [RowButton; 3] = [
                        (state.0, None, Action::Macro(MacroAction::Toggle(at)), state.1),
                        ("Edit", None, Action::Macro(MacroAction::Edit(at)), Emphasis::Secondary),
                        (delete.0, Some(Icon::Trash), Action::Macro(MacroAction::Delete(at)), delete.1),
                    ];
                    let phrases: Vec<&str> = known.say.iter().map(String::as_str).chain(known.listen.then_some("no key needed")).collect();
                    let keys: Vec<String> = known.hotkey.as_ref().map(|hotkey| hotkey.name().split('+').map(str::to_owned).collect()).unwrap_or_default();
                    y = renderer.draw_keyed_row(&known.name, &phrases.join("  ·  "), &keys, &buttons, inner, y, panel, regions)?;
                }
                Ok(y)
            })?;
        }
        y = self.draw_hint("Macros are kept in custom\\macros.toml, which you or an assistant can edit too.", content, y)?;
        if !tab.skipped().is_empty() {
            let mut skipped: Vec<String> = tab.skipped().iter().take(SKIPPED_SHOWN).map(|(place, why)| format!("{place}: {why}")).collect();
            if tab.skipped().len() > SKIPPED_SHOWN {
                skipped.push(format!("{} more in Kiku's log", tab.skipped().len() - SKIPPED_SHOWN));
            }
            y = self.draw_hint(&format!("Skipped {}.", skipped.join("; ")), content, y)?;
        }
        y = self.draw_programs(panel, content, y + SECTION_GAP, regions)?;
        self.draw_builtin_phrases(panel, content, y + SECTION_GAP, regions)
    }

    /// The switches for running programs, and the commands waiting for the user's approval.
    fn draw_programs(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let tab = &panel.view.macros;
        let y = self.draw_heading("Programs", content, y)?;
        let y = self.draw_card(content, y, |renderer, inner, mut y| {
            let policy = tab.policy();
            y = renderer.draw_toggle_row("Let macros run programs", policy.enabled, Action::Macro(MacroAction::ToggleRunPrograms), inner, y, panel, regions)?;
            if policy.enabled {
                y = renderer.draw_toggle_row("…from anywhere, not only custom\\scripts\\", policy.anywhere, Action::Macro(MacroAction::ToggleRunAnywhere), inner, y, panel, regions)?;
            }
            for (at, (name, command)) in tab.unapproved().iter().enumerate() {
                let approve: RowButton = ("Approve", Some(Icon::Check), Action::Macro(MacroAction::Approve(at)), Emphasis::Primary);
                y = renderer.draw_command_row(&format!("{name}: needs approval"), command, Some(approve), inner, y, panel, regions)?;
            }
            for (name, command) in tab.missing_programs() {
                y = renderer.draw_command_row(&format!("{name}: program not found, so it won't run"), command, None, inner, y, panel, regions)?;
            }
            Ok(y)
        })?;
        self.draw_hint("A run step starts a program only while this is on, from custom\\scripts\\ unless allowed from anywhere, and once you approve its exact command here. A changed command or program asks again.", content, y)
    }

    /// A command's macro and the whole command, nothing cut: the program as written, each argument on a line
    /// of its own with quotes and backslashes escaped, and the output. Returns the y below.
    #[allow(clippy::too_many_arguments)]
    fn draw_command_row(&mut self, title: &str, command: &Command, button: Option<RowButton>, content: D2D_RECT_F, y: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        self.begin_row(content, y)?;
        let mut right = content.right;
        if let Some((label, icon, action, emphasis)) = button {
            right = self.draw_text_button(label, icon, action, emphasis, right, y + MODEL_ROW / 2.0, panel, regions)? - 6.0;
        }
        let name = self.layout(&self.fonts.body, title, right - content.left - 8.0)?;
        let detail = self.layout(&self.fonts.small, &command_text(command), right - content.left - 8.0)?;
        let top = y + 8.0;
        self.draw_layout(&name, content.left, top, self.palette.text)?;
        self.draw_layout(&detail, content.left, top + height_of(&name)?, self.palette.text_dim)?;
        Ok((top + height_of(&name)? + height_of(&detail)? + 8.0).max(y + MODEL_ROW))
    }

    /// A switch for each phrase Kiku hears itself.
    fn draw_builtin_phrases(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let tab = &panel.view.macros;
        let y = self.draw_heading("Phrases Kiku knows", content, y)?;
        let y = self.draw_card(content, y, |renderer, inner, mut y| {
            for phrase in Phrase::ALL {
                y = renderer.draw_toggle_row(phrase.label(), phrase.is_on(&tab.phrases()), Action::Macro(MacroAction::TogglePhrase(phrase)), inner, y, panel, regions)?;
            }
            Ok(y)
        })?;
        self.draw_hint("Said as a whole line while you dictate, in any mode. Turned off, the words are dictated like any others.", content, y)
    }

    /// The name, what to say, the steps with their Add step menu, then Save and Back.
    fn draw_macro_editor(&mut self, editor: &Editor, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let tab = &panel.view.macros;
        let y = self.draw_heading("Name", content, y)?;
        let mut y = self.draw_macro_box(&editor.name, "e.g. Paste my email", MacroBox::Name, None, rect(content.left, y, content.right, y), panel, regions)?;

        y = self.draw_heading("Say", content, y + SECTION_GAP)?;
        for (at, phrase) in editor.say.iter().enumerate() {
            let placeholder = if at == 0 { "What you say, e.g. paste my email" } else { "Another way to say it" };
            y = self.draw_macro_box(phrase, placeholder, MacroBox::Say(at), None, rect(content.left, y, content.right, y), panel, regions)? + 8.0;
        }
        y = self.draw_hint("Say it as a whole line of dictation. {words} in it takes the rest of the line, {n:number} a number.", content, y - 8.0)?;

        y = self.draw_heading("Where", content, y + SECTION_GAP)?;
        y = self.draw_macro_box(&editor.apps, "Everywhere", MacroBox::Apps, None, rect(content.left, y, content.right, y), panel, regions)?;
        y = self.draw_hint("Apps it runs in, apart by commas, e.g. Discord, Chrome. Empty runs everywhere.", content, y)?;

        y = self.draw_heading("Trigger", content, y + SECTION_GAP)?;
        y = self.draw_card(content, y, |renderer, inner, mut y| {
            y = renderer.draw_macro_shortcut(editor, inner, y, panel, regions)?;
            if let Some(why) = tab.shortcut_problem() {
                y = renderer.draw_problem(why, inner, y - 8.0)? + 4.0;
            }
            renderer.draw_toggle_row("Say it without the key", editor.listen, Action::Macro(MacroAction::ToggleListen), inner, y, panel, regions)
        })?;
        let hint = tab.listen_note().unwrap_or("A shortcut runs the macro alone. Say it without the key runs it when a whole line you say matches a phrase.");
        y = self.draw_hint(hint, content, y)?;
        if !tab.wake().trim().is_empty() {
            y = self.draw_hint("Uses the wake word set on the Macros list.", content, y - 4.0)?;
        }

        let (right, middle) = heading_control(content, y + SECTION_GAP);
        let button = rect(right - BUTTON, middle - CONTROL / 2.0, right, middle + CONTROL / 2.0);
        self.draw_text_button("Add step", Some(Icon::Plus), Action::Macro(MacroAction::AddStepMenu), Emphasis::Secondary, right, middle, panel, regions)?;
        y = self.draw_heading_with_control("Steps", content, y + SECTION_GAP)?;
        if editor.steps.is_empty() {
            y = self.draw_hint("No steps yet. Add step chooses what happens, in order, when the macro runs.", content, y - 8.0)?;
        } else {
            y = self.draw_card(content, y, |renderer, inner, mut y| {
                for (at, step) in editor.steps.iter().enumerate() {
                    y = renderer.draw_macro_step(at, step, inner, y, panel, regions)?;
                }
                Ok(y)
            })?;
            y = self.draw_hint("Text may hold variables such as {date} or {clipboard}. A shortcut may be several, apart by commas.", content, y)?;
            if editor.steps.iter().any(|step| step.kind == StepKind::Run) {
                y = self.draw_hint("The program, from custom\\scripts\\, then one argument per box; each is passed as is, never through a shell.", content, y - 4.0)?;
            }
        }

        let middle = y + 8.0 + CONTROL / 2.0 + SECTION_GAP / 2.0;
        let left = self.draw_text_button("Save", Some(Icon::Check), Action::Macro(MacroAction::Save), Emphasis::Primary, content.right - GAP, middle, panel, regions)?;
        self.draw_text_button("Back", Some(Icon::ChevronLeft), Action::Macro(MacroAction::Back), Emphasis::Secondary, left - 8.0, middle, panel, regions)?;
        y = middle + CONTROL / 2.0;
        if tab.menu_open() {
            self.draw_add_step_menu(button, content.bottom, panel, regions)?;
        }
        Ok(y)
    }

    /// The macros that can be said here, in a popover under the title bar; a click runs one. Past the rows
    /// that fit above the footer the entries scroll.
    pub(super) fn draw_macro_palette(&mut self, palette: &Palette, panel: &Panel, regions: &mut Regions) -> Result<()> {
        const WIDTH: f32 = 360.0;
        let area = rect((panel.width - PADDING - WIDTH).max(PADDING), TITLE_HEIGHT + 4.0, panel.width - PADDING, panel.height - FOOTER_HEIGHT);
        let fit = ((height(&area) - LIST_ROW - 8.0) / LIST_ROW).floor().max(1.0) as usize;
        let shown = palette.entries.len().clamp(1, fit);
        let extent = Extent { content: LIST_ROW * palette.entries.len() as f32, viewport: LIST_ROW * shown as f32 };
        let list = rect(area.left, area.top, area.right, area.top + LIST_ROW * (shown + 1) as f32 + 8.0);
        self.popover(list, RADIUS_CARD)?;
        regions.targets.push((list, Target::Card));
        let place = if palette.app.is_empty() { "here".to_owned() } else { format!("in {}", palette.app) };
        let title = self.single_line(&self.fonts.small, &format!("You can say, {place}"), width(&list) - 24.0)?;
        self.draw_layout(&title, list.left + 12.0, list.top + 4.0 + (LIST_ROW - height_of(&title)?) / 2.0, self.palette.text_dim)?;
        if palette.entries.is_empty() {
            let none = self.single_line(&self.fonts.body, "No macro is on for this app and mode", width(&list) - 24.0)?;
            return self.draw_layout(&none, list.left + 12.0, list.top + 4.0 + LIST_ROW + (LIST_ROW - height_of(&none)?) / 2.0, self.palette.text);
        }
        let rows = rect(list.left, list.top + 4.0 + LIST_ROW, list.right, list.bottom - 4.0);
        regions.scroll_areas.push((rows, ScrollArea::Palette, extent));
        let top = panel.view.scroll(ScrollArea::Palette).top(extent);
        let visible = (top / LIST_ROW) as usize..(((top + extent.viewport) / LIST_ROW).ceil() as usize).min(palette.entries.len());
        self.push_clip(rows);
        for (at, entry) in palette.entries.iter().enumerate().take(visible.end).skip(visible.start) {
            let row_top = rows.top - top + LIST_ROW * at as f32;
            let row = rect(list.left + 4.0, row_top, list.right - 4.0, row_top + LIST_ROW);
            let middle = (row.top + row.bottom) / 2.0;
            let action = Action::RunFromPalette(at);
            if panel.view.hovered == Some(action) {
                self.fill_rounded(row, RADIUS_CONTROL - 2.0, self.palette.hover)?;
            }
            let half = (width(&row) - 16.0) / 2.0;
            let mut left = row.right - 8.0;
            if let Some(phrase) = &entry.phrase {
                let said = self.single_line(&self.fonts.small, &format!("“{phrase}”"), half - 8.0)?;
                left -= metrics(&said)?.width;
                self.draw_layout(&said, left, middle - height_of(&said)? / 2.0, self.palette.text_dim)?;
            } else if let Some(hotkey) = &entry.hotkey {
                let keys: Vec<String> = hotkey.name().split('+').map(str::to_owned).collect();
                left = self.draw_key_caps(&keys, left, middle)?;
            }
            let name = self.single_line(&self.fonts.body, &entry.name, (left - row.left - 16.0).max(1.0))?;
            self.draw_layout(&name, row.left + 8.0, middle - height_of(&name)? / 2.0, self.palette.text)?;
            regions.targets.push((clipped(row, rows), Target::Click(action)));
        }
        self.pop_clip();
        self.draw_scroll_thumb(list.right - 4.0, rows, extent, top)
    }

    /// The macro's shortcut, or None, with its Record and Clear buttons. Returns the y below.
    fn draw_macro_shortcut(&mut self, editor: &Editor, content: D2D_RECT_F, y: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        self.draw_row_label("Shortcut", content, y)?;
        let middle = y + SETTING_ROW / 2.0;
        let mut right = content.right;
        if editor.hotkey.is_some() {
            right = self.draw_text_button("Clear", None, Action::Macro(MacroAction::ClearHotkey), Emphasis::Ghost, right, middle, panel, regions)? - 6.0;
        }
        let (label, emphasis) = if panel.view.macros.recording() == Some(Recording::Hotkey) { ("Press the keys…", Emphasis::Primary) } else { ("Record", Emphasis::Secondary) };
        right = self.draw_text_button(label, Some(Icon::Keyboard), Action::Macro(MacroAction::RecordHotkey), emphasis, right, middle, panel, regions)? - 6.0;
        let shown = self.single_line(&self.fonts.body, editor.hotkey.as_deref().unwrap_or("None"), (right - content.left - KIND_WIDTH).max(0.0))?;
        self.draw_layout(&shown, content.left + KIND_WIDTH, middle - height_of(&shown)? / 2.0, self.palette.text_dim)?;
        Ok(y + SETTING_ROW)
    }

    /// A step's kind, its box, and its Record (a press) and remove buttons. Returns the y below.
    #[allow(clippy::too_many_arguments)]
    fn draw_macro_step(&mut self, at: usize, step: &EditedStep, content: D2D_RECT_F, y: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        let kind = step.kind;
        self.begin_row(content, y)?;
        let middle = y + SETTING_ROW / 2.0;
        let remove = rect(content.right + 6.0 - BUTTON, middle - BUTTON / 2.0, content.right + 6.0, middle + BUTTON / 2.0);
        self.icon_button(remove, Icon::Trash, Action::Macro(MacroAction::RemoveStep(at)), Tone::Dim, panel, regions)?;
        let mut right = remove.left - 6.0;
        if kind == StepKind::Press {
            let (label, emphasis) = if panel.view.macros.recording() == Some(Recording::Step(at)) { ("Press the keys…", Emphasis::Primary) } else { ("Record", Emphasis::Secondary) };
            right = self.draw_text_button(label, Some(Icon::Keyboard), Action::Macro(MacroAction::Record(at)), emphasis, right, middle, panel, regions)? - 6.0;
        }
        if kind == StepKind::Run {
            let output = format!("Output: {}", step.output());
            right = self.draw_text_button(&output, None, Action::Macro(MacroAction::CycleOutput(at)), Emphasis::Secondary, right, middle, panel, regions)? - 6.0;
        }
        let kind_label = self.single_line(&self.fonts.small, kind.label(), KIND_WIDTH - 8.0)?;
        self.draw_layout(&kind_label, content.left, middle - height_of(&kind_label)? / 2.0, self.palette.text_dim)?;
        let area = rect(content.left + KIND_WIDTH, middle - CONTROL / 2.0, right, middle + CONTROL / 2.0);
        self.draw_macro_box(&step.text, kind.placeholder(), MacroBox::Step(at), None, area, panel, regions)?;
        let mut y = y + SETTING_ROW;
        for (place, argument) in step.args.iter().enumerate() {
            self.begin_row(content, y)?;
            let middle = y + SETTING_ROW / 2.0;
            let area = rect(content.left + KIND_WIDTH, middle - CONTROL / 2.0, content.right, middle + CONTROL / 2.0);
            self.draw_macro_box(argument, "Argument", MacroBox::Argument { step: at, at: place }, None, area, panel, regions)?;
            y += SETTING_ROW;
        }
        Ok(y)
    }

    /// The choices of the Add step menu in two columns by its `button` (see [`list_top`]), kept above `bottom`.
    fn draw_add_step_menu(&mut self, button: D2D_RECT_F, bottom: f32, panel: &Panel, regions: &mut Regions) -> Result<()> {
        const COLUMN: f32 = 190.0;
        let rows = StepKind::ALL.len().div_ceil(2);
        let tall = LIST_ROW * rows as f32 + 8.0;
        let top = list_top(button, tall, bottom);
        let list = rect(button.right - 2.0 * COLUMN, top, button.right, top + tall);
        self.popover(list, RADIUS_CARD)?;
        // Clicks between the rows keep the menu open and reach nothing under it.
        regions.targets.push((list, Target::Card));
        for (place, kind) in StepKind::ALL.into_iter().enumerate() {
            let (column, line) = (place / rows, place % rows);
            let (left, row_top) = (list.left + COLUMN * column as f32, list.top + 4.0 + LIST_ROW * line as f32);
            let row = rect(left + 4.0, row_top, left + COLUMN - 4.0, row_top + LIST_ROW);
            let action = Action::Macro(MacroAction::AddStep(kind));
            if panel.view.hovered == Some(action) {
                self.fill_rounded(row, RADIUS_CONTROL - 2.0, self.palette.hover)?;
            }
            let label = self.single_line(&self.fonts.body, kind.label(), width(&row) - 16.0)?;
            self.draw_layout(&label, row.left + 8.0, (row.top + row.bottom) / 2.0 - height_of(&label)? / 2.0, self.palette.text)?;
            regions.targets.push((row, Target::Click(action)));
        }
        Ok(())
    }

    /// One of the tab's boxes across `area`: keys go to it once clicked. Returns the y below it.
    #[allow(clippy::too_many_arguments)]
    fn draw_macro_box(&mut self, field: &Field, placeholder: &str, which: MacroBox, icon: Option<Icon>, area: D2D_RECT_F, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        let role = BoxRole { one_line: true, icon, typing: panel.view.macros.focus() == Some(which), click: Some(Action::Macro(MacroAction::Focus(which))), clear: None };
        Ok(self.draw_text_box(field, placeholder, area, role, panel, regions)?.bottom)
    }

    /// Why a change was refused, in the warning colour. Returns the y below.
    fn draw_problem(&mut self, problem: &str, content: D2D_RECT_F, y: f32) -> Result<f32> {
        let problem = self.layout(&self.fonts.small, problem, width(&content) - 8.0)?;
        self.draw_layout(&problem, content.left + 4.0, y + 8.0, self.palette.warn)?;
        Ok(y + 8.0 + height_of(&problem)?)
    }
}

/// What a macros button does, for its tooltip.
pub(super) fn tooltip(action: MacroAction) -> Option<String> {
    Some(match action {
        MacroAction::New => "Make a macro: a phrase you say that runs steps".into(),
        MacroAction::Edit(_) => "Change this macro".into(),
        MacroAction::Toggle(_) => "On or off: a macro that is off is kept but never runs".into(),
        MacroAction::Delete(_) => "Delete this macro from macros.toml".into(),
        MacroAction::Approve(_) => "Allow this exact command and this version of its program to run".into(),
        MacroAction::ToggleRunPrograms => "Off: run steps only show a notice. On: approved programs may run".into(),
        MacroAction::ToggleRunAnywhere => "Also allow programs outside custom\\scripts\\ (each command still needs approving)".into(),
        MacroAction::TogglePhrase(_) => "Whether Kiku acts on this phrase or dictates it".into(),
        MacroAction::CycleOutput(_) => "What happens to what the program prints: nothing, paste it or show it as a notice".into(),
        MacroAction::AddStepMenu => "Choose what the next step does".into(),
        MacroAction::RemoveStep(_) => "Remove this step".into(),
        MacroAction::Record(_) => "Click, then press the keys (Esc cancels)".into(),
        MacroAction::RecordHotkey => "Click, then press the keys of the shortcut (Esc cancels)".into(),
        MacroAction::ClearHotkey => "Take the shortcut away".into(),
        MacroAction::ToggleListen => "Run this macro when you say one of its phrases as a whole line, with no key held".into(),
        MacroAction::Save => "Write this macro to macros.toml".into(),
        MacroAction::Back => "Leave without saving".into(),
        MacroAction::Focus(_) | MacroAction::AddStep(_) => return None,
    })
}

/// A command as it is approved, a line each: the program, every argument quoted and escaped so none reads
/// as two, and the output.
fn command_text(command: &Command) -> String {
    let output = match &command.output {
        RunOutput::Paste => "paste".to_owned(),
        RunOutput::Notify => "notify".to_owned(),
        RunOutput::Variable(name) => format!("variable:{name}"),
        RunOutput::None => "none".to_owned(),
    };
    let args = command.written.iter().map(|arg| format!("argument: \"{}\"", escape(arg).replace('"', "\\\"")));
    [format!("program: {}", command.program.display())].into_iter().chain(args).chain([format!("output: {output}")]).collect::<Vec<_>>().join("\n")
}
