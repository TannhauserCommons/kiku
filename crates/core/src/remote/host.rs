//! The host side of the remote engine: `kiku-engine --serve`. It keeps a key, a self-signed certificate
//! and a secret in a state folder (made on first run, reused after), listens for TLS 1.3 connections
//! and lets in those that show the secret.

use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    ServerConfig, ServerConnection, StreamOwned,
};

use super::frames::{Frame, FrameReader, HOTWORDS_ID, PING, PING_ID, SILENT_AFTER, TICK, WRITE_TIMEOUT};
use super::{decode_key, decode_pcm, decode_request, ModelRequest, encode_key, fingerprint_of, provider, same_bytes, HostCode, ACCEPTED, KEY_BYTES, MAX_FRAME, TLS13_ONLY};
use crate::{
    catalog,
    engine::{Device, Engine, EngineStream, ModelSpec, Result},
    engine_host::{
        decode_hotwords, encode_lines, payload_lines, read_frame_up_to, write_frame, HostedEngine, CLOSE, ERROR, FLUSH, HOTWORDS, LINES, LOAD, OK, OPEN, POLL, PUSH,
    },
    paths,
};

/// Streams open on one connection at once; an OPEN beyond this is an error.
const MAX_STREAMS: usize = 64;
const DEFAULT_LISTEN: &str = "0.0.0.0:7300";
/// Clients with an accepted secret served at once; more are closed.
const MAX_CONNECTIONS: usize = 4;
/// Connections still shedding TLS and their secret at once; more are closed on arrival.
const MAX_UNKNOWN: usize = 16;
/// A client gets this long in all to finish the handshake and show its secret.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Waited before answering a wrong secret, to slow guessing.
const WRONG_SECRET_PAUSE: Duration = Duration::from_millis(500);

const CERTIFICATE_FILE: &str = "cert.der";
const KEY_FILE: &str = "key.der";
const SECRET_FILE: &str = "secret";

/// Where `--serve` keeps its certificate, key and secret unless `--state` says otherwise.
pub fn default_state_dir() -> PathBuf {
    paths::data_dir().join("host")
}

/// Forgets the secret in `folder`, so the next `--serve` makes a new identity and so a new code.
pub fn forget_secret(folder: &Path) {
    let _ = fs::remove_file(folder.join(SECRET_FILE));
}

/// Runs `--serve [--listen <addr:port>] [--state <folder>]` until the process is ended.
pub fn serve(args: &[String]) -> ExitCode {
    match run(args) {
        Ok(never) => match never {},
        Err(error) => {
            eprintln!("kiku-engine --serve: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<std::convert::Infallible> {
    let (listen, state_folder) = parse_args(args)?;
    let state = State::load_or_create(&state_folder)?;
    let listener = TcpListener::bind(&listen).map_err(|e| format!("can't listen on {listen}: {e}"))?;
    let code = state.code(listener.local_addr()?);

    // The one line the tests and the app read; the secret is shown nowhere else.
    println!("kiku-host {code}");
    io::stdout().flush()?;

    let config = Arc::new(state.tls_config()?);
    let secret = Arc::new(state.secret);
    let (accepted, unknown) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    for tcp in listener.incoming().flatten() {
        let Some(unknown_slot) = Slot::take(&unknown, MAX_UNKNOWN) else { continue };
        let (config, secret, accepted) = (config.clone(), secret.clone(), accepted.clone());
        thread::spawn(move || {
            let _ = handle(tcp, config, &secret, unknown_slot, &accepted);
        });
    }
    Err("the listener stopped".into())
}

fn parse_args(args: &[String]) -> Result<(String, PathBuf)> {
    let (mut listen, mut state) = (DEFAULT_LISTEN.to_string(), default_state_dir());
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--serve" => {}
            "--listen" => listen = value("--listen")?,
            "--state" => state = PathBuf::from(value("--state")?),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    Ok((listen, state))
}

/// One of a counter's limited places, given back when dropped.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(active: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        if active.fetch_add(1, Ordering::SeqCst) < limit {
            Some(Self(active.clone()))
        } else {
            active.fetch_sub(1, Ordering::SeqCst);
            None
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The host's identity: its certificate (DER), the certificate's private key (PKCS#8 DER) and the secret.
struct State {
    certificate: Vec<u8>,
    key: Vec<u8>,
    secret: [u8; KEY_BYTES],
}

impl State {
    /// Reads the state folder, or makes a new identity in it if it holds none (or a damaged one).
    fn load_or_create(folder: &Path) -> Result<Self> {
        if let Some(state) = Self::load(folder) {
            return Ok(state);
        }
        fs::create_dir_all(folder).map_err(|e| format!("can't make {}: {e}", folder.display()))?;
        let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(vec!["kiku".to_string()])?;
        let mut secret = [0u8; KEY_BYTES];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut secret).map_err(|_| "the system has no random numbers")?;
        let state = Self { certificate: cert.der().to_vec(), key: signing_key.serialize_der(), secret };
        write_private(&folder.join(KEY_FILE), &state.key)?;
        write_private(&folder.join(SECRET_FILE), encode_key(&state.secret).as_bytes())?;
        write_private(&folder.join(CERTIFICATE_FILE), &state.certificate)?;
        Ok(state)
    }

    fn load(folder: &Path) -> Option<Self> {
        let certificate = fs::read(folder.join(CERTIFICATE_FILE)).ok()?;
        let key = fs::read(folder.join(KEY_FILE)).ok()?;
        let secret = decode_key(fs::read_to_string(folder.join(SECRET_FILE)).ok()?.trim()).ok()?;
        let state = Self { certificate, key, secret };
        state.tls_config().ok()?;
        Some(state)
    }

    fn tls_config(&self) -> Result<ServerConfig> {
        let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(self.key.clone()));
        Ok(ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(TLS13_ONLY)?
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(self.certificate.clone())], key)?)
    }

    /// The code for a host listening at `listening`; the real port is used even when 0 was asked for.
    fn code(&self, listening: SocketAddr) -> HostCode {
        // A host listening on every address has no one address; its PC's name will do on a LAN, and
        // whoever shares the code may edit it to a Tailscale name or a public address.
        let address = if listening.ip().is_unspecified() { format!("{}:{}", computer_name(), listening.port()) } else { listening.to_string() };
        HostCode { address, fingerprint: fingerprint_of(&self.certificate), secret: encode_key(&self.secret) }
    }
}

fn computer_name() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .or_else(|| fs::read_to_string("/etc/hostname").ok().map(|name| name.trim().to_string()).filter(|name| !name.is_empty()))
        .unwrap_or_else(|| "localhost".into())
}

/// Writes a file only its owner can read (on Unix; Windows' per-user data folder already is).
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path).and_then(|mut file| file.write_all(bytes)).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    Ok(())
}

/// Serves one client: TLS and the secret within one deadline, then its requests. The `unknown` place is
/// given up as soon as the secret has been judged, and only an accepted client takes an accepted one.
fn handle(tcp: TcpStream, config: Arc<ServerConfig>, secret: &[u8; KEY_BYTES], unknown: Slot, accepted: &Arc<AtomicUsize>) -> io::Result<()> {
    tcp.set_nodelay(true)?;
    let watchdog = Watchdog::start(&tcp, HANDSHAKE_TIMEOUT)?;
    let mut stream = StreamOwned::new(ServerConnection::new(config).map_err(io::Error::other)?, tcp);
    let mut shown = [0u8; KEY_BYTES];
    stream.read_exact(&mut shown)?;
    if !same_bytes(&shown, secret) {
        thread::sleep(WRONG_SECRET_PAUSE);
        stream.write_all(b"refused: the host doesn't accept this code's secret\n")?;
        return close(stream);
    }
    drop((watchdog, unknown));
    let Some(_slot) = Slot::take(accepted, MAX_CONNECTIONS) else { return Ok(()) };
    stream.write_all(format!("{ACCEPTED}\n").as_bytes())?;
    stream.flush()?;
    serve_requests(stream)
}

type Tls = StreamOwned<ServerConnection, TcpStream>;

/// Shuts a socket once its time is up, unless dropped first. Read timeouts alone can't bound a client
/// that sends a byte at a time, since each read waits afresh.
struct Watchdog {
    _done: mpsc::Sender<()>,
}

impl Watchdog {
    fn start(tcp: &TcpStream, limit: Duration) -> io::Result<Self> {
        let socket = tcp.try_clone()?;
        let (done, wait) = mpsc::channel();
        thread::spawn(move || {
            if wait.recv_timeout(limit) == Err(mpsc::RecvTimeoutError::Timeout) {
                let _ = socket.shutdown(Shutdown::Both);
            }
        });
        Ok(Self { _done: done })
    }
}

/// What a connected client asks for: first to load a model, which runs in a helper process of its own
/// until the client leaves; then streams, relayed to that engine with the local helper's request kinds.
fn serve_requests(mut stream: Tls) -> io::Result<()> {
    stream.sock.set_read_timeout(Some(SILENT_AFTER))?;
    stream.sock.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let (kind, payload) = read_frame_up_to(&mut stream, MAX_FRAME)?;
    let loaded = match kind {
        LOAD => load(&payload),
        _ => Err("ask for a model first".into()),
    };
    // Declared after the engine, so the streams end before it does.
    let engine = match loaded {
        Ok(engine) => engine,
        Err(error) => return write_frame(&mut stream, ERROR, error.to_string().as_bytes()),
    };
    write_frame(&mut stream, OK, &[])?;
    stream.sock.set_read_timeout(Some(TICK))?;

    let (requests, incoming) = mpsc::channel();
    let (replies, outgoing) = mpsc::channel();
    // The worker runs the model, which may take a while; this thread keeps reading and answering pings.
    thread::scope(|scope| {
        scope.spawn(|| work(&engine, incoming, replies));
        pump(&mut stream, requests, outgoing, reply_delay())
    })
}

/// `KIKU_HOST_DELAY_MS` (tests only): how long each reply waits before it is written.
fn reply_delay() -> Duration {
    let millis = std::env::var("KIKU_HOST_DELAY_MS").ok().and_then(|value| value.parse().ok());
    Duration::from_millis(millis.unwrap_or(0))
}

/// Passes the client's requests to the worker and the worker's replies, each after `delay`, to the client,
/// until either side ends or the client has been silent for [`SILENT_AFTER`]. Pings are answered here.
fn pump(stream: &mut Tls, requests: mpsc::Sender<Frame>, replies: mpsc::Receiver<Frame>, delay: Duration) -> io::Result<()> {
    let mut reader = FrameReader::default();
    let mut waiting: VecDeque<(Instant, Frame)> = VecDeque::new();
    let mut last_heard = Instant::now();
    loop {
        while let Ok(reply) = replies.try_recv() {
            waiting.push_back((Instant::now() + delay, reply));
        }
        while waiting.front().is_some_and(|(due, _)| *due <= Instant::now()) {
            let (_, (kind, payload)) = waiting.pop_front().unwrap();
            write_frame(stream, kind, &payload)?;
        }
        while let Some((kind, payload)) = reader.next(stream, MAX_FRAME)? {
            last_heard = Instant::now();
            if kind == PING {
                waiting.push_back((last_heard + delay, (OK, PING_ID.to_le_bytes().to_vec())));
            } else if requests.send((kind, payload)).is_err() {
                return Ok(());
            }
        }
        if last_heard.elapsed() > SILENT_AFTER {
            return Err(io::ErrorKind::TimedOut.into());
        }
    }
}

/// Runs the client's requests on the engine, one at a time, and queues the replies, each starting with
/// the id of the stream it is for.
fn work(engine: &HostedEngine, requests: mpsc::Receiver<Frame>, replies: mpsc::Sender<Frame>) {
    let mut streams: HashMap<u64, Box<dyn EngineStream + '_>> = HashMap::new();
    // A push has no reply, so its error waits for the stream's next poll.
    let mut failed: HashMap<u64, String> = HashMap::new();
    for (kind, payload) in requests {
        // Hotwords have no stream id: the payload is their lines.
        let (id, reply): (u64, Option<Result<Vec<u8>>>) = if kind == HOTWORDS {
            (HOTWORDS_ID, Some(Ok(vec![engine.set_hotwords(&decode_hotwords(payload_lines(&payload))) as u8])))
        } else {
            let Some(id) = payload.get(..8).map(|b| u64::from_le_bytes(b.try_into().unwrap())) else { return };
            let reply = match kind {
                OPEN if streams.len() >= MAX_STREAMS => Some(Err(format!("a connection may have {MAX_STREAMS} streams open").into())),
                OPEN => Some(engine.open_stream().map(|opened| {
                    streams.insert(id, opened);
                    Vec::new()
                })),
                PUSH => {
                    let audio = decode_pcm(&payload[8..]);
                    if let Some(Err(error)) = streams.get_mut(&id).map(|stream| stream.push(&audio)) {
                        failed.insert(id, error.to_string());
                    }
                    None
                }
                POLL | FLUSH => Some(match (failed.remove(&id), streams.get_mut(&id)) {
                    (Some(error), _) => Err(error.into()),
                    (None, Some(stream)) => if kind == POLL { stream.poll() } else { stream.flush() }.map(|lines| encode_lines(&lines)),
                    (None, None) => Err("no such stream".into()),
                }),
                CLOSE => {
                    streams.remove(&id);
                    failed.remove(&id);
                    None
                }
                _ => Some(Err("unknown request".into())),
            };
            (id, reply)
        };
        let (kind, body) = match reply {
            Some(Ok(body)) => (if kind == POLL || kind == FLUSH { LINES } else { OK }, body),
            Some(Err(error)) => (ERROR, error.to_string().into_bytes()),
            None => continue,
        };
        if replies.send((kind, [&id.to_le_bytes()[..], &body].concat())).is_err() {
            return;
        }
    }
}

/// Starts the model a client asked for and writes one line to stderr: the model, the device, the time and the outcome.
fn load(payload: &[u8]) -> Result<HostedEngine> {
    let request = decode_request(payload)?;
    let started = Instant::now();
    let outcome = start(&request);
    let device = if request.device == Device::Cuda { "GPU" } else { "CPU" };
    let seconds = started.elapsed().as_secs_f32();
    match &outcome {
        Ok(_) => eprintln!("load {} on the {device}: ok in {seconds:.1} s", request.model),
        Err(error) => eprintln!("load {} on the {device}: failed in {seconds:.1} s: {error}", request.model),
    }
    outcome
}

/// Starts the model from this host's own copy, in a helper process of its own.
fn start(request: &ModelRequest) -> Result<HostedEngine> {
    let model = catalog::by_id(&request.model).filter(|model| !model.imported()).ok_or_else(|| not_here(&request.model))?;
    let variant = model.variant(request.device);
    if !variant.installed() {
        return Err(not_here(&request.model).into());
    }
    HostedEngine::start(&ModelSpec { hotwords: request.hotwords.clone(), ..model.spec(variant) })
}

/// Says which models this host has, for a client that asked for one it doesn't.
fn not_here(id: &str) -> String {
    let have: Vec<&str> = catalog::models().into_iter().filter(|m| !m.imported() && m.variants.iter().any(|v| v.installed())).map(|m| m.id.as_str()).collect();
    format!("this host doesn't have the model {id}; it has: {}", if have.is_empty() { "none".to_string() } else { have.join(", ") })
}

fn close(mut stream: StreamOwned<ServerConnection, TcpStream>) -> io::Result<()> {
    stream.conn.send_close_notify();
    stream.flush()?;
    stream.conn.complete_io(&mut stream.sock).map(|_| ())
}
