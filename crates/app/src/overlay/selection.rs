//! Text selected by dragging across captions, possibly through several apps' sections. Sections are
//! read in the order the panel shows them: the indexes of the open sections, without folded or hidden ones.

use super::view::LineRef;
use kiku_core::captions::{Caption, Captions};

/// A place in a caption: its line, and a UTF-16 offset into the line as shown (with any "You: ").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextPoint {
    pub line: LineRef,
    pub offset: u32,
}

/// From where a drag started to where the pointer is now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
    pub anchor: TextPoint,
    pub focus: TextPoint,
    /// `Captions::relabels` when it was made: later labels moved the text it points into.
    pub relabels: u32,
}

/// A point in reading order: the section as drawn, the caption within it, the offset.
type Place = (usize, usize, u32);

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }

    /// The selected UTF-16 range of each caption it touches, in reading order.
    pub fn spans(&self, captions: &Captions, shown: &[usize]) -> Vec<(LineRef, u32, u32)> {
        self.walk(captions, shown).into_iter().map(|(line, _, start, end)| (line, start, end)).collect()
    }

    /// The selected text, a line per caption.
    pub fn text(&self, captions: &Captions, shown: &[usize]) -> String {
        let pieces: Vec<String> = self
            .walk(captions, shown)
            .into_iter()
            .map(|(_, caption, start, end)| {
                let shown: Vec<u16> = caption.display_text().encode_utf16().collect();
                String::from_utf16_lossy(&shown[start as usize..end as usize])
            })
            .collect();
        pieces.join("\n")
    }

    /// Every caption from the start to the end of the selection, with its selected range.
    /// Empty if a selected line has since been dropped.
    fn walk<'a>(&self, captions: &'a Captions, shown: &[usize]) -> Vec<(LineRef, &'a Caption, u32, u32)> {
        let (Some(a), Some(b)) = (place(captions, shown, self.anchor), place(captions, shown, self.focus)) else { return Vec::new() };
        let ((first_order, first, start), (last_order, last, end)) = if a <= b { (a, b) } else { (b, a) };
        let mut walked = Vec::new();
        for (order, &index) in shown.iter().enumerate().take(last_order + 1).skip(first_order) {
            let section = &captions.sections[index];
            let from_position = if order == first_order { first } else { 0 };
            let to_position = if order == last_order { last + 1 } else { section.captions.len() };
            for (position, caption) in (from_position..to_position).zip(section.captions.range(from_position..to_position)) {
                let here = (order, position);
                let length = caption.display_text().encode_utf16().count() as u32;
                let from = if here == (first_order, first) { start.min(length) } else { 0 };
                let to = if here == (last_order, last) { end.min(length) } else { length };
                walked.push((LineRef::of(index, caption), caption, from, to.max(from)));
            }
        }
        walked
    }
}

fn place(captions: &Captions, shown: &[usize], point: TextPoint) -> Option<Place> {
    let order = shown.iter().position(|&index| index == point.line.section)?;
    let position = point.line.position(&captions.sections[point.line.section].captions)?;
    Some((order, position, point.offset))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use kiku_core::{captions::YOU_SECTION, engine::Line};

    #[test]
    fn selection_runs_in_reading_order_across_sections() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let discord = captions.add_app("Discord", None);
        let line = |id, text: &str| Line { id, text: text.into(), complete: true, voice: None };
        captions.update_app(discord, 1, &[line(0, "Are you joining?"), line(1, "We start soon.")], now);
        captions.update_mic(2, &[line(0, "On my way.")], Some("Discord"));
        let point = |section, stream, id, offset| TextPoint { line: LineRef { section, stream, id }, offset };

        // From "you joining?" in Discord to "On my" in the You section, which is drawn after it.
        let selection = Selection { anchor: point(discord, 1, 0, 4), focus: point(YOU_SECTION, 2, 0, 5), relabels: 0 };
        let expected = "you joining?\nWe start soon.\nYou: On my way.\nOn my";
        let order = [discord, YOU_SECTION];
        assert_eq!(selection.text(&captions, &order), expected);
        let reversed = Selection { anchor: selection.focus, focus: selection.anchor, relabels: 0 };
        assert_eq!(reversed.text(&captions, &order), expected, "dragging upwards selects the same text");
        assert_eq!(selection.text(&captions, &[YOU_SECTION]), "", "a folded section ends the selection");
    }
}
