//! What the MCP server tells assistants about `custom\`: the macros (never a value or a step's text), every
//! problem by file and line, and what a phrase would do (private text masked).

use std::{fs, io, path::Path};

use super::{Approvals, CommandPolicy, Macro, Macros, Mode, Step, Variables};
use crate::words::{WordSettings, Words};

const PRIVATE: &str = "[private]";

/// `custom\`'s macros and variables as read, with their problems.
pub struct Customization {
    macros: Macros,
    problems: Vec<(String, String)>,
}

impl Customization {
    /// Reads `macros.toml` and `variables.toml` from `custom`, with `words`' `>` lines.
    pub fn read(custom: &Path, words: &Words) -> Customization {
        let mut problems = Vec::new();
        let mut read = |name: &str| match fs::read_to_string(custom.join(name)) {
            Ok(text) => text,
            Err(why) if why.kind() == io::ErrorKind::NotFound => String::new(),
            Err(why) => {
                problems.push((name.to_owned(), format!("can't be read: {}", why.kind())));
                String::new()
            }
        };
        let variables_text = read("variables.toml");
        let macros_text = read("macros.toml");
        let variables = Variables::parse(&variables_text).unwrap_or_else(|why| {
            problems.push(variables_problem(&why));
            Variables::default()
        });
        let mut macros = Macros::parse(&macros_text, words, &variables);
        problems.splice(0..0, macros.skipped.iter().cloned());
        // Plans keep their `run` steps so they can be named; the server never runs one.
        let mut approvals = Approvals::default();
        macros.commands().iter().for_each(|(_, command)| approvals.add(command));
        macros.set_commands(CommandPolicy { enabled: true, anywhere: true }, approvals);
        Customization { macros, problems }
    }

    /// Each macro: name, phrases, apps, modes, step kinds, shortcut, whether it listens and is on.
    pub fn list_macros(&self) -> String {
        if self.macros.entries().is_empty() {
            return "There are no macros in macros.toml.".into();
        }
        self.macros.entries().iter().map(describe).collect::<Vec<_>>().join("\n")
    }

    /// Every problem: where (`macros.toml:12`, `variables.toml:3`) and why, never a value.
    pub fn problems(&self) -> &[(String, String)] {
        &self.problems
    }

    /// What saying `phrase` in `app` would do, step by step, text shown as `[private]`.
    pub fn try_phrase(&self, phrase: &str, app: &str) -> String {
        let settings = WordSettings::default();
        let steps = self.macros.plan(&[phrase], app, &settings, &Mode::Normal);
        let names = self.macros.triggered(phrase, app, &settings);
        let steps: Vec<String> = steps.iter().enumerate().map(|(n, step)| format!("{}. {}", n + 1, describe_step(step))).collect();
        match (names.is_empty(), steps.is_empty()) {
            (_, true) => "Nothing: saying that does nothing here.".into(),
            (false, false) => format!("Runs {}:\n{}", names.join(", "), steps.join("\n")),
            (true, false) => format!("No macro runs; it would be dictated as:\n{}", steps.join("\n")),
        }
    }
}

/// Where a variables.toml error is, from its "line N: why" text.
fn variables_problem(why: &str) -> (String, String) {
    match why.strip_prefix("line ").and_then(|rest| rest.split_once(": ")) {
        Some((line, why)) => (format!("variables.toml:{line}"), why.to_owned()),
        None => ("variables.toml".to_owned(), why.to_owned()),
    }
}

fn describe(macro_: &Macro) -> String {
    let mut parts = vec![format!("- {} ({})", macro_.name, if macro_.enabled { "on" } else { "off" })];
    if !macro_.say.is_empty() {
        parts.push(format!("says: {}", macro_.say.iter().map(|say| format!("\"{say}\"")).collect::<Vec<_>>().join(", ")));
    }
    if !macro_.apps.is_empty() {
        parts.push(format!("apps: {}", macro_.apps.join(", ")));
    }
    if !macro_.modes.is_empty() {
        parts.push(format!("modes: {}", macro_.modes.join(", ")));
    }
    parts.push(format!("steps: {}", macro_.steps.iter().map(|step| step.kind()).collect::<Vec<_>>().join(", ")));
    if let Some(hotkey) = &macro_.hotkey {
        parts.push(format!("shortcut: {}", hotkey.name()));
    }
    if macro_.listen {
        parts.push("listens without the dictation key".into());
    }
    parts.join("; ")
}

fn describe_step(step: &Step) -> String {
    match step {
        Step::Paste { .. } => format!("paste {PRIVATE}"),
        Step::PasteFile { .. } => format!("paste a file {PRIVATE}"),
        Step::Attach { files, .. } => format!("attach {} file(s)", files.len()),
        Step::Press(chords) => format!("press {}", chords.iter().map(|chord| chord.iter().map(|key| key.name()).collect::<Vec<_>>().join("+")).collect::<Vec<_>>().join(", ")),
        Step::Copy { text: None, .. } => "copy the selection".into(),
        Step::Copy { .. } => format!("copy {PRIVATE}"),
        Step::Type { .. } => format!("type {PRIVATE}"),
        Step::Wait(how_long) => format!("wait {} ms", how_long.as_millis()),
        Step::Notify(_) => format!("notify {PRIVATE}"),
        Step::Open { .. } => format!("open {PRIVATE}"),
        Step::Switch { .. } => format!("switch to an app {PRIVATE}"),
        Step::Run { command, .. } => format!(
            "run {} (only when Settings → Macros allows programs and the command is approved; the server can't tell)",
            command.program.file_name().unwrap_or_default().to_string_lossy()
        ),
        Step::AskClaude { .. } => format!("ask Claude {PRIVATE}"),
        Step::SetMode(mode) => format!("switch to {} mode", mode.name()),
        Step::ShowPalette => "show the palette".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(name: &str, macros: &[u8], variables: &[u8]) -> Customization {
        let folder = std::env::temp_dir().join(format!("kiku-report-{name}-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("macros.toml"), macros).unwrap();
        fs::write(folder.join("variables.toml"), variables).unwrap();
        let found = Customization::read(&folder, &Words::default());
        fs::remove_dir_all(&folder).unwrap();
        found
    }

    #[test]
    fn a_macro_said_mid_sentence_is_named() {
        let toml = "[[macro]]\nname = \"Email\"\nsay = \"my email\"\nanywhere = true\nsteps = [ { paste = \"me@example.com\" } ]\n";
        let shown = read("anywhere", toml.as_bytes(), b"").try_phrase("send it to my email please", "Discord");
        assert!(shown.starts_with("Runs [Email]:"), "{shown}");
        assert!(!shown.contains("me@example.com"), "{shown}");
    }

    #[test]
    fn a_run_step_names_its_program_and_never_its_arguments() {
        let toml = "[[macro]]\nname = \"Tool\"\nsay = \"run tool\"\nsteps = [ { run = \"tool.bat\", args = [\"--token=abc123\"], output = \"paste\" } ]\n";
        let shown = read("run", toml.as_bytes(), b"").try_phrase("run tool", "Discord");
        assert!(shown.contains("run tool.bat") && shown.contains("approved"), "{shown}");
        assert!(!shown.contains("abc123") && !shown.contains("notify"), "{shown}");
    }

    #[test]
    fn a_file_that_cannot_be_read_is_a_problem() {
        let found = read("binary", &[0xff, 0xfe, 0x00], b"secret = \"hunter2\"\n");
        assert_eq!(found.problems(), [("macros.toml".to_owned(), "can't be read: invalid data".to_owned())]);
    }
}
