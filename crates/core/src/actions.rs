//! Voice actions: a phrase in dictation that presses keys (said as a whole line) or pastes saved text (said
//! anywhere), written as `> phrase => press ctrl+a` or `> phrase => paste text` in `words.txt`.

use std::fmt;

use crate::{
    keybinds::Key,
    words::{in_word, without_comment},
};

pub(crate) mod matching;

const VK_WIN: u16 = 0x5B;
const VK_L: u16 = 0x4C;
const NUMBER_WORDS: [&str; 21] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen",
    "sixteen", "seventeen", "eighteen", "nineteen", "twenty",
];

/// Text that must stay private (a snippet): its `Debug` shows only its length, and it has no `Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Private(String);

impl Private {
    pub fn new(text: String) -> Self {
        Self(text)
    }

    /// The text itself, for the paste and nothing else.
    pub fn reveal(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Private {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Private({} chars)", self.0.chars().count())
    }
}

/// What an action does.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Presses these keys in order, then lets them go in reverse; only when the phrase is the whole line.
    Press(Vec<Key>),
    /// Pastes this text wherever the phrase is said.
    Paste(Private),
}

/// A command's keys as the Try boxes show them: `[press Ctrl+A]`.
pub(crate) fn press_shown(keys: &[Key]) -> String {
    format!("[press {}]", keys.iter().map(|key| key.name()).collect::<Vec<_>>().join("+"))
}

/// A phrase and what saying it does.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    /// As written in the file, its words separated by single spaces.
    pub phrase: String,
    pub effect: Effect,
}

impl Action {
    /// A phrase that presses `keys`, written as in the file (`ctrl+shift+t`).
    pub fn press(phrase: &str, keys: &str) -> Result<Action, String> {
        Ok(Action { phrase: clean_phrase(phrase)?, effect: Effect::Press(parse_keys(keys)?) })
    }

    /// A phrase that pastes `text` as given (trimmed, line breaks as `\n`).
    pub fn paste(phrase: &str, text: &str) -> Result<Action, String> {
        Ok(Action { phrase: clean_phrase(phrase)?, effect: Effect::Paste(paste_text(text)?) })
    }

    /// Its line in the file, e.g. `> select all => press Ctrl+A`.
    pub fn to_line(&self) -> String {
        let doing = match &self.effect {
            Effect::Press(keys) => format!("press {}", keys.iter().map(|key| key.name()).collect::<Vec<_>>().join("+")),
            Effect::Paste(text) => format!("paste {}", escape(text.reveal())),
        };
        format!("> {} => {doing}", self.phrase)
    }

    /// How the Try boxes show it: a snippet as `[phrase]`, a command as `[press Ctrl+A]`.
    pub fn shown(&self) -> String {
        match &self.effect {
            Effect::Press(keys) => press_shown(keys),
            Effect::Paste(_) => format!("[{}]", self.phrase),
        }
    }

    /// What it does, never the text it pastes: "presses Ctrl+A", "pastes 2 lines, 25 characters".
    pub fn describe(&self) -> String {
        match &self.effect {
            Effect::Press(keys) => format!("presses {}", keys.iter().map(|key| key.name()).collect::<Vec<_>>().join("+")),
            Effect::Paste(text) => {
                let (text, lines) = (text.reveal(), text.reveal().lines().count());
                match lines {
                    0 | 1 => format!("pastes {} characters", text.chars().count()),
                    _ => format!("pastes {lines} lines, {} characters", text.chars().count()),
                }
            }
        }
    }
}

/// One thing dictation does in the app in front, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Paste(Private),
    Press(Vec<Key>),
}

impl Step {
    /// How many characters undo takes back after this step; `None` when it can't be undone (a key press).
    pub fn erase(&self) -> Option<usize> {
        match self {
            Step::Paste(text) => Some(text.reveal().chars().count()),
            Step::Press(_) => None,
        }
    }
}

/// One key going down or up, by Windows virtual-key code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyStroke {
    pub vk: u16,
    pub up: bool,
    /// Sent as an extended key (arrows, Home, End, Page Up/Down, Insert, Delete).
    pub extended: bool,
}

/// The strokes pressing `keys` in order and letting them go in reverse; modifiers as their left keys.
pub fn key_strokes(keys: &[Key]) -> Vec<KeyStroke> {
    let stroke = |key: &Key, up: bool| match *key {
        Key::Ctrl => KeyStroke { vk: 0xA2, up, extended: false },
        Key::Shift => KeyStroke { vk: 0xA0, up, extended: false },
        Key::Alt => KeyStroke { vk: 0xA4, up, extended: false },
        Key::Win => KeyStroke { vk: VK_WIN, up, extended: false },
        Key::Other(vk) => KeyStroke { vk, up, extended: matches!(vk, 0x21..=0x28 | 0x2D | 0x2E) },
    };
    keys.iter().map(|key| stroke(key, false)).chain(keys.iter().rev().map(|key| stroke(key, true))).collect()
}

/// `text` as the clipboard holds it: Windows line breaks.
pub fn clipboard_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// A word of what was said: its token and where it starts and ends, in characters.
pub(crate) struct Word {
    pub token: String,
    pub start: usize,
    pub end: usize,
}

/// The words of `text` as the rules see them, each lowercased, with the number words zero to twenty as digits.
pub(crate) fn words(text: &[char]) -> Vec<Word> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < text.len() {
        if !in_word(text, at) {
            at += 1;
            continue;
        }
        let start = at;
        while in_word(text, at) {
            at += 1;
        }
        let word: String = text[start..at].iter().collect::<String>().to_lowercase();
        let token = NUMBER_WORDS.iter().position(|number| *number == word).map_or(word, |number| number.to_string());
        found.push(Word { token, start, end: at });
    }
    found
}

/// What was said as comparable words: see [`words`].
pub(crate) fn tokens(text: &str) -> Vec<String> {
    words(&text.chars().collect::<Vec<_>>()).into_iter().map(|word| word.token).collect()
}

/// The action a line holds after its `>`: `phrase => press keys` or `phrase => paste text`. Reasons never
/// quote what follows the `=>`, nor the line.
pub(crate) fn parse(text: &str) -> Result<Action, String> {
    let (phrase, doing) = text.split_once("=>").ok_or("not an action (> phrase => press keys, or paste text)")?;
    let phrase = clean_phrase(phrase)?;
    let (verb, rest) = doing.trim().split_once(char::is_whitespace).unwrap_or((doing.trim(), ""));
    let effect = match verb.to_lowercase().as_str() {
        "press" => Effect::Press(parse_keys(without_comment(rest))?),
        "paste" => Effect::Paste(parse_paste(rest)?),
        _ => return Err("after => an action says press or paste".into()),
    };
    Ok(Action { phrase, effect })
}

/// A phrase with its words separated by single spaces.
fn clean_phrase(phrase: &str) -> Result<String, String> {
    let phrase = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
    if phrase.is_empty() {
        return Err("an action needs words before =>".into());
    }
    if tokens(&phrase).is_empty() {
        return Err("an action's phrase needs at least one word".into());
    }
    Ok(phrase)
}

/// Keys joined by `+`, in any case, with spaces allowed around the `+`.
pub(crate) fn parse_keys(text: &str) -> Result<Vec<Key>, String> {
    if text.trim().is_empty() {
        return Err("press needs keys, like ctrl+a".into());
    }
    let keys: Vec<Key> = text.split('+').map(Key::parse).collect::<Option<_>>().ok_or("press has a key Kiku doesn't know")?;
    let (win, l) = (keys.contains(&Key::Win), keys.contains(&Key::Other(VK_L)));
    if win && l {
        return Err("Win+L locks the PC and can't be pressed".into());
    }
    Ok(keys)
}

/// Paste text, trimmed, with `\n`, `\t` and `\\` as a line break, a tab and a backslash.
fn parse_paste(text: &str) -> Result<Private, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("paste needs the text to paste".into());
    }
    Ok(Private::new(unescape(text)))
}

/// Text to paste as given: trimmed, line breaks as `\n`.
fn paste_text(text: &str) -> Result<Private, String> {
    let text = text.trim().replace("\r\n", "\n").replace('\r', "\n");
    if text.is_empty() {
        return Err("paste needs the text to paste".into());
    }
    Ok(Private::new(text))
}

/// The inverse of [`escape`].
pub fn unescape(text: &str) -> String {
    let mut unescaped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let escaped = if c == '\\' { chars.peek().and_then(|next| match next { 'n' => Some('\n'), 't' => Some('\t'), '\\' => Some('\\'), _ => None }) } else { None };
        match escaped {
            Some(replacement) => {
                chars.next();
                unescaped.push(replacement);
            }
            None => unescaped.push(c),
        }
    }
    unescaped
}

/// Text on one line: a backslash, line break and tab as `\\`, `\n` and `\t`.
pub fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\n', "\\n").replace('\t', "\\t")
}

/// The strokes of `chords` pressed one after another.
pub fn sequence_strokes(chords: &[Vec<Key>]) -> Vec<KeyStroke> {
    chords.iter().flat_map(|chord| key_strokes(chord)).collect()
}
