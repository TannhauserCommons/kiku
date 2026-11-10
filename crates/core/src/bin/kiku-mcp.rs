//! An MCP server that lets Claude read Kiku: this session, everything saved in the history
//! database (searchable by app, by you, by time and by words), and whatever you sent with the ✨ button.
//! It only reads: the database is opened read-only, and each answer is a page of at most [`PAGE`]
//! lines that says how to read the next.
//!
//! Speaks JSON-RPC over stdin/stdout, one message per line. Register it with Claude Code:
//! `claude mcp add kiku -- "%LOCALAPPDATA%\Programs\kiku\kiku-mcp.exe"`
//! An optional argument names another database to read, e.g. a copy to try it on.

use std::{
    env,
    fmt::Display,
    fs,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use kiku_core::{
    history::{self, History, Query, Table},
    macros::Customization,
    paths, status, transcripts,
    words::{self, Words},
};
use serde_json::{json, Map, Value};

/// The protocol versions this server speaks, newest first.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// The most lines, rows, sessions or speakers in one answer.
const PAGE: usize = 300;
/// How many come back when a call doesn't say: enough for what was just said, small enough to stay readable.
const DEFAULT_LIMIT: usize = 50;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// A JSON-RPC error's code and message.
type Failure = (i64, String);

const NOTHING_SAVED: &str = "Kiku hasn't saved any captions yet.";
/// A status file this old was left by a Kiku that isn't running any more.
const STATUS_STALE: Duration = Duration::from_secs(2 * status::HEARTBEAT.as_secs() + 10);

fn main() -> io::Result<()> {
    let database = env::args_os().nth(1).map_or_else(history::path, PathBuf::from);
    let mut server = Server::new(database, transcripts::shared_path(), status::path(), paths::custom_dir(), words::path());
    let mut stdout = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        if let Some(reply) = server.handle(&line?) {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// Answers requests through one read-only connection to the database, kept for the whole run.
struct Server {
    database: PathBuf,
    shared: PathBuf,
    status: PathBuf,
    /// `custom\`, with `macros.toml` and `variables.toml`.
    custom: PathBuf,
    /// The word rules file, whose `>` lines are macros too.
    words: PathBuf,
    /// Opened by the first call that needs it, once Kiku has created the database.
    history: Option<History>,
}

impl Server {
    fn new(database: PathBuf, shared: PathBuf, status: PathBuf, custom: PathBuf, words: PathBuf) -> Self {
        Self { database, shared, status, custom, words, history: None }
    }

    /// The reply to one JSON-RPC message; none for notifications and blank lines.
    fn handle(&mut self, line: &str) -> Option<Value> {
        if line.trim().is_empty() {
            return None;
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => return Some(error_reply(Value::Null, (PARSE_ERROR, format!("invalid JSON: {error}")))),
        };
        let id = message.get("id").cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Some(error_reply(id.unwrap_or_default(), (INVALID_REQUEST, "expected a request with a method".into())));
        };
        // Notifications carry no id and get no reply.
        let id = id?;
        Some(match self.respond(method, &message["params"]) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(failure) => error_reply(id, failure),
        })
    }

    fn respond(&mut self, method: &str, params: &Value) -> Result<Value, Failure> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": protocol_version(params["protocolVersion"].as_str()),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "kiku", "version": env!("CARGO_PKG_VERSION") },
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => {
                let tool = params["name"].as_str().unwrap_or_default();
                let arguments = Arguments::parse(tool, &params["arguments"]).map_err(|error| (INVALID_PARAMS, error))?;
                // Failing to read the database is the tool's error, shown to the model, not the protocol's.
                let (text, failed) = match self.call(tool, &arguments) {
                    Ok(text) => (text, false),
                    Err(error) => (format!("Couldn't read the transcripts: {error}"), true),
                };
                Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": failed }))
            }
            method => Err((METHOD_NOT_FOUND, format!("unknown method {method}"))),
        }
    }

    fn call(&mut self, tool: &str, arguments: &Arguments) -> kiku_core::engine::Result<String> {
        if tool == "get_shared" {
            let text = fs::read_to_string(&self.shared).unwrap_or_else(|_| "Nothing has been shared yet.".into());
            return Ok(shared_page(&text, arguments));
        }
        if tool == "get_status" {
            return Ok(status_text(&self.status));
        }
        if let "list_macros" | "check_customization" | "try_phrase" = tool {
            return Ok(self.customization_answer(tool, arguments));
        }
        let Some(history) = self.history()? else { return Ok(NOTHING_SAVED.into()) };
        match tool {
            "get_live_captions" => match history.latest_session()? {
                Some(session) => lines(history, Query { session: Some(session), ..arguments.query() }),
                None => Ok(NOTHING_SAVED.into()),
            },
            "search_transcripts" | "read_session" => lines(history, arguments.query()),
            "list_speakers" => Ok(list(&history.speakers()?, arguments, "speakers", |s| s.id, |s| {
                let kind = if s.you { "the user's mic" } else { "app" };
                let last = s.last_heard.map_or("never".to_owned(), transcripts::friendly_time);
                format!("id {}: {} ({kind}), {} lines, last heard {last}", s.id, s.name, s.lines)
            })),
            "list_sessions" => Ok(list(&history.sessions()?, arguments, "sessions", |s| s.id, |s| {
                let apps = if s.apps.is_empty() { "You only".to_owned() } else { s.apps.replace(',', ", ") };
                format!("id {}: {} ({}), {} lines", s.id, transcripts::friendly_time(s.started), apps, s.lines)
            })),
            "describe_database" => describe(history),
            "query_database" => {
                let sql = arguments.sql.as_deref().unwrap_or_default();
                Ok(table_text(&history.read_sql(sql, arguments.limit)?, arguments.limit))
            }
            tool => Err(format!("{tool} isn't implemented").into()),
        }
    }

    /// A macro tool's answer, from `custom\` as it is now.
    fn customization_answer(&self, tool: &str, arguments: &Arguments) -> String {
        let words = Words::parse(&fs::read_to_string(&self.words).unwrap_or_default());
        let custom = Customization::read(&self.custom, &words);
        match tool {
            "list_macros" => custom.list_macros(),
            "check_customization" => match custom.problems() {
                [] => "No problems: every macro and variable loads.".into(),
                problems => problems.iter().map(|(place, why)| format!("{place}: {why}")).collect::<Vec<_>>().join("\n"),
            },
            _ => custom.try_phrase(arguments.phrase.as_deref().unwrap_or_default(), arguments.app.as_deref().unwrap_or_default()),
        }
    }

    /// The database, opened read-only on first use and kept open; `None` until Kiku has created it.
    fn history(&mut self) -> kiku_core::engine::Result<Option<&History>> {
        if self.history.is_none() && self.database.exists() {
            self.history = Some(History::open_read_only(&self.database)?);
        }
        Ok(self.history.as_ref())
    }
}

fn error_reply(id: Value, (code, message): Failure) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The client's protocol version if this server speaks it, otherwise the newest it does.
fn protocol_version(requested: Option<&str>) -> &'static str {
    PROTOCOL_VERSIONS.into_iter().find(|version| Some(*version) == requested).unwrap_or(PROTOCOL_VERSIONS[0])
}

fn tools() -> Value {
    let speaker = json!({ "type": ["string", "integer"], "description": "Only this speaker's lines: \"You\" (the user's mic), an app such as \"Discord\", or an id from list_speakers" });
    let conversation = json!({ "type": ["string", "integer"], "description": "An app's conversation: its lines plus what the user said to it, by name or id" });
    let minutes = json!({ "type": "number", "exclusiveMinimum": 0, "description": "Only the last this many minutes" });
    let text = json!({ "type": "string", "description": "Words to find, e.g. thermal paste, or \"thermal paste\" for the exact phrase" });
    let before = |description: &str| json!({ "type": "integer", "minimum": 1, "description": description });
    let older_lines = before("Only lines from before this line id, to read further back; each answer says which id to give");
    let limit = json!({ "type": "integer", "minimum": 1, "maximum": PAGE, "description": format!("How many, newest first (default {DEFAULT_LIMIT}, at most {PAGE})") });
    let pages = "Long answers come in pages, each saying how to read the next.";
    json!([
        tool(
            "get_shared",
            &format!("What the user just sent from Kiku with its ✨ button: a line, a selection, an app's transcript or everything. Call this whenever the user mentions something shared or sent from Kiku. {pages}"),
            json!({
                "from_line": { "type": "integer", "minimum": 1, "description": "The line to start from (default 1)" },
                "limit": { "type": "integer", "minimum": 1, "maximum": PAGE, "description": format!("How many lines (default and at most {PAGE})") },
            }),
            &[],
        ),
        tool(
            "get_live_captions",
            &format!("What Kiku has saved so far from the session running now (or the last one): what each app (Discord, a browser, ...) said, and the user's own lines as 'You' (with the app they were talking to). In auto save mode lines are saved a few seconds after they are said; in manual save mode this holds only what the user saved with a save button. The newest lines, oldest first, in local time: by default the last {DEFAULT_LIMIT}. For what is being said now, pass minutes (e.g. 5); for more, raise limit or read further back. {pages}"),
            json!({ "speaker": speaker, "conversation": conversation, "minutes": minutes, "before": older_lines, "limit": limit }),
            &[],
        ),
        tool(
            "search_transcripts",
            &format!("Search every saved line across past sessions, by words, speaker, conversation and time. The newest matches, oldest first. {pages}"),
            json!({ "text": text, "speaker": speaker, "conversation": conversation, "minutes": minutes, "before": older_lines, "limit": limit }),
            &[],
        ),
        tool(
            "list_speakers",
            &format!("Everyone Kiku has heard: the user ('You') and each app, with ids, line counts and when last heard, most recent first. {pages}"),
            json!({ "before": before("Only speakers listed after this speaker id; each answer says which id to give"), "limit": limit }),
            &[],
        ),
        tool(
            "list_sessions",
            &format!("Past runs of Kiku, newest first, with the apps heard and line counts. {pages}"),
            json!({ "before": before("Only sessions older than this session id; each answer says which id to give"), "limit": limit }),
            &[],
        ),
        tool(
            "get_status",
            "What Kiku is doing right now: whether it is running and captioning, the model, whether the user is dictating, the save mode, and each app (and the mic) with whether it is being captured now, when it was last heard and how many lines it has this run. Use it to find the active apps before reading their lines. Live state only, no caption text.",
            json!({}),
            &[],
        ),
        tool(
            "list_macros",
            "The user's voice macros from macros.toml (and words.txt's > lines): each one's name, phrases, apps, modes, step kinds, shortcut, whether it listens without the dictation key and whether it is on. Never what a step pastes or types, a variable's value, or a program's arguments.",
            json!({}),
            &[],
        ),
        tool(
            "check_customization",
            "Every problem in the user's custom folder (macros.toml, variables.toml), each with its file and line and why, never a value. Call it after editing those files to see whether every macro loads.",
            json!({}),
            &[],
        ),
        tool(
            "try_phrase",
            "What saying a phrase while dictating would do: the macro it runs and each of its steps by kind (text is shown as [private], keys by name), or that it would just be dictated, or nothing. Nothing is run.",
            json!({ "phrase": { "type": "string", "description": "What is said, as a whole line" }, "app": { "type": "string", "description": "The app in front, as captions name it, e.g. Discord (default: none)" } }),
            &["phrase"],
        ),
        tool(
            "describe_database",
            "The transcript database's tables and columns, how many lines, sessions and speakers it holds and the time it spans, with what query_database needs to know.",
            json!({}),
            &[],
        ),
        tool(
            "query_database",
            &format!("Run your own read-only SQL (SQLite) on the transcript database, for anything the other tools don't do: counts, grouping, time ranges, joins, any order. One statement that only reads (SELECT or WITH); anything that would change the database is refused. At most `limit` rows come back (default {DEFAULT_LIMIT}, at most {PAGE}), so aggregate or page with LIMIT and OFFSET. Times are Unix seconds: show them with datetime(at, 'unixepoch', 'localtime'). Call describe_database first for the tables."),
            json!({ "sql": { "type": "string", "description": "One SQLite statement that only reads" }, "limit": limit }),
            &["sql"],
        ),
        tool(
            "read_session",
            &format!("The saved lines of one session, by an id from list_sessions: the newest, oldest first. {pages}"),
            json!({ "id": { "type": "integer", "minimum": 1, "description": "The session's id" }, "before": older_lines, "limit": limit }),
            &["id"],
        ),
    ])
}

/// A tool's definition. Every tool only reads.
fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "annotations": { "readOnlyHint": true },
    })
}

/// A tool call's arguments, checked against the tool's input schema.
struct Arguments {
    speaker: Option<String>,
    conversation: Option<String>,
    since: Option<SystemTime>,
    text: Option<String>,
    session: Option<i64>,
    /// Where a page of lines, sessions or speakers starts: after this id.
    before: Option<i64>,
    /// Where a page of shared text starts, from 1.
    from_line: usize,
    /// An agent's own read-only SQL.
    sql: Option<String>,
    /// A phrase to try, and the app it is said in.
    phrase: Option<String>,
    app: Option<String>,
    limit: usize,
}

impl Arguments {
    /// Refuses unknown tools, unknown or missing arguments and values of the wrong type. Numbers may
    /// come as strings, as some clients send them.
    fn parse(tool: &str, arguments: &Value) -> Result<Self, String> {
        let tools = tools();
        let schema = tools.as_array().into_iter().flatten().find(|t| t["name"] == tool).map(|t| &t["inputSchema"]);
        let schema = schema.ok_or_else(|| format!("unknown tool {tool}"))?;
        let none = Map::new();
        let given = match arguments {
            Value::Null => &none,
            Value::Object(given) => given,
            _ => return Err(format!("arguments must be an object, not {arguments}")),
        };
        for required in schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if given.get(required).is_none_or(Value::is_null) {
                return Err(format!("{tool} needs {required}"));
            }
        }
        let mut parsed =
            Self { speaker: None, conversation: None, since: None, text: None, session: None, before: None, from_line: 1, sql: None, phrase: None, app: None, limit: DEFAULT_LIMIT };
        // A share was sent on purpose, so it comes whole unless it's longer than a page.
        if tool == "get_shared" {
            parsed.limit = PAGE;
        }
        for (name, value) in given.iter().filter(|(_, value)| !value.is_null()) {
            if schema["properties"].get(name).is_none() {
                return Err(format!("{tool} takes no argument {name}"));
            }
            match name.as_str() {
                "speaker" => parsed.speaker = Some(name_or_id(name, value)?),
                "conversation" => parsed.conversation = Some(name_or_id(name, value)?),
                "minutes" => parsed.since = minutes_ago(positive(name, value)?),
                "text" => parsed.text = Some(value.as_str().ok_or_else(|| format!("text must be a string, not {value}"))?.to_owned()),
                "sql" => parsed.sql = Some(value.as_str().ok_or_else(|| format!("sql must be a string, not {value}"))?.to_owned()),
                "phrase" => parsed.phrase = Some(value.as_str().ok_or_else(|| format!("phrase must be a string, not {value}"))?.to_owned()),
                "app" => parsed.app = Some(value.as_str().ok_or_else(|| format!("app must be a string, not {value}"))?.to_owned()),
                "id" => parsed.session = Some(whole(name, value)?),
                "before" => parsed.before = Some(whole(name, value)?),
                "from_line" => parsed.from_line = whole(name, value)? as usize,
                "limit" => parsed.limit = (whole(name, value)? as usize).min(PAGE),
                _ => return Err(format!("{name} isn't implemented")),
            }
        }
        Ok(parsed)
    }

    /// The lines these arguments ask for.
    fn query(&self) -> Query {
        Query {
            speaker: self.speaker.clone(),
            conversation: self.conversation.clone(),
            since: self.since,
            session: self.session,
            text: self.text.clone(),
            before: self.before,
            limit: self.limit,
            ..Default::default()
        }
    }
}

/// A number above 0, given as a number or as a string.
fn positive(name: &str, value: &Value) -> Result<f64, String> {
    let number = match value {
        Value::String(text) => text.trim().parse().ok(),
        _ => value.as_f64(),
    };
    number.filter(|n: &f64| n.is_finite() && *n > 0.0).ok_or_else(|| format!("{name} must be a number above 0, not {value}"))
}

/// A whole number above 0, such as an id or a count.
fn whole(name: &str, value: &Value) -> Result<i64, String> {
    let number = positive(name, value)?;
    if number.fract() != 0.0 || number > i64::MAX as f64 {
        return Err(format!("{name} must be a whole number, not {value}"));
    }
    Ok(number as i64)
}

/// The time this many minutes ago; `None`, so no limit, if that is further back than the clock goes.
fn minutes_ago(minutes: f64) -> Option<SystemTime> {
    Duration::try_from_secs_f64(minutes * 60.0).ok().and_then(|ago| SystemTime::now().checked_sub(ago))
}

/// A speaker given as a name or an id, as queries take it.
fn name_or_id(name: &str, value: &Value) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        _ => whole(name, value).map(|id| id.to_string()),
    }
}

/// A page of lines, oldest first, then how many older ones match and how to read them.
fn lines(history: &History, query: Query) -> kiku_core::engine::Result<String> {
    let lines = history.query(&query)?;
    let Some(oldest) = lines.first() else { return Ok("Nothing matching was said.".into()) };
    let older = history.count(&Query { before: Some(oldest.id), ..query })?;
    Ok(transcripts::format_history(&lines) + &next_page(older, "older lines", "before", oldest.id))
}

/// A page of `items`, in their order, starting after the one whose id is `before`; a row each.
fn list<T>(items: &[T], arguments: &Arguments, what: &str, id: impl Fn(&T) -> i64, row: impl Fn(&T) -> String) -> String {
    let start = arguments.before.map_or(0, |before| items.iter().position(|item| id(item) == before).map_or(items.len(), |at| at + 1));
    let page = &items[start..items.len().min(start + arguments.limit)];
    let Some(last) = page.last() else { return format!("No {what}.") };
    let rows: Vec<String> = page.iter().map(row).collect();
    rows.join("\n") + "\n" + &next_page(items.len() - start - page.len(), &format!("more {what}"), "before", id(last))
}

/// A page of the shared text, from `from_line`.
fn shared_page(text: &str, arguments: &Arguments) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = (arguments.from_line - 1).min(lines.len());
    let end = lines.len().min(start + arguments.limit);
    if start == end {
        return "Nothing more was shared.".into();
    }
    lines[start..end].join("\n") + "\n" + &next_page(lines.len() - end, "more lines", "from_line", end + 1)
}

/// The database's shape and size, and how to query it.
fn describe(history: &History) -> kiku_core::engine::Result<String> {
    let schema = history.read_sql(
        "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL AND type IN ('table', 'index') AND name NOT LIKE 'lines_search_%'",
        PAGE,
    )?;
    let size = history.read_sql(
        "SELECT (SELECT COUNT(*) FROM lines), (SELECT COUNT(*) FROM sessions), (SELECT COUNT(*) FROM speakers), \
         datetime(MIN(at), 'unixepoch', 'localtime'), datetime(MAX(at), 'unixepoch', 'localtime') FROM lines",
        1,
    )?;
    let [lines, sessions, speakers, first, last] = size.rows[0].clone().try_into().map_err(|_| "unexpected size query")?;
    let tables: Vec<String> = schema.rows.into_iter().map(|row| row.concat() + ";").collect();
    Ok(format!(
        "{lines} lines in {sessions} sessions from {speakers} speakers, from {first} to {last} (local time).\n\n{}\n\n\
         Notes for query_database:\n\
         - Times (lines.at, sessions.started, sessions.ended) are Unix seconds; show them with datetime(at, 'unixepoch', 'localtime').\n\
         - speakers.kind is 'you' (the user's mic, named \"You\") or 'app'. lines.speaker says who spoke; for the user's lines, lines.talking_to is the app they were talking to.\n\
         - lines.by_hand is 1 for lines saved with a save button.\n\
         - Full-text search: lines.id IN (SELECT rowid FROM lines_search WHERE lines_search MATCH 'words').",
        tables.join("\n")
    ))
}

/// Query results as rows of `|`-separated cells under a header, saying when rows were left out.
fn table_text(table: &Table, limit: usize) -> String {
    if table.rows.is_empty() {
        return format!("No rows. Columns: {}.", table.columns.join(", "));
    }
    let row = |cells: &[String]| cells.iter().map(|cell| cell.replace('\n', " ")).collect::<Vec<_>>().join(" | ");
    let mut text = row(&table.columns);
    for cells in &table.rows {
        text.push('\n');
        text.push_str(&row(cells));
    }
    if table.more {
        text.push_str(&format!("\n\nMore than {limit} rows: aggregate, or page with LIMIT and OFFSET."));
    }
    text
}

/// What the status file says Kiku is doing, or that it isn't running.
fn status_text(path: &Path) -> String {
    let Ok(status) = fs::read_to_string(path).map(|text| serde_json::from_str::<Value>(&text).unwrap_or_default()) else {
        return "Kiku isn't running.".into();
    };
    let updated = status["updated_at"].as_f64().map(seconds_to_time);
    let stale = updated.is_none_or(|at| at.elapsed().unwrap_or_default() > STATUS_STALE);
    let mut text = match (stale, updated) {
        (true, Some(at)) => format!("Kiku isn't running; it was last seen {}. What it was doing then:\n", ago(at)),
        (true, None) => return "Kiku isn't running.".into(),
        (false, _) => String::new(),
    };
    let captioning = if status["captioning"].as_bool() == Some(true) { "on" } else { "stopped" };
    let device = if status["device"] == "gpu" { "GPU" } else { "CPU" };
    text += &format!("Captioning: {captioning}. Model: {} on the {device}", status["model"].as_str().unwrap_or("unknown"));
    if let Some(next) = status["switching_to"].as_str() {
        text += &format!(", switching to {next}");
    }
    text += &format!(". Dictating: {}.\n", if status["dictating"].as_bool() == Some(true) { "yes" } else { "no" });
    text += &match status["save_mode"].as_str() {
        Some("auto") => format!(
            "Save mode: auto ({}), kept {} days.\n",
            if status["saves_only_mic"].as_bool() == Some(true) { "only the user's mic" } else { "every line" },
            status["keep_days"]
        ),
        _ => "Save mode: manual (only what the user saves with a save button).\n".into(),
    };
    let sources = status["sources"].as_array().cloned().unwrap_or_default();
    let describe = |source: &Value| {
        let heard = source["last_heard"].as_f64().map_or("not heard yet".to_owned(), |at| format!("last heard {}", ago(seconds_to_time(at))));
        let kind = if source["kind"] == "mic" { " (the user's mic)" } else { "" };
        format!("- {}{kind}: {heard}, {} lines this run", source["name"].as_str().unwrap_or("?"), source["lines"])
    };
    let (now, earlier): (Vec<&Value>, Vec<&Value>) = sources.iter().partition(|source| source["capturing"].as_bool() == Some(true));
    let earlier: Vec<&Value> = earlier.into_iter().filter(|source| !source["last_heard"].is_null()).collect();
    text += if now.is_empty() { "Capturing nothing now.\n" } else { "Capturing now:\n" };
    text += &now.iter().map(|source| describe(source) + "\n").collect::<String>();
    if !earlier.is_empty() {
        text += "Heard this run, not capturing now:\n";
        text += &earlier.iter().map(|source| describe(source) + "\n").collect::<String>();
    }
    text
}

fn seconds_to_time(seconds: f64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs_f64(seconds.max(0.0))
}

/// E.g. "Oct 30, 14:05 (3 min ago)".
fn ago(at: SystemTime) -> String {
    let minutes = at.elapsed().unwrap_or_default().as_secs() / 60;
    let ago = match minutes {
        0 => "just now".to_owned(),
        1..60 => format!("{minutes} min ago"),
        _ => format!("{} h {} min ago", minutes / 60, minutes % 60),
    };
    format!("{} ({ago})", transcripts::friendly_time(at))
}

/// Says how many more there are and how to read them; nothing when there are none.
fn next_page(more: usize, what: &str, argument: &str, value: impl Display) -> String {
    if more == 0 {
        return String::new();
    }
    format!("\n{more} {what}: to read them, call again with the same arguments and {argument}={value}.")
}

#[cfg(test)]
mod tests {
    use kiku_core::history::NewLine;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").join(format!("test-mcp-{name}.db"));
        for suffix in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(format!("{}{suffix}", path.display()));
        }
        path
    }

    /// A database with one session of `count` lines from Discord, numbered from 1.
    fn database(name: &str, count: usize) -> PathBuf {
        let path = scratch(name);
        let mut history = History::open_at(&path).unwrap();
        let session = history.start_session().unwrap();
        let start = SystemTime::now() - Duration::from_secs(count as u64);
        let line = |n: usize| NewLine {
            at: start + Duration::from_secs(n as u64),
            mic: false,
            source: "Discord".into(),
            talking_to: None,
            text: format!("line {n}"),
            by_hand: false,
        };
        history.add(session, &(1..=count).map(line).collect::<Vec<_>>()).unwrap();
        path
    }

    fn server(database: PathBuf) -> Server {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        Server::new(database, target.join("test-mcp-shared.txt"), target.join("test-mcp-status.json"), target.join("test-mcp-custom"), target.join("test-mcp-words.txt"))
    }

    fn request(server: &mut Server, method: &str, params: Value) -> Value {
        server.handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string()).unwrap()
    }

    /// The text a tool answered, or panics with the error it gave.
    fn call(server: &mut Server, tool: &str, arguments: Value) -> String {
        let reply = request(server, "tools/call", json!({ "name": tool, "arguments": arguments }));
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        reply["result"]["content"][0]["text"].as_str().unwrap().to_owned()
    }

    fn error_code(server: &mut Server, tool: &str, arguments: Value) -> Value {
        request(server, "tools/call", json!({ "name": tool, "arguments": arguments }))["error"]["code"].clone()
    }

    #[test]
    fn follows_the_protocol() {
        let mut server = server(scratch("protocol"));
        let version = |server: &mut Server, asked: &str| request(server, "initialize", json!({ "protocolVersion": asked }))["result"]["protocolVersion"].clone();
        assert_eq!(version(&mut server, "2025-03-26"), "2025-03-26", "a version both speak is kept");
        assert_eq!(version(&mut server, "2999-01-01"), PROTOCOL_VERSIONS[0], "otherwise the newest this server speaks");

        assert_eq!(server.handle("{not json").unwrap()["error"]["code"], PARSE_ERROR);
        assert_eq!(server.handle(r#"{"jsonrpc":"2.0","id":2}"#).unwrap()["error"]["code"], INVALID_REQUEST);
        assert_eq!(request(&mut server, "resources/list", json!({}))["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(server.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#), None);
        assert_eq!(request(&mut server, "ping", Value::Null)["result"], json!({}));

        let tools = request(&mut server, "tools/list", json!({}))["result"]["tools"].clone();
        for tool in tools.as_array().unwrap() {
            assert_eq!(tool["annotations"]["readOnlyHint"], true, "{}", tool["name"]);
            for name in ["id", "before", "limit", "from_line"] {
                if let Some(property) = tool["inputSchema"]["properties"].get(name) {
                    assert_eq!(property["type"], "integer", "{}.{name}", tool["name"]);
                }
            }
        }
    }

    #[test]
    fn refuses_unknown_tools_and_bad_arguments() {
        let mut server = server(database("arguments", 3));
        assert_eq!(error_code(&mut server, "delete_everything", json!({})), INVALID_PARAMS);
        assert_eq!(error_code(&mut server, "read_session", json!({})), INVALID_PARAMS, "id is required");
        assert_eq!(error_code(&mut server, "read_session", json!({ "id": "one" })), INVALID_PARAMS);
        assert_eq!(error_code(&mut server, "read_session", json!({ "id": 1.5 })), INVALID_PARAMS);
        assert_eq!(error_code(&mut server, "search_transcripts", json!({ "limit": 0 })), INVALID_PARAMS);
        assert_eq!(error_code(&mut server, "search_transcripts", json!({ "minutes": -5 })), INVALID_PARAMS);
        assert_eq!(error_code(&mut server, "search_transcripts", json!({ "lmit": 5 })), INVALID_PARAMS, "typos aren't ignored");
        assert_eq!(error_code(&mut server, "read_session", json!({ "id": 1, "speaker": "You" })), INVALID_PARAMS);

        assert!(call(&mut server, "read_session", json!({ "id": "1", "limit": "2" })).ends_with("line 3\n\n1 older lines: to read them, call again with the same arguments and before=2."));
        assert!(call(&mut server, "search_transcripts", json!({ "minutes": "1e9", "limit": 1.0 })).contains("line 3"), "numbers may be strings");
    }

    #[test]
    fn answers_come_in_pages_that_lead_to_the_next() {
        let mut server = server(database("pages", 700));
        let first = call(&mut server, "get_live_captions", json!({}));
        assert_eq!(first.matches("Discord: line").count(), DEFAULT_LIMIT, "a short answer unless a call asks for more");
        assert!(first.ends_with("650 older lines: to read them, call again with the same arguments and before=651."), "{}", &first[first.len() - 120..]);
        let most = call(&mut server, "get_live_captions", json!({ "limit": 1000 }));
        assert_eq!(most.matches("Discord: line").count(), PAGE, "never more than a page");

        let mut seen = Vec::new();
        let mut arguments = json!({ "id": 1, "limit": 250 });
        loop {
            let page = call(&mut server, "read_session", arguments.clone());
            let lines: Vec<&str> = page.lines().filter_map(|l| l.split_once("Discord: ").map(|(_, text)| text)).collect();
            seen.splice(0..0, lines.into_iter().map(String::from));
            let Some((_, next)) = page.rsplit_once("before=") else { break };
            arguments["before"] = json!(next.trim_end_matches('.'));
        }
        assert_eq!(seen, (1..=700).map(|n| format!("line {n}")).collect::<Vec<_>>(), "every line once, in order");

        let sessions = call(&mut server, "list_sessions", json!({ "limit": 1 }));
        assert!(sessions.starts_with("id 1: ") && !sessions.contains("more sessions"), "{sessions}");
        assert_eq!(call(&mut server, "list_sessions", json!({ "before": 1 })), "No sessions.");
    }

    #[test]
    fn agents_can_run_their_own_read_only_sql() {
        let mut server = server(database("sql", 400));
        let counted = call(&mut server, "query_database", json!({ "sql": "SELECT COUNT(*) AS lines FROM lines" }));
        assert_eq!(counted, "lines\n400");
        let capped = call(&mut server, "query_database", json!({ "sql": "SELECT text FROM lines ORDER BY id", "limit": 5 }));
        assert!(capped.starts_with("text\nline 1\n") && capped.contains("line 5\n\nMore than 5 rows"), "{capped}");
        assert!(call(&mut server, "describe_database", json!({})).starts_with("400 lines in 1 sessions from 1 speakers"));

        for sql in ["DELETE FROM lines", "UPDATE lines SET text = ''", "DROP TABLE lines", "SELECT 1; DELETE FROM lines"] {
            let reply = request(&mut server, "tools/call", json!({ "name": "query_database", "arguments": { "sql": sql } }));
            assert_eq!(reply["result"]["isError"], true, "{sql} must be refused");
        }
        assert_eq!(call(&mut server, "query_database", json!({ "sql": "SELECT COUNT(*) FROM lines" })), "COUNT(*)\n400", "nothing was changed");
    }

    #[test]
    fn status_says_what_runs_and_when_kiku_has_stopped() {
        let mut server = server(scratch("status"));
        let now = status::seconds(SystemTime::now());
        let status = |updated: f64| json!({
            "updated_at": updated, "captioning": true, "dictating": false, "model": "Parakeet Unified", "device": "gpu",
            "switching_to": null, "save_mode": "auto", "saves_only_mic": false, "keep_days": 30,
            "sources": [
                { "name": "You", "kind": "mic", "capturing": true, "last_heard": now - 60.0, "lines": 4 },
                { "name": "Discord", "kind": "app", "capturing": true, "last_heard": now, "lines": 12 },
                { "name": "Opera", "kind": "app", "capturing": false, "last_heard": now - 600.0, "lines": 3 },
            ],
        });
        fs::write(&server.status, status(now).to_string()).unwrap();
        let running = call(&mut server, "get_status", json!({}));
        assert!(running.starts_with("Captioning: on. Model: Parakeet Unified on the GPU. Dictating: no."), "{running}");
        assert!(running.contains("Capturing now:\n- You (the user's mic): last heard") && running.contains("- Discord: last heard"), "{running}");
        assert!(running.contains("not capturing now:\n- Opera: last heard"), "{running}");

        fs::write(&server.status, status(now - 3600.0).to_string()).unwrap();
        assert!(call(&mut server, "get_status", json!({})).starts_with("Kiku isn't running; it was last seen"));
        fs::remove_file(&server.status).unwrap();
        assert_eq!(call(&mut server, "get_status", json!({})), "Kiku isn't running.");
    }

    #[test]
    fn macro_tools_list_check_and_try_without_values() {
        let mut server = server(scratch("macros"));
        fs::create_dir_all(&server.custom).unwrap();
        fs::write(server.custom.join("variables.toml"), "email = \"me@example.com\"\n").unwrap();
        fs::write(server.custom.join("macros.toml"), "[[macro]]\nname = \"mail\"\nsay = \"my email\"\nsteps = [ { paste = \"{email}\" }, { press = \"enter\" } ]\n\n[[macro]]\nname = \"bad\"\nsay = \"bad\"\nsteps = [ { fly = \"away\" } ]\n").unwrap();
        let listed = call(&mut server, "list_macros", json!({}));
        assert!(listed.contains("mail") && listed.contains("my email") && listed.contains("paste, press"), "{listed}");
        assert!(!listed.contains("me@example.com") && !listed.contains("{email}"), "{listed}");
        assert!(call(&mut server, "check_customization", json!({})).starts_with("macros.toml:6: "));
        let tried = call(&mut server, "try_phrase", json!({ "phrase": "my email", "app": "Notepad" }));
        assert!(tried.contains("[mail]") && tried.contains("paste [private]") && tried.contains("press Enter"), "{tried}");
        assert!(!tried.contains("me@example.com"), "{tried}");
        assert_eq!(error_code(&mut server, "try_phrase", json!({})), INVALID_PARAMS);
        fs::remove_dir_all(&server.custom).unwrap();
    }

    #[test]
    fn long_shares_come_in_pages_too() {
        let mut server = server(scratch("shared"));
        fs::write(&server.shared, (1..=450).map(|n| format!("shared {n}\n")).collect::<String>()).unwrap();
        let first = call(&mut server, "get_shared", json!({}));
        assert!(first.starts_with("shared 1\n") && first.contains("shared 300\n") && !first.contains("shared 301"));
        assert!(first.ends_with("150 more lines: to read them, call again with the same arguments and from_line=301."));
        let second = call(&mut server, "get_shared", json!({ "from_line": 301 }));
        assert!(second.starts_with("shared 301\n") && second.ends_with("shared 450\n"), "{second}");
        assert_eq!(call(&mut server, "get_shared", json!({ "from_line": 999 })), "Nothing more was shared.");
    }

    #[test]
    fn reads_without_creating_and_opens_once_kiku_has_saved() {
        let path = scratch("later");
        let mut server = server(path.clone());
        assert_eq!(call(&mut server, "search_transcripts", json!({ "text": "line" })), NOTHING_SAVED);
        assert!(!path.exists(), "the reader never creates the database");

        let written = database("later", 2);
        assert_eq!(written, path);
        assert!(call(&mut server, "search_transcripts", json!({ "text": "line" })).contains("line 2"));
        assert!(server.history.as_ref().unwrap().start_session().is_err(), "the connection can't write");

        let mut app = History::open_at(&path).unwrap();
        let line = NewLine { at: SystemTime::now(), mic: true, source: "You".into(), talking_to: None, text: "said later".into(), by_hand: false };
        app.add(1, &[line]).unwrap();
        assert!(call(&mut server, "get_live_captions", json!({})).contains("You: said later"), "the open connection sees new lines");
    }
}
