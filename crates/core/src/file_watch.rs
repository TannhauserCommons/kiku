//! Noticing that a file changed, by its modified time, looking at most once a second.

use std::{
    fs,
    path::Path,
    time::{Duration, Instant, SystemTime},
};

/// The file is looked at for changes at most this often.
const CHECK_EVERY: Duration = Duration::from_secs(1);

/// A file's modified time when last read, and when it was last looked at.
#[derive(Default)]
pub struct FileWatch {
    /// `None` if the file was missing.
    modified: Option<SystemTime>,
    /// `None` before the first look.
    checked: Option<Instant>,
}

impl FileWatch {
    /// Whether the file at `path` should be read again: on the first look, and when its modified time moved
    /// since. Looks at most once a second.
    pub fn changed(&mut self, path: &Path) -> bool {
        if self.checked.is_some_and(|checked| checked.elapsed() < CHECK_EVERY) {
            return false;
        }
        let first = self.checked.replace(Instant::now()).is_none();
        let modified = modified(path);
        if !first && modified == self.modified {
            return false;
        }
        self.modified = modified;
        true
    }

    /// The file was just written by us: its new modified time is not a change.
    pub fn wrote(&mut self, path: &Path) {
        self.modified = modified(path);
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|file| file.modified()).ok()
}
