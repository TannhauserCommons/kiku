//! Captures one audio source on its own thread, delivered as mono f32 at the engine's sample rate.
//!
//! Windows does the downmix and resampling (`autoconvert`), so no DSP happens here.

use std::{
    sync::{
        mpsc::{Sender, SyncSender, TrySendError},
        Arc,
    },
    thread::JoinHandle,
    time::Instant,
};

use wasapi::{initialize_mta, AudioCaptureClient, AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};
use windows::{
    core::{w, Error},
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_EVENT, WAIT_FAILED, WAIT_OBJECT_0},
        System::Threading::{
            AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, OpenProcess, SetEvent,
            WaitForMultipleObjects, INFINITE, PROCESS_SYNCHRONIZE,
        },
    },
};

use kiku_core::engine::{Result, SAMPLE_RATE};

/// Where captured audio comes from.
#[derive(Clone, Debug)]
pub enum Source {
    /// Everything an app's process tree plays, excluding other apps.
    App { pid: u32 },
    /// The default recording device.
    Microphone,
}

/// A block of captured audio, tagged with the index of the source it came from.
pub struct Chunk {
    pub source: usize,
    pub samples: Vec<f32>,
    /// When it was read from Windows, to notice if decoding falls behind.
    pub captured: Instant,
}

const BYTES_PER_SAMPLE: usize = 4;
/// Audio is handed to the decoder in blocks this long, so it wakes ~10 times a second, not ~100.
const SEND_SAMPLES: usize = SAMPLE_RATE as usize / 10;
/// The capture thread waits on stop, packets ready and the app's process, in that order of precedence;
/// this is the second waking it.
const PACKETS_READY: WAIT_EVENT = WAIT_EVENT(WAIT_OBJECT_0.0 + 1);

/// A running capture: [`Capture::stop`] ends it; join the thread to learn why it ended.
pub struct Capture {
    stop: Arc<KernelHandle>,
    pub thread: JoinHandle<Result<()>>,
}

impl Capture {
    /// Ends the capture; its thread finishes right away.
    pub fn stop(&self) {
        let _ = unsafe { SetEvent(self.stop.0) };
    }
}

/// Where a capture hands its chunks: a `Sender`, or a bounded `SyncSender`, which drops a chunk when
/// the decoder is that far behind rather than block the capture thread.
pub trait Sink: Send + 'static {
    /// Hands over a chunk; false once nobody receives them.
    fn deliver(&self, chunk: Chunk) -> bool;
}

impl Sink for Sender<Chunk> {
    fn deliver(&self, chunk: Chunk) -> bool {
        self.send(chunk).is_ok()
    }
}

impl Sink for SyncSender<Chunk> {
    fn deliver(&self, chunk: Chunk) -> bool {
        !matches!(self.try_send(chunk), Err(TrySendError::Disconnected(_)))
    }
}

/// Starts capturing `source` and sends its audio to `sink` until stopped, the receiver is dropped,
/// or the app exits.
pub fn spawn(index: usize, source: Source, sink: impl Sink) -> Capture {
    spawn_notifying(index, source, sink, || ())
}

/// Like [`spawn`], and calls `ended` on the capture thread once the capture ends, however it ends.
pub fn spawn_notifying(index: usize, source: Source, sink: impl Sink, ended: impl FnOnce() + Send + 'static) -> Capture {
    // A failed event is a null handle, which makes the first wait fail and the capture end with that error.
    let stop = Arc::new(KernelHandle(unsafe { CreateEventW(None, true, false, None) }.unwrap_or_default()));
    let stop_signal = stop.clone();
    let thread = std::thread::spawn(move || {
        let result = capture_loop(index, &source, &sink, &stop_signal);
        ended();
        result
    });
    Capture { stop, thread }
}

fn capture_loop(index: usize, source: &Source, sink: &impl Sink, stop: &KernelHandle) -> Result<()> {
    initialize_mta().ok()?;
    let _priority = AudioPriority::claim();
    let mut client = open_client(source)?;
    let format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE as usize, 1, None);
    // Event-driven: per-app (process loopback) capture keeps only a small buffer, so timed reads lose audio.
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 };
    client.initialize_client(&format, &Direction::Capture, &mode)?;
    let ready = client.set_get_eventhandle()?;
    let capture = client.get_audiocaptureclient()?;
    let mut wake_on = vec![stop.0, ready.as_handle()];
    // An app's capture ends with the app. Should its process not open, discovery notices the exit instead.
    let app = match source {
        Source::App { pid } => unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, *pid) }.ok().map(KernelHandle),
        Source::Microphone => None,
    };
    wake_on.extend(app.as_ref().map(|process| process.0));
    client.start_stream()?;

    let (mut packet, mut samples) = (Vec::new(), Vec::new());
    loop {
        match unsafe { WaitForMultipleObjects(&wake_on, false, INFINITE) } {
            PACKETS_READY => read_packets(&capture, &mut packet, &mut samples)?,
            WAIT_FAILED => return Err(Error::from_thread().into()),
            // Stopped, or the app exited.
            _ => break,
        }
        if samples.len() >= SEND_SAMPLES {
            let chunk = Chunk { source: index, samples: std::mem::take(&mut samples), captured: Instant::now() };
            if !sink.deliver(chunk) {
                break;
            }
        }
    }
    client.stop_stream()?;
    Ok(())
}

/// The id of Windows' default recording device, which [`Source::Microphone`] captures; `None` when
/// there is none. Needs COM initialized on this thread.
pub fn default_microphone() -> Option<String> {
    DeviceEnumerator::new().ok()?.get_default_device(&Direction::Capture).ok()?.get_id().ok()
}

fn open_client(source: &Source) -> Result<AudioClient> {
    Ok(match source {
        Source::App { pid } => AudioClient::new_application_loopback_client(*pid, true)?,
        Source::Microphone => DeviceEnumerator::new()?.get_default_device(&Direction::Capture)?.get_iaudioclient()?,
    })
}

/// Appends every packet Windows has ready to `samples`. The event is auto-reset, so one wake can
/// stand for several packets; reading only one per wake falls behind until the buffer overflows.
fn read_packets(capture: &AudioCaptureClient, packet: &mut Vec<u8>, samples: &mut Vec<f32>) -> Result<()> {
    while let Some(frames) = capture.get_next_packet_size()?.filter(|&frames| frames > 0) {
        packet.resize(frames as usize * BYTES_PER_SAMPLE, 0);
        let (read, info) = capture.read_from_device(packet)?;
        if info.flags.silent {
            // A silent packet's data is to be ignored, and may not even be written.
            samples.resize(samples.len() + read as usize, 0.0);
        } else {
            let bytes = &packet[..read as usize * BYTES_PER_SAMPLE];
            samples.extend(bytes.as_chunks::<BYTES_PER_SAMPLE>().0.iter().map(|&b| f32::from_le_bytes(b)));
        }
    }
    Ok(())
}

/// Runs this thread in Windows' "Audio" multimedia class (MMCSS), as audio apps do, so CPU load
/// doesn't delay reads until Windows' small capture buffer overflows. Reverted when dropped.
struct AudioPriority(HANDLE);

impl AudioPriority {
    fn claim() -> Option<Self> {
        let mut task_index = 0;
        unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut task_index) }.ok().map(Self)
    }
}

impl Drop for AudioPriority {
    fn drop(&mut self) {
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.0) };
    }
}

/// An event or process handle, closed when dropped. Waiting on and setting one is safe from any thread.
struct KernelHandle(HANDLE);

unsafe impl Send for KernelHandle {}
unsafe impl Sync for KernelHandle {}

impl Drop for KernelHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::sync_channel;

    use super::*;

    fn chunk() -> Chunk {
        Chunk { source: 0, samples: vec![0.0; 4], captured: Instant::now() }
    }

    #[test]
    fn a_full_queue_drops_chunks_rather_than_block_capture() {
        let (sink, chunks) = sync_channel(1);
        assert!(sink.deliver(chunk()));
        assert!(sink.deliver(chunk()), "a full queue drops the chunk and capture goes on");
        assert_eq!(chunks.try_iter().count(), 1);
        drop(chunks);
        assert!(!sink.deliver(chunk()), "with nobody receiving, capture ends");
    }
}
