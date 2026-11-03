//! The Words tab's drawing: where word rules and transforms apply, the Try box and what it would become,
//! the file with its buttons, and the file's entries by section, each removable.

use windows::{core::Result, Win32::Graphics::Direct2D::Common::D2D_RECT_F};

use kiku_core::words::{Kind, Scope};

use super::{
    design::{CONTROL, SETTING_ROW},
    height_of,
    history::BoxRole,
    metrics, rect, Emphasis, Panel, Renderer, Tone, BUTTON, SECTION_GAP,
};
use crate::overlay::{
    render::Icon,
    view::{Action, Regions},
};

/// Lines the file couldn't read that the tab names; the log has every one.
const SKIPPED_SHOWN: usize = 3;

impl Renderer {
    /// Returns the y below the last row.
    pub(super) fn draw_word_settings(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let settings = panel.word_settings;
        // The card the hotword, suggestion and correction settings join.
        let mut y = self.draw_heading("Where they apply", content, y)?;
        y = self.draw_card(content, y, |renderer, inner, mut y| {
            for (label, transforms, chosen) in [("Word rules apply to", false, settings.rules), ("Transforms apply to", true, settings.transforms)] {
                let choices = Scope::ALL.map(|scope| (scope.name(), scope == chosen, Action::WordScope { transforms, scope }));
                y = renderer.draw_choice_row(label, &choices, inner, y, panel, regions)?;
            }
            renderer.draw_toggle_row("Listen for @ words", settings.hotwords, Action::WordHotwords(!settings.hotwords), inner, y, panel, regions)
        })?;
        let hint = "Captions are what the panel, captions mode, History and the MCP server get; dictation is what Kiku types or pastes. \
                    @ words are listened for in every app, by Parakeet Unified only.";
        y = self.draw_hint(hint, content, y)?;

        y = self.draw_heading("Try", content, y + SECTION_GAP)?;
        let view = panel.view;
        let role = BoxRole { one_line: true, typing: view.typing_try, click: Some(Action::TypeTry), clear: Some(Action::ClearTry) };
        let area = rect(content.left, y, content.right, y + CONTROL);
        y = self.draw_text_box(&view.try_words, "Type what was said, to see what Kiku makes of it", area, role, panel, regions)?.bottom;
        if !view.try_words.text.trim().is_empty() {
            let preview = panel.words.preview(&view.try_words.text, &settings);
            y = self.draw_card(content, y + 8.0, |renderer, inner, mut y| {
                for (label, text) in &preview {
                    let text = if text.is_empty() { "(nothing)" } else { text };
                    y = renderer.draw_two_line_row(text, label, &[], inner, y, panel, regions)?;
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
        let hint = "One entry a line: kicku => Kiku swaps words (nothing after => deletes them), @ Tannhauser is a word to listen for, \
                    ! lowercase, ! no-trailing-punctuation and ! capitalise change the whole line, and [Discord] starts what applies only to Discord ([You] is your mic).";
        y = self.draw_hint(hint, content, y)?;
        self.draw_word_entries(panel, content, y + SECTION_GAP, regions)
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

    /// An entry as it reads, what kind it is, and a button removing it. Returns the y below.
    #[allow(clippy::too_many_arguments)]
    fn draw_word_entry(&mut self, index: usize, kind: &Kind, content: D2D_RECT_F, y: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        self.begin_row(content, y)?;
        let middle = y + SETTING_ROW / 2.0;
        let remove = rect(content.right + 6.0 - BUTTON, middle - BUTTON / 2.0, content.right + 6.0, middle + BUTTON / 2.0);
        self.icon_button(remove, Icon::Trash, Action::RemoveWordEntry(index), Tone::Dim, panel, regions)?;
        let (text, what) = match kind {
            Kind::Rule { from, to } if to.is_empty() => (format!("{from}  →  deleted"), "rule"),
            Kind::Rule { from, to } => (format!("{from}  →  {to}"), "rule"),
            Kind::Word(word) => (word.clone(), "listen for"),
            Kind::Transform(transform) => (transform.name().to_owned(), "transform"),
        };
        let what = self.single_line(&self.fonts.small, what, f32::MAX)?;
        let what_left = remove.left - 6.0 - metrics(&what)?.width;
        self.draw_layout(&what, what_left, middle - height_of(&what)? / 2.0, self.palette.text_dim)?;
        let text = self.single_line(&self.fonts.body, &text, what_left - content.left - 12.0)?;
        self.draw_layout(&text, content.left, middle - height_of(&text)? / 2.0, self.palette.text)?;
        Ok(y + SETTING_ROW)
    }
}
