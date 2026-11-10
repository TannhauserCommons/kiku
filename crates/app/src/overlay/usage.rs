//! The app's own CPU and GPU use, for the panel's footer: this process plus the helper process that
//! runs a GPU model (see [`kiku_core::engine_host`]).

use std::time::Instant;

use windows::{
    core::HSTRING,
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE},
        System::{
            Performance::{
                PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
                PDH_FMT, PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY,
            },
            Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    },
};

/// Use since the previous sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Usage {
    /// Share of all CPU threads, like Task Manager shows it.
    pub cpu_percent: f32,
    /// The busiest GPU engine (3D, compute, copy, ...), like Task Manager's GPU column.
    /// Zero while the app hasn't used the GPU, or without GPU counters.
    pub gpu_percent: f32,
    pub gpu_memory_bytes: u64,
}

pub struct UsageMeter {
    own: ProcessMeter,
    helper: Option<ProcessMeter>,
    at: Instant,
    threads: f64,
}

/// One process's CPU time so far and its GPU counters.
struct ProcessMeter {
    pid: u32,
    process: HANDLE,
    /// The helper's handle is ours to close; this process's own is a pseudo-handle.
    owned: bool,
    gpu: Option<GpuCounters>,
    cpu_seconds: f64,
}

/// Windows' per-process GPU counters. Each covers every engine (or adapter) the process uses.
struct GpuCounters {
    query: PDH_HQUERY,
    engines: PDH_HCOUNTER,
    memory: PDH_HCOUNTER,
}

impl Default for UsageMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageMeter {
    pub fn new() -> Self {
        let own = ProcessMeter::new(unsafe { GetCurrentProcessId() }, unsafe { GetCurrentProcess() }, false);
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
        Self { own, helper: None, at: Instant::now(), threads }
    }

    /// Counts the helper process running a GPU model too, from now on; `None` once it has ended.
    pub fn watch_helper(&mut self, pid: Option<u32>) {
        if self.helper.as_ref().map(|h| h.pid) == pid {
            return;
        }
        self.helper = pid.and_then(|pid| {
            let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
            Some(ProcessMeter::new(pid, process, true))
        });
    }

    pub fn sample(&mut self) -> Usage {
        let at = Instant::now();
        let elapsed = at.duration_since(self.at).as_secs_f64().max(1e-3);
        self.at = at;
        let mut usage = Usage { cpu_percent: 0.0, gpu_percent: 0.0, gpu_memory_bytes: 0 };
        for meter in std::iter::once(&mut self.own).chain(self.helper.as_mut()) {
            let (cpu_seconds, gpu_percent, gpu_memory_bytes) = meter.sample();
            usage.cpu_percent += (100.0 * cpu_seconds / elapsed / self.threads) as f32;
            usage.gpu_percent = usage.gpu_percent.max(gpu_percent);
            usage.gpu_memory_bytes += gpu_memory_bytes;
        }
        usage
    }
}

impl ProcessMeter {
    fn new(pid: u32, process: HANDLE, owned: bool) -> Self {
        Self { pid, process, owned, gpu: GpuCounters::open(pid), cpu_seconds: cpu_seconds(process) }
    }

    /// CPU seconds since the last sample, and the GPU's busiest engine and dedicated memory.
    fn sample(&mut self) -> (f64, f32, u64) {
        let now = cpu_seconds(self.process);
        let spent = (now - self.cpu_seconds).max(0.0);
        self.cpu_seconds = now;
        let (gpu_percent, gpu_memory_bytes) = self.gpu.as_ref().map_or((0.0, 0), GpuCounters::sample);
        (spent, gpu_percent, gpu_memory_bytes)
    }
}

impl Drop for ProcessMeter {
    fn drop(&mut self) {
        if self.owned {
            let _ = unsafe { CloseHandle(self.process) };
        }
    }
}

impl GpuCounters {
    fn open(pid: u32) -> Option<Self> {
        let mut query = PDH_HQUERY::default();
        if unsafe { PdhOpenQueryW(None, 0, &mut query) } != 0 {
            return None;
        }
        let counter = |path: String| {
            let mut counter = PDH_HCOUNTER::default();
            (unsafe { PdhAddEnglishCounterW(query, &HSTRING::from(path), 0, &mut counter) } == 0).then_some(counter)
        };
        let engines = counter(format!(r"\GPU Engine(pid_{pid}_*)\Utilization Percentage"));
        let memory = counter(format!(r"\GPU Process Memory(pid_{pid}_*)\Dedicated Usage"));
        match (engines, memory) {
            (Some(engines), Some(memory)) => {
                // Utilization is a rate, so the first sample needs a collection before it.
                unsafe { PdhCollectQueryData(query) };
                Some(Self { query, engines, memory })
            }
            _ => {
                unsafe { PdhCloseQuery(query) };
                None
            }
        }
    }

    /// The busiest engine's utilization and the dedicated memory in use.
    fn sample(&self) -> (f32, u64) {
        // Fails with "no data" while the process has no GPU instances yet; those read as empty.
        unsafe { PdhCollectQueryData(self.query) };
        let engines = read_array(self.engines, PDH_FMT_DOUBLE);
        let busiest = engines.iter().map(|(_, v)| unsafe { v.Anonymous.doubleValue }).fold(0.0, f64::max).min(100.0) as f32;
        let memory = read_array(self.memory, PDH_FMT_LARGE).iter().map(|(_, v)| unsafe { v.Anonymous.largeValue } as u64).sum();
        (busiest, memory)
    }
}

impl Drop for GpuCounters {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

/// Every instance's name and value for a wildcard counter; empty if it can't be read.
fn read_array(counter: PDH_HCOUNTER, format: PDH_FMT) -> Vec<(String, PDH_FMT_COUNTERVALUE)> {
    let (mut bytes, mut count) = (0u32, 0u32);
    // The first call reports the size needed: the items followed by their names.
    unsafe { PdhGetFormattedCounterArrayW(counter, format, &mut bytes, &mut count, None) };
    if count == 0 {
        return Vec::new();
    }
    let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
    let items = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    if unsafe { PdhGetFormattedCounterArrayW(counter, format, &mut bytes, &mut count, Some(items)) } != 0 {
        return Vec::new();
    }
    let items = unsafe { std::slice::from_raw_parts(items, count as usize) };
    items.iter().map(|item| (unsafe { item.szName.to_string() }.unwrap_or_default(), item.FmtValue)).collect()
}

fn cpu_seconds(process: HANDLE) -> f64 {
    let [mut created, mut exited, mut kernel, mut user] = [FILETIME::default(); 4];
    if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }.is_err() {
        return 0.0;
    }
    let seconds = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64 / 1e7;
    seconds(kernel) + seconds(user)
}
