//! The transcript: one section per app holding everything it said this session.
//!
//! Every mic line goes into the permanent "You" section. It is also shown as "You:" inside the app
//! that spoke most recently, since that is usually who you are answering.

use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

use crate::engine::Line;

/// Oldest lines are dropped past this, to bound memory on very long sessions.
const MAX_LINES_PER_SECTION: usize = 10_000;
/// How long an app counts as the one you are talking to after it last spoke.
const CONVERSATION_WINDOW: Duration = Duration::from_secs(30);
pub const YOU: &str = "You";
/// The You section is created first and never removed.
pub const YOU_SECTION: usize = 0;

/// Identifies one engine stream. Engines number lines per stream and a restarted stream
/// (new model, re-opened mic, restarted app) starts again, so lines are matched on both.
pub type StreamId = u64;

/// A transcript line.
#[derive(Clone, Debug, PartialEq)]
pub struct Caption {
    pub stream: StreamId,
    pub id: u64,
    pub speaker: Option<&'static str>,
    pub text: String,
    pub complete: bool,
    /// When the line was first heard.
    pub time: SystemTime,
    /// The section's revision when this line or one before it last changed, so stamps only grow
    /// down the section and [`Section::changed_since`] can binary search them.
    revision: u64,
}

impl Caption {
    /// The line as shown and copied, e.g. "You: on my way".
    pub fn display_text(&self) -> String {
        match self.speaker {
            Some(speaker) => format!("{speaker}: {}", self.text),
            None => self.text.clone(),
        }
    }
}

/// One app's transcript.
#[derive(Clone, Debug)]
pub struct Section {
    pub label: String,
    /// The app's executable, for its icon.
    pub exe_path: Option<PathBuf>,
    pub captions: VecDeque<Caption>,
    /// When the app itself last spoke (not your mic lines shown here).
    last_spoke: Option<Instant>,
    /// Counts every line added or revised.
    revision: u64,
    /// How many of the oldest lines were dropped to stay under the cap.
    dropped: u64,
}

/// How long after its last line a section still counts as speaking, for the "Live" badge.
const SPEAKING_FOR: Duration = Duration::from_secs(10);

impl Section {
    fn new(label: &str, exe_path: Option<PathBuf>) -> Self {
        Self { label: label.to_owned(), exe_path, captions: VecDeque::new(), last_spoke: None, revision: 0, dropped: 0 }
    }

    /// Whether this app said something in the last few seconds.
    pub fn speaking(&self, now: Instant) -> bool {
        self.speaking_until().is_some_and(|until| now < until)
    }

    /// When the app stops counting as speaking if it says nothing more.
    pub fn speaking_until(&self) -> Option<Instant> {
        self.last_spoke.map(|t| t + SPEAKING_FOR)
    }

    /// When the app itself last said something; `None` for the You section.
    pub fn last_spoke(&self) -> Option<Instant> {
        self.last_spoke
    }

    /// Goes up with every line added or revised, so a reader can tell whether anything changed.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// How many of the oldest lines have been dropped to stay under the cap; a line's position plus
    /// this number stays the same for as long as the line is kept.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Where the lines added or revised after `revision` start: none before this position changed.
    pub fn changed_since(&self, revision: u64) -> usize {
        self.captions.partition_point(|caption| caption.revision <= revision)
    }
}

/// The You section, then a section per app in the order apps were heard.
#[derive(Clone, Debug)]
pub struct Captions {
    pub sections: Vec<Section>,
}

impl Default for Captions {
    fn default() -> Self {
        Self { sections: vec![Section::new(YOU, None)] }
    }
}

impl Captions {
    /// The section for an app, reusing one with the same name so a restarted app keeps its transcript.
    /// Returns its index for [`Captions::update_app`].
    pub fn add_app(&mut self, label: &str, exe_path: Option<PathBuf>) -> usize {
        if let Some(index) = self.sections.iter().position(|s| s.label == label) {
            return index;
        }
        self.sections.push(Section::new(label, exe_path));
        self.sections.len() - 1
    }

    /// Applies new or revised lines from an app's stream.
    pub fn update_app(&mut self, section: usize, stream: StreamId, lines: &[Line], now: Instant) {
        let section = &mut self.sections[section];
        for line in lines.iter().filter(|l| !l.text.is_empty()) {
            section.upsert(stream, line, None);
            section.last_spoke = Some(now);
        }
    }

    /// Applies new or revised lines from the mic's stream: to the You section, and as "You:" lines
    /// to the app you are talking to, if one spoke recently.
    pub fn update_mic(&mut self, stream: StreamId, lines: &[Line], now: Instant) {
        for line in lines.iter().filter(|l| !l.text.is_empty()) {
            self.sections[YOU_SECTION].upsert(stream, line, None);
            if let Some(app) = self.app_showing(stream, line.id).or_else(|| self.conversation_app(now)) {
                self.sections[app].upsert(stream, line, Some(YOU));
            }
        }
    }

    /// Apps with anything to show, then the You section, which is always shown; with their indexes.
    pub fn visible(&self) -> impl Iterator<Item = (usize, &Section)> {
        let apps = self.sections.iter().enumerate().skip(YOU_SECTION + 1).filter(|(_, s)| !s.captions.is_empty());
        apps.chain([(YOU_SECTION, &self.sections[YOU_SECTION])])
    }

    /// Your last `lines` lines, oldest first, joined into one text; `None` before you said anything.
    pub fn said(&self, lines: usize) -> Option<String> {
        let yours = &self.sections[YOU_SECTION].captions;
        let recent: Vec<&str> = yours.range(yours.len().saturating_sub(lines)..).map(|c| c.text.as_str()).collect();
        (!recent.is_empty()).then(|| recent.join(" "))
    }

    /// A line being revised stays in the app it was first shown in.
    pub fn app_showing(&self, stream: StreamId, id: u64) -> Option<usize> {
        (YOU_SECTION + 1..self.sections.len()).find(|&index| self.sections[index].find(stream, id).is_some())
    }

    fn conversation_app(&self, now: Instant) -> Option<usize> {
        self.sections
            .iter()
            .enumerate()
            .filter(|(_, s)| s.last_spoke.is_some_and(|t| now.duration_since(t) < CONVERSATION_WINDOW))
            .max_by_key(|(_, s)| s.last_spoke)
            .map(|(index, _)| index)
    }
}

impl Section {
    fn upsert(&mut self, stream: StreamId, line: &Line, speaker: Option<&'static str>) {
        self.revision += 1;
        if let Some(index) = self.find(stream, line.id) {
            let existing = &mut self.captions[index];
            existing.text.clone_from(&line.text);
            existing.complete = line.complete;
            // Revised lines are recent, so this stamps only the last few.
            self.captions.range_mut(index..).for_each(|caption| caption.revision = self.revision);
            return;
        }
        let (time, revision) = (SystemTime::now(), self.revision);
        self.captions.push_back(Caption { stream, id: line.id, speaker, text: line.text.clone(), complete: line.complete, time, revision });
        if self.captions.len() > MAX_LINES_PER_SECTION {
            self.captions.pop_front();
            self.dropped += 1;
        }
    }

    /// Revised lines are always recent, so search from the end.
    fn find(&self, stream: StreamId, id: u64) -> Option<usize> {
        self.captions.iter().rposition(|c| c.stream == stream && c.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: StreamId = 1;
    const MIC: StreamId = 2;

    fn line(id: u64, text: &str) -> Line {
        Line { id, text: text.into(), complete: true }
    }

    #[test]
    fn mic_joins_the_app_that_spoke_last() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let discord = captions.add_app("Discord", None);
        let opera = captions.add_app("Opera", None);
        captions.update_app(opera, APP, &[line(1, "Video")], now);
        captions.update_app(discord, APP, &[line(2, "Alex: joining?")], now + Duration::from_secs(1));
        captions.update_mic(MIC, &[line(3, "Two minutes.")], now + Duration::from_secs(2));
        assert_eq!(captions.sections[discord].captions.back().unwrap().speaker, Some(YOU));
    }

    #[test]
    fn mic_lines_always_land_in_you_and_in_the_app_you_are_talking_to() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let discord = captions.add_app("Discord", None);
        captions.update_app(discord, APP, &[line(1, "Joining?")], now);
        captions.update_mic(MIC, &[line(2, "On my way.")], now);
        captions.update_mic(MIC, &[line(3, "Anyone there?")], now + CONVERSATION_WINDOW);
        let texts = |section: usize| captions.sections[section].captions.iter().map(|c| c.display_text()).collect::<Vec<_>>();
        assert_eq!(texts(YOU_SECTION), ["On my way.", "Anyone there?"]);
        assert_eq!(texts(discord), ["Joining?", "You: On my way."]);
        assert_eq!(captions.visible().last().unwrap().0, YOU_SECTION, "You is drawn last");
    }

    #[test]
    fn revised_line_replaces_in_place_and_history_is_kept() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("App", None);
        captions.update_app(app, APP, &[Line { id: 1, text: "hel".into(), complete: false }], now);
        captions.update_app(app, APP, &[line(1, "hello")], now);
        for id in 2..=4 {
            captions.update_app(app, APP, &[line(id, "x")], now);
        }
        let texts: Vec<&str> = captions.sections[app].captions.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["hello", "x", "x", "x"]);
    }

    #[test]
    fn mic_and_app_lines_with_the_same_id_stay_separate() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("App", None);
        captions.update_app(app, APP, &[line(0, "from the app")], now);
        captions.update_mic(MIC, &[line(0, "from you")], now);
        let texts: Vec<&str> = captions.sections[app].captions.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["from the app", "from you"]);
    }

    #[test]
    fn a_restarted_stream_adds_lines_instead_of_overwriting_old_ones() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("App", None);
        captions.update_app(app, 1, &[line(0, "before the switch")], now);
        captions.update_app(app, 2, &[line(0, "after the switch")], now);
        let texts: Vec<&str> = captions.sections[app].captions.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["before the switch", "after the switch"]);
    }

    #[test]
    fn said_grows_backwards_one_line_at_a_time() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        assert_eq!(captions.said(1), None);
        let app = captions.add_app("Discord", None);
        captions.update_mic(MIC, &[line(1, "Earlier.")], now);
        captions.update_app(app, APP, &[line(1, "What now?")], now);
        captions.update_mic(MIC, &[line(2, "I can reapply paste.")], now);
        assert_eq!(captions.said(1).as_deref(), Some("I can reapply paste."));
        assert_eq!(captions.said(2).as_deref(), Some("Earlier. I can reapply paste."));
        assert_eq!(captions.said(5).as_deref(), Some("Earlier. I can reapply paste."));
    }

    #[test]
    fn changed_since_finds_new_and_revised_lines() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("App", None);
        let open = |id, text: &str| Line { id, text: text.into(), complete: false };
        captions.update_app(app, APP, &[line(1, "One."), open(2, "tw"), line(3, "Three.")], now);
        let seen = captions.sections[app].revision();
        assert_eq!(captions.sections[app].changed_since(seen), 3, "nothing changed");
        captions.update_app(app, APP, &[line(2, "Two.")], now);
        assert_eq!(captions.sections[app].changed_since(seen), 1, "the revised line and all after it");
        captions.update_app(app, APP, &[line(4, "Four.")], now);
        assert_eq!(captions.sections[app].changed_since(captions.sections[app].revision() - 1), 3, "only the new line");
    }

    #[test]
    fn dropped_lines_are_counted() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let lines: Vec<Line> = (0..MAX_LINES_PER_SECTION as u64 + 5).map(|id| line(id, "x")).collect();
        captions.update_mic(MIC, &lines, now);
        let you = &captions.sections[YOU_SECTION];
        assert_eq!((you.captions.len(), you.dropped()), (MAX_LINES_PER_SECTION, 5));
        assert_eq!(you.captions[0].id, 5);
    }

    #[test]
    fn restarted_app_keeps_its_section() {
        let mut captions = Captions::default();
        let first = captions.add_app("Discord", None);
        assert_eq!(captions.add_app("Discord", None), first);
    }
}
