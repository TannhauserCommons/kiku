//! History's drawing: the search box and its filters over past sessions or what the search found, SQL
//! mode with its rows as a table, and one session's lines.

use std::ops::Range;

use windows::{
    core::Result,
    Win32::Graphics::{
        Direct2D::Common::D2D_RECT_F,
        DirectWrite::{IDWriteTextLayout, DWRITE_HIT_TEST_METRICS},
    },
};

use super::{
    clipped, estimated_height,
    design::{background_at, with_alpha, BODY_SIZE, CONTROL, RADIUS_CONTROL, SETTING_ROW},
    expand, height, height_of, metrics, pointer_on, rect, share_space, width, Emphasis, Panel, Renderer, Tone, BUTTON, SECTION_HEADER,
    THUMB_OFFSET, TOOLBAR_INSET,
};
use crate::overlay::{
    field::Field,
    finder::{session_apps, session_length, Period, TextBox, Who, EXAMPLES},
    render::Icon,
    rows::Rows,
    view::{Action, Dropdown, Extent, Regions, ScrollArea, Target},
};
use kiku_core::{history::Table, transcripts};

/// A past session or a search result: a line of text over a line of detail.
const LIST_ROW: f32 = 46.0;
/// A row of the SQL's table.
const TABLE_ROW: f32 = 24.0;
/// The widest a table column asks to be before the columns share the width.
const COLUMN_MAX: f32 = 320.0;
/// Rows of the table measured for its column widths, so the columns stay put while it scrolls.
const COLUMN_SAMPLE: usize = 50;
/// The most lines the SQL box grows to before its text scrolls.
const SQL_LINES: usize = 4;
/// Between a text box's edge and its text, and the room an icon at its edge takes.
const BOX_PAD: f32 = 10.0;
pub(super) const BOX_ICON: f32 = 28.0;

/// What a text box is for: one line or wrapping, whether keys go to it, and what clicking it and its clear
/// button (shown on one line once there is text) do.
pub(super) struct BoxRole {
    pub one_line: bool,
    /// Before a one-line box's text, e.g. a search's magnifier.
    pub icon: Option<Icon>,
    pub typing: bool,
    pub click: Option<Action>,
    pub clear: Option<Action>,
}

/// A history box: keys go to it once clicked, and the search box has a clear button.
fn history_box(text_box: TextBox, panel: &Panel) -> BoxRole {
    let search = text_box == TextBox::Search;
    BoxRole { one_line: search, icon: search.then_some(Icon::Search), typing: panel.view.finder.typing == Some(text_box), click: Some(Action::Type(text_box)), clear: search.then_some(Action::ClearSearch) }
}

impl Renderer {
    /// History: a back button, its heading and the Search | SQL switch, then the mode that is on.
    pub(super) fn draw_history(&mut self, panel: &Panel, content: D2D_RECT_F, regions: &mut Regions) -> Result<()> {
        let finder = &panel.view.finder;
        let top = self.draw_screen_header("History", Action::ShowLive, content, panel, regions)?;
        let modes = [("Search", !finder.sql_mode, Action::ShowSql(false)), ("SQL", finder.sql_mode, Action::ShowSql(true))];
        // The mode that is on isn't clicked again.
        let modes: Vec<_> = modes.iter().map(|&(label, on, action)| (label, on, (!on).then_some(action))).collect();
        self.draw_segments(&modes, content.right, content.top + SECTION_HEADER / 2.0, panel, regions)?;
        let body = rect(content.left, top, content.right, content.bottom);
        if finder.sql_mode { self.draw_sql(panel, body, regions) } else { self.draw_search(panel, body, regions) }
    }

    /// The search box and the filters, over past sessions or, once either is set, what they found.
    fn draw_search(&mut self, panel: &Panel, area: D2D_RECT_F, regions: &mut Regions) -> Result<()> {
        let finder = &panel.view.finder;
        let search_box = self.draw_text_box(&finder.search, "Search everything said", area, history_box(TextBox::Search, panel), panel, regions)?;
        let middle = search_box.bottom + 8.0 + CONTROL / 2.0;
        let who_box = self.draw_filters(panel, area, middle, regions)?;
        let body = rect(area.left, middle + CONTROL / 2.0 + 8.0, area.right, area.bottom);
        if finder.searching() {
            self.draw_results(panel, body, regions)?;
        } else {
            self.draw_sessions(panel, body, regions)?;
        }
        if panel.view.open_dropdown == Some(Dropdown::Who) {
            let choices = finder.who_choices();
            let names: Vec<String> = choices.iter().map(Who::name).collect();
            let options: Vec<_> =
                choices.iter().zip(&names).enumerate().map(|(index, (who, name))| (name.as_str(), *who == finder.who, Action::SetWho(index))).collect();
            self.draw_dropdown_list(Dropdown::Who, who_box, area.bottom, &options, panel, regions)?;
        }
        Ok(())
    }

    /// How far back and whose lines, on the right; on the left how many lines were found, with buttons to
    /// copy them all or ask Claude about them, or how many sessions there are. Returns where the who box is.
    fn draw_filters(&mut self, panel: &Panel, area: D2D_RECT_F, middle: f32, regions: &mut Regions) -> Result<D2D_RECT_F> {
        let finder = &panel.view.finder;
        let periods: Vec<_> = Period::ALL.iter().map(|&p| (p.name(), p == finder.period, (p != finder.period).then_some(Action::SetPeriod(p)))).collect();
        let periods_left = self.draw_segments(&periods, area.right, middle, panel, regions)?;
        let who = self.draw_dropdown_button(&finder.who.name(), Action::ToggleDropdown(Dropdown::Who), periods_left - 8.0, middle - SETTING_ROW / 2.0, panel, regions)?;
        let found = &finder.found;
        let count = match finder.searching() {
            true if found.loading && found.lines.is_empty() => "Searching…".to_owned(),
            true => format!("{} line{}", found.total, if found.total == 1 { "" } else { "s" }),
            false => format!("{} session{}", panel.view.saved.len(), if panel.view.saved.len() == 1 { "" } else { "s" }),
        };
        let count = self.single_line(&self.fonts.small, &count, who.left - area.left - 12.0)?;
        self.draw_layout(&count, area.left + 4.0, middle - height_of(&count)? / 2.0, self.palette.text_dim)?;
        if finder.searching() && !found.lines.is_empty() {
            let mut x = area.left + 8.0 + metrics(&count)?.width;
            for (icon, action, tone) in [(Icon::Copy, Action::CopyResults, Tone::Dim), (Icon::Sparkles, Action::ShareResults, Tone::Accent)] {
                if x + BUTTON > who.left - 4.0 {
                    break;
                }
                self.icon_button(rect(x, middle - BUTTON / 2.0, x + BUTTON, middle + BUTTON / 2.0), icon, action, tone, panel, regions)?;
                x += BUTTON;
            }
        }
        Ok(who)
    }

    /// A text box across the top of `area`: its text, or `placeholder` while empty, and while keys go to it
    /// the caret. A one-line box slides to keep the caret in view, after its icon if any; a wrapping one grows
    /// up to [`SQL_LINES`] lines. Returns where the box is.
    pub(super) fn draw_text_box(&mut self, field: &Field, placeholder: &str, area: D2D_RECT_F, role: BoxRole, panel: &Panel, regions: &mut Regions) -> Result<D2D_RECT_F> {
        let palette = self.palette.clone();
        let (one_line, typing) = (role.one_line, role.typing);
        let clear = role.clear.filter(|_| one_line && !field.text.is_empty());
        let icon = role.icon.filter(|_| one_line);
        let text_left = area.left + if icon.is_some() { BOX_ICON } else { BOX_PAD };
        let text_right = area.right - if clear.is_some() { BOX_ICON } else { BOX_PAD };
        let text_width = text_right - text_left;
        let layout = self.layout(&self.fonts.body, &field.text, if one_line { f32::MAX } else { text_width })?;
        let lines = super::line_heights(&layout)?;
        let line = lines.first().copied().unwrap_or(0.0);
        let shown_height: f32 = lines.iter().take(SQL_LINES).sum();
        let box_height = if one_line { CONTROL } else { (shown_height + 2.0 * (CONTROL - line) / 2.0).max(CONTROL) };
        let area = rect(area.left, area.top, area.right, area.top + box_height);

        // A box clicked into rings while keys go to it; one always typed in (the font list's) needs none.
        if typing && role.click.is_some() {
            // The accent around the box, with the window's own colour under the box's glass.
            self.fill_rounded(expand(area, 2.0), RADIUS_CONTROL + 2.0, with_alpha(palette.accent, 0.35))?;
            self.fill_rounded(area, RADIUS_CONTROL, background_at(&palette, (area.top + area.bottom) / 2.0, panel.height))?;
        }
        self.glass(area, RADIUS_CONTROL, palette.track)?;
        if let Some(action) = role.click {
            if !typing && pointer_on(panel, action).0 {
                self.fill_rounded(area, RADIUS_CONTROL, palette.hover)?;
            }
            regions.targets.push((area, Target::Click(action)));
        }
        let middle = area.top + CONTROL / 2.0;
        if let Some(icon) = icon {
            self.icon(icon, rect(area.left + 9.0, middle - 7.0, area.left + 23.0, middle + 7.0), palette.text_faint)?;
        }
        if let Some(action) = clear {
            let button = rect(area.right - BOX_ICON + 2.0, middle - 12.0, area.right - 2.0, middle + 12.0);
            self.icon_button(button, Icon::Close, action, Tone::Faint, panel, regions)?;
        }

        let caret = caret_at(&layout, field.caret_utf16())?;
        // The text slides left past the caret on one line, or up to the caret's line in a box full of lines.
        let x = text_left - if one_line { (caret.0 - text_width + 2.0).max(0.0) } else { 0.0 };
        let y = middle - line / 2.0 - if one_line { 0.0 } else { (caret.1 + caret.2 - shown_height).max(0.0) };
        self.push_clip(rect(text_left - 1.0, area.top + 2.0, text_right + 1.0, area.bottom - 2.0));
        if field.text.is_empty() {
            let placeholder = self.single_line(&self.fonts.body, placeholder, text_width)?;
            self.draw_layout(&placeholder, text_left, y, palette.text_faint)?;
        } else {
            self.draw_layout(&layout, x, y, palette.text)?;
        }
        if typing {
            let (caret_x, caret_y) = (x + caret.0, y + caret.1);
            self.fill_rounded(rect(caret_x - 0.75, caret_y, caret_x + 0.75, caret_y + caret.2), 0.0, palette.accent)?;
        }
        self.pop_clip();
        Ok(area)
    }

    /// What the search found, newest first: each line's text from shortly before its first match, the
    /// search words marked, over who said it and when. Under the pointer a line shows buttons to copy it or
    /// ask Claude about it; clicking it opens its session there.
    fn draw_results(&mut self, panel: &Panel, body: D2D_RECT_F, regions: &mut Regions) -> Result<()> {
        let (finder, palette) = (&panel.view.finder, self.palette.clone());
        let lines = &finder.found.lines;
        if lines.is_empty() {
            if !finder.found.loading {
                let hint = "Fewer words, or a wider time or who, finds more. The word being typed also finds longer words it starts.";
                self.draw_empty(Icon::Search, "Nothing found", hint, body)?;
            }
            return Ok(());
        }
        let (area, extent, top) = self.list_area(panel, body, ScrollArea::Results, lines.len(), regions);
        // About how many characters a row shows, at half the text size each; a match further in is brought into view.
        let fits = ((width(&area) - 16.0) / (BODY_SIZE * 0.5)) as usize;
        self.push_clip(area);
        for index in shown_rows(top, extent, LIST_ROW, lines.len()) {
            let line = &lines[index];
            let y = area.top - top + LIST_ROW * index as f32;
            if index > 0 {
                self.hairline(area.left + 8.0, area.right - 8.0, y)?;
            }
            let row = rect(area.left, y + 2.0, area.right, y + LIST_ROW - 2.0);
            let actions = [Action::OpenResult(index), Action::CopyResult(index), Action::ShareResult(index)];
            let hovered = panel.view.hovered.is_some_and(|action| actions.contains(&action));
            if hovered {
                self.fill_rounded(row, RADIUS_CONTROL, palette.hover)?;
            }
            regions.targets.push((clipped(row, area), Target::Click(Action::OpenResult(index))));
            let mut right = row.right - 8.0;
            if hovered {
                let buttons = vec![(Icon::Copy, actions[1], Tone::Dim), (Icon::Sparkles, actions[2], Tone::Accent)];
                right = self.clipped_toolbar(&[buttons], row.right - TOOLBAR_INSET, (row.top + row.bottom) / 2.0, area, panel, regions)? - 6.0;
            }
            let speaker = match &line.talking_to {
                Some(app) if line.you => format!("{} (to {app})", line.speaker),
                _ => line.speaker.clone(),
            };
            let snippet = finder.snippet(&line.text, fits);
            let detail = format!("{speaker}  ·  {}", transcripts::friendly_time(line.at));
            self.draw_list_row(&snippet, &finder.matches(&snippet), &detail, row, right)?;
        }
        self.pop_clip();
        self.draw_scroll_thumb(area.right + 3.0, area, extent, top)
    }

    /// Past sessions, newest first: the apps heard over when, for how long and how many lines. Under the
    /// pointer a session shows a delete button, which asks once more before deleting.
    fn draw_sessions(&mut self, panel: &Panel, body: D2D_RECT_F, regions: &mut Regions) -> Result<()> {
        let (saved, palette) = (&panel.view.saved, self.palette.clone());
        if saved.is_empty() {
            let hint = "In auto save mode lines are saved as they're said; in manual mode, press a save button.";
            return self.draw_empty(Icon::History, "Nothing saved yet", hint, body);
        }
        let (area, extent, top) = self.list_area(panel, body, ScrollArea::SavedList, saved.len(), regions);
        self.push_clip(area);
        for index in shown_rows(top, extent, LIST_ROW, saved.len()) {
            let session = &saved[index];
            let y = area.top - top + LIST_ROW * index as f32;
            if index > 0 {
                self.hairline(area.left + 8.0, area.right - 8.0, y)?;
            }
            let row = rect(area.left, y + 2.0, area.right, y + LIST_ROW - 2.0);
            let (open, delete) = (Action::OpenSaved(index), Action::DeleteSession(index));
            let hovered = matches!(panel.view.hovered, Some(action) if action == open || action == delete);
            let confirming = panel.view.finder.deleting == Some(session.id);
            if hovered {
                self.fill_rounded(row, RADIUS_CONTROL, palette.hover)?;
            }
            regions.targets.push((clipped(row, area), Target::Click(open)));
            let middle = (row.top + row.bottom) / 2.0;
            let right = if confirming {
                let label = format!("Delete {} line{}", session.lines, if session.lines == 1 { "" } else { "s" });
                let first = regions.targets.len();
                let left = self.draw_text_button(&label, Some(Icon::Trash), delete, Emphasis::Danger, row.right - 8.0, middle, panel, regions)?;
                clip_targets(regions, first, area);
                left - 8.0
            } else if hovered {
                self.clipped_toolbar(&[vec![(Icon::Trash, delete, Tone::Dim)]], row.right - TOOLBAR_INSET, middle, area, panel, regions)? - 6.0
            } else {
                let chevron = rect(row.right - 26.0, middle - 7.0, row.right - 12.0, middle + 7.0);
                self.icon(Icon::ChevronRight, chevron, palette.text_faint)?;
                chevron.left - 8.0
            };
            let when = transcripts::friendly_time(session.started);
            let detail = format!("{when}  ·  {}  ·  {} line{}", session_length(session), session.lines, if session.lines == 1 { "" } else { "s" });
            self.draw_list_row(&session_apps(session), &[], &detail, row, right)?;
        }
        self.pop_clip();
        self.draw_scroll_thumb(area.right + 3.0, area, extent, top)
    }

    /// A list's scroll area in `body`, reaching past it so the rows' text lines up with the heading, holding
    /// `count` rows: where it is, its extent and how far it is scrolled.
    fn list_area(&self, panel: &Panel, body: D2D_RECT_F, scroll: ScrollArea, count: usize, regions: &mut Regions) -> (D2D_RECT_F, Extent, f32) {
        let area = rect(body.left - 8.0, body.top, body.right + 8.0, body.bottom);
        let extent = Extent { content: LIST_ROW * count as f32, viewport: height(&area) };
        regions.scroll_areas.push((area, scroll, extent));
        (area, extent, panel.view.scroll(scroll).top(extent))
    }

    /// A list row's text, cut to end at `right`, with `marks` (UTF-16 ranges) highlighted, over a line of detail.
    fn draw_list_row(&self, text: &str, marks: &[Range<u32>], detail: &str, row: D2D_RECT_F, right: f32) -> Result<()> {
        let left = row.left + 8.0;
        let text = self.single_line(&self.fonts.body, text, right - left)?;
        let detail = self.single_line(&self.fonts.small, detail, right - left)?;
        let (text_height, detail_height) = (height_of(&text)?, height_of(&detail)?);
        let top = (row.top + row.bottom) / 2.0 - (text_height + detail_height + 2.0) / 2.0;
        for mark in marks {
            self.fill_text_range(&text, mark.start, mark.end, left, top, with_alpha(self.palette.accent, 0.3))?;
        }
        self.draw_layout(&text, left, top, self.palette.text)?;
        self.draw_layout(&detail, left, top + text_height + 2.0, self.palette.text_dim)
    }

    /// A floating toolbar on a row of a list in `area`, whose buttons can't be clicked outside it. Returns its left edge.
    #[allow(clippy::too_many_arguments)]
    fn clipped_toolbar(&mut self, groups: &[Vec<(Icon, Action, Tone)>], right: f32, middle: f32, area: D2D_RECT_F, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        let first = regions.targets.len();
        let left = self.draw_toolbar(groups, right, middle, panel, regions)?;
        clip_targets(regions, first, area);
        Ok(left)
    }

    /// SQL mode: the query box, examples to run and Run, then the rows as a table, or why the query failed.
    /// Before anything has run it says what can be asked.
    fn draw_sql(&mut self, panel: &Panel, area: D2D_RECT_F, regions: &mut Regions) -> Result<()> {
        let finder = &panel.view.finder;
        let placeholder = "SELECT … : one SQLite query that only reads. Enter runs it.";
        let sql_box = self.draw_text_box(&finder.sql, placeholder, area, history_box(TextBox::Sql, panel), panel, regions)?;
        let middle = sql_box.bottom + 8.0 + CONTROL / 2.0;
        let run_left = self.draw_text_button("Run", Some(Icon::Play), Action::RunSql, Emphasis::Primary, area.right, middle, panel, regions)?;
        // As many examples as fit before Run, left to right.
        let mut x = area.left;
        for (index, (name, _)) in EXAMPLES.iter().enumerate() {
            let right = x + (metrics(&self.layout(&self.fonts.button, name, f32::MAX)?)?.width + 28.0).ceil();
            if right > run_left - 8.0 {
                break;
            }
            self.draw_text_button(name, None, Action::Example(index), Emphasis::Secondary, right, middle, panel, regions)?;
            x = right + 6.0;
        }
        let body = rect(area.left, middle + CONTROL / 2.0 + 8.0, area.right, area.bottom);
        match &finder.table {
            None if finder.running => Ok(()),
            None => {
                let hint = "Tables: lines, speakers and sessions. Times are Unix seconds: datetime(at, 'unixepoch', 'localtime') shows them.";
                self.draw_empty(Icon::Database, "Ask your transcripts anything", hint, body)
            }
            Some(Err(error)) => {
                let error = self.layout(&self.fonts.body, &format!("The query failed: {error}"), width(&body) - 8.0)?;
                self.draw_layout(&error, body.left + 4.0, body.top + 4.0, self.palette.error)
            }
            Some(Ok(table)) => self.draw_table(table, finder.running, panel, body, regions),
        }
    }

    /// How many rows there are, with buttons to copy them or ask Claude about them, over the rows under a
    /// header that stays put. Columns ask for the width their text needs and share what there is.
    fn draw_table(&mut self, table: &Table, running: bool, panel: &Panel, body: D2D_RECT_F, regions: &mut Regions) -> Result<()> {
        let palette = self.palette.clone();
        let middle = body.top + BUTTON / 2.0;
        let mut right = body.right + 6.0;
        for (icon, action, tone) in [(Icon::Sparkles, Action::ShareResults, Tone::Accent), (Icon::Copy, Action::CopyResults, Tone::Dim)] {
            self.icon_button(rect(right - BUTTON, middle - BUTTON / 2.0, right, middle + BUTTON / 2.0), icon, action, tone, panel, regions)?;
            right -= BUTTON;
        }
        let rows = table.rows.len();
        let status = match (running, table.more) {
            (true, _) => "Running…".to_owned(),
            (false, true) => format!("The first {rows} rows: aggregate, or page with LIMIT and OFFSET, for the rest"),
            (false, false) => format!("{rows} row{}", if rows == 1 { "" } else { "s" }),
        };
        let status = self.single_line(&self.fonts.small, &status, right - body.left - 8.0)?;
        self.draw_layout(&status, body.left + 4.0, middle - height_of(&status)? / 2.0, palette.text_dim)?;

        let mut wanted: Vec<f32> = Vec::with_capacity(table.columns.len());
        for (column, name) in table.columns.iter().enumerate() {
            let mut widest = metrics(&self.layout(&self.fonts.strong, name, f32::MAX)?)?.width;
            for cells in table.rows.iter().take(COLUMN_SAMPLE) {
                widest = widest.max(metrics(&self.layout(&self.fonts.small, &one_line(&cells[column]), f32::MAX)?)?.width);
            }
            wanted.push((widest + 16.0).min(COLUMN_MAX));
        }
        let widths = share_space(&wanted, width(&body));
        let header = rect(body.left, middle + BUTTON / 2.0 + 4.0, body.right, middle + BUTTON / 2.0 + 4.0 + TABLE_ROW);
        self.draw_table_row(&table.columns, &widths, header, true)?;
        self.hairline(body.left, body.right, header.bottom)?;

        let area = rect(body.left, header.bottom + 1.0, body.right, body.bottom);
        let extent = Extent { content: TABLE_ROW * rows as f32, viewport: height(&area) };
        regions.scroll_areas.push((area, ScrollArea::Table, extent));
        let top = panel.view.scroll(ScrollArea::Table).top(extent);
        self.push_clip(area);
        for index in shown_rows(top, extent, TABLE_ROW, rows) {
            let y = area.top - top + TABLE_ROW * index as f32;
            let row = rect(area.left, y, area.right, y + TABLE_ROW);
            if index % 2 == 1 {
                self.fill_rounded(row, 4.0, palette.hover)?;
            }
            self.draw_table_row(&table.rows[index], &widths, row, false)?;
        }
        self.pop_clip();
        self.draw_scroll_thumb(area.right + THUMB_OFFSET, area, extent, top)
    }

    /// One row of cells in columns `widths` wide, each cut to fit; the header's in a stronger weight.
    fn draw_table_row(&self, cells: &[String], widths: &[f32], row: D2D_RECT_F, header: bool) -> Result<()> {
        let (format, colour) = if header { (&self.fonts.strong, self.palette.text_dim) } else { (&self.fonts.small, self.palette.text) };
        let mut x = row.left;
        for (cell, &column) in cells.iter().zip(widths) {
            let text = self.single_line(format, &one_line(cell), column - 12.0)?;
            self.draw_layout(&text, x + 6.0, (row.top + row.bottom - height_of(&text)?) / 2.0, colour)?;
            x += column;
        }
        Ok(())
    }

    /// One past session's lines under its title. One opened from the search starts with that line in view
    /// and marked; the search words are marked wherever they show.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_saved_file(
        &mut self,
        panel: &Panel,
        session: i64,
        title: &str,
        lines: &[String],
        focus: Option<usize>,
        content: D2D_RECT_F,
        regions: &mut Regions,
    ) -> Result<()> {
        let body_top = self.draw_screen_header(title, Action::ShowSaved, content, panel, regions)?;
        let area = rect(content.left, body_top, content.right, content.bottom);
        // Reopening the running session shows its new lines.
        let key = (session, lines.len());
        let mut rows = match self.saved_rows.take() {
            Some((open, rows)) if open == key => rows,
            _ => {
                let font_size = unsafe { self.fonts.body.GetFontSize() };
                let mut rows = Rows::default();
                lines.iter().for_each(|line| rows.push_estimate(estimated_height(line.len(), font_size, width(&area))));
                rows
            }
        };
        let drawn = self.draw_saved_lines(&mut rows, lines, focus, area, panel, regions);
        self.saved_rows = Some((key, rows));
        drawn
    }

    /// Measures the lines that show until none on screen is an estimate, then draws them. Opened at a line and
    /// not scrolled yet, it shows from a little above that line, and says where that was for the view to keep.
    #[allow(clippy::too_many_arguments)]
    fn draw_saved_lines(&self, rows: &mut Rows, lines: &[String], focus: Option<usize>, area: D2D_RECT_F, panel: &Panel, regions: &mut Regions) -> Result<()> {
        let (text_width, viewport) = (width(&area), height(&area));
        rows.set_width(text_width);
        let reveal = focus.filter(|_| !panel.view.scrolls.contains_key(&ScrollArea::SavedFile));
        let (extent, top) = loop {
            let extent = Extent { content: rows.total(), viewport };
            let top = match reveal {
                Some(row) => (rows.top(row) - viewport / 3.0).clamp(0.0, (extent.content - viewport).max(0.0)),
                None => panel.view.scroll(ScrollArea::SavedFile).top(extent),
            };
            let estimated = rows.estimated_between(top, top + viewport);
            if estimated.is_empty() {
                break (extent, top);
            }
            for row in estimated {
                rows.set_measured(row, height_of(&self.layout(&self.fonts.body, &lines[row], text_width)?)?);
            }
        };
        if reveal.is_some() {
            regions.revealed = Some((ScrollArea::SavedFile, top));
        }
        regions.scroll_areas.push((area, ScrollArea::SavedFile, extent));
        let (finder, palette) = (&panel.view.finder, self.palette.clone());
        self.push_clip(area);
        for row in rows.between(top, top + viewport) {
            let layout = self.layout(&self.fonts.body, &lines[row], text_width)?;
            let y = area.top - top + rows.top(row);
            if focus == Some(row) {
                self.fill_rounded(rect(area.left - 6.0, y - 2.0, area.right + 2.0, y + rows.height(row) + 2.0), RADIUS_CONTROL - 2.0, with_alpha(palette.accent, 0.14))?;
            }
            for mark in finder.matches(&lines[row]) {
                self.fill_text_range(&layout, mark.start, mark.end, area.left, y, with_alpha(palette.accent, 0.3))?;
            }
            self.draw_layout(&layout, area.left, y, palette.text)?;
        }
        self.pop_clip();
        self.draw_scroll_thumb(area.right + THUMB_OFFSET, area, extent, top)
    }
}

/// Where the caret at UTF-16 offset `position` is in laid-out text: its left, top and height.
fn caret_at(layout: &IDWriteTextLayout, position: u32) -> Result<(f32, f32, f32)> {
    let (mut x, mut y, mut hit) = (0.0, 0.0, DWRITE_HIT_TEST_METRICS::default());
    unsafe { layout.HitTestTextPosition(position, false, &mut x, &mut y, &mut hit)? };
    Ok((x, y, hit.height))
}

/// The rows of a list `row` tall that show when it is scrolled to `top`.
fn shown_rows(top: f32, extent: Extent, row: f32, count: usize) -> Range<usize> {
    (top / row) as usize..(((top + extent.viewport) / row).ceil() as usize).min(count)
}

/// Keeps targets added since `first` from reaching outside `area`, the list they float over.
fn clip_targets(regions: &mut Regions, first: usize, area: D2D_RECT_F) {
    for target in &mut regions.targets[first..] {
        target.0 = clipped(target.0, area);
    }
}

/// A cell's text on one line.
fn one_line(text: &str) -> String {
    text.replace(['\n', '\r', '\t'], " ")
}
