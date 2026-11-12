//! The helper process that runs Kiku's model; see `engine_host`. Started by the app,
//! it ends when the app closes its input or ends it. `--probe <backend> <dir> [vad]` instead tries
//! an imported model on a test clip and prints what it heard. `--serve` hosts the model for other
//! computers (`remote::host`). `--models` and `--download` are the catalog's commands, `--check` tries a host (`models_cli`).

// No console window; the app talks to it over pipes.
#![windows_subsystem = "windows"]

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--probe") => kiku_core::engine_host::serve_probe(&args[1..]),
        Some("--serve") => {
            attach_parent_console();
            kiku_core::remote::host::serve(&args)
        }
        Some("--models" | "--download" | "--check") => {
            attach_parent_console();
            kiku_core::models_cli::run(&args)
        }
        _ => {
            kiku_core::engine_host::serve();
            std::process::ExitCode::SUCCESS
        }
    }
}

/// A GUI-subsystem exe has no console; `--serve` run from one shows its code and errors there, unless
/// stdout is already a pipe or file.
#[cfg(windows)]
fn attach_parent_console() {
    use std::{fs::OpenOptions, os::windows::io::IntoRawHandle};
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        System::Console::{AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE},
    };
    // SAFETY: plain Win32 calls; the console handles given to SetStdHandle are owned by the process from then on.
    unsafe {
        let missing = |id| {
            let handle = GetStdHandle(id);
            handle.is_null() || handle == INVALID_HANDLE_VALUE
        };
        if !missing(STD_OUTPUT_HANDLE) || AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        for id in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if missing(id)
                && let Ok(console) = OpenOptions::new().write(true).open("CONOUT$")
            {
                SetStdHandle(id, console.into_raw_handle());
            }
        }
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}
