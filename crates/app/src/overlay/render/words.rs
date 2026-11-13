//! The Words tab's drawing: correcting a misheard word, where word rules and transforms apply, the Try box
//! and what it would become, the file with its buttons, and the file's entries by section, each removable.

use windows::{core::Result, Win32::Graphics::Direct2D::Common::D2D_RECT_F};

use kiku_core::{
    actions::Effect,
    captions::YOU,
    words::{strength_text, Kind, Scope, STRENGTHS},
};

use super::{
    design::{CONTROL, SETTING_ROW},
    height_of,
    history::BoxRole,
    metrics, rect, strength_fraction, width, Emphasis, Panel, Renderer, Tone, BUTTON, KNOB, SECTION_GAP, TRACK,
};
use crate::overlay::{
    render::Icon,
    view::{Action, Regions, Slider, Target, WordBox},
};

/// Lines the file couldn't read that the tab names; the log has every one.
const SKIPPED_SHOWN: usize = 3;

impl Renderer {
    /// Returns the y below the last row.
    pub(super) fn draw_word_settings(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let settings = panel.word_settings;
        let y = self.draw_new_word(panel, content, y, regions)?;
        let y = self.draw_correction(panel, content, y + SECTION_GAP, regions)?;
        // The card the hotword, suggestion and correction settings join.
        let mut y = self.draw_heading("Where they apply", content, y + SECTION_GAP)?;
        y = self.draw_card(content, y, |renderer, inner, mut y| {
            for (label, transforms, chosen) in [("Word rules apply to", false, settings.rules), ("Transforms apply to", true, settings.transforms)] {
                let choices = Scope::ALL.map(|scope| (scope.name(), scope == chosen, Action::WordScope { transforms, scope }));
                y = renderer.draw_choice_row(label, &choices, inner, y, panel, regions)?;
            }
            y = renderer.draw_toggle_row("Listen for @ words", settings.hotwords, Action::WordHotwords(!settings.hotwords), inner, y, panel, regions)?;
            let strength = settings.strength;
            renderer.draw_slider_row("Strength", &strength_text(strength), strength_fraction(strength), Slider::Strength, inner, y, regions)
        })?;
        let hint = "Captions are what the panel, captions mode, History and the MCP server get; dictation is what Kiku types or pastes. \
                    @ words are listened for in every app, by Parakeet Unified only, as strongly as Strength says (1.5 at first). The \
                    stronger, the likelier they are heard; toward 4 they start to be heard where they weren't said. Each word below \
                    can have a strength of its own.";
        y = self.draw_hint(hint, content, y)?;

        y = self.draw_heading("Try", content, y + SECTION_GAP)?;
        let view = panel.view;
        let role = BoxRole { icon: Some(Icon::Search), ..word_box_role(WordBox::Try, panel) };
        let area = rect(content.left, y, content.right, y + CONTROL);
        y = self.draw_text_box(&view.try_words, "Type what was said, to see what Kiku makes of it", area, role, panel, regions)?.bottom;
        if !view.try_words.text.trim().is_empty() {
            let preview = panel.macros.preview_rows(&view.try_words.text, &settings);
            y = self.draw_card(content, y + 8.0, |renderer, inner, mut y| {
                for (label, text) in preview {
                    let text = if text.is_empty() { "(nothing)" } else { &text };
                    y = renderer.draw_two_line_row(text, &label, &[], inner, y, panel, regions)?;
                }
                Ok(y)
            })?;
        }

        y = self.draw_heading("The file", content, y + SECTION_GAP)?;
        y = self.draw_card(content, y, |renderer, inner, y| {
            let open = [("Open file", None, Action::OpenWords, Emphasis::Secondary)];
            let y = renderer.draw_two_line_row("words.txt", "Kiku reloads it whenever it is saved", &open, inner, y, panel, regions)?;
            let copy = [("Copy instructions", Some(Icon::Copy), Action::CopyWordInstructions, Emphasis::Secondary)];
            renderer.draw_two_line_row("Ask an assistant", "How the file works, for you to add what you want", &copy, inner, y, panel, regions)
        })?;
        let hint = "One entry a line: kicku => Kiku swaps words (nothing after => deletes them), @ Tannhauser is a word to listen for \
                    (:2.5 after it gives it a strength of its own), \
                    ! lowercase, ! no-trailing-punctuation and ! capitalise change the whole line, and [Discord] starts what applies only to Discord ([You] is your mic).";
        y = self.draw_hint(hint, content, y)?;
        self.draw_word_entries(panel, content, y + SECTION_GAP, regions)
    }

    /// A word to listen for, typed with how strongly, and added on its own. Returns the y below.
    fn draw_new_word(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let view = panel.view;
        let mut y = self.draw_heading("Listen for a word", content, y)?;
        let button = self.draw_text_button("Add hotword", None, Action::AddWord, Emphasis::Secondary, content.right, y + CONTROL / 2.0, panel, regions)?;
        let area = rect(content.left, y, button - 8.0, y + CONTROL);
        y = self.draw_text_box(&view.new_word, "e.g. Lull", area, word_box_role(WordBox::Hotword, panel), panel, regions)?.bottom;
        let strength = view.new_strength.unwrap_or(panel.word_settings.strength);
        y = self.draw_card(content, y + 8.0, |renderer, inner, y| {
            renderer.draw_slider_row("Strength", &strength_text(strength), strength_fraction(strength), Slider::NewWordStrength, inner, y, regions)
        })?;
        self.draw_hint("Names and terms the model should listen out for.", content, y)
    }

    /// What was heard and what it should be, side by side, then making it a rule (everywhere, or only where
    /// its line came from) or a hotword, each added on its own and explained. Returns the y below.
    fn draw_correction(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        const GAP: f32 = 8.0;
        let (view, correction) = (panel.view, &panel.view.correction);
        let mut y = self.draw_heading("Fix a mistake", content, y)?;
        let half = (width(&content) - GAP) / 2.0;
        let boxes = [(WordBox::Heard, "Heard", "e.g. dictionary", content.left), (WordBox::Fix, "Should be", "e.g. fictionary", content.left + half + GAP)];
        let mut bottom = y;
        for (which, label, placeholder, left) in boxes {
            let label = self.single_line(&self.fonts.small, label, half)?;
            self.draw_layout(&label, left + 2.0, y, self.palette.text_dim)?;
            let top = y + height_of(&label)? + 4.0;
            let area = rect(left, top, left + half, top + CONTROL);
            bottom = self.draw_text_box(view.word_box(which), placeholder, area, word_box_role(which, panel), panel, regions)?.bottom;
        }
        y = bottom + 12.0;
        let (heard, fix) = (quoted(&correction.heard.text, "what was heard"), quoted(&correction.fix.text, "what it should be"));
        let (rule, hotword) = (format!("{heard} becomes {fix} once the model writes it"), "The model listens out for it as it works out what was said".to_owned());
        y = self.draw_card(content, y, |renderer, inner, mut y| {
            y = renderer.draw_two_line_row("Add a rule", &rule, &[("Add rule", None, Action::AddRule, Emphasis::Secondary)], inner, y, panel, regions)?;
            if let Some(app) = &correction.app {
                let only = if app == YOU { "Only your lines".to_owned() } else { format!("Only in {app}") };
                let choices = [("Everywhere", !correction.only_app, Action::CorrectOnlyApp(false)), (only.as_str(), correction.only_app, Action::CorrectOnlyApp(true))];
                y = renderer.draw_choice_row("The rule applies", &choices, inner, y, panel, regions)?;
            }
            renderer.draw_two_line_row(&format!("Also listen for {fix}"), &hotword, &[("Add hotword", None, Action::AddHotword, Emphasis::Secondary)], inner, y, panel, regions)
        })?;
        let hint = "A rule swaps the words heard for what they should be after the model writes them, for that exact mistake, with every \
                    model. A hotword makes the model listen out for the word, so it can get it right in any sentence; only Parakeet \
                    Unified listens for hotwords. Add either one, or both.";
        self.draw_hint(hint, content, y)
    }

    /// The file's entries under a heading per section, everywhere's first, then the lines it skipped. Returns the y below.
    fn draw_word_entries(&mut self, panel: &Panel, content: D2D_RECT_F, mut y: f32, regions: &mut Regions) -> Result<f32> {
        let words = panel.words;
        let entries = words.entries();
        if entries.is_empty() {
            y = self.draw_heading("Entries", content, y)?;
            y = self.draw_hint("None yet. Open the file to add some, or copy the instructions for an assistant.", content, y - 8.0)?;
        }
        let sections = std::iter::once(None).chain(words.sections().into_iter().map(Some));
        for (index, app) in sections.enumerate() {
            let shown: Vec<usize> = (0..entries.len()).filter(|&at| entries[at].is_in(app)).collect();
            if shown.is_empty() {
                continue;
            }
            let heading = app.map_or_else(|| "Everywhere".to_owned(), |app| format!("Only in {app}"));
            y = self.draw_heading(&heading, content, if index == 0 { y } else { y + SECTION_GAP })?;
            y = self.draw_card(content, y, |renderer, inner, mut y| {
                for &at in &shown {
                    y = renderer.draw_word_entry(at, &entries[at].kind, inner, y, panel, regions)?;
                }
                Ok(y)
            })?;
        }
        if !words.skipped.is_empty() {
            let mut skipped: Vec<String> = words.skipped.iter().take(SKIPPED_SHOWN).map(|(line, why)| format!("line {line}: {why}")).collect();
            if words.skipped.len() > SKIPPED_SHOWN {
                skipped.push(format!("{} more in Kiku's log", words.skipped.len() - SKIPPED_SHOWN));
            }
            y = self.draw_hint(&format!("Skipped {}.", skipped.join("; ")), content, y)?;
        }
        Ok(y)
    }

    /// An entry as it reads, what kind it is (with a word to listen for, how strongly), and a button removing
    /// it. Returns the y below.
    #[allow(clippy::too_many_arguments)]
    fn draw_word_entry(&mut self, index: usize, kind: &Kind, content: D2D_RECT_F, y: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        self.begin_row(content, y)?;
        let middle = y + SETTING_ROW / 2.0;
        let remove = rect(content.right + 6.0 - BUTTON, middle - BUTTON / 2.0, content.right + 6.0, middle + BUTTON / 2.0);
        self.icon_button(remove, Icon::Trash, Action::RemoveWordEntry(index), Tone::Dim, panel, regions)?;
        let mut right = remove.left - 6.0;
        if let Kind::Word { strength, .. } = kind {
            right = self.draw_word_strength(index, *strength, right, middle, panel, regions)? - 12.0;
        }
        let (text, what) = match kind {
            Kind::Rule { from, to } if to.is_empty() => (format!("{from}  →  deleted"), "rule"),
            Kind::Rule { from, to } => (format!("{from}  →  {to}"), "rule"),
            Kind::Word { word, .. } => (word.clone(), "listen for"),
            Kind::Transform(transform) => (transform.name().to_owned(), "transform"),
            Kind::Action(action) => {
                let what = match action.effect {
                    Effect::Press(_) => "command",
                    Effect::Paste(_) => "snippet",
                };
                (format!("{}  →  {}", action.phrase, action.describe()), what)
            }
        };
        let what = self.single_line(&self.fonts.small, what, f32::MAX)?;
        let what_left = right - metrics(&what)?.width;
        self.draw_layout(&what, what_left, middle - height_of(&what)? / 2.0, self.palette.text_dim)?;
        let text = self.single_line(&self.fonts.body, &text, what_left - content.left - 12.0)?;
        self.draw_layout(&text, content.left, middle - height_of(&text)? / 2.0, self.palette.text)?;
        Ok(y + SETTING_ROW)
    }

    /// A word's strength: a slider, its value (faint while it is the rest's, in the warning colour above
    /// what settings offer) and, while it has its own, a button back to the rest's, ending at `right`.
    /// Returns the slider's left edge.
    #[allow(clippy::too_many_arguments)]
    fn draw_word_strength(&mut self, index: usize, own: Option<f32>, right: f32, middle: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        const SLIDER: f32 = 120.0;
        const VALUE: f32 = 30.0;
        let dragged = panel.view.word_strength.filter(|&(at, _)| at == index).map(|(_, strength)| strength);
        let own = dragged.or(own);
        let strength = own.unwrap_or(panel.word_settings.strength);
        let reset = rect(right - BUTTON, middle - BUTTON / 2.0, right, middle + BUTTON / 2.0);
        if own.is_some() {
            self.icon_button(reset, Icon::Reset, Action::ResetWordStrength(index), Tone::Dim, panel, regions)?;
        }
        let colour = match own {
            Some(strength) if strength > *STRENGTHS.end() => self.palette.warn,
            Some(_) => self.palette.text,
            None => self.palette.text_faint,
        };
        let value = self.single_line(&self.fonts.small, &strength_text(strength), f32::MAX)?;
        let value_right = reset.left - 2.0;
        self.draw_layout(&value, value_right - metrics(&value)?.width, middle - height_of(&value)? / 2.0, colour)?;
        let track_right = value_right - VALUE - KNOB / 2.0;
        let track = rect(track_right - SLIDER, middle - TRACK / 2.0, track_right, middle + TRACK / 2.0);
        let fraction = strength_fraction(strength);
        let band = if own.is_some() { self.palette.accent } else { self.palette.text_faint };
        self.draw_slider(track, fraction, &[(0.0, fraction, band)])?;
        // The clickable band is taller than the track so it is easy to grab.
        regions.targets.push((rect(track.left, middle - 12.0, track.right, middle + 12.0), Target::Slider(Slider::WordStrength(index))));
        Ok(track.left - KNOB / 2.0)
    }
}

/// One of the tab's boxes: keys go to it once clicked, and it has a clear button.
fn word_box_role(which: WordBox, panel: &Panel) -> BoxRole {
    BoxRole { one_line: true, icon: None, typing: panel.view.typing_word == Some(which), click: Some(Action::TypeWord(which)), clear: Some(Action::ClearWord(which)) }
}

/// `text` in quotes, or `otherwise` while it is empty.
fn quoted(text: &str, otherwise: &str) -> String {
    match text.trim() {
        "" => otherwise.to_owned(),
        text => format!("“{text}”"),
    }
}
