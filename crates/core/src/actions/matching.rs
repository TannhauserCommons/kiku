//! Finding triggers in what was said: an index per section, built when the file is read, so a line costs
//! lookups, not a scan of every trigger. A phrase said as the whole line sits in a trie of its tokens (with
//! slots that take a number or the rest of the line); a phrase also found inside a sentence sits in a second
//! trie of plain tokens. Voice actions (`words.txt`) and macros (`macros.toml`) share it.

use std::collections::HashMap;

use super::{words, Word};

/// Snippets found in one line become private-use characters from here, so nothing else can be mistaken for one.
const SENTINEL_START: u32 = 0xE000;
const SENTINEL_END: u32 = 0xF8FF;

/// One word of a trigger's phrase.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Part {
    /// A token as [`super::tokens`] makes them.
    Word(String),
    /// One token that is digits (number words included).
    Number,
    /// The rest of the line, at least one word; ends a phrase.
    Rest,
}

/// A phrase that, said in `section` (`None` is everywhere), finds `value`.
pub(crate) struct Trigger<'a, T> {
    pub section: Option<&'a str>,
    pub phrase: Vec<Part>,
    /// Also found inside a sentence, not only as the whole line; its phrase is words alone.
    pub anywhere: bool,
    pub value: T,
}

/// What a line of dictation holds.
pub(crate) enum Said<'a, T> {
    /// The whole line is a trigger's phrase, with what its slots took, in phrase order.
    Line(&'a T, Vec<String>),
    /// The line through the caller's rules, with the triggers found inside it.
    Text(Vec<Chunk<'a, T>>),
}

pub(crate) enum Chunk<'a, T> {
    Text(String),
    Trigger(&'a T),
}

#[derive(Clone, PartialEq)]
struct Node<T> {
    value: Option<T>,
    next: HashMap<String, Node<T>>,
    number: Option<Box<Node<T>>>,
    rest: Option<T>,
}

impl<T> Default for Node<T> {
    fn default() -> Self {
        Node { value: None, next: HashMap::new(), number: None, rest: None }
    }
}

impl<T> Node<T> {
    fn is_empty(&self) -> bool {
        self.value.is_none() && self.next.is_empty() && self.number.is_none() && self.rest.is_none()
    }

    fn insert(&mut self, phrase: Vec<Part>, value: T) {
        let mut node = self;
        for part in phrase {
            node = match part {
                Part::Word(token) => node.next.entry(token).or_default(),
                Part::Number => node.number.get_or_insert_default(),
                Part::Rest => {
                    node.rest = Some(value);
                    return;
                }
            };
        }
        node.value = Some(value);
    }

    /// What exactly `words` say, a literal word before a number slot before the rest; the slots' values go
    /// to `taken`.
    fn exact<'a>(&'a self, words: &[Word], chars: &[char], taken: &mut Vec<String>) -> Option<&'a T> {
        let Some((word, after)) = words.split_first() else { return self.value.as_ref() };
        if let Some(found) = self.next.get(&word.token).and_then(|next| next.exact(after, chars, taken)) {
            return Some(found);
        }
        if let Some(number) = self.number.as_deref().filter(|_| word.token.bytes().all(|b| b.is_ascii_digit())) {
            taken.push(word.token.clone());
            if let Some(found) = number.exact(after, chars, taken) {
                return Some(found);
            }
            taken.pop();
        }
        let last = words.last().unwrap_or(word);
        // Punctuation glued to the front of the first word (an opening quote) is part of what was said.
        let start = chars[..word.start].iter().rposition(|c| c.is_whitespace() || c.is_alphanumeric()).map_or(0, |at| at + 1);
        self.rest.as_ref().inspect(|_| taken.push(chars[start..last.end].iter().collect()))
    }

    /// The longest phrase starting at `words[0]`, with how many words it takes.
    fn longest(&self, words: &[Word]) -> Option<(usize, &T)> {
        let (mut node, mut best) = (self, None);
        for (at, word) in words.iter().enumerate() {
            let Some(next) = node.next.get(&word.token) else { break };
            node = next;
            if let Some(value) = &node.value {
                best = Some((at + 1, value));
            }
        }
        best
    }
}

#[derive(Clone, PartialEq)]
struct Section<T> {
    lines: Node<T>,
    anywhere: Node<T>,
}

impl<T> Default for Section<T> {
    fn default() -> Self {
        Section { lines: Node::default(), anywhere: Node::default() }
    }
}

/// Every trigger by section, for looking up what was said.
#[derive(Clone, PartialEq)]
pub(crate) struct Index<T> {
    everywhere: Section<T>,
    /// By app name, lowercased.
    apps: HashMap<String, Section<T>>,
}

impl<T> Default for Index<T> {
    fn default() -> Self {
        Index { everywhere: Section::default(), apps: HashMap::new() }
    }
}

/// The sections that apply in one app: its own, then everywhere's.
pub(crate) struct Sections<'a, T>(Vec<&'a Section<T>>);

impl<T: Clone> Index<T> {
    pub fn build<'a>(triggers: impl Iterator<Item = Trigger<'a, T>>) -> Index<T> {
        let mut index = Index::default();
        for Trigger { section, phrase, anywhere, value } in triggers {
            let section = match section {
                Some(app) => index.apps.entry(app.to_lowercase()).or_default(),
                None => &mut index.everywhere,
            };
            if anywhere {
                section.anywhere.insert(phrase.clone(), value.clone());
            }
            section.lines.insert(phrase, value);
        }
        index
    }
}

impl<T> Index<T> {
    pub fn is_empty(&self) -> bool {
        self.everywhere.lines.is_empty() && self.apps.is_empty()
    }

    pub fn sections(&self, app: &str) -> Sections<'_, T> {
        Sections(self.apps.get(&app.to_lowercase()).into_iter().chain([&self.everywhere]).collect())
    }
}

impl<'a, T> Sections<'a, T> {
    /// What `line` is in `app`: a trigger if it is exactly a trigger's phrase, else the line through `apply`
    /// with the triggers inside it found. `None` when nothing is left to say.
    pub fn said(&self, line: &str, apply: impl Fn(&str) -> String) -> Option<Said<'a, T>> {
        let chars: Vec<char> = line.chars().filter(|c| !(SENTINEL_START..=SENTINEL_END).contains(&(*c as u32))).collect();
        let spoken = words(&chars);
        for section in &self.0 {
            let mut taken = Vec::new();
            if let Some(value) = section.lines.exact(&spoken, &chars, &mut taken) {
                return Some(Said::Line(value, taken));
            }
        }
        let (marked, found) = self.mark(&chars, &spoken);
        let text = apply(&marked);
        if text.trim().is_empty() {
            return None;
        }
        Some(Said::Text(split(&text, &found)))
    }

    /// `chars` with each anywhere trigger in it, longest first at each word, replaced by a sentinel numbered
    /// by its place in the returned triggers.
    fn mark(&self, chars: &[char], spoken: &[Word]) -> (String, Vec<&'a T>) {
        let (mut marked, mut found) = (String::with_capacity(chars.len()), Vec::new());
        let (mut copied, mut at) = (0, 0);
        while at < spoken.len() {
            let longest = self.0.iter().filter_map(|section| section.anywhere.longest(&spoken[at..])).reduce(|best, next| if next.0 > best.0 { next } else { best });
            let sentinel = char::from_u32(SENTINEL_START + found.len() as u32).filter(|c| *c as u32 <= SENTINEL_END);
            let (Some((length, value)), Some(sentinel)) = (longest, sentinel) else {
                at += 1;
                continue;
            };
            marked.extend(&chars[copied..spoken[at].start]);
            marked.push(sentinel);
            found.push(value);
            copied = spoken[at + length - 1].end;
            at += length;
        }
        marked.extend(&chars[copied..]);
        (marked, found)
    }
}

/// `text` cut at its sentinels, each replaced by the trigger it stands for.
fn split<'a, T>(text: &str, found: &[&'a T]) -> Vec<Chunk<'a, T>> {
    let mut chunks = Vec::new();
    let mut literal = String::new();
    for c in text.chars() {
        match (c as u32).checked_sub(SENTINEL_START).and_then(|at| found.get(at as usize)) {
            Some(value) => {
                if !literal.is_empty() {
                    chunks.push(Chunk::Text(std::mem::take(&mut literal)));
                }
                chunks.push(Chunk::Trigger(*value));
            }
            None => literal.push(c),
        }
    }
    if !literal.is_empty() {
        chunks.push(Chunk::Text(literal));
    }
    chunks
}
