//! The Dictation card's drawing: the key, how long Kiku keeps listening, and the stop phrases.

use windows::{core::Result, Win32::Graphics::Direct2D::Common::D2D_RECT_F};

use kiku_core::dictation::{DictationKey, RELEASE_DELAYS};

use super::{design::{CONTROL, SETTING_ROW}, history::BoxRole, rect, Panel, Renderer};
use crate::overlay::{
    dictation_card::DictationAction,
    view::{Action, Regions},
};

/// The room for a row's label before its box.
const LABEL_WIDTH: f32 = 120.0;

impl Renderer {
    /// The Dictation card with its hint. Returns the y below.
    pub(super) fn draw_dictation_settings(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let y = self.draw_heading("Dictation", content, y)?;
        let card = &panel.view.dictation;
        let y = self.draw_card(content, y, |renderer, inner, mut y| {
            y = renderer.draw_dictation_key_row(panel, inner, y, regions)?;
            let labels = RELEASE_DELAYS.map(|ms| if ms == 0 { "Off".to_owned() } else { format!("{ms} ms") });
            let delays: Vec<_> = RELEASE_DELAYS
                .iter()
                .zip(&labels)
                .map(|(&ms, label)| (label.as_str(), card.delay_chosen().as_millis() == u128::from(ms), Some(Action::Dictation(DictationAction::Delay(ms)))))
                .collect();
            renderer.draw_row_label("Keep listening", inner, y)?;
            renderer.draw_segments(&delays, inner.right, y + SETTING_ROW / 2.0, panel, regions)?;
            y += SETTING_ROW;

            renderer.draw_row_label("Stop phrases", inner, y)?;
            let top = y + (SETTING_ROW - CONTROL) / 2.0;
            let role = BoxRole { one_line: true, icon: None, typing: card.typing(), click: Some(Action::Dictation(DictationAction::FocusStop)), clear: None };
            renderer.draw_text_box(&card.stop, "e.g. that's all Kiku, over and out", rect(inner.left + LABEL_WIDTH, top, inner.right, top + CONTROL), role, panel, regions)?;
            Ok(y + SETTING_ROW)
        })?;
        let hint = "Keep listening carries on that long after you let go, so a last word isn't cut off. Saying a stop phrase at the end of a line ends dictation; leave the box empty for none, and separate phrases with commas.";
        self.draw_hint(hint, content, y)
    }

    /// Hold or Toggle: how the dictation key dictates. Both are clickable, the chosen one too. Returns the y below.
    pub(super) fn draw_dictation_key_row(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let chosen = panel.view.dictation.key_chosen();
        let keys = [("Hold", DictationKey::Hold), ("Toggle", DictationKey::Toggle)].map(|(label, key)| (label, chosen == key, Some(Action::Dictation(DictationAction::Key(key)))));
        self.draw_row_label("Dictation key", content, y)?;
        self.draw_segments(&keys, content.right, y + SETTING_ROW / 2.0, panel, regions)?;
        Ok(y + SETTING_ROW)
    }
}

/// What a Dictation card control does, for its tooltip.
pub(super) fn tooltip(action: DictationAction) -> Option<String> {
    Some(match action {
        DictationAction::Key(DictationKey::Hold) => "Dictate while the key is held".into(),
        DictationAction::Key(DictationKey::Toggle) => "A press of the key starts dictation and the next press ends it".into(),
        DictationAction::Delay(0) => "Stop listening the moment the key is let go".into(),
        DictationAction::Delay(ms) => format!("Keep listening {ms} ms after the key is let go"),
        DictationAction::FocusStop => return None,
    })
}
