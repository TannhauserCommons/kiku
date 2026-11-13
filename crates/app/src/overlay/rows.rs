//! Heights of a long list of wrapped rows, such as a section's captions or a past session's lines,
//! so a frame lays out only the rows on screen.

use std::{collections::VecDeque, ops::Range};

use kiku_core::captions::{Caption, Section};

/// Running heights of rows wrapped at one width. A row not yet laid out at that width, because it is
/// new or the width changed, carries an estimate until it comes on screen and is measured.
#[derive(Default)]
pub struct Rows {
    width: f32,
    rows: VecDeque<Row>,
}

#[derive(Clone, Copy)]
struct Row {
    /// Where the row starts, counted from the first row ever added, so dropping old rows moves nothing.
    start: f64,
    height: f32,
    /// Measured at `width`, not estimated.
    measured: bool,
}

impl Row {
    fn end(&self) -> f64 {
        self.start + self.height as f64
    }
}

impl Rows {
    /// Wraps at `width` from now on; rows measured at another width keep their height as an estimate.
    pub fn set_width(&mut self, width: f32) {
        if width != self.width {
            self.width = width;
            self.rows.iter_mut().for_each(|row| row.measured = false);
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The height of all the rows together.
    pub fn total(&self) -> f32 {
        self.rows.back().map_or(0.0, |last| (last.end() - self.origin()) as f32)
    }

    /// Where row `index` starts, from the top of the first row.
    pub fn top(&self, index: usize) -> f32 {
        (self.rows[index].start - self.origin()) as f32
    }

    pub fn height(&self, index: usize) -> f32 {
        self.rows[index].height
    }

    /// The rows that show between `top` and `bottom`, found by binary search.
    pub fn between(&self, top: f32, bottom: f32) -> Range<usize> {
        let (top, bottom) = (self.origin() + top as f64, self.origin() + bottom as f64);
        let first = self.rows.partition_point(|row| row.end() <= top);
        let end = self.rows.partition_point(|row| row.start < bottom);
        first..end.max(first)
    }

    /// The rows between `top` and `bottom` whose height is only an estimate.
    pub fn estimated_between(&self, top: f32, bottom: f32) -> Vec<usize> {
        self.between(top, bottom).filter(|&index| !self.rows[index].measured).collect()
    }

    /// Adds a row at the end with an estimated height.
    pub fn push_estimate(&mut self, height: f32) {
        let start = self.rows.back().map_or(0.0, Row::end);
        self.rows.push_back(Row { start, height, measured: false });
    }

    /// Records row `index`'s measured height; the rows after it move by the difference.
    pub fn set_measured(&mut self, index: usize, height: f32) {
        let change = (height - self.rows[index].height) as f64;
        self.rows[index] = Row { height, measured: true, ..self.rows[index] };
        self.rows.range_mut(index + 1..).for_each(|row| row.start += change);
    }

    /// Gives row `index` an estimated height again, e.g. because what it shows changed.
    pub fn set_estimate(&mut self, index: usize, height: f32) {
        self.set_measured(index, height);
        self.rows[index].measured = false;
    }

    /// Drops the first `count` rows.
    pub fn drop_front(&mut self, count: usize) {
        self.rows.drain(..count.min(self.rows.len()));
    }

    /// Drops the rows from `len` on.
    pub fn truncate(&mut self, len: usize) {
        self.rows.truncate(len);
    }

    fn origin(&self) -> f64 {
        self.rows.front().map_or(0.0, |first| first.start)
    }
}

/// A section's caption rows, kept in step with the section from frame to frame.
#[derive(Default)]
pub struct SectionRows {
    pub rows: Rows,
    /// The section's revision and dropped lines when last synced.
    revision: u64,
    dropped: u64,
}

impl SectionRows {
    /// Drops the rows of lines the section dropped, and gives new and revised lines an `estimate`d height
    /// (from the line before it and the line itself).
    pub fn sync(&mut self, section: &Section, estimate: impl Fn(Option<&Caption>, &Caption) -> f32) {
        let dropped = (section.dropped() - self.dropped) as usize;
        self.rows.drop_front(dropped);
        // The line that is now first may have been in the middle of a turn, and shows differently.
        if dropped > 0 && !self.rows.is_empty() && !section.captions.is_empty() {
            self.rows.set_estimate(0, estimate(None, &section.captions[0]));
        }
        let changed = section.changed_since(self.revision).min(self.rows.len());
        self.rows.truncate(changed);
        for row in changed..section.captions.len() {
            self.rows.push_estimate(estimate(row.checked_sub(1).map(|before| &section.captions[before]), &section.captions[row]));
        }
        (self.revision, self.dropped) = (section.revision(), section.dropped());
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use kiku_core::{captions::Captions, engine::Line};

    fn rows(heights: &[f32]) -> Rows {
        let mut rows = Rows::default();
        heights.iter().for_each(|&height| rows.push_estimate(height));
        rows
    }

    #[test]
    fn finds_the_rows_on_screen() {
        let rows = rows(&[10.0, 20.0, 30.0, 40.0]);
        assert_eq!(rows.total(), 100.0);
        assert_eq!(rows.between(0.0, 10.0), 0..1);
        assert_eq!(rows.between(15.0, 35.0), 1..3);
        assert_eq!(rows.between(60.0, 200.0), 3..4);
        assert_eq!(rows.between(100.0, 120.0), 4..4);
    }

    #[test]
    fn a_measured_row_moves_the_rows_after_it() {
        let mut rows = rows(&[10.0, 10.0, 10.0]);
        rows.set_width(100.0);
        assert_eq!(rows.estimated_between(0.0, 30.0), [0, 1, 2]);
        rows.set_measured(1, 25.0);
        assert_eq!((rows.top(2), rows.total()), (35.0, 45.0));
        assert_eq!(rows.estimated_between(0.0, 45.0), [0, 2]);
        rows.set_width(80.0);
        assert_eq!(rows.estimated_between(0.0, 45.0), [0, 1, 2], "a new width makes every height an estimate");
        assert_eq!(rows.height(1), 25.0, "but keeps it as the estimate");
    }

    #[test]
    fn the_line_that_becomes_first_is_estimated_again() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("App", None);
        let line = |id| Line { id, text: format!("line {id}"), complete: true, voice: Some(0) };
        let lines: Vec<Line> = (0..10_000).map(line).collect();
        captions.update_app(app, 1, &lines, Instant::now());
        let (mut section, tall_when_first) = (SectionRows::default(), |before: Option<&Caption>, _: &Caption| if before.is_none() { 30.0 } else { 10.0 });
        section.sync(&captions.sections[app], tall_when_first);
        (0..10_000).for_each(|row| section.rows.set_measured(row, 10.0));
        captions.update_app(app, 1, &[line(10_000)], now);
        section.sync(&captions.sections[app], tall_when_first);
        assert_eq!((section.rows.len(), section.rows.height(0)), (10_000, 30.0), "the new first row isn't a stale 10");
        assert_eq!(section.rows.estimated_between(0.0, 30.0), [0]);
    }

    #[test]
    fn dropping_old_rows_keeps_the_others_in_place() {
        let mut rows = rows(&[10.0, 20.0, 30.0]);
        rows.drop_front(1);
        assert_eq!((rows.len(), rows.top(0), rows.top(1), rows.total()), (2, 0.0, 20.0, 50.0));
        rows.truncate(1);
        rows.push_estimate(5.0);
        assert_eq!(rows.total(), 25.0);
    }

    #[test]
    fn section_rows_follow_new_and_revised_lines() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("App", None);
        let line = |id, complete| Line { id, text: format!("line {id}"), complete, voice: None };
        captions.update_app(app, 1, &[line(1, true), line(2, false), line(3, true)], now);
        let mut section = SectionRows::default();
        section.sync(&captions.sections[app], |_, _| 10.0);
        (0..3).for_each(|row| section.rows.set_measured(row, 20.0));
        captions.update_app(app, 1, &[line(2, true), line(4, true)], now);
        section.sync(&captions.sections[app], |_, _| 10.0);
        assert_eq!(section.rows.len(), 4);
        assert_eq!(section.rows.estimated_between(0.0, 100.0), [1, 2, 3], "the revised line, those after it, and the new one");
        assert_eq!(section.rows.total(), 50.0);
    }
}
