//! The remote engine's acceptance tests. They drive the real
//! `kiku-engine --serve` on 127.0.0.1. Fast ones need nothing; `#[ignore]`d ones need Parakeet Unified
//! installed (`KIKU_MODELS_DIR` can point at the folder) and run with `--include-ignored`.

use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader},
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use kiku_core::{
    catalog,
    engine::{self, Device, Hotword, Line, SAMPLE_RATE},
    engine_host::{hear, probe_clip},
    remote::{self, HostCode, ModelRequest},
    settings::{EngineWhere, Settings},
};

const MODEL: &str = "parakeet-unified";
/// A well-formed code: 43 base64url characters each, whose last carries no stray bits.
const FINGERPRINT: &str = "ffffffffffffffffffffffffffffffffffffffffffQ";
const SECRET: &str = "ssssssssssssssssssssssssssssssssssssssssssg";
const WRONG: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const QUICK: Duration = Duration::from_secs(5);

/// A running `kiku-engine --serve`, ended when dropped.
struct Host {
    child: Child,
    code: HostCode,
}

impl Host {
    fn start(state: &PathBuf, delay_ms: Option<u64>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kiku-engine"));
        command.args(["--serve", "--listen", "127.0.0.1:0", "--state"]).arg(state);
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        if let Some(delay) = delay_ms {
            command.env("KIKU_HOST_DELAY_MS", delay.to_string());
        }
        let mut child = command.spawn().expect("kiku-engine starts");
        let stdout = child.stdout.take().unwrap();
        let (line_tx, line_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let _ = line_tx.send(line);
            let _ = io::copy(&mut reader, &mut io::sink());
        });
        let line = line_rx.recv_timeout(Duration::from_secs(30)).expect("the host prints its code within 30 s");
        let code = line.trim().strip_prefix("kiku-host ").unwrap_or_else(|| panic!("the host's first line is `kiku-host <code>`, not {line:?}"));
        let code = HostCode::parse(code).expect("the host's code parses");
        Self { child, code }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A fresh folder for a host's key and certificate.
fn state_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("kiku-remote-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    folder
}

fn request(model: &str, hotwords: Vec<Hotword>) -> ModelRequest {
    ModelRequest { model: model.into(), device: Device::Cpu, hotwords }
}

/// Connects and returns the error and how long it took.
fn refusal(code: &HostCode) -> (String, Duration) {
    let started = Instant::now();
    let error = match remote::connect(code, &request(MODEL, Vec::new())) {
        Ok(_) => panic!("connected with {code}"),
        Err(error) => error.to_string().to_lowercase(),
    };
    (error, started.elapsed())
}

fn lull(boost: f32) -> Vec<Hotword> {
    vec![Hotword { text: "Lull".into(), boost }]
}

/// Keeps each line's newest revision.
fn take(lines: &mut BTreeMap<u64, Line>, new: Vec<Line>) {
    for line in new {
        lines.insert(line.id, line);
    }
}

fn finished_text(lines: &BTreeMap<u64, Line>) -> String {
    lines.values().filter(|line| line.complete).map(|line| line.text.as_str()).collect::<Vec<_>>().join(" ")
}

// Fast.

#[test]
fn host_code_round_trips() {
    let text = format!("kiku://127.0.0.1:7300/#{FINGERPRINT}.{SECRET}");
    let code = HostCode::parse(&text).unwrap();
    assert_eq!((code.address(), code.fingerprint(), code.secret()), ("127.0.0.1:7300", FINGERPRINT, SECRET));
    assert_eq!(code.to_string(), text);
    let named = format!("kiku://studio.tail1234.ts.net:7300/#{FINGERPRINT}.{SECRET}");
    assert_eq!(HostCode::parse(&named).unwrap().to_string(), named);
    for garbage in [
        String::new(),
        "kiku://".into(),
        format!("http://127.0.0.1:7300/#{FINGERPRINT}.{SECRET}"),
        format!("kiku://127.0.0.1/#{FINGERPRINT}.{SECRET}"),
        format!("kiku://127.0.0.1:7300/#{FINGERPRINT}"),
        format!("kiku://127.0.0.1:7300/#{FINGERPRINT}.short"),
        format!("kiku://127.0.0.1:notaport/#{FINGERPRINT}.{SECRET}"),
    ] {
        assert!(HostCode::parse(&garbage).is_err(), "{garbage:?} parsed");
    }
    let state = state_folder("round-trip");
    let host = Host::start(&state, None);
    assert_eq!(HostCode::parse(&host.code.to_string()).unwrap(), host.code);
}

#[test]
fn the_code_survives_a_restart() {
    let state = state_folder("restart");
    let first = Host::start(&state, None).code.clone();
    let second = Host::start(&state, None).code.clone();
    assert_eq!((first.fingerprint(), first.secret()), (second.fingerprint(), second.secret()));
    let elsewhere = Host::start(&state_folder("restart-elsewhere"), None).code.clone();
    assert_ne!(elsewhere.secret(), first.secret(), "each host makes its own secret");
}

#[test]
fn wrong_secret_is_refused() {
    let host = Host::start(&state_folder("secret"), None);
    let (error, took) = refusal(&host.code.with_secret(WRONG));
    assert!(error.contains("refused"), "{error}");
    assert!(took < QUICK, "took {took:?}");
}

#[test]
fn wrong_certificate_is_refused() {
    let host = Host::start(&state_folder("certificate"), None);
    let (error, took) = refusal(&host.code.with_fingerprint(WRONG));
    assert!(error.contains("certificate"), "{error}");
    assert!(took < QUICK, "took {took:?}");
}

#[test]
fn nobody_listening_fails_fast() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let code = HostCode::parse(&format!("kiku://127.0.0.1:{port}/#{FINGERPRINT}.{SECRET}")).unwrap();
    let (_, took) = refusal(&code);
    assert!(took < QUICK, "took {took:?}");
}

#[test]
fn settings_keep_the_host() {
    let defaults = Settings::default();
    assert_eq!((defaults.engine_where, defaults.engine_host.as_str(), defaults.host_on), (EngineWhere::ThisPc, "", false));
    let code = format!("kiku://studio.tail1234.ts.net:7300/#{FINGERPRINT}.{SECRET}");
    let settings = Settings { engine_where: EngineWhere::Host, engine_host: code.clone(), host_on: true, ..Settings::default() };
    let read = Settings::parse(&settings.to_text());
    assert_eq!((read.engine_where, read.engine_host.as_str(), read.host_on), (EngineWhere::Host, code.as_str(), true));
    assert_eq!(read, settings);
}

// With Parakeet Unified.

#[test]
#[ignore]
fn remote_hears_what_local_hears() {
    let host = Host::start(&state_folder("hears"), None);
    let remote = remote::connect(&host.code, &request(MODEL, Vec::new())).expect("connects and loads");
    let heard = hear(remote.as_ref(), &probe_clip()).unwrap();
    let model = catalog::by_id(MODEL).expect("in the catalog");
    let local = engine::load(&model.spec(model.variant(Device::Cpu))).unwrap();
    assert_eq!(heard, hear(local.as_ref(), &probe_clip()).unwrap());
    assert!(heard.contains("best of times"), "{heard}");
}

#[test]
#[ignore]
fn remote_hotword_strength_changes_what_is_heard() {
    let host = Host::start(&state_folder("hotwords"), None);
    let engine = remote::connect(&host.code, &request(MODEL, lull(1.5))).expect("connects and loads");
    let heard = || hear(engine.as_ref(), &probe_clip()).unwrap();
    let usual = heard();
    assert!(!usual.contains("Lull"), "{usual}");
    assert!(engine.set_hotwords(&lull(8.0)), "a new strength needs no reload");
    let strong = heard();
    assert!(strong.contains("Lull"), "{strong}");
    assert!(engine.set_hotwords(&lull(1.5)));
    assert_eq!(heard(), usual);
}

#[test]
#[ignore]
fn unknown_model_names_the_hosts_models() {
    let host = Host::start(&state_folder("unknown"), None);
    let error = match remote::connect(&host.code, &request("no-such-model", Vec::new())) {
        Ok(_) => panic!("loaded a model that doesn't exist"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains(MODEL), "{error}");
}

#[test]
#[ignore]
fn poll_never_waits_for_the_network() {
    let host = Host::start(&state_folder("delay"), Some(300));
    let engine = remote::connect(&host.code, &request(MODEL, Vec::new())).expect("connects and loads");
    let mut stream = engine.open_stream().unwrap();
    let mut lines = BTreeMap::new();
    let mut slowest = Duration::ZERO;
    for chunk in probe_clip().chunks(SAMPLE_RATE as usize / 10) {
        stream.push(chunk).unwrap();
        let started = Instant::now();
        let new = stream.poll().unwrap();
        slowest = slowest.max(started.elapsed());
        take(&mut lines, new);
    }
    assert!(slowest < Duration::from_millis(50), "a poll took {slowest:?}");
    take(&mut lines, stream.flush().unwrap());
    let heard = finished_text(&lines);
    assert!(heard.contains("best of times"), "{heard}");
}

#[test]
#[ignore]
fn losing_the_host_is_noticed() {
    let mut host = Host::start(&state_folder("lost"), None);
    let engine = remote::connect(&host.code, &request(MODEL, Vec::new())).expect("connects and loads");
    let mut stream = engine.open_stream().unwrap();
    stream.push(&probe_clip()[..SAMPLE_RATE as usize]).unwrap();
    assert!(!engine.lost());
    host.child.kill().unwrap();
    host.child.wait().unwrap();
    let killed = Instant::now();
    let limit = |started: Instant, what: &str| assert!(started.elapsed() < Duration::from_secs(1), "{what} took {:?}", started.elapsed());
    loop {
        let started = Instant::now();
        let _ = stream.poll();
        limit(started, "a poll");
        let started = Instant::now();
        let _ = stream.push(&[0.0; 1600]);
        limit(started, "a push");
        let started = Instant::now();
        let lost = engine.lost();
        limit(started, "lost()");
        if lost {
            break;
        }
        assert!(killed.elapsed() < Duration::from_secs(3), "still not lost {:?} after the host ended", killed.elapsed());
        thread::sleep(Duration::from_millis(50));
    }
}
