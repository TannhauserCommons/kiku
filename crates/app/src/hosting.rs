//! Hosting this PC's models for other computers: runs `kiku-engine --serve` while the setting is on,
//! keeps its code in [`Controls`] for the settings window, and ends it with Kiku.

use std::{
    ffi::c_void,
    io::{BufRead, BufReader},
    os::windows::{io::AsRawHandle, process::CommandExt},
    process::{Child, Command, Stdio},
    sync::{atomic::Ordering, Arc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use kiku_core::{controls::Controls, engine::Result, engine_host::helper_path, log, remote::host};
use windows::Win32::{
    Foundation::HANDLE,
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// How often the setting, the host process and quitting are looked at.
const TICK: Duration = Duration::from_millis(250);
/// A host that ended is started again after this, doubling with each failure in a row up to the longest
/// wait. One that ran that long starts over at the first wait.
const FIRST_RETRY: Duration = Duration::from_secs(2);
const LONGEST_RETRY: Duration = Duration::from_secs(300);
/// What the host prints before its code.
const CODE_PREFIX: &str = "kiku-host ";

/// A running host and the threads reading what it prints.
struct Running {
    child: Child,
    started: Instant,
    readers: Vec<JoinHandle<()>>,
}

/// Runs until Kiku quits: the host runs while `host.on` is set, starts again if it ends on its own,
/// and gets a new code when asked.
pub fn run(controls: &Arc<Controls>) {
    let job = kill_on_close_job();
    let (mut running, mut retry_after, mut retry_at) = (None::<Running>, FIRST_RETRY, Instant::now());
    loop {
        if controls.quitting.load(Ordering::Relaxed) {
            return stop(&mut running, controls);
        }
        if controls.take_new_code_request() {
            stop(&mut running, controls);
            host::forget_secret(&host::default_state_dir());
            log::write("hosting: made a new code");
            retry_at = Instant::now();
        }
        match (controls.host_on(), &mut running) {
            (true, None) if Instant::now() >= retry_at => match start(controls, job) {
                Ok(started) => running = Some(started),
                Err(error) => {
                    log::write(format!("hosting: couldn't start the host: {error}"));
                    (retry_after, retry_at) = put_off(retry_after, None);
                }
            },
            (false, Some(_)) => stop(&mut running, controls),
            (true, Some(host)) => {
                if let Ok(Some(status)) = host.child.try_wait() {
                    log::write(format!("hosting: the host ended ({status}), starting it again"));
                    (retry_after, retry_at) = put_off(retry_after, Some(host.started.elapsed()));
                    stop(&mut running, controls);
                }
            }
            _ => {}
        }
        thread::sleep(TICK);
    }
}

/// The next wait after a failure, and when it ends.
fn put_off(after: Duration, ran_for: Option<Duration>) -> (Duration, Instant) {
    let after = match ran_for {
        Some(ran) if ran >= LONGEST_RETRY => FIRST_RETRY,
        _ => (after * 2).min(LONGEST_RETRY),
    };
    (after, Instant::now() + after)
}

fn start(controls: &Arc<Controls>, job: Option<HANDLE>) -> Result<Running> {
    let mut child = Command::new(helper_path()?)
        .arg("--serve")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    if let Some(job) = job {
        // SAFETY: both handles are valid; the job outlives the child.
        unsafe { AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())) }.ok();
    }
    let (stdout, stderr) = (child.stdout.take().ok_or("no stdout")?, child.stderr.take().ok_or("no stderr")?);
    let code_to = controls.clone();
    let code_reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(|line| line.ok()) {
            if let Some(code) = line.strip_prefix(CODE_PREFIX) {
                code_to.set_hosted_code(Some(code.trim().to_owned()));
            }
        }
    });
    let error_reader = thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(|line| line.ok()) {
            log::write(format!("hosting: {line}"));
        }
    });
    log::write("hosting: started the host");
    Ok(Running { child, started: Instant::now(), readers: vec![code_reader, error_reader] })
}

/// Ends the host, if running, and forgets its code.
fn stop(running: &mut Option<Running>, controls: &Controls) {
    if let Some(mut host) = running.take() {
        let _ = host.child.kill();
        let _ = host.child.wait();
        host.readers.into_iter().for_each(|reader| {
            let _ = reader.join();
        });
        log::write("hosting: stopped the host");
    }
    controls.set_hosted_code(None);
}

/// A job that ends its processes when Kiku does, even if Kiku is ended from outside.
fn kill_on_close_job() -> Option<HANDLE> {
    // SAFETY: plain Win32 calls; the handle is kept for the life of the process, which is the point.
    unsafe {
        let job = CreateJobObjectW(None, None).ok()?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits as *const _ as *const c_void, size).ok()?;
        Some(job)
    }
}
