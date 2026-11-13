//! Speakers' names: how a name shows on a line in each label style, the box that names a speaker, and
//! the Apps tab's label styles with their previews and its list of saved voices.

use windows::{
    core::Result,
    Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F},
};

use kiku_core::{
    captions::{Caption, YOU_SECTION},
    settings::LabelStyle,
    voices::book::{VoiceInfo, VoiceScope},
};

use super::{
    caption_color,
    design::{black, white, with_alpha, CONTROL, ROW_GAP},
    expand, height, height_of, history::BoxRole, line_heights, metrics, pointer_on, rect, text_rects, width, Emphasis, Panel, Renderer, RowButton,
    MODEL_ROW, SECTION_GAP, SPEAKERS,
};
use crate::overlay::{
    labels::{badge_text, Shape, BADGE_GUTTER, RAIL_GUTTER},
    render::Icon,
    view::{Action, LineRef, Regions, Target, WordBox},
};

/// A round badge's diameter, and a rail's width.
const BADGE: f32 = 20.0;
const RAIL: f32 = 3.0;
/// The box that names a speaker, in front of the line it opens on.
const NAME_BOX: f32 = 220.0;

/// What is written on a colour: dark ink on a light one, light ink on a dark one.
fn ink_on(colour: D2D1_COLOR_F) -> D2D1_COLOR_F {
    if 0.299 * colour.r + 0.587 * colour.g + 0.114 * colour.b > 0.6 { black(0.85) } else { white(0.95) }
}

fn style_name(style: LabelStyle) -> &'static str {
    match style {
        LabelStyle::EveryLine => "Lines",
        LabelStyle::Turns => "Turns",
        LabelStyle::Tags => "Tags",
        LabelStyle::Colour => "Colour",
        LabelStyle::Badges => "Badges",
        LabelStyle::Blocks => "Blocks",
    }
}

/// What a label style does, for its tooltip.
pub(super) fn style_tip(style: LabelStyle, you: bool) -> String {
    let whose = if you { "your" } else { "speakers'" };
    let how = match style {
        LabelStyle::EveryLine => "the name before every line",
        LabelStyle::Turns => "the name once for each turn, then a bar in their colour down its lines",
        LabelStyle::Tags => "a short tag on every line, tinted in their colour",
        LabelStyle::Colour => "only a bar in their colour; the name shows under the pointer",
        LabelStyle::Badges => "a round badge with the number or initial at each turn",
        LabelStyle::Blocks => "a small heading with the name above each turn",
    };
    format!("The {whose} names as {how}")
}

impl Renderer {
    /// What a label style draws beside a line's text, which sits at `text_x`, `y` in `layout`: a pill behind a
    /// tag, a rail down a turn, a badge at its start, and a hover plate on the name. Each is where a click names
    /// the speaker, when the line's can be named. `at` is the left of the column, the line's top and its height.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_label_marks(&mut self, caption: &Caption, shape: Shape, layout: &windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, at: (f32, f32, f32), line: LineRef, panel: &Panel, regions: &mut Regions) -> Result<()> {
        let Some(label) = caption.label() else { return Ok(()) };
        let (left, y, line_height) = at;
        let colour = unsafe { self.label_brush(label, caption.voice).GetColor() };
        let action = Action::NameVoice(line);
        let hovered = caption.nameable() && pointer_on(panel, action).0;
        let mut click = |area: D2D_RECT_F| {
            if caption.nameable() {
                regions.targets.push((area, Target::Click(action)));
            }
        };
        let lead = shape.lead(caption);
        if lead.marked > 0 && let Some(&name) = text_rects(layout, 0, lead.marked, left + shape.indent(caption), y)?.first() {
            if lead.pill {
                let pill = rect(name.left, name.top + 1.0, name.right, name.bottom - 1.0);
                self.fill_rounded(pill, height(&pill) / 2.0, with_alpha(colour, if hovered { 0.42 } else { 0.26 }))?;
            } else if hovered {
                self.fill_rounded(expand(name, 2.0), 4.0, with_alpha(colour, 0.2))?;
            }
            click(name);
        }
        if shape.rail(caption) {
            let top = if shape.first { y } else { y - ROW_GAP };
            // Square ends, so a turn's rows (each drawn up to the one before) make one unbroken rail.
            let bar = rect(left + 1.0, top, left + 1.0 + RAIL, y + line_height);
            self.fill_rounded(bar, 0.0, with_alpha(colour, if hovered { 1.0 } else { 0.85 }))?;
            click(rect(left - 2.0, top, left + RAIL_GUTTER - 2.0, y + line_height));
        }
        if shape.badge(caption) {
            let first_line = line_heights(layout)?.first().copied().unwrap_or(line_height);
            let (x, middle) = (left + BADGE / 2.0 + 1.0, y + first_line / 2.0);
            self.fill_dot(x, middle, BADGE / 2.0 + if hovered { 1.5 } else { 0.0 }, colour)?;
            let text = self.layout(&self.fonts.strong, &badge_text(&caption.tag().unwrap_or_default()), f32::MAX)?;
            let size = metrics(&text)?;
            self.draw_layout(&text, x - size.width / 2.0, middle - size.height / 2.0, ink_on(colour))?;
            click(rect(left, middle - BADGE / 2.0, left + BADGE_GUTTER - 6.0, middle + BADGE / 2.0));
        }
        Ok(())
    }

    /// For the colour-only style, the name in the speaker's colour right-aligned at `right`: shown while the line is hovered.
    pub(super) fn draw_name_chip(&mut self, caption: &Caption, right: f32, middle: f32) -> Result<()> {
        let (Some(label), Some(tag)) = (caption.label(), caption.tag()) else { return Ok(()) };
        let colour = unsafe { self.label_brush(label, caption.voice).GetColor() };
        let text = self.single_line(&self.fonts.strong, &tag, 160.0)?;
        let size = metrics(&text)?;
        let chip = rect(right - size.width - 16.0, middle - size.height / 2.0 - 3.0, right, middle + size.height / 2.0 + 3.0);
        self.glass(chip, height(&chip) / 2.0, self.palette.popover)?;
        self.draw_layout(&text, chip.left + 8.0, middle - size.height / 2.0, colour)
    }

    /// The box that names the speaker of the line drawn at `text_x`, `y`, over its start.
    pub(super) fn draw_naming_box(&mut self, text_x: f32, y: f32, visible: D2D_RECT_F, panel: &Panel, regions: &mut Regions) -> Result<()> {
        let Some(line) = panel.view.naming else { return Ok(()) };
        // Kept inside what shows of the section, so a top line's box isn't cut off under the header.
        let top = (y - 3.0).min(visible.bottom - CONTROL).max(visible.top);
        let area = rect(text_x - 4.0, top, (text_x + NAME_BOX).min(visible.right), top + CONTROL);
        let role = BoxRole { one_line: true, icon: None, typing: true, click: Some(Action::NameVoice(line)), clear: None };
        self.draw_text_box(&panel.view.name, "Name, then Enter", area, role, panel, regions)?;
        Ok(())
    }

    /// Three sample lines in the label style chosen for yours (or the speakers'), in a recessed plate under a
    /// choice row. Returns the y below.
    fn draw_label_preview(&mut self, you: bool, panel: &Panel, inner: D2D_RECT_F, y: f32) -> Result<f32> {
        self.begin_row(inner, y)?;
        let mark = self.scene.borrow().mark();
        let section = if you { YOU_SECTION } else { 1 };
        let lines = self.preview.sections[section].captions.clone();
        let (left, text_width) = (inner.left + 12.0, width(&inner) - 24.0);
        let mut top = y + 12.0;
        for row in 0..lines.len() {
            let (caption, before) = (&lines[row], row.checked_sub(1).map(|before| &lines[before]));
            let shape = Shape::of(caption, before, panel.labels);
            let layout = self.caption_layout(caption, shape, text_width)?;
            let line_height = height_of(&layout)?;
            let line = LineRef::of(section, caption);
            self.draw_label_marks(caption, shape, &layout, (left, top, line_height), line, panel, &mut Regions::default())?;
            self.draw_layout(&layout, left + shape.indent(caption), top, caption_color(caption, &self.palette))?;
            top += line_height + ROW_GAP;
        }
        let plate = rect(inner.left, y + 6.0, inner.right, top - ROW_GAP + 12.0);
        self.scene.borrow_mut().under(mark, |scene| scene.glass(plate, super::RADIUS_CONTROL, self.palette.track));
        Ok(plate.bottom + 6.0)
    }

    /// How names show on lines: a choice and a preview for the speakers' and for yours. Returns the y below.
    pub(super) fn draw_label_settings(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let mut y = self.draw_heading("Names on lines", content, y)?;
        for (you, label) in [(false, "Speakers"), (true, "Your lines")] {
            let chosen = if you { panel.labels.you } else { panel.labels.speakers };
            let styles = LabelStyle::ALL.map(|style| (style_name(style), chosen == style, Action::SetLabelStyle { you, style }));
            y = self.draw_card(content, y, |renderer, inner, y| {
                let y = renderer.draw_choice_row(label, &styles, inner, y, panel, regions)?;
                renderer.draw_label_preview(you, panel, inner, y)
            })? + 8.0;
        }
        let hint = "A speaker you haven't named shows as their number. A turn is a run of lines by one voice. Click a name (or the badge, tag or bar) in the panel to name them. Copied text keeps \"Speaker 1: \". Captions mode shows lines, tags or badges, and tags for the other styles.";
        self.draw_hint(hint, content, y - 8.0)
    }

    /// The voices you have named: the name in a colour, where it is recognised, Rename and Forget. Returns the y below.
    pub(super) fn draw_saved_voices(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let y = self.draw_heading("Saved voices", content, y + SECTION_GAP)?;
        let voices = &panel.saved_voices;
        if voices.is_empty() {
            let y = self.draw_card(content, y, |renderer, inner, y| {
                renderer.begin_row(inner, y)?;
                let none = renderer.layout(&renderer.fonts.small, "Name a speaker by clicking their name in the panel.", width(&inner))?;
                renderer.draw_layout(&none, inner.left, y + 12.0, renderer.palette.text_dim)?;
                Ok(y + 36.0)
            })?;
            return self.draw_hint("Named voices are kept on this PC, encrypted for your Windows account, and never sent anywhere.", content, y);
        }
        let y = self.draw_card(content, y, |renderer, inner, mut y| {
            for (index, voice) in voices.iter().enumerate() {
                y = renderer.draw_voice_rows(index, voice, panel, inner, y, regions)?;
            }
            Ok(y)
        })?;
        self.draw_hint("Forget deletes a voice from this PC at once. Named voices are kept encrypted for your Windows account and never sent anywhere.", content, y)
    }

    fn draw_voice_rows(&mut self, index: usize, voice: &VoiceInfo, panel: &Panel, inner: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        self.begin_row(inner, y)?;
        let middle = y + MODEL_ROW / 2.0;
        let colour = unsafe { self.voice_brushes[index % SPEAKERS].GetColor() };
        let forget = match panel.view.confirm_forget == Some(voice.id) {
            true => ("Press again", Some(Icon::Trash), Action::ForgetSaved(voice.id), Emphasis::Danger),
            false => ("Forget", None, Action::ForgetSaved(voice.id), Emphasis::Ghost),
        };
        let buttons: [RowButton; 2] = [("Rename", Some(Icon::Pencil), Action::RenameSaved(voice.id), Emphasis::Secondary), forget];
        let mut right = inner.right;
        for &(label, icon, action, emphasis) in buttons.iter().rev() {
            right = self.draw_text_button(label, icon, action, emphasis, right, middle, panel, regions)? - 6.0;
        }
        if panel.view.typing_word == Some(WordBox::Rename) && panel.view.renaming == Some(voice.id) {
            let area = rect(inner.left, middle - CONTROL / 2.0, right - 2.0, middle + CONTROL / 2.0);
            let role = BoxRole { one_line: true, icon: None, typing: true, click: Some(Action::RenameSaved(voice.id)), clear: None };
            self.draw_text_box(&panel.view.rename, "New name, then Enter", area, role, panel, regions)?;
        } else {
            let name = self.single_line(&self.fonts.label, &voice.name, right - inner.left - 8.0)?;
            self.draw_layout(&name, inner.left, middle - height_of(&name)? / 2.0, colour)?;
        }
        // A voice named at the mic has no app to be limited to.
        let Some(home) = &voice.home else { return Ok(y + MODEL_ROW) };
        let only = format!("Only in {home}");
        let everywhere = voice.scope == VoiceScope::Everywhere;
        let scope = [("Everywhere", everywhere, Action::SetVoiceScope(voice.id, false)), (only.as_str(), !everywhere, Action::SetVoiceScope(voice.id, true))];
        self.draw_choice_row("Recognised", &scope, inner, y + MODEL_ROW, panel, regions)
    }
}
