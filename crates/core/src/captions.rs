//! The transcript: one section per app holding everything it said this session.
//!
//! Every mic line goes into the permanent "You" section. It is also shown as "You:" inside the apps
//! you pinned, whichever window is in front, or with none pinned, inside the app in front.

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

use crate::{engine::Line, voices::book::Place};

/// Oldest lines are dropped past this, to bound memory on very long sessions.
const MAX_LINES_PER_SECTION: usize = 10_000;
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
    /// Who said it among the voices of its stream, when voices are told apart.
    pub voice: Option<u32>,
    /// The name shown before the text: your own lines' ("You", "Mic 2"), or the speaker's once a section holds two.
    label: Option<String>,
    /// Whether the label is a name given to the voice rather than its number.
    named: bool,
    /// Said into your mic: in the You section, or one of your lines copied into an app.
    pub mine: bool,
    /// Where its voice was recognised, which says what its voice number and any name mean.
    place: Option<Place>,
    pub text: String,
    pub complete: bool,
    /// When the line was first heard.
    pub time: SystemTime,
    /// The section's revision when this line or one before it last changed, so stamps only grow
    /// down the section and [`Section::changed_since`] can binary search them.
    revision: u64,
}

impl Caption {
    /// The line as shown and copied, e.g. "You: on my way" or "Speaker 2: on my way".
    pub fn display_text(&self) -> String {
        match &self.label {
            Some(label) => format!("{label}: {}", self.text),
            None => self.text.clone(),
        }
    }

    /// The name before the text, without its colon.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    fn set_label(&mut self, label: Option<(String, bool)>) {
        (self.label, self.named) = match label {
            Some((label, named)) => (Some(label), named),
            None => (None, false),
        };
    }

    /// Whether the voice has a name of its own, which the label then is.
    pub fn is_named(&self) -> bool {
        self.named
    }

    /// Whether the voice that said it can be given a name: any but the first voice at your mic, which is "You".
    pub fn nameable(&self) -> bool {
        match self.voice {
            Some(voice) => !self.mine || voice > 0,
            None => false,
        }
    }

    /// The label as short as it goes: a speaker without a name is its number ("3"), anything else its label.
    pub fn tag(&self) -> Option<String> {
        let label = self.label()?;
        match (self.named, self.mine, self.voice) {
            (false, false, Some(voice)) => Some((voice + 1).to_string()),
            _ => Some(label.to_owned()),
        }
    }

    /// How many UTF-16 units of [`Caption::display_text`] are the label and its colon.
    pub fn label_len(&self) -> usize {
        self.label.as_ref().map_or(0, |label| label.encode_utf16().count() + 1)
    }

    /// The length of [`Caption::display_text`] in bytes, for estimating a row's height.
    pub fn display_len(&self) -> usize {
        self.text.len() + self.label.as_ref().map_or(0, |label| label.len() + 2)
    }
}

/// One app's transcript.
#[derive(Clone, Debug)]
pub struct Section {
    pub label: String,
    /// The app's executable, for its icon.
    pub exe_path: Option<PathBuf>,
    pub captions: VecDeque<Caption>,
    /// Whether this is the You section: its voices are the mic's.
    mic: bool,
    /// The voices of the section's own lines, which are labelled once there are two.
    voices: BTreeSet<u32>,
    /// The saved voices among the voices heard in each place (by zero-based number): a name replaces "Speaker N".
    /// Lines keep the place they were heard in, so changing where speakers are recognised leaves earlier lines as they were.
    names: HashMap<Place, HashMap<u32, String>>,
    /// How often earlier lines were given labels, which moves the text after them.
    relabels: u32,
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
    fn new(label: &str, exe_path: Option<PathBuf>, mic: bool) -> Self {
        Self { label: label.to_owned(), exe_path, captions: VecDeque::new(), mic, voices: BTreeSet::new(), names: HashMap::new(), relabels: 0, last_spoke: None, revision: 0, dropped: 0 }
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
    /// Apps, by label, that get every mic line once they have a section.
    pub pinned: Vec<String>,
    /// Where the voices of each stream are recognised, for streams that tell voices apart.
    places: HashMap<StreamId, Place>,
}

impl Default for Captions {
    fn default() -> Self {
        Self { sections: vec![Section::new(YOU, None, true)], pinned: Vec::new(), places: HashMap::new() }
    }
}

impl Captions {
    /// The section for an app, reusing one with the same name so a restarted app keeps its transcript.
    /// Returns its index for [`Captions::update_app`].
    pub fn add_app(&mut self, label: &str, exe_path: Option<PathBuf>) -> usize {
        if let Some(index) = self.sections.iter().position(|s| s.label == label) {
            return index;
        }
        self.sections.push(Section::new(label, exe_path, false));
        self.sections.len() - 1
    }

    /// Applies new or revised lines from an app's stream.
    pub fn update_app(&mut self, section: usize, stream: StreamId, lines: &[Line], now: Instant) {
        let place = self.places.get(&stream).cloned();
        let section = &mut self.sections[section];
        for line in lines.iter().filter(|l| !l.text.is_empty()) {
            section.upsert(stream, line, None, place.as_ref());
            section.last_spoke = Some(now);
        }
    }

    /// Applies new or revised lines from the mic's stream: to the You section, and as "You:" lines
    /// to the pinned apps, or with none heard yet, to the app labelled `in_front` (the window in front).
    pub fn update_mic(&mut self, stream: StreamId, lines: &[Line], in_front: Option<&str>) {
        let place = self.places.get(&stream).cloned();
        let apps = YOU_SECTION + 1..self.sections.len();
        let pinned: Vec<usize> = apps.clone().filter(|&index| self.is_pinned(index)).collect();
        let in_front = apps.into_iter().find(|&index| in_front == Some(self.sections[index].label.as_str()));
        for line in lines.iter().filter(|l| !l.text.is_empty()) {
            self.sections[YOU_SECTION].upsert(stream, line, None, place.as_ref());
            let apps = match pinned.is_empty() {
                true => self.app_showing(stream, line.id).or(in_front).into_iter().collect(),
                false => pinned.clone(),
            };
            for app in apps {
                self.sections[app].upsert(stream, line, Some(YOU), place.as_ref());
            }
        }
    }

    /// Says where the voices of `stream` are recognised, so its lines are named by that place's names.
    pub fn set_stream_place(&mut self, stream: StreamId, place: Place) {
        self.places.insert(stream, place);
    }

    /// Names the saved voices heard in `place` (zero-based number to name) in a section, replacing that place's earlier
    /// names there and relabelling its lines; lines of other places keep theirs.
    pub fn set_names(&mut self, section: usize, place: &Place, names: HashMap<u32, String>) {
        self.sections[section].set_names(place, names);
    }

    /// Names the saved voices among the mic's, in the You section and on your lines shown in apps. The first mic voice
    /// is always "You".
    pub fn set_mic_names(&mut self, names: HashMap<u32, String>) {
        self.sections.iter_mut().for_each(|section| section.set_names(&Place::Mic, names.clone()));
    }

    /// Whether the section at `index` gets every mic line.
    pub fn is_pinned(&self, index: usize) -> bool {
        index != YOU_SECTION && self.pinned.contains(&self.sections[index].label)
    }

    /// How often lines were given labels after they were shown, over every section: a selection made
    /// before that has shifted.
    pub fn relabels(&self) -> u32 {
        self.sections.iter().map(|section| section.relabels).sum()
    }

    /// Pins or unpins the app labelled `label`.
    pub fn pin(&mut self, label: &str, on: bool) {
        self.pinned.retain(|pinned| pinned != label);
        if on {
            self.pinned.push(label.to_owned());
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
}

/// What a second, third... voice at the mic is called, before its number.
pub const MIC: &str = "Mic";

/// What a voice at the mic is called: you first, then "Mic 2", "Mic 3".
fn mic_name(voice: Option<u32>) -> String {
    match voice {
        None | Some(0) => YOU.to_owned(),
        Some(other) => format!("{MIC} {}", other + 1),
    }
}

impl Section {
    /// The label for a line with `voice` heard in `place`: yours (`speaker`) always, the section's own once it holds two
    /// voices; a saved voice's name always, except for the first voice at the mic, which is always "You".
    /// With whether it is a saved voice's name.
    fn label_for(&self, speaker: Option<&str>, voice: Option<u32>, place: Option<&Place>) -> Option<(String, bool)> {
        let named = || self.names.get(place?)?.get(&voice?).map(|name| (name.clone(), true));
        let numbered = |label| Some((label, false));
        match (speaker, voice) {
            (Some(_), None | Some(0)) => numbered(YOU.to_owned()),
            (Some(_), _) => named().or_else(|| numbered(mic_name(voice))),
            (None, Some(0)) if self.mic => (self.voices.len() >= 2).then(|| (YOU.to_owned(), false)),
            (None, Some(voice)) => named().or_else(|| {
                (self.voices.len() >= 2).then(|| (if self.mic { mic_name(Some(voice)) } else { format!("Speaker {}", voice + 1) }, false))
            }),
            (None, None) => None,
        }
    }

    /// Replaces the names of the voices heard in `place`, relabelling the lines they changed.
    fn set_names(&mut self, place: &Place, names: HashMap<u32, String>) {
        if self.names.get(place).map_or(names.is_empty(), |old| *old == names) {
            return;
        }
        match names.is_empty() {
            true => self.names.remove(place),
            false => self.names.insert(place.clone(), names),
        };
        self.revision += 1;
        self.label_own_lines();
    }

    fn upsert(&mut self, stream: StreamId, line: &Line, speaker: Option<&'static str>, place: Option<&Place>) {

        self.revision += 1;
        let before = self.voices.len();
        if let (None, Some(voice)) = (speaker, line.voice) {
            self.voices.insert(voice);
        }
        let label = self.label_for(speaker, line.voice, place);
        if let Some(index) = self.find(stream, line.id) {
            let existing = &mut self.captions[index];
            existing.text.clone_from(&line.text);
            existing.complete = line.complete;
            existing.voice = line.voice;
            existing.set_label(label);
            // Revised lines are recent, so this stamps only the last few.
            self.captions.range_mut(index..).for_each(|caption| caption.revision = self.revision);
        } else {
            let (time, revision) = (SystemTime::now(), self.revision);
            let mut caption = Caption { stream, id: line.id, speaker, voice: line.voice, label: None, named: false, mine: speaker.is_some() || self.mic, place: place.cloned(), text: line.text.clone(), complete: line.complete, time, revision };
            caption.set_label(label);
            self.captions.push_back(caption);
            self.trim();
        }
        if before < 2 && self.voices.len() >= 2 {
            self.label_own_lines();
        }
    }

    /// A second voice has come: the lines before it get their labels too, and the whole section counts as changed.
    fn label_own_lines(&mut self) {
        let labels: Vec<_> = self.captions.iter().map(|c| self.label_for(c.speaker, c.voice, c.place.as_ref())).collect();
        self.relabels += 1;
        for (caption, label) in self.captions.iter_mut().zip(labels) {
            caption.set_label(label);
            caption.revision = self.revision;
        }
    }

    fn trim(&mut self) {
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
        Line { id, text: text.into(), complete: true, voice: None }
    }

    #[test]
    fn mic_joins_the_app_in_front_not_the_one_that_spoke_last() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let discord = captions.add_app("Discord", None);
        let opera = captions.add_app("Opera", None);
        captions.update_app(discord, APP, &[line(1, "Alex: joining?")], now);
        captions.update_app(opera, APP, &[line(2, "Video")], now + Duration::from_secs(1));
        captions.update_mic(MIC, &[line(3, "Two minutes.")], Some("Discord"));
        assert_eq!(captions.sections[discord].captions.back().unwrap().speaker, Some(YOU));
        assert_eq!(captions.sections[opera].captions.back().unwrap().speaker, None);
    }

    #[test]
    fn mic_lines_always_land_in_you_and_in_the_app_in_front() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let discord = captions.add_app("Discord", None);
        captions.update_app(discord, APP, &[line(1, "Joining?")], now);
        captions.update_mic(MIC, &[line(2, "On my way.")], Some("Discord"));
        captions.update_mic(MIC, &[line(3, "Anyone there?")], Some("Notepad"));
        let texts = |section: usize| captions.sections[section].captions.iter().map(|c| c.display_text()).collect::<Vec<_>>();
        assert_eq!(texts(YOU_SECTION), ["On my way.", "Anyone there?"]);
        assert_eq!(texts(discord), ["Joining?", "You: On my way."]);
        assert_eq!(captions.visible().last().unwrap().0, YOU_SECTION, "You is drawn last");
    }

    #[test]
    fn pinned_apps_get_every_mic_line_and_the_app_in_front_none() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let discord = captions.add_app("Discord", None);
        let browser = captions.add_app("Browser", None);
        captions.pin("Discord", true);
        captions.update_app(browser, APP, &[line(1, "A song.")], now);
        captions.update_mic(MIC, &[line(2, "Still here.")], Some("Browser"));
        let texts = |section: usize| captions.sections[section].captions.iter().map(|c| c.display_text()).collect::<Vec<_>>();
        assert_eq!(texts(discord), ["You: Still here."], "whichever window is in front");
        assert_eq!(texts(browser), ["A song."]);

        captions.pin("Discord", false);
        captions.update_mic(MIC, &[line(3, "Hello?")], Some("Browser"));
        assert_eq!(captions.sections[browser].captions.back().unwrap().display_text(), "You: Hello?");
    }

    #[test]
    fn revised_line_replaces_in_place_and_history_is_kept() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("App", None);
        captions.update_app(app, APP, &[Line { id: 1, text: "hel".into(), complete: false, voice: None }], now);
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
        captions.update_mic(MIC, &[line(0, "from you")], Some("App"));
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
        captions.update_mic(MIC, &[line(1, "Earlier.")], None);
        captions.update_app(app, APP, &[line(1, "What now?")], now);
        captions.update_mic(MIC, &[line(2, "I can reapply paste.")], None);
        assert_eq!(captions.said(1).as_deref(), Some("I can reapply paste."));
        assert_eq!(captions.said(2).as_deref(), Some("Earlier. I can reapply paste."));
        assert_eq!(captions.said(5).as_deref(), Some("Earlier. I can reapply paste."));
    }

    #[test]
    fn changed_since_finds_new_and_revised_lines() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("App", None);
        let open = |id, text: &str| Line { id, text: text.into(), complete: false, voice: None };
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
        let mut captions = Captions::default();
        let lines: Vec<Line> = (0..MAX_LINES_PER_SECTION as u64 + 5).map(|id| line(id, "x")).collect();
        captions.update_mic(MIC, &lines, None);
        let you = &captions.sections[YOU_SECTION];
        assert_eq!((you.captions.len(), you.dropped()), (MAX_LINES_PER_SECTION, 5));
        assert_eq!(you.captions[0].id, 5);
    }

    fn said_by(id: u64, text: &str, voice: u32) -> Line {
        Line { voice: Some(voice), ..line(id, text) }
    }

    fn shown(captions: &Captions, section: usize) -> Vec<String> {
        captions.sections[section].captions.iter().map(Caption::display_text).collect()
    }

    #[test]
    fn captions_keep_their_lines_voice() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("Meet", None);
        captions.update_app(app, APP, &[said_by(1, "Hello.", 2), line(2, "Unlabelled.")], now);
        let voices: Vec<Option<u32>> = captions.sections[app].captions.iter().map(|c| c.voice).collect();
        assert_eq!(voices, [Some(2), None]);
    }

    #[test]
    fn an_app_shows_speaker_labels_only_once_it_holds_two_voices() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("Meet", None);
        captions.update_app(app, APP, &[said_by(1, "Welcome.", 0), said_by(2, "Thanks.", 0)], now);
        assert_eq!(shown(&captions, app), ["Welcome.", "Thanks."], "one narrator stays clean");
        let seen = captions.sections[app].revision();
        captions.update_app(app, APP, &[said_by(3, "Me too.", 1)], now);
        assert_eq!(shown(&captions, app), ["Speaker 1: Welcome.", "Speaker 1: Thanks.", "Speaker 2: Me too."], "earlier lines get theirs");
        assert_eq!(captions.sections[app].changed_since(seen), 0, "the relabelled lines count as changed");
    }

    #[test]
    fn tags_are_short_and_only_other_voices_can_be_named() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("Meet", None);
        captions.update_app(app, APP, &[said_by(1, "Hi.", 0), said_by(2, "Hello.", 2)], now);
        captions.update_mic(MIC, &[said_by(1, "Me.", 0), said_by(2, "Not me.", 1)], Some("Meet"));
        let tags: Vec<_> = captions.sections[app].captions.iter().map(|c| (c.tag(), c.nameable(), c.is_named())).collect();
        assert_eq!(tags, [(Some("1".into()), true, false), (Some("3".into()), true, false), (Some("You".into()), false, false), (Some("Mic 2".into()), true, false)]);
    }

    #[test]
    fn a_label_says_how_much_of_the_shown_text_it_is() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("Meet", None);
        captions.update_app(app, APP, &[said_by(1, "Welcome.", 0), said_by(2, "Me too.", 1)], now);
        let [first, _] = [&captions.sections[app].captions[0], &captions.sections[app].captions[1]];
        let shown = first.display_text();
        assert_eq!((first.label(), first.label_len(), first.display_len()), (Some("Speaker 1"), "Speaker 1:".len(), shown.len()));
        assert_eq!(&shown[..first.label_len()], "Speaker 1:");
    }

    #[test]
    fn mic_voices_are_you_then_mic_2() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("Discord", None);
        captions.update_app(app, APP, &[said_by(1, "Joining?", 0), said_by(2, "Yes.", 1)], now);
        captions.update_mic(MIC, &[said_by(1, "On my way.", 0)], Some("Discord"));
        assert_eq!(shown(&captions, YOU_SECTION), ["On my way."], "your section is clean while it is only you");
        captions.update_mic(MIC, &[said_by(2, "Hi, I'm his friend.", 1), line(3, "Without a voice.")], Some("Discord"));
        assert_eq!(shown(&captions, YOU_SECTION), ["You: On my way.", "Mic 2: Hi, I'm his friend.", "Without a voice."]);
        assert_eq!(shown(&captions, app), ["Speaker 1: Joining?", "Speaker 2: Yes.", "You: On my way.", "Mic 2: Hi, I'm his friend.", "You: Without a voice."]);
    }

    #[test]
    fn a_saved_voices_name_replaces_its_label_on_earlier_lines_too() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("Discord", None);
        let place = Place::App("Discord".into());
        captions.set_stream_place(APP, place.clone());
        captions.set_stream_place(MIC, Place::Mic);
        captions.update_app(app, APP, &[said_by(1, "Joining?", 0)], now);
        assert_eq!(shown(&captions, app), ["Joining?"], "one voice, no label");
        let stamp = captions.sections[app].revision();
        captions.set_names(app, &place, HashMap::from([(0, "Alex".to_string())]));
        assert_eq!(shown(&captions, app), ["Alex: Joining?"], "a name is shown even for the only voice");
        assert!(captions.sections[app].revision() > stamp);
        assert_eq!(captions.sections[app].changed_since(stamp), 0, "the relabelled lines count as changed, for the panel's rows and captions mode");
        captions.update_app(app, APP, &[said_by(2, "Yes.", 1)], now);
        assert_eq!(shown(&captions, app), ["Alex: Joining?", "Speaker 2: Yes."]);
        let stamp = captions.sections[app].revision();
        captions.set_names(app, &place, HashMap::new());
        assert_eq!(shown(&captions, app), ["Speaker 1: Joining?", "Speaker 2: Yes."], "forgotten, it is a number again");
        assert_eq!(captions.sections[app].changed_since(stamp), 0);
        captions.set_names(app, &place, HashMap::new());
        assert_eq!(captions.sections[app].revision(), stamp + 1, "no change, no new stamp");

        captions.update_mic(MIC, &[said_by(3, "On my way.", 0), said_by(4, "Hi.", 1)], Some("Discord"));
        let stamp = captions.sections[app].revision();
        captions.set_mic_names(HashMap::from([(0, "Me".to_string()), (1, "Sam".to_string())]));
        assert_eq!(shown(&captions, app)[2..], ["You: On my way.", "Sam: Hi."], "the first mic voice stays You; another may take a saved name");
        assert_eq!(shown(&captions, YOU_SECTION), ["You: On my way.", "Sam: Hi."]);
        assert_eq!(captions.sections[app].changed_since(stamp), 0);
    }

    #[test]
    fn changing_where_voices_are_recognised_leaves_earlier_lines_named_as_they_were() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("Discord", None);
        let (each, across) = (Place::App("Discord".into()), Place::AllApps);
        captions.set_stream_place(1, each.clone());
        captions.update_app(app, 1, &[said_by(1, "Before.", 0)], now);
        captions.set_names(app, &each, HashMap::from([(0, "Alex".to_string())]));
        // The setting flips: a new stream recognises voices across apps, where number 1 is someone else.
        captions.set_stream_place(2, across.clone());
        captions.update_app(app, 2, &[said_by(1, "After.", 0)], now);
        captions.set_names(app, &across, HashMap::from([(0, "Sam".to_string())]));
        assert_eq!(shown(&captions, app), ["Alex: Before.", "Sam: After."]);
        captions.set_names(app, &across, HashMap::new());
        assert_eq!(shown(&captions, app), ["Alex: Before.", "After."], "the other place's names never reach the earlier lines");
    }


    #[test]
    fn restarted_app_keeps_its_section() {
        let mut captions = Captions::default();
        let first = captions.add_app("Discord", None);
        assert_eq!(captions.add_app("Discord", None), first);
    }
}
