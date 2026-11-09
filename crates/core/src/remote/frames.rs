//! What the client and the host share once connected: frames read a piece at a time (so a socket
//! timeout never loses half a frame), the keepalive and the shape of the host's replies.

use std::{
    io::{self, Read},
    time::Duration,
};

/// A client's request for a sign of life; the host answers with an `OK` reply for [`PING_ID`].
pub(crate) const PING: u8 = 8;
/// The id the host's replies carry when they are for no stream.
pub(crate) const PING_ID: u64 = u64::MAX - 1;
/// The id of the reply to a hotwords request, which has no stream.
pub(crate) const HOTWORDS_ID: u64 = u64::MAX;

/// How long each side waits on its socket before looking at what else needs doing.
pub(crate) const TICK: Duration = Duration::from_millis(10);
/// Nothing sent for this long and the client pings.
pub(crate) const PING_AFTER: Duration = Duration::from_secs(5);
/// Nothing heard from the host for this long and it is counted lost.
pub(crate) const LOST_AFTER: Duration = Duration::from_secs(15);
/// A host drops a client that has been silent this long.
pub(crate) const SILENT_AFTER: Duration = Duration::from_secs(30);
/// A write that can't finish in this long means the other side is gone.
pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// A frame is a kind, a length and a payload.
pub(crate) type Frame = (u8, Vec<u8>);

/// Collects frames from a socket that has a read timeout, keeping what has arrived of the next one.
#[derive(Default)]
pub(crate) struct FrameReader {
    buffer: Vec<u8>,
}

impl FrameReader {
    /// The next whole frame, or `None` when none has arrived yet (after waiting at most the socket's
    /// timeout, and only if one was not already buffered). A frame over `limit` is an error.
    pub(crate) fn next(&mut self, reader: &mut impl Read, limit: usize) -> io::Result<Option<Frame>> {
        if let Some(frame) = self.take(limit)? {
            return Ok(Some(frame));
        }
        let mut chunk = [0u8; 16 << 10];
        match reader.read(&mut chunk) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {}
            Err(error) => return Err(error),
        }
        self.take(limit)
    }

    fn take(&mut self, limit: usize) -> io::Result<Option<Frame>> {
        let Some(header) = self.buffer.get(..5) else { return Ok(None) };
        let length = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
        if length > limit {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too long"));
        }
        if self.buffer.len() < 5 + length {
            return Ok(None);
        }
        let kind = self.buffer[0];
        let payload = self.buffer[5..5 + length].to_vec();
        self.buffer.drain(..5 + length);
        Ok(Some((kind, payload)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gives its pieces one read at a time, with a timeout between them.
    struct Dribble(Vec<Option<Vec<u8>>>);

    impl Read for Dribble {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            match self.0.remove(0) {
                Some(piece) => {
                    out[..piece.len()].copy_from_slice(&piece);
                    Ok(piece.len())
                }
                None => Err(io::ErrorKind::TimedOut.into()),
            }
        }
    }

    #[test]
    fn a_timeout_keeps_half_a_frame() {
        let mut bytes = vec![4u8, 3, 0, 0, 0, b'a', b'b', b'c', 1, 0, 0, 0, 0];
        let tail = bytes.split_off(7);
        let mut source = Dribble(vec![Some(bytes), None, Some(tail)]);
        let mut reader = FrameReader::default();
        assert_eq!(reader.next(&mut source, 100).unwrap(), None);
        assert_eq!(reader.next(&mut source, 100).unwrap(), None);
        assert_eq!(reader.next(&mut source, 100).unwrap(), Some((4, b"abc".to_vec())));
        assert_eq!(reader.next(&mut source, 100).unwrap(), Some((1, Vec::new())));
        assert!(FrameReader { buffer: vec![1, 200, 0, 0, 0] }.next(&mut source, 100).is_err());
    }
}
