//! Voice actions in dictation (`.loop/voice-actions/SPINE.md`): a `words.txt` line starting with `>` makes a
//! phrase press keys (said as a whole line) or paste saved text (said anywhere). Tests are named by the unit
//! that makes them pass (`v1_` .. `v4_`); the gate runs a unit's and every earlier unit's.

use std::{fs, time::Instant};

use kiku_core::{
    actions::{clipboard_text, key_strokes, Action, Effect, KeyStroke, Private, Step},
    keybinds::Key,
    words::{Kind, Output, Scope, WordBook, WordSettings, Words, ASSISTANT_INSTRUCTIONS, HEADER},
};

/// Snippet text that must never leak anywhere but the paste itself.
const CANARY: &str = "s3cr3t-CANARY-77";

const FILE: &str = "\
# My actions
> select all => press ctrl+a
> new tab => press Ctrl + T   # a comment after a press line
> paste email => paste me@example.com
> paste copy pasta 1 => paste first line\\nsecond line
> paste number two => paste 555 0102
> paste secret => paste s3cr3t-CANARY-77
all => everything
select => choose
main => primary
! lowercase

[Discord]
> paste email => paste discord@example.com
";

fn settings() -> WordSettings {
    WordSettings::default()
}

fn plan(words: &Words, said: &[&str], app: &str) -> Vec<Step> {
    let said: Vec<String> = said.iter().map(|line| line.to_string()).collect();
    words.plan(&said, app, &settings())
}

fn paste(text: &str) -> Step {
    Step::Paste(Private::new(text.to_owned()))
}

fn press(keys: &[Key]) -> Step {
    Step::Press(keys.to_vec())
}

fn letter(c: char) -> Key {
    Key::Other(c as u16)
}

fn stroke(vk: u16, up: bool, extended: bool) -> KeyStroke {
    KeyStroke { vk, up, extended }
}

fn actions(words: &Words) -> Vec<(Option<&str>, &Action)> {
    words
        .entries()
        .iter()
        .filter_map(|entry| match &entry.kind {
            Kind::Action(action) => Some((entry.section.as_deref(), action)),
            _ => None,
        })
        .collect()
}

// ---- V1: parsing ----

#[test]
fn v1_press_and_paste_lines_become_actions_in_their_sections() {
    let words = Words::parse(FILE);
    assert!(words.skipped.is_empty(), "{:?}", words.skipped);
    let found: Vec<(Option<&str>, &str)> = actions(&words).iter().map(|(section, action)| (*section, action.phrase.as_str())).collect();
    assert_eq!(
        found,
        vec![
            (None, "select all"),
            (None, "new tab"),
            (None, "paste email"),
            (None, "paste copy pasta 1"),
            (None, "paste number two"),
            (None, "paste secret"),
            (Some("Discord"), "paste email"),
        ]
    );
    let effects: Vec<&Effect> = actions(&words).iter().map(|(_, action)| &action.effect).collect();
    assert_eq!(effects[0], &Effect::Press(vec![Key::Ctrl, letter('A')]));
    assert_eq!(effects[1], &Effect::Press(vec![Key::Ctrl, letter('T')]), "any case, spaces around +, a comment after it");
    assert_eq!(effects[2], &Effect::Paste(Private::new("me@example.com".into())));
    assert_eq!(effects[3], &Effect::Paste(Private::new("first line\nsecond line".into())));
}

#[test]
fn v1_paste_text_is_kept_as_written_with_its_escapes() {
    let words = Words::parse("> sig => paste a # not a comment => still text\\tTab\\\\n\n");
    assert!(words.skipped.is_empty(), "{:?}", words.skipped);
    let (_, action) = actions(&words)[0];
    assert_eq!(action.effect, Effect::Paste(Private::new("a # not a comment => still text\tTab\\n".into())));
}

#[test]
fn v1_key_names_cover_editing_and_navigation_keys() {
    let line = "> keys => press enter+tab+esc+space+backspace+delete+insert+home+end+page up+page down+up+down+left+right+f5+key 0x2c\n";
    let words = Words::parse(line);
    assert!(words.skipped.is_empty(), "{:?}", words.skipped);
    let vks = [0x0D, 0x09, 0x1B, 0x20, 0x08, 0x2E, 0x2D, 0x24, 0x23, 0x21, 0x22, 0x26, 0x28, 0x25, 0x27, 0x74, 0x2C];
    assert_eq!(actions(&words)[0].1.effect, Effect::Press(vks.iter().map(|&vk| Key::Other(vk)).collect()));
    let words = Words::parse("> a => press Escape+PageUp+PgDn+Del\n");
    assert_eq!(actions(&words)[0].1.effect, Effect::Press([0x1B, 0x21, 0x22, 0x2E].iter().map(|&vk| Key::Other(vk)).collect()));
}

#[test]
fn v1_bad_action_lines_are_skipped_saying_why() {
    let words = Words::parse(
        "> broken => press ctrl+nonsense\n> lock => press win+l\n> lock two => press L+WIN\n> nothing here\n> => press ctrl+a\n> ... => press ctrl+a\n> what => shout hello\n> empty => paste\n> none => press\n> fine => press ctrl+shift+f5\n> FINE! => press ctrl+b\n",
    );
    let lines: Vec<usize> = words.skipped.iter().map(|(line, _)| *line).collect();
    assert_eq!(lines, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 11], "{:?}", words.skipped);
    for at in [1, 2] {
        let why = words.skipped[at].1.to_lowercase();
        assert!(why.contains("win+l") || why.contains("lock"), "{why}");
    }
    assert!(words.skipped[9].1.contains("10"), "a repeated phrase names the line it repeats: {}", words.skipped[9].1);
    assert_eq!(actions(&words).len(), 1);
    assert!(words.entries().iter().all(|entry| matches!(entry.kind, Kind::Action(_))), "a > line is never read as a rule");
}

#[test]
fn v1_snippet_text_never_leaks_into_reasons_or_debug_output() {
    let lines = [
        "> leak one => s3cr3t-CANARY-77",
        "> leak two => s3cr3t-CANARY-77 and more",
        "> => paste s3cr3t-CANARY-77",
        "> paste secret => paste s3cr3t-CANARY-77",
        "> paste secret => paste s3cr3t-CANARY-77",
        "> s3cr3t-CANARY-77",
    ];
    let words = Words::parse(&lines.join("\n"));
    assert_eq!(words.skipped.len(), 5, "{}", words.skipped.len());
    for (_, why) in &words.skipped {
        assert!(!why.contains(CANARY), "a reason quotes snippet text");
    }
    assert!(!format!("{words:?}").contains(CANARY), "Words' Debug shows snippet text");
    for entry in words.entries() {
        assert!(!format!("{entry:?}").contains(CANARY), "Entry's Debug shows snippet text");
    }
    let private = Private::new(CANARY.into());
    assert!(!format!("{private:?}").contains(CANARY));
    assert!(!format!("{:?}", Step::Paste(private.clone())).contains(CANARY));
    assert!(!format!("{:?}", Effect::Paste(private.clone())).contains(CANARY));
    assert_eq!(private.reveal(), CANARY);
}

#[test]
fn v1_files_without_actions_read_as_before() {
    let file = "kicku => Kiku\n@ Lull :2.5\n! lowercase\n[Discord]\ngg => good game\n";
    let words = Words::parse(file);
    assert!(words.skipped.is_empty());
    assert_eq!(words.entries().len(), 4);
    assert!(actions(&words).is_empty());
    assert_eq!(words.apply("kicku gg", "Discord", Output::Captions, &settings()), "Kiku good game");
}

#[test]
fn v1_key_strokes_press_in_order_and_release_in_reverse() {
    let strokes = key_strokes(&[Key::Ctrl, Key::Shift, letter('T')]);
    assert_eq!(
        strokes,
        vec![
            stroke(0xA2, false, false),
            stroke(0xA0, false, false),
            stroke(0x54, false, false),
            stroke(0x54, true, false),
            stroke(0xA0, true, false),
            stroke(0xA2, true, false),
        ]
    );
    let strokes = key_strokes(&[Key::Alt, Key::Win, Key::Other(0x25)]);
    assert_eq!(strokes[..3], [stroke(0xA4, false, false), stroke(0x5B, false, false), stroke(0x25, false, true)], "arrows are extended keys");
    for vk in [0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x2D, 0x2E] {
        assert!(key_strokes(&[Key::Other(vk)]).iter().all(|s| s.extended), "{vk:#x} is an extended key");
    }
    assert!(key_strokes(&[Key::Other(0x0D)]).iter().all(|s| !s.extended), "Enter on the main keyboard is not");
}

#[test]
fn v1_clipboard_text_uses_windows_line_breaks_once() {
    assert_eq!(clipboard_text("a\nb\r\nc"), "a\r\nb\r\nc");
    assert_eq!(clipboard_text("plain"), "plain");
}

// ---- V2: matching and planning dictation ----

#[test]
fn v2_a_command_said_alone_presses_its_keys() {
    let words = Words::parse(FILE);
    for said in ["Select all.", "select all", "  SELECT ALL!  ", "Select, all?"] {
        assert_eq!(plan(&words, &[said], "Notepad"), vec![press(&[Key::Ctrl, letter('A')])], "{said:?}");
    }
    assert_eq!(plan(&words, &["New tab."], "Notepad"), vec![press(&[Key::Ctrl, letter('T')])]);
}

#[test]
fn v2_a_command_inside_a_sentence_is_just_words() {
    let words = Words::parse("> select all => press ctrl+a\n");
    for said in ["Please select all of them.", "Select all the things.", "Select all, select all."] {
        assert_eq!(plan(&words, &[said], "Notepad"), vec![paste(&format!("{said} "))], "{said:?}");
    }
}

#[test]
fn v2_a_snippet_said_inside_a_sentence_is_pasted_there() {
    let words = Words::parse(FILE);
    assert_eq!(plan(&words, &["My email is paste email."], "Notepad"), vec![paste("my email is me@example.com. ")], "lowercase transform on the words, not the snippet");
    let words = Words::parse("> paste email => paste Me@Example.com\n> paste phone => paste 555 0102\n> paste => paste NOPE\n");
    assert_eq!(
        plan(&words, &["Email paste email, phone paste phone."], "Notepad"),
        vec![paste("Email Me@Example.com, phone 555 0102. ")],
        "two snippets in a line, the longest phrase first"
    );
    assert_eq!(plan(&words, &["Paste emails."], "Notepad"), vec![paste("NOPE emails. ")], "whole words only");
}

#[test]
fn v2_a_snippet_said_alone_is_pasted_as_written() {
    let words = Words::parse(FILE);
    assert_eq!(plan(&words, &["Paste email."], "Notepad"), vec![paste("me@example.com")]);
    assert_eq!(plan(&words, &["Paste copy pasta one."], "Notepad"), vec![paste("first line\nsecond line")]);
}

#[test]
fn v2_numbers_match_said_as_words_or_digits() {
    let words = Words::parse(FILE);
    for said in ["Paste copy pasta one.", "paste copy pasta 1"] {
        assert_eq!(plan(&words, &[said], "Notepad"), vec![paste("first line\nsecond line")], "{said:?}");
    }
    for said in ["Paste number 2.", "Paste number two"] {
        assert_eq!(plan(&words, &[said], "Notepad"), vec![paste("555 0102")], "{said:?}");
    }
}

#[test]
fn v2_an_apps_own_action_comes_first() {
    let words = Words::parse(FILE);
    assert_eq!(plan(&words, &["Paste email."], "Discord"), vec![paste("discord@example.com")]);
    assert_eq!(plan(&words, &["Paste email."], "discord"), vec![paste("discord@example.com")], "app names in any case");
    assert_eq!(plan(&words, &["Paste email."], "Notepad"), vec![paste("me@example.com")]);
    assert_eq!(plan(&words, &["Select all."], "Discord"), vec![press(&[Key::Ctrl, letter('A')])], "everywhere's actions work in an app too");
}

#[test]
fn v2_word_rules_and_transforms_never_change_a_phrase_or_a_snippet() {
    // `all => everything`, `select => choose`, `main => primary` and `! lowercase` are in FILE.
    let words = Words::parse(&FILE.replace("me@example.com", "Main St, MAIN"));
    assert_eq!(plan(&words, &["Select all."], "Notepad"), vec![press(&[Key::Ctrl, letter('A')])]);
    assert_eq!(plan(&words, &["Main is paste email."], "Notepad"), vec![paste("primary is Main St, MAIN. ")]);
    assert_eq!(plan(&words, &["Choose everything."], "Notepad"), vec![paste("choose everything. ")], "what the rules make is not a phrase");
}

#[test]
fn v2_snippets_never_reach_captions_or_the_try_box() {
    let words = Words::parse(FILE);
    let said = "My secret is paste secret.";
    for output in [Output::Captions, Output::Dictation] {
        assert!(!words.apply(said, "Notepad", output, &settings()).contains(CANARY), "{output:?}");
    }
    assert!(!words.apply_to_caption(said, "Notepad", &settings()).contains(CANARY));
    let shown = words.preview(said, &settings());
    assert!(shown.iter().all(|(_, text)| !text.contains(CANARY)), "{shown:?}");
    assert!(shown.iter().any(|(_, text)| text.contains("[paste secret]")), "the Try box names the snippet: {shown:?}");
    let shown = words.preview("Select all.", &settings());
    assert!(shown.iter().any(|(_, text)| text.contains("Ctrl+A")), "the Try box says what a command presses: {shown:?}");
}

#[test]
fn v2_a_batch_runs_in_order_with_text_merged() {
    let words = Words::parse(FILE);
    assert_eq!(
        plan(&words, &["Hello.", "There.", "Select all.", "Bye.", "Paste email."], "Notepad"),
        vec![paste("hello. there. "), press(&[Key::Ctrl, letter('A')]), paste("bye. me@example.com")]
    );
    assert_eq!(plan(&words, &["Select all.", "Select all."], "Notepad"), vec![press(&[Key::Ctrl, letter('A')]); 2]);
}

#[test]
fn v2_nothing_said_does_nothing() {
    let words = Words::parse(FILE);
    for said in [&[][..], &[""], &["", "   "]] {
        assert_eq!(plan(&words, said, "Notepad"), vec![], "{said:?}");
    }
}

#[test]
fn v2_without_actions_dictation_is_typed_as_before() {
    let file = "um =>\nkicku => Kiku\n! capitalise\n[Notepad]\ngg => good game\n";
    let words = Words::parse(file);
    let said = ["Um.", "kicku is here", "", "gg, um, gg.", "Um"];
    let before: String = said
        .iter()
        .map(|line| words.apply(line, "Notepad", Output::Dictation, &settings()))
        .filter(|line| !line.is_empty())
        .map(|line| format!("{line} "))
        .collect();
    assert_eq!(plan(&words, &said, "Notepad"), vec![paste(&before)]);
}

#[test]
fn v2_private_use_characters_said_are_not_snippets() {
    let words = Words::parse(FILE);
    let said = "a \u{E000} b \u{E001}.";
    let steps = plan(&words, &[said], "Notepad");
    assert!(steps.iter().all(|step| !matches!(step, Step::Paste(text) if text.reveal().contains(CANARY) || text.reveal().contains("me@example.com"))), "{steps:?}");
}

#[test]
fn v2_undo_takes_back_a_paste_but_not_a_key_press() {
    assert_eq!(paste("é\n✓ ok").erase(), Some(6));
    assert_eq!(press(&[Key::Ctrl, letter('A')]).erase(), None);
}

#[test]
fn v2_matching_is_a_lookup_not_a_scan() {
    let file: String = (0..20_000).map(|n| format!("> phrase number {n} alpha => paste text {n}\n")).collect();
    let words = Words::parse(&file);
    assert!(words.skipped.is_empty());
    let said: Vec<String> = (0..200).map(|n| format!("Here is phrase number {} alpha and some more words to read through.", n * 97)).collect();
    let started = Instant::now();
    let steps = words.plan(&said, "Notepad", &settings());
    assert!(started.elapsed().as_secs_f32() < 1.0, "planning took {:?}", started.elapsed());
    assert_eq!(steps.len(), 1);
}

// ---- V3: editing the file ----

#[test]
fn v3_actions_can_be_added_and_removed_keeping_the_file() {
    let mut words = Words::parse("# mine\nteh => the\n\n[Discord]\ngg => good game\n");
    words.add_action(&Action::press("open tab", "ctrl+t").unwrap(), None).unwrap();
    let tricky = "1 Main St\n\tFlat #2 => [B]\\C";
    words.add_action(&Action::paste("paste address", tricky).unwrap(), Some("Discord")).unwrap();
    words.add_action(&Action::paste("paste name", "Ada").unwrap(), Some("Slack")).unwrap();
    let text = words.to_text();
    assert!(text.starts_with("# mine\nteh => the\n> open tab => press Ctrl+T\n"), "{text}");
    assert!(text.contains("[Slack]\n> paste name => paste Ada\n"), "{text}");
    let again = Words::parse(&text);
    assert!(again.skipped.is_empty(), "{:?}", again.skipped);
    assert_eq!(plan(&again, &["Open tab."], "Notepad"), vec![press(&[Key::Ctrl, letter('T')])]);
    assert_eq!(plan(&again, &["Paste address."], "Discord"), vec![paste(tricky)]);
    assert_eq!(plan(&again, &["gg"], "Discord"), vec![paste("good game ")], "the section's rule is still there");

    let mut again = again;
    let entry = again.entries().iter().find(|e| matches!(&e.kind, Kind::Action(a) if a.phrase == "open tab")).unwrap().clone();
    assert!(again.remove(&entry));
    assert!(!again.remove(&entry));
    assert!(again.to_text().starts_with("# mine\nteh => the\n\n[Discord]\n"), "{}", again.to_text());
    assert_eq!(plan(&again, &["Open tab."], "Notepad"), vec![paste("Open tab. ")]);
}

#[test]
fn v3_bad_actions_are_refused_without_quoting_snippets() {
    let mut words = Words::parse("> paste email => paste me@example.com\n");
    assert!(Action::press("bad", "ctrl+nonsense").is_err());
    assert!(Action::press("lock", "win+l").is_err());
    assert!(Action::press("", "ctrl+a").is_err());
    assert!(Action::paste("...", CANARY).is_err_and(|why| !why.contains(CANARY)));
    assert!(Action::paste("empty", "").is_err());
    let repeated = words.add_action(&Action::paste("Paste Email", CANARY).unwrap(), None);
    assert!(repeated.is_err_and(|why| !why.contains(CANARY)), "a phrase already there is refused");
    assert_eq!(words.to_text(), "> paste email => paste me@example.com\n");
}

#[test]
fn v3_an_action_line_written_reads_back_the_same() {
    for text in ["plain", "two\nlines", " spaced ", "back\\slash \\n literal", "tab\there", "# hash => arrow", "[Section]", "@ at", "! bang", "> gt"] {
        let action = Action::paste("say it", text).unwrap();
        let words = Words::parse(&action.to_line());
        assert!(words.skipped.is_empty(), "{text:?}: {:?}", words.skipped);
        assert_eq!(actions(&words)[0].1, &action, "{text:?}");
    }
    let action = Action::press("go", "ctrl + shift + page down").unwrap();
    assert_eq!(action.to_line(), "> go => press Ctrl+Shift+Page Down");
}

#[test]
fn v3_an_action_describes_itself_without_its_text() {
    assert_eq!(Action::press("go", "ctrl+shift+t").unwrap().describe(), "presses Ctrl+Shift+T");
    let described = Action::paste("paste secret", &format!("{CANARY}\nline two")).unwrap().describe();
    assert!(!described.contains(CANARY), "{described}");
    assert!(described.contains("25 characters") && described.contains("2 lines"), "{described}");
}

#[test]
fn v3_the_word_book_plans_with_the_file_as_saved() {
    let folder = std::env::temp_dir().join(format!("kiku-voice-actions-{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    let book = WordBook::at(folder.join("words.txt"));
    book.add_action(&Action::press("select all", "ctrl+a").unwrap(), None).unwrap();
    assert_eq!(book.plan(&["Select all.".into()], "Notepad"), vec![press(&[Key::Ctrl, letter('A')])]);
    book.set_settings(WordSettings { transforms: Scope::Captions, ..WordSettings::default() });
    book.add_action(&Action::paste("paste email", "me@example.com").unwrap(), Some("Notepad")).unwrap();
    assert_eq!(book.plan(&["It's paste email.".into()], "Notepad"), vec![paste("It's me@example.com. ")]);
    let saved = fs::read_to_string(folder.join("words.txt")).unwrap();
    assert!(saved.starts_with(HEADER), "a new file starts with the header");
    let _ = fs::remove_dir_all(&folder);
}

// ---- V4: what people and assistants read ----

#[test]
fn v4_the_header_and_instructions_explain_actions() {
    let skill = include_str!("../../../skills/kiku/SKILL.md");
    for (name, text) in [("HEADER", HEADER), ("ASSISTANT_INSTRUCTIONS", ASSISTANT_INSTRUCTIONS), ("SKILL.md", skill)] {
        let line = |verb: &str| text.lines().any(|line| line.trim_start_matches(['#', ' ']).starts_with('>') && line.contains(&format!("=> {verb} ")));
        assert!(line("press"), "{name}: an example press line");
        assert!(line("paste"), "{name}: an example paste line");
    }
    assert!(ASSISTANT_INSTRUCTIONS.contains("\\n"), "how to write a line break in pasted text");
}
