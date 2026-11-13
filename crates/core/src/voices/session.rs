//! The voices heard in this run of Kiku (`.loop/speakers/NAMES.md`, N8 and N9). A stream numbers its speakers
//! by itself, from 0; the session gives each a number that holds for the whole run, so a quiet minute, an app that
//! restarts or a model that reloads renumbers no one. A stream opens knowing the voices in its place, and hands back
//! its speakers' prints when it ends and every few minutes, which update the session's voices and fold into the saved
//! ones. The session lives in memory only: only voices the user names are saved, through [`SavedVoices`].

use std::collections::HashMap;

use super::{
    book::{valid_name, Place, Print, SavedVoices, VoiceBook, VoiceId, VoiceScope},
    HeardVoice, Known,
};
use crate::engine::Line;

/// What a phrase needs with a saved voice's print to be that voice. Saved voices are few and rest on a settled print, so
/// this is lower than the session bar, which also has to hold when the run knows many voices (`.loop/speakers/names-progress.md`).
pub const NAMED_SIMILARITY: f32 = 0.45;
/// What a phrase needs with a speaker heard earlier this run to be that speaker: raised from clustering's 0.4 because strangers
/// took existing numbers in a run that knew 16 voices (F15).
pub const SESSION_SIMILARITY: f32 = 0.55;
/// A stream is told about at most this many unnamed voices of its place (the most recently heard).
pub const MAX_SESSION_SEEDS: usize = 16;
/// A voice's print counts for at most this much audio (seconds), so it keeps following the speaker.
const MAX_SECONDS: f32 = 600.0;

/// A voice heard this run.
struct Voice {
    place: Place,
    /// 1, 2, ... within the place, in the order the voices were first heard.
    number: u32,
    print: Print,
    seconds: f32,
    /// The saved voice this is, once the user named it or it was recognised as one.
    saved: Option<VoiceId>,
    /// When it was last heard, by the session's clock.
    heard_at: u64,
}

/// What a stream's speaker with a given label was told to be when it opened.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Seed {
    Voice(usize),
    Saved(VoiceId),
}

/// One stream's link to the session: which voice each of its speakers' labels is.
pub struct Following {
    place: Place,
    app: Option<String>,
    seeds: Vec<Seed>,
    /// The session voice of each label seen so far.
    voices: Vec<Option<usize>>,
    /// What each label had heard when it was last handed back, so the next hand-back counts only the audio since.
    taken: Vec<Heard>,
}

/// The audio a speaker has absorbed: the seconds-weighted sum of its embeddings (scaled to length 1) and its seconds.
#[derive(Clone, Default)]
struct Heard {
    sum: Vec<f32>,
    seconds: f32,
}

impl Heard {
    fn of(voice: &HeardVoice) -> Self {
        Self { sum: voice.print.values().iter().map(|x| x * voice.strength).collect(), seconds: voice.seconds }
    }

    /// What was heard after `earlier`: its mean as a print, and the seconds.
    fn since(&self, earlier: &Heard) -> Option<(Print, f32)> {
        let sum: Vec<f32> = match earlier.sum.is_empty() {
            true => self.sum.clone(),
            false => self.sum.iter().zip(&earlier.sum).map(|(now, before)| now - before).collect(),
        };
        Some((Print::new(&sum)?, self.seconds - earlier.seconds)).filter(|(_, seconds)| *seconds > 0.0)
    }
}

impl Following {
    pub fn place(&self) -> &Place {
        &self.place
    }
}

/// How close a phrase must be to a voice to be it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bars {
    /// For a saved voice.
    pub named: f32,
    /// For a voice heard earlier this run.
    pub session: f32,
    /// How many unnamed voices of its place a stream is told about when it opens.
    pub seeds: usize,
}

impl Default for Bars {
    fn default() -> Self {
        Self { named: NAMED_SIMILARITY, session: SESSION_SIMILARITY, seeds: MAX_SESSION_SEEDS }
    }
}

/// The voices of this run, by place.
#[derive(Default)]
pub struct Session {
    voices: Vec<Voice>,
    bars: Bars,
    clock: u64,
}

impl Session {
    /// A session recognising voices with these bars instead of the tuned ones.
    pub fn with_bars(bars: Bars) -> Self {
        Self { voices: Vec::new(), bars, clock: 0 }
    }

    /// What a stream from `place` (of the app labelled `app`, none for the mic) opens knowing: the saved voices
    /// whose scope covers it, then the unnamed voices heard in its place. Its speakers are labelled in that order.
    pub fn follow(&self, place: Place, app: Option<&str>, book: &VoiceBook) -> (Following, Vec<Known>) {
        let mut seeds = Vec::new();
        let mut known = Vec::new();
        for saved in book.voices().iter().filter(|saved| saved.scope.covers(app)) {
            seeds.push(Seed::Saved(saved.id));
            known.push(Known { print: saved.print.clone(), min_similarity: self.bars.named });
        }
        let mut unnamed: Vec<usize> = (0..self.voices.len()).filter(|&i| self.voices[i].place == place && !self.is_saved(i, book)).collect();
        unnamed.sort_by_key(|&i| std::cmp::Reverse(self.voices[i].heard_at));
        for i in unnamed.into_iter().take(self.bars.seeds) {
            seeds.push(Seed::Voice(i));
            known.push(Known { print: self.voices[i].print.clone(), min_similarity: self.bars.session });
        }
        (Following { place, app: app.map(str::to_owned), seeds, voices: Vec::new(), taken: Vec::new() }, known)
    }

    /// How many voices have been heard in `place` this run.
    pub fn voice_count(&self, place: &Place) -> usize {
        self.voices.iter().filter(|voice| voice.place == *place).count()
    }

    /// Gives `lines` the number of their speaker's voice in the session, as a zero-based `voice` (so the first
    /// voice of a place is 0). `heard` is asked for the stream's speakers when a line has a label not seen before;
    /// it gives `None` for a stream that doesn't track them (the model is on another computer), whose lines keep the
    /// stream's own labels. A label without a print (a first phrase too short to embed) is left without a voice.
    pub fn number_lines(&mut self, following: &mut Following, lines: &mut [Line], saved: &SavedVoices, heard: impl FnOnce() -> Option<Vec<HeardVoice>>) {
        let known = |following: &Following, label: u32| following.voices.get(label as usize).copied().flatten();
        let unseen = lines.iter().filter_map(|line| line.voice).any(|label| known(following, label).is_none());
        if unseen {
            match heard() {
                Some(heard) => self.link(following, &heard, &saved.snapshot()),
                None => return,
            }
        }
        for line in lines.iter_mut() {
            let voice = line.voice.and_then(|label| known(following, label));
            voice.into_iter().for_each(|i| self.hear(i));
            line.voice = voice.map(|i| self.voices[i].number - 1);
        }
    }

    /// Folds what a stream heard into the session's voices, and into the saved voices they are: each voice's print
    /// is the mean of what it rests on and the audio the stream heard since the last time, weighted by seconds.
    pub fn hand_back(&mut self, following: &mut Following, heard: &[HeardVoice], book: &VoiceBook, saved: &SavedVoices) {
        self.link(following, heard, book);
        for voice in heard {
            let Some(i) = following.voices.get(voice.label as usize).copied().flatten() else { continue };
            let now = Heard::of(voice);
            let before = std::mem::replace(&mut following.taken[voice.label as usize], now.clone());
            let Some((fresh, seconds)) = now.since(&before) else { continue };
            self.hear(i);
            let session = &mut self.voices[i];
            session.print = session.print.blended(session.seconds, &fresh, seconds);
            session.seconds = (session.seconds + seconds).min(MAX_SECONDS);
            if let Some(id) = session.saved && !saved.learn(id, &fresh, seconds) {
                session.saved = None;
            }
        }
    }

    fn hear(&mut self, i: usize) {
        self.clock += 1;
        self.voices[i].heard_at = self.clock;
    }


    /// Names the voice shown as `voice` (zero-based) in `place`, saving it for every app; renames it if it already
    /// has a name. `None` when the session has no such voice. Hand back the place's streams first, so its print is fresh.
    pub fn name(&mut self, place: &Place, voice: u32, name: &str, app: Option<&str>, saved: &SavedVoices) -> Option<VoiceId> {
        let session = self.voices.iter_mut().find(|v| v.place == *place && v.number == voice + 1)?;
        valid_name(name).ok()?;
        match session.saved.filter(|&id| saved.rename(id, name).is_ok()) {
            Some(id) => Some(id),
            None => {
                let id = saved.add(name, session.print.clone(), session.seconds, VoiceScope::Everywhere, app);
                session.saved = Some(id);
                Some(id)
            }
        }
    }

    /// The names of the voices heard in `place` that are saved voices, by zero-based number.
    pub fn names(&self, place: &Place, book: &VoiceBook) -> HashMap<u32, String> {
        let named = self.voices.iter().filter(|voice| voice.place == *place);
        named.filter_map(|voice| Some((voice.number - 1, book.get(voice.saved?)?.name.clone()))).collect()
    }

    fn is_saved(&self, i: usize, book: &VoiceBook) -> bool {
        self.voices[i].saved.is_some_and(|id| book.get(id).is_some())
    }

    /// Gives every speaker in `heard` its session voice, for the labels this stream hasn't linked yet.
    fn link(&mut self, following: &mut Following, heard: &[HeardVoice], book: &VoiceBook) {
        for voice in heard {
            let label = voice.label as usize;
            if following.voices.len() <= label {
                following.voices.resize(label + 1, None);
                following.taken.resize(label + 1, Heard::default());
            }
            if following.voices[label].is_none() {
                following.voices[label] = Some(self.voice_of(following, voice, book));
            }
        }
    }

    /// The session voice of a speaker not seen yet: the one it started out as, or the closest voice of the place
    /// that this stream hasn't linked to another of its speakers, or a new one.
    fn voice_of(&mut self, following: &Following, speaker: &HeardVoice, book: &VoiceBook) -> usize {
        match following.seeds.get(speaker.label as usize) {
            Some(&Seed::Voice(i)) => i,
            Some(&Seed::Saved(id)) => match (0..self.voices.len()).find(|&i| self.voices[i].place == following.place && self.voices[i].saved == Some(id)) {
                Some(i) => i,
                None => self.add(following.place.clone(), speaker, Some(id)),
            },
            None => match self.closest(following, &speaker.print, book) {
                Some(i) => i,
                None => self.add(following.place.clone(), speaker, None),
            },
        }
    }

    /// A voice of the place, not yet taken by another speaker of the stream, that `print` is close enough to: a voice
    /// heard since the stream opened (in another stream, in across-apps mode).
    fn closest(&self, following: &Following, print: &Print, book: &VoiceBook) -> Option<usize> {
        let allowed = |i: usize| {
            let voice = &self.voices[i];
            let scope_covers = voice.saved.and_then(|id| book.get(id)).is_none_or(|saved| saved.scope.covers(following.app.as_deref()));
            let seeded = following.seeds.iter().any(|seed| match *seed {
                Seed::Voice(seed) => seed == i,
                Seed::Saved(id) => voice.saved == Some(id),
            });
            voice.place == following.place && scope_covers && !seeded && !following.voices.contains(&Some(i))
        };
        let similarity = |i: usize| print.similarity(&self.voices[i].print);
        let bar = |i: usize| if self.is_saved(i, book) { self.bars.named } else { self.bars.session };
        (0..self.voices.len()).filter(|&i| allowed(i) && similarity(i) >= bar(i)).max_by(|&a, &b| similarity(a).total_cmp(&similarity(b)))
    }

    fn add(&mut self, place: Place, speaker: &HeardVoice, saved: Option<VoiceId>) -> usize {
        let number = self.voices.iter().filter(|voice| voice.place == place).count() as u32 + 1;
        self.voices.push(Voice { place, number, print: speaker.print.clone(), seconds: 0.0, saved, heard_at: 0 });
        self.voices.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn print(values: &[f32]) -> Print {
        Print::new(values).unwrap()
    }

    fn heard(label: u32, values: &[f32], seconds: f32) -> HeardVoice {
        HeardVoice { label, print: print(values), strength: seconds, seconds }
    }

    fn line(voice: Option<u32>) -> Line {
        Line { id: 0, text: "hi".into(), complete: true, voice }
    }

    fn numbers(session: &mut Session, following: &mut Following, labels: &[u32], saved: &SavedVoices, speakers: Vec<HeardVoice>) -> Vec<Option<u32>> {
        let mut lines: Vec<Line> = labels.iter().map(|&label| line(Some(label))).collect();
        session.number_lines(following, &mut lines, saved, || Some(speakers));
        lines.iter().map(|line| line.voice).collect()
    }

    const ALEX: [f32; 3] = [1.0, 0.0, 0.0];
    const SAM: [f32; 3] = [0.0, 1.0, 0.0];

    #[test]
    fn a_speaker_keeps_their_number_when_the_stream_restarts() {
        let (mut session, book, saved) = (Session::default(), VoiceBook::default(), SavedVoices::in_memory());
        let place = Place::App("Discord".into());
        let (mut first, known) = session.follow(place.clone(), Some("Discord"), &book);
        assert!(known.is_empty());
        let speakers = vec![heard(0, &ALEX, 5.0), heard(1, &SAM, 4.0)];
        assert_eq!(numbers(&mut session, &mut first, &[0, 1, 0], &saved, speakers.clone()), [Some(0), Some(1), Some(0)]);
        session.hand_back(&mut first, &speakers, &book, &saved);
        // The app restarts: the new stream knows both, the most recently heard first.
        let (mut second, known) = session.follow(place, Some("Discord"), &book);
        assert_eq!(known.len(), 2);
        let label = |values: &[f32]| known.iter().position(|k| k.print.similarity(&print(values)) > 0.99).unwrap() as u32;
        let (alex, sam) = (label(&ALEX), label(&SAM));
        let speakers = vec![heard(alex, &ALEX, 1.0), heard(sam, &SAM, 1.0), heard(2, &[0.0, 0.0, 1.0], 1.0)];
        assert_eq!(numbers(&mut session, &mut second, &[sam, alex, 2], &saved, speakers), [Some(1), Some(0), Some(2)], "a third voice is the next number");
    }

    #[test]
    fn places_number_on_their_own() {
        let (mut session, saved) = (Session::default(), SavedVoices::in_memory());
        let book = saved.snapshot();
        let mut discord = session.follow(Place::App("Discord".into()), Some("Discord"), &book).0;
        let mut edge = session.follow(Place::App("Edge".into()), Some("Edge"), &book).0;
        numbers(&mut session, &mut discord, &[0, 1], &saved, vec![heard(0, &ALEX, 3.0), heard(1, &SAM, 3.0)]);
        assert_eq!(numbers(&mut session, &mut edge, &[0], &saved, vec![heard(0, &SAM, 3.0)]), [Some(0)], "Edge's first voice is its 1, whoever it sounds like");
        assert_eq!(session.follow(Place::App("Edge".into()), Some("Edge"), &book).1.len(), 1);
    }

    #[test]
    fn across_apps_one_numbering_binds_a_voice_heard_in_another_stream() {
        let (mut session, saved) = (Session::default(), SavedVoices::in_memory());
        let book = saved.snapshot();
        let mut discord = session.follow(Place::AllApps, Some("Discord"), &book).0;
        let mut game = session.follow(Place::AllApps, Some("Game"), &book).0;
        numbers(&mut session, &mut discord, &[0], &saved, vec![heard(0, &ALEX, 3.0)]);
        numbers(&mut session, &mut discord, &[1], &saved, vec![heard(0, &ALEX, 3.0), heard(1, &SAM, 3.0)]);
        let close_to_sam = [0.1, 1.0, 0.0];
        assert_eq!(numbers(&mut session, &mut game, &[0], &saved, vec![heard(0, &close_to_sam, 2.0)]), [Some(1)], "the game's new speaker is Sam");
    }

    #[test]
    fn a_saved_voice_is_recognised_named_and_learned_from() {
        let (mut session, saved) = (Session::default(), SavedVoices::in_memory());
        let alex = saved.add("Alex", print(&ALEX), 60.0, VoiceScope::Everywhere, None);
        let only_edge = saved.add("Edgar", print(&SAM), 60.0, VoiceScope::App("Edge".into()), Some("Edge"));
        let book = saved.snapshot();
        let (mut following, known) = session.follow(Place::App("Discord".into()), Some("Discord"), &book);
        assert_eq!(known.len(), 1, "Edgar is only known in Edge");
        assert_eq!(known[0].min_similarity, NAMED_SIMILARITY, "a saved voice has its own bar");
        // Alex is label 0 of the stream, a stranger is label 1.
        let speakers = vec![heard(0, &[0.9, 0.1, 0.0], 20.0), heard(1, &[0.0, 0.0, 1.0], 5.0)];
        assert_eq!(numbers(&mut session, &mut following, &[0, 1], &saved, speakers.clone()), [Some(0), Some(1)]);
        assert_eq!(session.names(&Place::App("Discord".into()), &book), HashMap::from([(0, "Alex".to_string())]));
        session.hand_back(&mut following, &speakers, &book, &saved);
        let learned = saved.snapshot();
        let voice = learned.get(alex).unwrap();
        assert_eq!(voice.seconds, 80.0);
        assert!(voice.print.values()[1] > 0.0, "Alex's print moved toward what was heard");
        assert!(learned.get(only_edge).unwrap().seconds == 60.0);
        // Handing the same stream back again adds only what is new.
        session.hand_back(&mut following, &[heard(0, &[0.9, 0.1, 0.0], 25.0)], &learned, &saved);
        assert_eq!(saved.snapshot().get(alex).unwrap().seconds, 85.0);
    }


    #[test]
    fn a_stranger_never_binds_to_a_seeded_voice_the_stream_has_not_heard_yet() {
        let (mut session, saved) = (Session::default(), SavedVoices::in_memory());
        let place = Place::App("Discord".into());
        let mut first = session.follow(place.clone(), Some("Discord"), &saved.snapshot()).0;
        let speakers = vec![heard(0, &ALEX, 5.0), heard(1, &SAM, 5.0)];
        numbers(&mut session, &mut first, &[0, 1], &saved, speakers.clone());
        session.hand_back(&mut first, &speakers, &saved.snapshot(), &saved);
        let (mut second, known) = session.follow(place, Some("Discord"), &saved.snapshot());
        let alex = known.iter().position(|k| k.print.similarity(&print(&ALEX)) > 0.99).unwrap() as u32;
        // A speaker the stream met as new (label 2) sounds like Alex, whom it was told about but hasn't heard: two people.
        let stranger = [0.9, 0.0, 0.1];
        assert_eq!(numbers(&mut session, &mut second, &[2], &saved, vec![heard(2, &stranger, 3.0)]), [Some(2)], "a third voice, not Alex");
        assert_eq!(numbers(&mut session, &mut second, &[alex], &saved, vec![heard(2, &stranger, 3.0), heard(alex, &ALEX, 3.0)]), [Some(0)], "and Alex is still Alex");
    }

    #[test]
    fn a_hand_back_folds_only_the_audio_heard_since_the_last_one() {
        let (mut session, saved) = (Session::default(), SavedVoices::in_memory());
        let id = saved.add("Alex", print(&ALEX), 100.0, VoiceScope::Everywhere, None);
        let (mut following, _) = session.follow(Place::Mic, None, &saved.snapshot());
        // 100 s in the old room, handed back; then 10 s in a new one (the stream's sum holds both).
        let old_room = heard(0, &ALEX, 100.0);
        session.hand_back(&mut following, std::slice::from_ref(&old_room), &saved.snapshot(), &saved);
        let (new_room, sum) = (print(&SAM), 100.0 * old_room.print.values()[0]);
        let both: Vec<f32> = old_room.print.values().iter().zip(new_room.values()).map(|(a, b)| 100.0 * a + 10.0 * b).collect();
        let sum_length = both.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(sum > 0.0);
        let heard_both = HeardVoice { label: 0, print: print(&both), strength: sum_length, seconds: 110.0 };
        let before = saved.snapshot().get(id).unwrap().print.clone();
        session.hand_back(&mut following, &[heard_both], &saved.snapshot(), &saved);
        let learned = saved.snapshot().get(id).unwrap().clone();
        assert_eq!(learned.seconds, 210.0);
        let expected = before.blended(200.0, &new_room, 10.0);
        assert!(learned.print.similarity(&expected) > 0.9999, "only the 10 s of the new room are folded in, once");
    }

    #[test]
    fn naming_an_unnamed_voice_keeps_its_number_and_renaming_changes_the_name() {
        let (mut session, saved) = (Session::default(), SavedVoices::in_memory());
        let place = Place::App("Discord".into());
        let mut following = session.follow(place.clone(), Some("Discord"), &VoiceBook::default()).0;
        let speakers = vec![heard(0, &ALEX, 5.0), heard(1, &SAM, 6.0)];
        numbers(&mut session, &mut following, &[0, 1], &saved, speakers.clone());
        session.hand_back(&mut following, &speakers, &VoiceBook::default(), &saved);
        let id = session.name(&place, 1, "Sam", Some("Discord"), &saved).unwrap();
        assert_eq!(saved.snapshot().get(id).unwrap().seconds, 6.0);
        assert_eq!(session.names(&place, &saved.snapshot()), HashMap::from([(1, "Sam".to_string())]));
        assert_eq!(session.name(&place, 1, "Samuel", Some("Discord"), &saved), Some(id), "naming a named voice renames it");
        assert_eq!(saved.snapshot().voices().len(), 1);
        assert_eq!(session.name(&place, 7, "Nobody", None, &saved), None);
        assert_eq!(session.name(&place, 1, "  ", None, &saved), None, "a blank name changes nothing");
        assert_eq!(saved.snapshot().get(id).unwrap().name, "Samuel");
        // A forgotten voice is unnamed again, and its stream goes on.
        saved.forget(id);
        assert!(session.names(&place, &saved.snapshot()).is_empty());
        session.hand_back(&mut following, &[heard(1, &SAM, 9.0)], &saved.snapshot(), &saved);
        assert_eq!(session.voices[1].saved, None);
    }

    #[test]
    fn a_stream_that_tracks_nothing_keeps_its_own_labels() {
        let (mut session, saved) = (Session::default(), SavedVoices::in_memory());
        let book = saved.snapshot();
        let mut following = session.follow(Place::Mic, None, &book).0;
        let mut lines = vec![line(Some(2)), line(None)];
        session.number_lines(&mut following, &mut lines, &saved, || None);
        assert_eq!(lines.iter().map(|l| l.voice).collect::<Vec<_>>(), [Some(2), None]);
    }

    #[test]
    fn a_label_without_a_print_gets_no_voice() {
        let (mut session, saved) = (Session::default(), SavedVoices::in_memory());
        let book = saved.snapshot();
        let mut following = session.follow(Place::Mic, None, &book).0;
        assert_eq!(numbers(&mut session, &mut following, &[0, 1], &saved, vec![heard(1, &SAM, 3.0)]), [None, Some(0)]);
    }
}
