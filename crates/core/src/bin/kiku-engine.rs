//! The helper process that runs Kiku's model; see `engine_host`. Started by the app,
//! it ends when the app closes its input or ends it. `--probe <backend> <dir> [vad]` instead tries
//! an imported model on a test clip and prints what it heard.

// No console window; the app talks to it over pipes.
#![windows_subsystem = "windows"]

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--probe") => kiku_core::engine_host::serve_probe(&args[1..]),
        _ => {
            kiku_core::engine_host::serve();
            std::process::ExitCode::SUCCESS
        }
    }
}
