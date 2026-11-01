//! Spike: can this Windows build capture one app's audio (process loopback)?
//! Usage: cargo run --example loopback_probe -- Discord.exe opera.exe
use std::{collections::VecDeque, ffi::OsStr, thread, time::{Duration, Instant}};
use sysinfo::{ProcessRefreshKind, RefreshKind, System};
use wasapi::*;

fn root_pid(sys: &System, exe: &str) -> Option<u32> {
    let procs: Vec<_> = sys.processes_by_name(OsStr::new(exe)).collect();
    procs.iter().find(|p| {
        p.parent().and_then(|pp| sys.process(pp)).map_or(true, |pp| pp.name() != OsStr::new(exe))
    }).map(|p| p.pid().as_u32())
}

fn probe(exe: String, pid: u32) {
    initialize_mta().ok().unwrap();
    let fmt = WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None);
    let mut client = match AudioClient::new_application_loopback_client(pid, true) {
        Ok(c) => c,
        Err(e) => return println!("{exe} ({pid}): activation FAILED: {e}"),
    };
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 };
    if let Err(e) = client.initialize_client(&fmt, &Direction::Capture, &mode) {
        return println!("{exe} ({pid}): initialize FAILED: {e}");
    }
    let ev = client.set_get_eventhandle().unwrap();
    let cap = client.get_audiocaptureclient().unwrap();
    client.start_stream().unwrap();
    let mut q = VecDeque::new();
    let start = Instant::now();
    let (mut sum, mut n, mut sec) = (0f64, 0usize, 0);
    while start.elapsed() < Duration::from_secs(8) {
        let _ = ev.wait_for_event(500);
        cap.read_from_device_to_deque(&mut q).ok();
        while q.len() >= 4 {
            let b: Vec<u8> = q.drain(..4).collect();
            let s = f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64;
            sum += s * s; n += 1;
        }
        if start.elapsed().as_secs() > sec {
            sec = start.elapsed().as_secs();
            let rms = if n > 0 { (sum / n as f64).sqrt() } else { 0.0 };
            println!("{exe} ({pid}) t={sec}s samples={n} rms={rms:.5}");
            sum = 0.0; n = 0;
        }
    }
    client.stop_stream().ok();
}

fn main() {
    let sys = System::new_with_specifics(RefreshKind::nothing().with_processes(ProcessRefreshKind::everything())).expect("sysinfo");
    let handles: Vec<_> = std::env::args().skip(1).filter_map(|exe| {
        let pid = exe.parse().ok().or_else(|| root_pid(&sys, &exe));
        println!("{exe}: root pid {pid:?}");
        pid.map(|pid| thread::spawn(move || probe(exe, pid)))
    }).collect();
    for h in handles { h.join().ok(); }
}
