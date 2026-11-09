//! `kiku-engine --models` and `--download <id> [--device cpu|gpu]`: the catalog from the command line,
//! for a host with no settings window. Downloads use the app's worker (resume, sha256 checks).
//! `--check <code>` tries a host from the other side.

use std::{
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use crate::{
    catalog::{self, Model, Variant},
    download::{Downloads, State, Target},
    engine::Device,
    engine_host,
    remote::{self, HostCode, ModelRequest},
};

/// How often a download's progress is printed.
const REPORT_EVERY: Duration = Duration::from_secs(2);
const USAGE: &str = "usage: kiku-engine --download <id> [--device cpu|gpu]";

const CHECK_USAGE: &str = "usage: kiku-engine --check [<code>] [--model <id>] [--device cpu|gpu]  (without a code, KIKU_HOST_CODE is used)";

/// Runs the command named by `args` (`--models`, `--download ...` or `--check ...`).
pub fn run(args: &[String]) -> ExitCode {
    let result = match args.first().map(String::as_str) {
        Some("--models") => {
            print!("{}", list());
            Ok(())
        }
        Some("--check") => check_command(&args[1..]),
        _ => download_command(&args[1..]),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// The catalog's models, not those imported.
fn offered() -> impl Iterator<Item = &'static Model> {
    catalog::models().into_iter().filter(|model| !model.imported())
}

/// One line per model: its id, name, and each device it has a build for, installed or not, with its size.
fn list() -> String {
    offered().map(|model| format!("{}\n", describe(model))).collect()
}

fn describe(model: &Model) -> String {
    let builds = [(Device::Cpu, "cpu"), (Device::Cuda, "gpu")].into_iter().filter_map(|(device, name)| {
        let variant = build_for(model, device)?;
        let state = if variant.installed() { "installed" } else { "not installed" };
        Some(format!("{name}: {state} ({}, {})", variant.precision, megabytes(variant.size())))
    });
    format!("{}  {}  {}", model.id, model.name, builds.collect::<Vec<_>>().join("  "))
}

/// The build the app would run on `device`, if the model has one for it.
fn build_for(model: &Model, device: Device) -> Option<&Variant> {
    Some(model.variant(device)).filter(|variant| variant.device == device)
}

fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes / 1_000_000)
}

fn download_command(args: &[String]) -> Result<(), String> {
    let id = args.first().filter(|id| !id.starts_with("--")).ok_or(USAGE)?;
    let device = match args.get(1).map(String::as_str) {
        None => Device::Cpu,
        Some("--device") => parse_device(args.get(2)).ok_or(USAGE)?,
        _ => return Err(USAGE.into()),
    };
    let model = offered().find(|model| &model.id == id).ok_or_else(|| {
        let ids: Vec<&str> = offered().map(|model| model.id.as_str()).collect();
        format!("no model {id}; the models are: {}", ids.join(", "))
    })?;
    let variant = build_for(model, device).ok_or_else(|| format!("{id} has no build for that device"))?;
    fetch(model, Target::Model(model, variant))?;
    if device == Device::Cuda && !cfg!(windows) {
        println!("On Linux the GPU also needs NVIDIA's driver with CUDA 12.9 and cuDNN 9 installed (the Docker image has them).");
    }
    Ok(())
}

fn parse_device(name: Option<&String>) -> Option<Device> {
    match name?.as_str() {
        "cpu" => Some(Device::Cpu),
        "gpu" => Some(Device::Cuda),
        _ => None,
    }
}

/// Connects to the host in the code, has it load a model and hear the test clip, and says how long that took.
/// Only the host's address is printed, never the code's secret.
fn check_command(args: &[String]) -> Result<(), String> {
    let given = args.first().filter(|code| !code.starts_with("--"));
    let code = given.cloned().or_else(|| std::env::var("KIKU_HOST_CODE").ok().filter(|code| !code.is_empty())).ok_or(CHECK_USAGE)?;
    let (mut model, mut device) = (catalog::DEFAULT_ID.to_string(), Device::Cpu);
    let mut options = args[usize::from(given.is_some())..].iter();
    while let Some(option) = options.next() {
        match option.as_str() {
            "--model" => model = options.next().ok_or(CHECK_USAGE)?.clone(),
            "--device" => device = parse_device(options.next()).ok_or(CHECK_USAGE)?,
            _ => return Err(CHECK_USAGE.into()),
        }
    }
    let code = HostCode::parse(&code).map_err(|error| format!("that isn't a host code: {error}"))?;
    println!("checking {} for {model} on the {}", code.address(), if device == Device::Cuda { "GPU" } else { "CPU" });

    let request = ModelRequest { model, device, hotwords: Vec::new() };
    let started = Instant::now();
    let engine = remote::connect(&code, &request).map_err(|error| format!("the host didn't load it: {error}"))?;
    println!("connected and loaded in {:.1} s", started.elapsed().as_secs_f32());
    let started = Instant::now();
    let heard = engine_host::hear(engine.as_ref(), &engine_host::probe_clip())
        .map_err(|error| format!("the host couldn't transcribe the test clip: {error}"))?;
    println!("heard in {:.1} s: {heard}", started.elapsed().as_secs_f32());
    if heard.trim().is_empty() {
        return Err("the host heard nothing in the test clip".into());
    }
    Ok(())
}

/// Downloads `target` and what it needs, printing progress, and returns once it is installed.
fn fetch(model: &Model, target: Target) -> Result<(), String> {
    if target.complete() {
        println!("{} already installed", model.id);
        return Ok(());
    }
    println!("downloading {} ({})", model.id, megabytes(target.size()));
    let downloads = Downloads::start(|| {});
    downloads.download(target);
    let targets: Vec<Target> = [Some(target), target.gpu_pack()].into_iter().flatten().collect();
    loop {
        let mut pending = false;
        for target in &targets {
            // A finished download leaves the queue; a failed or paused one stays in it.
            let Some(progress) = downloads.progress(*target) else { continue };
            match progress.state {
                State::Failed(why) => return Err(format!("download of {} failed: {why}", model.id)),
                State::Paused => return Err(format!("download of {} was paused", model.id)),
                State::Queued | State::Running => {
                    pending = true;
                    let percent = if progress.total == 0 { 0 } else { progress.done * 100 / progress.total };
                    println!("{}: {percent}% ({} of {})", target.key(), megabytes(progress.done), megabytes(progress.total));
                }
            }
        }
        if !pending {
            if !target.complete() {
                return Err(format!("download of {} stopped before every file was in", model.id));
            }
            println!("{} installed", model.id);
            return Ok(());
        }
        thread::sleep(REPORT_EVERY);
    }
}
