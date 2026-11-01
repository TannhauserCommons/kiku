//! A small diagnostic log at `%APPDATA%\kiku\log.txt`; the app has no console to print to.
//! The previous run's log is kept as `log.old.txt`. A message repeated within a minute is counted
//! instead of written again, and the log stops growing at 1 MiB, so a failure repeating many times
//! a second can't fill the disk.

use std::{
    collections::HashMap,
    fmt::Display,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

use crate::{paths, transcripts::local_clock};

/// The same message is written at most once in this long; repeats in between are counted.
const REPEAT_EVERY: Duration = Duration::from_secs(60);
/// Nothing more is written this run once the log is this big.
const MAX_BYTES: u64 = 1 << 20;

static LOG: Mutex<Option<Log>> = Mutex::new(None);

/// Starts a fresh log for this run, keeping the last one.
pub fn start() {
    let directory = paths::data_dir();
    let _ = fs::create_dir_all(&directory);
    let path = directory.join("log.txt");
    let _ = fs::rename(&path, directory.join("log.old.txt"));
    *LOG.lock().unwrap() = Some(Log::new(path));
    write(format!("started {}", env!("CARGO_PKG_VERSION")));
}

/// Appends a timestamped line. Logging must never break captioning, so failures are ignored.
pub fn write(message: impl Display) {
    if let Some(log) = LOG.lock().unwrap().as_mut() {
        log.write(message.to_string(), Instant::now());
    }
}

struct Log {
    path: PathBuf,
    size: u64,
    /// When each recent message was last written, and how many times it came again since.
    recent: HashMap<String, (Instant, u32)>,
}

impl Log {
    fn new(path: PathBuf) -> Self {
        let size = fs::metadata(&path).map_or(0, |file| file.len());
        Self { path, size, recent: HashMap::new() }
    }

    fn write(&mut self, message: String, now: Instant) {
        if self.size >= MAX_BYTES {
            return;
        }
        self.recent.retain(|_, (written, repeats)| *repeats > 0 || now.duration_since(*written) < REPEAT_EVERY);
        let line = match self.recent.get_mut(&message) {
            Some((written, repeats)) if now.duration_since(*written) < REPEAT_EVERY => {
                *repeats += 1;
                return;
            }
            Some((_, repeats)) => format!("{message} (and {repeats} more times since it was last logged)"),
            None => message.clone(),
        };
        self.recent.insert(message, (now, 0));
        self.append(&line);
        if self.size >= MAX_BYTES {
            self.append("the log is full: nothing more is logged until Kiku restarts");
        }
    }

    fn append(&mut self, text: &str) {
        let line = format!("[{}] {text}\n", local_clock(SystemTime::now()));
        let written = OpenOptions::new().create(true).append(true).open(&self.path).and_then(|mut file| file.write_all(line.as_bytes()));
        if written.is_ok() {
            self.size += line.len() as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn fresh_log(name: &str) -> Log {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").join(format!("test-log-{name}.txt"));
        let _ = fs::remove_file(&path);
        Log::new(path)
    }

    fn lines(log: &Log) -> Vec<String> {
        fs::read_to_string(&log.path).unwrap().lines().map(|line| line.split_once("] ").unwrap().1.to_owned()).collect()
    }

    #[test]
    fn repeats_are_counted_instead_of_written() {
        let mut log = fresh_log("repeats");
        let start = Instant::now();
        for tick in 0..300 {
            let now = start + Duration::from_millis(200 * tick);
            log.write("source 3: stream failed; reopening".into(), now);
            log.write("source 4: stream failed; reopening".into(), now);
        }
        log.write("source 3: stream failed; reopening".into(), start + Duration::from_secs(61));
        assert_eq!(
            lines(&log),
            [
                "source 3: stream failed; reopening",
                "source 4: stream failed; reopening",
                "source 3: stream failed; reopening (and 299 more times since it was last logged)",
            ]
        );
    }

    #[test]
    fn the_log_stops_growing_when_full() {
        let mut log = fresh_log("full");
        let start = Instant::now();
        for n in 0..3000 {
            log.write(format!("{n} {}", "x".repeat(500)), start);
        }
        let size = fs::metadata(&log.path).unwrap().len();
        assert!((MAX_BYTES..MAX_BYTES + 1024).contains(&size), "{size} bytes");
        assert_eq!(lines(&log).last().unwrap(), "the log is full: nothing more is logged until Kiku restarts");
        assert!(log.recent.len() < 3000, "messages past the cap aren't kept either");
    }
}
