//! Every saved caption line, kept in a SQLite database (`%APPDATA%\kiku\transcripts.db`)
//! so it can be searched by speaker, conversation, time and text, here and by the MCP server.
//!
//! Who said what is structured: each line points to a row in `speakers` (you, or an app such as
//! Discord). Your lines also point to the app you were talking to, so each is stored once yet shows up
//! both under you and in that app's conversation.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, params_from_iter, types::ValueRef, Connection, OpenFlags, OptionalExtension, ToSql, TransactionBehavior};

use crate::{engine::Result, paths};

/// The WAL is moved into the database once it holds this many pages (4 KiB each; SQLite's default is 1000)...
const WAL_PAGES: i64 = 256;
/// ...and then cut back to that size, instead of keeping the largest it has been.
const WAL_SIZE_LIMIT: i64 = WAL_PAGES * 4096;
/// Free space is given back to the disk when it is at least this share of the database...
const RECLAIM_FREE_SHARE: f64 = 0.25;
/// ...and the data left is at most this big, so the VACUUM that does it stays quick.
const RECLAIM_MAX_BYTES: i64 = 32 << 20;
/// The most rows SQL typed by an agent or the user brings back...
pub const SQL_MAX_ROWS: usize = 300;
/// ...and how long it may run before it is stopped.
pub const SQL_TIMEOUT: Duration = Duration::from_secs(5);

/// A finished line ready to store.
#[derive(Clone, Debug, PartialEq)]
pub struct NewLine {
    pub at: SystemTime,
    /// Said into your microphone, as opposed to played by an app.
    pub mic: bool,
    /// The app's name, or "You" for the mic.
    pub source: String,
    /// For mic lines, the app you were talking to, if one had spoken recently.
    pub talking_to: Option<String>,
    pub text: String,
    /// Saved with a save button, so kept when old lines are deleted.
    pub by_hand: bool,
}

/// A stored line, as queries return it.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredLine {
    /// For reading further back from this line with [`Query::before`].
    pub id: i64,
    pub session: i64,
    pub at: SystemTime,
    /// The speaker's name: an app's, or "You".
    pub speaker: String,
    /// Said by you, into your microphone.
    pub you: bool,
    /// For your lines, the app you were talking to.
    pub talking_to: Option<String>,
    pub text: String,
}

/// Someone heard: you, or an app.
#[derive(Clone, Debug, PartialEq)]
pub struct Speaker {
    pub id: i64,
    pub name: String,
    pub you: bool,
    pub lines: i64,
    pub last_heard: Option<SystemTime>,
}

/// One run of the app.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub id: i64,
    pub started: SystemTime,
    pub ended: Option<SystemTime>,
    pub lines: i64,
    /// The apps heard, comma-separated.
    pub apps: String,
    /// When its last line was said.
    pub last_line: SystemTime,
}

/// The result of [`History::read_sql`]: column names, rows as text, and whether rows were left out.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub more: bool,
}

/// Which lines a query returns; empty fields don't filter. Results are oldest first.
#[derive(Clone, Debug, Default)]
pub struct Query {
    /// Only this speaker's lines, by name ("You", "Discord") or speaker id.
    pub speaker: Option<String>,
    /// An app's conversation with you: its lines plus what you said to it, by name or speaker id.
    pub conversation: Option<String>,
    pub since: Option<SystemTime>,
    pub session: Option<i64>,
    /// Full-text search, e.g. `thermal paste` or `"thermal paste"`.
    pub text: Option<String>,
    /// The last word of `text` may be the start of a longer word, as while it is being typed.
    pub prefix: bool,
    /// Only lines that come before this line (by id) in time: the next page back.
    pub before: Option<i64>,
    /// The newest this many lines; 0 for all.
    pub limit: usize,
}

pub struct History {
    connection: Connection,
}

impl History {
    /// Opens (creating if needed) the database the app and the MCP server share.
    pub fn open() -> Result<Self> {
        Self::open_at(&path())
    }

    /// Opens an existing database only to read it, as the MCP server does: nothing is created or migrated.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        connection.busy_timeout(Duration::from_secs(2))?;
        Ok(Self { connection })
    }

    pub fn open_at(path: &Path) -> Result<Self> {
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)?;
        }
        let mut connection = Connection::open(path)?;
        // The app writes while the MCP server reads; WAL lets them do both at once.
        connection.pragma_update(None, "journal_mode", "WAL")?;
        // SQLite's advice for WAL: a power cut may lose the last commits but can't corrupt the database.
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "wal_autocheckpoint", WAL_PAGES)?;
        connection.pragma_update(None, "journal_size_limit", WAL_SIZE_LIMIT)?;
        connection.busy_timeout(Duration::from_secs(2))?;
        migrate(&mut connection)?;
        Ok(Self { connection })
    }

    /// Starts recording a run of the app; returns its session id.
    pub fn start_session(&self) -> Result<i64> {
        self.connection.execute("INSERT INTO sessions (started) VALUES (?1)", [seconds(SystemTime::now())])?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn end_session(&self, session: i64) -> Result<()> {
        self.connection.execute("UPDATE sessions SET ended = ?1 WHERE id = ?2", params![seconds(SystemTime::now()), session])?;
        Ok(())
    }

    /// Stores lines in one transaction, adding speakers heard for the first time.
    pub fn add(&mut self, session: i64, lines: &[NewLine]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        {
            let mut insert =
                transaction.prepare_cached("INSERT INTO lines (session, at, speaker, talking_to, text, by_hand) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
            for line in lines {
                let speaker = speaker_id(&transaction, line.mic, &line.source)?;
                let talking_to = line.talking_to.as_deref().map(|app| speaker_id(&transaction, false, app)).transpose()?;
                insert.execute(params![session, seconds(line.at), speaker, talking_to, line.text, line.by_hand])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn query(&self, query: &Query) -> Result<Vec<StoredLine>> {
        let (filter, mut values) = filter(query);
        let limit = if query.limit == 0 { -1 } else { query.limit as i64 };
        values.push(Box::new(limit));
        // The newest `limit` lines, returned oldest first.
        let sql = format!(
            "SELECT * FROM (
                SELECT lines.session, lines.at, speaker.name, speaker.kind = 'you', listener.name, lines.text, lines.id
                FROM lines
                JOIN speakers AS speaker ON speaker.id = lines.speaker
                LEFT JOIN speakers AS listener ON listener.id = lines.talking_to
                {filter} ORDER BY lines.at DESC, lines.id DESC LIMIT ?
             ) ORDER BY 2, 7"
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values.iter()), |row| {
            Ok(StoredLine {
                id: row.get(6)?,
                session: row.get(0)?,
                at: time(row.get(1)?),
                speaker: row.get(2)?,
                you: row.get(3)?,
                talking_to: row.get(4)?,
                text: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// How many lines match `query`, ignoring its limit: e.g. how many are left before a page.
    pub fn count(&self, query: &Query) -> Result<usize> {
        let (filter, values) = filter(query);
        let sql = format!("SELECT COUNT(*) FROM lines {filter}");
        Ok(self.connection.query_row(&sql, params_from_iter(values.iter()), |row| row.get::<_, i64>(0))? as usize)
    }

    /// Everyone heard so far, most recently heard first.
    pub fn speakers(&self) -> Result<Vec<Speaker>> {
        let mut statement = self.connection.prepare(
            "SELECT speakers.id, speakers.name, speakers.kind = 'you', COUNT(lines.id), MAX(lines.at)
             FROM speakers LEFT JOIN lines ON lines.speaker = speakers.id
             GROUP BY speakers.id ORDER BY MAX(lines.at) DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Speaker {
                id: row.get(0)?,
                name: row.get(1)?,
                you: row.get(2)?,
                lines: row.get(3)?,
                last_heard: row.get::<_, Option<f64>>(4)?.map(time),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Sessions with anything recorded, newest first.
    pub fn sessions(&self) -> Result<Vec<Session>> {
        let mut statement = self.connection.prepare(
            "SELECT sessions.id, sessions.started, sessions.ended, COUNT(lines.id),
                    COALESCE(GROUP_CONCAT(DISTINCT CASE WHEN speakers.kind = 'app' THEN speakers.name END), ''), MAX(lines.at)
             FROM sessions JOIN lines ON lines.session = sessions.id JOIN speakers ON speakers.id = lines.speaker
             GROUP BY sessions.id ORDER BY sessions.started DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Session {
                id: row.get(0)?,
                started: time(row.get(1)?),
                ended: row.get::<_, Option<f64>>(2)?.map(time),
                lines: row.get(3)?,
                apps: row.get(4)?,
                last_line: time(row.get(5)?),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Deletes up to `batch` lines older than `cutoff`, except those saved by hand, and sessions left
    /// empty. Returns how many lines were deleted: a full batch means there may be more.
    pub fn delete_before(&self, cutoff: SystemTime, batch: usize) -> Result<usize> {
        let deleted = self.connection.execute(
            "DELETE FROM lines WHERE id IN (SELECT id FROM lines WHERE at < ?1 AND NOT by_hand LIMIT ?2)",
            params![seconds(cutoff), batch as i64],
        )?;
        self.connection.execute(
            "DELETE FROM sessions WHERE started < ?1 AND NOT EXISTS (SELECT 1 FROM lines WHERE lines.session = sessions.id)",
            [seconds(cutoff)],
        )?;
        Ok(deleted)
    }

    /// Deletes a session's lines, and the session itself once it has ended: a session still running
    /// keeps saving into it. Returns how many lines were deleted.
    pub fn delete_session(&mut self, session: i64) -> Result<usize> {
        let transaction = self.connection.transaction()?;
        let deleted = transaction.execute("DELETE FROM lines WHERE session = ?1", [session])?;
        transaction.execute("DELETE FROM sessions WHERE id = ?1 AND ended IS NOT NULL", [session])?;
        transaction.commit()?;
        Ok(deleted)
    }

    /// Gives the space of deleted lines back to the disk, when that is worth it and quick: see
    /// [`RECLAIM_FREE_SHARE`]. Deleted space is otherwise reused for new lines. Returns whether it did.
    pub fn reclaim_space(&self) -> Result<bool> {
        let pragma = |name: &str| self.connection.pragma_query_value(None, name, |row| row.get::<_, i64>(0));
        let (pages, free, page_size) = (pragma("page_count")?, pragma("freelist_count")?, pragma("page_size")?);
        if (free as f64) < pages as f64 * RECLAIM_FREE_SHARE || (pages - free) * page_size > RECLAIM_MAX_BYTES {
            return Ok(false);
        }
        // VACUUM rewrites every page through the WAL; the checkpoint moves them over and empties it.
        self.connection.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(true)
    }

    /// The most recent session, e.g. the one running now.
    pub fn latest_session(&self) -> Result<Option<i64>> {
        Ok(self.connection.query_row("SELECT MAX(id) FROM sessions", [], |row| row.get(0)).optional()?.flatten())
    }

    /// Runs one SQL statement that only reads, e.g. for an agent's or the user's own questions about
    /// the transcripts: at most `max_rows` rows (never more than [`SQL_MAX_ROWS`]), and stopped after
    /// [`SQL_TIMEOUT`]. Statements that would change anything are refused, and so is more than one.
    pub fn read_sql(&self, sql: &str, max_rows: usize) -> Result<Table> {
        self.read_sql_within(sql, max_rows.min(SQL_MAX_ROWS), SQL_TIMEOUT)
    }

    fn read_sql_within(&self, sql: &str, max_rows: usize, timeout: Duration) -> Result<Table> {
        let mut statement = self.connection.prepare(sql)?;
        if !statement.readonly() {
            return Err("only statements that read are allowed".into());
        }
        let columns: Vec<String> = statement.column_names().map(Into::into).collect();
        let interrupt = self.connection.get_interrupt_handle();
        let (finished, watched) = mpsc::channel::<()>();
        let watchdog = thread::spawn(move || {
            if watched.recv_timeout(timeout) == Err(RecvTimeoutError::Timeout) {
                interrupt.interrupt();
            }
        });
        let mut rows = Vec::new();
        let mut more = false;
        let read = (|| -> rusqlite::Result<()> {
            let mut cursor = statement.query([])?;
            while let Some(row) = cursor.next()? {
                if rows.len() == max_rows {
                    more = true;
                    break;
                }
                rows.push((0..columns.len()).map(|column| row.get_ref(column).map(text_of)).collect::<rusqlite::Result<_>>()?);
            }
            Ok(())
        })();
        drop(finished);
        let _ = watchdog.join();
        drop(statement);
        // SQLite counts BEGIN and SAVEPOINT as reading; left open, the transaction would pin this
        // connection's snapshot (no new lines, a WAL that can't be cut back) until it closes.
        if !self.connection.is_autocommit() {
            self.connection.execute_batch("ROLLBACK")?;
            return Err("transactions aren't allowed: run a single statement that reads".into());
        }
        match read {
            Err(rusqlite::Error::SqliteFailure(failure, _)) if failure.code == rusqlite::ErrorCode::OperationInterrupted => {
                Err(format!("stopped after {} s; narrow the query", timeout.as_secs_f32()).into())
            }
            read => Ok(read.map(|()| Table { columns, rows, more })?),
        }
    }
}

/// A value as text: numbers as written, blobs by size.
fn text_of(value: ValueRef) -> String {
    match value {
        ValueRef::Null => "NULL".into(),
        ValueRef::Integer(number) => number.to_string(),
        ValueRef::Real(number) => number.to_string(),
        ValueRef::Text(text) => String::from_utf8_lossy(text).into_owned(),
        ValueRef::Blob(bytes) => format!("<{} bytes>", bytes.len()),
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    id INTEGER PRIMARY KEY,
    started REAL NOT NULL,
    ended REAL
);
CREATE TABLE IF NOT EXISTS speakers (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('you', 'app')),
    name TEXT NOT NULL COLLATE NOCASE,
    UNIQUE (kind, name)
);
CREATE TABLE IF NOT EXISTS lines (
    id INTEGER PRIMARY KEY,
    session INTEGER NOT NULL REFERENCES sessions(id),
    at REAL NOT NULL,
    speaker INTEGER NOT NULL REFERENCES speakers(id),
    -- For your lines, the app you were talking to.
    talking_to INTEGER REFERENCES speakers(id),
    text TEXT NOT NULL,
    -- Saved with a save button: never deleted for being old.
    by_hand INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS lines_by_time ON lines (at);
CREATE INDEX IF NOT EXISTS lines_by_session ON lines (session, at);
CREATE INDEX IF NOT EXISTS lines_by_speaker ON lines (speaker, at);
CREATE INDEX IF NOT EXISTS lines_by_listener ON lines (talking_to, at);
CREATE VIRTUAL TABLE IF NOT EXISTS lines_search USING fts5 (text, content = 'lines', content_rowid = 'id');
CREATE TRIGGER IF NOT EXISTS lines_searchable AFTER INSERT ON lines BEGIN
    INSERT INTO lines_search (rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER IF NOT EXISTS lines_unsearchable AFTER DELETE ON lines BEGIN
    INSERT INTO lines_search (lines_search, rowid, text) VALUES ('delete', old.id, old.text);
END;
";

/// Schema changes in order: each takes a database from `user_version` = its index to the next.
/// Databases from before versions were recorded are at 0.
const MIGRATIONS: [fn(&Connection) -> rusqlite::Result<()>; 2] = [migrate_to_speakers, add_by_hand];

/// Creates the tables in a new database, or brings an older one up to date, in one transaction.
fn migrate(connection: &mut Connection) -> Result<()> {
    // Immediate: of two threads opening an old database at once, the second waits and finds it migrated.
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: u32 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let pending = MIGRATIONS.get(version as usize..).unwrap_or_default();
    if pending.is_empty() {
        return Ok(());
    }
    let has_lines = transaction.prepare("SELECT 1 FROM sqlite_master WHERE name = 'lines'")?.exists([])?;
    if has_lines {
        pending.iter().try_for_each(|step| step(&transaction))?;
    }
    transaction.execute_batch(SCHEMA)?;
    transaction.pragma_update(None, "user_version", MIGRATIONS.len() as u32)?;
    transaction.commit()?;
    Ok(())
}

/// Moves a database from before speakers existed, when lines held `mic`, `source` and `talking_to` as
/// text, to speaker ids. Line ids are kept, so the search index stays valid.
fn migrate_to_speakers(connection: &Connection) -> rusqlite::Result<()> {
    let old = connection.prepare("SELECT 1 FROM pragma_table_info('lines') WHERE name = 'source'")?.exists([])?;
    if !old {
        return Ok(());
    }
    connection.execute_batch(
        "CREATE TABLE speakers (
             id INTEGER PRIMARY KEY,
             kind TEXT NOT NULL CHECK (kind IN ('you', 'app')),
             name TEXT NOT NULL COLLATE NOCASE,
             UNIQUE (kind, name)
         );
         INSERT OR IGNORE INTO speakers (kind, name)
             SELECT CASE WHEN mic = 1 THEN 'you' ELSE 'app' END, source FROM lines ORDER BY id;
         INSERT OR IGNORE INTO speakers (kind, name) SELECT 'app', talking_to FROM lines WHERE talking_to IS NOT NULL;
         CREATE TABLE lines_with_speakers (
             id INTEGER PRIMARY KEY,
             session INTEGER NOT NULL REFERENCES sessions(id),
             at REAL NOT NULL,
             speaker INTEGER NOT NULL REFERENCES speakers(id),
             talking_to INTEGER REFERENCES speakers(id),
             text TEXT NOT NULL
         );
         INSERT INTO lines_with_speakers (id, session, at, speaker, talking_to, text)
             SELECT lines.id, lines.session, lines.at, speaker.id, listener.id, lines.text
             FROM lines
             JOIN speakers AS speaker ON speaker.kind = CASE WHEN lines.mic = 1 THEN 'you' ELSE 'app' END AND speaker.name = lines.source
             LEFT JOIN speakers AS listener ON listener.kind = 'app' AND listener.name = lines.talking_to;
         DROP TABLE lines;
         ALTER TABLE lines_with_speakers RENAME TO lines;",
    )
}

/// Marks which lines were saved with a save button; earlier lines count as saved automatically.
fn add_by_hand(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch("ALTER TABLE lines ADD COLUMN by_hand INTEGER NOT NULL DEFAULT 0")
}

/// The WHERE clause selecting a query's lines, and its values.
fn filter(query: &Query) -> (String, Vec<Box<dyn ToSql>>) {
    let mut conditions = Vec::new();
    let mut values: Vec<Box<dyn ToSql>> = Vec::new();
    if let Some(speaker) = &query.speaker {
        conditions.push("lines.speaker IN (SELECT id FROM speakers WHERE name = ? OR id = ?)");
        values.extend([Box::new(speaker.clone()) as Box<dyn ToSql>, Box::new(speaker.clone())]);
    }
    if let Some(app) = &query.conversation {
        conditions.push("(lines.speaker IN (SELECT id FROM speakers WHERE kind = 'app' AND (name = ? OR id = ?)) OR lines.talking_to IN (SELECT id FROM speakers WHERE name = ? OR id = ?))");
        values.extend((0..4).map(|_| Box::new(app.clone()) as Box<dyn ToSql>));
    }
    if let Some(since) = query.since {
        conditions.push("lines.at >= ?");
        values.push(Box::new(seconds(since)));
    }
    if let Some(session) = query.session {
        conditions.push("lines.session = ?");
        values.push(Box::new(session));
    }
    if let Some(text) = &query.text {
        conditions.push("lines.id IN (SELECT rowid FROM lines_search WHERE lines_search MATCH ?)");
        values.push(Box::new(search_expression(text, query.prefix)));
    }
    if let Some(before) = query.before {
        // In the order lines are returned: by time, then id.
        conditions.push("(lines.at, lines.id) < (SELECT at, id FROM lines WHERE id = ?)");
        values.push(Box::new(before));
    }
    let filter = if conditions.is_empty() { String::new() } else { format!("WHERE {}", conditions.join(" AND ")) };
    (filter, values)
}

/// The id of a speaker, added if heard for the first time. Your lines (`you`) all share one speaker.
fn speaker_id(connection: &Connection, you: bool, name: &str) -> rusqlite::Result<i64> {
    let kind = if you { "you" } else { "app" };
    connection.execute("INSERT OR IGNORE INTO speakers (kind, name) VALUES (?1, ?2)", params![kind, name])?;
    connection.query_row("SELECT id FROM speakers WHERE kind = ?1 AND name = ?2", params![kind, name], |row| row.get(0))
}

pub fn path() -> PathBuf {
    paths::data_dir().join("transcripts.db")
}

/// Plain words become a search for all of them, quoted so characters like `'` and `-` aren't read as
/// search syntax; with `prefix`, the last also finds longer words it starts. Text with its own quotes,
/// e.g. `"thermal paste"`, is passed through as a phrase search.
fn search_expression(text: &str, prefix: bool) -> String {
    if text.contains('"') {
        return text.to_owned();
    }
    let mut expression = text.split_whitespace().map(|word| format!("\"{word}\"")).collect::<Vec<_>>().join(" ");
    if prefix && !expression.is_empty() && !text.ends_with(char::is_whitespace) {
        expression.push('*');
    }
    expression
}

/// Where a search for `search` (as [`Query::text`], with [`Query::prefix`]) matches in `text`, as byte
/// ranges of whole words, for highlighting them. Words are compared as the search index does: runs of
/// letters and digits, ignoring case.
pub fn matched_words(text: &str, search: &str, prefix: bool) -> Vec<Range<usize>> {
    let wanted: Vec<String> = words(search).map(|(_, word)| word.to_lowercase()).collect();
    let last_is_prefix = prefix && !search.contains('"') && !search.ends_with(char::is_whitespace);
    let matches = |word: &str| {
        let word = word.to_lowercase();
        wanted.iter().enumerate().any(|(index, w)| *w == word || (last_is_prefix && index == wanted.len() - 1 && word.starts_with(w.as_str())))
    };
    words(text).filter(|(_, word)| matches(word)).map(|(start, word)| start..start + word.len()).collect()
}

/// The runs of letters and digits in `text`, with where each starts.
fn words(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).map(move |word| (word.as_ptr() as usize - text.as_ptr() as usize, word))
}

/// Times are stored as seconds since 1970 (UTC).
fn seconds(time: SystemTime) -> f64 {
    time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

fn time(seconds: f64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs_f64(seconds.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(name: &str) -> History {
        History::open_at(&fresh_path(name)).unwrap()
    }

    fn fresh_path(name: &str) -> PathBuf {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").join(format!("test-{name}.db"));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        path
    }

    fn line(at: SystemTime, mic: bool, source: &str, talking_to: Option<&str>, text: &str) -> NewLine {
        NewLine { at, mic, source: source.into(), talking_to: talking_to.map(Into::into), text: text.into(), by_hand: false }
    }

    fn texts(lines: Vec<StoredLine>) -> Vec<String> {
        lines.into_iter().map(|l| l.text).collect()
    }

    #[test]
    fn lines_can_be_found_by_speaker_by_conversation_and_by_text() {
        let mut history = database("queries");
        let session = history.start_session().unwrap();
        let now = SystemTime::now();
        let second = |n| now + Duration::from_secs(n);
        history
            .add(session, &[
                line(second(0), false, "Discord", None, "Did you reapply the thermal paste?"),
                line(second(1), true, "You", Some("Discord"), "Not yet, tonight."),
                line(second(2), false, "Opera", None, "Welcome back to the stream."),
                line(second(3), true, "You", None, "Note to self: buy paste."),
            ])
            .unwrap();

        let only = |speaker: &str| history.query(&Query { speaker: Some(speaker.into()), ..Default::default() }).unwrap();
        assert_eq!(texts(only("discord")), ["Did you reapply the thermal paste?"], "just the app, not your replies");
        assert_eq!(texts(only("You")), ["Not yet, tonight.", "Note to self: buy paste."]);
        let conversation = history.query(&Query { conversation: Some("Discord".into()), ..Default::default() }).unwrap();
        assert_eq!(texts(conversation), ["Did you reapply the thermal paste?", "Not yet, tonight."]);

        let speakers = history.speakers().unwrap();
        let names: Vec<(&str, bool, i64)> = speakers.iter().map(|s| (s.name.as_str(), s.you, s.lines)).collect();
        assert_eq!(names, [("You", true, 2), ("Opera", false, 1), ("Discord", false, 1)], "most recently heard first");
        let opera = speakers.iter().find(|s| s.name == "Opera").unwrap().id.to_string();
        assert_eq!(texts(only(&opera)), ["Welcome back to the stream."], "speakers can be named by id too");
        let paste = history.query(&Query { text: Some("paste".into()), ..Default::default() }).unwrap();
        assert_eq!(texts(paste), ["Did you reapply the thermal paste?", "Note to self: buy paste."]);
        let words = history.query(&Query { text: Some("don't buy-paste".into()), ..Default::default() }).unwrap();
        assert!(words.is_empty(), "punctuation is searched for, not read as search syntax");
        let phrase = history.query(&Query { text: Some("\"thermal paste\"".into()), ..Default::default() }).unwrap();
        assert_eq!(texts(phrase), ["Did you reapply the thermal paste?"]);
        let newest = history.query(&Query { limit: 1, ..Default::default() }).unwrap();
        assert_eq!(texts(newest), ["Note to self: buy paste."]);
        assert_eq!(history.sessions().unwrap()[0].apps.split(',').count(), 2);
    }

    #[test]
    fn a_word_being_typed_finds_longer_words_and_matches_are_highlighted() {
        let mut history = database("prefix");
        let session = history.start_session().unwrap();
        history.add(session, &[line(SystemTime::now(), false, "Discord", None, "Thermal paste, thermostat and THERMS.")]).unwrap();
        let typed = |text: &str, prefix| history.query(&Query { text: Some(text.into()), prefix, ..Default::default() }).unwrap().len();
        assert_eq!((typed("therm", false), typed("therm", true)), (0, 1));
        assert_eq!(typed("therm ", true), 0, "a space ends the word");

        let text = "Thermal paste, thermostat and THERMS.";
        let shown = |search: &str, prefix| matched_words(text, search, prefix).into_iter().map(|range| &text[range]).collect::<Vec<_>>();
        assert_eq!(shown("therms PASTE", false), ["paste", "THERMS"], "whole words, any case");
        assert_eq!(shown("paste therm", true), ["Thermal", "paste", "thermostat", "THERMS"]);
        assert_eq!(shown("therm paste", true), ["paste"], "only the last word is a prefix");
        assert_eq!(matched_words("naïve café", "café", false), [Range { start: 7, end: 12 }], "byte ranges of whole characters");
    }

    #[test]
    fn sql_only_reads_one_statement_with_capped_rows_and_time() {
        let mut history = database("sql");
        let session = history.start_session().unwrap();
        let many: Vec<NewLine> = (0..SQL_MAX_ROWS + 5).map(|n| line(time(n as f64), false, "Discord", None, &format!("line {n}"))).collect();
        history.add(session, &many).unwrap();

        let table = history.read_sql("SELECT text AS said FROM lines ORDER BY id LIMIT 2", 10).unwrap();
        assert_eq!((table.columns, table.rows, table.more), (vec!["said".to_owned()], vec![vec!["line 0".to_owned()], vec!["line 1".to_owned()]], false));
        let capped = history.read_sql("SELECT id FROM lines", usize::MAX).unwrap();
        assert_eq!((capped.rows.len(), capped.more), (SQL_MAX_ROWS, true), "never more than the cap");
        assert_eq!(history.read_sql("SELECT 1;  -- trailing comment", 1).unwrap().rows, [["1"]]);

        for sql in ["DELETE FROM lines", "UPDATE lines SET text = ''", "DROP TABLE lines", "SELECT 1; DELETE FROM lines", "INSERT INTO speakers (kind, name) VALUES ('app', 'x')", "BEGIN", "SAVEPOINT s"] {
            assert!(history.read_sql(sql, 1).is_err(), "{sql} must be refused");
        }
        assert!(history.connection.is_autocommit(), "no transaction is left open");
        let forever = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT COUNT(*) FROM n";
        let stopped = history.read_sql_within(forever, 1, Duration::from_millis(200)).unwrap_err();
        assert!(stopped.to_string().starts_with("stopped after 0.2 s"), "{stopped}");
        assert_eq!(history.read_sql("SELECT COUNT(*) FROM lines", 1).unwrap().rows, [[(SQL_MAX_ROWS + 5).to_string()]], "nothing changed");
    }

    #[test]
    fn deleting_a_session_takes_its_lines_and_keeps_a_running_one() {
        let mut history = database("delete-session");
        let (old, running) = (history.start_session().unwrap(), history.start_session().unwrap());
        let now = SystemTime::now();
        history.add(old, &[line(now, false, "Discord", None, "old one"), line(now + Duration::from_secs(90), true, "You", None, "old two")]).unwrap();
        history.add(running, &[line(now, false, "Discord", None, "running")]).unwrap();
        history.end_session(old).unwrap();
        let sessions = history.sessions().unwrap();
        assert!(sessions[1].last_line.duration_since(sessions[1].started).unwrap_or_default() >= Duration::from_secs(89), "the old one ran 90 s");

        assert_eq!(history.delete_session(old).unwrap(), 2);
        assert_eq!(history.delete_session(running).unwrap(), 1);
        assert!(history.sessions().unwrap().is_empty() && history.query(&Query::default()).unwrap().is_empty());
        assert!(history.query(&Query { text: Some("old".into()), ..Default::default() }).unwrap().is_empty(), "they leave the search index too");
        assert_eq!(history.latest_session().unwrap(), Some(running), "the running session keeps saving into its row");
    }

    #[test]
    fn databases_from_before_speakers_are_moved_over() {
        let path = fresh_path("migration");
        let old = Connection::open(&path).unwrap();
        old.execute_batch(
            "CREATE TABLE sessions (id INTEGER PRIMARY KEY, started REAL NOT NULL, ended REAL);
             CREATE TABLE lines (id INTEGER PRIMARY KEY, session INTEGER NOT NULL, at REAL NOT NULL, mic INTEGER NOT NULL,
                                 source TEXT NOT NULL, talking_to TEXT, text TEXT NOT NULL);
             CREATE VIRTUAL TABLE lines_search USING fts5 (text, content = 'lines', content_rowid = 'id');
             INSERT INTO sessions VALUES (1, 100, 200);
             INSERT INTO lines VALUES (7, 1, 101, 0, 'Discord', NULL, 'Joining?'), (8, 1, 102, 1, 'You', 'Discord', 'On my way.');
             INSERT INTO lines_search (lines_search) VALUES ('rebuild');",
        )
        .unwrap();
        drop(old);

        let history = History::open_at(&path).unwrap();
        let lines = history.query(&Query { conversation: Some("Discord".into()), ..Default::default() }).unwrap();
        let shown: Vec<(&str, Option<&str>, &str)> = lines.iter().map(|l| (l.speaker.as_str(), l.talking_to.as_deref(), l.text.as_str())).collect();
        assert_eq!(shown, [("Discord", None, "Joining?"), ("You", Some("Discord"), "On my way.")]);
        let found = history.query(&Query { text: Some("way".into()), ..Default::default() }).unwrap();
        assert_eq!(texts(found), ["On my way."], "the search index still points at the same lines");
    }

    #[test]
    fn databases_from_before_versions_get_lines_saved_by_hand() {
        let path = fresh_path("unversioned");
        let old = Connection::open(&path).unwrap();
        old.execute_batch(
            "CREATE TABLE sessions (id INTEGER PRIMARY KEY, started REAL NOT NULL, ended REAL);
             CREATE TABLE speakers (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, UNIQUE (kind, name));
             CREATE TABLE lines (id INTEGER PRIMARY KEY, session INTEGER NOT NULL, at REAL NOT NULL, speaker INTEGER NOT NULL,
                                 talking_to INTEGER, text TEXT NOT NULL);
             CREATE VIRTUAL TABLE lines_search USING fts5 (text, content = 'lines', content_rowid = 'id');
             INSERT INTO sessions VALUES (1, 100, 200);
             INSERT INTO speakers VALUES (1, 'app', 'Discord');
             INSERT INTO lines VALUES (7, 1, 101, 1, NULL, 'Joining?');
             INSERT INTO lines_search (lines_search) VALUES ('rebuild');",
        )
        .unwrap();
        drop(old);

        let mut history = History::open_at(&path).unwrap();
        let version: u32 = history.connection.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
        assert_eq!(version as usize, MIGRATIONS.len());
        history.add(1, &[NewLine { by_hand: true, ..line(time(102.0), false, "Discord", None, "Kept.") }]).unwrap();
        assert_eq!(history.delete_before(time(1000.0), 10).unwrap(), 1, "older lines count as saved automatically");
        assert_eq!(texts(history.query(&Query::default()).unwrap()), ["Kept."]);
        drop(history);
        History::open_at(&path).expect("opening a migrated database again changes nothing");
    }

    #[test]
    fn old_lines_and_their_empty_sessions_are_deleted_unless_saved_by_hand() {
        let mut history = database("retention");
        let session = history.start_session().unwrap();
        let now = SystemTime::now();
        let old = now - Duration::from_secs(40 * 86_400);
        let kept = NewLine { by_hand: true, ..line(old, false, "Discord", None, "saved by hand") };
        history.add(session, &[line(old, false, "Discord", None, "ancient"), kept, line(now, false, "Discord", None, "recent")]).unwrap();

        assert_eq!(history.delete_before(now - Duration::from_secs(30 * 86_400), 10).unwrap(), 1);
        assert_eq!(texts(history.query(&Query::default()).unwrap()), ["saved by hand", "recent"]);
        let search = history.query(&Query { text: Some("ancient".into()), ..Default::default() }).unwrap();
        assert!(search.is_empty(), "deleted lines leave the search index too");
    }

    #[test]
    fn old_lines_are_deleted_in_batches_and_their_space_given_back() {
        let path = fresh_path("reclaim");
        let mut history = History::open_at(&path).unwrap();
        let session = history.start_session().unwrap();
        let old: Vec<NewLine> = (0..2000).map(|n| line(time(n as f64), false, "Discord", None, &"words ".repeat(20))).collect();
        history.add(session, &old).unwrap();
        history.add(session, &[line(SystemTime::now(), false, "Discord", None, "recent")]).unwrap();
        assert!(!history.reclaim_space().unwrap(), "nothing to give back yet");

        let cutoff = SystemTime::now() - Duration::from_secs(86_400);
        assert_eq!(history.delete_before(cutoff, 1500).unwrap(), 1500, "a full batch");
        assert_eq!(history.delete_before(cutoff, 1500).unwrap(), 500, "the rest");
        let pages = || history.connection.pragma_query_value(None, "page_count", |row| row.get::<_, i64>(0)).unwrap();
        let before = pages();
        assert!(history.reclaim_space().unwrap());
        assert!(pages() < before / 4, "{} pages left of {before}", pages());
        assert_eq!(std::fs::metadata(format!("{}-wal", path.display())).unwrap().len(), 0, "the WAL is emptied too");
        assert_eq!(texts(history.query(&Query::default()).unwrap()), ["recent"]);
    }
}
