//! Transcripts as text: `shared.txt` (what was last sent to Claude, for the MCP server), the transcript
//! saved with a miss, and history lines formatted for reading. Lines themselves live in [`crate::history`].

use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use chrono::{DateTime, Days, Local};

use crate::{captions::Section, engine::Result, history::StoredLine, paths};

/// Writes the given sections to a new file in `directory`, named after the time and `name`; returns its path.
pub(crate) fn save_in(directory: &Path, sections: &[Section], name: &str) -> Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let now = local_time(SystemTime::now());
    let file_name = format!("{} {}.txt", file_stamp(&now), sanitize(name));
    let path = directory.join(file_name);
    let heading = format!("Kiku transcript, saved {}", readable_stamp(&now));
    fs::write(&path, format_transcript(&heading, sections))?;
    Ok(path)
}

/// Replaces `shared.txt` with text the user sent to Claude.
pub fn write_shared(text: &str) -> Result<()> {
    let heading = format!("Shared from Kiku at {}", readable_stamp(&local_time(SystemTime::now())));
    Ok(replace(&shared_path(), &format!("{heading}\n\n{text}\n"))?)
}

pub fn shared_path() -> PathBuf {
    directory().with_file_name("shared.txt")
}

/// Replaces a file's text in one step: written whole to a temporary file, flushed to disk, then
/// renamed over it, so readers never see half a file and a crash or power cut leaves the old one.
pub fn replace(path: &Path, text: &str) -> io::Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, path)
}

/// Sections as a transcript body: "== App ==" headings and "[HH:MM:SS] text" lines.
pub fn transcript_text(sections: &[Section]) -> String {
    format_transcript("", sections).trim_start().to_owned()
}

/// Lines from the history database as text: "[HH:MM:SS] Discord: ..." and "[HH:MM:SS] You (to Discord): ...",
/// with the date wherever it changes.
pub fn format_history(lines: &[StoredLine]) -> String {
    let mut text = String::new();
    let mut day = None;
    for line in lines {
        let local = local_time(line.at);
        if day != Some(local.date_naive()) {
            day = Some(local.date_naive());
            text.push_str(&format!("{}== {} ==\n", if text.is_empty() { "" } else { "\n" }, local.format("%b %-d, %Y")));
        }
        let speaker = match &line.talking_to {
            Some(app) if line.you => format!("{} (to {app})", line.speaker),
            _ => line.speaker.clone(),
        };
        text.push_str(&format!("[{}] {speaker}: {}\n", clock(&local), line.text));
    }
    text
}

/// E.g. "Oct 30, 14:05" in local time.
pub fn friendly_time(time: SystemTime) -> String {
    local_time(time).format("%b %-d, %H:%M").to_string()
}

/// Midnight in local time `days_ago` days before today; 0 is the start of today.
pub fn start_of_day(days_ago: u32) -> SystemTime {
    let day = Local::now().date_naive() - Days::new(days_ago.into());
    // In the rare place where a clock change skips midnight, it is now instead.
    day.and_hms_opt(0, 0, 0).and_then(|midnight| midnight.and_local_timezone(Local).earliest()).map_or_else(SystemTime::now, Into::into)
}

/// E.g. "2026-10-30", the local date as SQLite writes it.
pub fn local_date(time: SystemTime) -> String {
    local_time(time).format("%Y-%m-%d").to_string()
}

/// The app's transcripts folder; `shared.txt` and the misses folder sit beside it.
pub fn directory() -> PathBuf {
    paths::data_dir().join("transcripts")
}

fn format_transcript(heading: &str, sections: &[Section]) -> String {
    let mut text = format!("{heading}\n");
    for section in sections {
        text.push_str(&format!("\n== {} ==\n", section.label));
        for caption in section.captions.iter().filter(|c| c.complete) {
            let time = clock(&local_time(caption.time));
            match caption.speaker {
                Some(speaker) => text.push_str(&format!("[{time}] {speaker}: {}\n", caption.text)),
                None => text.push_str(&format!("[{time}] {}\n", caption.text)),
            }
        }
    }
    text
}

/// Windows forbids these in file names.
pub(crate) fn sanitize(name: &str) -> String {
    name.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) { '_' } else { c }).collect()
}

fn file_stamp(t: &DateTime<Local>) -> String {
    t.format("%Y-%m-%d %H-%M-%S").to_string()
}

fn readable_stamp(t: &DateTime<Local>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// The local time as used in file names, e.g. "2026-10-30 18-40-12".
pub fn stamp_now() -> String {
    file_stamp(&local_time(SystemTime::now()))
}

/// "HH:MM:SS" in local time.
pub fn local_clock(time: SystemTime) -> String {
    clock(&local_time(time))
}

fn clock(t: &DateTime<Local>) -> String {
    t.format("%H:%M:%S").to_string()
}

fn local_time(time: SystemTime) -> DateTime<Local> {
    time.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captions::Captions;
    use crate::engine::Line;
    use std::time::Instant;

    #[test]
    fn transcript_lists_complete_lines_under_each_app() {
        let mut captions = Captions::default();
        let app = captions.add_app("Discord", None);
        let line = |id, text: &str, complete| Line { id, text: text.into(), complete };
        captions.update_app(app, 1, &[line(1, "Are you joining?", true), line(2, "still talk", false)], Instant::now());
        captions.update_mic(2, &[line(3, "Two minutes.", true)], Some("Discord"));

        let text = format_transcript("heading", &captions.sections[app..=app]);
        let body: Vec<&str> = text.lines().skip(2).map(|l| l.split_once("] ").map_or(l, |(_, rest)| rest)).collect();
        assert_eq!(body, ["== Discord ==", "Are you joining?", "You: Two minutes."]);
    }

    #[test]
    fn written_files_are_named_after_the_app_and_hold_the_lines() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-transcripts");
        let _ = fs::remove_dir_all(&directory);
        let mut captions = Captions::default();
        let app = captions.add_app("Discord", None);
        captions.update_app(app, 1, &[Line { id: 1, text: "Hello there.".into(), complete: true }], Instant::now());

        let path = save_in(&directory, &captions.sections, "Discord").unwrap();
        assert!(path.file_stem().unwrap().to_string_lossy().ends_with(" Discord"));
        assert!(fs::read_to_string(&path).unwrap().contains("Hello there."));
    }

    #[test]
    fn replacing_a_file_leaves_only_the_new_text() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-replace.txt");
        fs::write(&path, "old text that is longer").unwrap();
        replace(&path, "new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn days_start_at_local_midnight() {
        let today = start_of_day(0);
        assert!(today <= SystemTime::now() && local_clock(today) == "00:00:00");
        assert!(local_date(start_of_day(1)) < local_date(today));
    }

    #[test]
    fn file_names_avoid_forbidden_characters() {
        assert_eq!(sanitize("a/b:c"), "a_b_c");
    }
}
