//! Dictation as the user controls it: the dictation key held or pressed to toggle, listening on for a moment
//! after it ends, and phrases that end it. One state machine that the key, Kiku's actions and the stop phrases
//! all drive; the panel feeds it events and times and does what it says.

use std::time::{Duration, Instant};

use crate::actions::{tokens, words};

/// How long the key is held before dictation starts, so a chord in that time never dictates.
pub const HOLD_AFTER: Duration = Duration::from_millis(250);
/// The longest release delay; one read from the file above it is this.
pub const MAX_RELEASE_DELAY: Duration = Duration::from_millis(2000);
/// The release delays Settings offers, in milliseconds.
pub const RELEASE_DELAYS: [u64; 5] = [0, 250, 500, 750, 1000];
/// The stop phrase until the user sets their own.
pub const DEFAULT_STOP: &str = "that's all Kiku";

/// How the dictation key dictates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DictationKey {
    /// While it is held.
    #[default]
    Hold,
    /// A bare press starts dictation, the next ends it.
    Toggle,
}

/// The `dictation.*` settings.
#[derive(Clone, Debug, PartialEq)]
pub struct DictationSettings {
    pub key: DictationKey,
    /// How long dictation goes on after it ends, up to [`MAX_RELEASE_DELAY`].
    pub release_delay: Duration,
    /// Phrases that end dictation when a dictated line ends with one; empty is none.
    pub stop: Vec<String>,
}

impl Default for DictationSettings {
    fn default() -> Self {
        Self { key: DictationKey::Hold, release_delay: Duration::ZERO, stop: vec![DEFAULT_STOP.to_owned()] }
    }
}

impl DictationSettings {
    /// Reads one `dictation.*` line; a value it can't read leaves the setting as it was (a delay as 0).
    pub(crate) fn apply_setting(&mut self, key: &str, value: &str) {
        match key.strip_prefix("dictation.").unwrap_or(key) {
            "key" => match value {
                "hold" => self.key = DictationKey::Hold,
                "toggle" => self.key = DictationKey::Toggle,
                _ => {}
            },
            "release_delay" => {
                self.release_delay = value.parse().map_or(Duration::ZERO, |ms| Duration::from_millis(ms).min(MAX_RELEASE_DELAY));
            }
            "stop" => self.stop = value.split(',').map(str::trim).filter(|phrase| !phrase.is_empty()).map(str::to_owned).collect(),
            _ => {}
        }
    }

    /// The `dictation.*` lines, each ending in a line break.
    pub(crate) fn to_settings(&self) -> String {
        let key = match self.key {
            DictationKey::Hold => "hold",
            DictationKey::Toggle => "toggle",
        };
        format!("dictation.key = {key}\ndictation.release_delay = {}\ndictation.stop = {}\n", self.release_delay.as_millis(), self.stop.join(", "))
    }
}

/// The state of dictation. Times come from the caller, so tests inject them.
#[derive(Debug)]
pub struct Dictation {
    key: DictationKey,
    release_delay: Duration,
    /// When the key in hand went down; `None` while it is up.
    down: Option<Instant>,
    /// The press in hand has done what it will (dictated, started or ended dictation, or was a chord).
    spent: bool,
    on: bool,
    latched: bool,
    /// When the release delay that is running ends.
    ends: Option<Instant>,
}

impl Dictation {
    pub fn new(settings: &DictationSettings) -> Self {
        Self {
            key: settings.key,
            release_delay: settings.release_delay,
            down: None,
            spent: false,
            on: false,
            latched: false,
            ends: None,
        }
    }

    /// New settings from now on; dictation in progress goes on.
    pub fn set(&mut self, settings: &DictationSettings) {
        self.key = settings.key;
        self.release_delay = settings.release_delay;
    }

    /// The dictation key went down.
    pub fn key_down(&mut self, now: Instant) {
        if self.down.is_some() {
            return;
        }
        self.down = Some(now);
        // Pressed in a release delay: dictation carries on as if it never ended.
        self.spent = self.ends.take().is_some();
    }

    /// The dictation key was let go.
    pub fn key_up(&mut self, now: Instant) {
        self.tick(now);
        if self.down.take().is_none() {
            return;
        }
        if self.latched {
            if !self.spent {
                self.stop(now);
            }
        } else if self.on {
            self.stop(now);
        } else if self.key == DictationKey::Toggle && !self.spent {
            self.start(now);
        }
    }

    /// Another key was pressed with the dictation key; `ending` for the one that stops captioning.
    pub fn chord(&mut self, now: Instant, ending: bool) {
        self.tick(now);
        self.spent = true;
        if ending || !self.latched {
            self.stop_now();
        }
    }

    /// Starts dictation, which stays on until told (a bare press, a stop, a stop phrase, captions stopping).
    pub fn start(&mut self, _now: Instant) {
        self.on = true;
        self.latched = true;
        self.spent = true;
        self.ends = None;
    }

    /// Ends dictation after the release delay.
    pub fn stop(&mut self, now: Instant) {
        if !self.on || self.ends.is_some() {
            return;
        }
        if self.release_delay.is_zero() {
            self.stop_now();
        } else {
            self.ends = Some(now + self.release_delay);
        }
    }

    /// Stops dictation when it is on (or ending), else starts it.
    pub fn toggle(&mut self, now: Instant) {
        if self.on {
            self.stop(now);
        } else {
            self.start(now);
        }
    }

    /// Ends dictation at once, with no release delay.
    pub fn stop_now(&mut self) {
        self.on = false;
        self.latched = false;
        self.spent = true;
        self.ends = None;
    }

    /// Catches up with `now`: a hold that has lasted [`HOLD_AFTER`] dictates, a release delay that is over ends.
    pub fn tick(&mut self, now: Instant) {
        if self.hold_due().is_some_and(|due| now >= due) {
            self.on = true;
            self.spent = true;
        }
        if self.ends.is_some_and(|ends| now >= ends) {
            self.stop_now();
        }
    }

    pub fn dictating(&self) -> bool {
        self.on
    }

    /// On until told: started by a toggle press or a start step, not by holding the key.
    pub fn latched(&self) -> bool {
        self.latched
    }

    /// When [`Dictation::tick`] must run next, if anything is due.
    pub fn next_wake(&self) -> Option<Instant> {
        [self.hold_due(), self.ends].into_iter().flatten().min()
    }

    /// When the key held now starts dictation, if it will.
    fn hold_due(&self) -> Option<Instant> {
        let down = self.down.filter(|_| self.key == DictationKey::Hold && !self.spent && !self.on)?;
        Some(down + HOLD_AFTER)
    }
}

/// The text before the stop phrase `line` ends with (whole words, case and punctuation aside), trimmed of
/// spaces and commas; `None` when it ends with none of `phrases`.
pub fn stop_phrase<'a>(line: &'a str, phrases: &[String]) -> Option<&'a str> {
    if phrases.is_empty() {
        return None;
    }
    // Most lines end with no phrase's last word: that is found without tokenising the line.
    let last = tokens(last_run(line));
    let ends_like = |phrase: &String| tokens(last_run(phrase)) == last;
    if !phrases.iter().any(ends_like) {
        return None;
    }
    let spoken = words(&line.chars().collect::<Vec<_>>());
    // The longest phrase that matches, so one ending another ("all Kiku", "that's all Kiku") is never cut short.
    phrases.iter().filter_map(|phrase| {
        let phrase = tokens(phrase);
        let first = spoken.len().checked_sub(phrase.len()).filter(|_| !phrase.is_empty())?;
        if !spoken[first..].iter().map(|word| &word.token).eq(phrase.iter()) {
            return None;
        }
        let start = line.char_indices().nth(spoken[first].start)?.0;
        Some(line[..start].trim_end_matches([' ', ',']))
    })
    .min_by_key(|before| before.len())
}

/// The last run of letters and digits in `text`.
fn last_run(text: &str) -> &str {
    text.split(|c: char| !c.is_alphanumeric()).rfind(|run| !run.is_empty()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_stop_phrase_wins() {
        let phrases = vec!["all Kiku".to_owned(), "that's all Kiku".to_owned()];
        assert_eq!(stop_phrase("send it that's all Kiku", &phrases), Some("send it"));
    }

    #[test]
    fn a_late_tick_still_dictates_a_long_hold() {
        let t = Instant::now();
        let mut dictation = Dictation::new(&DictationSettings { release_delay: Duration::from_millis(500), ..DictationSettings::default() });
        dictation.key_down(t);
        dictation.key_up(t + Duration::from_millis(300));
        assert!(dictation.dictating(), "held past 250 ms: dictating, in its release delay");
    }
}
