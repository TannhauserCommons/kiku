//! A text box's text and caret, edited by keys: the history screen's search and SQL boxes, and the font list's search.

use windows::Win32::UI::Input::KeyboardAndMouse::{VIRTUAL_KEY, VK_BACK, VK_DELETE, VK_END, VK_HOME, VK_LEFT, VK_RIGHT, VK_V};

/// Text being typed. The caret is a byte offset, always on a character boundary.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Field {
    pub text: String,
    caret: usize,
    /// The first half of a character typed as two UTF-16 units, waiting for the second.
    high_surrogate: Option<u16>,
}

impl Field {
    /// Replaces the text, with the caret at its end.
    pub fn set(&mut self, text: &str) {
        self.text = text.to_owned();
        self.caret = self.text.len();
    }

    pub fn clear(&mut self) {
        self.set("");
    }

    /// The caret as a UTF-16 offset, as DirectWrite counts.
    pub fn caret_utf16(&self) -> u32 {
        self.text[..self.caret].encode_utf16().count() as u32
    }

    /// Puts `text` at the caret, as one line: line breaks and tabs become spaces, other control characters are dropped.
    pub fn insert(&mut self, text: &str) {
        let text: String = text.chars().map(|c| if c.is_whitespace() { ' ' } else { c }).filter(|c| !c.is_control()).collect();
        self.text.insert_str(self.caret, &text);
        self.caret += text.len();
    }

    /// One UTF-16 unit from a WM_CHAR message. Control characters are keys such as Enter, handled as keys.
    pub fn type_unit(&mut self, unit: u16) {
        if unit < 0x20 || unit == 0x7F {
            return;
        }
        let units = match self.high_surrogate.take() {
            Some(high) => vec![high, unit],
            None if (0xD800..0xDC00).contains(&unit) => return self.high_surrogate = Some(unit),
            None => vec![unit],
        };
        self.insert(&String::from_utf16_lossy(&units));
    }

    /// Applies an editing key (Backspace, Delete, the arrows, Home, End; with Ctrl, by words, and Ctrl+V
    /// pastes what `paste` gives). Returns whether `key` was one; the box's owner handles the rest.
    pub fn edit(&mut self, key: VIRTUAL_KEY, ctrl: bool, paste: impl FnOnce() -> Option<String>) -> bool {
        match key {
            VK_BACK => self.backspace(ctrl),
            VK_DELETE => self.delete(ctrl),
            VK_LEFT => self.left(ctrl),
            VK_RIGHT => self.right(ctrl),
            VK_HOME => self.home(),
            VK_END => self.end(),
            VK_V if ctrl => self.insert(&paste().unwrap_or_default()),
            _ => return false,
        }
        true
    }

    /// Deletes the character before the caret, or the word with `word`.
    pub fn backspace(&mut self, word: bool) {
        let start = if word { self.word_start() } else { self.previous() };
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
    }

    /// Deletes the character after the caret, or the word with `word`.
    pub fn delete(&mut self, word: bool) {
        let end = if word { self.word_end() } else { self.next() };
        self.text.replace_range(self.caret..end, "");
    }

    pub fn left(&mut self, word: bool) {
        self.caret = if word { self.word_start() } else { self.previous() };
    }

    pub fn right(&mut self, word: bool) {
        self.caret = if word { self.word_end() } else { self.next() };
    }

    pub fn home(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.caret = self.text.len();
    }

    fn previous(&self) -> usize {
        self.text[..self.caret].char_indices().next_back().map_or(0, |(at, _)| at)
    }

    fn next(&self) -> usize {
        self.text[self.caret..].chars().next().map_or(self.caret, |c| self.caret + c.len_utf8())
    }

    /// Where the word before the caret starts, past any spaces before the caret.
    fn word_start(&self) -> usize {
        let before = self.text[..self.caret].trim_end();
        before.rfind(char::is_whitespace).map_or(0, |at| at + 1)
    }

    /// Where the word after the caret ends, past any spaces after the caret.
    fn word_end(&self) -> usize {
        let after = &self.text[self.caret..];
        let word = after.len() - after.trim_start().len();
        after[word..].find(char::is_whitespace).map_or(self.text.len(), |at| self.caret + word + at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> Field {
        let mut field = Field::default();
        field.insert(text);
        field
    }

    #[test]
    fn typing_goes_in_at_the_caret_on_one_line() {
        let mut field = typed("thermal paste");
        field.left(true);
        field.insert("old\r\n");
        assert_eq!(field.text, "thermal old  paste");
        field.home();
        field.right(false);
        field.insert("\u{7}");
        assert_eq!(field.text, "thermal old  paste", "control characters are dropped");
        assert_eq!(field.caret_utf16(), 1);
    }

    #[test]
    fn deleting_takes_characters_or_words() {
        let mut field = typed("café au lait  ");
        field.backspace(true);
        assert_eq!(field.text, "café au ");
        field.backspace(false);
        field.backspace(false);
        assert_eq!(field.text, "café a");
        field.home();
        field.delete(true);
        assert_eq!(field.text, " a");
        field.delete(true);
        assert_eq!(field.text, "");
        field.backspace(false);
        field.delete(false);
        assert_eq!(field.caret_utf16(), 0, "nothing to delete is fine");
    }

    #[test]
    fn characters_beyond_utf16s_first_plane_arrive_in_two_halves() {
        let mut field = typed("ok ");
        "👍\r".encode_utf16().for_each(|unit| field.type_unit(unit));
        assert_eq!((field.text.as_str(), field.caret_utf16()), ("ok 👍", 5), "and Enter types nothing");
        field.backspace(false);
        assert_eq!(field.text, "ok ");
    }
}
