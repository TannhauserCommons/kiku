//! The history screen's finding: its search and SQL boxes, the filters, what they found, and a thread of
//! its own that runs every query, so the panel never waits on the database.

use std::{
    ops::Range,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, SystemTime},
};

use kiku_core::{
    engine,
    history::{self, History, Query, Session, Speaker, StoredLine, Table, SQL_MAX_ROWS},
    transcripts,
};

use super::field::Field;

/// Lines found per page; scrolling near the end of the results asks for the next.
const PAGE: usize = 200;
/// The most lines copied or sent to Claude at once.
const EXPORT_MAX: usize = 10_000;
/// A result's text cut to show its first match starts this many characters before it.
const SNIPPET_LEAD: usize = 24;

/// How far back a search looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Period {
    Today,
    Week,
    #[default]
    All,
}

impl Period {
    pub const ALL: [Period; 3] = [Period::Today, Period::Week, Period::All];

    pub fn name(self) -> &'static str {
        match self {
            Period::Today => "Today",
            Period::Week => "7 days",
            Period::All => "All",
        }
    }

    fn since(self) -> Option<SystemTime> {
        match self {
            Period::Today => Some(transcripts::start_of_day(0)),
            Period::Week => Some(transcripts::start_of_day(6)),
            Period::All => None,
        }
    }
}

/// Whose lines a search looks at.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Who {
    #[default]
    Everyone,
    /// One speaker's own lines: you, or an app.
    Speaker(String),
    /// An app's lines and what you said to it.
    Conversation(String),
}

impl Who {
    pub fn name(&self) -> String {
        match self {
            Who::Everyone => "Everyone".into(),
            Who::Speaker(name) => name.clone(),
            Who::Conversation(app) => format!("{app} + you"),
        }
    }
}

/// The history screen's two text boxes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextBox {
    Search,
    Sql,
}

/// Examples SQL mode offers to run: their names, and what they show.
pub const EXAMPLES: [(&str, &str); 3] = [
    ("Who talked most", "Who said the most today, in lines and words"),
    ("Time per app", "How many minutes each app was heard in the last 7 days"),
    ("Between two times", "Every line said between 9:00 and 12:00 today: edit the times to suit"),
];

/// An example's SQL; the times one is for this morning, to edit.
pub fn example_sql(index: usize) -> String {
    match index {
        0 => "SELECT speakers.name AS speaker, COUNT(*) AS lines, \
              SUM(LENGTH(lines.text) - LENGTH(REPLACE(lines.text, ' ', '')) + 1) AS words \
              FROM lines JOIN speakers ON speakers.id = lines.speaker \
              WHERE lines.at >= unixepoch('now', 'localtime', 'start of day', 'utc') \
              GROUP BY speakers.id ORDER BY words DESC"
            .into(),
        1 => "SELECT speakers.name AS app, COUNT(DISTINCT CAST(lines.at / 60 AS INTEGER)) AS minutes_heard, COUNT(*) AS lines \
              FROM lines JOIN speakers ON speakers.id = lines.speaker \
              WHERE speakers.kind = 'app' AND lines.at >= unixepoch('now', '-7 days') \
              GROUP BY speakers.id ORDER BY minutes_heard DESC"
            .into(),
        _ => {
            let today = transcripts::local_date(SystemTime::now());
            format!(
                "SELECT time(lines.at, 'unixepoch', 'localtime') AS time, speakers.name AS speaker, lines.text \
                 FROM lines JOIN speakers ON speakers.id = lines.speaker \
                 WHERE lines.at BETWEEN unixepoch('{today} 09:00', 'utc') AND unixepoch('{today} 12:00', 'utc') \
                 ORDER BY lines.at"
            )
        }
    }
}

/// What the history screen shows and is asking for.
#[derive(Default)]
pub struct Finder {
    /// SQL mode, instead of the search.
    pub sql_mode: bool,
    pub search: Field,
    pub sql: Field,
    /// The box keys go to, while the panel has the keyboard.
    pub typing: Option<TextBox>,
    pub period: Period,
    pub who: Who,
    /// Everyone heard, for the filter: most recently heard first.
    pub speakers: Vec<Speaker>,
    pub found: Found,
    /// The last SQL's rows, or why it failed.
    pub table: Option<Result<Table, String>>,
    /// SQL is running.
    pub running: bool,
    /// The session whose delete button was pressed once; pressing it again deletes it.
    pub deleting: Option<i64>,
    /// Counts searches, so the results of one typed over since are dropped.
    searches: u64,
    /// The search box's text when the search last started.
    searched: String,
}

/// What the search found so far.
#[derive(Default)]
pub struct Found {
    /// Newest first.
    pub lines: Vec<StoredLine>,
    /// How many lines match in all.
    pub total: usize,
    /// Every match is in `lines`.
    pub complete: bool,
    /// A page is on its way.
    pub loading: bool,
}

impl Finder {
    /// Whether the screen lists results rather than past sessions: something was typed or a filter set.
    pub fn searching(&self) -> bool {
        !self.search.text.trim().is_empty() || self.period != Period::All || self.who != Who::Everyone
    }

    /// Whether the search box holds other text than the search running.
    pub fn text_changed(&self) -> bool {
        self.search.text != self.searched
    }

    /// The filter's choices: everyone, then each speaker, and each app's conversation with you.
    pub fn who_choices(&self) -> Vec<Who> {
        let mut choices = vec![Who::Everyone];
        for speaker in &self.speakers {
            choices.push(Who::Speaker(speaker.name.clone()));
            if !speaker.you {
                choices.push(Who::Conversation(speaker.name.clone()));
            }
        }
        choices
    }

    /// The lines the search and filters ask for; `None` when none is set.
    fn query(&self) -> Option<Query> {
        if !self.searching() {
            return None;
        }
        let text = (!self.search.text.trim().is_empty()).then(|| self.search.text.clone());
        let (speaker, conversation) = match &self.who {
            Who::Everyone => (None, None),
            Who::Speaker(name) => (Some(name.clone()), None),
            Who::Conversation(app) => (None, Some(app.clone())),
        };
        Some(Query { text, prefix: true, speaker, conversation, since: self.period.since(), limit: PAGE, ..Default::default() })
    }

    /// Starts the search over after the text or a filter changed: the request for its first page, if any.
    pub fn search(&mut self) -> Option<Request> {
        self.searches += 1;
        self.searched.clone_from(&self.search.text);
        self.found = Found::default();
        let query = self.query()?;
        self.found.loading = true;
        Some(Request::Find { query, search: self.searches })
    }

    /// The request for the next page of results, unless they are all here or on their way.
    pub fn more(&mut self) -> Option<Request> {
        let oldest = self.found.lines.last()?.id;
        if self.found.complete || self.found.loading {
            return None;
        }
        self.found.loading = true;
        Some(Request::Find { query: Query { before: Some(oldest), ..self.query()? }, search: self.searches })
    }

    /// The request for every result (up to [`EXPORT_MAX`]) as text, to copy or `share`.
    pub fn export(&self, share: bool) -> Option<Request> {
        Some(Request::Export { query: Query { limit: EXPORT_MAX, ..self.query()? }, share })
    }

    /// Takes in a page of results, unless the search has changed since it was asked for.
    pub fn take_page(&mut self, search: u64, page: Vec<StoredLine>, total: Option<usize>) {
        if search != self.searches {
            return;
        }
        let found = &mut self.found;
        found.complete = page.len() < PAGE;
        found.lines.extend(page);
        found.total = total.unwrap_or(found.total);
        found.loading = false;
    }

    /// Where the search words are in a result's text, as UTF-16 ranges of the text `snippet` gives.
    pub fn matches(&self, text: &str) -> Vec<Range<u32>> {
        history::matched_words(text, &self.search.text, true).into_iter().map(|range| utf16_range(text, range)).collect()
    }

    /// A result's text, cut to show its first match when that ends past the first `fits` characters: from
    /// a word shortly before it, after "…".
    pub fn snippet<'t>(&self, text: &'t str, fits: usize) -> std::borrow::Cow<'t, str> {
        let Some(Range { start: first, end }) = history::matched_words(text, &self.search.text, true).into_iter().next() else { return text.into() };
        if text[..end].chars().count() <= fits || text[..first].chars().count() <= SNIPPET_LEAD {
            return text.into();
        }
        let lead = text[..first].char_indices().rev().nth(SNIPPET_LEAD - 1).map_or(0, |(at, _)| at);
        let start = text[lead..first].find(char::is_whitespace).map_or(lead, |at| lead + at + 1);
        format!("…{}", &text[start..]).into()
    }
}

/// SQL results as text: a header row, then a row each, with tabs between the cells.
pub fn table_text(table: &Table) -> String {
    let row = |cells: &[String]| cells.iter().map(|cell| cell.replace(['\t', '\n'], " ")).collect::<Vec<_>>().join("\t");
    std::iter::once(row(&table.columns)).chain(table.rows.iter().map(|cells| row(cells))).collect::<Vec<_>>().join("\n")
}

/// A byte range of `text` as UTF-16 offsets.
fn utf16_range(text: &str, range: Range<usize>) -> Range<u32> {
    let units = |end: usize| text[..end].encode_utf16().count() as u32;
    units(range.start)..units(range.end)
}

/// A line as one row of text: "[14:05:12] Discord: ...".
pub fn line_text(line: &StoredLine) -> String {
    transcripts::format_history(std::slice::from_ref(line)).lines().last().unwrap_or_default().to_owned()
}

/// E.g. "Discord, Opera  ·  Oct 30, 14:05", or "You" when only the mic was heard.
pub fn session_title(session: &Session) -> String {
    format!("{}  ·  {}", session_apps(session), transcripts::friendly_time(session.started))
}

/// The apps a session heard, or "You" when only the mic was.
pub fn session_apps(session: &Session) -> String {
    if session.apps.is_empty() { "You".to_owned() } else { session.apps.replace(',', ", ") }
}

/// How long a session went on, from its start to its end or its last line: e.g. "1 h 20 min".
pub fn session_length(session: &Session) -> String {
    let end = session.ended.unwrap_or(session.last_line).max(session.last_line);
    let minutes = end.duration_since(session.started).unwrap_or(Duration::ZERO).as_secs() / 60;
    match minutes {
        0 => "under a minute".into(),
        1..60 => format!("{minutes} min"),
        _ => format!("{} h {} min", minutes / 60, minutes % 60),
    }
}

/// A question for the query thread.
pub enum Request {
    /// Past sessions, and everyone heard.
    Sessions,
    Find { query: Query, search: u64 },
    /// A session's lines, with `line` (by id) to show.
    Open { session: i64, line: Option<i64> },
    Sql(String),
    /// Every line a query finds, as text, to copy or share.
    Export { query: Query, share: bool },
    DeleteSession(i64),
}

/// The query thread's answer.
pub enum Reply {
    Sessions(Vec<Session>, Vec<Speaker>),
    Found { search: u64, page: Vec<StoredLine>, total: Option<usize> },
    /// A session's lines as rows to read, and the row of the line asked for.
    Opened { session: i64, title: String, rows: Vec<String>, focus: Option<usize> },
    Table(Result<Table, String>),
    Exported { text: String, lines: usize, share: bool },
    Deleted(usize),
    Failed(String),
}

/// Runs queries on a thread of its own and hands back the answers.
pub struct Searcher {
    requests: Sender<Request>,
    replies: Receiver<Reply>,
}

impl Searcher {
    /// Starts the thread; `wake` is called after each answer, e.g. to post the panel a message.
    pub fn start(wake: impl Fn() + Send + 'static) -> Self {
        let (requests, waiting) = mpsc::channel();
        let (answers, replies) = mpsc::channel();
        thread::Builder::new().name("history".into()).spawn(move || serve(waiting, answers, wake)).expect("can start the history thread");
        Self { requests, replies }
    }

    pub fn ask(&self, request: Request) {
        let _ = self.requests.send(request);
    }

    /// The answers that have come in.
    pub fn replies(&self) -> impl Iterator<Item = Reply> + '_ {
        self.replies.try_iter()
    }
}

/// Answers requests in order until the panel goes, skipping searches that newer ones replaced.
fn serve(requests: Receiver<Request>, replies: Sender<Reply>, wake: impl Fn()) {
    let mut reader = None;
    while let Ok(first) = requests.recv() {
        let waiting: Vec<Request> = std::iter::once(first).chain(requests.try_iter()).collect();
        let is_find = |request: &Request| matches!(request, Request::Find { .. });
        let newest = waiting.iter().rposition(is_find);
        let current = waiting.into_iter().enumerate().filter(|(index, request)| !is_find(request) || Some(*index) == newest);
        for (_, request) in current {
            let reply = answer(&mut reader, request).unwrap_or_else(|error| Reply::Failed(error.to_string()));
            if replies.send(reply).is_err() {
                return;
            }
            wake();
        }
    }
}

/// Answers one request through `reader`, a connection that only reads, opened on first use.
fn answer(reader: &mut Option<History>, request: Request) -> engine::Result<Reply> {
    let history = match reader {
        Some(history) => history,
        None => reader.insert(History::open_read_only(&history::path())?),
    };
    Ok(match request {
        Request::Sessions => Reply::Sessions(history.sessions()?, history.speakers()?),
        Request::Find { query, search } => {
            let total = query.before.is_none().then(|| history.count(&query)).transpose()?;
            let mut page = history.query(&query)?;
            page.reverse();
            Reply::Found { search, page, total }
        }
        Request::Open { session, line } => {
            let found = history.sessions()?.into_iter().find(|s| s.id == session).ok_or("that session has been deleted")?;
            let lines = history.query(&Query { session: Some(session), ..Default::default() })?;
            // A line's row is the last of the text up to it, which may add a date above it.
            let focus = line.and_then(|id| lines.iter().position(|l| l.id == id)).map(|at| transcripts::format_history(&lines[..=at]).lines().count() - 1);
            let rows = transcripts::format_history(&lines).lines().map(str::to_owned).collect();
            Reply::Opened { session, title: session_title(&found), rows, focus }
        }
        Request::Sql(sql) => Reply::Table(history.read_sql(&sql, SQL_MAX_ROWS).map_err(|error| error.to_string())),
        Request::Export { query, share } => {
            let lines = history.query(&query)?;
            Reply::Exported { text: transcripts::format_history(&lines), lines: lines.len(), share }
        }
        // The reader only reads; deleting takes a connection that writes.
        Request::DeleteSession(session) => Reply::Deleted(History::open()?.delete_session(session)?),
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use kiku_core::history::NewLine;

    use super::*;

    fn finder(text: &str) -> Finder {
        let mut finder = Finder::default();
        finder.search.set(text);
        finder
    }

    #[test]
    fn the_search_and_filters_combine_into_one_query() {
        assert!(finder("  ").query().is_none(), "nothing typed or set lists the sessions");
        let mut typed = finder("thermal pa");
        let query = typed.query().unwrap();
        assert_eq!((query.text.as_deref(), query.prefix, query.since, query.limit), (Some("thermal pa"), true, None, PAGE));

        typed.who = Who::Conversation("Discord".into());
        typed.period = Period::Today;
        let query = typed.query().unwrap();
        assert_eq!((query.conversation.as_deref(), query.speaker), (Some("Discord"), None));
        assert_eq!(query.since, Some(transcripts::start_of_day(0)));
        let filtered = Finder { who: Who::Speaker("You".into()), ..Finder::default() }.query().unwrap();
        assert_eq!((filtered.text, filtered.speaker.as_deref()), (None, Some("You")), "a filter alone lists its lines");
    }

    #[test]
    fn pages_follow_on_and_answers_to_old_searches_are_dropped() {
        let mut finder = finder("paste");
        let line = |id| StoredLine { id, session: 1, at: SystemTime::now(), speaker: "Discord".into(), you: false, talking_to: None, text: "paste".into() };
        let Some(Request::Find { search: first, .. }) = finder.search() else { panic!("a search") };
        assert!(finder.more().is_none(), "nothing to follow on from yet");
        finder.take_page(first, (0..PAGE as i64).rev().map(line).collect(), Some(250));
        let Some(Request::Find { query, search }) = finder.more() else { panic!("the next page") };
        assert_eq!((query.before, search), (Some(0), first));
        assert!(finder.more().is_none(), "one page at a time");

        finder.search.set("pastel");
        let Some(Request::Find { search: second, .. }) = finder.search() else { panic!("a search") };
        finder.take_page(first, vec![line(99)], None);
        assert!(finder.found.lines.is_empty() && finder.found.loading, "the old search's page is dropped");
        finder.take_page(second, vec![line(5)], Some(1));
        assert_eq!((finder.found.lines.len(), finder.found.total, finder.found.complete), (1, 1, true));
    }

    #[test]
    fn results_show_from_shortly_before_their_first_match() {
        let finder = finder("paste");
        let long = "We spent most of the evening talking about the build, and then about thermal paste again.";
        let snippet = finder.snippet(long, 60);
        assert!(snippet.starts_with("…") && snippet.ends_with("thermal paste again."), "{snippet}");
        assert!(snippet.chars().count() < 40, "{snippet}");
        assert_eq!(finder.snippet(long, 100), long, "a match that shows anyway leaves the line whole");
        assert_eq!(finder.snippet("thermal paste", 5), "thermal paste", "so does an early one");
        assert_eq!(finder.matches("café paste"), [Range { start: 5, end: 10 }], "ranges count UTF-16 units");
    }

    #[test]
    fn the_examples_run() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-finder-examples.db");
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        let mut history = History::open_at(&path).unwrap();
        let session = history.start_session().unwrap();
        let line = |mic, source: &str, text: &str| NewLine { at: SystemTime::now(), mic, source: source.into(), talking_to: None, text: text.into(), by_hand: false };
        history.add(session, &[line(false, "Discord", "Who has the certificate?"), line(true, "You", "I do.")]).unwrap();
        for (index, (name, _)) in EXAMPLES.iter().enumerate() {
            let table = history.read_sql(&example_sql(index), SQL_MAX_ROWS).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(!table.columns.is_empty());
        }
        assert_eq!(history.read_sql(&example_sql(0), SQL_MAX_ROWS).unwrap().rows[0], ["Discord", "1", "4"]);
    }
}
