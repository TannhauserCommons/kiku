//! The client's engine: a model running on a host. One thread owns the TLS stream, writing what callers
//! queue and reading what the host sends, so `push` and `poll` only touch memory; only `open_stream`,
//! `flush` and `set_hotwords` wait for the host's reply.

use std::{
    collections::{HashMap, VecDeque},
    io::{self, Write},
    net::TcpStream,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Condvar, Mutex, MutexGuard,
    },
    thread,
    time::{Duration, Instant},
};

use rustls::{ClientConnection, StreamOwned};

use super::frames::{Frame, FrameReader, HOTWORDS_ID, LOST_AFTER, PING, PING_AFTER, TICK, WRITE_TIMEOUT};
use crate::{
    engine::{Engine, EngineStream, Hotword, Line, Result},
    engine_host::{decode_lines, encode_hotword, write_frame, CLOSE, ERROR, FLUSH, HOTWORDS, OPEN, POLL, PUSH},
};

/// Audio and requests waiting to be written; a host this far behind is as good as lost.
const MAX_QUEUED_BYTES: usize = 32 << 20;
/// How often a caller waiting for a reply looks again at whether the host is lost.
const WAIT_SLICE: Duration = Duration::from_millis(100);

type Tls = StreamOwned<ClientConnection, TcpStream>;
type Reply = std::result::Result<Vec<u8>, String>;

/// What has come back for one stream (or, under [`HOTWORDS_ID`], for hotword changes), in the order the
/// host answered.
#[derive(Default)]
struct StreamState {
    /// Lines that have arrived and not been handed out.
    lines: Vec<Line>,
    /// A failure that has arrived and not been handed out.
    error: Option<String>,
    /// The requests still unanswered, oldest first.
    waiting: VecDeque<u8>,
    flushes_asked: u64,
    flushes_done: u64,
    /// The answer to an open or a hotwords request.
    answer: Option<Reply>,
}

#[derive(Default)]
struct Shared {
    streams: Mutex<HashMap<u64, StreamState>>,
    changed: Condvar,
    lost: AtomicBool,
    queued_bytes: AtomicUsize,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, HashMap<u64, StreamState>> {
        self.streams.lock().unwrap()
    }

    /// Takes no lock, so callers may hold it; a waiter that misses the wake-up sees the flag within
    /// [`WAIT_SLICE`].
    fn mark_lost(&self) {
        self.lost.store(true, Ordering::Relaxed);
        self.changed.notify_all();
    }

    fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }

    /// Files a reply from the host under the request it answers.
    fn answer(&self, kind: u8, payload: &[u8]) {
        let Some((id, body)) = payload.split_at_checked(8).map(|(id, body)| (u64::from_le_bytes(id.try_into().unwrap()), body)) else { return };
        let reply: Reply = if kind == ERROR { Err(String::from_utf8_lossy(body).into_owned()) } else { Ok(body.to_vec()) };
        let mut streams = self.lock();
        let Some(state) = streams.get_mut(&id) else { return };
        match state.waiting.pop_front() {
            Some(request @ (POLL | FLUSH)) => {
                match reply.and_then(|body| decode_lines(&body).map_err(|e| e.to_string())) {
                    Ok(lines) => state.lines.extend(lines),
                    Err(error) => state.error = Some(error),
                }
                if request == FLUSH {
                    state.flushes_done += 1;
                }
            }
            Some(_) => state.answer = Some(reply),
            None => {}
        }
        self.changed.notify_all();
    }
}

/// A model running on a host.
pub(super) struct RemoteEngine {
    shared: Arc<Shared>,
    outgoing: mpsc::Sender<Frame>,
    next_stream: AtomicU64,
    /// Hotword changes go one at a time, as their replies have no stream to tell them apart.
    hotwords: Mutex<()>,
}

impl RemoteEngine {
    /// Takes over a connection whose model is loaded.
    pub(super) fn start(stream: Tls) -> Result<Self> {
        stream.sock.set_read_timeout(Some(TICK))?;
        stream.sock.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let shared = Arc::new(Shared::default());
        let (outgoing, queue) = mpsc::channel();
        let io_shared = shared.clone();
        thread::Builder::new().name("remote-engine".into()).spawn(move || {
            if run_io(stream, &queue, &io_shared).is_err() {
                io_shared.mark_lost();
            }
        })?;
        Ok(Self { shared, outgoing, next_stream: AtomicU64::new(0), hotwords: Mutex::new(()) })
    }

    fn lost_error(&self) -> Box<dyn std::error::Error + Send + Sync> {
        "the connection to the host was lost".into()
    }

    /// Queues a frame for the host without waiting.
    fn send(&self, kind: u8, payload: Vec<u8>) -> Result<()> {
        if self.shared.is_lost() {
            return Err(self.lost_error());
        }
        let size = payload.len();
        if self.shared.queued_bytes.fetch_add(size, Ordering::Relaxed) > MAX_QUEUED_BYTES {
            self.shared.queued_bytes.fetch_sub(size, Ordering::Relaxed);
            self.shared.mark_lost();
            return Err(self.lost_error());
        }
        self.outgoing.send((kind, payload)).map_err(|_| {
            self.shared.queued_bytes.fetch_sub(size, Ordering::Relaxed);
            self.lost_error()
        })
    }

    /// Queues a request that gets one reply, noting it under `id`; the caller holds the lock so the
    /// notes and the requests stay in the same order.
    fn ask(&self, streams: &mut HashMap<u64, StreamState>, id: u64, kind: u8, payload: Vec<u8>) -> Result<()> {
        self.send(kind, payload)?;
        streams.entry(id).or_default().waiting.push_back(kind);
        Ok(())
    }

    /// Waits until `ready` says what it found in stream `id`, or the host is lost.
    fn wait_for<T>(&self, id: u64, mut streams: MutexGuard<'_, HashMap<u64, StreamState>>, ready: impl Fn(&mut StreamState) -> Option<T>) -> Result<T> {
        loop {
            if let Some(found) = streams.get_mut(&id).and_then(&ready) {
                return Ok(found);
            }
            if self.shared.is_lost() {
                return Err(self.lost_error());
            }
            streams = self.shared.changed.wait_timeout(streams, WAIT_SLICE).unwrap().0;
        }
    }

    /// Asks for an answer from the host and waits for it; an error reply becomes an `Err`.
    fn request(&self, id: u64, kind: u8, payload: Vec<u8>) -> Result<Vec<u8>> {
        let mut streams = self.shared.lock();
        self.ask(&mut streams, id, kind, payload)?;
        self.wait_for(id, streams, |state| state.answer.take())?.map_err(Into::into)
    }
}

/// Writes what is queued, reads what the host sends, and pings when it has been quiet; ends with an
/// error when the connection fails or the host has said nothing for [`LOST_AFTER`].
fn run_io(mut stream: Tls, queue: &mpsc::Receiver<Frame>, shared: &Shared) -> io::Result<()> {
    let mut reader = FrameReader::default();
    let (mut last_sent, mut last_heard) = (Instant::now(), Instant::now());
    loop {
        loop {
            match queue.try_recv() {
                Ok((kind, payload)) => {
                    shared.queued_bytes.fetch_sub(payload.len(), Ordering::Relaxed);
                    write_frame(&mut stream, kind, &payload)?;
                    last_sent = Instant::now();
                }
                Err(mpsc::TryRecvError::Empty) => break,
                // The engine is gone and everything it queued is written.
                Err(mpsc::TryRecvError::Disconnected) => return stream.flush(),
            }
        }
        while let Some((kind, payload)) = reader.next(&mut stream, super::MAX_FRAME)? {
            last_heard = Instant::now();
            shared.answer(kind, &payload);
        }
        if last_heard.elapsed() > LOST_AFTER {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if last_sent.elapsed() > PING_AFTER {
            write_frame(&mut stream, PING, &0u64.to_le_bytes())?;
            last_sent = Instant::now();
        }
    }
}

impl Engine for RemoteEngine {
    fn open_stream(&self) -> Result<Box<dyn EngineStream + '_>> {
        let id = self.next_stream.fetch_add(1, Ordering::Relaxed);
        self.shared.lock().insert(id, StreamState::default());
        let stream = RemoteStream { engine: self, id };
        // A failure drops `stream`, which forgets the id.
        self.request(id, OPEN, id.to_le_bytes().to_vec())?;
        Ok(Box::new(stream))
    }

    fn lost(&self) -> bool {
        self.shared.is_lost()
    }

    fn set_hotwords(&self, words: &[Hotword]) -> bool {
        let _one_at_a_time = self.hotwords.lock().unwrap();
        let lines: Vec<String> = words.iter().map(encode_hotword).collect();
        self.request(HOTWORDS_ID, HOTWORDS, lines.join("\n").into_bytes()).is_ok_and(|reply| reply == [1])
    }
}

struct RemoteStream<'a> {
    engine: &'a RemoteEngine,
    id: u64,
}

impl RemoteStream<'_> {
    /// What has arrived for this stream, or the failure that has.
    fn take(state: &mut StreamState) -> Result<Vec<Line>> {
        if !state.lines.is_empty() {
            return Ok(std::mem::take(&mut state.lines));
        }
        match state.error.take() {
            Some(error) => Err(error.into()),
            None => Ok(Vec::new()),
        }
    }
}

impl EngineStream for RemoteStream<'_> {
    fn push(&mut self, audio: &[f32]) -> Result<()> {
        let mut payload = self.id.to_le_bytes().to_vec();
        payload.extend(super::encode_pcm(audio));
        self.engine.send(PUSH, payload)
    }

    /// Hands out what the host has sent since the last call, and asks it for more unless it is
    /// still answering the last ask.
    fn poll(&mut self) -> Result<Vec<Line>> {
        let mut streams = self.engine.shared.lock();
        if self.engine.shared.is_lost() {
            return Err(self.engine.lost_error());
        }
        let state = streams.get_mut(&self.id).ok_or("no such stream")?;
        // Lines come first; a pending error waits for the next call so none are lost.
        let result = Self::take(state);
        if !state.waiting.contains(&POLL) {
            self.engine.ask(&mut streams, self.id, POLL, self.id.to_le_bytes().to_vec())?;
        }
        result
    }

    /// Waits for the host to finish everything pushed before, and returns all lines not yet handed out.
    fn flush(&mut self) -> Result<Vec<Line>> {
        let mut streams = self.engine.shared.lock();
        self.engine.ask(&mut streams, self.id, FLUSH, self.id.to_le_bytes().to_vec())?;
        let state = streams.get_mut(&self.id).ok_or("no such stream")?;
        state.flushes_asked += 1;
        let mine = state.flushes_asked;
        self.engine.wait_for(self.id, streams, |state| (state.flushes_done >= mine).then(|| Self::take(state)))?
    }
}

impl Drop for RemoteStream<'_> {
    fn drop(&mut self) {
        let _ = self.engine.send(CLOSE, self.id.to_le_bytes().to_vec());
        self.engine.shared.lock().remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_hands_out_lines_before_a_pending_error() {
        let (outgoing, _queue) = mpsc::channel();
        let engine = RemoteEngine { shared: Arc::default(), outgoing, next_stream: AtomicU64::new(1), hotwords: Mutex::new(()) };
        let line = Line { id: 1, text: "hello".into(), complete: true };
        let state = StreamState { lines: vec![line], error: Some("boom".into()), waiting: VecDeque::from([POLL]), ..Default::default() };
        engine.shared.lock().insert(0, state);
        let mut stream = RemoteStream { engine: &engine, id: 0 };
        assert_eq!(stream.poll().unwrap().len(), 1);
        assert_eq!(stream.poll().unwrap_err().to_string(), "boom");
    }
}
