//! The panel's one way to dictation and Kiku's actions: the captions key's events and the actions macro
//! steps ask for both come here. It holds the one [`Dictation`], keeps `Controls::dictating` in step with it,
//! does the dictation actions itself and hands every other action back for the panel to perform. Apart from
//! the window, so tests drive it.

use std::{sync::atomic::Ordering, time::Instant};

use kiku_core::{
    controls::Controls,
    dictation::{Dictation, DictationSettings},
    macros::{KikuAction, KikuRequest},
};

use super::keys::KeyEvent;

pub struct Dispatch {
    dictation: Dictation,
}

impl Dispatch {
    pub fn new(settings: &DictationSettings) -> Dispatch {
        Dispatch { dictation: Dictation::new(settings) }
    }

    /// New dictation settings from now on.
    pub fn set(&mut self, settings: &DictationSettings) {
        self.dictation.set(settings);
    }

    /// Feeds a captions-key event to dictation; the action its command key asks for, if any (none for Quit,
    /// which the panel handles as a key).
    pub fn key(&mut self, event: KeyEvent, now: Instant, controls: &Controls) -> Option<KikuAction> {
        let mut action = None;
        match event {
            KeyEvent::Held => self.dictation.key_down(now),
            KeyEvent::Released => self.dictation.key_up(now),
            KeyEvent::Command(command) => {
                action = command.action();
                self.dictation.chord(now, action == Some(KikuAction::ToggleCaptions));
            }
            _ => self.dictation.chord(now, false),
        }
        self.sync(controls);
        action
    }

    /// Does `request`'s part in dictation; the request back when the panel must still perform it (every
    /// action but the dictation ones).
    pub fn perform(&mut self, request: KikuRequest, now: Instant, controls: &Controls) -> Option<KikuRequest> {
        let stopped = controls.stopped.load(Ordering::Relaxed);
        let rest = match request.action {
            // Nothing is heard while captioning is stopped, so dictation doesn't start.
            KikuAction::StartDictation | KikuAction::ToggleDictation if stopped && !self.dictation.dictating() => None,
            KikuAction::StartDictation => {
                self.dictation.start(now);
                None
            }
            KikuAction::StopDictation => {
                if request.at_once { self.dictation.stop_now() } else { self.dictation.stop(now) }
                None
            }
            KikuAction::ToggleDictation => {
                self.dictation.toggle(now);
                None
            }
            KikuAction::StopCaptions => Some(request),
            KikuAction::ToggleCaptions if !stopped => Some(request),
            _ => return Some(request),
        };
        if rest.is_some() {
            self.dictation.stop_now();
        }
        self.sync(controls);
        rest
    }

    /// Catches dictation up with `now`, as the panel's timer does at [`Dispatch::next_wake`].
    pub fn tick(&mut self, now: Instant, controls: &Controls) {
        self.dictation.tick(now);
        self.sync(controls);
    }

    pub fn next_wake(&self) -> Option<Instant> {
        self.dictation.next_wake()
    }

    /// Dictating until told (a toggle press or a start step): the panel shows the Dictating badge.
    pub fn latched(&self) -> bool {
        self.dictation.latched()
    }

    fn sync(&self, controls: &Controls) {
        controls.dictating.store(self.dictation.dictating(), Ordering::Relaxed);
    }
}
