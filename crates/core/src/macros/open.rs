//! What an `open` step may open: a web or mail link, a file or folder that exists, or an app by name.

use std::path::{Path, PathBuf};

use crate::actions::Private;

/// Extensions of programs, scripts and shortcuts: `open` never starts these (that is `run`'s gated job).
const PROGRAM_EXTENSIONS: &[&str] = &[
    "exe", "com", "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "msi", "msp", "msc", "scr", "lnk", "url", "pif", "cpl", "hta", "jar",
    "reg", "appref-ms", "application", "inf", "py", "pyw", "ps2", "sct", "wsc", "gadget", "xll", "settingcontent-ms", "diagcab", "chm", "docm", "xlsm", "pptm",
    "xlam", "dotm",
];

#[derive(Clone, Debug, PartialEq)]
pub enum OpenTarget {
    /// `http`, `https` or `mailto`.
    Url(Private),
    /// An existing file or folder that isn't a program, script or shortcut, on a local drive.
    Path(PathBuf),
    /// An app by name: the app finds it among the Start menu's shortcuts and App Paths and never passes the
    /// name to the shell as given; a name it doesn't find is an error.
    App(String),
}

/// `text` (a filled `open` step) as what to open, or why it may not be: another kind of link, a path that
/// isn't there, a program, a network path, or anything that reads as a command line. Errors never quote `text`.
pub fn open_target(text: &str) -> Result<OpenTarget, String> {
    target(text, true)
}

/// Whether a literal `open` step could ever be allowed, as [`open_target`] but not asking whether a path exists.
pub(super) fn check_open(text: &str) -> Result<(), String> {
    target(text, false).map(|_| ())
}

fn target(text: &str, must_exist: bool) -> Result<OpenTarget, String> {
    let text = text.trim();
    if let Some(scheme) = scheme(text) {
        return match scheme.to_ascii_lowercase().as_str() {
            "http" | "https" | "mailto" => Ok(OpenTarget::Url(Private::new(text.to_owned()))),
            _ => Err("that kind of link isn't opened: only http, https and mailto".into()),
        };
    }
    if text.starts_with("\\\\") || text.starts_with("//") {
        return Err("a network path isn't opened: use a path on a drive".into());
    }
    if Path::new(text).is_absolute() {
        return local_path(PathBuf::from(text), must_exist).map(OpenTarget::Path);
    }
    if text.contains(['/', '\\']) {
        return Err("a path to open must be complete (like D:\\Notes\\todo.txt) and exist".into());
    }
    app_name(text).map(|name| OpenTarget::App(name.to_owned()))
}

fn local_path(path: PathBuf, must_exist: bool) -> Result<PathBuf, String> {
    if must_exist && !path.exists() {
        return Err("that file or folder doesn't exist".into());
    }
    if !path.is_dir() && is_program(&path) {
        return Err("programs, scripts and shortcuts aren't opened (use a run step)".into());
    }
    Ok(path)
}

/// Whether `name` is a program as Windows would see it: it drops trailing dots and spaces, and a `:` after
/// the drive names a stream of a file (`notes.txt:x.exe`), so both count as a program.
fn is_program(name: impl AsRef<Path>) -> bool {
    let name = name.as_ref().to_string_lossy();
    let name = name.trim_end_matches(['.', ' ']);
    let after_drive = name.get(2..).filter(|_| name.as_bytes().get(1) == Some(&b':')).unwrap_or(name);
    let extension = Path::new(name).extension().and_then(|extension| extension.to_str());
    after_drive.contains(':') || extension.is_some_and(|extension| PROGRAM_EXTENSIONS.iter().any(|program| extension.eq_ignore_ascii_case(program)))
}

/// The scheme of `text` if it starts like a link (`name:`); one letter is a drive, so a path.
fn scheme(text: &str) -> Option<&str> {
    let (scheme, _) = text.split_once(':')?;
    let like_scheme = scheme.len() > 1 && scheme.starts_with(|c: char| c.is_ascii_alphabetic()) && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    like_scheme.then_some(scheme)
}

fn app_name(text: &str) -> Result<&str, String> {
    if text.is_empty() {
        return Err("there is nothing to open".into());
    }
    if text == "." || text == ".." || is_program(text) {
        return Err("that isn't an app's name".into());
    }
    if text.chars().any(|c| c.is_control() || "\"'`/\\&|<>^%:".contains(c)) {
        return Err("an app's name is words only: no quotes, slashes, : or & | < > ^ %".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(text: &str) -> String {
        open_target(text).expect_err(text)
    }

    #[test]
    fn programs_scripts_and_shortcuts_are_not_opened() {
        let dir = std::env::temp_dir().join(format!("kiku-open-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("folder.exe")).unwrap();
        for name in ["a.exe", "b.BAT", "c.Ps1", "d.lnk", "e.appref-ms"] {
            std::fs::write(dir.join(name), "").unwrap();
            assert!(refused(dir.join(name).to_str().unwrap()).contains("aren't opened"));
        }
        assert!(is_program("C:\\x\\evil.exe.") && is_program("C:\\x\\notes.txt:evil.exe") && !is_program("C:\\x\\notes.txt"));
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        assert!(open_target(dir.join("notes.txt").to_str().unwrap()).is_ok());
        assert!(open_target(dir.join("folder.exe").to_str().unwrap()).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_program_run_can_start_is_refused_by_open() {
        for name in ["a.exe", "b.bat", "c.cmd", "d.ps1", "e.py", "f.pyw", "g.docm", "h.xlsm", "i.chm"] {
            assert!(is_program(format!("C:\\x\\{name}")), "{name}");
        }
    }

    #[test]
    fn app_names_cannot_reach_the_shell_as_programs() {
        for name in [".", "..", "calc.exe", "run.CMD", "x.ps1", "calc.exe.", "calc.exe . "] {
            assert!(refused(name).contains("app's name"), "{name}");
        }
        assert_eq!(open_target("Notepad").unwrap(), OpenTarget::App("Notepad".into()));
    }

    #[test]
    fn network_and_device_paths_are_refused_before_the_disk_is_touched() {
        for path in ["\\\\server\\share\\x", "\\\\?\\C:\\x", "\\\\.\\C:\\x", "//server/share/x"] {
            assert!(refused(path).contains("network path"), "{path}");
        }
    }

    #[test]
    fn refusals_do_not_quote_the_value() {
        for text in ["secretscheme:abc", "/secret/missing/file.txt", "secret/relative", "secret.exe"] {
            assert!(!refused(text).contains("secret"), "{text}");
        }
    }

    #[test]
    fn a_literal_target_is_checked_without_its_existence() {
        assert!(check_open("/no/such/place/notes.txt").is_ok() || cfg!(windows));
        assert!(check_open("javascript:x").is_err());
        assert!(check_open("https://example.com").is_ok());
    }
}
