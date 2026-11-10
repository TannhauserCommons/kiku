//! The variable language: `{name}`, `{name:arguments}`, then `|filter` or `|filter:argument`; `{{` and `}}`
//! are braces.

use std::{
    fmt::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{
    format::{Item, StrftimeItems},
    NaiveDateTime,
};

use super::transcript::{HowMuch, TranscriptLine, Who};
use crate::{actions::Private, words::Transform};

/// What filling a template needs from the world; the app's reads the clipboard, captions and files.
pub trait Context {
    /// A value from `variables.toml`.
    fn variable(&self, name: &str) -> Option<Private>;
    /// Local time.
    fn now(&self) -> NaiveDateTime;
    fn clipboard(&self) -> Private;
    fn selection(&self) -> Private;
    /// The app in front, named as captions name it.
    fn app(&self) -> String;
    /// Its window's title.
    fn window(&self) -> String;
    /// A path as written in a macro: relative to `custom\`, or absolute.
    fn resolve(&self, path: &Path) -> PathBuf;
    /// A text file's contents (M7's limits: [`super::read_text_file`]); `path` is resolved.
    fn read_file(&self, path: &Path) -> Result<Private, String>;
    /// Finished lines, oldest first.
    fn transcript(&self, who: &Who, how_much: &HowMuch) -> Vec<TranscriptLine>;
    /// `text` through the word rules of the app in front.
    fn apply_rules(&self, text: &str) -> String;
    /// Which of `choices` `{random:...}` gives.
    fn pick(&self, choices: &[&str]) -> usize;
}

/// A parsed template. Its `Debug` shows no text, as it can hold private text.
#[derive(Clone, Default, PartialEq)]
pub struct Template {
    nodes: Vec<Node>,
}

#[derive(Clone, PartialEq)]
enum Node {
    Text(String),
    Variable(Variable),
}

/// `{source|filter|filter}`.
#[derive(Clone, PartialEq)]
struct Variable {
    source: Source,
    filters: Vec<Filter>,
}

#[derive(Clone, PartialEq)]
enum Source {
    /// A name that isn't built in: `variables.toml`'s, or a macro's slot until it is bound.
    Name(String),
    /// A value bound to a name when a plan is made.
    Bound(Private),
    /// `date`, `time` and `weekday`: now, in this chrono format.
    Clock(String),
    Clipboard,
    Selection,
    App,
    Window,
    File(String),
    Random(Vec<String>),
    Transcript(Who, HowMuch),
}

#[derive(Clone, PartialEq)]
enum Filter {
    Transform(Transform),
    Uppercase,
    Title,
    Trim,
    Rules,
    Url,
    Quote,
    Oneline,
    Words(usize),
    Chars(usize),
    Names,
    Times,
}

/// A value on its way through the filters. A transcript keeps each line's speaker and time while its
/// text still has one line per entry, which is what `names` and `times` need.
struct Value {
    text: String,
    lines: Vec<(String, NaiveDateTime)>,
}

impl Template {
    /// Parses `text`; an unknown built-in variable's arguments or an unknown filter is an error naming it.
    pub fn parse(text: &str) -> Result<Template, String> {
        let mut nodes = Vec::new();
        let mut literal = String::new();
        let mut rest = text;
        while let Some(at) = rest.find(['{', '}']) {
            literal.push_str(&rest[..at]);
            let brace = rest.as_bytes()[at] as char;
            let after = &rest[at + 1..];
            if let Some(doubled) = after.strip_prefix(brace) {
                literal.push(brace);
                rest = doubled;
                continue;
            }
            if brace == '}' {
                return Err("a } with no { (write }} for a brace)".into());
            }
            let end = after.find('}').ok_or("a { with no } (write {{ for a brace)")?;
            if !literal.is_empty() {
                nodes.push(Node::Text(std::mem::take(&mut literal)));
            }
            nodes.push(Node::Variable(Variable::parse(&after[..end])?));
            rest = &after[end + 1..];
        }
        literal.push_str(rest);
        if !literal.is_empty() {
            nodes.push(Node::Text(literal));
        }
        Ok(Template { nodes })
    }

    /// Text taken as written: no braces are variables.
    pub fn literal(text: &str) -> Template {
        let mut template = Template::default();
        template.push_str(text);
        template
    }

    /// Adds `text` as written at the end.
    pub fn push_str(&mut self, text: &str) {
        match self.nodes.last_mut() {
            Some(Node::Text(last)) => last.push_str(text),
            _ if text.is_empty() => {}
            _ => self.nodes.push(Node::Text(text.to_owned())),
        }
    }

    /// The text if it has no variables.
    pub fn as_literal(&self) -> Option<&str> {
        match self.nodes.as_slice() {
            [] => Some(""),
            [Node::Text(text)] => Some(text),
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Adds `other` at the end.
    pub fn append(&mut self, other: &Template) {
        let mut nodes = other.nodes.iter();
        if let Some(Node::Text(text)) = other.nodes.first() {
            self.push_str(text);
            nodes.next();
        }
        self.nodes.extend(nodes.cloned());
    }

    /// The names it uses that aren't built in: `variables.toml`'s, or a macro's slots.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for node in &self.nodes {
            if let Node::Variable(Variable { source: Source::Name(name), .. }) = node
                && !names.contains(name)
            {
                names.push(name.clone());
            }
        }
        names
    }

    /// This template with `value` in place of every use of `name` (a slot, or the line heard).
    pub fn bind(&self, name: &str, value: &Private) -> Template {
        let mut bound = self.clone();
        for node in &mut bound.nodes {
            if let Node::Variable(Variable { source, .. }) = node
                && matches!(source, Source::Name(used) if used == name)
            {
                *source = Source::Bound(value.clone());
            }
        }
        bound
    }

    /// The text, filled; an error says why (a file that couldn't be read) and never holds a value.
    pub fn fill(&self, context: &dyn Context) -> Result<Private, String> {
        let mut filled = String::new();
        for node in &self.nodes {
            match node {
                Node::Text(text) => filled.push_str(text),
                Node::Variable(variable) => filled.push_str(&variable.value(context)?.text),
            }
        }
        Ok(Private::new(filled))
    }
}

impl Variable {
    fn parse(body: &str) -> Result<Variable, String> {
        let name_end = body.find([':', '|']).unwrap_or(body.len());
        let (name, mut rest) = body.split_at(name_end);
        let arguments = rest.strip_prefix(':').map(|arguments| {
            // `|` separates `random`'s choices, so nothing can be filtered after them.
            let end = if name == "random" { arguments.len() } else { arguments.find('|').unwrap_or(arguments.len()) };
            rest = &arguments[end..];
            &arguments[..end]
        });
        let filters = match rest.strip_prefix('|') {
            Some(filters) => filters.split('|').map(Filter::parse).collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        check_labels_first(&filters)?;
        Ok(Variable { source: Source::parse(name, arguments)?, filters })
    }

    fn value(&self, context: &dyn Context) -> Result<Value, String> {
        let mut value = self.source.value(context)?;
        for filter in &self.filters {
            value = filter.apply(value, context);
        }
        Ok(value)
    }
}

impl Source {
    fn parse(name: &str, arguments: Option<&str>) -> Result<Source, String> {
        let none = |source: Source| match arguments {
            None => Ok(source),
            Some(_) => Err(format!("{} takes no arguments", shown(name, "a variable"))),
        };
        let needed = || arguments.filter(|arguments| !arguments.is_empty()).ok_or(format!("{} needs an argument", shown(name, "a variable")));
        match name {
            "" => Err("a { } with no name".into()),
            "date" => clock(arguments.unwrap_or("%Y-%m-%d")),
            "time" => clock(arguments.unwrap_or("%H:%M")),
            "weekday" => none(Source::Clock("%A".into())),
            "clipboard" => none(Source::Clipboard),
            "selection" => none(Source::Selection),
            "app" => none(Source::App),
            "window" => none(Source::Window),
            "file" => Ok(Source::File(needed()?.to_owned())),
            "random" => Ok(Source::Random(needed()?.split('|').map(str::to_owned).collect())),
            "transcript" => {
                let arguments = needed()?;
                let (who, how_much) = arguments.split_once(':').unwrap_or((arguments, "line"));
                Ok(Source::Transcript(parse_who(who)?, parse_how_much(how_much)?))
            }
            "last_line" => Ok(Source::Transcript(parse_who(needed()?)?, HowMuch::Line)),
            "last_minute" => Ok(Source::Transcript(parse_who(needed()?)?, HowMuch::Last(Duration::from_secs(60)))),
            _ => none(Source::Name(name.to_owned())),
        }
    }

    fn value(&self, context: &dyn Context) -> Result<Value, String> {
        let text = match self {
            Source::Name(name) => context.variable(name).ok_or(format!("no value for {}", shown(name, "a variable")))?.reveal().to_owned(),
            Source::Bound(value) => value.reveal().to_owned(),
            Source::Clock(format) => {
                let mut text = String::new();
                write!(text, "{}", context.now().format(format)).map_err(|_| format!("{} isn't a date format", shown(format, "that")))?;
                text
            }
            Source::Clipboard => context.clipboard().reveal().to_owned(),
            Source::Selection => context.selection().reveal().to_owned(),
            Source::App => context.app(),
            Source::Window => context.window(),
            Source::File(path) => context.read_file(&context.resolve(Path::new(path)))?.reveal().to_owned(),
            Source::Random(choices) => {
                let choices: Vec<&str> = choices.iter().map(String::as_str).collect();
                choices[context.pick(&choices) % choices.len()].to_owned()
            }
            Source::Transcript(who, how_much) => {
                let lines = context.transcript(who, how_much);
                let text = lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n");
                let lines = lines.into_iter().map(|line| (line.speaker, line.time)).collect();
                return Ok(Value { text, lines });
            }
        };
        Ok(Value { text, lines: Vec::new() })
    }
}

impl Filter {
    fn parse(spec: &str) -> Result<Filter, String> {
        let (name, argument) = match spec.split_once(':') {
            Some((name, argument)) => (name, Some(argument)),
            None => (spec, None),
        };
        let count = || {
            let argument = argument.ok_or(format!("{} needs a number", shown(name, "a filter")))?;
            argument.trim().parse::<usize>().map_err(|_| format!("{} needs a number, not {}", shown(name, "a filter"), shown(argument, "that")))
        };
        let filter = match name {
            "uppercase" => Filter::Uppercase,
            "title" => Filter::Title,
            "trim" => Filter::Trim,
            "rules" => Filter::Rules,
            "url" => Filter::Url,
            "quote" => Filter::Quote,
            "oneline" => Filter::Oneline,
            "names" => Filter::Names,
            "times" => Filter::Times,
            "words" => return Ok(Filter::Words(count()?)),
            "chars" => return Ok(Filter::Chars(count()?)),
            _ => Filter::Transform(Transform::named(name).ok_or(format!("unknown filter {}", shown(name, "(an unknown name)")))?),
        };
        match argument {
            None => Ok(filter),
            Some(_) => Err(format!("{} takes no argument", shown(name, "a filter"))),
        }
    }

    fn apply(&self, value: Value, context: &dyn Context) -> Value {
        match self {
            Filter::Names => return value.labelled(|(speaker, _)| format!("{speaker}: ")),
            Filter::Times => return value.labelled(|(_, time)| time.format("[%H:%M] ").to_string()),
            _ => {}
        }
        let text = &value.text;
        let changed = match self {
            Filter::Transform(transform) => transform.apply(text),
            Filter::Uppercase => text.to_uppercase(),
            Filter::Title => title(text),
            Filter::Trim => text.trim().to_owned(),
            Filter::Rules => context.apply_rules(text),
            Filter::Url => percent_encode(text),
            Filter::Quote => text.lines().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n"),
            Filter::Oneline => text.lines().collect::<Vec<_>>().join(" "),
            Filter::Words(count) => text.split_whitespace().take(*count).collect::<Vec<_>>().join(" "),
            Filter::Chars(count) => text.chars().take(*count).collect(),
            Filter::Names | Filter::Times => unreachable!("handled above"),
        };
        value.with_text(changed)
    }
}

impl Value {
    /// `text` in place of the old, keeping the lines' speakers and times only while the text has a line for each.
    fn with_text(self, text: String) -> Value {
        let keeps = text.split('\n').count() == self.lines.len();
        Value { text, lines: if keeps { self.lines } else { Vec::new() } }
    }

    /// Each line of a transcript starting with `label` of its entry; other text is left as it is.
    fn labelled(self, label: impl Fn(&(String, NaiveDateTime)) -> String) -> Value {
        if self.lines.is_empty() || self.text.split('\n').count() != self.lines.len() {
            return self;
        }
        let text = self.text.split('\n').zip(&self.lines).map(|(line, entry)| format!("{}{line}", label(entry))).collect::<Vec<_>>().join("\n");
        Value { text, lines: self.lines }
    }
}

/// `names` and `times` label a transcript's lines, which the filters that cut or join lines leave nothing to label.
fn check_labels_first(filters: &[Filter]) -> Result<(), String> {
    let cut = filters.iter().position(|filter| matches!(filter, Filter::Words(_) | Filter::Chars(_) | Filter::Oneline));
    let label = filters.iter().rposition(|filter| matches!(filter, Filter::Names | Filter::Times));
    match (cut, label) {
        (Some(cut), Some(label)) if label > cut => Err("names and times must come before words, chars and oneline".into()),
        _ => Ok(()),
    }
}

fn clock(format: &str) -> Result<Source, String> {
    if StrftimeItems::new(format).any(|item| item == Item::Error) {
        return Err(format!("{} isn't a date format", shown(format, "that")));
    }
    Ok(Source::Clock(format.to_owned()))
}

fn parse_who(text: &str) -> Result<Who, String> {
    let text = text.trim();
    match text.to_lowercase().as_str() {
        "" => Err("a transcript needs to say whose".into()),
        "you" => Ok(Who::You),
        "others" => Ok(Who::Others),
        "everyone" => Ok(Who::Everyone),
        "front" => Ok(Who::Front),
        _ => Ok(Who::App(text.to_owned())),
    }
}

/// The longest span of transcript asked for (100 years), so subtracting it from the clock can't overflow.
const MAX_SECONDS: u64 = 100 * 366 * 24 * 3600;

fn parse_how_much(text: &str) -> Result<HowMuch, String> {
    let lower = text.trim().to_lowercase();
    let number = |digits: &str| digits.trim().parse::<u64>().ok();
    let seconds = |amount: Option<u64>, unit: u64| amount.and_then(|amount| amount.checked_mul(unit)).filter(|seconds| *seconds <= MAX_SECONDS);
    let parsed = match lower.as_str() {
        "line" => Some(HowMuch::Line),
        "paragraph" => Some(HowMuch::Paragraph),
        "since me" => Some(HowMuch::SinceMe),
        "session" => Some(HowMuch::Session),
        _ => lower
            .strip_suffix(" lines")
            .or(lower.strip_suffix(" line"))
            .and_then(number)
            .map(|count| HowMuch::Lines(count as usize))
            .or_else(|| lower.strip_suffix('m').and_then(|digits| seconds(number(digits), 60)).map(|s| HowMuch::Last(Duration::from_secs(s))))
            .or_else(|| lower.strip_suffix('s').and_then(|digits| seconds(number(digits), 1)).map(|s| HowMuch::Last(Duration::from_secs(s)))),
    };
    parsed.ok_or(format!("{} isn't an amount of transcript (line, N lines, Nm, Ns, paragraph, since me or session)", shown(text, "that")))
}

/// Upper-cases the first letter of each word.
fn title(text: &str) -> String {
    let mut after_space = true;
    text.chars()
        .flat_map(|c| {
            let upper = if after_space { c.to_uppercase().collect::<String>() } else { c.to_string() };
            after_space = c.is_whitespace();
            upper.chars().collect::<Vec<_>>()
        })
        .collect()
}

/// Everything but letters, digits and `-_.~` as `%XX`.
fn percent_encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

impl fmt::Debug for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Template(..)")
    }
}

/// What a message may quote from inside braces: `text` when it looks like a name, else `instead`.
/// Step text is private, so a mistyped secret must not come back in an error.
pub(super) fn shown(text: &str, instead: &str) -> String {
    let name_like = text.chars().count() <= 32 && text.chars().all(|c| c.is_alphanumeric() || " _-%".contains(c));
    if name_like { format!("{text:?}") } else { instead.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_never_quote_what_does_not_look_like_a_name() {
        let secret = "x".repeat(40);
        for text in ["{api_key=\"abc123def\"}", "{tok:abc}", "{date:%Y/%m/%d}", "{words:abc=def}", "{transcript:a=b}", "{nope=1|zz=2}", &format!("{{{secret}}}"), &format!("{{{secret}:1}}"), &format!("{{words:{secret}}}"), &format!("{{date:{secret}}}"), &format!("{{a|{secret}}}")] {
            if let Err(why) = Template::parse(text) {
                for leaked in ["abc123def", "api_key", "=def", "a=b", "zz=2", "%Y/%m/%d", secret.as_str()] {
                    assert!(!why.contains(leaked), "{text} -> {why}");
                }
            }
        }
        assert!(Template::parse("{frobnicate}").is_ok());
        assert!(Template::parse("{a|frobnicate}").unwrap_err().contains("frobnicate"));
    }

    #[test]
    fn an_amount_too_large_is_rejected_not_overflowed() {
        assert!(Template::parse("{transcript:Discord:400000000000000000m}").is_err());
        assert!(Template::parse("{transcript:Discord:18446744073709551615s}").is_err());
        assert!(Template::parse("{transcript:Discord:90m}").is_ok());
    }

    #[test]
    fn names_and_times_must_come_before_filters_that_cut_lines() {
        for after in ["words:3", "chars:9", "oneline"] {
            assert!(Template::parse(&format!("{{transcript:others:2m|{after}|names}}")).is_err());
            assert!(Template::parse(&format!("{{transcript:others:2m|{after}|times}}")).is_err());
        }
        assert!(Template::parse("{transcript:others:2m|names|times|oneline}").is_ok());
    }

    #[test]
    fn an_empty_line_is_still_labelled() {
        let time = NaiveDateTime::default();
        let value = Value { text: String::new(), lines: vec![("Ann".into(), time)] };
        assert_eq!(value.labelled(|(speaker, _)| format!("{speaker}: ")).text, "Ann: ");
    }
}
