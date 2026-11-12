//! Which sections the panel folds to one row or hides. A section folds by hand, by itself once its app
//! has been quiet a while (opening again when it speaks), or when another is shown alone (solo). Hiding
//! takes an app off the panel only; it is still captioned and saved.

use std::{
    collections::{BTreeSet, HashMap},
    time::{Duration, Instant},
};

use kiku_core::captions::{Captions, Section, YOU_SECTION};

/// The auto-fold setting's choices, in minutes; 0 never folds.
pub const AUTO_FOLD_MINUTES: [u32; 5] = [0, 1, 2, 5, 10];

#[derive(Default)]
pub struct Folds {
    /// Sections folded (true) or opened (false) by hand, which overrides auto-fold.
    by_hand: HashMap<usize, bool>,
    /// The section shown alone, and the folds by hand from before, to put back.
    solo: Option<(usize, HashMap<usize, bool>)>,
    hidden: BTreeSet<usize>,
}

/// How long an app has to be quiet before it folds by itself, from the setting in minutes.
pub fn auto_fold_after(minutes: u32) -> Option<Duration> {
    (minutes > 0).then(|| Duration::from_secs(60 * minutes as u64))
}

impl Folds {
    /// Whether a section shows as one row: as set by hand, else once its app has been quiet for
    /// `auto_fold`. Your own section folds only by hand.
    pub fn folded(&self, index: usize, section: &Section, auto_fold: Option<Duration>, now: Instant) -> bool {
        match self.by_hand.get(&index) {
            Some(&folded) => folded,
            None => index != YOU_SECTION && auto_fold.zip(section.last_spoke()).is_some_and(|(after, spoke)| now >= spoke + after),
        }
    }

    /// When the next open app folds by itself, if any does.
    pub fn next_auto_fold(&self, captions: &Captions, auto_fold: Option<Duration>, now: Instant) -> Option<Instant> {
        let after = auto_fold?;
        let automatic = captions.visible().filter(|(index, _)| *index != YOU_SECTION && !self.by_hand.contains_key(index));
        automatic.filter_map(|(_, section)| Some(section.last_spoke()? + after)).filter(|&at| at > now).min()
    }

    /// Opens a section shown `folded`, or folds an open one, by hand.
    pub fn toggle(&mut self, index: usize, folded: bool) {
        self.by_hand.insert(index, !folded);
    }

    /// Shows `index` alone by folding the other `sections`, or puts the folds back if it already is.
    pub fn solo(&mut self, index: usize, sections: impl IntoIterator<Item = usize>) {
        match self.solo.take() {
            Some((alone, before)) if alone == index => self.by_hand = before,
            earlier => {
                let before = earlier.map_or_else(|| self.by_hand.clone(), |(_, before)| before);
                self.by_hand = sections.into_iter().map(|section| (section, section != index)).collect();
                self.solo = Some((index, before));
            }
        }
    }

    /// Takes an app off the panel; your own section always stays.
    pub fn hide(&mut self, index: usize) {
        if index != YOU_SECTION {
            self.hidden.insert(index);
        }
    }

    pub fn show_hidden(&mut self) {
        self.hidden.clear();
    }

    pub fn is_hidden(&self, index: usize) -> bool {
        self.hidden.contains(&index)
    }

    pub fn hidden_count(&self) -> usize {
        self.hidden.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiku_core::engine::Line;

    const TWO_MINUTES: Option<Duration> = Some(Duration::from_secs(120));

    fn captions(now: Instant) -> (Captions, usize, usize) {
        let mut captions = Captions::default();
        let line = Line { id: 1, text: "hello".into(), complete: true, voice: None };
        let (discord, opera) = (captions.add_app("Discord", None), captions.add_app("Opera", None));
        captions.update_app(discord, 1, std::slice::from_ref(&line), now);
        captions.update_app(opera, 2, &[line], now);
        captions.update_mic(3, &[Line { id: 1, text: "hi".into(), complete: true, voice: None }], Some("Discord"));
        (captions, discord, opera)
    }

    #[test]
    fn a_quiet_app_folds_by_itself_and_opens_when_it_speaks() {
        let now = Instant::now();
        let (mut captions, discord, _) = captions(now);
        let (folds, later) = (Folds::default(), now + Duration::from_secs(121));
        assert!(!folds.folded(discord, &captions.sections[discord], TWO_MINUTES, now));
        assert!(folds.folded(discord, &captions.sections[discord], TWO_MINUTES, later));
        assert!(!folds.folded(discord, &captions.sections[discord], None, later), "auto-fold off");
        assert!(!folds.folded(YOU_SECTION, &captions.sections[YOU_SECTION], TWO_MINUTES, later), "you fold only by hand");
        assert_eq!(folds.next_auto_fold(&captions, TWO_MINUTES, now), Some(now + Duration::from_secs(120)));

        captions.update_app(discord, 1, &[Line { id: 2, text: "back".into(), complete: true, voice: None }], later);
        assert!(!folds.folded(discord, &captions.sections[discord], TWO_MINUTES, later));
    }

    #[test]
    fn folding_by_hand_overrides_auto_fold() {
        let now = Instant::now();
        let (captions, discord, opera) = captions(now);
        let (mut folds, later) = (Folds::default(), now + Duration::from_secs(600));
        folds.toggle(discord, true);
        assert!(!folds.folded(discord, &captions.sections[discord], TWO_MINUTES, later), "opened by hand stays open");
        folds.toggle(opera, false);
        assert!(folds.folded(opera, &captions.sections[opera], TWO_MINUTES, now), "folded by hand stays folded");
        assert_eq!(folds.next_auto_fold(&captions, TWO_MINUTES, now), None);
    }

    #[test]
    fn solo_folds_the_others_and_doing_it_again_puts_them_back() {
        let now = Instant::now();
        let (captions, discord, opera) = captions(now);
        let mut folds = Folds::default();
        folds.toggle(YOU_SECTION, false);
        let sections = [discord, opera, YOU_SECTION];
        let folded = |folds: &Folds| sections.map(|index| folds.folded(index, &captions.sections[index], None, now));

        folds.solo(opera, sections);
        assert_eq!(folded(&folds), [true, false, true]);
        folds.solo(discord, sections);
        assert_eq!(folded(&folds), [false, true, true], "soloing another app moves the solo");
        folds.solo(discord, sections);
        assert_eq!(folded(&folds), [false, false, true], "the folds from before the solo are back");
    }

    #[test]
    fn your_section_is_never_hidden() {
        let mut folds = Folds::default();
        folds.hide(YOU_SECTION);
        folds.hide(2);
        assert!(!folds.is_hidden(YOU_SECTION));
        assert_eq!(folds.hidden_count(), 1);
        folds.show_hidden();
        assert!(!folds.is_hidden(2));
    }
}
