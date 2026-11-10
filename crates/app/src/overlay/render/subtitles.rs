//! Captions mode's drawing: each line a glossy pill sized to its text, newest at the bottom; the drag
//! tab beside them; and the Appearance tab: the theme's tiles and colours, and captions mode's rows
//! with a preview pill.

use std::path::Path;

use windows::{
    core::{Interface, Result, BOOL, HSTRING},
    Win32::Graphics::{
        Direct2D::{
            Common::{D2D1_COLOR_F, D2D_RECT_F},
            ID2D1RenderTarget,
        },
        DirectWrite::{
            IDWriteLocalizedStrings, IDWriteTextFormat, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_SEMI_BOLD,
            DWRITE_TEXT_RANGE,
        },
    },
};

use super::{
    design::{self, gradient, hex, mix, with_alpha, Palette, CLEAR, RADIUS_CARD, RADIUS_CONTROL, SETTING_ROW},
    expand, height, height_of, line_heights, list_top, metrics, pointer_on, rect, text_format, width, Frame, Panel, Renderer, Surface, HANDLE_HEIGHT,
    HANDLE_STRIP, KNOB, TRACK,
};
use crate::overlay::{
    render::Icon,
    scene::{Glass, Gloss},
    view::{Action, Dropdown, Paint, Regions, Slider, Target},
};
use kiku_core::{
    captions::YOU,
    keybinds::Command,
    settings::{MIN_TEXT_OPACITY, MIN_WINDOW_OPACITY},
    subtitles::{Look, MAX_ROWS, PRESET_FONTS},
    theme::{Colours, Theme, Themes},
};

/// Spacing as fractions of the text size, so every size keeps the same proportions.
const PAD_X: f32 = 0.8;
const PAD_Y: f32 = 0.32;
/// How far the text's halo reaches on each side, in DIPs.
const HALO: f32 = 0.75;
const GAP: f32 = 0.3;
const ICON: f32 = 1.05;
const ICON_GAP: f32 = 0.45;
/// Room around the pills for antialiasing, in DIPs.
const MARGIN: f32 = 4.0;
/// The drag tab's size in DIPs, the same as the panel's.
pub const DRAG_TAB: (f32, f32) = (HANDLE_STRIP, HANDLE_HEIGHT);
const SIZES: [(&str, u16); 4] = [("S", 18), ("M", 22), ("L", 28), ("XL", 34)];
/// The colours a captions swatch offers: light ones for pills, deep ones for text or dark pills.
const CAPTION_COLOURS: [u32; 16] = [
    0xffffff, 0xdde1e6, 0xc2e2f6, 0xd5e68e, 0xf6c3d7, 0xffe8a3, 0xffd2b3, 0xdccdf7, //
    0x111317, 0x222b38, 0x0d2a3d, 0x223010, 0x3b1326, 0x2f6fd6, 0xd9415e, 0xf5b301,
];
/// Window backgrounds: dark ones, then light ones.
const BACKGROUNDS: [u32; 16] = [
    0x1d2633, 0x123a40, 0x26282c, 0x101114, 0x24304a, 0x2a2140, 0x1f2b22, 0x3a2422, //
    0xf4f9fd, 0xf3fbf5, 0xfef4f8, 0xffffff, 0xf5f1e8, 0xeef0f3, 0xfdf6e3, 0xf1eefb,
];
/// Accents: bright ones for dark backgrounds, then deep ones for light backgrounds.
const ACCENTS: [u32; 16] = [
    0x6ccfe0, 0x72e6c3, 0xa9c4e3, 0xf5c451, 0xff9e7a, 0xf48fb1, 0xc3a6f5, 0xb5dc7c, //
    0x0a5296, 0x145f2a, 0x9e1450, 0x6a2bb0, 0xb3261e, 0x8a5300, 0x00695c, 0x3d4a57,
];
/// The theme tiles' height, and the space between them; seven share the card's width.
const TILE_HEIGHT: f32 = 52.0;
const TILE_GAP: f32 = 8.0;

/// One line of captions mode.
pub struct SubtitleLine<'a> {
    pub text: &'a str,
    pub speaker: Speaker<'a>,
}

/// Who a line is from, which is shown in front of it.
#[derive(Clone, Copy)]
pub enum Speaker<'a> {
    /// Your mic: "You: …".
    You,
    /// An app, by its executable's icon.
    App(Option<&'a Path>),
    /// Kiku itself, e.g. how to leave captions mode: nothing in front.
    Kiku,
}

/// Everything a frame of captions mode depends on.
pub struct SubtitleScene<'a> {
    /// Oldest first.
    pub lines: &'a [SubtitleLine<'a>],
    pub look: &'a Look,
    /// The pills' opacity in percent.
    pub opacity: u8,
    /// The app's theme, for the drag tab.
    pub themes: Themes,
    pub rows: usize,
    /// The window's size in DIPs.
    pub width: f32,
    pub height: f32,
    pub dpi: f32,
}

/// A look made ready to draw.
struct Style {
    format: IDWriteTextFormat,
    size: f32,
    /// A single-row pill's height.
    pill: f32,
    text: D2D1_COLOR_F,
    /// Around the text, stronger as the pills get more see-through, so text stays readable over anything.
    halo: D2D1_COLOR_F,
    gloss: Gloss,
}

/// Where the Appearance tab's drop-downs sit, so their lists can be drawn over later rows.
pub(super) struct AppearanceAnchors {
    pub font: D2D_RECT_F,
    /// The theme's colour swatches, in [`Paint::ALL`]'s order.
    pub swatches: [D2D_RECT_F; 4],
}

impl AppearanceAnchors {
    pub fn swatch(&self, paint: Paint) -> D2D_RECT_F {
        self.swatches[Paint::ALL.iter().position(|&p| p == paint).unwrap_or(0)]
    }
}

impl Renderer {
    /// Draws captions mode and the part that changed since the last frame. Returns the frame and the
    /// area the pills cover, in DIPs.
    pub fn render_subtitles<'s>(&mut self, surface: &'s mut Surface, scene: &SubtitleScene) -> Result<(&'s mut Frame, D2D_RECT_F)> {
        let mut block = D2D_RECT_F::default();
        let frame = self.render_on(surface, (scene.width, scene.height), 0.0, scene.dpi, &scene.themes, |renderer| {
            block = renderer.draw_subtitles(scene)?;
            Ok(())
        })?;
        Ok((frame, block))
    }

    /// The tab that drags captions mode, glossy like the pills of `look`; its dots brighten under the pointer.
    pub fn render_drag_tab<'s>(&mut self, surface: &'s mut Surface, hovered: bool, look: &Look, themes: &Themes, dpi: f32) -> Result<&'s mut Frame> {
        self.render_on(surface, DRAG_TAB, 0.0, dpi, themes, |renderer| {
            let tab = rect(0.0, 0.0, DRAG_TAB.0, DRAG_TAB.1);
            renderer.scene.borrow_mut().pill(tab, DRAG_TAB.0 / 2.0, gloss(look.pill, 100));
            renderer.grip(tab, hex(look.text, if hovered { 0.9 } else { 0.55 }))
        })
    }

    /// The window height, in DIPs, that fits `rows` rows of captions even when each is a pill of its own.
    pub fn subtitle_height(&self, look: &Look, rows: usize) -> Result<f32> {
        let style = self.style(look, 100)?;
        Ok(rows as f32 * style.pill + rows.saturating_sub(1) as f32 * GAP * style.size + 2.0 * MARGIN)
    }

    /// The preset fonts, then every other installed font family.
    pub fn font_families(&self) -> Result<Vec<String>> {
        let mut collection = None;
        unsafe { self.dwrite.GetSystemFontCollection(&mut collection, false)? };
        let mut installed = Vec::new();
        if let Some(collection) = collection {
            for index in 0..unsafe { collection.GetFontFamilyCount() } {
                installed.push(english_name(&unsafe { collection.GetFontFamily(index)?.GetFamilyNames()? })?);
            }
        }
        installed.sort_by_key(|name| name.to_lowercase());
        let mut fonts: Vec<String> = PRESET_FONTS.map(str::to_owned).into();
        // Names starting with @ are the sideways fonts for vertical text.
        fonts.extend(installed.into_iter().filter(|name| !name.starts_with('@') && !PRESET_FONTS.contains(&name.as_str())));
        fonts.dedup();
        Ok(fonts)
    }

    /// The newest line at the bottom and older ones above it until `rows` rows show; the oldest
    /// shown may show only its last rows. Returns the area the pills cover.
    fn draw_subtitles(&mut self, scene: &SubtitleScene) -> Result<D2D_RECT_F> {
        let style = self.style(scene.look, scene.opacity)?;
        let (mut bottom, mut rows_left) = (scene.height - MARGIN, scene.rows);
        let mut block: Option<D2D_RECT_F> = None;
        for line in scene.lines.iter().rev() {
            if rows_left == 0 {
                break;
            }
            let (pill, rows) = self.draw_subtitle(line, &style, scene.width / 2.0, bottom, scene.width - 2.0 * MARGIN, rows_left)?;
            block = Some(block.map_or(pill, |block| cover(block, pill)));
            rows_left -= rows;
            bottom = pill.top - GAP * style.size;
        }
        Ok(block.unwrap_or_default())
    }

    /// One line's pill, centred on `center` and ending at `bottom`, showing at most `max_rows` of its
    /// last rows. Returns the pill and how many rows it shows.
    fn draw_subtitle(
        &mut self,
        line: &SubtitleLine,
        style: &Style,
        center: f32,
        bottom: f32,
        max_width: f32,
        max_rows: usize,
    ) -> Result<(D2D_RECT_F, usize)> {
        let (size, pad_x, pad_y) = (style.size, PAD_X * style.size, PAD_Y * style.size);
        let lead = if matches!(line.speaker, Speaker::App(_)) { (ICON + ICON_GAP) * size } else { 0.0 };
        let you = matches!(line.speaker, Speaker::You);
        let text = if you { format!("{YOU}: {}", line.text) } else { line.text.to_owned() };
        let layout = self.layout(&style.format, &text, max_width - 2.0 * pad_x - lead)?;
        if you {
            let prefix = DWRITE_TEXT_RANGE { startPosition: 0, length: YOU.len() as u32 + 1 };
            unsafe { layout.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, prefix)? };
        }
        let rows = line_heights(&layout)?;
        let shown = rows.len().min(max_rows);
        let (hidden, tall) = rows.split_at(rows.len() - shown);
        let (hidden, tall): (f32, f32) = (hidden.iter().sum(), tall.iter().sum());
        let half_width = (metrics(&layout)?.width + 2.0 * pad_x + lead) / 2.0;
        let pill = rect(center - half_width, bottom - tall - 2.0 * pad_y, center + half_width, bottom);
        self.scene.borrow_mut().pill(pill, style.pill / 2.0, style.gloss);
        if let Speaker::App(icon) = line.speaker {
            self.draw_app_icon(icon, pill.left + pad_x, (pill.top + pill.bottom) / 2.0, ICON * size, style)?;
        }
        let (x, y) = (pill.left + pad_x + lead, pill.top + pad_y);
        if hidden > 0.0 {
            self.push_clip(rect(pill.left, y, pill.right, pill.bottom));
        }
        if style.halo.a > 0.0 {
            // Four faint copies make an even edge; one offset copy reads as a misprint.
            let halo = with_alpha(style.halo, style.halo.a / 3.0);
            for (dx, dy) in [(-HALO, 0.0), (HALO, 0.0), (0.0, -HALO), (0.0, HALO)] {
                self.draw_layout(&layout, x + dx, y - hidden + dy, halo)?;
            }
        }
        self.draw_layout(&layout, x, y - hidden, style.text)?;
        if hidden > 0.0 {
            self.pop_clip();
        }
        Ok((pill, shown))
    }

    /// The app's icon in a square of `size` whose left edge is at `x`; a dot when it has none.
    fn draw_app_icon(&mut self, exe: Option<&Path>, x: f32, middle: f32, size: f32, style: &Style) -> Result<()> {
        let target: ID2D1RenderTarget = self.target.cast()?;
        match exe.and_then(|exe| self.icons.get(&target, exe)) {
            Some(bitmap) => self.scene.borrow_mut().bitmap(&bitmap, rect(x, middle - size / 2.0, x + size, middle + size / 2.0)),
            None => self.fill_dot(x + size / 2.0, middle, size * 0.22, style.gloss.rim)?,
        }
        Ok(())
    }

    fn style(&self, look: &Look, opacity: u8) -> Result<Style> {
        let format = self.subtitle_format(look)?;
        let size = look.size as f32;
        let row = height_of(&self.layout(&format, "Ag", f32::MAX)?)?;
        let text = hex(look.text, 1.0);
        // Light text gets a dark halo, dark text a light one; none on solid pills.
        let see_through = 1.0 - opacity as f32 / 100.0;
        let halo = hex(if luminance(text) > 0.5 { 0x000000 } else { 0xffffff }, (see_through * 0.85 * 20.0).round() / 20.0);
        Ok(Style { format, size, pill: row + 2.0 * PAD_Y * size, text, halo, gloss: gloss(look.pill, opacity) })
    }

    /// Made again only when the font or size changes.
    fn subtitle_format(&self, look: &Look) -> Result<IDWriteTextFormat> {
        let mut cached = self.subtitle_format.borrow_mut();
        if let Some((font, size, format)) = cached.as_ref()
            && *font == look.font
            && *size == look.size
        {
            return Ok(format.clone());
        }
        let format = text_format(&self.dwrite, &look.font, look.size as f32, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_STYLE_NORMAL)?;
        *cached = Some((look.font.clone(), look.size, format.clone()));
        Ok(format)
    }

    /// The app's theme: a tile for each preset and one for Custom (a "+" until it is made), the chosen one
    /// ringed; then a swatch for each of its colours, which edit Custom. Returns the y below, and where the
    /// swatches are, so their palettes can be drawn over later rows.
    pub(super) fn draw_theme_card(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<(f32, [D2D_RECT_F; 4])> {
        let y = self.draw_heading("Theme", content, y)?;
        let mut swatches = [D2D_RECT_F::default(); 4];
        let y = self.draw_card(content, y, |renderer, inner, y| {
            let count = Theme::ALL.len() as f32 + 1.0;
            let tile_width = (width(&inner) - (count - 1.0) * TILE_GAP) / count;
            let tile = |index: usize| {
                let left = inner.left + index as f32 * (tile_width + TILE_GAP);
                rect(left, y + 12.0, left + tile_width, y + 12.0 + TILE_HEIGHT)
            };
            for (index, theme) in Theme::ALL.into_iter().enumerate() {
                renderer.draw_theme_tile(theme, design::preset(theme), theme.colours(), tile(index), panel, regions)?;
            }
            let custom = tile(Theme::ALL.len());
            let bottom = match panel.themes.custom() {
                Some(made) => {
                    let palette = renderer.custom_palette(made);
                    renderer.draw_theme_tile(Theme::Custom, &palette, made.colours, custom, panel, regions)?
                }
                None => renderer.draw_new_theme_tile(custom, panel, regions)?,
            };
            swatches = renderer.draw_theme_colours(panel, inner, bottom + 12.0, regions)?;
            let fraction = |opacity: u8, min: u8| (opacity - min) as f32 / (100 - min) as f32;
            let mut y = bottom + 12.0 + SETTING_ROW;
            let rows = [(false, panel.opacity, "Window opacity", "Text opacity"), (true, panel.locked_opacity, "Locked window", "Locked text")];
            for (locked, opacity, window, text) in rows {
                let (window_fraction, text_fraction) = (fraction(opacity.window, MIN_WINDOW_OPACITY), fraction(opacity.text, MIN_TEXT_OPACITY));
                y = renderer.draw_slider_row(window, &format!("{}%", opacity.window), window_fraction, Slider::WindowOpacity { locked }, inner, y, regions)?;
                y = renderer.draw_slider_row(text, &format!("{}%", opacity.text), text_fraction, Slider::TextOpacity { locked }, inner, y, regions)?;
            }
            Ok(y)
        })?;
        let hint = "Changing a colour saves it as your Custom theme; the presets stay as they are. Locked, clicks pass through the panel, so it can be more see-through then.";
        Ok((self.draw_hint(hint, content, y)?, swatches))
    }

    /// `theme`'s tile: its window, a card with its accent, and a captions pill, with its name under it.
    /// Returns the y below the name.
    fn draw_theme_tile(&mut self, theme: Theme, look: &Palette, colours: Colours, tile: D2D_RECT_F, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        let action = Action::SetTheme(theme);
        let (chosen, hovered) = (panel.themes.chosen() == theme, pointer_on(panel, action).0);
        self.tile_ring(tile, chosen, hovered)?;
        if look.edge.a > 0.0 {
            self.fill_rounded(expand(tile, 0.5), RADIUS_CONTROL + 0.5, look.edge)?;
        }
        self.glass(tile, RADIUS_CONTROL, look.window)?;
        let card = rect(tile.left + 7.0, tile.top + 8.0, tile.right - 7.0, tile.top + 22.0);
        self.glass(card, 4.0, look.card)?;
        self.fill_dot(card.left + 7.0, card.top + 7.0, 3.0, look.accent)?;
        self.fill_rounded(rect(card.left + 14.0, card.top + 6.0, card.right - 8.0, card.top + 8.0), 1.0, look.text_dim)?;
        let bar = rect(tile.left + 12.0, tile.bottom - 21.0, tile.right - 12.0, tile.bottom - 9.0);
        self.scene.borrow_mut().pill(bar, height(&bar) / 2.0, gloss(colours.pill, 100));
        let middle = (bar.top + bar.bottom) / 2.0;
        self.fill_rounded(rect(bar.left + 8.0, middle - 1.0, bar.right - 8.0, middle + 1.0), 1.0, hex(colours.text, 1.0))?;
        self.draw_tile_name(theme, tile, action, chosen, hovered, regions)
    }

    /// The tile that makes the Custom theme from the one shown: a faint outline around a plus.
    /// Returns the y below its name.
    fn draw_new_theme_tile(&mut self, tile: D2D_RECT_F, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        let action = Action::SetTheme(Theme::Custom);
        let (hovered, pressed) = pointer_on(panel, action);
        let palette = self.palette.clone();
        let rim = if hovered { palette.text_dim } else { with_alpha(palette.text_faint, 0.55) };
        let body = if pressed { palette.pressed } else if hovered { gradient(palette.hover, palette.hover) } else { gradient(CLEAR, CLEAR) };
        self.glass(tile, RADIUS_CONTROL, Glass { rim_top: rim, rim_bottom: rim, ..body })?;
        let (x, y) = ((tile.left + tile.right) / 2.0, (tile.top + tile.bottom) / 2.0);
        self.icon(Icon::Plus, rect(x - 9.0, y - 9.0, x + 9.0, y + 9.0), if hovered { palette.text } else { palette.text_dim })?;
        self.draw_tile_name(Theme::Custom, tile, action, false, hovered, regions)
    }

    /// The ring around the chosen tile, or a fainter one under the pointer.
    fn tile_ring(&self, tile: D2D_RECT_F, chosen: bool, hovered: bool) -> Result<()> {
        if chosen {
            self.fill_rounded(expand(tile, 2.5), RADIUS_CONTROL + 2.5, self.palette.accent)?;
        } else if hovered {
            self.fill_rounded(expand(tile, 1.5), RADIUS_CONTROL + 1.5, self.palette.text_faint)?;
        }
        Ok(())
    }

    /// A tile's name under it, and the tile and name as one target. Returns the y below the name.
    fn draw_tile_name(&mut self, theme: Theme, tile: D2D_RECT_F, action: Action, chosen: bool, hovered: bool, regions: &mut Regions) -> Result<f32> {
        let name = self.layout(if chosen { &self.fonts.strong } else { &self.fonts.small }, theme.name(), f32::MAX)?;
        let name_top = tile.bottom + 7.0;
        let colour = if chosen || hovered { self.palette.text } else { self.palette.text_dim };
        self.draw_layout(&name, (tile.left + tile.right - metrics(&name)?.width) / 2.0, name_top, colour)?;
        let bottom = name_top + height_of(&name)?;
        regions.targets.push((rect(tile.left - 3.0, tile.top - 3.0, tile.right + 3.0, bottom), Target::Click(action)));
        Ok(bottom)
    }

    /// A row of the theme's colours, each a swatch with its name that opens a palette. Returns the swatches.
    fn draw_theme_colours(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<[D2D_RECT_F; 4]> {
        const SWATCH: (f32, f32) = (32.0, 24.0);
        self.begin_row(content, y)?;
        let (middle, cell) = (y + SETTING_ROW / 2.0, width(&content) / Paint::ALL.len() as f32);
        let colours = panel.themes.colours();
        let mut swatches = [D2D_RECT_F::default(); 4];
        for (index, paint) in Paint::ALL.into_iter().enumerate() {
            let left = content.left + index as f32 * cell;
            let swatch = rect(left, middle - SWATCH.1 / 2.0, left + SWATCH.0, middle + SWATCH.1 / 2.0);
            self.draw_swatch(paint, paint.colour(colours), swatch, panel, regions)?;
            let name = self.layout(&self.fonts.small, paint.name(), f32::MAX)?;
            self.draw_layout(&name, swatch.right + 8.0, middle - height_of(&name)? / 2.0, self.palette.text_dim)?;
            swatches[index] = swatch;
        }
        Ok(swatches)
    }

    /// A chip showing `value`, which opens `paint`'s palette.
    fn draw_swatch(&mut self, paint: Paint, value: u32, swatch: D2D_RECT_F, panel: &Panel, regions: &mut Regions) -> Result<()> {
        // Rounded less than a switch's track, so it reads as a colour, not a toggle.
        const RADIUS: f32 = 6.0;
        let action = Action::ToggleDropdown(Dropdown::Colour(paint));
        let open = panel.view.open_dropdown == Some(Dropdown::Colour(paint));
        let (hovered, pressed) = pointer_on(panel, action);
        let (palette, radius) = (self.palette.clone(), RADIUS);
        // An outline keeps a colour equal to the card's visible.
        let outline = with_alpha(palette.text_faint, 0.6);
        self.glass(expand(swatch, 1.0), radius + 1.0, Glass { rim_top: outline, rim_bottom: outline, ..gradient(CLEAR, CLEAR) })?;
        self.glass(swatch, radius, if open || pressed { palette.pressed } else { palette.plate })?;
        if hovered && !open {
            self.fill_rounded(swatch, radius, palette.hover)?;
        }
        self.fill_rounded(rect(swatch.left + 3.0, swatch.top + 3.0, swatch.right - 3.0, swatch.bottom - 3.0), radius - 3.0, palette.swatch_rim)?;
        self.fill_rounded(rect(swatch.left + 4.0, swatch.top + 4.0, swatch.right - 4.0, swatch.bottom - 4.0), radius - 4.0, hex(value, 1.0))?;
        regions.targets.push((swatch, Target::Click(action)));
        Ok(())
    }

    /// Captions mode in settings: a preview of its look in the theme's colours first, so changes show
    /// without scrolling; then on or off, rows, fading, and the parts of its look of its own. Returns the
    /// y below, and where the font drop-down is, so its list can be drawn over the rows after it.
    pub(super) fn draw_subtitle_settings(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<(f32, D2D_RECT_F)> {
        let settings = panel.subtitles;
        let look = settings.look(&panel.themes);
        let y = self.draw_heading("Captions mode", content, y)?;
        let y = self.draw_subtitle_preview(&look, settings.opacity, content, y)? + design::GAP;
        let mut font = D2D_RECT_F::default();
        let y = self.draw_card(content, y, |renderer, inner, mut y| {
            y = renderer.draw_toggle_row("Show captions instead of the panel", settings.on, Action::ShowSubtitles(!settings.on), inner, y, panel, regions)?;
            let row_labels: Vec<String> = (1..=MAX_ROWS).map(|rows| rows.to_string()).collect();
            let rows: Vec<_> = row_labels.iter().zip(1..=MAX_ROWS).map(|(l, r)| (l.as_str(), settings.rows == r, Action::SubtitleRows(r))).collect();
            y = renderer.draw_choice_row("Rows", &rows, inner, y, panel, regions)?;
            let fade = settings.auto_hide;
            y = renderer.draw_toggle_row("Fade out after 4 s quiet", fade, Action::SubtitleAutoHide(!fade), inner, y, panel, regions)?;
            let opacity = settings.opacity;
            let value = if opacity == 0 { "Text only".to_owned() } else { format!("{opacity}% solid") };
            y = renderer.draw_slider_row("Pill opacity", &value, opacity as f32 / 100.0, Slider::PillOpacity, inner, y, regions)?;
            renderer.draw_row_label("Font", inner, y)?;
            font = renderer.draw_dropdown_button(&look.font, Action::ToggleDropdown(Dropdown::Font), inner.right, y, panel, regions)?;
            y += SETTING_ROW;
            let sizes = SIZES.map(|(label, size)| (label, look.size == size, Action::SubtitleSize(size)));
            renderer.draw_choice_row("Size", &sizes, inner, y, panel, regions)
        })?;
        let hint = format!(
            "{} or the panel's captions button switches modes. Captions let clicks through: drag the tab beside them to move them, double-click it for the panel.",
            panel.keybinds.shortcut(Command::Subtitles)
        );
        let bottom = self.draw_hint(&hint, content, y)?;
        Ok((bottom, font))
    }

    /// A pill in `look` on a desktop-like tile from `y`, since captions show over the desktop, not over this
    /// window. Returns the y below.
    fn draw_subtitle_preview(&mut self, look: &Look, opacity: u8, content: D2D_RECT_F, y: f32) -> Result<f32> {
        let style = self.style(look, opacity)?;
        let tile = rect(content.left, y, content.right, y + style.pill + 2.0 * design::GAP);
        self.glass(tile, RADIUS_CARD, Glass { top: hex(0x5b6b7d, 1.0), bottom: hex(0x2c3540, 1.0), ..self.palette.card })?;
        let preview = SubtitleLine { text: "This is how captions look.", speaker: Speaker::You };
        self.draw_subtitle(&preview, &style, (content.left + content.right) / 2.0, tile.bottom - design::GAP, width(&content) - 2.0 * design::GAP, 1)?;
        Ok(tile.bottom)
    }

    /// `label`, a slider at `fraction` setting what `slider` sets, and its `value` at the end. Returns the y below.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_slider_row(&mut self, label: &str, value: &str, fraction: f32, slider: Slider, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        const VALUE: f32 = 72.0;
        let middle = y + SETTING_ROW / 2.0;
        self.draw_row_label(label, content, y)?;
        let value = self.layout(&self.fonts.small, value, f32::MAX)?;
        self.draw_layout(&value, content.right - metrics(&value)?.width, middle - height_of(&value)? / 2.0, self.palette.text_dim)?;
        let (left, right) = (content.left + 120.0, content.right - VALUE - KNOB / 2.0);
        let track = rect(left, middle - TRACK / 2.0, right, middle + TRACK / 2.0);
        self.draw_slider(track, fraction, &[(0.0, fraction, self.palette.accent)])?;
        // The clickable band is taller than the track so it is easy to grab.
        regions.targets.push((rect(left, middle - 12.0, right, middle + 12.0), Target::Slider(slider)));
        Ok(y + SETTING_ROW)
    }

    /// The palette by a colour swatch `anchor` (see [`list_top`]), inside `content`: a grid of colours with
    /// the one in use ringed, then a link to Windows' picker for any other.
    pub(super) fn draw_palette(&mut self, paint: Paint, anchor: D2D_RECT_F, content: D2D_RECT_F, panel: &Panel, regions: &mut Regions) -> Result<()> {
        const CELL: f32 = 22.0;
        const SPACE: f32 = 6.0;
        const COLUMNS: usize = 8;
        const INSET: f32 = 12.0;
        let current = paint.colour(panel.themes.colours());
        let offered = match paint {
            Paint::Background => &BACKGROUNDS,
            Paint::Accent => &ACCENTS,
            Paint::Pill | Paint::Text => &CAPTION_COLOURS,
        };
        let more = self.layout(&self.fonts.small, "More colours…", f32::MAX)?;
        let grid = (COLUMNS as f32 * (CELL + SPACE) - SPACE, offered.len().div_ceil(COLUMNS) as f32 * (CELL + SPACE));
        let (wide, tall) = (grid.0 + 2.0 * INSET, 2.0 * INSET + grid.1 + height_of(&more)?);
        let top = list_top(anchor, tall, content);
        // Lined up with the swatch's right edge, moved right only as far as keeps it in the content.
        let left = (anchor.right - wide).max(content.left);
        let list = rect(left, top, left + wide, top + tall);
        self.popover(list, RADIUS_CARD)?;
        // Clicks between the colours keep the palette open and reach nothing under it.
        regions.targets.push((list, Target::Card));
        for (index, &value) in offered.iter().enumerate() {
            let (x, y) = (list.left + INSET + (index % COLUMNS) as f32 * (CELL + SPACE), list.top + INSET + (index / COLUMNS) as f32 * (CELL + SPACE));
            let cell = rect(x, y, x + CELL, y + CELL);
            let action = Action::SetColour(paint, value);
            if value == current || panel.view.hovered == Some(action) {
                let ring = if value == current { self.palette.accent } else { self.palette.text_dim };
                self.fill_rounded(expand(cell, 2.5), CELL / 2.0 + 2.5, ring)?;
            }
            // A faint rim keeps colours close to the list's own visible.
            self.fill_rounded(expand(cell, 1.0), CELL / 2.0 + 1.0, self.palette.swatch_rim)?;
            self.fill_rounded(cell, CELL / 2.0, hex(value, 1.0))?;
            regions.targets.push((cell, Target::Click(action)));
        }
        let (x, y) = (list.left + INSET, list.top + INSET + grid.1);
        let link = rect(x - 6.0, y - 3.0, x + metrics(&more)?.width + 6.0, y + height_of(&more)? + 3.0);
        if panel.view.hovered == Some(Action::MoreColours(paint)) {
            self.fill_rounded(link, height(&link) / 2.0, self.palette.hover)?;
        }
        self.draw_layout(&more, x, y, self.palette.accent)?;
        regions.targets.push((link, Target::Click(Action::MoreColours(paint))));
        Ok(())
    }
}

/// A pill's shading, worked out from its one colour: lighter at the top, a deeper rim, and a highlight
/// that is softer on dark colours.
fn gloss(pill: u32, opacity: u8) -> Gloss {
    let alpha = opacity as f32 / 100.0;
    let base = hex(pill, alpha);
    let light = luminance(base) > 0.45;
    let white = hex(0xffffff, alpha);
    let deepen = |c: f32| (1.1 * c - 0.35).clamp(0.0, 1.0);
    let rim = if light { D2D1_COLOR_F { r: deepen(base.r), g: deepen(base.g), b: deepen(base.b), a: alpha } } else { mix(base, white, 0.2) };
    Gloss { top: mix(base, white, 0.4), bottom: base, rim, highlight: D2D1_COLOR_F { a: alpha * if light { 0.6 } else { 0.16 }, ..white } }
}

fn luminance(colour: D2D1_COLOR_F) -> f32 {
    0.2126 * colour.r + 0.7152 * colour.g + 0.0722 * colour.b
}

fn cover(a: D2D_RECT_F, b: D2D_RECT_F) -> D2D_RECT_F {
    rect(a.left.min(b.left), a.top.min(b.top), a.right.max(b.right), a.bottom.max(b.bottom))
}

fn english_name(names: &IDWriteLocalizedStrings) -> Result<String> {
    let (mut index, mut exists) = (0, BOOL(0));
    unsafe { names.FindLocaleName(&HSTRING::from("en-us"), &mut index, &mut exists)? };
    let index = if exists.as_bool() { index } else { 0 };
    let length = unsafe { names.GetStringLength(index)? } as usize;
    let mut name = vec![0u16; length + 1];
    unsafe { names.GetString(index, &mut name)? };
    Ok(String::from_utf16_lossy(&name[..length]))
}
