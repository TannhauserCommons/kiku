//! Word rules: fixed swaps, words to listen for and simple transforms applied to captions and dictation,
//! kept in `%APPDATA%\kiku\words.txt`, a plain file people and assistants edit. Kiku reloads it when it
//! changes, and edits it keeping its comments and order.
//!
//! ```text
//! kicku => Kiku          # whole words or a phrase, any case, become the text after => as written
//! @ Tannhauser           # a word to listen for
//! @ Lull :2.5            # listened for more strongly than the rest (their strength is in settings)
//! ! lowercase            # a transform, applied in the order listed
//! [Discord]              # what follows applies only to Discord ("You" is the mic)
//! gg => good game
//! ```

use std::{
    collections::HashMap,
    fmt, fs, io,
    ops::RangeInclusive,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use crate::{
    actions::{
        self,
        matching::{Chunk, Index, Part, Said, Trigger},
        Action, Effect, Step,
    },
    engine::Hotword,
    file_watch::FileWatch,
    keybinds::Key,
    log, paths, transcripts,
};

/// What a line of dictation turns into.
enum Piece {
    /// Text ready to paste.
    Text(String),
    /// A whole-line snippet, pasted exactly: a space comes only before text that follows it.
    Snippet(String),
    Press(Vec<Key>),
}

/// What a new file starts with.
pub const HEADER: &str = "\
# Kiku's word rules. Kiku reloads this file whenever it is saved.
#   kicku => Kiku       these words, any case, become the text after => (nothing after it deletes them)
#   @ Tannhauser        a word to listen for, in every app (Parakeet Unified listens)
#   @ Lull :2.5         listened for with a strength of its own (Settings → Words sets the rest's; 1.5 at first)
#   ! lowercase         a transform: lowercase, no-trailing-punctuation or capitalise
#   > select all => press ctrl+a     a command: said as a whole line in dictation, it presses the keys
#   > my email => paste me@example.com   said anywhere in a sentence in dictation, it pastes the text
#   [Discord]           what follows applies only to Discord; [You] is your mic
# Settings → Words picks whether rules and transforms apply to captions, dictation or both.
";

/// What "Copy instructions for an assistant" copies: how to edit the file, ending where the user adds their request.
pub const ASSISTANT_INSTRUCTIONS: &str = "\
I use Kiku for live captions and dictation on Windows. Change what it does to my words before it shows or types them.

The steps live in one plain text file, %APPDATA%\\kiku\\words.txt, which Kiku reloads as soon as it is saved. One entry per line:

    # a comment; a # with spaces around it also starts a comment at the end of a rule, @ or ! line (not a > line)
    kicku => Kiku        a rule: these words or this phrase, in any case and only as whole words (punctuation next to them is fine), become the text after => exactly as written
    um =>                nothing after => deletes the words
    @ Tannhauser         a name or word Kiku should listen for, in every app whatever section it is in (only Parakeet Unified listens; other models ignore it)
    @ Lull :2.5          listened for with a strength of its own, from 0.5 to 4 (Settings → Words sets the strength of the rest, 1.5 at first)
    ! lowercase          a transform; the built-in ones are lowercase, no-trailing-punctuation and capitalise (the first letter)
    > select all => press ctrl+a      a command: when what I dictate is only this phrase (case and punctuation aside), press the keys instead of typing; names like Enter, Tab, Esc, Up, Home, Page Down, F1 to F24, joined by +
    > my email => paste me@example.com
    [Discord]            the lines after it apply only to this app, named as Kiku shows it; [You] is my microphone

Lines before the first [App] line apply everywhere. Rules run before transforms, the general ones before an app's, and transforms in the order listed. Where rules overlap, the longest wins. Rules are fixed text: no patterns or wildcards.

Use rules for fixed swaps and deletions, such as a misheard name to its spelling, a word to an emoji, or dropping \"um\". Names and words Kiku keeps mishearing go on @ lines, which help the model hear them; a rule only changes the text afterwards, so rewriting a real word with one changes it everywhere it is said. A word still misheard can be given a higher strength, but the stronger it is, the likelier it is heard where a similar word was said; toward 4 that starts to happen. Settings → Words in Kiku chooses whether rules and transforms apply to captions (the panel, captions mode, History and Kiku's MCP server), to dictation (what Kiku types or pastes, where an app's lines apply to the app being typed into) or both; by default rules apply to both and transforms to dictation.

Actions (> lines) work only when I dictate, never in captions, History or Kiku's MCP server, and an app's section applies when I dictate into that app and wins over the general ones. A > line takes no comment at its end: its text after \"paste \" is pasted exactly as written, so a # there is pasted too. A snippet, like \"my email\" above, replaces its phrase anywhere in a dictated sentence with the text after \"paste \", where \\n is a line break, \\t a tab and \\\\ a backslash; said alone it is pasted exactly. Snippets match whole words, longest phrase first; number words from zero to twenty also match digits. Win+L is refused. Choose distinctive phrases: a command fires whenever the whole line is its phrase, so \"select all\" said alone presses the keys, and a snippet's text never appears in captions, History, the log or the MCP server. My undo shortcut takes back a snippet's paste, not a key press.

For several steps in one phrase (pastes, key presses, files, opening things, scripts), voice macros live in %APPDATA%\\kiku\\custom\\macros.toml. Read the README.md beside it first: it has every step, variable, trigger and mode, and the safety rules. Keep my values in variables.toml, never in a macro, and do not read variables.toml back to me unless I ask. Never route its values through {selection} or copy = true. You can't approve a script or turn on running programs: only I can, in Kiku's Macros tab.

Edit the file in place, keeping my comments and the order of what is there. What I want:
";

/// Where text goes: shown as captions (the panel, captions mode, History, the MCP server) or typed by dictation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    Captions,
    Dictation,
}

/// Which outputs a kind of entry applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Captions,
    Dictation,
    Both,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Captions, Scope::Dictation, Scope::Both];

    pub fn name(self) -> &'static str {
        match self {
            Scope::Captions => "Captions",
            Scope::Dictation => "Dictation",
            Scope::Both => "Both",
        }
    }

    pub fn covers(self, output: Output) -> bool {
        match self {
            Scope::Both => true,
            Scope::Captions => output == Output::Captions,
            Scope::Dictation => output == Output::Dictation,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Scope::Captions => "captions",
            Scope::Dictation => "dictation",
            Scope::Both => "both",
        }
    }
}

/// How strongly the model listens for an `@` word without a strength of its own, until settings change it.
pub const DEFAULT_STRENGTH: f32 = 1.5;
/// The strengths settings offer. Toward the top, words start to be heard where they weren't said.
pub const STRENGTHS: RangeInclusive<f32> = 0.5..=4.0;
/// The strongest a word may be in the file, for trying past what settings offer.
const MAX_STRENGTH: f32 = 10.0;

/// A strength as the file and settings write it, to a tenth.
pub fn strength_text(strength: f32) -> String {
    format!("{}", (strength * 10.0).round() / 10.0)
}

/// A strength the file or settings give, if it is one: a number above 0, up to [`MAX_STRENGTH`].
fn read_strength(text: &str) -> Option<f32> {
    text.parse::<f32>().ok().filter(|strength| *strength > 0.0 && *strength <= MAX_STRENGTH)
}

/// Where word rules and transforms apply, kept as `words.*` lines in settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordSettings {
    pub rules: Scope,
    pub transforms: Scope,
    /// The model listens for the `@` words, on models that can.
    pub hotwords: bool,
    /// How strongly it listens for the words without a strength of their own.
    pub strength: f32,
}

impl Default for WordSettings {
    fn default() -> Self {
        Self { rules: Scope::Both, transforms: Scope::Dictation, hotwords: true, strength: DEFAULT_STRENGTH }
    }
}

impl WordSettings {
    /// Applies one `words.*` settings line; anything unknown keeps its default.
    pub fn apply_setting(&mut self, key: &str, value: &str) {
        let scope = Scope::ALL.into_iter().find(|scope| scope.id() == value);
        match key.strip_prefix("words.").unwrap_or(key) {
            "rules" => self.rules = scope.unwrap_or(self.rules),
            "transforms" => self.transforms = scope.unwrap_or(self.transforms),
            "hotwords" => self.hotwords = value != "off",
            // 0.2.6 named its three strengths.
            "strength" => self.strength = match value {
                "gentle" => 1.0,
                "normal" => DEFAULT_STRENGTH,
                "strong" => 2.5,
                number => read_strength(number).unwrap_or(self.strength),
            },
            _ => {}
        }
    }

    pub fn to_settings(&self) -> String {
        let hotwords = if self.hotwords { "on" } else { "off" };
        let (rules, transforms, strength) = (self.rules.id(), self.transforms.id(), strength_text(self.strength));
        format!("words.rules = {rules}\nwords.transforms = {transforms}\nwords.hotwords = {hotwords}\nwords.strength = {strength}\n")
    }
}

/// A built-in change to a whole line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transform {
    Lowercase,
    NoTrailingPunctuation,
    /// Upper-cases the first letter.
    Capitalise,
}

impl Transform {
    pub const ALL: [Transform; 3] = [Transform::Lowercase, Transform::NoTrailingPunctuation, Transform::Capitalise];

    /// Its name in the file.
    pub fn name(self) -> &'static str {
        match self {
            Transform::Lowercase => "lowercase",
            Transform::NoTrailingPunctuation => "no-trailing-punctuation",
            Transform::Capitalise => "capitalise",
        }
    }

    pub(crate) fn named(name: &str) -> Option<Transform> {
        let name = if name.eq_ignore_ascii_case("capitalize") { "capitalise" } else { name };
        Self::ALL.into_iter().find(|transform| transform.name().eq_ignore_ascii_case(name))
    }

    pub(crate) fn apply(self, text: &str) -> String {
        match self {
            Transform::Lowercase => text.to_lowercase(),
            Transform::NoTrailingPunctuation => text.trim_end().trim_end_matches(|c| ".,!?;:…。、！？".contains(c)).to_owned(),
            Transform::Capitalise => match text.char_indices().find(|(_, c)| c.is_alphabetic()) {
                Some((at, first)) => format!("{}{}{}", &text[..at], first.to_uppercase(), &text[at + first.len_utf8()..]),
                None => text.to_owned(),
            },
        }
    }
}

/// What an entry does.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// These words become `to` as written; an empty `to` deletes them.
    Rule { from: String, to: String },
    /// A word to listen for, and how strongly if not as strongly as settings say.
    Word { word: String, strength: Option<f32> },
    Transform(Transform),
    /// A voice action, used only by dictation.
    Action(Action),
}

/// An entry in the file, with the app section it is in (`None` before the first section).
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub section: Option<String>,
    pub kind: Kind,
    /// Its line in the file, from 0.
    line: usize,
}

impl Entry {
    /// Whether it is in `app`'s section, or with `None` before any section.
    pub fn is_in(&self, app: Option<&str>) -> bool {
        same_section(self.section.as_deref(), app)
    }
}

/// The parsed file, with its lines as written so edits keep comments and order.
#[derive(Clone, Default, PartialEq)]
pub struct Words {
    lines: Vec<String>,
    entries: Vec<Entry>,
    /// Lines that couldn't be read: their number, from 1, and why.
    pub skipped: Vec<(usize, String)>,
    /// The voice actions, looked up when dictating.
    index: Index<Action>,
}

/// Counts only: the lines can hold snippet text.
impl fmt::Debug for Words {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Words").field("lines", &self.lines.len()).field("entries", &self.entries.len()).field("skipped", &self.skipped.len()).finish()
    }
}

impl Words {
    /// What dictating `said` (finished lines, in order) does in the app labelled `app`: the text to paste, with
    /// snippets spliced in, and the keys commands press, in order.
    pub fn plan(&self, said: &[String], app: &str, settings: &WordSettings) -> Vec<Step> {
        let mut steps = Vec::new();
        let mut text = String::new();
        let mut after_snippet = false;
        for piece in self.pieces(said, app, settings, |action| match &action.effect { Effect::Paste(text) => text.reveal().to_owned(), Effect::Press(_) => String::new() }) {
            if after_snippet && !matches!(piece, Piece::Press(_)) {
                text.push(' ');
            }
            after_snippet = matches!(piece, Piece::Snippet(_));
            match piece {
                Piece::Text(more) | Piece::Snippet(more) => text.push_str(&more),
                Piece::Press(keys) => {
                    if !text.is_empty() {
                        steps.push(Step::Paste(actions::Private::new(std::mem::take(&mut text))));
                    }
                    steps.push(Step::Press(keys));
                }
            }
        }
        if !text.is_empty() {
            steps.push(Step::Paste(actions::Private::new(text)));
        }
        steps
    }

    /// Each line said as dictation types it, `shown` giving what a snippet is replaced with.
    fn pieces(&self, said: &[String], app: &str, settings: &WordSettings, shown: impl Fn(&Action) -> String) -> Vec<Piece> {
        let apply = |line: &str| self.apply(line, app, Output::Dictation, settings);
        if self.index.is_empty() {
            let typed = |line: String| (!line.trim().is_empty()).then(|| Piece::Text(format!("{line} ")));
            return said.iter().filter_map(|line| typed(apply(line))).collect();
        }
        let sections = self.index.sections(app);
        let piece = |line: &String| match sections.said(line, apply)? {
            Said::Line(action, _) => Some(match &action.effect {
                Effect::Press(keys) => Piece::Press(keys.clone()),
                Effect::Paste(_) => Piece::Snippet(shown(action)),
            }),
            Said::Text(chunks) => {
                let mut text: String = chunks.into_iter().map(|chunk| match chunk {
                    Chunk::Text(text) => text,
                    Chunk::Trigger(action) => shown(action),
                }).collect();
                text.push(' ');
                Some(Piece::Text(text))
            }
        };
        said.iter().filter_map(piece).collect()
    }

    /// Adds `action` at the end of everywhere's entries or of `app`'s section (made if missing); a phrase
    /// already there is refused.
    pub fn add_action(&mut self, action: &Action, app: Option<&str>) -> Result<(), String> {
        let phrase = actions::tokens(&action.phrase);
        let known = self.entries.iter().any(|entry| match &entry.kind {
            Kind::Action(known) => entry.is_in(app) && actions::tokens(&known.phrase) == phrase,
            _ => false,
        });
        if known {
            return Err("There is an action for those words already".into());
        }
        self.insert_entry(action.to_line(), Kind::Action(action.clone()), app).map_err(|()| "That action can't be written to the file".into())
    }
}

/// What one line of the file says.
enum Parsed {
    Nothing,
    Section(String),
    Entry(Kind),
}

impl Words {
    pub fn parse(text: &str) -> Words {
        let mut words = Words { lines: text.lines().map(str::to_owned).collect(), ..Default::default() };
        words.read_lines();
        words
    }

    /// The file's text, as written with any edits.
    pub fn to_text(&self) -> String {
        self.lines.iter().map(|line| format!("{line}\n")).collect()
    }

    /// Every entry, in file order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The words to listen for, from every section, each with its own strength or else `strength`.
    pub fn hotwords(&self, strength: f32) -> Vec<Hotword> {
        let hotword = |entry: &Entry| match &entry.kind {
            Kind::Word { word, strength: own } => Some(Hotword { text: word.clone(), boost: own.unwrap_or(strength) }),
            _ => None,
        };
        self.entries.iter().filter_map(hotword).collect()
    }

    /// `text` as it reaches `output`, said by (captions) or typed into (dictation) the app labelled `app`,
    /// with the steps `settings` turn on for it.
    pub fn apply(&self, text: &str, app: &str, output: Output, settings: &WordSettings) -> String {
        self.run(text, app, settings.rules.covers(output), settings.transforms.covers(output))
    }

    /// A caption on its way to dictation (the paste shortcut): only the steps dictation has and captions lack,
    /// since the caption had the rest.
    pub fn apply_to_caption(&self, text: &str, app: &str, settings: &WordSettings) -> String {
        let only_dictation = |scope: Scope| scope == Scope::Dictation;
        self.run(text, app, only_dictation(settings.rules), only_dictation(settings.transforms))
    }

    /// What `text` becomes in captions and dictation, everywhere and then in each app with a section where
    /// that differs, labelled for the Words tab's Try box.
    pub fn preview(&self, text: &str, settings: &WordSettings) -> Vec<(String, String)> {
        self.preview_with(text, settings, |app| self.dictation_preview(text, app, settings))
    }

    /// [`Words::preview`] with `dictation` giving what dictating the text in an app shows.
    pub(crate) fn preview_with(&self, text: &str, settings: &WordSettings, dictation: impl Fn(&str) -> String) -> Vec<(String, String)> {
        let mut shown = Vec::new();
        let result = |app: &str, output| match output {
            Output::Captions => self.apply(text, app, output, settings),
            Output::Dictation => dictation(app),
        };
        for (name, output) in [("Captions", Output::Captions), ("Dictation", Output::Dictation)] {
            let everywhere = result("", output);
            shown.push((name.to_owned(), everywhere.clone()));
            for app in self.sections() {
                let in_app = result(app, output);
                if in_app != everywhere {
                    shown.push((format!("{name} in {app}"), in_app));
                }
            }
        }
        shown
    }

    /// Dictating `text` in `app` as the Try box shows it: a snippet as `[phrase]`, a command as `[press keys]`.
    fn dictation_preview(&self, text: &str, app: &str, settings: &WordSettings) -> String {
        let pieces = self.pieces(&[text.to_owned()], app, settings, Action::shown);
        let shown: String = pieces.into_iter().map(|piece| match piece { Piece::Text(text) | Piece::Snippet(text) => text, Piece::Press(keys) => actions::press_shown(&keys) }).collect();
        shown.strip_suffix(' ').unwrap_or(&shown).to_owned()
    }

    /// The apps with a section, once each, in file order.
    pub fn sections(&self) -> Vec<&str> {
        let mut apps: Vec<&str> = Vec::new();
        for app in self.entries.iter().filter_map(|entry| entry.section.as_deref()) {
            if !apps.iter().any(|known| known.eq_ignore_ascii_case(app)) {
                apps.push(app);
            }
        }
        apps
    }

    /// Adds a rule turning `from` into `to`, everywhere or in `app`'s section (made at the end if missing).
    pub fn add_rule(&mut self, from: &str, to: &str, app: Option<&str>) -> Result<(), String> {
        let (from, to) = (from.split_whitespace().collect::<Vec<_>>().join(" "), to.trim());
        if from.is_empty() {
            return Err("A rule needs the words to replace".into());
        }
        let line = if to.is_empty() { format!("{from} =>") } else { format!("{from} => {to}") };
        let rule = Kind::Rule { from, to: to.to_owned() };
        self.insert_entry(line, rule, app).map_err(|()| "Those words can't be written as a rule: they hold => or # or start with @, ! or [".into())
    }

    /// Adds a word to listen for, unless it is listed already.
    pub fn add_word(&mut self, word: &str) -> Result<(), String> {
        let word = word.trim();
        if self.entries.iter().any(|entry| matches!(&entry.kind, Kind::Word { word: known, .. } if known.eq_ignore_ascii_case(word))) {
            return Ok(());
        }
        let kind = Kind::Word { word: word.to_owned(), strength: None };
        self.insert_entry(format!("@ {word}"), kind, None).map_err(|()| "Type one word or name to listen for".into())
    }

    /// Gives the word to listen for in the line holding an entry like `entry` a strength of its own, or with
    /// `None` the one settings give, keeping a comment after it; returns whether there was one.
    pub fn set_strength(&mut self, entry: &Entry, strength: Option<f32>) -> bool {
        let Kind::Word { word, .. } = &entry.kind else { return false };
        let Some(line) = self.line_of(entry) else { return false };
        let comment = self.lines[line][without_comment(&self.lines[line]).trim_end().len()..].to_owned();
        let strength = strength.map(|strength| format!(" :{}", strength_text(strength))).unwrap_or_default();
        self.lines[line] = format!("@ {word}{strength}{comment}");
        self.read_lines();
        true
    }

    /// Removes the line holding an entry like `entry`; returns whether there was one.
    pub fn remove(&mut self, entry: &Entry) -> bool {
        let Some(line) = self.line_of(entry) else { return false };
        self.lines.remove(line);
        self.read_lines();
        true
    }

    /// The line holding an entry like `entry`: the one it was read from if it is still there, else the first like it.
    fn line_of(&self, entry: &Entry) -> Option<usize> {
        let same = |e: &&Entry| e.kind == entry.kind && e.is_in(entry.section.as_deref());
        self.entries.iter().filter(same).min_by_key(|e| e.line != entry.line).map(|e| e.line)
    }

    /// Inserts `line` if it reads back as `kind`, which rules out text the file's format would read otherwise.
    fn insert_entry(&mut self, line: String, kind: Kind, app: Option<&str>) -> Result<(), ()> {
        match parse_line(&line) {
            Ok(Parsed::Entry(read)) if read == kind => {
                self.insert(line, app);
                Ok(())
            }
            _ => Err(()),
        }
    }

    /// Puts `line` after the last entry of `app`'s section, or of the lines before any section; a missing
    /// section is added at the end.
    fn insert(&mut self, line: String, app: Option<&str>) {
        let headers: Vec<(usize, Option<String>)> = std::iter::once((0, None))
            .chain(self.lines.iter().enumerate().filter_map(|(at, text)| match parse_line(text) {
                Ok(Parsed::Section(name)) => Some((at + 1, Some(name))),
                _ => None,
            }))
            .collect();
        let Some(index) = headers.iter().rposition(|(_, name)| same_section(name.as_deref(), app)) else {
            if self.lines.last().is_some_and(|last| !last.trim().is_empty()) {
                self.lines.push(String::new());
            }
            self.lines.extend([format!("[{}]", app.unwrap_or_default()), line]);
            return self.read_lines();
        };
        let (start, end) = (headers[index].0, headers.get(index + 1).map_or(self.lines.len(), |&(next, _)| next - 1));
        let last_entry = self.entries.iter().map(|entry| entry.line).rfind(|line| (start..end).contains(line));
        let last_text = (start..end).rev().find(|&at| !self.lines[at].trim().is_empty());
        let at = last_entry.or(last_text).map_or(start, |last| last + 1);
        if at < self.lines.len() && self.lines[at].trim_start().starts_with('[') {
            self.lines.insert(at, String::new());
        }
        self.lines.insert(at, line);
        self.read_lines();
    }

    fn read_lines(&mut self) {
        (self.entries, self.skipped) = (Vec::new(), Vec::new());
        let mut section: Option<String> = None;
        let mut phrases: HashMap<(Option<String>, Vec<String>), usize> = HashMap::new();
        for (at, text) in self.lines.iter().enumerate() {
            match parse_line(text) {
                Ok(Parsed::Nothing) => {}
                Ok(Parsed::Section(name)) => section = Some(name),
                Ok(Parsed::Entry(kind)) => {
                    if let Kind::Action(action) = &kind {
                        let phrase = (section.as_ref().map(|name| name.to_lowercase()), actions::tokens(&action.phrase));
                        if let Some(first) = phrases.get(&phrase) {
                            self.skipped.push((at + 1, format!("this phrase is already used on line {first}")));
                            continue;
                        }
                        phrases.insert(phrase, at + 1);
                    }
                    self.entries.push(Entry { section: section.clone(), kind, line: at });
                }
                Err(why) => self.skipped.push((at + 1, why)),
            }
        }
        let triggers = self.entries.iter().filter_map(|entry| match &entry.kind {
            Kind::Action(action) => Some(Trigger {
                section: entry.section.as_deref(),
                phrase: actions::tokens(&action.phrase).into_iter().map(Part::Word).collect(),
                anywhere: matches!(action.effect, Effect::Paste(_)),
                value: action.clone(),
            }),
            _ => None,
        });
        self.index = Index::build(triggers);
    }

    /// Rules, everywhere and then in `app`'s section, then transforms in the same order.
    fn run(&self, text: &str, app: &str, rules: bool, transforms: bool) -> String {
        let scopes = [None, Some(app)];
        let mut text = text.to_owned();
        if rules {
            for scope in scopes {
                let mut swaps: Vec<Swap> = self.entries.iter().filter(|e| e.is_in(scope)).filter_map(Swap::of).collect();
                // Longest first; a stable sort keeps the file's order between rules as long.
                swaps.sort_by_key(|swap| std::cmp::Reverse(swap.length()));
                if !swaps.is_empty() {
                    text = replace(&text, &swaps);
                }
            }
        }
        if transforms {
            for scope in scopes {
                for entry in self.entries.iter().filter(|e| e.is_in(scope)) {
                    if let Kind::Transform(transform) = entry.kind {
                        text = transform.apply(&text);
                    }
                }
            }
        }
        text
    }
}

fn same_section(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        (a, b) => a.is_none() && b.is_none(),
    }
}

/// A line of the file, or why it can't be read.
fn parse_line(line: &str) -> Result<Parsed, String> {
    // Read before comments and rules: a snippet's text may hold ` # ` and `=>`.
    if let Some(action) = line.trim().strip_prefix('>') {
        return actions::parse(action).map(|action| Parsed::Entry(Kind::Action(action)));
    }
    let line = without_comment(line).trim();
    if line.is_empty() {
        return Ok(Parsed::Nothing);
    }
    if let Some(name) = line.strip_prefix('[') {
        let name = name.strip_suffix(']').ok_or("a section needs a closing ]")?.trim();
        return if name.is_empty() { Err("a section needs an app's name".into()) } else { Ok(Parsed::Section(name.to_owned())) };
    }
    if let Some(word) = line.strip_prefix('@') {
        return parse_word(word).map(Parsed::Entry);
    }
    if let Some(name) = line.strip_prefix('!') {
        let names = Transform::ALL.map(Transform::name).join(", ");
        return Transform::named(name.trim()).map(|t| Parsed::Entry(Kind::Transform(t))).ok_or(format!("no transform \"{}\" (there are {names})", name.trim()));
    }
    let (from, to) = line.split_once("=>").ok_or("not a rule (words => replacement), @ word, ! transform or [App]")?;
    let from = from.split_whitespace().collect::<Vec<_>>().join(" ");
    if from.is_empty() {
        return Err("a rule needs words before =>".into());
    }
    Ok(Parsed::Entry(Kind::Rule { from, to: to.trim().to_owned() }))
}

/// An `@` line's word, and its strength if one follows it (`:2.5`; 0.2.6's `:stronger` and `:weaker` read as
/// the strengths they gave).
fn parse_word(text: &str) -> Result<Kind, String> {
    let text = text.trim();
    let (before, last) = text.rsplit_once(char::is_whitespace).map_or(("", text), |(before, last)| (before.trim(), last));
    let (word, strength) = match last.strip_prefix(':') {
        Some(given) => {
            let strength = match given.to_ascii_lowercase().as_str() {
                "stronger" => Some(2.5),
                "weaker" => Some(1.0),
                number => read_strength(number),
            };
            (before, Some(strength.ok_or(format!("\"{last}\" isn't a strength: give a number above 0, up to 10, like :2.5"))?))
        }
        None => (text, None),
    };
    if word.is_empty() { Err("@ needs a word after it".into()) } else { Ok(Kind::Word { word: word.to_owned(), strength }) }
}

/// The line up to a comment: a # at its start, or with spaces around it.
pub(crate) fn without_comment(line: &str) -> &str {
    if line.trim_start().starts_with('#') {
        return "";
    }
    let bytes = line.as_bytes();
    let comment = (1..bytes.len()).find(|&at| bytes[at] == b'#' && bytes[at - 1].is_ascii_whitespace() && bytes.get(at + 1).is_none_or(u8::is_ascii_whitespace));
    comment.map_or(line, |at| &line[..at])
}

/// A rule ready to match: its words as characters, and what replaces them.
struct Swap<'a> {
    words: Vec<Vec<char>>,
    to: &'a str,
}

impl<'a> Swap<'a> {
    fn of(entry: &'a Entry) -> Option<Self> {
        match &entry.kind {
            Kind::Rule { from, to } => Some(Swap { words: from.split_whitespace().map(|word| word.chars().collect()).collect(), to }),
            _ => None,
        }
    }

    fn length(&self) -> usize {
        self.words.iter().map(Vec::len).sum::<usize>() + self.words.len()
    }

    /// Where a match starting at `start` ends: the words in any case, with whitespace between them and whole
    /// words at both ends.
    fn match_at(&self, text: &[char], start: usize) -> Option<usize> {
        let mut at = start;
        for (index, word) in self.words.iter().enumerate() {
            if index > 0 {
                let spaces = text[at..].iter().take_while(|c| c.is_whitespace()).count();
                if spaces == 0 {
                    return None;
                }
                at += spaces;
            }
            let end = at + word.len();
            if end > text.len() || !text[at..end].iter().zip(word).all(|(&a, &b)| same_letter(a, b)) {
                return None;
            }
            at = end;
        }
        (!in_word(text, at)).then_some(at)
    }
}

/// `text` with every match of `swaps` replaced, trying them in order at each word's start. A deletion also
/// takes a comma right after the words and the spaces after them, or before them at the end.
fn replace(text: &str, swaps: &[Swap]) -> String {
    let text: Vec<char> = text.chars().collect();
    let mut replaced = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let starts_word = at == 0 || !in_word(&text, at - 1);
        if let Some((end, to)) = swaps.iter().filter(|_| starts_word).find_map(|swap| Some((swap.match_at(&text, at)?, swap.to))) {
            at = end;
            if to.is_empty() {
                at += usize::from(text.get(at) == Some(&','));
                at += text[at..].iter().take_while(|c| c.is_whitespace()).count();
                if at == text.len() {
                    replaced.truncate(replaced.trim_end().len());
                }
            } else {
                replaced.push_str(to);
            }
            continue;
        }
        replaced.push(text[at]);
        at += 1;
    }
    replaced
}

/// Whether the character at `at` is part of a word: a letter or digit, or an apostrophe inside a word ("don't").
pub(crate) fn in_word(text: &[char], at: usize) -> bool {
    let Some(&c) = text.get(at) else { return false };
    let letter = |at: Option<usize>| at.and_then(|at| text.get(at)).is_some_and(|c| c.is_alphanumeric());
    c.is_alphanumeric() || (matches!(c, '\'' | '’') && letter(at.checked_sub(1)) && letter(Some(at + 1)))
}

fn same_letter(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// The word rules in use, shared by the decoder and the panel: the file as last read, and the settings.
pub struct WordBook {
    path: PathBuf,
    state: Mutex<Loaded>,
}

#[derive(Default)]
struct Loaded {
    words: Arc<Words>,
    settings: WordSettings,
    watch: FileWatch,
}

impl Default for WordBook {
    fn default() -> Self {
        Self::at(path())
    }
}

impl WordBook {
    /// The rules kept in the file at `path`, read on the first [`WordBook::refresh`].
    pub fn at(path: PathBuf) -> Self {
        Self { path, state: Mutex::default() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the file again if it changed, looking at most once a second; returns whether it was read.
    pub fn refresh(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        if !state.watch.changed(&self.path) {
            return false;
        }
        state.words = Arc::new(self.read());
        true
    }

    /// The rules as last read.
    pub fn words(&self) -> Arc<Words> {
        self.state.lock().unwrap().words.clone()
    }

    pub fn settings(&self) -> WordSettings {
        self.state.lock().unwrap().settings
    }

    pub fn set_settings(&self, settings: WordSettings) {
        self.state.lock().unwrap().settings = settings;
    }

    /// The words the model listens for: the `@` words while the setting is on.
    pub fn hotwords(&self) -> Vec<Hotword> {
        let state = self.state.lock().unwrap();
        if !state.settings.hotwords {
            return Vec::new();
        }
        state.words.hotwords(state.settings.strength)
    }

    /// `text` as it reaches `output` for the app labelled `app`; see [`Words::apply`].
    pub fn apply(&self, text: &str, app: &str, output: Output) -> String {
        let state = self.state.lock().unwrap();
        state.words.apply(text, app, output, &state.settings)
    }

    /// A caption on its way to dictation; see [`Words::apply_to_caption`].
    pub fn apply_to_caption(&self, text: &str, app: &str) -> String {
        let state = self.state.lock().unwrap();
        state.words.apply_to_caption(text, app, &state.settings)
    }

    /// Adds a rule; see [`Words::add_rule`]. Saved at once.
    pub fn add_rule(&self, from: &str, to: &str, app: Option<&str>) -> io::Result<()> {
        self.edit(|words| words.add_rule(from, to, app))
    }

    /// What dictating `said` does in `app`; see [`Words::plan`].
    pub fn plan(&self, said: &[String], app: &str) -> Vec<Step> {
        let state = self.state.lock().unwrap();
        state.words.plan(said, app, &state.settings)
    }

    /// Adds a voice action; see [`Words::add_action`]. Saved at once.
    pub fn add_action(&self, action: &Action, app: Option<&str>) -> io::Result<()> {
        self.edit(|words| words.add_action(action, app))
    }

    /// Adds a word to listen for; see [`Words::add_word`]. Saved at once.
    pub fn add_word(&self, word: &str) -> io::Result<()> {
        self.edit(|words| words.add_word(word))
    }

    /// Changes how strongly a word is listened for; see [`Words::set_strength`]. Saved at once.
    pub fn set_strength(&self, entry: &Entry, strength: Option<f32>) -> io::Result<()> {
        self.edit(|words| if words.set_strength(entry, strength) { Ok(()) } else { Err("That word is no longer in the file".into()) })
    }

    /// Removes an entry; see [`Words::remove`]. Saved at once.
    pub fn remove(&self, entry: &Entry) -> io::Result<()> {
        self.edit(|words| if words.remove(entry) { Ok(()) } else { Err("That entry is no longer in the file".into()) })
    }

    /// Creates the file with a short commented header if it is missing; returns its path, for opening it.
    pub fn create(&self) -> io::Result<&Path> {
        if !self.path.exists() {
            self.write(&Words::parse(HEADER), &mut self.state.lock().unwrap())?;
        }
        Ok(&self.path)
    }

    /// Changes the file as it is on disk now, writing it in one step.
    fn edit(&self, change: impl FnOnce(&mut Words) -> Result<(), String>) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        let mut words = if self.path.exists() { self.read() } else { Words::parse(HEADER) };
        change(&mut words).map_err(io::Error::other)?;
        self.write(&words, &mut state)
    }

    fn write(&self, words: &Words, state: &mut Loaded) -> io::Result<()> {
        if let Some(folder) = self.path.parent() {
            fs::create_dir_all(folder)?;
        }
        transcripts::replace(&self.path, &words.to_text())?;
        state.watch.wrote(&self.path);
        state.words = Arc::new(words.clone());
        Ok(())
    }

    /// The file's rules, logging the lines skipped; a missing or unreadable file has none.
    fn read(&self) -> Words {
        let words = Words::parse(&fs::read_to_string(&self.path).unwrap_or_default());
        for (line, why) in &words.skipped {
            log::write(format!("words.txt line {line} skipped: {why}"));
        }
        words
    }
}

/// Where `words.txt` is.
pub fn path() -> PathBuf {
    paths::data_dir().join("words.txt")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: WordSettings = WordSettings { rules: Scope::Both, transforms: Scope::Both, hotwords: true, strength: DEFAULT_STRENGTH };

    fn run(file: &str, text: &str, app: &str) -> String {
        Words::parse(file).apply(text, app, Output::Captions, &BOTH)
    }

    fn plan_text(file: &str, said: &[&str], app: &str) -> Vec<String> {
        let said: Vec<String> = said.iter().map(|line| line.to_string()).collect();
        let steps = Words::parse(file).plan(&said, app, &BOTH);
        steps.into_iter().map(|step| match step { Step::Paste(text) => text.reveal().to_owned(), Step::Press(_) => "<press>".into() }).collect()
    }

    #[test]
    fn a_snippet_line_is_spaced_from_text_after_it_only() {
        let file = "> sig => paste Regards\n";
        assert_eq!(plan_text(file, &["sig", "thanks"], ""), ["Regards thanks "]);
        assert_eq!(plan_text(file, &["thanks", "sig"], ""), ["thanks Regards"]);
        assert_eq!(plan_text(file, &["sig"], ""), ["Regards"]);
    }

    #[test]
    fn an_apps_action_beats_everywheres_whole_line_action() {
        let file = "> save => press ctrl+s\n> done => paste D\n[Discord]\n> save => paste X\n> done => press ctrl+d\n";
        assert_eq!(plan_text(file, &["save"], "Discord"), ["X"]);
        assert_eq!(plan_text(file, &["done"], "Discord"), ["<press>"]);
        assert_eq!(plan_text(file, &["save"], "Notepad"), ["<press>"]);
    }

    #[test]
    fn the_file_reads_comments_sections_and_skips_bad_lines() {
        let words = Words::parse(
            "# Kiku\nkicku => Kiku   # a comment\n@ Tannhauser\n! no-trailing-punctuation\nnonsense\n! shout\n[Discord]\ngg => good game\n! Capitalize\n[]\nC sharp => C#\n=> nothing\n",
        );
        let kinds: Vec<(Option<&str>, &Kind)> = words.entries().iter().map(|e| (e.section.as_deref(), &e.kind)).collect();
        let rule = |from: &str, to: &str| Kind::Rule { from: from.into(), to: to.into() };
        assert_eq!(
            kinds,
            [
                (None, &rule("kicku", "Kiku")),
                (None, &Kind::Word { word: "Tannhauser".into(), strength: None }),
                (None, &Kind::Transform(Transform::NoTrailingPunctuation)),
                (Some("Discord"), &rule("gg", "good game")),
                (Some("Discord"), &Kind::Transform(Transform::Capitalise)),
                (Some("Discord"), &rule("C sharp", "C#")),
            ]
        );
        assert_eq!(words.skipped.iter().map(|(line, _)| *line).collect::<Vec<_>>(), [5, 6, 10, 12]);
        assert_eq!(words.hotwords(DEFAULT_STRENGTH), [Hotword { text: "Tannhauser".into(), boost: 1.5 }]);
    }

    #[test]
    fn hotwords_have_their_own_strength_or_the_one_settings_give() {
        let file = "@ Kiku\n@ Lull :2.8  # my app\n@ Re:Zero :0.75\n@ old :stronger\n@ lol :loud\n@ :2\n@ x :0\n@ y :11\n";
        let words = Words::parse(file);
        assert_eq!(words.skipped.iter().map(|(line, _)| *line).collect::<Vec<_>>(), [5, 6, 7, 8]);
        let boosts = |strength| words.hotwords(strength).into_iter().map(|word| (word.text, word.boost)).collect::<Vec<_>>();
        let expected = |boosts: [f32; 4]| ["Kiku", "Lull", "Re:Zero", "old"].into_iter().map(String::from).zip(boosts).collect::<Vec<_>>();
        assert_eq!(boosts(1.5), expected([1.5, 2.8, 0.75, 2.5]));
        assert_eq!(boosts(3.2), expected([3.2, 2.8, 0.75, 2.5]), "settings move only the words without their own");
    }

    #[test]
    fn a_words_strength_is_changed_in_place() {
        let mut words = Words::parse("# mine\n@ Lull  # my app\nkicku => Kiku\n");
        let lull = words.entries()[0].clone();
        assert!(words.set_strength(&lull, Some(2.8000002)));
        assert_eq!(words.to_text(), "# mine\n@ Lull :2.8  # my app\nkicku => Kiku\n");
        assert_eq!(words.hotwords(1.5)[0].boost, 2.8);
        let lull = words.entries()[0].clone();
        assert!(words.set_strength(&lull, None));
        assert_eq!(words.to_text(), "# mine\n@ Lull  # my app\nkicku => Kiku\n");
        assert!(!words.set_strength(&words.entries()[1].clone(), Some(2.0)), "a rule has no strength");
    }

    #[test]
    fn rules_match_whole_words_in_any_case_with_punctuation_beside_them() {
        let file = "kicku => Kiku\ncat => dog\n";
        assert_eq!(run(file, "KICKU, is it kicku?", ""), "Kiku, is it Kiku?");
        assert_eq!(run(file, "concatenate the cat's cat.", ""), "concatenate the cat's dog.");
        assert_eq!(run(file, "\"cat\" (cat)", ""), "\"dog\" (dog)");
    }

    #[test]
    fn phrases_match_across_spaces_and_the_longest_rule_wins() {
        let file = "new => old\nnew york => NYC\nnew york city => the city\n";
        assert_eq!(run(file, "New  York is new; New York City too", ""), "NYC is old; the city too");
        assert_eq!(run(file, "new, york", ""), "old, york");
    }

    #[test]
    fn rules_use_the_output_as_written_and_can_delete() {
        let file = "um =>\nheart => ❤️\n";
        assert_eq!(run(file, "Um, I think, um, it's my heart um", ""), "I think, it's my ❤️");
    }

    #[test]
    fn global_entries_run_before_the_apps_and_rules_before_transforms() {
        let file = "! capitalise\nbrb => be right back.\n[Discord]\nback => there\n! no-trailing-punctuation\n[You]\n! lowercase\n";
        assert_eq!(run(file, "brb", "discord"), "Be right there");
        assert_eq!(run(file, "brb", "Spotify"), "Be right back.");
        assert_eq!(run(file, "BRB", "You"), "be right back.");
    }

    #[test]
    fn transforms_change_the_whole_line() {
        assert_eq!(Transform::Lowercase.apply("Hello THERE"), "hello there");
        assert_eq!(Transform::NoTrailingPunctuation.apply("Really?! "), "Really");
        assert_eq!(Transform::NoTrailingPunctuation.apply("\"Quoted.\""), "\"Quoted.\"");
        assert_eq!(Transform::Capitalise.apply("¿qué tal?"), "¿Qué tal?");
    }

    #[test]
    fn settings_choose_where_rules_and_transforms_apply() {
        let words = Words::parse("kicku => Kiku\n! lowercase\n");
        let defaults = WordSettings::default();
        assert_eq!(words.apply("Hi kicku", "", Output::Captions, &defaults), "Hi Kiku");
        assert_eq!(words.apply("Hi kicku", "", Output::Dictation, &defaults), "hi kiku");
        let captions_only = WordSettings { rules: Scope::Captions, transforms: Scope::Captions, ..defaults };
        assert_eq!(words.apply("Hi kicku", "", Output::Dictation, &captions_only), "Hi kicku");
        // A caption pasted by dictation had the rules already, so only the lowercase is left to do.
        assert_eq!(words.apply_to_caption("Hi Kiku", "", &defaults), "hi kiku");
        assert_eq!(words.apply_to_caption("Hi kicku", "", &captions_only), "Hi kicku");

        let mut read = WordSettings::default();
        let changed = WordSettings { rules: Scope::Dictation, transforms: Scope::Both, hotwords: false, strength: 3.2 };
        for line in changed.to_settings().lines().chain(["words.rules = loud", "words.other = both"]) {
            let (key, value) = line.split_once(" = ").unwrap();
            read.apply_setting(key, value);
        }
        assert_eq!(read, changed);
        read.apply_setting("words.strength", "strong");
        assert_eq!(read.strength, 2.5);
    }

    #[test]
    fn the_preview_shows_each_app_whose_section_changes_the_text() {
        let words = Words::parse("kicku => Kiku\n[Discord]\ngg => good game\n[Spotify]\n! lowercase\n");
        let preview = words.preview("gg kicku", &WordSettings::default());
        let expected = [("Captions", "gg Kiku"), ("Captions in Discord", "good game Kiku"), ("Dictation", "gg Kiku"), ("Dictation in Discord", "good game Kiku"), ("Dictation in Spotify", "gg kiku")];
        assert_eq!(preview, expected.map(|(label, text)| (label.to_owned(), text.to_owned())));
    }

    #[test]
    fn edits_keep_comments_and_order() {
        let mut words = Words::parse("# mine\nkicku => Kiku\n\n[Discord]\ngg => good game\n\n# later\n");
        words.add_rule("lol", "😂", Some("discord")).unwrap();
        words.add_rule("  um  ", "", None).unwrap();
        words.add_word("Tannhauser").unwrap();
        words.add_word("tannhauser").unwrap();
        words.add_rule("hi", "hello", Some("Slack")).unwrap();
        assert_eq!(
            words.to_text(),
            "# mine\nkicku => Kiku\num =>\n@ Tannhauser\n\n[Discord]\ngg => good game\nlol => 😂\n\n# later\n\n[Slack]\nhi => hello\n"
        );
        let gg = words.entries().iter().find(|e| matches!(&e.kind, Kind::Rule { from, .. } if from == "gg")).unwrap().clone();
        assert!(words.remove(&gg));
        assert!(!words.remove(&gg));
        assert!(!words.to_text().contains("gg"));
        assert!(words.add_rule("a => b", "c", None).is_err());
        assert!(words.add_rule(" ", "c", None).is_err());
    }

    #[test]
    fn a_rule_added_to_a_file_with_only_sections_goes_before_them() {
        let mut words = Words::parse("[Discord]\ngg => good game\n");
        words.add_rule("kicku", "Kiku", None).unwrap();
        assert_eq!(words.to_text(), "kicku => Kiku\n\n[Discord]\ngg => good game\n");
        assert_eq!(words.entries()[0].section, None);
    }

    #[test]
    fn the_book_writes_the_file_and_reads_changes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-words/words.txt");
        let _ = fs::remove_file(&path);
        let book = WordBook::at(path.clone());
        assert!(book.refresh());
        assert!(book.words().entries().is_empty());
        book.add_rule("kicku", "Kiku", None).unwrap();
        assert!(fs::read_to_string(&path).unwrap().starts_with("# Kiku's word rules"));
        assert_eq!(book.apply("kicku", "", Output::Captions), "Kiku");
        book.remove(&book.words().entries()[0].clone()).unwrap();
        assert_eq!(book.apply("kicku", "", Output::Captions), "kicku");

        book.add_word("Tannhauser").unwrap();
        assert_eq!(book.hotwords(), [Hotword { text: "Tannhauser".into(), boost: 1.5 }]);
        book.set_settings(WordSettings { hotwords: false, ..book.settings() });
        assert!(book.hotwords().is_empty(), "the setting turns listening off");
    }
}
