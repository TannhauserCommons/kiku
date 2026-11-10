//! Saves caption lines to the [`History`] database. In auto save mode every finished line is queued
//! as it finishes; in manual mode only what a save button asks for. Queued lines are written every
//! few seconds, and at once when captioning stops, the model changes, a save button is pressed or
//! the app quits.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::{Duration, Instant, SystemTime},
};

use crate::{
    captions::{Captions, StreamId, YOU, YOU_SECTION},
    controls::{Controls, SaveRequest},
    engine::Line,
    history::{self, History, NewLine},
    log,
};

/// How often queued lines are written: at most this much is lost if the PC loses power.
const WRITE_EVERY: Duration = Duration::from_secs(5);
/// How often lines older than the kept number of days are deleted.
const PRUNE_EVERY: Duration = Duration::from_secs(3600);
/// Old lines deleted per tick, so deleting many never holds up captions for long.
const PRUNE_BATCH: usize = 1000;
const DAY: Duration = Duration::from_secs(86_400);
/// How often opening the database is tried again after it failed.
const REOPEN_EVERY: Duration = Duration::from_secs(60);
/// The most lines kept waiting while they can't be saved (hours of talk); the oldest go first.
const MAX_PENDING: usize = 10_000;

pub struct Recorder {
    path: PathBuf,
    /// `None` while the database can't be opened; captions still work, unsaved, and opening is
    /// tried again every [`REOPEN_EVERY`].
    history: Option<(History, i64)>,
    last_open: Instant,
    pending: Vec<NewLine>,
    /// Lines already queued, so a line reported finished twice is saved once.
    saved: HashSet<(StreamId, u64)>,
    last_write: Instant,
    /// When old lines were last all deleted, and for how many kept days.
    last_prune: Option<(Instant, u32)>,
    /// Old lines deleted so far by the batches of the deletion under way.
    pruned: usize,
}

impl Recorder {
    /// Opens the database and starts a session.
    pub fn start() -> Self {
        Self::start_at(history::path())
    }

    fn start_at(path: PathBuf) -> Self {
        let history = open(&path);
        let now = Instant::now();
        Self { path, history, last_open: now, pending: Vec::new(), saved: HashSet::new(), last_write: now, last_prune: None, pruned: 0 }
    }

    /// In auto save mode, queues the newly finished lines from a stream. `section` is the app's
    /// section, or `None` for the mic.
    pub fn note(&mut self, controls: &Controls, captions: &Captions, stream: StreamId, section: Option<usize>, lines: &[Line]) {
        let mic_only = controls.save_mic_only.load(Ordering::Relaxed);
        if !controls.auto_save.load(Ordering::Relaxed) || (mic_only && section.is_some()) {
            return;
        }
        for line in lines.iter().filter(|l| l.complete && !l.text.is_empty()) {
            self.queue(captions, (stream, line.id), section, &line.text, SystemTime::now(), false);
        }
    }

    /// Queues the finished lines of one section, or of all of them, that aren't saved yet: what a
    /// save button does in manual save mode. They are kept when old lines are deleted.
    pub fn save_captions(&mut self, captions: &Captions, request: SaveRequest) {
        let sections = match request {
            SaveRequest::Section(index) => index..index + 1,
            SaveRequest::All => 0..captions.sections.len(),
        };
        for index in sections {
            for caption in captions.sections[index].captions.iter().filter(|c| c.complete && !c.text.is_empty()) {
                let mic = index == YOU_SECTION || caption.speaker.is_some();
                let section = (!mic).then_some(index);
                self.queue(captions, (caption.stream, caption.id), section, &caption.text, caption.time, true);
            }
        }
    }

    /// Queues a line, by its stream and id, unless it was queued before. Your mic lines (`section` is
    /// `None`) note the app you were talking to.
    fn queue(&mut self, captions: &Captions, (stream, id): (StreamId, u64), section: Option<usize>, text: &str, at: SystemTime, by_hand: bool) {
        if !self.saved.insert((stream, id)) {
            return;
        }
        let label = |index: usize| captions.sections[index].label.clone();
        let (source, talking_to) = match section {
            Some(section) => (label(section), None),
            None => (YOU.to_owned(), captions.app_showing(stream, id).map(label)),
        };
        self.pending.push(NewLine { at, mic: section.is_none(), source, talking_to, text: text.to_owned(), by_hand });
    }

    /// Writes queued lines every few seconds. In auto save mode, also deletes old ones every hour or
    /// when the setting changes, a batch per tick; lines saved by hand are kept.
    pub fn tick(&mut self, controls: &Controls) {
        if self.last_write.elapsed() >= WRITE_EVERY {
            self.write();
        }
        if !controls.auto_save.load(Ordering::Relaxed) {
            return;
        }
        let keep_days = controls.keep_days.load(Ordering::Relaxed);
        let due = self.last_prune.is_none_or(|(at, days)| at.elapsed() >= PRUNE_EVERY || days != keep_days);
        if due && !self.prune(keep_days) {
            self.last_prune = Some((Instant::now(), keep_days));
        }
    }

    /// Writes everything queued now, opening the database first if it failed to open a while ago.
    pub fn write(&mut self) {
        self.last_write = Instant::now();
        if self.pending.is_empty() {
            return;
        }
        if self.history.is_none() && self.last_open.elapsed() >= REOPEN_EVERY {
            self.history = open(&self.path);
            self.last_open = Instant::now();
        }
        let written = match &mut self.history {
            Some((history, session)) => {
                history.add(*session, &self.pending).inspect_err(|error| log::write(format!("couldn't save transcript lines: {error}"))).is_ok()
            }
            None => false,
        };
        if written {
            self.pending.clear();
        } else if self.pending.len() > MAX_PENDING {
            // Kept queued for the next try (e.g. while the MCP server briefly holds a lock), within bounds.
            self.pending.drain(..self.pending.len() - MAX_PENDING);
            log::write("too many transcript lines are waiting to be saved; dropping the oldest");
        }
    }

    /// Writes what's queued and marks the session ended, as the app quits.
    pub fn finish(&mut self) {
        self.write();
        if let Some((history, session)) = &self.history
            && let Err(error) = history.end_session(*session)
        {
            log::write(format!("couldn't close the transcript session: {error}"));
        }
    }

    /// Deletes a batch of lines older than `keep_days` (0 keeps everything); returns whether more are left.
    fn prune(&mut self, keep_days: u32) -> bool {
        let Some((history, _)) = &self.history else { return false };
        if keep_days == 0 {
            return false;
        }
        // A hand-edited number of days can reach back before the clock's epoch; nothing is that old.
        let Some(cutoff) = SystemTime::now().checked_sub(DAY * keep_days) else { return false };
        let deleted = history.delete_before(cutoff, PRUNE_BATCH).unwrap_or_else(|error| {
            log::write(format!("couldn't delete old transcripts: {error}"));
            0
        });
        self.pruned += deleted;
        let more = deleted == PRUNE_BATCH;
        if !more && self.pruned > 0 {
            log::write(format!("deleted {} transcript lines older than {keep_days} days", self.pruned));
            self.pruned = 0;
        }
        more
    }
}

/// Opens the database, gives deleted space back to the disk if that's worth it, and starts a session.
fn open(path: &Path) -> Option<(History, i64)> {
    let opened = History::open_at(path).and_then(|history| {
        match history.reclaim_space() {
            Ok(true) => log::write("gave the space of deleted transcript lines back to the disk"),
            Ok(false) => {}
            Err(error) => log::write(format!("couldn't give deleted transcript space back: {error}")),
        }
        let session = history.start_session()?;
        Ok((history, session))
    });
    opened.inspect_err(|error| log::write(format!("transcripts can't be saved for now: {error}"))).ok()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::history::Query;

    use super::*;

    #[test]
    fn lines_wait_within_bounds_while_the_database_cant_open_then_are_saved() {
        // A file where the database's folder should be keeps it from opening.
        let blocker = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-recorder-blocker");
        let _ = fs::remove_dir_all(&blocker);
        fs::write(&blocker, "").unwrap();
        let path = blocker.join("transcripts.db");
        let mut recorder = Recorder::start_at(path.clone());
        assert!(recorder.history.is_none());

        let line = |n: usize| NewLine { at: SystemTime::now(), mic: true, source: YOU.into(), talking_to: None, text: format!("line {n}"), by_hand: false };
        recorder.pending.extend((0..MAX_PENDING + 5).map(line));
        recorder.write();
        assert_eq!(recorder.pending.len(), MAX_PENDING);
        assert_eq!(recorder.pending[0].text, "line 5", "the oldest go first");

        fs::remove_file(&blocker).unwrap();
        recorder.write();
        assert!(recorder.history.is_none(), "not tried again within a minute");
        recorder.last_open -= REOPEN_EVERY;
        recorder.write();
        assert!(recorder.pending.is_empty());
        assert_eq!(History::open_at(&path).unwrap().count(&Query::default()).unwrap(), MAX_PENDING);
    }
}
