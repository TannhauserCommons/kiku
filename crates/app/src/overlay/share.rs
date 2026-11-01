//! The ✨ buttons: open a new chat in the Claude desktop app about some captions.
//!
//! The text is always written to `shared.txt`. When this app's MCP server is set up in Claude Desktop,
//! the chat just asks Claude to read it with `get_shared`, so any length works. Otherwise the text goes
//! in the chat's prefilled prompt, which holds about 14,000 characters, or on the clipboard beyond that.

use std::{env, fs, path::PathBuf};

use windows::{
    core::{w, HSTRING},
    Win32::{Foundation::HWND, UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL}},
};

use super::clipboard;
use kiku_core::{engine::Result, transcripts};

/// Claude Desktop's link for a new chat; `q` prefills the message box without sending it.
const NEW_CHAT: &str = "claude://claude.ai/new";
/// Comfortably under the ~14,000 characters a prefilled prompt keeps.
const PREFILL_LIMIT: usize = 12_000;

/// How the captions reached Claude.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sent {
    /// Claude reads them with the MCP server's `get_shared`.
    ThroughMcp,
    /// They are in the new chat's message box.
    Prefilled,
    /// Too long for the message box: they are on the clipboard to paste.
    Clipboard,
}

/// Opens a new Claude chat about `text`, described as `what` (e.g. "a line from Discord").
pub fn send(owner: HWND, what: &str, text: &str) -> Result<Sent> {
    transcripts::write_shared(text)?;
    let (prompt, sent) = if mcp_set_up() {
        (format!("I shared {what} from Kiku. Read it with the kiku get_shared tool."), Sent::ThroughMcp)
    } else {
        let prompt = format!("From Kiku, {what}:\n\n{text}\n\n");
        if prompt.chars().count() <= PREFILL_LIMIT {
            (prompt, Sent::Prefilled)
        } else {
            clipboard::copy(owner, &prompt)?;
            (String::new(), Sent::Clipboard)
        }
    };
    let link = if prompt.is_empty() { NEW_CHAT.to_owned() } else { format!("{NEW_CHAT}?q={}", encode(&prompt)) };
    // ShellExecute returns a value above 32 on success.
    let opened = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(link), None, None, SW_SHOWNORMAL) };
    if opened.0 as isize <= 32 {
        return Err("couldn't open Claude; is the Claude desktop app installed?".into());
    }
    Ok(sent)
}

/// Whether Claude Desktop's config lists this app's MCP server.
fn mcp_set_up() -> bool {
    desktop_configs().iter().any(|path| fs::read_to_string(path).is_ok_and(|config| config.contains("kiku-mcp")))
}

/// Claude Desktop's config: the Store app keeps it in its package folder, other installs in `%APPDATA%`.
fn desktop_configs() -> Vec<PathBuf> {
    let folder = |variable: &str| env::var_os(variable).map(PathBuf::from).unwrap_or_default();
    let packages = folder("LOCALAPPDATA").join("Packages");
    let packaged = fs::read_dir(&packages)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("Claude_")))
        .map(|package| package.join(r"LocalCache\Roaming\Claude\claude_desktop_config.json"));
    packaged.chain([folder("APPDATA").join(r"Claude\claude_desktop_config.json")]).collect()
}

/// Percent-encodes text for a URL query value.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::encode;

    #[test]
    fn query_text_is_percent_encoded() {
        assert_eq!(encode("You: on my way ✨\n"), "You%3A%20on%20my%20way%20%E2%9C%A8%0A");
    }
}
