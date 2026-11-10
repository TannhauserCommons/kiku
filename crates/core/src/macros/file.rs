//! `macros.toml` and `variables.toml` as data, and reading the text files macros paste.

use std::{
    collections::BTreeMap,
    fmt, fs,
    io::Read,
    path::Path,
};

use toml_edit::{Array, ArrayOfTables, Document, DocumentMut, InlineTable, Item, RawString, Table, TomlError, Value};

use super::{
    commands::{DEFAULT_OUTPUT, DEFAULT_TIMEOUT_MS},
    modes::OwnMode,
};
use crate::{actions::Private, keybinds::Hotkey};

/// The most a pasted text file may hold.
pub const MAX_FILE_BYTES: u64 = 1 << 20;
/// The most files one `attach` step takes.
pub const MAX_ATTACHED: usize = 20;

/// The user's own values from `variables.toml`, all private.
#[derive(Clone, Default)]
pub struct Variables(BTreeMap<String, Private>);

impl Variables {
    /// Reads `variables.toml`; an error never quotes a value.
    pub fn parse(toml: &str) -> Result<Variables, String> {
        let table: toml::Table = toml.parse().map_err(|why: toml::de::Error| match why.span() {
            Some(span) => format!("line {}: {}", toml[..span.start].matches('\n').count() + 1, why.message()),
            None => why.message().to_owned(),
        })?;
        let values = table
            .into_iter()
            .map(|(name, value)| match value {
                toml::Value::String(text) => Ok((name, Private::new(text))),
                _ => Err(format!("{name} must be text")),
            })
            .collect::<Result<_, String>>()?;
        Ok(Variables(values))
    }

    pub fn get(&self, name: &str) -> Option<Private> {
        self.0.get(name).cloned()
    }

    pub fn names(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }
}

/// A macro as written in `macros.toml`. Its `Debug` shows no step's text.
#[derive(Clone, PartialEq)]
pub struct Macro {
    pub name: String,
    /// The phrases that run it, with slots (`go to line {n:number}`).
    pub say: Vec<String>,
    /// Only in these apps (as `[App]` sections); empty is everywhere.
    pub apps: Vec<String>,
    /// May be said mid-sentence; only for macros whose steps all paste text.
    pub anywhere: bool,
    /// Off in the Macros tab: listed, never run (`enabled = false`).
    pub enabled: bool,
    /// A shortcut that runs it alone, without saying it.
    pub hotkey: Option<Hotkey>,
    /// Runs when your mic's finished line is one of its phrases, without the dictation key.
    pub listen: bool,
    /// Only in these modes; empty is every mode but `off`.
    pub modes: Vec<String>,
    pub steps: Vec<StepSpec>,
}

/// A step as written: its text is a template, unparsed.
#[derive(Clone, PartialEq)]
pub enum StepSpec {
    Paste(String),
    PasteFile { path: String, raw: bool },
    Attach(Vec<String>),
    Press(String),
    /// The selection (`None`) or this text, to the clipboard.
    Copy(Option<String>),
    Type(String),
    /// Milliseconds.
    Wait(u64),
    Notify(String),
    /// A URL, a file or folder, or an app by name.
    Open(String),
    /// Brings an app (as captions name it) to the front.
    Switch(String),
    /// A program, relative to `custom\scripts\` or absolute, with its arguments, what to do with what it
    /// prints (`paste`, `notify`, `variable:name`, `none`) and how long it may take.
    Run { program: String, args: Vec<String>, output: String, timeout_ms: u64 },
    AskClaude(String),
    /// Switches to the mode of this name.
    Mode(String),
    /// One of Kiku's actions, by name, with `open settings`' tab or `open history`'s search; each a template.
    Kiku { action: String, tab: Option<String>, search: Option<String> },
}

const MACRO_KEYS: [&str; 9] = ["name", "say", "apps", "anywhere", "enabled", "hotkey", "listen", "modes", "steps"];
const MODE_KEYS: [&str; 2] = ["name", "apps"];
const STEP_KEYS: [&str; 14] = ["paste", "paste_file", "attach", "press", "copy", "type", "wait", "notify", "mode", "open", "switch", "run", "ask_claude", "kiku"];
/// Keys that belong to a step beside its own, and the step they belong to.
const STEP_OPTIONS: [(&str, &str); 6] = [("raw", "paste_file"), ("args", "run"), ("output", "run"), ("timeout_ms", "run"), ("tab", "kiku"), ("search", "kiku")];

/// A `[[macro]]` table: where its header starts (see [`line_of`]) and the macro it reads as, or why not.
pub(crate) type Located = (usize, Result<Macro, String>);

/// The `[[macro]]` tables of `macros.toml`, in order; an error is the file's own (the line, and why).
pub(crate) fn read_macros(toml: &str) -> Result<Vec<Located>, (usize, String)> {
    let document = Document::parse(toml).map_err(|why| (line_of(toml, why.span().map_or(0, |span| span.start)), why.message().to_owned()))?;
    let Some(tables) = document.as_table().get("macro").and_then(Item::as_array_of_tables) else { return Ok(Vec::new()) };
    Ok(tables.iter().map(|table| (table.span().map_or(0, |span| span.start), read_macro(table))).collect())
}

/// The `[[mode]]` tables of `macros.toml`, in order, each with where its header starts; the file's own error
/// is [`read_macros`]'s to report.
pub(crate) fn read_modes(toml: &str) -> Vec<(usize, Result<OwnMode, String>)> {
    let Ok(document) = Document::parse(toml) else { return Vec::new() };
    let Some(tables) = document.as_table().get("mode").and_then(Item::as_array_of_tables) else { return Vec::new() };
    tables.iter().map(|table| (table.span().map_or(0, |span| span.start), read_mode(table))).collect()
}

fn read_mode(table: &Table) -> Result<OwnMode, String> {
    if let Some(key) = table.iter().map(|(key, _)| key).find(|key| !MODE_KEYS.contains(key)) {
        return Err(format!("unknown key {key}"));
    }
    let get = |key| table.get(key).and_then(Item::as_value);
    let name = text(get("name").ok_or("a mode needs a name")?, "name")?;
    let apps = get("apps").map(|apps| texts(apps, "apps")).transpose()?.unwrap_or_default();
    Ok(OwnMode { name, apps })
}

/// `toml` with `new` added as its last `[[macro]]`, comments and order kept.
pub(crate) fn with_macro(toml: &str, new: &Macro) -> Result<String, String> {
    let mut document = edit(toml)?;
    let mut table = write_macro(new);
    table.decor_mut().set_prefix("\n");
    document.entry("macro").or_insert(Item::ArrayOfTables(ArrayOfTables::new())).as_array_of_tables_mut().ok_or("macro isn't a list of [[macro]] tables")?.push(table);
    Ok(document.to_string())
}

/// `toml` with its `at`th `[[macro]]` written as `new`, in the same place with the comments before it.
pub(crate) fn with_macro_at(toml: &str, new: &Macro, at: usize) -> Result<String, String> {
    let mut document = edit(toml)?;
    let tables = document.get_mut("macro").and_then(Item::as_array_of_tables_mut).ok_or("there are no [[macro]] tables")?;
    let old = tables.get_mut(at).ok_or("there is no such [[macro]] table")?;
    let mut table = write_macro(new);
    table.decor_mut().set_prefix(old.decor().prefix().cloned().unwrap_or_default());
    *old = table;
    Ok(document.to_string())
}

/// `toml` without its `at`th `[[macro]]`.
pub(crate) fn without_macro(toml: &str, at: usize) -> Result<String, String> {
    let mut document = edit(toml)?;
    let tables = document.get_mut("macro").and_then(Item::as_array_of_tables_mut).ok_or("there are no [[macro]] tables")?;
    let kept = tables.get(at).map(comments_before).unwrap_or_default();
    tables.remove(at);
    match tables.get_mut(at) {
        Some(next) => {
            let prefix = format!("{kept}{}", next.decor().prefix().and_then(RawString::as_str).unwrap_or_default());
            next.decor_mut().set_prefix(prefix);
        }
        None => {
            let emptied = tables.is_empty();
            let trailing = document.trailing().as_str().unwrap_or_default().to_owned();
            if emptied {
                document.remove("macro");
                document.set_trailing(format!("{kept}{trailing}"));
            } else {
                document.set_trailing(format!("{trailing}{kept}"));
            }
        }
    }
    Ok(document.to_string())
}

/// What stood before a table's header when it holds comments (blank lines alone go with the table).
fn comments_before(table: &Table) -> String {
    let prefix = table.decor().prefix().and_then(RawString::as_str).unwrap_or_default();
    if prefix.trim().is_empty() { String::new() } else { prefix.to_owned() }
}

fn edit(toml: &str) -> Result<DocumentMut, String> {
    toml.parse().map_err(|why: TomlError| format!("macros.toml can't be read: {}", why.message()))
}

/// The line, from 1, of a byte offset.
pub(crate) fn line_of(toml: &str, offset: usize) -> usize {
    toml[..offset].matches('\n').count() + 1
}

fn read_macro(table: &Table) -> Result<Macro, String> {
    if let Some(key) = table.iter().map(|(key, _)| key).find(|key| !MACRO_KEYS.contains(key)) {
        return Err(format!("unknown key {key}"));
    }
    let get = |key| table.get(key).and_then(Item::as_value);
    let name = text(get("name").ok_or("a macro needs a name")?, "name")?;
    let say = get("say").map(|say| texts(say, "say")).transpose()?.unwrap_or_default();
    let hotkey = get("hotkey").map(|hotkey| Hotkey::parse(&text(hotkey, "hotkey")?).map_err(|why| format!("hotkey: {why}"))).transpose()?;
    let apps = get("apps").map(|apps| texts(apps, "apps")).transpose()?.unwrap_or_default();
    let modes = get("modes").map(|modes| texts(modes, "modes")).transpose()?.unwrap_or_default();
    let flag = |key| get(key).map(|value| value.as_bool().ok_or(format!("{key} must be true or false"))).transpose();
    let steps = get("steps").and_then(Value::as_array).ok_or("a macro needs steps, a list like [ { press = \"ctrl+a\" } ]")?;
    let steps = steps.iter().map(|step| read_step(step.as_inline_table().ok_or("each step is a table like { press = \"ctrl+a\" }")?)).collect::<Result<_, _>>()?;
    Ok(Macro { name, say, apps, anywhere: flag("anywhere")?.unwrap_or(false), enabled: flag("enabled")?.unwrap_or(true), hotkey, listen: flag("listen")?.unwrap_or(false), modes, steps })
}

fn read_step(table: &InlineTable) -> Result<StepSpec, String> {
    let is_option = |key: &str| STEP_OPTIONS.iter().any(|(option, _)| *option == key);
    let kinds: Vec<&str> = table.iter().map(|(key, _)| key).filter(|key| !is_option(key)).collect();
    let [kind] = kinds[..] else { return Err(format!("a step has exactly one of {}", STEP_KEYS.join(", "))) };
    let value = &table[kind];
    if !STEP_KEYS.contains(&kind) {
        return Err(format!("unknown step {kind}"));
    }
    if let Some((option, owner)) = STEP_OPTIONS.iter().find(|(option, owner)| table.contains_key(option) && kind != *owner) {
        return Err(format!("{option} is only for {owner}"));
    }
    Ok(match kind {
        "paste" => StepSpec::Paste(text(value, kind)?),
        "paste_file" => {
            let raw = table.get("raw").map(|raw| raw.as_bool().ok_or("raw must be true or false")).transpose()?.unwrap_or(false);
            StepSpec::PasteFile { path: text(value, kind)?, raw }
        }
        "attach" => StepSpec::Attach(texts(value, kind)?),
        "press" => StepSpec::Press(text(value, kind)?),
        "copy" => match value.as_bool() {
            Some(true) => StepSpec::Copy(None),
            _ => StepSpec::Copy(Some(text(value, "copy (true for the selection, or the text)")?)),
        },
        "type" => StepSpec::Type(text(value, kind)?),
        "wait" => StepSpec::Wait(value.as_integer().and_then(|ms| u64::try_from(ms).ok()).ok_or("wait is a number of milliseconds")?),
        "mode" => StepSpec::Mode(text(value, kind)?),
        "open" => StepSpec::Open(text(value, kind)?),
        "switch" => StepSpec::Switch(text(value, kind)?),
        "ask_claude" => StepSpec::AskClaude(text(value, kind)?),
        "run" => {
            let timeout = table.get("timeout_ms").map(|ms| ms.as_integer().and_then(|ms| u64::try_from(ms).ok()).ok_or("timeout_ms is a number of milliseconds")).transpose()?;
            StepSpec::Run {
                program: text(value, kind)?,
                args: table.get("args").map(|args| texts(args, "args")).transpose()?.unwrap_or_default(),
                output: table.get("output").map(|output| text(output, "output")).transpose()?.unwrap_or_else(|| DEFAULT_OUTPUT.to_owned()),
                timeout_ms: timeout.unwrap_or(DEFAULT_TIMEOUT_MS),
            }
        }
        "kiku" => StepSpec::Kiku {
            action: text(value, kind)?,
            tab: table.get("tab").map(|tab| text(tab, "tab")).transpose()?,
            search: table.get("search").map(|search| text(search, "search")).transpose()?,
        },
        _ => StepSpec::Notify(text(value, kind)?),
    })
}

fn text(value: &Value, what: &str) -> Result<String, String> {
    value.as_str().map(str::to_owned).ok_or(format!("{what} must be text"))
}

/// One text, or a list of them.
fn texts(value: &Value, what: &str) -> Result<Vec<String>, String> {
    match value.as_array() {
        Some(list) => list.iter().map(|item| text(item, what)).collect(),
        None => Ok(vec![text(value, what)?]),
    }
}

fn write_macro(macro_: &Macro) -> Table {
    let mut table = Table::new();
    table.insert("name", toml_edit::value(&macro_.name));
    if !macro_.say.is_empty() {
        table.insert("say", toml_edit::value(write_texts(&macro_.say)));
    }
    if !macro_.apps.is_empty() {
        table.insert("apps", toml_edit::value(macro_.apps.iter().collect::<Array>()));
    }
    if macro_.anywhere {
        table.insert("anywhere", toml_edit::value(true));
    }
    if !macro_.enabled {
        table.insert("enabled", toml_edit::value(false));
    }
    if !macro_.modes.is_empty() {
        table.insert("modes", toml_edit::value(macro_.modes.iter().collect::<Array>()));
    }
    if let Some(hotkey) = &macro_.hotkey {
        table.insert("hotkey", toml_edit::value(hotkey.name()));
    }
    if macro_.listen {
        table.insert("listen", toml_edit::value(true));
    }
    table.insert("steps", toml_edit::value(macro_.steps.iter().map(write_step).collect::<Array>()));
    table
}

fn write_texts(texts: &[String]) -> Value {
    match texts {
        [one] => one.into(),
        many => many.iter().collect::<Array>().into(),
    }
}

fn write_step(step: &StepSpec) -> InlineTable {
    let mut table = InlineTable::new();
    let value: Value = match step {
        StepSpec::Paste(text) | StepSpec::Press(text) | StepSpec::Type(text) | StepSpec::Notify(text) | StepSpec::Mode(text) | StepSpec::Open(text) | StepSpec::Switch(text) | StepSpec::AskClaude(text) => text.into(),
        StepSpec::PasteFile { path, .. } => path.into(),
        StepSpec::Attach(files) => write_texts(files),
        StepSpec::Copy(text) => text.as_ref().map_or(true.into(), Into::into),
        StepSpec::Wait(ms) => i64::try_from(*ms).unwrap_or(i64::MAX).into(),
        StepSpec::Run { program, .. } => program.into(),
        StepSpec::Kiku { action, .. } => action.into(),
    };
    table.insert(step.kind(), value);
    match step {
        StepSpec::PasteFile { raw: true, .. } => {
            table.insert("raw", true.into());
        }
        StepSpec::Run { args, output, timeout_ms, .. } => {
            if !args.is_empty() {
                table.insert("args", args.iter().collect::<Array>().into());
            }
            if output != DEFAULT_OUTPUT {
                table.insert("output", output.into());
            }
            if *timeout_ms != DEFAULT_TIMEOUT_MS {
                table.insert("timeout_ms", i64::try_from(*timeout_ms).unwrap_or(i64::MAX).into());
            }
        }
        StepSpec::Kiku { tab, search, .. } => {
            for (key, option) in [("tab", tab), ("search", search)] {
                if let Some(option) = option {
                    table.insert(key, option.into());
                }
            }
        }
        _ => {}
    }
    table
}

/// A text file's contents: UTF-8, or UTF-16 with a byte order mark, at most [`MAX_FILE_BYTES`]. A missing,
/// too large or binary file is an error naming the file and why ("use attach" for binary).
pub fn read_text_file(path: &Path) -> Result<Private, String> {
    let shown = path.display();
    let binary = || format!("{shown} isn't a text file: use attach to send it as a file");
    let metadata = fs::metadata(path).map_err(|why| format!("{shown} can't be read: {why}"))?;
    if !metadata.is_file() {
        return Err(format!("{shown} isn't a file"));
    }
    let too_big = || format!("{shown} is bigger than 1 MiB");
    if metadata.len() > MAX_FILE_BYTES {
        return Err(too_big());
    }
    let mut bytes = Vec::new();
    let read = fs::File::open(path).and_then(|file| file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes));
    read.map_err(|why| format!("{shown} can't be read: {why}"))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(too_big());
    }
    let text = match bytes.as_slice() {
        [0xFF, 0xFE, wide @ ..] => utf16(wide, u16::from_le_bytes),
        [0xFE, 0xFF, wide @ ..] => utf16(wide, u16::from_be_bytes),
        [0xEF, 0xBB, 0xBF, text @ ..] => String::from_utf8(text.to_vec()).ok(),
        text => String::from_utf8(text.to_vec()).ok(),
    };
    text.filter(|text| !text.contains('\0')).map(Private::new).ok_or_else(binary)
}

/// UTF-16 `bytes` (no byte order mark) as text, `None` when they aren't valid UTF-16.
fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|pair| unit(*pair)).collect();
    String::from_utf16(&units).ok()
}

impl fmt::Debug for Variables {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.names()).finish()
    }
}

impl fmt::Debug for Macro {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let steps: Vec<&str> = self.steps.iter().map(StepSpec::kind).collect();
        f.debug_struct("Macro").field("name", &self.name).field("say", &self.say).field("apps", &self.apps).field("steps", &steps).finish()
    }
}

impl fmt::Debug for StepSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind())
    }
}

impl StepSpec {
    /// Its key in the file: `paste`, `press`, ...
    pub fn kind(&self) -> &'static str {
        match self {
            StepSpec::Paste(_) => "paste",
            StepSpec::PasteFile { .. } => "paste_file",
            StepSpec::Attach(_) => "attach",
            StepSpec::Press(_) => "press",
            StepSpec::Copy(_) => "copy",
            StepSpec::Type(_) => "type",
            StepSpec::Wait(_) => "wait",
            StepSpec::Notify(_) => "notify",
            StepSpec::Open(_) => "open",
            StepSpec::Switch(_) => "switch",
            StepSpec::Run { .. } => "run",
            StepSpec::AskClaude(_) => "ask_claude",
            StepSpec::Mode(_) => "mode",
            StepSpec::Kiku { .. } => "kiku",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREE: &str = "# top\n[[macro]]\n# about a\nname = \"a\"\nsay = [\"a\"]\nsteps = []\n\n# about b\n[[macro]]\nname = \"b\"\nsay = [\"b\"]\nsteps = []\n\n[[macro]]\nname = \"c\"\nsay = [\"c\"]\nsteps = []\n# end\n";

    fn names(toml: &str) -> Vec<String> {
        read_macros(toml).unwrap().into_iter().map(|(_, macro_)| macro_.unwrap().name).collect()
    }

    #[test]
    fn a_run_step_round_trips_and_its_options_belong_to_it() {
        let step = StepSpec::Run { program: "a.bat".into(), args: vec!["{words}".into(), "-x".into()], output: "variable:out".into(), timeout_ms: 500 };
        let new = Macro { name: "r".into(), say: vec!["r".into()], apps: vec![], anywhere: false, enabled: true, hotkey: None, listen: false, modes: vec![], steps: vec![step.clone()] };
        let text = with_macro("", &new).unwrap();
        assert_eq!(read_macros(&text).unwrap().remove(0).1.unwrap().steps, [step]);
        let plain = read_macros("[[macro]]\nname = \"r\"\nsteps = [ { run = \"a.bat\" } ]\n").unwrap().remove(0).1.unwrap();
        assert_eq!(plain.steps, [StepSpec::Run { program: "a.bat".into(), args: vec![], output: "none".into(), timeout_ms: 10_000 }]);
        let misplaced = read_macros("[[macro]]\nname = \"r\"\nsteps = [ { paste = \"a\", args = [\"b\"] } ]\n").unwrap().remove(0).1;
        assert_eq!(misplaced.unwrap_err(), "args is only for run");
    }

    #[test]
    fn replacing_a_macro_keeps_its_place_and_the_comments_before_it() {
        let new = Macro { name: "b2".into(), say: vec!["b two".into()], apps: vec![], anywhere: false, enabled: false, hotkey: None, listen: false, modes: vec![], steps: vec![] };
        let text = with_macro_at(THREE, &new, 1).unwrap();
        assert!(text.contains("# about b") && text.contains("# end"), "{text}");
        assert_eq!(names(&text), ["a", "b2", "c"]);
    }

    #[test]
    fn removing_the_first_macro_keeps_the_header_comment() {
        let text = without_macro(THREE, 0).unwrap();
        assert!(text.starts_with("# top\n"), "{text}");
        assert!(text.contains("# about b") && !text.contains("# about a"));
        assert_eq!(names(&text), ["b", "c"]);
    }

    #[test]
    fn removing_a_middle_macro_keeps_the_comments_before_it() {
        let text = without_macro(THREE, 1).unwrap();
        assert!(text.contains("# about b") && text.contains("# top"));
        assert_eq!(names(&text), ["a", "c"]);
    }

    #[test]
    fn removing_the_last_macro_keeps_what_precedes_it() {
        let text = without_macro("# top\n[[macro]]\nname = \"a\"\nsay = [\"a\"]\nsteps = []\n\n# about b\n[[macro]]\nname = \"b\"\nsay = [\"b\"]\nsteps = []\n", 1).unwrap();
        assert!(text.contains("# about b") && text.starts_with("# top\n"), "{text}");
        assert_eq!(names(&text), ["a"]);
        assert_eq!(without_macro(&text, 0).unwrap().trim(), "# top\n\n# about b");
    }
}
