//! How a speaker's name shows on a line, in each label style: what goes before the text, what is drawn
//! beside it, and how offsets in the text as copied ("Name: text") map to the text as laid out.

use std::{collections::HashMap, time::Instant};

use kiku_core::{
    captions::{Caption, Captions},
    engine::Line,
    settings::{LabelStyle, Labels},
    voices::book::Place,
};

/// The room left of the text for a turn's rail, and for a round badge.
pub const RAIL_GUTTER: f32 = 12.0;
pub const BADGE_GUTTER: f32 = 30.0;

/// A line's label style, and whether the line starts its turn (a turn is a run of lines with one label).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub style: LabelStyle,
    pub first: bool,
}

/// What is put before a line's text in its layout.
#[derive(Debug, Default, PartialEq)]
pub struct Lead {
    pub text: String,
    /// The UTF-16 units of `text` drawn in the speaker's colour.
    pub marked: u32,
    /// A tinted pill goes behind the marked part.
    pub pill: bool,
    /// The marked part is a small heading on a line of its own.
    pub header: bool,
}

impl Lead {
    /// The lead of a line labelled `label` and `tag`, in `style`; `first` says whether it starts its turn.
    pub fn new(style: LabelStyle, first: bool, label: Option<&str>, tag: Option<&str>) -> Lead {
        let (Some(label), Some(tag)) = (label, tag) else { return Lead::default() };
        let units = |text: &str| text.encode_utf16().count() as u32;
        match style {
            LabelStyle::EveryLine => Lead { text: format!("{tag}: "), marked: units(tag) + 1, ..Lead::default() },
            LabelStyle::Turns if first => Lead::new(LabelStyle::EveryLine, first, Some(label), Some(tag)),
            LabelStyle::Tags => Lead { text: format!("\u{2002}{tag}\u{2002} "), marked: units(tag) + 2, pill: true, header: false },
            LabelStyle::Blocks if first => Lead { text: format!("{label}\n"), marked: units(label), pill: false, header: true },
            _ => Lead::default(),
        }
    }

    pub fn len(&self) -> u32 {
        self.text.encode_utf16().count() as u32
    }
}

/// What a round badge says: the speaker's number, else the first letter of the name.
pub fn badge_text(tag: &str) -> String {
    match tag.rsplit(' ').next().filter(|last| last.chars().all(|c| c.is_ascii_digit())) {
        Some(number) => number.to_owned(),
        None => tag.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default(),
    }
}

impl Shape {
    /// How `caption` shows after `previous`, the line before it in its section.
    pub fn of(caption: &Caption, previous: Option<&Caption>, labels: Labels) -> Shape {
        Shape { style: labels.style(caption.mine), first: previous.is_none_or(|before| before.label() != caption.label()) }
    }

    pub fn lead(self, caption: &Caption) -> Lead {
        Lead::new(self.style, self.first, caption.label(), caption.tag().as_deref())
    }

    /// The room left of the text, the same for every line of a labelled speaker so a turn lines up.
    pub fn indent(self, caption: &Caption) -> f32 {
        match (self.style, caption.label()) {
            (_, None) => 0.0,
            (LabelStyle::Turns | LabelStyle::Colour, _) => RAIL_GUTTER,
            (LabelStyle::Badges, _) => BADGE_GUTTER,
            _ => 0.0,
        }
    }

    /// Whether a rail runs down the line's left.
    pub fn rail(self, caption: &Caption) -> bool {
        caption.label().is_some() && matches!(self.style, LabelStyle::Turns | LabelStyle::Colour)
    }

    /// Whether a round badge starts the line.
    pub fn badge(self, caption: &Caption) -> bool {
        caption.label().is_some() && self.first && self.style == LabelStyle::Badges
    }

    /// Where the text laid out starts in the text as copied: after "Name: ".
    fn copied_lead(caption: &Caption) -> u32 {
        caption.label().map_or(0, |_| caption.label_len() as u32 + 1)
    }

    /// An offset in the copied text ("Name: text") as an offset in the laid out text; inside the name it
    /// snaps to the start or end of what stands for it.
    pub fn to_laid_out(self, caption: &Caption, copied: u32) -> u32 {
        let (copied_lead, lead) = (Self::copied_lead(caption), self.lead(caption).len());
        match copied {
            0 => 0,
            _ if copied >= copied_lead => copied - copied_lead + lead,
            _ => lead,
        }
    }

    /// The reverse of [`Shape::to_laid_out`].
    pub fn to_copied(self, caption: &Caption, laid_out: u32) -> u32 {
        let (copied_lead, lead) = (Self::copied_lead(caption), self.lead(caption).len());
        match laid_out {
            _ if laid_out >= lead => laid_out - lead + copied_lead,
            _ if laid_out * 2 < lead => 0,
            _ => copied_lead,
        }
    }
}

/// A call of three speakers (the second named) and a second voice at your mic, for the settings' previews.
pub fn sample() -> Captions {
    let (mut captions, now) = (Captions::default(), Instant::now());
    let app = captions.add_app("Discord", None);
    let place = Place::App("Discord".to_owned());
    captions.set_stream_place(1, place.clone());
    let said = |id, voice, text: &str| Line { id, text: text.into(), complete: true, voice: Some(voice) };
    let call = [(1, "Did the build pass?"), (1, "I checked it twice."), (0, "Yes, it did.")];
    let lines: Vec<Line> = call.iter().enumerate().map(|(id, &(voice, text))| said(id as u64, voice, text)).collect();
    captions.update_app(app, 1, &lines, now);
    captions.set_names(app, &place, HashMap::from([(1, "Maya".to_owned())]));
    let mine = [(0, "Did the build pass?"), (0, "I checked it twice."), (1, "Yes, it did.")];
    let lines: Vec<Line> = mine.iter().enumerate().map(|(id, &(voice, text))| said(id as u64, voice, text)).collect();
    captions.update_mic(2, &lines, None);
    captions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call() -> Captions {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("Discord", None);
        let said = |id, voice| Line { id, text: format!("line {id}"), complete: true, voice: Some(voice) };
        captions.update_app(app, 1, &[said(0, 0), said(1, 0), said(2, 1)], now);
        captions
    }

    #[test]
    fn a_turn_is_a_run_of_lines_with_one_label() {
        let captions = call();
        let lines = &captions.sections[1].captions;
        let shapes: Vec<bool> = (0..3).map(|i| Shape::of(&lines[i], i.checked_sub(1).map(|p| &lines[p]), Labels::default()).first).collect();
        assert_eq!(shapes, [true, false, true]);
    }

    #[test]
    fn each_style_puts_its_own_lead_before_the_text() {
        let lead = |style, first| Lead::new(style, first, Some("Speaker 3"), Some("3"));
        assert_eq!(lead(LabelStyle::EveryLine, false).text, "3: ", "an unnamed speaker is short");
        assert_eq!(lead(LabelStyle::Turns, true).text, "3: ");
        assert_eq!(lead(LabelStyle::Turns, false), Lead::default());
        assert_eq!(lead(LabelStyle::Tags, false).text, "\u{2002}3\u{2002} ");
        assert!(lead(LabelStyle::Tags, false).pill);
        assert_eq!((lead(LabelStyle::Blocks, true).text.as_str(), lead(LabelStyle::Blocks, false).len()), ("Speaker 3\n", 0));
        assert_eq!((lead(LabelStyle::Colour, true).len(), lead(LabelStyle::Badges, true).len()), (0, 0));
        assert_eq!(Lead::new(LabelStyle::Tags, true, None, None), Lead::default(), "an unlabelled line has no lead");
    }

    #[test]
    fn badges_say_the_number_or_the_initial() {
        assert_eq!((badge_text("3").as_str(), badge_text("maya").as_str(), badge_text("Mic 2").as_str(), badge_text("You").as_str()), ("3", "M", "2", "Y"));
    }

    #[test]
    fn copied_offsets_and_laid_out_offsets_meet_at_the_text() {
        let captions = call();
        let caption = &captions.sections[1].captions[1];
        let shape = Shape { style: LabelStyle::Turns, first: false };
        let copied = caption.display_text();
        assert_eq!(copied, "Speaker 1: line 1");
        assert_eq!(shape.to_laid_out(caption, 0), 0);
        assert_eq!(shape.to_laid_out(caption, 11), 0, "the start of the text");
        assert_eq!(shape.to_laid_out(caption, 13), 2);
        assert_eq!(shape.to_copied(caption, 2), 13);
        assert_eq!(shape.to_copied(caption, 0), 11, "no lead: the first character is the text's");
        let every = Shape { style: LabelStyle::EveryLine, first: true };
        assert_eq!(every.to_laid_out(caption, 11), 3, "\"Speaker 1: \" is copied, \"1: \" shown");
        assert!((0..=copied.len() as u32).filter(|&d| d == 0 || d >= 11).all(|d| every.to_copied(caption, every.to_laid_out(caption, d)) == d), "with the name in the layout, the text maps one to one");
    }
}
