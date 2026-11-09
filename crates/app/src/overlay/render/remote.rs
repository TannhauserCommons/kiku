//! The Listening tab's remote engine cards: where models run, and sharing this PC's models.

use windows::{
    core::Result,
    Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F},
};

use kiku_core::{remote::HostCode, settings::EngineWhere};

use super::{
    design::{CONTROL, SETTING_ROW},
    height_of,
    history::BoxRole,
    rect, width, Emphasis, Panel, Renderer, RowButton, SECTION_GAP,
};
use crate::overlay::{
    field::Field,
    render::Icon,
    view::{Action, Regions, WordBox},
};

/// How a code is shown: its address, the secret and certificate left out.
fn masked(code: &HostCode) -> String {
    format!("kiku://{}/#•••", code.address())
}

/// What to tell about a code that isn't whole, saying whether the one saved before is still in use.
fn broken_code_note(saved: bool) -> String {
    let still = if saved { ", so Kiku still uses the one before" } else { "" };
    format!("That isn't a whole code{still}. Copy it again from the other computer's Share this PC's models.")
}

/// `field` with every character after the code's address made a bullet, the caret on the same character.
fn bulleted(field: &Field) -> Field {
    let secret_from = shown_up_to(&field.text);
    let text: String = field.text.char_indices().map(|(at, c)| if at >= secret_from { '•' } else { c }).collect();
    let mut units = 0;
    let before_caret = field
        .text
        .chars()
        .take_while(|c| {
            let before = units;
            units += c.len_utf16();
            before < field.caret_utf16() as usize
        })
        .count();
    let mut shown = Field::default();
    shown.set(&text);
    (before_caret..text.chars().count()).for_each(|_| shown.left(false));
    shown
}

/// How much of a code's text may be drawn: `kiku://` and the address (`host` or `[v6]`, then `:port`),
/// whatever follows them, as an edit may have damaged the `/#` that marks the keys. Text that isn't the
/// start of a code could be a bare secret, so none of it is drawn.
fn shown_up_to(text: &str) -> usize {
    const SCHEME: &str = "kiku://";
    let rest = text.trim_start();
    let lead = text.len() - rest.len();
    let is_scheme = |head: &str| SCHEME.starts_with(&head.to_ascii_lowercase());
    if rest.len() <= SCHEME.len() {
        return if is_scheme(rest) { text.len() } else { 0 };
    }
    if !rest.get(..SCHEME.len()).is_some_and(is_scheme) {
        return 0;
    }
    let address = &rest[SCHEME.len()..];
    let host = match address.strip_prefix('[') {
        Some(v6) => v6.find(']').map_or(0, |end| end + 2),
        None => address.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))).unwrap_or(address.len()),
    };
    let port = address[host..].strip_prefix(':').map_or(0, |digits| 1 + digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len()));
    lead + SCHEME.len() + host + port
}

impl Renderer {
    /// Where models run, then sharing this PC's. Returns the y below.
    pub(super) fn draw_remote_settings(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let y = self.draw_where_card(panel, content, y, regions)?;
        self.draw_share_card(panel, content, y + SECTION_GAP, regions)
    }

    /// This PC or another computer, and for another computer its code and what Kiku is doing with it.
    fn draw_where_card(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let on_host = panel.engine_where == EngineWhere::Host;
        let typed = panel.view.host_code.text.trim();
        let parsed = HostCode::parse(typed);
        let mut y = self.draw_heading("Where models run", content, y)?;
        y = self.draw_card(content, y, |renderer, inner, mut y| {
            let places = [("This PC", !on_host, Action::SetEngineWhere(EngineWhere::ThisPc)), ("Another computer", on_host, Action::SetEngineWhere(EngineWhere::Host))];
            y = renderer.draw_choice_row("Run models on", &places, inner, y, panel, regions)?;
            if on_host {
                y = renderer.draw_code_row(parsed.as_ref().ok(), panel, inner, y, regions)?;
            }
            Ok(y)
        })?;
        if !on_host {
            return self.draw_hint("Models run on this PC, and nothing is sent anywhere.", content, y);
        }
        let palette = self.palette.clone();
        let (status, colour) = match (typed.is_empty(), &parsed, panel.model.remote.as_deref()) {
            (true, ..) => ("Paste the code the other computer shows.".to_owned(), palette.text_dim),
            (false, Err(_), _) => (broken_code_note(!panel.engine_host.is_empty()), palette.error),
            (false, Ok(_), Some(address)) => (format!("Running on {address}."), palette.accent),
            (false, Ok(code), None) if panel.model.loaded => {
                (format!("Running on this PC for now: {} didn't answer. Kiku tries again every minute.", code.address()), palette.warn)
            }
            (false, Ok(code), None) => (format!("Connecting to {}…", code.address()), palette.text_dim),
        };
        y = self.draw_note(&status, colour, content, y)?;
        let sent = "Your mic's and apps' audio is sent to that computer to be captioned, over a connection only it can read. Otherwise Kiku is fully offline.";
        self.draw_hint(sent, content, y)
    }

    /// The code's text box: a valid code not being edited shows only its address; any other text shows
    /// everything after `/#` as bullets, one for one, so the secret is never drawn.
    fn draw_code_row(&mut self, valid: Option<&HostCode>, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        self.begin_row(content, y)?;
        let typing = panel.view.typing_word == Some(WordBox::Host);
        let shown = match valid {
            Some(code) if !typing => {
                let mut field = Field::default();
                field.set(&masked(code));
                field
            }
            _ => bulleted(&panel.view.host_code),
        };
        let role = BoxRole { one_line: true, icon: None, typing, click: Some(Action::TypeWord(WordBox::Host)), clear: Some(Action::ClearWord(WordBox::Host)) };
        let top = y + (SETTING_ROW - CONTROL) / 2.0;
        self.draw_text_box(&shown, "Paste the other computer's code: kiku://…", rect(content.left, top, content.right, top + CONTROL), role, panel, regions)?;
        Ok(y + SETTING_ROW)
    }

    /// A line under a card in `colour`. Returns the y below.
    fn draw_note(&mut self, text: &str, colour: D2D1_COLOR_F, content: D2D_RECT_F, y: f32) -> Result<f32> {
        let note = self.layout(&self.fonts.small, text, width(&content) - 8.0)?;
        self.draw_layout(&note, content.left + 4.0, y + 8.0, colour)?;
        Ok(y + 8.0 + height_of(&note)?)
    }

    /// The switch and, while sharing, the code with Copy and New code.
    fn draw_share_card(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let y = self.draw_heading("Share this PC's models", content, y)?;
        let sharing = panel.host_on;
        let y = self.draw_card(content, y, |renderer, inner, mut y| {
            y = renderer.draw_toggle_row("Let other computers use them", sharing, Action::ShareModels(!sharing), inner, y, panel, regions)?;
            if !sharing {
                return Ok(y);
            }
            let Some(code) = panel.hosted_code.as_deref() else {
                return renderer.draw_two_line_row("Starting…", "Getting this PC ready to share", &[], inner, y, panel, regions);
            };
            let name = HostCode::parse(code).map(|code| masked(&code)).unwrap_or_else(|_| "Your code".to_owned());
            let confirming = panel.view.confirm_new_code;
            let new_code: RowButton = match confirming {
                true => ("Press again", Some(Icon::Reset), Action::NewHostCode, Emphasis::Danger),
                false => ("New code", Some(Icon::Reset), Action::NewHostCode, Emphasis::Secondary),
            };
            let buttons = [("Copy", Some(Icon::Copy), Action::CopyHostCode, Emphasis::Secondary), new_code];
            let detail = if confirming { "The old code stops working for every computer using it" } else { "Copy has the whole code; its secret stays hidden here" };
            renderer.draw_two_line_row(&name, detail, &buttons, inner, y, panel, regions)
        })?;
        if !sharing {
            return self.draw_hint("Lets other computers caption with this PC's models, using a code you give them.", content, y);
        }
        let hint = "On the other computer, choose Another computer under Where models run and paste the code. \
                    Windows may ask for an administrator once, to let other computers connect. \
                    To reach this PC from outside your home network, a VPN like Tailscale is the easy way, or forward the port.";
        self.draw_hint(hint, content, y)
    }
}

#[cfg(test)]
mod tests {
    use super::shown_up_to;

    /// What part of `text` may be drawn.
    fn shown(text: &str) -> &str {
        &text[..shown_up_to(text)]
    }

    #[test]
    fn only_the_address_of_a_code_is_drawn() {
        let keys = "ffffffffffffffffffffffffffffffffffffffffffQ.ssssssssssssssssssssssssssssssssssssssssssg";
        for (text, drawn) in [
            (format!("kiku://studio:7300/#{keys}"), "kiku://studio:7300"),
            (format!("kiku://studio:7300/{keys}"), "kiku://studio:7300"),
            (format!("kiku://studio:7300#{keys}"), "kiku://studio:7300"),
            (format!("kiku://studio:7300{keys}"), "kiku://studio:7300"),
            (format!("kiku://192.168.1.20:7300/#{keys}"), "kiku://192.168.1.20:7300"),
            (format!("kiku://[::1]:7300/#{keys}"), "kiku://[::1]:7300"),
            (format!("  Kiku://studio:7300/#{keys}"), "  Kiku://studio:7300"),
            (format!("kiku://[{keys}"), "kiku://"),
            (keys.to_owned(), ""),
            ("kiku:/".to_owned(), "kiku:/"),
            ("kiku://studio:73".to_owned(), "kiku://studio:73"),
        ] {
            assert_eq!(shown(&text), drawn, "{text}");
        }
    }
}
