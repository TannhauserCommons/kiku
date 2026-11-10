//! Checking a macro when its file is read, and turning it into the steps and phrases plans are made from.

use std::{path::PathBuf, time::Duration};

use super::{
    commands::Command,
    file::{Macro, StepSpec, Variables, MAX_ATTACHED},
    modes::{Mode, Modes},
    open::check_open,
    plan::{Beat, Prepared, Step, Times, MAX_REPEAT},
    template::{shown, Template},
    RunOutput,
};
use crate::actions::{self, matching::Part, Action, Effect};

/// One phrase that runs a macro: its words and slots, and the slots' names in phrase order.
pub(super) struct Phrase {
    pub parts: Vec<Part>,
    pub slots: Vec<String>,
}

/// A macro ready to index.
pub(super) struct Compiled {
    pub steps: Vec<Prepared>,
    /// What the Try boxes show for it: its name in brackets, or as a `>` line is shown.
    pub label: String,
    pub phrases: Vec<Phrase>,
    /// May be said inside a sentence, not only as the whole line.
    pub anywhere: bool,
}

/// `macro_` as plans use it, or why it can't run.
pub(super) fn compile(macro_: &Macro, variables: &Variables, modes: &Modes) -> Result<Compiled, String> {
    if macro_.name.trim().is_empty() {
        return Err("a macro needs a name".into());
    }
    if macro_.steps.is_empty() {
        return Err("a macro needs at least one step".into());
    }
    for name in &macro_.modes {
        if modes.lookup(name)? == Mode::Off {
            return Err("macros don't run while dictation is off: take off out of modes".into());
        }
    }
    let phrases = macro_.say.iter().map(|say| phrase(say)).collect::<Result<Vec<_>, _>>()?;
    if phrases.is_empty() && macro_.hotkey.is_none() {
        return Err("a macro needs something to say, or a shortcut".into());
    }
    if phrases.is_empty() && macro_.listen {
        return Err("listen needs something to say".into());
    }
    if macro_.anywhere {
        let text_only = macro_.steps.iter().all(|step| matches!(step, StepSpec::Paste(_) | StepSpec::PasteFile { .. }));
        if !text_only || phrases.iter().any(|phrase| !phrase.slots.is_empty()) {
            return Err("anywhere is only for macros that paste text and have no slots in what they say".into());
        }
    }
    // A shortcut says nothing, so it has no words to fill a slot from.
    let slot = |name: &str| macro_.hotkey.is_none() && phrases.iter().all(|phrase| phrase.slots.iter().any(|slot| slot == name));
    let known = |name: &str| name == "heard" || variables.get(name).is_some() || slot(name);
    let mut steps = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    for spec in &macro_.steps {
        let known = |name: &str| known(name) || kept.iter().any(|made| made == name);
        let step = step_of(spec, &macro_.name, &known, &slot, modes)?;
        if let Prepared::Ready(Step::Run { command: Command { output: RunOutput::Variable(name), .. }, .. }) = &step {
            kept.push(name.clone());
        }
        steps.push(step);
    }
    Ok(Compiled { steps, label: format!("[{}]", macro_.name), phrases, anywhere: macro_.anywhere })
}

/// A `>` line of `words.txt` as a one-step macro: a press runs for the whole line, a paste pastes its text as
/// written (no variables) and can be said anywhere.
pub(super) fn from_action(action: &Action) -> Compiled {
    let step = match &action.effect {
        Effect::Press(keys) => Step::Press(vec![keys.clone()]),
        Effect::Paste(text) => Step::Paste { text: Template::literal(text.reveal()), from: Some(action.phrase.clone()) },
    };
    let step = Prepared::Ready(step);
    let parts = actions::tokens(&action.phrase).into_iter().map(Part::Word).collect();
    Compiled { steps: vec![step], label: action.shown(), phrases: vec![Phrase { parts, slots: Vec::new() }], anywhere: matches!(action.effect, Effect::Paste(_)) }
}

/// What someone says to run a macro: words, `{name}` for the rest of the line, `{name:number}` for a number.
fn phrase(say: &str) -> Result<Phrase, String> {
    let (mut parts, mut slots) = (Vec::new(), Vec::<String>::new());
    for word in say.split_whitespace() {
        if matches!(parts.last(), Some(Part::Rest)) {
            return Err("{name} takes the rest of the line, so nothing can come after it".into());
        }
        let Some(slot) = word.strip_prefix('{').and_then(|word| word.strip_suffix('}')) else {
            parts.extend(actions::tokens(word).into_iter().map(Part::Word));
            continue;
        };
        let (name, kind) = slot.split_once(':').unwrap_or((slot, ""));
        if name.is_empty() || slots.iter().any(|known| known == name) {
            return Err("each slot in what a macro says needs a name of its own".into());
        }
        parts.push(match kind {
            "" => Part::Rest,
            "number" => Part::Number,
            _ => return Err(format!("slot {name} is {{{name}}} or {{{name}:number}}")),
        });
        slots.push(name.to_owned());
    }
    if !parts.iter().any(|part| matches!(part, Part::Word(_))) {
        return Err("what a macro says needs at least one word besides its slots".into());
    }
    Ok(Phrase { parts, slots })
}

fn step_of(spec: &StepSpec, name: &str, known: &impl Fn(&str) -> bool, slot: &impl Fn(&str) -> bool, modes: &Modes) -> Result<Prepared, String> {
    let from = name.to_owned();
    if let StepSpec::Press(keys) = spec {
        return Ok(Prepared::Press(beats(keys, slot)?));
    }
    Ok(Prepared::Ready(match spec {
        StepSpec::Paste(text) => Step::Paste { text: template(text, known)?, from: Some(from) },
        StepSpec::PasteFile { path, raw } => Step::PasteFile { path: PathBuf::from(path), raw: *raw, from },
        StepSpec::Attach(files) if files.is_empty() => return Err("attach needs a file".into()),
        StepSpec::Attach(files) if files.len() > MAX_ATTACHED => return Err(format!("attach takes at most {MAX_ATTACHED} files")),
        StepSpec::Attach(files) => Step::Attach { files: files.iter().map(PathBuf::from).collect(), from },
        StepSpec::Press(_) => unreachable!("press steps return above"),
        StepSpec::Copy(text) => Step::Copy { text: text.as_deref().map(|text| template(text, known)).transpose()?, from },
        StepSpec::Type(text) => Step::Type { text: template(text, known)?, from },
        StepSpec::Wait(ms) => Step::Wait(Duration::from_millis(*ms)),
        StepSpec::Notify(message) => Step::Notify(message.clone()),
        StepSpec::Mode(name) => Step::SetMode(modes.lookup(name)?),
        StepSpec::Open(target) => {
            let target = template(target, known)?;
            target.as_literal().map_or(Ok(()), check_open)?;
            Step::Open { target, from }
        }
        StepSpec::Switch(app) => Step::Switch { app: template(app, known)?, from },
        StepSpec::Run { program, args, output, timeout_ms } => {
            let templates = args.iter().map(|arg| template(arg, known)).collect::<Result<_, _>>()?;
            let command = Command::new(program, args, templates, output, *timeout_ms)?;
            if let RunOutput::Variable(name) = &command.output {
                check_output_name(name, known)?;
            }
            Step::Run { command, from, contents: None }
        }
        StepSpec::AskClaude(text) => Step::AskClaude { text: template(text, known)?, from },
    }))
}

/// Whether `name` can hold a program's output: a plain name nothing else uses.
fn check_output_name(name: &str, known: &impl Fn(&str) -> bool) -> Result<(), String> {
    let plain = Template::parse(&format!("{{{name}}}")).is_ok_and(|template| template.names() == [name]);
    if !plain || known(name) {
        return Err(format!("{} can't hold output: it is built in, a variable, a slot or not a plain name", shown(name, "(an unusual name)")));
    }
    Ok(())
}

/// A press step's chords, comma separated, each optionally followed by `×3` or `×{slot}`.
fn beats(text: &str, slot: &impl Fn(&str) -> bool) -> Result<Vec<Beat>, String> {
    text.split(',')
        .map(|beat| {
            let (keys, times) = match beat.split_once('×') {
                Some((keys, times)) => (keys, Some(times.trim())),
                None => (beat, None),
            };
            let times = match times {
                None => Times::Fixed(1),
                Some(times) => match times.strip_prefix('{').and_then(|times| times.strip_suffix('}')) {
                    Some(name) if slot(name) => Times::Slot(name.to_owned()),
                    Some(name) => return Err(format!("no slot called {} to repeat by", shown(name, "(an unusual name)"))),
                    None => match times.parse::<usize>() {
                        Ok(count) if (1..=MAX_REPEAT).contains(&count) => Times::Fixed(count),
                        _ => return Err(format!("a repeat is × and a number from 1 to {MAX_REPEAT}, or a slot in braces")),
                    },
                },
            };
            Ok(Beat { chord: actions::parse_keys(keys)?, times })
        })
        .collect()
}

/// A template whose names are all `known`.
fn template(text: &str, known: &impl Fn(&str) -> bool) -> Result<Template, String> {
    let template = Template::parse(text)?;
    match template.names().iter().find(|name| !known(name)) {
        Some(name) => Err(format!("no variable or slot called {}", shown(name, "(an unusual name)"))),
        None => Ok(template),
    }
}
