//! Running the model on another computer, a host: `kiku-engine --serve` there, and an [`Engine`] here
//! that talks to it over TLS, pinned to the host's certificate and keyed by a secret, both in the code
//! the host prints. Kiku is otherwise offline, so this runs only when the user picks another computer.

mod client;
mod frames;
pub mod host;

use std::{
    fmt,
    io::{self, Read, Write},
    net::{Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ring::digest;
use subtle::ConstantTimeEq;
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{ring as ring_provider, CryptoProvider},
    pki_types::{CertificateDer, ServerName, UnixTime},
    ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned,
};

use crate::{
    engine::{Device, Engine, Hotword, Result},
    engine_host::{decode_hotwords, encode_hotword, payload_lines, read_frame_up_to, write_frame, ERROR, LOAD},
};

/// Bytes in a fingerprint and in a secret.
pub(crate) const KEY_BYTES: usize = 32;
/// Connecting (name lookup, TCP, TLS, secret) gives up after this long in all.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// What the host answers when it accepts the secret; anything else is the reason it didn't.
pub(crate) const ACCEPTED: &str = "ok";

/// A host's code, `kiku://<address>:<port>/#<fingerprint>.<secret>`: where it listens, the SHA-256 of its
/// certificate and its secret, each 32 bytes as unpadded base64url (43 characters).
#[derive(Clone, PartialEq, Eq)]
pub struct HostCode {
    address: String,
    fingerprint: String,
    secret: String,
}

impl HostCode {
    /// Reads a code, checking each part: the scheme, `<host>:<port>` and two 32-byte base64url keys.
    pub fn parse(text: &str) -> Result<Self> {
        let rest = text.trim().strip_prefix("kiku://").ok_or("a host code starts with kiku://")?;
        let (address, keys) = rest.split_once("/#").ok_or("a host code has /#<fingerprint>.<secret> after its address")?;
        let (fingerprint, secret) = keys.split_once('.').ok_or("a host code has <fingerprint>.<secret> after /#")?;
        check_address(address)?;
        decode_key(fingerprint).map_err(|e| format!("the code's certificate fingerprint is not valid: {e}"))?;
        decode_key(secret).map_err(|e| format!("the code's secret is not valid: {e}"))?;
        Ok(Self { address: address.into(), fingerprint: fingerprint.into(), secret: secret.into() })
    }

    /// `<host>:<port>`.
    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// The same code reaching the host another way (a LAN, Tailscale or public address).
    pub fn with_address(&self, address: &str) -> Self {
        Self { address: address.into(), ..self.clone() }
    }

    pub fn with_fingerprint(&self, fingerprint: &str) -> Self {
        Self { fingerprint: fingerprint.into(), ..self.clone() }
    }

    pub fn with_secret(&self, secret: &str) -> Self {
        Self { secret: secret.into(), ..self.clone() }
    }
}

impl fmt::Debug for HostCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostCode").field("address", &self.address).field("fingerprint", &self.fingerprint).field("secret", &"<hidden>").finish()
    }
}

impl fmt::Display for HostCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "kiku://{}/#{}.{}", self.address, self.fingerprint, self.secret)
    }
}

fn check_address(address: &str) -> Result<()> {
    let (host, port) = address.rsplit_once(':').ok_or("the code's address needs a port, like host:7300")?;
    if port.parse::<u16>().map_or(true, |port| port == 0) {
        return Err(format!("the code's port {port:?} is not a port number").into());
    }
    let host_ok = match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(v6) => v6.parse::<Ipv6Addr>().is_ok(),
        None => !host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')),
    };
    if host_ok { Ok(()) } else { Err(format!("the code's host {host:?} is not a name or an address").into()) }
}

pub(crate) fn decode_key(text: &str) -> Result<[u8; KEY_BYTES]> {
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| "not 43 base64url characters")?;
    bytes.try_into().map_err(|_| "not 32 bytes".into())
}

pub(crate) fn encode_key(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The SHA-256 of a certificate, as a code carries it.
pub(crate) fn fingerprint_of(certificate: &[u8]) -> String {
    encode_key(digest::digest(&digest::SHA256, certificate).as_ref())
}

/// The model to run on the host: a catalog id and device, picked as this PC would pick its variant.
#[derive(Clone, Debug)]
pub struct ModelRequest {
    pub model: String,
    pub device: Device,
    pub hotwords: Vec<Hotword>,
}

/// How long a load may take on the host: its helper's own 90 s limit, and the network.
const LOAD_REPLY_TIMEOUT: Duration = Duration::from_secs(100);
/// The longest frame the host reads (audio comes in chunks of a fraction of a second).
pub(crate) const MAX_FRAME: usize = 16 << 20;

/// Connects to the host in `code` and loads `request` there; answers once the model heard the test clip.
pub fn connect(code: &HostCode, request: &ModelRequest) -> Result<Box<dyn Engine>> {
    let mut connection = Connection::open(code)?;
    connection.stream.sock.set_read_timeout(Some(LOAD_REPLY_TIMEOUT))?;
    write_frame(&mut connection.stream, LOAD, &encode_request(request)).map_err(tls_error)?;
    let (kind, payload) = read_frame_up_to(&mut connection.stream, MAX_FRAME).map_err(tls_error)?;
    if kind == ERROR {
        return Err(String::from_utf8_lossy(&payload).into_owned().into());
    }
    Ok(Box::new(client::RemoteEngine::start(connection.stream)?))
}

/// A load request: the model's id, the device and the hotwords, a line each.
fn encode_request(request: &ModelRequest) -> Vec<u8> {
    let device = if request.device == Device::Cuda { "cuda" } else { "cpu" };
    let hotwords: String = request.hotwords.iter().map(|word| format!("\n{}", encode_hotword(word))).collect();
    format!("{}\n{device}{hotwords}", request.model).into_bytes()
}

/// The host's side of [`encode_request`].
pub(crate) fn decode_request(payload: &[u8]) -> Result<ModelRequest> {
    let mut parts = payload_lines(payload).into_iter();
    let model = parts.next().ok_or("no model asked for")?;
    let device = if parts.next().as_deref() == Some("cuda") { Device::Cuda } else { Device::Cpu };
    Ok(ModelRequest { model, device, hotwords: decode_hotwords(parts) })
}

/// Audio on the wire: 16-bit little-endian samples.
pub(crate) fn encode_pcm(audio: &[f32]) -> impl Iterator<Item = u8> + '_ {
    audio.iter().flat_map(|sample| ((sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes())
}

pub(crate) fn decode_pcm(bytes: &[u8]) -> Vec<f32> {
    bytes.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b) as f32 / 32768.0).collect()
}

/// A TLS connection to a host that accepted the code's secret; requests go over `stream`.
pub(crate) struct Connection {
    pub(crate) stream: StreamOwned<ClientConnection, TcpStream>,
}

impl Connection {
    /// Connects, checks the host's certificate against the code and shows it the secret, all within
    /// [`CONNECT_TIMEOUT`]. The stream's timeouts are cleared; the caller sets its own.
    pub(crate) fn open(code: &HostCode) -> Result<Self> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let secret = decode_key(code.secret()).map_err(|e| format!("the code's secret is not valid: {e}"))?;
        let fingerprint = decode_key(code.fingerprint()).map_err(|e| format!("the code's certificate fingerprint is not valid: {e}"))?;

        let tcp = tcp_connect(code.address(), deadline)?;
        tcp.set_nodelay(true)?;
        let tls = ClientConnection::new(Arc::new(pinned_config(fingerprint)), ServerName::try_from("kiku")?)?;
        let mut stream = StreamOwned::new(tls, tcp);

        // Each step gets what is left of the deadline.
        stream.sock.set_write_timeout(Some(remaining(deadline)?))?;
        stream.sock.set_read_timeout(Some(remaining(deadline)?))?;
        while stream.conn.is_handshaking() {
            stream.conn.complete_io(&mut stream.sock).map_err(tls_error)?;
        }
        stream.write_all(&secret).map_err(tls_error)?;
        stream.flush().map_err(tls_error)?;

        stream.sock.set_read_timeout(Some(remaining(deadline)?))?;
        let answer = read_line(&mut stream)?;
        if answer != ACCEPTED {
            return Err(if answer.is_empty() { "the host closed the connection".to_string() } else { answer }.into());
        }
        stream.sock.set_read_timeout(None)?;
        stream.sock.set_write_timeout(None)?;
        Ok(Self { stream })
    }
}

fn remaining(deadline: Instant) -> Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() { Err("the host didn't answer in time".into()) } else { Ok(left) }
}

/// Resolves `address` on a thread, since name lookups can't be given a timeout, then tries each address.
fn tcp_connect(address: &str, deadline: Instant) -> Result<TcpStream> {
    let (tx, rx) = mpsc::channel();
    let name = address.to_string();
    thread::spawn(move || {
        let _ = tx.send(name.to_socket_addrs().map(Iterator::collect::<Vec<SocketAddr>>));
    });
    let mut addresses = rx.recv_timeout(remaining(deadline)?).map_err(|_| format!("looking up {address} took too long"))??;
    // A host listens on IPv4 by default, and Windows takes about 2 s to give up on each IPv6 address a
    // PC's name resolves to first, which would use up the deadline.
    addresses.sort_by_key(SocketAddr::is_ipv6);
    let mut last = io::Error::new(io::ErrorKind::NotFound, format!("{address} has no address"));
    for addr in addresses {
        match TcpStream::connect_timeout(&addr, remaining(deadline)?) {
            Ok(tcp) => return Ok(tcp),
            Err(error) => last = error,
        }
    }
    Err(format!("couldn't reach {address}: {last}").into())
}

/// A TLS error's own text: the verifier's reason travels inside an `io::Error`.
fn tls_error(error: io::Error) -> Box<dyn std::error::Error + Send + Sync> {
    match error.get_ref() {
        Some(inner) => inner.to_string().into(),
        None => format!("couldn't talk to the host: {error}").into(),
    }
}

/// Reads up to a newline, without the newline; empty if the host closed first.
fn read_line(stream: &mut impl Read) -> Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8];
    while line.len() < 256 {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => line.push(byte[0]),
            Err(error) => return Err(tls_error(error)),
        }
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

pub(crate) fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring_provider::default_provider())
}

pub(crate) const TLS13_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];

/// Whether two secrets or fingerprints are equal, taking the same time wherever they differ.
pub(crate) fn same_bytes(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

fn pinned_config(fingerprint: [u8; KEY_BYTES]) -> ClientConfig {
    let provider = provider();
    let verifier = Pinned { fingerprint, provider: provider.clone() };
    ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(TLS13_ONLY)
        .expect("TLS 1.3 is in the ring provider")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth()
}

/// Accepts exactly the certificate whose SHA-256 is the code's fingerprint; the handshake's signature is
/// still checked, so only the holder of that certificate's key passes.
#[derive(Debug)]
struct Pinned {
    fingerprint: [u8; KEY_BYTES],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let seen = digest::digest(&digest::SHA256, end_entity.as_ref());
        if same_bytes(seen.as_ref(), &self.fingerprint) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("the host's certificate is not the one in the code".into()))
        }
    }

    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not used".into()))
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_audio_survive_the_network() {
        let request = ModelRequest { model: "parakeet-unified".into(), device: Device::Cuda, hotwords: vec![Hotword { text: "Lull".into(), boost: 2.5 }] };
        let read = decode_request(&encode_request(&request)).unwrap();
        assert_eq!((read.model, read.device, read.hotwords), (request.model, request.device, request.hotwords));
        // 16-bit audio comes back as it went; louder samples are clipped, not wrapped.
        let heard: Vec<f32> = [i16::MIN, -1, 0, 1, 12345, i16::MAX].iter().map(|&v| v as f32 / 32768.0).collect();
        let bytes: Vec<u8> = encode_pcm(&heard).collect();
        assert_eq!(decode_pcm(&bytes), heard);
        assert_eq!(decode_pcm(&encode_pcm(&[2.0, -2.0]).collect::<Vec<_>>()), [32767.0 / 32768.0, -1.0]);
    }
}
