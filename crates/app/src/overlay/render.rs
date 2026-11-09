//! Draws the panel into an off-screen bitmap with Direct2D and DirectWrite, on the CPU, and records
//! where its buttons and scroll areas are. Only the part that changed since the last frame is drawn.
//!
//! Sizes are in DIPs (1/96 inch); Direct2D scales them to the monitor's DPI.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::Rc,
};

use windows::{
    core::{Interface, Result, BOOL, HSTRING},
    Win32::{
        Foundation::{HWND, POINT, RECT, SIZE},
        Graphics::{
            Direct2D::{
                Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F},
                D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1RenderTarget, ID2D1SolidColorBrush,
                D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
                D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
            },
            DirectWrite::{
                DWriteCreateFactory, IDWriteFactory, IDWriteInlineObject, IDWriteTextFormat, IDWriteTextLayout, IDWriteTextLayout1,
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_TEXT_ALIGNMENT_CENTER,
                DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE, DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_HIT_TEST_METRICS, DWRITE_LINE_METRICS, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE, DWRITE_TRIMMING,
                DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::{
                CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
                BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
            },
        },
        UI::WindowsAndMessaging::{UpdateLayeredWindowIndirect, ULW_ALPHA, UPDATELAYEREDWINDOWINFO},
    },
};
use windows_numerics::Matrix3x2;

mod design;
mod history;
mod icon;
mod remote;
mod subtitles;
mod words;

use design::*;
pub use icon::{Icon, IconSet};
use history::{BoxRole, BOX_ICON};
use subtitles::AppearanceAnchors;
pub use subtitles::{Speaker, SubtitleLine, SubtitleScene, DRAG_TAB};

use super::{
    finder::{Period, EXAMPLES},
    folds::{auto_fold_after, AUTO_FOLD_MINUTES},
    icons::Icons,
    rows::{Rows, SectionRows},
    scene::{Glass, Scene},
    usage::Usage,
    view::{
        Action, BlockPrompt, CaptionRegion, Corner, Dropdown, Extent, ImportStatus, LineRef, Paint, Rebind, Regions, Screen, Scroll, ScrollArea,
        SetupStep, Slider, Tab, Target, UpdateCheck, View,
    },
};
use kiku_core::{
    captions::{Caption, Captions, Section, YOU, YOU_SECTION},
    catalog::{self, Choice, Model, GPU_PACK},
    controls::{Blocked, Input, ModelState, THRESHOLD_MAX_DB, THRESHOLD_MIN_DB},
    download::{self, Downloads, Progress, State, Target as Download},
    engine::Device,
    hardware::{gb, Gpu, Hardware},
    keybinds::{Command, Key, Keybinds},
    settings::{BlockAnswer, EngineWhere, Opacity, YourLines},
    subtitles::SubtitleSettings,
    theme::{CustomTheme, Theme, Themes},
    update::{self, Release},
    words::{Scope, WordSettings, Words, STRENGTHS},
};
use std::time::Instant;

pub const TITLE_HEIGHT: f32 = 44.0;
/// A see-through strip left of the panel holding the drag handle; the panel's x = 0 starts after it.
pub const HANDLE_STRIP: f32 = 14.0;
const HANDLE_HEIGHT: f32 = 48.0;
const FOOTER_HEIGHT: f32 = 32.0;
const SECTION_HEADER: f32 = 30.0;
/// Between a section's header and its captions.
const CAPTION_GAP: f32 = 4.0;
/// The fold chevron's width at the left of a section header.
const CHEVRON: f32 = 20.0;
/// How far captions sit in from a section header's left, under its app icon.
const CAPTION_INDENT: f32 = 16.0;
/// How far the highlight behind your lines reaches left of their text, its bar at that edge.
const HIGHLIGHT_INSET: f32 = 10.0;
const BUTTON: f32 = 28.0;
/// A button in a floating toolbar.
const TOOL_BUTTON: f32 = 24.0;
/// Between the title bar's groups of buttons, with a hairline in it.
const GROUP_GAP: f32 = 9.0;
const ICON: f32 = 16.0;
/// Kiku's icon before a window's title: the small icon size, so it is drawn from an embedded size.
const MARK: f32 = 16.0;
const RESIZE_GRIP: f32 = 18.0;
/// A model in settings: its name, and its note on a second line.
const MODEL_ROW: f32 = 48.0;
/// A slider's or meter's track, and its knob.
const TRACK: f32 = 4.0;
const KNOB: f32 = 16.0;
/// Room for the waiting message in a section with no captions yet.
const EMPTY_SECTION: f32 = 24.0;
/// Space right of each caption for its toolbar of copy and ✨.
const LINE_BUTTONS: f32 = 56.0;
/// Rows an open drop-down list shows before it scrolls, and their height.
const DROPDOWN_ROWS: usize = 10;
const LIST_ROW: f32 = 30.0;
/// How far scrolled captions fade into the header above them, and settings into the window's edges.
const FADE: f32 = 16.0;
const SETTINGS_FADE: f32 = 24.0;
/// A floating surface's shadow, as (spread, drop, strength) layers: tight and feathered, with no visible steps.
const POPOVER_SHADOW: [(f32, f32, f32); 4] = [(0.0, 1.0, 0.30), (1.0, 1.5, 0.18), (2.0, 2.5, 0.10), (3.5, 3.5, 0.05)];
/// Lighter, for a toolbar floating just above its row.
const TOOLBAR_SHADOW: [(f32, f32, f32); 2] = [(0.0, 1.0, 0.30), (1.0, 2.0, 0.12)];
/// How far hover toolbars end inside the content's right edge: one column for headers and lines, and the
/// lines' clip doesn't cut their shadow.
const TOOLBAR_INSET: f32 = 4.0;
/// Room either side of a segment's label.
const SEGMENT_PADDING: f32 = 10.0;
/// The settings window's tabs.
const NAV_HEIGHT: f32 = 54.0;
/// Setup's bar of Skip, Back and Next at the bottom of the settings window.
const SETUP_FOOTER: f32 = 60.0;
/// Between groups of settings.
const SECTION_GAP: f32 = 20.0;
/// Between a settings heading and the card under it.
const HEADING_GAP: f32 = 8.0;
/// The block prompt's "Don't ask again" row, and the prompt's corners.
const CHECK_ROW: f32 = 20.0;
const RADIUS_PROMPT: f32 = 14.0;
/// How far right of a scroll area its thumb ends, in the window's padding.
const THUMB_OFFSET: f32 = 9.0;

/// Everything one frame depends on.
pub struct Panel<'a> {
    pub captions: &'a Captions,
    pub view: &'a View,
    pub stopped: bool,
    pub dictating: bool,
    pub keybinds: &'a Keybinds,
    /// Keys pressed so far while setting the captions key.
    pub captured: &'a [Key],
    pub model: ModelState,
    pub hardware: &'a Hardware,
    pub downloads: &'a Downloads,
    pub import: &'a ImportStatus,
    /// Auto save mode; the save buttons only show in manual mode.
    pub auto_save: bool,
    pub save_mic_only: bool,
    pub keep_days: u32,
    pub check_updates: bool,
    /// Minutes an app is quiet before it folds by itself; 0 never.
    pub auto_fold: u32,
    /// Apps not captured, which the panel leaves out.
    pub blocked: &'a [Blocked],
    pub block_answer: BlockAnswer,
    pub your_lines: YourLines,
    /// The app's look: the panel, the settings window and captions mode.
    pub themes: Themes,
    /// Kiku only dictates: no app is captioned.
    pub dictation_only: bool,
    /// Where models run, whether this PC shares its own, and the code others use for it once it listens.
    pub engine_where: EngineWhere,
    pub host_on: bool,
    pub hosted_code: Option<String>,
    /// The other computer's code in use, empty when none is saved.
    pub engine_host: String,
    /// The panel's opacity now, and its settings while unlocked and while locked.
    pub shown_opacity: Opacity,
    pub opacity: Opacity,
    pub locked_opacity: Opacity,
    pub subtitles: &'a SubtitleSettings,
    /// The word rules file as last read, and where its rules and transforms apply.
    pub words: &'a Words,
    pub word_settings: WordSettings,
    pub settings_open: bool,
    pub usage: Option<Usage>,
    pub meters: [Meter; 2],
    /// The window's size in DIPs.
    pub width: f32,
    pub height: f32,
    pub dpi: f32,
}

/// One input's sensitivity: its threshold and the loudest level heard lately.
pub struct Meter {
    pub input: Input,
    pub threshold_db: f32,
    pub level_db: f32,
}

/// How far along a slider drawn in `track` a click or drag at `x` is, from 0 to 1.
pub fn fraction_at(track: &D2D_RECT_F, x: f32) -> f32 {
    ((x - track.left) / width(track)).clamp(0.0, 1.0)
}

/// The hotword strength for a click or drag at `x` on a strength slider drawn in `track`, to a tenth.
pub fn strength_at(track: &D2D_RECT_F, x: f32) -> f32 {
    let strength = STRENGTHS.start() + fraction_at(track, x) * (STRENGTHS.end() - STRENGTHS.start());
    (strength * 10.0).round() / 10.0
}

/// Where a hotword strength sits along a strength slider, from 0 to 1.
fn strength_fraction(strength: f32) -> f32 {
    ((strength - STRENGTHS.start()) / (STRENGTHS.end() - STRENGTHS.start())).clamp(0.0, 1.0)
}

/// The threshold for a click or drag at `x` on a sensitivity slider drawn in `track`.
pub fn threshold_at(track: &D2D_RECT_F, x: f32) -> f32 {
    THRESHOLD_MIN_DB + fraction_at(track, x) * (THRESHOLD_MAX_DB - THRESHOLD_MIN_DB)
}

/// A drawn panel: a memory DC holding a premultiplied BGRA bitmap, ready for a layered window.
pub struct Frame {
    pub dc: HDC,
    pub width: i32,
    pub height: i32,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    /// The pixels drawn since the window last showed this frame.
    dirty: Option<RECT>,
    /// The whole window's opacity when it last showed this frame.
    alpha: u8,
}

impl Frame {
    /// Shows the frame on the layered `window` at `position` with the whole window's opacity at `alpha`,
    /// pushing only what was drawn since it last did, or all of it when `alpha` changed.
    pub fn present(&mut self, window: HWND, position: POINT, alpha: u8) -> Result<()> {
        let whole = RECT { left: 0, top: 0, right: self.width, bottom: self.height };
        let Some(dirty) = (if alpha != self.alpha { Some(whole) } else { self.dirty }) else { return Ok(()) };
        let (size, source) = (SIZE { cx: self.width, cy: self.height }, POINT::default());
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: AC_SRC_ALPHA as u8 };
        let update = UPDATELAYEREDWINDOWINFO {
            cbSize: size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            pptDst: &position,
            psize: &size,
            hdcSrc: self.dc,
            pptSrc: &source,
            pblend: &blend,
            dwFlags: ULW_ALPHA,
            prcDirty: &dirty,
            ..Default::default()
        };
        unsafe { UpdateLayeredWindowIndirect(window, &update).ok()? };
        (self.dirty, self.alpha) = (None, alpha);
        Ok(())
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

struct Fonts {
    title: IDWriteTextFormat,
    heading: IDWriteTextFormat,
    label: IDWriteTextFormat,
    body: IDWriteTextFormat,
    button: IDWriteTextFormat,
    caption: IDWriteTextFormat,
    /// Your own captions, when they are shown in italics.
    your_caption: IDWriteTextFormat,
    small: IDWriteTextFormat,
    /// Small and semibold: badges, key caps, a chosen segment.
    strong: IDWriteTextFormat,
    /// Settings headings, in capitals.
    overline: IDWriteTextFormat,
}

/// What a cached layout was made from.
#[derive(Clone, PartialEq, Eq, Hash)]
struct LayoutKey {
    /// The text format's COM pointer; formats live as long as the renderer.
    format: usize,
    width: u32,
    single_line: bool,
    text: String,
}

/// Text layouts used by the last frame and this one, so text that hasn't changed isn't laid out again.
#[derive(Default)]
struct Layouts {
    current: HashMap<LayoutKey, IDWriteTextLayout>,
    previous: HashMap<LayoutKey, IDWriteTextLayout>,
}

impl Layouts {
    /// Starts a frame: layouts the last frame didn't use are let go.
    fn next_frame(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }

    fn get(&mut self, key: &LayoutKey) -> Option<IDWriteTextLayout> {
        if let Some(layout) = self.current.get(key) {
            return Some(layout.clone());
        }
        let layout = self.previous.remove(key)?;
        self.current.insert(key.clone(), layout.clone());
        Some(layout)
    }
}

/// One window's drawing: its frame, the scene last drawn on it, and the text layouts that scene used.
#[derive(Default)]
pub struct Surface {
    /// Reused until the window changes size.
    frame: Option<Frame>,
    drawn: Scene,
    layouts: Layouts,
    /// The themes `drawn` was drawn in.
    themes: Option<Themes>,
    /// The opacities of the background and the text `drawn` was drawn with.
    see_through: (u8, u8),
}

impl Surface {
    pub fn frame(&mut self) -> Option<&mut Frame> {
        self.frame.as_mut()
    }
}

/// Drawing resources shared by every window; each window keeps its own [`Surface`].
pub struct Renderer {
    dwrite: IDWriteFactory,
    /// Draws on the CPU into the part of the frame that changed.
    target: ID2D1DCRenderTarget,
    /// One brush, recolored per draw call.
    brush: ID2D1SolidColorBrush,
    /// Colors the "You:" prefix inside a caption.
    speaker_brush: ID2D1SolidColorBrush,
    fonts: Fonts,
    /// The colours of the frame being drawn, worked out when the themes change.
    palette: Rc<Palette>,
    /// The themes' palette as it is, before the panel's opacities change it.
    themed: Rc<Palette>,
    /// The window's background is see-through, so scrolled text is cut at the edges instead of faded.
    see_through: bool,
    themes: Themes,
    /// The Custom theme's palette, kept for its tile while a preset shows.
    custom_palette: Option<(CustomTheme, Rc<Palette>)>,
    /// The layouts of the surface being drawn.
    layouts: RefCell<Layouts>,
    /// Captions mode's text format, for its font and size.
    subtitle_format: RefCell<Option<(String, u16, IDWriteTextFormat)>>,
    /// The "…" that ends cut text, made once per text format.
    ellipses: RefCell<HashMap<usize, IDWriteInlineObject>>,
    icons: Icons,
    icon_set: IconSet,
    /// The frame being recorded.
    scene: RefCell<Scene>,
    /// Each section's caption heights, by section index.
    section_rows: Vec<SectionRows>,
    /// The open history session's row heights, by session and line count.
    saved_rows: Option<((i64, usize), Rows)>,
    /// The settings window's body height in the last frame, for scrolling it.
    settings_height: f32,
    /// Where the first row of the card being drawn starts; rows after it get a hairline above.
    card_first_row: Cell<f32>,
    /// How your lines in an app's section are marked; their heights are measured again when it changes.
    your_lines: YourLines,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        let d2d: ID2D1Factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let target = unsafe { d2d.CreateDCRenderTarget(&render_target_properties())? };
        let themes = Themes::default();
        let palette = Rc::new(design::preset(themes.chosen()).clone());
        let brush = unsafe { target.CreateSolidColorBrush(&palette.text, None)? };
        let speaker_brush = unsafe { target.CreateSolidColorBrush(&palette.accent, None)? };
        let format = |family, size, weight| text_format(&dwrite, family, size, weight, DWRITE_FONT_STYLE_NORMAL);
        let (ui, display) = ui_families(&dwrite)?;
        let fonts = Fonts {
            title: format(display, TITLE_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            heading: format(display, HEADING_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            label: format(ui, LABEL_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            body: format(ui, BODY_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            button: format(ui, BUTTON_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            caption: format(ui, CAPTION_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            your_caption: text_format(&dwrite, ui, CAPTION_SIZE, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_ITALIC)?,
            small: format(ui, SMALL_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            strong: format(ui, SMALL_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            overline: format(ui, OVERLINE_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
        };
        Ok(Self {
            dwrite,
            target,
            brush,
            speaker_brush,
            fonts,
            themed: palette.clone(),
            see_through: false,
            palette,
            themes,
            custom_palette: None,
            layouts: RefCell::default(),
            subtitle_format: RefCell::default(),
            ellipses: RefCell::default(),
            icons: Icons::new()?,
            icon_set: IconSet::new(&d2d)?,
            scene: RefCell::default(),
            section_rows: Vec::new(),
            saved_rows: None,
            settings_height: 0.0,
            card_first_row: Cell::new(0.0),
            your_lines: YourLines::default(),
        })
    }

    /// Draws in `themes` from now on, working their palette out only when they changed.
    fn use_themes(&mut self, themes: &Themes) {
        if self.themes == *themes {
            return;
        }
        self.themes = *themes;
        self.themed = match themes.custom().filter(|_| themes.chosen() == Theme::Custom) {
            Some(custom) => self.custom_palette(custom),
            None => Rc::new(design::preset(themes.chosen()).clone()),
        };
        self.see_through(100, 100);
    }

    /// Draws with the window's background at `window` percent opacity and the text at `text`.
    fn see_through(&mut self, window: u8, text: u8) {
        self.see_through = window < 100;
        self.palette = match (window, text) {
            (100, 100) => self.themed.clone(),
            _ => Rc::new(self.themed.see_through(window as f32 / 100.0, text as f32 / 100.0)),
        };
        unsafe { self.speaker_brush.SetColor(&self.palette.accent) };
    }

    /// The Custom theme's palette, worked out again only when its colours change.
    fn custom_palette(&mut self, custom: &CustomTheme) -> Rc<Palette> {
        match &self.custom_palette {
            Some((made_from, palette)) if made_from == custom => palette.clone(),
            _ => {
                let palette = Rc::new(design::custom(custom));
                self.custom_palette = Some((*custom, palette.clone()));
                palette
            }
        }
    }

    /// Lays out the whole panel and draws the part that changed since the last frame. Returns the
    /// frame with its clickable and scrollable regions.
    pub fn render<'s>(&mut self, surface: &'s mut Surface, panel: &Panel) -> Result<(&'s mut Frame, Regions)> {
        let mut regions = Regions::default();
        let size = (panel.width + HANDLE_STRIP, panel.height);
        let see_through = (panel.shown_opacity.window, panel.shown_opacity.text);
        if surface.see_through != see_through {
            // Cached captions keep the "You:" brush's colour, so all is drawn again.
            (surface.frame, surface.see_through) = (None, see_through);
        }
        if self.your_lines != panel.your_lines {
            // Italics wrap differently.
            (self.your_lines, self.section_rows) = (panel.your_lines, Vec::new());
        }
        let frame = self.render_on(surface, size, HANDLE_STRIP, panel.dpi, &panel.themes, |renderer| {
            renderer.see_through(see_through.0, see_through.1);
            let drawn = renderer.draw_panel(panel, &mut regions);
            renderer.see_through(100, 100);
            drawn
        })?;
        Ok((frame, regions))
    }

    /// Lays out the settings window and draws the part that changed since the last frame. Returns the
    /// frame with its clickable and scrollable regions.
    pub fn render_settings<'s>(&mut self, surface: &'s mut Surface, panel: &Panel) -> Result<(&'s mut Frame, Regions)> {
        let mut regions = Regions::default();
        let size = (panel.width, panel.height);
        let frame = self.render_on(surface, size, 0.0, panel.dpi, &panel.themes, |renderer| renderer.draw_settings_window(panel, &mut regions))?;
        Ok((frame, regions))
    }

    /// Records a frame of `size` DIPs in `themes` with `record`, whose x = 0 is `origin` DIPs from the frame's
    /// left edge, and draws the part that differs from what `surface` last showed.
    pub(super) fn render_on<'s>(
        &mut self,
        surface: &'s mut Surface,
        size: (f32, f32),
        origin: f32,
        dpi: f32,
        themes: &Themes,
        record: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<&'s mut Frame> {
        self.use_themes(themes);
        if surface.themes != Some(*themes) {
            // Cached captions keep the recoloured "You:" brush, which comparing scenes can't see, so all is drawn again.
            (surface.frame, surface.themes) = (None, Some(*themes));
        }
        let scale = dpi / 96.0;
        let (width, height) = ((size.0 * scale).round() as i32, (size.1 * scale).round() as i32);
        std::mem::swap(&mut *self.layouts.borrow_mut(), &mut surface.layouts);
        self.layouts.borrow_mut().next_frame();
        let recorded = record(self);
        std::mem::swap(&mut *self.layouts.borrow_mut(), &mut surface.layouts);
        let scene = self.scene.take();
        recorded?;

        let whole = RECT { left: 0, top: 0, right: width, bottom: height };
        let changed: Vec<RECT> = match &mut surface.frame {
            Some(frame) if (frame.width, frame.height) == (width, height) => {
                scene.changes_since(&surface.drawn).into_iter().filter_map(|area| pixels_of(area, dpi, origin, whole)).collect()
            }
            frame => {
                *frame = Some(blank_frame(width, height)?);
                vec![whole]
            }
        };
        let dc = surface.frame.as_ref().map(|f| f.dc).unwrap_or_default();
        for &area in &changed {
            if let Err(error) = self.draw_scene(dc, &scene, area, origin, dpi) {
                // What the bitmap holds is unknown now, so the next frame starts afresh.
                surface.frame = None;
                return Err(error);
            }
        }
        surface.drawn = scene;
        let frame = surface.frame.as_mut().unwrap();
        frame.dirty = changed.into_iter().chain(frame.dirty).reduce(union_of);
        Ok(frame)
    }

    /// Draws `scene` into the `area` of the frame in `dc`, in pixels, leaving the rest of it as it was.
    fn draw_scene(&self, dc: HDC, scene: &Scene, area: RECT, origin: f32, dpi: f32) -> Result<()> {
        let scale = dpi / 96.0;
        unsafe {
            self.target.BindDC(dc, &area)?;
            self.target.SetDpi(dpi, dpi);
            self.target.BeginDraw();
            self.target.SetTransform(&Matrix3x2::translation(origin - area.left as f32 / scale, -area.top as f32 / scale));
            self.target.Clear(Some(&CLEAR));
            // The area in DIPs, a DIP wider all round so rounding to pixels never leaves an edge out.
            let dips = |pixel: i32| pixel as f32 / scale;
            let band = D2D_RECT_F { left: dips(area.left) - origin - 1.0, top: dips(area.top) - 1.0, right: dips(area.right) - origin + 1.0, bottom: dips(area.bottom) + 1.0 };
            let drawn = scene.draw(&self.target, &self.brush, &self.icon_set, band);
            self.target.EndDraw(None, None).and(drawn)
        }
    }

    fn draw_panel(&mut self, panel: &Panel, regions: &mut Regions) -> Result<()> {
        let collapsed = panel.view.collapsed;
        self.draw_window(panel.width, panel.height)?;
        // Collapsed, the whole bar drags, so the tab would only stick out.
        if !collapsed {
            self.draw_drag_handle(panel, regions)?;
        }
        self.draw_title_bar(panel, regions)?;
        if !collapsed {
            let content = rect(PADDING, TITLE_HEIGHT, panel.width - PADDING, panel.height - FOOTER_HEIGHT);
            match &panel.view.screen {
                Screen::Live => {
                    self.draw_live(panel, content, regions)?;
                    self.draw_selection_share(panel, content, regions)?;
                }
                Screen::SavedList => self.draw_history(panel, content, regions)?,
                Screen::SavedFile { session, title, lines, focus } => self.draw_saved_file(panel, *session, title, lines, *focus, content, regions)?,
            }
            self.draw_footer(panel, regions)?;
            self.draw_resize_grips(panel, regions)?;
            if let (Screen::Live, Some(prompt)) = (&panel.view.screen, panel.view.blocking) {
                self.draw_block_prompt(prompt, panel, regions)?;
            }
            if panel.view.quit_prompt {
                self.draw_quit_prompt(panel, regions)?;
            } else if let Some(release) = &panel.view.update {
                self.draw_update_prompt(release, panel, regions)?;
            }
        }
        self.draw_tooltip(panel, regions)
    }

    /// A window's glass, brightest behind its title bar, inside its edge if the theme has one.
    fn draw_window(&self, width: f32, height: f32) -> Result<()> {
        let palette = self.palette.clone();
        let mut window = rect(0.0, 0.0, width, height);
        if palette.edge.a > 0.0 {
            self.fill_rounded(window, RADIUS_WINDOW, palette.edge)?;
            window = inset(window, 1.0);
        }
        self.glass(window, RADIUS_WINDOW, palette.window)?;
        self.glass(rect(window.left + 1.0, window.top + 1.0, window.right - 1.0, TITLE_HEIGHT + 8.0), RADIUS_WINDOW - 1.0, palette.title_band)
    }

    /// A tab of window glass on the panel's left edge, lit while the pointer is anywhere it can drag from.
    /// Drawn after the window: the tab goes beneath it, so only its outer corners show rounded, and over
    /// the joint the window's own glass covers its rim, so tab and window read as one piece.
    fn draw_drag_handle(&mut self, panel: &Panel, regions: &mut Regions) -> Result<()> {
        let (palette, hovered) = (self.palette.clone(), panel.view.drag_hover);
        let middle = panel.height / 2.0;
        let half = (HANDLE_HEIGHT / 2.0).min(middle - RADIUS_WINDOW);
        let strip = rect(-HANDLE_STRIP, middle - half, 0.0, middle + half);
        let tab = rect(strip.left, strip.top, RADIUS_WINDOW, strip.bottom);
        let radius = HANDLE_STRIP / 2.0;
        let (top, bottom) = (background_at(&palette, tab.top, panel.height), background_at(&palette, tab.bottom, panel.height));
        self.scene.borrow_mut().under(0, |scene| {
            if palette.edge.a > 0.0 {
                scene.fill_rounded(expand(tab, 1.0), radius + 1.0, palette.edge);
            }
            scene.glass(tab, radius, Glass { top, bottom, ..palette.window });
            if hovered {
                scene.fill_rounded(tab, radius, palette.hover);
            }
        });
        self.glass(rect(0.0, tab.top, 2.0, tab.bottom), 0.0, gradient(top, bottom))?;
        self.grip(strip, if hovered { palette.text } else { palette.text_dim })?;
        regions.targets.push((strip, Target::Drag));
        Ok(())
    }

    /// A drag tab's dots, in the middle of `area`.
    fn grip(&self, area: D2D_RECT_F, colour: D2D1_COLOR_F) -> Result<()> {
        let (x, y) = centre(area);
        self.icon(Icon::Grip, rect(x - ICON / 2.0, y - ICON / 2.0, x + ICON / 2.0, y + ICON / 2.0), colour)
    }

    /// The mark and title, what Kiku is doing (or the "N hidden" chip and a notice), and two groups of
    /// buttons: the app's, then the window's. The whole bar drags the window too.
    fn draw_title_bar(&mut self, panel: &Panel, regions: &mut Regions) -> Result<()> {
        regions.targets.push((rect(0.0, 0.0, panel.width, TITLE_HEIGHT), Target::Drag));
        let middle = TITLE_HEIGHT / 2.0;
        let groups = title_buttons(panel);
        let count: usize = groups.iter().map(Vec::len).sum();
        let buttons_left = panel.width - PADDING + 6.0 - BUTTON * count as f32 - GROUP_GAP * (groups.len() - 1) as f32;
        let title_right = self.draw_brand("Kiku", panel.dpi, middle, buttons_left - 8.0)?;

        let mut status_right = buttons_left - 8.0;
        match panel.view.folds.hidden_count() {
            hidden if hidden > 0 && !panel.view.collapsed => status_right = self.draw_hidden_chip(hidden, status_right, middle, panel, regions)? - 8.0,
            _ => {}
        }
        self.draw_status(panel, title_right + 14.0, status_right, middle, regions)?;

        let mut x = buttons_left;
        for (index, group) in groups.into_iter().enumerate() {
            if index > 0 {
                let line = x + GROUP_GAP / 2.0;
                self.fill_rounded(rect(line - 0.5, middle - 8.0, line + 0.5, middle + 8.0), 0.0, self.palette.divider)?;
                x += GROUP_GAP;
            }
            for (icon, action, tone) in group {
                self.icon_button(rect(x, middle - BUTTON / 2.0, x + BUTTON, middle + BUTTON / 2.0), icon, action, tone, panel, regions)?;
                x += BUTTON;
            }
        }
        Ok(())
    }

    /// Kiku's icon, then a window's `title` if it fits before `room`. Returns the right edge of what was drawn.
    fn draw_brand(&mut self, title: &str, dpi: f32, middle: f32, room: f32) -> Result<f32> {
        let mark = rect(PADDING, middle - MARK / 2.0, PADDING + MARK, middle + MARK / 2.0);
        let target: ID2D1RenderTarget = self.target.cast()?;
        if let Some(icon) = self.icons.kiku(&target, (MARK * dpi / 96.0).round() as i32) {
            self.scene.borrow_mut().bitmap(&icon, mark);
        }
        let title = self.layout(&self.fonts.title, title, f32::MAX)?;
        let (x, title_width) = (mark.right + 8.0, metrics(&title)?.width);
        if x + title_width > room {
            return Ok(mark.right);
        }
        self.draw_layout(&title, x, middle - height_of(&title)? / 2.0, self.palette.text)?;
        Ok(x + title_width)
    }

    /// What Kiku is doing, after a dot of its colour, or a notice, between `left` and `right`. Clicking
    /// the state stops or starts captioning.
    fn draw_status(&mut self, panel: &Panel, left: f32, right: f32, middle: f32, regions: &mut Regions) -> Result<()> {
        // A narrow panel has no room for it.
        if right - left < 40.0 {
            return Ok(());
        }
        let status = status_text(panel, &self.palette);
        let mut x = left;
        if let Some(dot) = status.dot {
            self.fill_dot(x + 3.0, middle, 3.0, dot)?;
            x += 12.0;
        }
        let text = self.single_line(&self.fonts.small, &status.text, right - x)?;
        let size = metrics(&text)?;
        if status.clickable {
            let area = rect(left - 8.0, middle - 12.0, x + size.width + 8.0, middle + 12.0);
            if panel.view.hovered == Some(Action::ToggleStop) && contains(&area, panel.view.pointer) {
                self.fill_rounded(area, 12.0, self.palette.hover)?;
            }
            regions.targets.push((area, Target::Click(Action::ToggleStop)));
        }
        self.draw_layout(&text, x, middle - size.height / 2.0, status.color)
    }

    /// "N hidden  Show" ending at `right`, which brings the hidden apps back. Returns its left edge.
    fn draw_hidden_chip(&mut self, hidden: usize, right: f32, middle: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        const CHIP_ICON: f32 = 12.0;
        let action = Action::ShowHidden;
        let count = self.layout(&self.fonts.small, &format!("{hidden} hidden"), f32::MAX)?;
        let show = self.layout(&self.fonts.strong, "Show", f32::MAX)?;
        let (count_width, show_width) = (metrics(&count)?.width, metrics(&show)?.width);
        let chip = rect(right - (CHIP_ICON + count_width + show_width + 32.0).ceil(), middle - 11.0, right, middle + 11.0);
        let (hovered, pressed) = pointer_on(panel, action);
        let palette = self.palette.clone();
        self.glass(chip, 11.0, if pressed { palette.pressed } else if hovered { palette.plate } else { palette.card })?;
        let x = chip.left + 10.0;
        self.icon(Icon::EyeOff, rect(x, middle - CHIP_ICON / 2.0, x + CHIP_ICON, middle + CHIP_ICON / 2.0), palette.text_dim)?;
        self.draw_layout(&count, x + CHIP_ICON + 6.0, middle - height_of(&count)? / 2.0, palette.text_dim)?;
        self.draw_layout(&show, chip.right - 10.0 - show_width, middle - height_of(&show)? / 2.0, palette.accent)?;
        regions.targets.push((chip, Target::Click(action)));
        Ok(chip.left)
    }

    /// Every app that has spoken and the You section, minus hidden and blocked apps, in order. A folded section is
    /// its header alone; the open ones share the rest fairly, each in its own scrollable area. An empty
    /// section says what the app is waiting for.
    fn draw_live(&mut self, panel: &Panel, content: D2D_RECT_F, regions: &mut Regions) -> Result<()> {
        let (now, folds) = (Instant::now(), &panel.view.folds);
        let auto_fold = auto_fold_after(panel.auto_fold);
        let sections: Vec<(usize, &Section, bool)> = panel
            .captions
            .visible()
            .filter(|(index, section)| !folds.is_hidden(*index) && (*index == YOU_SECTION || !panel.blocked.iter().any(|app| app.name == section.label)))
            .map(|(index, section)| (index, section, folds.folded(index, section, auto_fold, now)))
            .collect();
        let open: Vec<(usize, &Section)> = sections.iter().filter(|(.., folded)| !folded).map(|&(index, section, _)| (index, section)).collect();
        regions.sections = open.iter().map(|(index, _)| *index).collect();
        let text_width = width(&content) - CAPTION_INDENT - LINE_BUTTONS;
        let selected: HashMap<LineRef, (u32, u32)> = panel
            .view
            .selection
            .map(|s| s.spans(panel.captions, &regions.sections).into_iter().map(|(line, start, end)| (line, (start, end))).collect())
            .unwrap_or_default();
        let mut y = content.top;
        let available = content.bottom - y - (SECTION_HEADER + GAP) * sections.len() as f32 - CAPTION_GAP * open.len() as f32;
        let mut extents = self.measure_sections(panel, &open, text_width, available.max(0.0))?.into_iter();

        for (position, &(index, section, folded)) in sections.iter().enumerate() {
            if position > 0 {
                self.hairline(content.left + CAPTION_INDENT, content.right, y - GAP / 2.0)?;
            }
            let header = rect(content.left, y, content.right, y + SECTION_HEADER);
            self.draw_section_header(section, index, folded, header, panel, regions)?;
            y += SECTION_HEADER;
            let Some(extent) = (!folded).then(|| extents.next()).flatten() else {
                y += GAP;
                continue;
            };
            y += CAPTION_GAP;
            let area = rect(content.left + CAPTION_INDENT, y, content.right, y + extent.viewport);
            regions.scroll_areas.push((area, ScrollArea::Section(index), extent));
            let top = panel.view.scroll(ScrollArea::Section(index)).top(extent);
            if section.captions.is_empty() {
                self.draw_waiting(index, panel, area)?;
            }
            self.draw_captions(index, &section.captions, area, text_width, top, &selected, panel, regions)?;
            if top > 0.0 {
                self.fade(rect(content.left, area.top, content.right, area.top + FADE), Edge::Top, panel.height)?;
            }
            self.draw_scroll_thumb(area.right + THUMB_OFFSET, area, extent, top)?;
            if top < extent.max_top() - 1.0 {
                self.draw_latest_button(index, area, text_width, panel, regions)?;
            }
            y += extent.viewport + GAP;
        }
        Ok(())
    }

    /// What an empty section is waiting for, after an icon saying where its sound comes from.
    fn draw_waiting(&mut self, index: usize, panel: &Panel, area: D2D_RECT_F) -> Result<()> {
        let (icon, text) = if panel.stopped {
            (Icon::Power, format!("Stopped. Press the power button above or {} to start again.", panel.keybinds.shortcut(Command::Stop)))
        } else {
            (if index == YOU_SECTION { Icon::Mic } else { Icon::Speaker }, "Listening to your mic and any app that plays audio…".to_owned())
        };
        let text = self.single_line(&self.fonts.body, &text, width(&area) - 22.0)?;
        let middle = area.top + height_of(&text)? / 2.0;
        self.icon(icon, rect(area.left, middle - 7.0, area.left + 14.0, middle + 7.0), self.palette.text_faint)?;
        self.draw_layout(&text, area.left + 22.0, area.top, self.palette.text_dim)
    }

    /// "Block <app>?" in a card over the panel, which it shades and keeps from being clicked: until Kiku
    /// quits, always, or not, and "Don't ask again", which keeps the answer for every app.
    fn draw_block_prompt(&mut self, prompt: BlockPrompt, panel: &Panel, regions: &mut Regions) -> Result<()> {
        let question = format!("Block {}?", panel.captions.sections[prompt.section].label);
        let detail = "Kiku stops capturing and captioning it. What it said before stays in History.";
        let answers = [
            ("Just this session", Action::BlockFor(false), Emphasis::Primary),
            ("Always", Action::BlockFor(true), Emphasis::Secondary),
            ("Cancel", Action::CancelBlock, Emphasis::Ghost),
        ];
        let checkbox = |renderer: &mut Self, x: f32, y: f32, regions: &mut Regions| {
            renderer.draw_checkbox("Don't ask again", prompt.dont_ask, Action::DontAskToBlock, x, y, panel, regions)
        };
        self.draw_prompt(&question, detail, Some((CHECK_ROW, &checkbox as &PromptExtra<'_>)), &answers, panel, regions)
    }

    /// Asked on quitting while a download runs: pause it until next time, or keep Kiku open.
    fn draw_quit_prompt(&mut self, panel: &Panel, regions: &mut Regions) -> Result<()> {
        let progress = panel.downloads.active().map(|p| format!(" ({})", progress_text(&p))).unwrap_or_default();
        let detail = format!("A download is running{progress}. Quitting pauses it; it resumes where it stopped next time Kiku starts.");
        let answers = [("Keep downloading", Action::KeepDownloading, Emphasis::Primary), ("Quit, resume next time", Action::QuitAndPause, Emphasis::Secondary)];
        self.draw_prompt("Quit while downloading?", &detail, None, &answers, panel, regions)
    }

    /// A newer Kiku is out: install it now, or be asked again at the next check.
    fn draw_update_prompt(&mut self, release: &Release, panel: &Panel, regions: &mut Regions) -> Result<()> {
        let notes = release.notes.trim();
        let gap = if notes.is_empty() { "" } else { "\n\n" };
        let detail = format!("{notes}{gap}Updating closes Kiku for a moment and opens it again.");
        let answers = [("Update", Action::InstallUpdate, Emphasis::Primary), ("Later", Action::UpdateLater, Emphasis::Ghost)];
        self.draw_prompt(&format!("Kiku {} is out", release.version), &detail, None, &answers, panel, regions)
    }

    /// A question in a card over the panel, which it shades and keeps from being clicked: a line of detail,
    /// anything `extra` draws in a row of its height, and the answers as buttons, the first rightmost.
    fn draw_prompt(&mut self, question: &str, detail: &str, extra: Option<(f32, &PromptExtra<'_>)>, answers: &[(&str, Action, Emphasis)], panel: &Panel, regions: &mut Regions) -> Result<()> {
        const PAD: f32 = 20.0;
        let shaded = rect(0.0, TITLE_HEIGHT, panel.width, panel.height);
        // Rounded like the window's bottom corners; its top runs under the title bar's edge.
        self.push_clip(shaded);
        self.fill_rounded(rect(0.0, TITLE_HEIGHT - RADIUS_WINDOW, panel.width, panel.height), RADIUS_WINDOW, self.palette.shade)?;
        self.pop_clip();
        regions.targets.push((shaded, Target::Card));

        let card_width = (panel.width - 2.0 * PADDING).min(400.0);
        let inner = card_width - 2.0 * PAD;
        let question = self.single_line(&self.fonts.heading, question, inner)?;
        let detail = self.layout(&self.fonts.body, detail, inner)?;
        let (question_height, detail_height) = (height_of(&question)?, height_of(&detail)?);
        let extra_height = extra.map_or(0.0, |(height, _)| height + 14.0);
        let card_height = 2.0 * PAD + question_height + 6.0 + detail_height + 20.0 + extra_height + CONTROL;
        let (left, top) = ((panel.width - card_width) / 2.0, (TITLE_HEIGHT + panel.height - card_height) / 2.0);
        let card = rect(left, top, left + card_width, top + card_height);
        self.popover(card, RADIUS_PROMPT)?;
        regions.targets.push((card, Target::Card));

        let (x, mut y) = (card.left + PAD, card.top + PAD);
        self.draw_layout(&question, x, y, self.palette.text)?;
        y += question_height + 6.0;
        self.draw_layout(&detail, x, y, self.palette.text_dim)?;
        y += detail_height + 14.0;
        if let Some((height, draw)) = extra {
            draw(self, x, y, regions)?;
            y += height + 14.0;
        }
        y += 6.0;
        let mut right = card.right - PAD;
        for &(label, action, emphasis) in answers {
            right = self.draw_text_button(label, None, action, emphasis, right, y + CONTROL / 2.0, panel, regions)? - 8.0;
        }
        Ok(())
    }

    /// A tick box and its label in a row of [`CHECK_ROW`] from `y`; the label is clickable too.
    #[allow(clippy::too_many_arguments)]
    fn draw_checkbox(&mut self, label: &str, checked: bool, action: Action, x: f32, y: f32, panel: &Panel, regions: &mut Regions) -> Result<()> {
        let middle = y + CHECK_ROW / 2.0;
        let tick = rect(x, middle - 8.0, x + 16.0, middle + 8.0);
        let (hovered, pressed) = pointer_on(panel, action);
        let palette = self.palette.clone();
        if checked {
            self.glass(tick, 4.0, palette.primary)?;
            self.icon(Icon::Check, inset(tick, 2.0), palette.on_accent)?;
        } else {
            self.glass(tick, 4.0, if pressed { palette.pressed } else { palette.plate })?;
            if hovered {
                self.fill_rounded(tick, 4.0, palette.hover)?;
            }
        }
        let label = self.layout(&self.fonts.body, label, f32::MAX)?;
        self.draw_layout(&label, tick.right + 8.0, middle - height_of(&label)? / 2.0, if hovered { palette.text } else { palette.text_dim })?;
        regions.targets.push((rect(x - 4.0, y, tick.right + 12.0 + metrics(&label)?.width, y + CHECK_ROW), Target::Click(action)));
        Ok(())
    }

    /// A button reading `label`, after `icon` if any, whose right edge is at `right`, centred on `middle`.
    /// Returns its left edge.
    #[allow(clippy::too_many_arguments)]
    fn draw_text_button(
        &mut self,
        label: &str,
        icon: Option<Icon>,
        action: Action,
        emphasis: Emphasis,
        right: f32,
        middle: f32,
        panel: &Panel,
        regions: &mut Regions,
    ) -> Result<f32> {
        const BUTTON_ICON: f32 = 14.0;
        let label = self.layout(&self.fonts.button, label, f32::MAX)?;
        let icon_room = if icon.is_some() { BUTTON_ICON + 6.0 } else { 0.0 };
        let button = rect(right - (metrics(&label)?.width + icon_room + 28.0).ceil(), middle - CONTROL / 2.0, right, middle + CONTROL / 2.0);
        let (hovered, pressed) = pointer_on(panel, action);
        let (palette, radius) = (self.palette.clone(), CONTROL / 2.0);
        let colour = match emphasis {
            Emphasis::Primary => {
                self.glass(button, radius, palette.primary)?;
                // A band of light across the top half, like the glass it is.
                let band = rect(button.left + 1.0, button.top + 1.0, button.right - 1.0, button.top + height(&button) * 0.45);
                self.glass(band, radius - 1.0, gradient(white(if hovered { 0.45 } else { 0.32 }), white(0.0)))?;
                if pressed {
                    self.fill_rounded(button, radius, black(0.14))?;
                }
                palette.on_accent
            }
            Emphasis::Secondary => {
                self.glass(button, radius, if pressed { palette.pressed } else { palette.plate })?;
                if hovered && !pressed {
                    self.fill_rounded(button, radius, palette.hover)?;
                }
                palette.text
            }
            Emphasis::Ghost => {
                if hovered {
                    self.glass(button, radius, if pressed { palette.pressed } else { palette.plate })?;
                }
                if hovered { palette.text } else { palette.text_dim }
            }
            Emphasis::Danger => {
                self.glass(button, radius, palette.danger)?;
                if pressed {
                    self.fill_rounded(button, radius, black(0.14))?;
                }
                palette.on_accent
            }
        };
        let nudge = if pressed { 0.5 } else { 0.0 };
        let mut x = button.left + 14.0;
        if let Some(icon) = icon {
            self.icon(icon, rect(x, middle - BUTTON_ICON / 2.0 + nudge, x + BUTTON_ICON, middle + BUTTON_ICON / 2.0 + nudge), colour)?;
            x += icon_room;
        }
        self.draw_layout(&label, x, middle - height_of(&label)? / 2.0 + nudge, colour)?;
        regions.targets.push((button, Target::Click(action)));
        Ok(button.left)
    }

    /// Brings each section's caption heights in step with its lines, then measures the lines each
    /// section shows until none on screen is an estimate. Returns how much each section holds and shows.
    fn measure_sections(&mut self, panel: &Panel, sections: &[(usize, &Section)], text_width: f32, available: f32) -> Result<Vec<Extent>> {
        let font_size = unsafe { self.fonts.caption.GetFontSize() };
        let shown_len = |caption: &Caption| caption.text.len() + caption.speaker.map_or(0, |speaker| speaker.len() + 2);
        let estimate = |caption: &Caption| estimated_height(shown_len(caption), font_size, text_width) + ROW_GAP;
        for &(index, section) in sections {
            if self.section_rows.len() <= index {
                self.section_rows.resize_with(index + 1, SectionRows::default);
            }
            self.section_rows[index].sync(section, estimate);
            self.section_rows[index].rows.set_width(text_width);
        }
        loop {
            let contents: Vec<f32> = sections
                .iter()
                .map(|&(index, section)| if section.captions.is_empty() { EMPTY_SECTION } else { self.section_rows[index].rows.total() })
                .collect();
            let extents: Vec<Extent> =
                contents.iter().zip(share_space(&contents, available)).map(|(&content, viewport)| Extent { content, viewport }).collect();
            let mut measured = false;
            for (&(index, section), extent) in sections.iter().zip(&extents) {
                let top = panel.view.scroll(ScrollArea::Section(index)).top(*extent);
                for row in self.section_rows[index].rows.estimated_between(top, top + extent.viewport) {
                    let height = self.caption_height(&section.captions[row], text_width)?;
                    self.section_rows[index].rows.set_measured(row, height + ROW_GAP);
                    measured = true;
                }
            }
            if !measured {
                return Ok(extents);
            }
        }
    }

    /// Over a section scrolled up from its newest lines, a button back down to them, at the bottom right of
    /// its text so it leaves the lines' toolbars free.
    fn draw_latest_button(&mut self, index: usize, area: D2D_RECT_F, text_width: f32, panel: &Panel, regions: &mut Regions) -> Result<()> {
        if height(&area) < CONTROL + 12.0 {
            return Ok(());
        }
        let middle = area.bottom - 6.0 - CONTROL / 2.0;
        let action = Action::ScrollToLatest(index);
        self.draw_text_button("Latest", Some(Icon::ChevronDown), action, Emphasis::Primary, area.left + text_width, middle, panel, regions)?;
        Ok(())
    }

    /// After text is selected, a button to send it to Claude, in the bottom-right corner of the captions.
    fn draw_selection_share(&mut self, panel: &Panel, content: D2D_RECT_F, regions: &mut Regions) -> Result<()> {
        if !panel.view.selection.is_some_and(|s| !s.is_empty()) {
            return Ok(());
        }
        let middle = content.bottom - 8.0 - CONTROL / 2.0;
        let label = "Ask Claude about the selection";
        let left = self.draw_text_button(label, Some(Icon::Sparkles), Action::ShareSelection, Emphasis::Primary, content.right, middle, panel, regions)?;
        self.draw_text_button("Correct", Some(Icon::Pencil), Action::CorrectSelection, Emphasis::Secondary, left - 8.0, middle, panel, regions)?;
        Ok(())
    }

    /// The fold chevron, app icon and name, a "Live" badge while it is speaking, and when folded its latest
    /// line, dimmed and cut to fit. While the header is hovered, a toolbar shows on the right: copy, ✨,
    /// and in manual save mode save; then hide and block (not for you).
    fn draw_section_header(&mut self, section: &Section, index: usize, folded: bool, header: D2D_RECT_F, panel: &Panel, regions: &mut Regions) -> Result<()> {
        regions.targets.push((header, Target::Header(index)));
        let middle = (header.top + header.bottom) / 2.0;
        let chevron = rect(header.left - 6.0, middle - BUTTON / 2.0, header.left - 6.0 + CHEVRON, middle + BUTTON / 2.0);
        self.icon_button(chevron, if folded { Icon::ChevronRight } else { Icon::ChevronDown }, Action::ToggleFold(index), Tone::Faint, panel, regions)?;
        let icon_x = header.left + CAPTION_INDENT;
        let place = rect(icon_x, middle - ICON / 2.0, icon_x + ICON, middle + ICON / 2.0);
        let target: ID2D1RenderTarget = self.target.cast()?;
        match section.exe_path.as_ref().and_then(|exe| self.icons.get(&target, exe)) {
            Some(bitmap) => self.scene.borrow_mut().bitmap(&bitmap, place),
            None if index == YOU_SECTION => self.icon(Icon::Mic, place, self.palette.accent)?,
            None => self.icon(Icon::Speaker, place, self.palette.text_dim)?,
        }

        let mut right = header.right;
        if panel.view.hovered_header == Some(index) {
            let mut share = vec![(Icon::Copy, Action::CopySection(index), Tone::Dim), (Icon::Sparkles, Action::ShareSection(index), Tone::Accent)];
            if !panel.auto_save {
                share.push((Icon::Save, Action::SaveSection(index), Tone::Dim));
            }
            let mut groups = vec![share];
            if index != YOU_SECTION {
                let pin = if panel.captions.is_pinned(index) { Tone::Accent } else { Tone::Dim };
                groups.push(vec![(Icon::Pin, Action::Pin(index), pin), (Icon::EyeOff, Action::Hide(index), Tone::Dim), (Icon::Ban, Action::Block(index), Tone::Dim)]);
            }
            right = self.draw_toolbar(&groups, header.right - TOOLBAR_INSET, middle, panel, regions)? - 6.0;
        }

        let live = (!panel.stopped && section.speaking(Instant::now())).then(|| self.layout(&self.fonts.strong, "Live", f32::MAX)).transpose()?;
        let badge_width = live.as_ref().map_or(Ok(0.0), |live| metrics(live).map(|m| (m.width + 27.0).ceil()))?;
        let label_x = icon_x + ICON + 8.0;
        let label = self.single_line(&self.fonts.label, &section.label, right - label_x - badge_width - 8.0)?;
        self.draw_layout(&label, label_x, middle - height_of(&label)? / 2.0, self.palette.text)?;
        let mut x = label_x + metrics(&label)?.width + 8.0;
        if let Some(live) = live {
            self.draw_live_badge(&live, rect(x, middle - 9.0, x + badge_width, middle + 9.0))?;
            x += badge_width + 8.0;
        }
        if let Some(latest) = section.captions.back().filter(|_| folded && right - x > 24.0) {
            let latest = self.single_line(&self.fonts.body, &latest.display_text(), right - x - 4.0)?;
            self.draw_layout(&latest, x, middle - height_of(&latest)? / 2.0, self.palette.text_faint)?;
        }
        Ok(())
    }

    /// An accent capsule with a dot and "Live".
    fn draw_live_badge(&self, live: &IDWriteTextLayout, badge: D2D_RECT_F) -> Result<()> {
        let (accent, middle) = (self.palette.accent, (badge.top + badge.bottom) / 2.0);
        let glass = Glass { top: with_alpha(accent, 0.2), bottom: with_alpha(accent, 0.1), rim_top: with_alpha(accent, 0.45), rim_bottom: with_alpha(accent, 0.2) };
        self.glass(badge, height(&badge) / 2.0, glass)?;
        self.fill_dot(badge.left + 10.0, middle, 3.0, accent)?;
        self.draw_layout(live, badge.left + 18.0, middle - height_of(live)? / 2.0, accent)
    }

    /// Small buttons in a floating capsule that ends at `right`, centred on `middle`; groups are set apart.
    /// Returns its left edge.
    fn draw_toolbar(&mut self, groups: &[Vec<(Icon, Action, Tone)>], right: f32, middle: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        const PAD: f32 = 2.0;
        const APART: f32 = 7.0;
        let count: usize = groups.iter().map(Vec::len).sum();
        let bar_width = 2.0 * PAD + TOOL_BUTTON * count as f32 + APART * (groups.len() - 1) as f32;
        let bar = rect(right - bar_width, middle - TOOL_BUTTON / 2.0 - PAD, right, middle + TOOL_BUTTON / 2.0 + PAD);
        self.shadow(bar, height(&bar) / 2.0, &TOOLBAR_SHADOW)?;
        self.glass(bar, height(&bar) / 2.0, self.palette.popover)?;
        let mut x = bar.left + PAD;
        for (index, group) in groups.iter().enumerate() {
            if index > 0 {
                let line = x + APART / 2.0;
                self.fill_rounded(rect(line - 0.5, middle - 7.0, line + 0.5, middle + 7.0), 0.0, self.palette.divider)?;
                x += APART;
            }
            for &(icon, action, tone) in group {
                self.icon_button(rect(x, bar.top + PAD, x + TOOL_BUTTON, bar.bottom - PAD), icon, action, tone, panel, regions)?;
                x += TOOL_BUTTON;
            }
        }
        Ok(bar.left)
    }

    /// Caption text can be drag-selected; the hovered line gets a toolbar with copy and ✨ in the gutter on the right.
    /// The first text line that starts in view goes to the top of `area`, so none shows cut in half there and every
    /// section's first line sits the same gap below its header; what is left of the height stays below the newest.
    #[allow(clippy::too_many_arguments)]
    fn draw_captions(
        &mut self,
        section: usize,
        captions: &VecDeque<Caption>,
        area: D2D_RECT_F,
        text_width: f32,
        top: f32,
        selected: &HashMap<LineRef, (u32, u32)>,
        panel: &Panel,
        regions: &mut Regions,
    ) -> Result<()> {
        let top = self.first_whole_line(section, captions, top, text_width)?;
        let rows = &self.section_rows[section].rows;
        let shown: Vec<(usize, f32, f32)> =
            rows.between(top, top + height(&area)).map(|row| (row, rows.top(row), rows.height(row) - ROW_GAP)).collect();
        let origin = area.top - top;
        // Wide enough for the highlight's bar, left of the text.
        self.push_clip(rect(area.left - HIGHLIGHT_INSET, area.top, area.right, area.bottom));
        for (row, row_top, line_height) in shown {
            let caption = &captions[row];
            let y = origin + row_top;
            let line = LineRef::of(section, caption);
            let layout = self.caption_layout(caption, text_width)?;
            if caption.speaker.is_some() && self.your_lines.highlight() {
                self.draw_your_highlight(area.left, y, text_width, line_height)?;
            }
            if let Some(&(start, end)) = selected.get(&line) {
                self.fill_text_range(&layout, start, end, area.left, y, self.palette.selected)?;
            }
            let visible = clipped(rect(area.left, y, area.left + text_width, y + line_height), area);
            let row = clipped(rect(area.left, y, area.right, y + line_height), area);
            regions.captions.push(CaptionRegion { line, visible, row, x: area.left, y, width: text_width });
            let hovered = matches!(panel.view.hovered, Some(Action::CopyLine(l) | Action::CorrectLine(l) | Action::ShareLine(l)) if l == line);
            if hovered || panel.view.hovered_line == Some(line) {
                let buttons = vec![
                    (Icon::Copy, Action::CopyLine(line), Tone::Dim),
                    (Icon::Pencil, Action::CorrectLine(line), Tone::Dim),
                    (Icon::Sparkles, Action::ShareLine(line), Tone::Accent),
                ];
                let first = regions.targets.len();
                // Level with the line's first row.
                let middle = y + line_height.min(CAPTION_SIZE * 1.33) / 2.0;
                self.draw_toolbar(&[buttons], area.right - TOOLBAR_INSET, middle, panel, regions)?;
                for target in &mut regions.targets[first..] {
                    target.0 = clipped(target.0, area);
                }
            }
            if caption.speaker.is_some() {
                // Color just the "You:" prefix, so speaker and finished/unfinished stay separate signals.
                let prefix = DWRITE_TEXT_RANGE { startPosition: 0, length: (YOU.len() + 1) as u32 };
                unsafe { layout.SetDrawingEffect(&self.speaker_brush, prefix)? };
            }
            self.draw_layout(&layout, area.left, y, caption_color(caption, &self.palette))?;
        }
        self.pop_clip();
        Ok(())
    }

    /// Where the first text line starting at or below `top` in a section's captions starts; `top` itself when
    /// it falls in the gap after a caption. Snapping by text line, not caption, keeps a live section from jumping.
    fn first_whole_line(&self, section: usize, captions: &VecDeque<Caption>, top: f32, text_width: f32) -> Result<f32> {
        let rows = &self.section_rows[section].rows;
        let Some(row) = rows.between(top, top + 1.0).next() else { return Ok(top) };
        let mut line_top = rows.top(row);
        let layout = self.caption_layout(&captions[row], text_width)?;
        for line_height in line_heights(&layout)? {
            if line_top >= top - 0.5 {
                return Ok(line_top);
            }
            line_top += line_height;
        }
        Ok(top)
    }

    /// A centred icon over a short title and a hint, for a screen with nothing to show.
    fn draw_empty(&self, icon: Icon, title: &str, hint: &str, area: D2D_RECT_F) -> Result<()> {
        const EMPTY_ICON: f32 = 32.0;
        let centre_x = (area.left + area.right) / 2.0;
        let title = self.layout(&self.fonts.label, title, f32::MAX)?;
        let hint = self.layout(&self.fonts.small, hint, width(&area).min(320.0))?;
        unsafe { hint.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)? };
        let (title_height, hint_height) = (height_of(&title)?, height_of(&hint)?);
        let mut y = area.top + (height(&area) - EMPTY_ICON - 12.0 - title_height - 4.0 - hint_height) * 0.4;
        self.icon(icon, rect(centre_x - EMPTY_ICON / 2.0, y, centre_x + EMPTY_ICON / 2.0, y + EMPTY_ICON), self.palette.text_faint)?;
        y += EMPTY_ICON + 12.0;
        self.draw_layout(&title, centre_x - metrics(&title)?.width / 2.0, y, self.palette.text)?;
        let hint_width = unsafe { hint.GetMaxWidth() };
        self.draw_layout(&hint, centre_x - hint_width / 2.0, y + title_height + 4.0, self.palette.text_dim)
    }

    /// A back button and a screen's heading; returns where the body starts.
    fn draw_screen_header(&mut self, heading: &str, back: Action, content: D2D_RECT_F, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        let middle = content.top + SECTION_HEADER / 2.0;
        let button = rect(content.left - 8.0, middle - BUTTON / 2.0, content.left - 8.0 + BUTTON, middle + BUTTON / 2.0);
        self.icon_button(button, Icon::ChevronLeft, back, Tone::Dim, panel, regions)?;
        let x = button.right + 4.0;
        let heading = self.single_line(&self.fonts.heading, heading, content.right - x)?;
        self.draw_layout(&heading, x, middle - height_of(&heading)? / 2.0, self.palette.text)?;
        Ok(content.top + SECTION_HEADER + 8.0)
    }

    /// A title bar that drags the window, with a close button; the tabs; and the open tab's settings.
    fn draw_settings_window(&mut self, panel: &Panel, regions: &mut Regions) -> Result<()> {
        self.draw_window(panel.width, panel.height)?;
        regions.targets.push((rect(0.0, 0.0, panel.width, TITLE_HEIGHT), Target::Drag));
        let middle = TITLE_HEIGHT / 2.0;
        let title_right = self.draw_brand(if panel.view.setup.is_some() { "Set up Kiku" } else { "Settings" }, panel.dpi, middle, f32::MAX)?;
        let close = rect(panel.width - PADDING + 6.0 - BUTTON, middle - BUTTON / 2.0, panel.width - PADDING + 6.0, middle + BUTTON / 2.0);
        if let Some((notice, colour)) = notice(panel, &self.palette) {
            let x = title_right + 14.0;
            let notice = self.single_line(&self.fonts.small, &notice, close.left - x - 8.0)?;
            self.draw_layout(&notice, x, middle - height_of(&notice)? / 2.0, colour)?;
        }
        self.icon_button(close, Icon::Close, Action::CloseSettings, Tone::Close, panel, regions)?;
        match panel.view.setup {
            Some(step) => {
                let steps_bottom = self.draw_setup_steps(step, panel.width, TITLE_HEIGHT)?;
                let footer_top = panel.height - SETUP_FOOTER;
                self.draw_settings(panel, rect(PADDING, steps_bottom + 4.0, panel.width - PADDING, footer_top), regions)?;
                self.draw_setup_footer(step, panel, footer_top, regions)?;
            }
            None => {
                let nav_bottom = self.draw_nav(panel, TITLE_HEIGHT, regions)?;
                self.draw_settings(panel, rect(PADDING, nav_bottom + 4.0, panel.width - PADDING, panel.height - 8.0), regions)?;
            }
        }
        self.draw_tooltip(panel, regions)
    }

    /// Setup's steps as numbered pills across the window, the current one lit and those done ticked.
    /// Returns the y below them.
    fn draw_setup_steps(&mut self, current: SetupStep, width: f32, top: f32) -> Result<f32> {
        let palette = self.palette.clone();
        let bar = rect(PADDING, top, width - PADDING, top + CONTROL + 8.0);
        let item_width = (self::width(&bar) - 8.0) / SetupStep::ALL.len() as f32;
        let at = SetupStep::ALL.iter().position(|&s| s == current).unwrap_or(0);
        for (index, step) in SetupStep::ALL.into_iter().enumerate() {
            let left = bar.left + 4.0 + index as f32 * item_width;
            let item = rect(left + 2.0, bar.top + 4.0, left + item_width - 2.0, bar.bottom - 4.0);
            let middle = (item.top + item.bottom) / 2.0;
            if index == at {
                self.glass(item, height(&item) / 2.0, palette.raised)?;
            }
            let colour = if index <= at { palette.text } else { palette.text_dim };
            let label = self.layout(if index == at { &self.fonts.strong } else { &self.fonts.small }, &format!("{}  {}", index + 1, step.name()), f32::MAX)?;
            let label_width = metrics(&label)?.width;
            let mut x = (item.left + item.right - label_width) / 2.0;
            if index < at {
                self.icon(Icon::Check, rect(x - 18.0, middle - 7.0, x - 4.0, middle + 7.0), palette.accent)?;
                x += 8.0;
            }
            self.draw_layout(&label, x, middle - height_of(&label)? / 2.0, colour)?;
        }
        Ok(bar.bottom)
    }

    /// Setup's buttons: Skip on the left; Back and Next (Done on the last step) on the right.
    fn draw_setup_footer(&mut self, step: SetupStep, panel: &Panel, top: f32, regions: &mut Regions) -> Result<()> {
        self.hairline(PADDING, panel.width - PADDING, top)?;
        let middle = top + SETUP_FOOTER / 2.0;
        let next = if step.next().is_some() { "Next" } else { "Done" };
        let mut right = self.draw_text_button(next, None, Action::SetupNext, Emphasis::Primary, panel.width - PADDING, middle, panel, regions)? - 8.0;
        if step.back().is_some() {
            right = self.draw_text_button("Back", None, Action::SetupBack, Emphasis::Secondary, right, middle, panel, regions)?;
        }
        let skip = self.layout(&self.fonts.button, "Skip setup", f32::MAX)?;
        let skip_right = PADDING + metrics(&skip)?.width + 28.0;
        if skip_right < right {
            self.draw_text_button("Skip setup", None, Action::SkipSetup, Emphasis::Ghost, skip_right, middle, panel, regions)?;
        }
        Ok(())
    }

    /// One step of setup: a line on what it is for, then the settings it sets, each with its default
    /// already picked. Returns the y below.
    fn draw_setup_step(&mut self, step: SetupStep, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        match step {
            SetupStep::Keys => {
                let intro = format!("Hold {} to dictate into any app; with a letter it runs a command. Click a row to change it, or keep these.", panel.keybinds.hold_name());
                let y = self.draw_intro(&intro, content, y)?;
                self.draw_shortcuts(panel, content, y, regions)
            }
            SetupStep::Model => {
                let y = self.draw_intro("Pick the model that turns speech into captions. Everything runs on this PC; nothing is sent anywhere.", content, y)?;
                let y = self.draw_recommendation(panel, content, y, regions)?;
                self.draw_models(panel, content, y + SECTION_GAP, regions)
            }
            SetupStep::Gpu => {
                let y = self.draw_intro("An NVIDIA graphics card runs the Parakeet models with almost no CPU. Kiku checks your card and driver for you.", content, y)?;
                self.draw_gpu(panel, content, y, regions)
            }
            SetupStep::Saving => {
                let y = self.draw_intro("Kiku can keep what it captions, to search later from History or with Claude. It stays on this PC.", content, y)?;
                let y = self.draw_transcript_settings(panel, content, y, regions)?;
                self.draw_update_settings(panel, content, y + SECTION_GAP, regions)
            }
        }
    }

    /// A step's opening line. Returns the y below.
    fn draw_intro(&mut self, text: &str, content: D2D_RECT_F, y: f32) -> Result<f32> {
        let intro = self.layout(&self.fonts.body, text, width(&content) - 8.0)?;
        self.draw_layout(&intro, content.left + 4.0, y, self.palette.text_dim)?;
        Ok(y + height_of(&intro)? + SECTION_GAP)
    }

    /// The model recommended for this PC and why, picked unless another is. Returns the y below.
    fn draw_recommendation(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let recommended = panel.hardware.recommend();
        let picked = panel.model.wanted();
        let chosen = picked.model == recommended.model && picked.device() == recommended.variant.device;
        let place = if recommended.variant.device == Device::Cuda { "on the GPU" } else { "on the CPU" };
        let y = self.draw_heading("Recommended for this PC", content, y)?;
        let y = self.draw_card(content, y, |renderer, inner, y| {
            let buttons: Vec<RowButton> = if chosen { Vec::new() } else { vec![("Use this", None, Action::UseRecommended, Emphasis::Secondary)] };
            renderer.draw_two_line_row(&format!("{} {place}", recommended.model.name), &recommended.why, &buttons, inner, y, panel, regions)
        })?;
        let pc = describe_pc(panel.hardware);
        let next = if chosen { "  Next downloads it if it isn't here yet." } else { "" };
        self.draw_hint(&format!("This PC: {pc}.{next}"), content, y)
    }

    /// The settings window's tabs as a bar of glass across it, each an icon over its name; the open one
    /// sits in a raised capsule. Returns the y below it.
    fn draw_nav(&mut self, panel: &Panel, top: f32, regions: &mut Regions) -> Result<f32> {
        let palette = self.palette.clone();
        let bar = rect(PADDING, top, panel.width - PADDING, top + NAV_HEIGHT);
        self.glass(bar, RADIUS_CARD, palette.nav)?;
        let item_width = (width(&bar) - 8.0) / Tab::ALL.len() as f32;
        for (index, tab) in Tab::ALL.into_iter().enumerate() {
            let left = bar.left + 4.0 + index as f32 * item_width;
            let item = rect(left, bar.top + 4.0, left + item_width, bar.bottom - 4.0);
            let action = Action::ShowTab(tab);
            let (open, (hovered, pressed)) = (panel.view.tab == tab, pointer_on(panel, action));
            if open {
                self.glass(item, RADIUS_CONTROL + 1.0, palette.raised)?;
            } else if pressed {
                self.glass(item, RADIUS_CONTROL + 1.0, palette.pressed)?;
            } else if hovered {
                self.fill_rounded(item, RADIUS_CONTROL + 1.0, palette.hover)?;
            }
            let centre_x = (item.left + item.right) / 2.0;
            let icon_colour = if open { palette.accent } else if hovered { palette.text } else { palette.text_dim };
            self.icon(tab_icon(tab), rect(centre_x - ICON / 2.0, item.top + 7.0, centre_x + ICON / 2.0, item.top + 7.0 + ICON), icon_colour)?;
            let label = self.layout(if open { &self.fonts.strong } else { &self.fonts.small }, tab.name(), f32::MAX)?;
            let label_colour = if open || hovered { palette.text } else { palette.text_dim };
            self.draw_layout(&label, centre_x - metrics(&label)?.width / 2.0, item.top + 9.0 + ICON, label_colour)?;
            regions.targets.push((item, Target::Click(action)));
        }
        Ok(bar.bottom)
    }

    /// The open tab's settings, scrolling when the window is too short for them.
    fn draw_settings(&mut self, panel: &Panel, area: D2D_RECT_F, regions: &mut Regions) -> Result<()> {
        // Scrolled by the height the last frame measured; this frame's is known once drawn.
        let scroll = panel.view.scroll(ScrollArea::Settings);
        let top = scroll.top(Extent { content: self.settings_height, viewport: height(&area) });

        let (first_target, first_area) = (regions.targets.len(), regions.scroll_areas.len());
        let start = area.top - top;
        self.push_clip(area);
        let bottom = self.draw_settings_body(panel, area, start + 12.0, top, regions);
        self.pop_clip();
        // Controls scrolled out of view must not be clickable.
        for target in &mut regions.targets[first_target..] {
            target.0 = clipped(target.0, area);
        }
        self.settings_height = bottom? + PADDING - start;
        let extent = Extent { content: self.settings_height, viewport: height(&area) };
        // Under any drop-down list's own scroll area.
        regions.scroll_areas.insert(first_area, (area, ScrollArea::Settings, extent));
        self.draw_scroll_thumb(area.right + THUMB_OFFSET, area, extent, scroll.top(extent))
    }

    /// The open tab's rows in `content` scrolled to `top`, faded into the window's edges where more of them
    /// is out of view, then any open drop-down's list over them. Returns the y below the last row.
    fn draw_settings_body(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, top: f32, regions: &mut Regions) -> Result<f32> {
        let mut anchors = None;
        let bottom = match (panel.view.setup, panel.view.tab) {
            (Some(step), _) => self.draw_setup_step(step, panel, content, y, regions)?,
            (None, Tab::General) => self.draw_general_settings(panel, content, y, regions)?,
            (None, Tab::Listening) => self.draw_listening_settings(panel, content, y, regions)?,
            (None, Tab::Apps) => self.draw_app_settings(panel, content, y, regions)?,
            (None, Tab::Words) => self.draw_word_settings(panel, content, y, regions)?,
            (None, Tab::Appearance) => {
                let (y, swatches) = self.draw_theme_card(panel, content, y, regions)?;
                let (bottom, font) = self.draw_subtitle_settings(panel, content, y + SECTION_GAP, regions)?;
                anchors = Some(AppearanceAnchors { font, swatches });
                bottom
            }
            (None, Tab::Shortcuts) => self.draw_shortcuts(panel, content, y, regions)?,
        };
        if top > 0.0 {
            self.fade(rect(content.left, content.top, content.right, content.top + SETTINGS_FADE), Edge::Top, panel.height)?;
        }
        if bottom + PADDING > content.bottom + 0.5 {
            self.fade(rect(content.left, content.bottom - SETTINGS_FADE, content.right, content.bottom), Edge::Bottom, panel.height)?;
        }
        match (anchors, panel.view.open_dropdown) {
            (Some(anchors), Some(Dropdown::Font)) => self.draw_font_list(anchors.font, content.bottom, panel, regions)?,
            (Some(anchors), Some(Dropdown::Colour(paint))) => self.draw_palette(paint, anchors.swatch(paint), content, panel, regions)?,
            _ => {}
        }
        Ok(bottom)
    }

    /// Sensitivity sliders (a live level meter per input, with the threshold as a draggable knob), the
    /// models and the GPU. Returns the y below the last row.
    fn draw_listening_settings(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let mut y = self.draw_heading("Sensitivity", content, y)?;
        y = self.draw_card(content, y, |renderer, inner, mut y| {
            for meter in &panel.meters {
                y = renderer.draw_meter(meter, inner, y, regions)?;
            }
            Ok(y)
        })?;
        y = self.draw_hint("Drag the knob; sound below it is ignored, like Discord's input sensitivity.", content, y)?;
        y = self.draw_models(panel, content, y + SECTION_GAP, regions)?;
        y = self.draw_gpu(panel, content, y + SECTION_GAP, regions)?;
        self.draw_remote_settings(panel, content, y + SECTION_GAP, regions)
    }

    /// The models with their downloads, the device picker by the heading, and adding one's own.
    /// Returns the y below.
    fn draw_models(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        self.draw_device_picker(panel, content, y, regions)?;
        let mut y = self.draw_heading_with_control("Model", content, y)?;
        y = self.draw_card(content, y, |renderer, inner, mut y| {
            for model in catalog::models() {
                y = renderer.draw_model_row(model, inner, y, panel, regions)?;
            }
            renderer.draw_add_model_row(inner, y, panel, regions)
        })?;
        let hint = match panel.import {
            ImportStatus::Idle | ImportStatus::Trying => "Download a model to pick it. Add a model takes a folder with a sherpa-onnx or Moonshine model of your own.",
            ImportStatus::Imported(text) | ImportStatus::Failed(text) => text,
        };
        self.draw_hint(hint, content, y)
    }

    /// The GPU: the card found and its driver, the GPU pack with its download, and why models run on the
    /// CPU if they do. Returns the y below.
    fn draw_gpu(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let y = self.draw_heading("GPU", content, y)?;
        let problem = panel.hardware.gpu_problem();
        let y = self.draw_card(content, y, |renderer, inner, y| {
            let (card, detail) = match (&panel.hardware.gpu, &problem) {
                (Some(gpu), None) => (format!("NVIDIA {}", gpu.name), gpu_detail(gpu)),
                (_, Some(problem)) => ("No GPU for models".to_owned(), format!("{}: models run on the CPU", capitalised(problem))),
                (None, None) => unreachable!("a missing GPU is a problem"),
            };
            let mut y = renderer.draw_two_line_row(&card, &detail, &[], inner, y, panel, regions)?;
            if problem.is_none() {
                let on_gpu = panel.model.wanted().device == Device::Cuda;
                let devices = [("GPU", on_gpu, Action::PickDevice(Device::Cuda)), ("CPU", !on_gpu, Action::PickDevice(Device::Cpu))];
                y = renderer.draw_choice_row("Run models on", &devices, inner, y, panel, regions)?;
                let pack = catalog::component(GPU_PACK);
                let target = Download::Component(pack);
                let buttons = download_buttons(target, pack.installed(), panel);
                let detail = panel.downloads.progress(target).map(|p| progress_text(&p)).unwrap_or_else(|| match pack.installed() {
                    true => "Installed next to Kiku".to_owned(),
                    false => format!("{} from NVIDIA and Microsoft, once, for every GPU model", megabytes(pack.size())),
                });
                y = renderer.draw_two_line_row("GPU pack: CUDA 12, cuDNN 9", &detail, &buttons, inner, y, panel, regions)?;
            }
            Ok(y)
        })?;
        let hint = match (&panel.model.gpu_fallback, problem) {
            (Some(why), _) => format!("{} runs on the CPU for now: {why}.", panel.model.active.model.name),
            (None, Some(_)) => "Every model runs on the CPU; Moonshine and the int8 Parakeets are made for it.".to_owned(),
            (None, None) => "On the GPU, the pack downloads with the model if it isn't here yet. Moonshine always runs on the CPU, and the CPU always works.".to_owned(),
        };
        self.draw_hint(&hint, content, y)
    }

    /// How the panel folds apps, then blocking: what the block button does, and the blocked apps. Returns the y below.
    fn draw_app_settings(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let mut y = self.draw_heading("Folding", content, y)?;
        // "Never" last, after the minutes.
        let minutes: Vec<u32> = AUTO_FOLD_MINUTES.iter().copied().filter(|&m| m != 0).chain([0]).collect();
        let labels: Vec<String> = minutes.iter().map(|&m| if m == 0 { "Never".into() } else { format!("{m} min") }).collect();
        let choices: Vec<_> = minutes.iter().zip(&labels).map(|(&m, label)| (label.as_str(), panel.auto_fold == m, Action::AutoFold(m))).collect();
        y = self.draw_card(content, y, |renderer, inner, y| renderer.draw_choice_row("Fold apps quiet for", &choices, inner, y, panel, regions))?;
        let hint = "A folded app keeps one row with its latest line, and opens again when it speaks. The arrow by an app's name folds or opens it by hand, which stays; double-click the name to show that app alone, and again to undo. Hiding an app takes it off the panel only: it is still captioned and saved.";
        y = self.draw_hint(hint, content, y)?;

        y = self.draw_heading("Blocking", content, y + SECTION_GAP)?;
        let answers = [("Ask first", BlockAnswer::Ask), ("Just this session", BlockAnswer::Session), ("Always", BlockAnswer::Always)];
        let answers = answers.map(|(label, answer)| (label, panel.block_answer == answer, Action::SetBlockAnswer(answer)));
        y = self.draw_card(content, y, |renderer, inner, mut y| {
            y = renderer.draw_choice_row("The block button", &answers, inner, y, panel, regions)?;
            for (index, app) in panel.blocked.iter().enumerate() {
                y = renderer.draw_blocked_row(index, app, inner, y, panel, regions)?;
            }
            Ok(y)
        })?;
        if panel.blocked.is_empty() {
            let none = "No apps are blocked. The block button on an app stops capturing and captioning it; what it said before stays in History.";
            y = self.draw_hint(none, content, y)?;
        }

        y = self.draw_heading("Your lines", content, y + SECTION_GAP)?;
        let looks = [("Italics", YourLines::Italic), ("Highlight", YourLines::Highlight), ("Both", YourLines::Both)];
        let looks = looks.map(|(label, look)| (label, panel.your_lines == look, Action::SetYourLines(look)));
        let pinned = &panel.captions.pinned;
        y = self.draw_card(content, y, |renderer, inner, mut y| {
            y = renderer.draw_choice_row("Mark them with", &looks, inner, y, panel, regions)?;
            for (index, app) in pinned.iter().enumerate() {
                let unpin = [("Unpin", None, Action::Unpin(index), Emphasis::Secondary)];
                y = renderer.draw_two_line_row(app, "Gets every line you say", &unpin, inner, y, panel, regions)?;
            }
            Ok(y)
        })?;
        let hint = "What you say also shows in the app you are in, marked so it stands apart from the app's lines. The pin button on an app puts every line you say in it instead, whichever window is in front.";
        self.draw_hint(hint, content, y)
    }

    /// A blocked app over how long it is blocked for, and Unblock. Returns the y below.
    fn draw_blocked_row(&mut self, index: usize, app: &Blocked, content: D2D_RECT_F, y: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        self.begin_row(content, y)?;
        let middle = y + MODEL_ROW / 2.0;
        let unblock = self.draw_text_button("Unblock", None, Action::Unblock(index), Emphasis::Secondary, content.right, middle, panel, regions)?;
        let text_width = unblock - content.left - 12.0;
        let name = self.single_line(&self.fonts.body, &app.name, text_width)?;
        let until = self.single_line(&self.fonts.small, if app.always { "Blocked always" } else { "Blocked until Kiku quits" }, text_width)?;
        let (name_height, until_height) = (height_of(&name)?, height_of(&until)?);
        let top = middle - (name_height + until_height) / 2.0;
        self.draw_layout(&name, content.left, top, self.palette.text)?;
        self.draw_layout(&until, content.left, top + name_height, self.palette.text_dim)?;
        Ok(y + MODEL_ROW)
    }

    /// Save mode, updates, then the way back into setup. Returns the y below.
    fn draw_general_settings(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let y = self.draw_mode_settings(panel, content, y, regions)?;
        let y = self.draw_transcript_settings(panel, content, y + SECTION_GAP, regions)?;
        let y = self.draw_update_settings(panel, content, y + SECTION_GAP, regions)?;
        let y = self.draw_heading("Setup", content, y + SECTION_GAP)?;
        self.draw_card(content, y, |renderer, inner, y| {
            let buttons = [("Run setup again", None, Action::OpenSetup, Emphasis::Secondary)];
            renderer.draw_two_line_row("First-run setup", "Keys, model, GPU and saving, one step at a time", &buttons, inner, y, panel, regions)
        })
    }

    /// Captions and dictation, or dictation only. Returns the y below.
    fn draw_mode_settings(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let y = self.draw_heading("Mode", content, y)?;
        let only = panel.dictation_only;
        let y = self.draw_card(content, y, |renderer, inner, y| {
            let modes = [("Captions and dictation", !only, Action::DictationOnly(false)), ("Dictation only", only, Action::DictationOnly(true))];
            renderer.draw_choice_row("Kiku does", &modes, inner, y, panel, regions)
        })?;
        let hint = format!("Dictation only captions no apps and hears your mic only while you hold {}.", panel.keybinds.hold_name());
        self.draw_hint(&hint, content, y)
    }

    /// Whether Kiku checks for updates, and what a check sends. Returns the y below.
    fn draw_update_settings(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let y = self.draw_heading("Updates", content, y)?;
        let on = panel.check_updates;
        let version = format!("Kiku {}", env!("CARGO_PKG_VERSION"));
        let y = self.draw_card(content, y, |renderer, inner, y| {
            if !update::available() {
                return renderer.draw_two_line_row(&version, "This build doesn't check for updates", &[], inner, y, panel, regions);
            }
            let y = renderer.draw_toggle_row("Check once a day", on, Action::CheckUpdates(!on), inner, y, panel, regions)?;
            let (state, button) = match &panel.view.update_check {
                Some(UpdateCheck::Checking) => ("Checking…".to_owned(), "Checking…"),
                Some(UpdateCheck::Latest) => ("The latest version".to_owned(), "Check now"),
                Some(UpdateCheck::Failed(why)) => (format!("Couldn't check: {why}"), "Try again"),
                None => ("The version you have".to_owned(), "Check now"),
            };
            let check = [(button, None, Action::CheckUpdatesNow, Emphasis::Secondary)];
            renderer.draw_two_line_row(&version, &state, &check, inner, y, panel, regions)
        })?;
        let hint = "A check reads the latest version from Kiku's releases page; nothing else is sent.";
        self.draw_hint(hint, content, y)
    }

    /// Save mode, and in auto mode its own settings. Returns the y below.
    fn draw_transcript_settings(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let y = self.draw_heading("Transcripts", content, y)?;
        let y = self.draw_card(content, y, |renderer, inner, mut y| {
            let modes = [("Auto", panel.auto_save, Action::SetAutoSave(true)), ("Manual", !panel.auto_save, Action::SetAutoSave(false))];
            y = renderer.draw_choice_row("Save mode", &modes, inner, y, panel, regions)?;
            if panel.auto_save {
                let mic_only = panel.save_mic_only;
                y = renderer.draw_toggle_row("Save only my mic", mic_only, Action::SaveMicOnly(!mic_only), inner, y, panel, regions)?;
                let days = [("1 day", 1), ("1 week", 7), ("30 days", 30), ("90 days", 90), ("1 year", 365), ("Never", 0)];
                let days = days.map(|(l, d)| (l, panel.keep_days == d, Action::KeepDays(d)));
                y = renderer.draw_choice_row("Delete after", &days, inner, y, panel, regions)?;
            }
            Ok(y)
        })?;
        let hint = if panel.auto_save {
            "Each finished line is saved as it's said, so you and Claude can search past talk by app, by you, or by words."
        } else {
            "Nothing is saved until you press a save button (title bar for every app, or an app's own). Saved lines are kept."
        };
        self.draw_hint(hint, content, y)
    }

    /// A heading in small capitals. Returns the y below.
    fn draw_heading(&mut self, heading: &str, content: D2D_RECT_F, y: f32) -> Result<f32> {
        let heading = self.layout(&self.fonts.overline, &heading.to_uppercase(), width(&content))?;
        let spacing = DWRITE_TEXT_RANGE { startPosition: 0, length: u32::MAX };
        unsafe { heading.cast::<IDWriteTextLayout1>()?.SetCharacterSpacing(0.0, 0.7, 0.0, spacing)? };
        self.draw_layout(&heading, content.left + 4.0, y, self.palette.text_dim)?;
        Ok(y + height_of(&heading)? + HEADING_GAP)
    }

    /// A heading with a control on its right placed by [`heading_control`]. Returns the y below both.
    fn draw_heading_with_control(&mut self, heading: &str, content: D2D_RECT_F, y: f32) -> Result<f32> {
        let control_bottom = heading_control(content, y).1 + CONTROL / 2.0;
        Ok(self.draw_heading(heading, content, y)?.max(control_bottom + HEADING_GAP))
    }

    /// A hint under a card. Returns the y below.
    fn draw_hint(&mut self, hint: &str, content: D2D_RECT_F, y: f32) -> Result<f32> {
        let hint = self.layout(&self.fonts.small, hint, width(&content) - 8.0)?;
        self.draw_layout(&hint, content.left + 4.0, y + 8.0, self.palette.text_dim)?;
        Ok(y + 8.0 + height_of(&hint)?)
    }

    /// A card across `content` from `y`, holding what `rows` draws inside its padding, from the y it is
    /// given to the y it returns. Returns the y below the card.
    fn draw_card(&mut self, content: D2D_RECT_F, y: f32, rows: impl FnOnce(&mut Self, D2D_RECT_F, f32) -> Result<f32>) -> Result<f32> {
        let mark = self.scene.borrow().mark();
        let inner = rect(content.left + GAP, content.top, content.right - GAP, content.bottom);
        self.card_first_row.set(y + 4.0);
        let bottom = rows(self, inner, y + 4.0)? + 4.0;
        let card = rect(content.left, y, content.right, bottom);
        self.scene.borrow_mut().under(mark, |scene| scene.glass(card, RADIUS_CARD, self.palette.card));
        Ok(bottom)
    }

    /// Starts a row in a card at `y`: below the card's first row, a hairline sets it apart from the one above.
    fn begin_row(&self, content: D2D_RECT_F, y: f32) -> Result<()> {
        if y > self.card_first_row.get() + 0.5 {
            self.hairline(content.left, content.right, y)?;
        }
        Ok(())
    }

    /// A setting's name at the left of its row.
    fn draw_row_label(&self, label: &str, content: D2D_RECT_F, y: f32) -> Result<()> {
        self.begin_row(content, y)?;
        let label = self.layout(&self.fonts.body, label, width(&content))?;
        self.draw_layout(&label, content.left, y + (SETTING_ROW - height_of(&label)?) / 2.0, self.palette.text)
    }

    /// A label on the left and choices on the right, left to right. Returns the y below.
    fn draw_choice_row(&mut self, label: &str, choices: &[(&str, bool, Action)], content: D2D_RECT_F, y: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        self.draw_row_label(label, content, y)?;
        // The chosen one isn't clicked again.
        let choices: Vec<_> = choices.iter().map(|&(label, chosen, action)| (label, chosen, (!chosen).then_some(action))).collect();
        self.draw_segments(&choices, content.right, y + SETTING_ROW / 2.0, panel, regions)?;
        Ok(y + SETTING_ROW)
    }

    /// A label on the left and a switch on the right that `action` flips. Returns the y below.
    #[allow(clippy::too_many_arguments)]
    fn draw_toggle_row(&mut self, label: &str, on: bool, action: Action, content: D2D_RECT_F, y: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        const TRACK_WIDTH: f32 = 40.0;
        const TRACK_HEIGHT: f32 = 22.0;
        self.draw_row_label(label, content, y)?;
        let middle = y + SETTING_ROW / 2.0;
        let track = rect(content.right - TRACK_WIDTH, middle - TRACK_HEIGHT / 2.0, content.right, middle + TRACK_HEIGHT / 2.0);
        let radius = TRACK_HEIGHT / 2.0;
        self.glass(track, radius, if on { self.palette.primary } else { self.palette.track })?;
        if pointer_on(panel, action).0 {
            self.fill_rounded(track, radius, self.palette.hover)?;
        }
        let knob = TRACK_HEIGHT - 4.0;
        let x = if on { track.right - 2.0 - knob } else { track.left + 2.0 };
        self.draw_knob(rect(x, track.top + 2.0, x + knob, track.bottom - 2.0))?;
        regions.targets.push((rect(track.left - 6.0, middle - 14.0, track.right + 4.0, middle + 14.0), Target::Click(action)));
        Ok(y + SETTING_ROW)
    }

    /// Choices of equal width side by side, left to right, in a recessed capsule whose right edge is
    /// `right`, centred on `middle`. The chosen one is lit; one without an action can't be clicked.
    /// Returns the capsule's left edge.
    fn draw_segments(&mut self, choices: &[(&str, bool, Option<Action>)], right: f32, middle: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        let palette = self.palette.clone();
        let mut widest: f32 = 0.0;
        for (label, ..) in choices {
            widest = widest.max(metrics(&self.layout(&self.fonts.strong, label, f32::MAX)?)?.width);
        }
        let segment_width = widest.ceil() + 2.0 * SEGMENT_PADDING;
        let track = rect(right - segment_width * choices.len() as f32 - 4.0, middle - CONTROL / 2.0, right, middle + CONTROL / 2.0);
        self.glass(track, CONTROL / 2.0, palette.track)?;
        for (index, &(label, chosen, action)) in choices.iter().enumerate() {
            let left = track.left + 2.0 + segment_width * index as f32;
            let segment = rect(left, track.top + 2.0, left + segment_width, track.bottom - 2.0);
            let radius = height(&segment) / 2.0;
            let (hovered, pressed) = action.map_or((false, false), |action| pointer_on(panel, action));
            if chosen {
                self.glass(segment, radius, palette.chosen)?;
            } else if pressed {
                self.glass(segment, radius, palette.pressed)?;
            } else if hovered {
                self.fill_rounded(segment, radius, palette.hover)?;
            }
            let colour = match (chosen || hovered, action) {
                (true, _) => palette.text,
                (false, Some(_)) => palette.text_dim,
                (false, None) => with_alpha(palette.text_faint, 0.55),
            };
            let layout = self.layout(if chosen { &self.fonts.strong } else { &self.fonts.small }, label, f32::MAX)?;
            let size = metrics(&layout)?;
            self.draw_layout(&layout, (segment.left + segment.right - size.width) / 2.0, middle - size.height / 2.0, colour)?;
            if let Some(action) = action {
                regions.targets.push((segment, Target::Click(action)));
            }
        }
        Ok(track.left)
    }

    /// A drop-down's closed box ending at `right`, centred in the setting row at `row`: the current value and
    /// a chevron. Returns where it is.
    fn draw_dropdown_button(&mut self, value: &str, action: Action, right: f32, row: f32, panel: &Panel, regions: &mut Regions) -> Result<D2D_RECT_F> {
        const CHEVRON_ICON: f32 = 12.0;
        let middle = row + SETTING_ROW / 2.0;
        let value = self.layout(&self.fonts.small, value, f32::MAX)?;
        let area = rect(right - (metrics(&value)?.width + CHEVRON_ICON + 32.0).ceil(), middle - CONTROL / 2.0, right, middle + CONTROL / 2.0);
        let open = matches!(action, Action::ToggleDropdown(d) if panel.view.open_dropdown == Some(d));
        let (hovered, pressed) = pointer_on(panel, action);
        let palette = self.palette.clone();
        self.glass(area, RADIUS_CONTROL, if open || pressed { palette.pressed } else { palette.plate })?;
        if hovered && !open {
            self.fill_rounded(area, RADIUS_CONTROL, palette.hover)?;
        }
        self.draw_layout(&value, area.left + 12.0, middle - height_of(&value)? / 2.0, palette.text)?;
        let chevron = rect(area.right - 10.0 - CHEVRON_ICON, middle - CHEVRON_ICON / 2.0, area.right - 10.0, middle + CHEVRON_ICON / 2.0);
        self.icon(if open { Icon::ChevronUp } else { Icon::ChevronDown }, chevron, palette.text_dim)?;
        regions.targets.push((area, Target::Click(action)));
        Ok(area)
    }

    /// An open drop-down's list by its box `anchor`, drawn over the rows around it (see [`list_top`]); the
    /// chosen option is ticked. A long list shows [`DROPDOWN_ROWS`] at a time and scrolls.
    #[allow(clippy::too_many_arguments)]
    fn draw_dropdown_list(
        &mut self,
        dropdown: Dropdown,
        anchor: D2D_RECT_F,
        bottom: f32,
        options: &[(&str, bool, Action)],
        panel: &Panel,
        regions: &mut Regions,
    ) -> Result<()> {
        const ROW: f32 = 30.0;
        let extent = Extent { content: ROW * options.len() as f32, viewport: ROW * options.len().min(DROPDOWN_ROWS) as f32 };
        let top = panel.view.scroll(ScrollArea::Dropdown(dropdown)).top(extent);
        let shown = (top / ROW) as usize..(((top + extent.viewport) / ROW).ceil() as usize).min(options.len());
        let mut widest = width(&anchor);
        for (label, ..) in &options[shown.clone()] {
            widest = widest.max(metrics(&self.layout(&self.fonts.body, label, f32::MAX)?)?.width + 48.0);
        }
        let tall = extent.viewport + 8.0;
        let list_top = list_top(anchor, tall, bottom);
        let list = rect(anchor.right - widest, list_top, anchor.right, list_top + tall);
        self.popover(list, RADIUS_CARD)?;
        let rows = rect(list.left, list.top + 4.0, list.right, list.bottom - 4.0);
        regions.scroll_areas.push((rows, ScrollArea::Dropdown(dropdown), extent));
        self.push_clip(rows);
        for (index, &(label, chosen, action)) in options.iter().enumerate().take(shown.end).skip(shown.start) {
            let row_top = rows.top - top + ROW * index as f32;
            let row = rect(list.left + 4.0, row_top, list.right - 4.0, row_top + ROW);
            if panel.view.hovered == Some(action) {
                self.fill_rounded(row, RADIUS_CONTROL - 2.0, self.palette.hover)?;
            }
            let middle = (row.top + row.bottom) / 2.0;
            if chosen {
                self.icon(Icon::Check, rect(row.left + 8.0, middle - 7.0, row.left + 22.0, middle + 7.0), self.palette.accent)?;
            }
            let label = self.layout(&self.fonts.body, label, f32::MAX)?;
            self.draw_layout(&label, row.left + 30.0, middle - height_of(&label)? / 2.0, self.palette.text)?;
            regions.targets.push((clipped(row, rows), Target::Click(action)));
        }
        self.pop_clip();
        self.draw_scroll_thumb(list.right - 4.0, rows, extent, top)
    }

    /// The font list by its box `anchor`, drawn over the rows around it (see [`list_top`]): a search field, then
    /// the fonts matching it, the chosen one ticked, [`DROPDOWN_ROWS`] at a time. Only the rows in view are laid out.
    fn draw_font_list(&mut self, anchor: D2D_RECT_F, bottom: f32, panel: &Panel, regions: &mut Regions) -> Result<()> {
        const WIDTH: f32 = 280.0;
        let view = panel.view;
        // As tall as the whole list would be whatever is typed, so the field stays put while typing.
        let viewport = LIST_ROW * view.fonts.len().clamp(1, DROPDOWN_ROWS) as f32;
        let extent = Extent { content: LIST_ROW * view.font_matches.len() as f32, viewport };
        let top = view.scroll(ScrollArea::Dropdown(Dropdown::Font)).top(extent);
        let tall = 6.0 + CONTROL + 4.0 + viewport + 4.0;
        let list_top = list_top(anchor, tall, bottom);
        let list = rect(anchor.right - WIDTH.max(width(&anchor)), list_top, anchor.right, list_top + tall);
        self.popover(list, RADIUS_CARD)?;
        // Clicks on the field or between rows keep the list open and reach nothing under it.
        regions.targets.push((list, Target::Card));
        let role = BoxRole { one_line: true, icon: Some(Icon::Search), typing: true, click: None, clear: Some(Action::ClearFontSearch) };
        let field = self.draw_text_box(&view.font_search, "Search fonts", rect(list.left + 6.0, list.top + 6.0, list.right - 6.0, list.bottom), role, panel, regions)?;
        let rows = rect(list.left, field.bottom + 4.0, list.right, list.bottom - 4.0);
        regions.scroll_areas.push((rows, ScrollArea::Dropdown(Dropdown::Font), extent));
        if view.font_matches.is_empty() {
            let none = self.layout(&self.fonts.body, "No fonts match", f32::MAX)?;
            return self.draw_layout(&none, field.left + BOX_ICON, rows.top + (LIST_ROW - height_of(&none)?) / 2.0, self.palette.text_faint);
        }
        let shown = (top / LIST_ROW) as usize..(((top + viewport) / LIST_ROW).ceil() as usize).min(view.font_matches.len());
        // While searching, Enter picks the first match: it shows as hovered unless another row is.
        let enter_picks = !view.font_search.text.is_empty() && !matches!(view.hovered, Some(Action::SubtitleFont(_)));
        self.push_clip(rows);
        for (place, &font) in view.font_matches[shown.clone()].iter().enumerate() {
            let row_top = rows.top - top + LIST_ROW * (shown.start + place) as f32;
            // Rows share the field's edges, and names start where typed text does.
            let row = rect(field.left, row_top, field.right, row_top + LIST_ROW);
            let action = Action::SubtitleFont(font);
            if view.hovered == Some(action) || (enter_picks && shown.start + place == 0) {
                self.fill_rounded(row, RADIUS_CONTROL - 2.0, self.palette.hover)?;
            }
            let middle = (row.top + row.bottom) / 2.0;
            let name = &view.fonts[font];
            if *name == panel.subtitles.font {
                // In the column of the field's magnifier.
                self.icon(Icon::Check, rect(row.left + 9.0, middle - 7.0, row.left + 23.0, middle + 7.0), self.palette.accent)?;
            }
            let label = self.single_line(&self.fonts.body, name, width(&row) - BOX_ICON - 10.0)?;
            self.draw_layout(&label, row.left + BOX_ICON, middle - height_of(&label)? / 2.0, self.palette.text)?;
            regions.targets.push((clipped(row, rows), Target::Click(action)));
        }
        self.pop_clip();
        self.draw_scroll_thumb(list.right - 4.0, rows, extent, top)
    }

    /// One row per shortcut, its keys on key caps; clicking a row waits for its new keys. Returns the y below the last row.
    fn draw_shortcuts(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        let (right, middle) = heading_control(content, y);
        self.draw_text_button("Reset", Some(Icon::Reset), Action::ResetKeys, Emphasis::Secondary, right, middle, panel, regions)?;
        let y = self.draw_heading_with_control("Shortcuts", content, y)?;
        let binds = panel.keybinds;
        let mut rows = vec![(Rebind::Hold, "Hold to dictate into any text box".to_owned(), binds.hold_name())];
        rows.extend(Command::ALL.map(|c| (Rebind::Command(c), c.label().to_owned(), binds.shortcut(c))));
        self.draw_card(content, y, |renderer, inner, mut y| {
            let palette = renderer.palette.clone();
            for (rebind, label, keys) in rows {
                renderer.begin_row(inner, y)?;
                let row = rect(inner.left - 8.0, y + 3.0, inner.right + 8.0, y + SETTING_ROW - 3.0);
                let action = Action::Rebind(rebind);
                let waiting = panel.view.rebinding == Some(rebind);
                if waiting {
                    let accent = palette.accent;
                    let glass = Glass { top: with_alpha(accent, 0.12), bottom: with_alpha(accent, 0.06), rim_top: with_alpha(accent, 0.6), rim_bottom: with_alpha(accent, 0.35) };
                    renderer.glass(row, RADIUS_CONTROL, glass)?;
                } else if panel.view.hovered == Some(action) {
                    renderer.fill_rounded(row, RADIUS_CONTROL, palette.hover)?;
                }
                let middle = y + SETTING_ROW / 2.0;
                let keys_left = match (waiting, rebind) {
                    (true, Rebind::Hold) if !panel.captured.is_empty() => {
                        let held: Vec<String> = panel.captured.iter().map(|k| k.name()).collect();
                        renderer.draw_key_caps(&held, inner.right, middle)?
                    }
                    (true, _) => {
                        let prompt = match rebind {
                            Rebind::Hold => "Hold the new keys, then let go (Esc cancels)".to_owned(),
                            Rebind::Command(_) => format!("Press the key to use with {} (Esc cancels)", binds.hold_name()),
                        };
                        let prompt = renderer.layout(&renderer.fonts.small, &prompt, f32::MAX)?;
                        let left = inner.right - metrics(&prompt)?.width;
                        renderer.draw_layout(&prompt, left, middle - height_of(&prompt)? / 2.0, palette.accent)?;
                        left
                    }
                    (false, _) => renderer.draw_key_caps(&keys.split('+').map(str::to_owned).collect::<Vec<_>>(), inner.right, middle)?,
                };
                let label = renderer.single_line(&renderer.fonts.body, &label, keys_left - inner.left - 12.0)?;
                renderer.draw_layout(&label, inner.left, middle - height_of(&label)? / 2.0, palette.text)?;
                regions.targets.push((rect(inner.left - 8.0, y, inner.right + 8.0, y + SETTING_ROW), Target::Click(action)));
                y += SETTING_ROW;
            }
            Ok(y)
        })
    }

    /// Each key on a cap of its own, raised on a lip, the last one ending at `right`. Returns the first's left edge.
    fn draw_key_caps(&self, keys: &[String], right: f32, middle: f32) -> Result<f32> {
        const CAP: f32 = 22.0;
        /// Wider than any one letter or digit, so caps line up in columns from row to row.
        const LETTER_CAP: f32 = 26.0;
        let mut right = right;
        for key in keys.iter().rev() {
            let label = self.layout(&self.fonts.strong, key, f32::MAX)?;
            let size = metrics(&label)?;
            let cap_width = if key.chars().count() == 1 { LETTER_CAP } else { (size.width + 16.0).max(LETTER_CAP).ceil() };
            let cap = rect(right - cap_width, middle - CAP / 2.0 - 1.0, right, middle + CAP / 2.0 - 1.0);
            self.fill_rounded(rect(cap.left, cap.top + 2.0, cap.right, cap.bottom + 2.0), 5.0, self.palette.shadow)?;
            self.glass(cap, 5.0, self.palette.plate)?;
            self.draw_layout(&label, (cap.left + cap.right - size.width) / 2.0, (cap.top + cap.bottom - size.height) / 2.0, self.palette.text)?;
            right = cap.left - 4.0;
        }
        Ok(right + 4.0)
    }

    /// "Run on: CPU | GPU" on the right of the Model heading. GPU needs a model with a GPU build (not
    /// Moonshine) and an NVIDIA card with a recent enough driver; the GPU pack downloads with a model.
    fn draw_device_picker(&mut self, panel: &Panel, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<()> {
        let choice = panel.model.wanted();
        let wanted = choice.device();
        let gpu_model = choice.model.has_gpu_variant();
        let usable = gpu_model && panel.hardware.gpu_problem().is_none();
        let options = [(Device::Cpu, "CPU", true), (Device::Cuda, "GPU", usable)];
        let options = options.map(|(device, label, usable)| (label, wanted == device, (usable && wanted != device).then_some(Action::PickDevice(device))));
        let (right, middle) = heading_control(content, y);
        let left = self.draw_segments(&options, right, middle, panel, regions)?;
        let note = match (gpu_model, panel.hardware.gpu_problem()) {
            (false, _) => "Run on (this model is CPU only)",
            (true, Some(_)) => "Run on (no usable GPU)",
            (true, None) => "Run on",
        };
        let note = self.layout(&self.fonts.small, note, f32::MAX)?;
        self.draw_layout(&note, left - metrics(&note)?.width - 8.0, middle - height_of(&note)? / 2.0, self.palette.text_dim)
    }

    /// A radio-style row: the picked model's radio lit and its row tinted, its name over its note (or why
    /// it failed), and on the right its download: the size, its progress, or Delete. Returns the y below.
    fn draw_model_row(&mut self, model: &'static Model, content: D2D_RECT_F, y: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        self.begin_row(content, y)?;
        let palette = self.palette.clone();
        let choice = Choice { model, device: panel.model.wanted().device };
        let target = Download::Model(model, choice.variant());
        let runnable = download::runnable(choice);
        let picked = panel.model.wanted().model == model;
        let action = Action::PickModel(model);
        let row = rect(content.left - 8.0, y + 3.0, content.right + 8.0, y + MODEL_ROW - 3.0);
        if picked && runnable {
            self.fill_rounded(row, RADIUS_CONTROL, with_alpha(palette.accent, palette.selected.a * 0.45))?;
        } else if runnable && panel.view.hovered == Some(action) {
            self.fill_rounded(row, RADIUS_CONTROL, palette.hover)?;
        }
        if runnable {
            regions.targets.push((row, Target::Click(action)));
        }
        let middle = y + MODEL_ROW / 2.0;
        let radio_x = content.left + 8.0;
        if picked {
            self.glass(rect(radio_x - 8.0, middle - 8.0, radio_x + 8.0, middle + 8.0), 8.0, palette.primary)?;
            self.fill_dot(radio_x, middle, 3.0, white(1.0))?;
        } else {
            let ring = if runnable { palette.text_faint } else { with_alpha(palette.text_faint, 0.5) };
            self.scene.borrow_mut().ring(radio_x, middle, 7.25, 1.5, ring);
        }

        // A GPU build's pack downloads as a job of its own after the model's; the row follows whichever runs.
        let target = [Some(target), target.gpu_pack()].into_iter().flatten().find(|t| panel.downloads.progress(*t).is_some()).unwrap_or(target);
        // A running model whose build for the device picked is missing still offers its download.
        let in_use = panel.model.active.model == model && panel.model.loaded && target.complete();
        let buttons = if in_use { Vec::new() } else { download_buttons(target, target.complete(), panel) };
        let mut right = content.right;
        for &(label, icon, action, emphasis) in buttons.iter().rev() {
            right = self.draw_text_button(label, icon, action, emphasis, right, middle, panel, regions)? - 6.0;
        }
        let (status, colour) = model_status(model, target, in_use, panel, &palette);
        if !status.is_empty() {
            let status = self.layout(&self.fonts.small, &status, f32::MAX)?;
            right -= metrics(&status)?.width;
            self.draw_layout(&status, right, middle - height_of(&status)? / 2.0, colour)?;
            right -= 10.0;
        }

        let text_x = radio_x + 18.0;
        let text_width = right - text_x - 8.0;
        let failure = panel.model.failure.as_ref().filter(|_| panel.model.failed.is_some_and(|c| c.model == model));
        let note = match (failure, panel.downloads.progress(target)) {
            (Some(why), _) => (format!("Failed to load: {why}"), palette.error),
            (None, Some(Progress { state: State::Failed(why), .. })) => (format!("Download failed: {why}"), palette.error),
            _ => (model_note(model, choice.device()), if runnable { palette.text_dim } else { palette.text_faint }),
        };
        let name = self.single_line(&self.fonts.body, &model.name, text_width)?;
        let note_layout = self.single_line(&self.fonts.small, &note.0, text_width)?;
        let (name_height, note_height) = (height_of(&name)?, height_of(&note_layout)?);
        let top = middle - (name_height + note_height) / 2.0;
        self.draw_layout(&name, text_x, top, if runnable || picked { palette.text } else { palette.text_dim })?;
        self.draw_layout(&note_layout, text_x, top + name_height, note.1)?;
        if let Some(progress) = panel.downloads.progress(target).filter(|p| p.total > 0) {
            self.draw_progress(rect(text_x, row.bottom - 4.0, content.right, row.bottom - 2.0), progress.done as f32 / progress.total as f32)?;
        }
        Ok(y + MODEL_ROW)
    }

    /// "Add a model…" under the models, with what trying the last one found. Returns the y below.
    fn draw_add_model_row(&mut self, content: D2D_RECT_F, y: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        self.begin_row(content, y)?;
        let middle = y + SETTING_ROW / 2.0;
        let (label, detail) = match panel.import {
            ImportStatus::Trying => ("Trying the model in that folder…", ""),
            _ => ("Your own model", "A folder of a sherpa-onnx or Moonshine model"),
        };
        if *panel.import != ImportStatus::Trying {
            self.draw_text_button("Add a model…", Some(Icon::Plus), Action::Import, Emphasis::Secondary, content.right, middle, panel, regions)?;
        }
        let name = self.layout(&self.fonts.body, label, f32::MAX)?;
        let detail = self.layout(&self.fonts.small, detail, f32::MAX)?;
        let top = middle - (height_of(&name)? + height_of(&detail)?) / 2.0;
        self.draw_layout(&name, content.left + 26.0, top, self.palette.text)?;
        self.draw_layout(&detail, content.left + 26.0, top + height_of(&name)?, self.palette.text_dim)?;
        Ok(y + SETTING_ROW)
    }

    /// A name over a detail on the left, and buttons on the right. Returns the y below.
    #[allow(clippy::too_many_arguments)]
    fn draw_two_line_row(&mut self, name: &str, detail: &str, buttons: &[RowButton], content: D2D_RECT_F, y: f32, panel: &Panel, regions: &mut Regions) -> Result<f32> {
        self.begin_row(content, y)?;
        let middle = y + MODEL_ROW / 2.0;
        let mut right = content.right;
        for &(label, icon, action, emphasis) in buttons.iter().rev() {
            right = self.draw_text_button(label, icon, action, emphasis, right, middle, panel, regions)? - 6.0;
        }
        let name = self.single_line(&self.fonts.body, name, right - content.left - 8.0)?;
        let detail = self.single_line(&self.fonts.small, detail, right - content.left - 8.0)?;
        let top = middle - (height_of(&name)? + height_of(&detail)?) / 2.0;
        self.draw_layout(&name, content.left, top, self.palette.text)?;
        self.draw_layout(&detail, content.left, top + height_of(&name)?, self.palette.text_dim)?;
        Ok(y + MODEL_ROW)
    }

    /// A thin bar filled `fraction` of the way, for a download's progress.
    fn draw_progress(&self, track: D2D_RECT_F, fraction: f32) -> Result<()> {
        self.glass(track, height(&track) / 2.0, self.palette.track)?;
        let fill = rect(track.left, track.top, track.left + width(&track) * fraction.clamp(0.0, 1.0), track.bottom);
        self.fill_rounded(fill, height(&track) / 2.0, self.palette.accent)
    }

    /// An input's name and threshold over its slider, whose track shows the live level. Returns the y below.
    fn draw_meter(&mut self, meter: &Meter, content: D2D_RECT_F, y: f32, regions: &mut Regions) -> Result<f32> {
        self.begin_row(content, y)?;
        let name = match meter.input {
            Input::Mic => "Your mic",
            Input::Computer => "Computer audio",
        };
        let name = self.layout(&self.fonts.body, name, width(&content))?;
        let text_top = y + 10.0;
        self.draw_layout(&name, content.left, text_top, self.palette.text)?;
        let value = if meter.threshold_db <= THRESHOLD_MIN_DB {
            "Off · all sound passes".to_owned()
        } else {
            format!("{:.0} dB", meter.threshold_db)
        };
        let value = self.layout(&self.fonts.small, &value, f32::MAX)?;
        let name_height = height_of(&name)?;
        self.draw_layout(&value, content.right - metrics(&value)?.width, text_top + (name_height - height_of(&value)?) / 2.0, self.palette.text_dim)?;

        let top = text_top + name_height + 12.0;
        let track = rect(content.left + KNOB / 2.0, top, content.right - KNOB / 2.0, top + TRACK);
        // Like Discord's: the level is faint up to the knob, where sound is ignored, and lit past it.
        let (level, knob) = (fraction_of(meter.level_db), fraction_of(meter.threshold_db));
        let bands = [(0.0, level.min(knob), with_alpha(self.palette.text_faint, 0.55)), (knob, level, self.palette.accent)];
        self.draw_slider(track, knob, &bands)?;
        // The clickable band is taller than the track so it is easy to grab.
        regions.targets.push((rect(track.left, top - 10.0, track.right, track.bottom + 10.0), Target::Slider(Slider::Threshold(meter.input))));
        Ok(track.bottom + 16.0)
    }

    /// A recessed track coloured in `bands` of (from, to, colour), from 0 to 1 along it, with a knob at `marker`.
    fn draw_slider(&self, track: D2D_RECT_F, marker: f32, bands: &[(f32, f32, D2D1_COLOR_F)]) -> Result<()> {
        let radius = height(&track) / 2.0;
        self.glass(track, radius, self.palette.track)?;
        let at = |fraction: f32| track.left + fraction * width(&track);
        for &(from, to, colour) in bands {
            if at(to) > at(from) + 1.0 {
                self.fill_rounded(rect(at(from), track.top, at(to), track.bottom), radius, colour)?;
            }
        }
        let (x, middle) = (at(marker), (track.top + track.bottom) / 2.0);
        self.draw_knob(rect(x - KNOB / 2.0, middle - KNOB / 2.0, x + KNOB / 2.0, middle + KNOB / 2.0))
    }

    /// A round knob raised off what it sits on.
    fn draw_knob(&self, knob: D2D_RECT_F) -> Result<()> {
        let radius = width(&knob) / 2.0;
        self.fill_rounded(rect(knob.left - 0.5, knob.top + 0.5, knob.right + 0.5, knob.bottom + 1.5), radius + 0.5, self.palette.shadow)?;
        self.glass(knob, radius, self.palette.knob)
    }

    /// The captions key to dictate, a button for every shortcut, and what Kiku costs to run. Locked, it
    /// says how to unlock instead.
    fn draw_footer(&mut self, panel: &Panel, regions: &mut Regions) -> Result<()> {
        let top = panel.height - FOOTER_HEIGHT;
        self.hairline(PADDING, panel.width - PADDING, top)?;
        let middle = top + FOOTER_HEIGHT / 2.0;
        let keys = rect(PADDING - 6.0, middle - BUTTON / 2.0, PADDING - 6.0 + BUTTON, middle + BUTTON / 2.0);
        self.icon_button(keys, Icon::Keyboard, Action::ShowShortcuts, Tone::Faint, panel, regions)?;
        let binds = panel.keybinds;
        let hint = match panel.view.locked {
            true => format!("Locked: clicks pass through  ·  {} unlocks", binds.shortcut(Command::Lock)),
            false => format!("Hold {} to dictate", binds.hold_name()),
        };
        // Clear of the resize grip.
        let mut right = panel.width - 28.0;
        if let Some(usage) = panel.usage {
            let usage = self.layout(&self.fonts.small, &usage_text(usage), f32::MAX)?;
            right -= metrics(&usage)?.width;
            self.draw_layout(&usage, right, middle - height_of(&usage)? / 2.0, self.palette.text_faint)?;
            right -= GAP;
        }
        let hint = self.single_line(&self.fonts.small, &hint, right - keys.right - 2.0)?;
        self.draw_layout(&hint, keys.right + 2.0, middle - height_of(&hint)? / 2.0, self.palette.text_faint)
    }

    /// Dots in a diagonal in the bottom-right corner. The top-left corner resizes too, without dots that
    /// would crowd the title.
    fn draw_resize_grips(&mut self, panel: &Panel, regions: &mut Regions) -> Result<()> {
        let (x, y) = (panel.width - 7.0, panel.height - 7.0);
        for (dx, dy) in [(0.0, 0.0), (4.0, 0.0), (0.0, 4.0), (8.0, 0.0), (4.0, 4.0), (0.0, 8.0)] {
            self.fill_dot(x - dx, y - dy, 1.2, self.palette.grip)?;
        }
        regions.targets.push((rect(0.0, 0.0, RESIZE_GRIP, RESIZE_GRIP), Target::Resize(Corner::TopLeft)));
        let bottom_right = rect(panel.width - RESIZE_GRIP, panel.height - RESIZE_GRIP, panel.width, panel.height);
        regions.targets.push((bottom_right, Target::Resize(Corner::BottomRight)));
        Ok(())
    }

    /// An icon centred in `area`, bare at rest: under the pointer a plate lifts behind it, held down it is
    /// pushed in, and a quiet icon brightens. A button that is on keeps its plate.
    fn icon_button(&mut self, area: D2D_RECT_F, icon: Icon, action: Action, tone: Tone, panel: &Panel, regions: &mut Regions) -> Result<()> {
        let (hovered, pressed) = pointer_on(panel, action);
        let palette = self.palette.clone();
        let plate = inset(area, 1.0);
        let radius = RADIUS_CONTROL.min(height(&plate) / 2.0);
        match (tone, hovered, pressed) {
            (_, _, true) => self.glass(plate, radius, palette.pressed)?,
            (Tone::Close, true, _) => self.glass(plate, radius, palette.danger)?,
            (_, true, _) | (Tone::On, ..) => self.glass(plate, radius, palette.plate)?,
            _ => {}
        }
        let side = (width(&area).min(height(&area)) * 0.6).round().min(ICON);
        let nudge = if pressed { 0.5 } else { 0.0 };
        let (x, y) = ((area.left + area.right - side) / 2.0, (area.top + area.bottom - side) / 2.0 + nudge);
        self.icon(icon, rect(x, y, x + side, y + side), tone.colour(&palette, hovered))?;
        regions.targets.push((area, Target::Click(action)));
        Ok(())
    }

    /// The hovered button's tooltip, once the pointer has rested on it: under the button, or above it,
    /// or beside it when the window is too short for either.
    fn draw_tooltip(&mut self, panel: &Panel, regions: &Regions) -> Result<()> {
        let view = panel.view;
        let Some(action) = view.hovered.filter(|_| view.tip && view.pressed.is_none()) else { return Ok(()) };
        let (Some(tip), Some(anchor)) = (tooltip(action, panel), regions.rect_at(Target::Click(action), view.pointer.0, view.pointer.1)) else {
            return Ok(());
        };
        let tip = self.layout(&self.fonts.small, &tip, 280.0)?;
        let size = metrics(&tip)?;
        let (tip_width, tip_height) = ((size.width + 16.0).ceil(), (size.height + 8.0).max(22.0).ceil());
        let x = ((anchor.left + anchor.right - tip_width) / 2.0).clamp(6.0, (panel.width - 6.0 - tip_width).max(6.0));
        let (x, y) = if anchor.bottom + 6.0 + tip_height <= panel.height - 4.0 {
            (x, anchor.bottom + 6.0)
        } else if anchor.top - 6.0 - tip_height >= 4.0 {
            (x, anchor.top - 6.0 - tip_height)
        } else {
            ((anchor.left - 8.0 - tip_width).max(4.0), (anchor.top + anchor.bottom - tip_height) / 2.0)
        };
        self.popover(rect(x, y, x + tip_width, y + tip_height), 6.0)?;
        self.draw_layout(&tip, x + 8.0, y + (tip_height - size.height) / 2.0, self.palette.text)
    }

    /// The scroll position of `area` as a thin thumb whose right edge is at `right`; none when all of it fits.
    fn draw_scroll_thumb(&self, right: f32, area: D2D_RECT_F, extent: Extent, top: f32) -> Result<()> {
        let overflow = extent.content - extent.viewport;
        if overflow <= 0.5 {
            return Ok(());
        }
        let tall = (extent.viewport * extent.viewport / extent.content).max(24.0).min(height(&area));
        let y = area.top + (top / overflow).clamp(0.0, 1.0) * (height(&area) - tall);
        self.fill_rounded(rect(right - 3.0, y, right, y + tall), 1.5, self.palette.grip)
    }

    /// Where in a caption's text a point falls, as a UTF-16 offset; `x` and `y` are relative to the text's top-left.
    pub fn text_offset(&self, caption: &Caption, width: f32, x: f32, y: f32) -> Result<u32> {
        // Outside a frame there is no surface's cache to use, and a cache kept here would never be pruned.
        let layout = self.new_layout(self.caption_format(caption), &caption.display_text(), width.max(1.0), false)?;
        let (mut trailing, mut inside, mut hit) = (Default::default(), Default::default(), DWRITE_HIT_TEST_METRICS::default());
        unsafe { layout.HitTestPoint(x, y, &mut trailing, &mut inside, &mut hit)? };
        Ok(hit.textPosition + if trailing.as_bool() { hit.length } else { 0 })
    }

    /// Highlights the characters `start..end` of laid-out text drawn at `x`, `y` in `colour`.
    fn fill_text_range(&self, layout: &IDWriteTextLayout, start: u32, end: u32, x: f32, y: f32, colour: D2D1_COLOR_F) -> Result<()> {
        if end <= start {
            return Ok(());
        }
        let mut count = 0;
        // The first call only reports how many rectangles the range needs.
        let _ = unsafe { layout.HitTestTextRange(start, end - start, x, y, None, &mut count) };
        let mut boxes = vec![DWRITE_HIT_TEST_METRICS::default(); count as usize];
        unsafe { layout.HitTestTextRange(start, end - start, x, y, Some(&mut boxes), &mut count)? };
        for b in &boxes {
            self.fill_rounded(rect(b.left, b.top, b.left + b.width, b.top + b.height), 2.0, colour)?;
        }
        Ok(())
    }

    fn caption_height(&self, caption: &Caption, width: f32) -> Result<f32> {
        height_of(&self.caption_layout(caption, width)?)
    }

    /// A caption laid out as shown: "You: …" in italics when it is yours.
    fn caption_layout(&self, caption: &Caption, width: f32) -> Result<IDWriteTextLayout> {
        self.layout(self.caption_format(caption), &caption.display_text(), width)
    }

    fn caption_format(&self, caption: &Caption) -> &IDWriteTextFormat {
        if caption.speaker.is_some() && self.your_lines.italic() { &self.fonts.your_caption } else { &self.fonts.caption }
    }

    /// Behind one of your lines in an app's section: a wash of the accent with a bar at its left edge.
    fn draw_your_highlight(&self, left: f32, y: f32, text_width: f32, line_height: f32) -> Result<()> {
        let band = rect(left - HIGHLIGHT_INSET, y - 2.0, left + text_width + 6.0, y + line_height + 2.0);
        self.fill_rounded(band, 6.0, self.palette.yours)?;
        self.fill_rounded(rect(band.left, band.top, band.left + 3.0, band.bottom), 1.5, self.palette.accent)
    }

    /// One line of text, cut with "…" if it doesn't fit in `max_width`.
    fn single_line(&self, format: &IDWriteTextFormat, text: &str, max_width: f32) -> Result<IDWriteTextLayout> {
        self.cached_layout(format, text, max_width, true)
    }

    fn layout(&self, format: &IDWriteTextFormat, text: &str, max_width: f32) -> Result<IDWriteTextLayout> {
        self.cached_layout(format, text, max_width, false)
    }

    /// Lays out `text`, or reuses the layout of the same text, format and width from the last frame.
    fn cached_layout(&self, format: &IDWriteTextFormat, text: &str, max_width: f32, single_line: bool) -> Result<IDWriteTextLayout> {
        let max_width = max_width.max(1.0);
        let key = LayoutKey { format: format.as_raw() as usize, width: max_width.to_bits(), single_line, text: text.to_owned() };
        if let Some(layout) = self.layouts.borrow_mut().get(&key) {
            return Ok(layout);
        }
        let layout = self.new_layout(format, text, max_width, single_line)?;
        self.layouts.borrow_mut().current.insert(key, layout.clone());
        Ok(layout)
    }

    fn new_layout(&self, format: &IDWriteTextFormat, text: &str, max_width: f32, single_line: bool) -> Result<IDWriteTextLayout> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let layout = unsafe { self.dwrite.CreateTextLayout(&wide, format, max_width, f32::MAX)? };
        if single_line {
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            unsafe {
                layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                layout.SetTrimming(&trimming, &self.ellipsis(format)?)?;
            }
        }
        Ok(layout)
    }

    fn ellipsis(&self, format: &IDWriteTextFormat) -> Result<IDWriteInlineObject> {
        let key = format.as_raw() as usize;
        if let Some(sign) = self.ellipses.borrow().get(&key) {
            return Ok(sign.clone());
        }
        let sign = unsafe { self.dwrite.CreateEllipsisTrimmingSign(format)? };
        self.ellipses.borrow_mut().insert(key, sign.clone());
        Ok(sign)
    }

    fn draw_layout(&self, layout: &IDWriteTextLayout, x: f32, y: f32, color: D2D1_COLOR_F) -> Result<()> {
        self.scene.borrow_mut().text(layout, x, y, color)
    }

    pub(super) fn fill_rounded(&self, area: D2D_RECT_F, radius: f32, color: D2D1_COLOR_F) -> Result<()> {
        self.scene.borrow_mut().fill_rounded(area, radius, color);
        Ok(())
    }

    pub(super) fn icon(&self, icon: Icon, area: D2D_RECT_F, color: D2D1_COLOR_F) -> Result<()> {
        self.scene.borrow_mut().icon(icon, area, color);
        Ok(())
    }

    fn fill_dot(&self, x: f32, y: f32, radius: f32, color: D2D1_COLOR_F) -> Result<()> {
        self.scene.borrow_mut().fill_dot(x, y, radius, color);
        Ok(())
    }

    fn glass(&self, area: D2D_RECT_F, radius: f32, glass: Glass) -> Result<()> {
        self.scene.borrow_mut().glass(area, radius, glass);
        Ok(())
    }

    /// A one-DIP line across from `left` to `right` at `y`, setting rows or groups apart.
    fn hairline(&self, left: f32, right: f32, y: f32) -> Result<()> {
        self.fill_rounded(rect(left, y, right, y + 1.0), 0.0, self.palette.divider)
    }

    /// A surface floating over others, with a soft shadow under it.
    fn popover(&self, area: D2D_RECT_F, radius: f32) -> Result<()> {
        self.shadow(area, radius, &POPOVER_SHADOW)?;
        self.glass(area, radius, self.palette.popover)
    }

    /// A shadow under `area` in feathered `layers` of (spread, drop, strength).
    fn shadow(&self, area: D2D_RECT_F, radius: f32, layers: &[(f32, f32, f32)]) -> Result<()> {
        let shadow = self.palette.shadow;
        for &(spread, drop, strength) in layers {
            let shade = rect(area.left - spread, area.top - spread + drop, area.right + spread, area.bottom + spread + drop);
            self.fill_rounded(shade, radius + spread, with_alpha(shadow, shadow.a * strength))?;
        }
        Ok(())
    }

    /// Fades what is drawn in `area` of a window `window_height` tall into the window's background at
    /// `edge`, its top or bottom: solid there, see-through at the other side.
    fn fade(&self, area: D2D_RECT_F, edge: Edge, window_height: f32) -> Result<()> {
        // Background drawn over a see-through background would show as a band.
        if self.see_through {
            return Ok(());
        }
        let (solid, clear) = match edge {
            Edge::Top => (background_at(&self.palette, area.top, window_height), background_at(&self.palette, area.bottom, window_height)),
            Edge::Bottom => (background_at(&self.palette, area.bottom, window_height), background_at(&self.palette, area.top, window_height)),
        };
        let clear = with_alpha(clear, 0.0);
        self.glass(area, 0.0, if edge == Edge::Top { gradient(solid, clear) } else { gradient(clear, solid) })
    }

    fn push_clip(&self, area: D2D_RECT_F) {
        self.scene.borrow_mut().push_clip(area);
    }

    fn pop_clip(&self) {
        self.scene.borrow_mut().pop_clip();
    }
}

/// Gives each area its content height when it fits, splitting the rest evenly among
/// the areas that need more. Returns the heights in the same order.
fn share_space(contents: &[f32], available: f32) -> Vec<f32> {
    let mut order: Vec<usize> = (0..contents.len()).collect();
    order.sort_by(|&a, &b| contents[a].total_cmp(&contents[b]));
    let mut shares = vec![0.0; contents.len()];
    let mut remaining = available;
    for (handled, &index) in order.iter().enumerate() {
        let fair = remaining / (contents.len() - handled) as f32;
        shares[index] = contents[index].min(fair);
        remaining -= shares[index];
    }
    // Space nobody needs goes to the last area, so the panel stays filled.
    if let Some(last) = shares.last_mut() {
        *last += remaining;
    }
    shares
}

/// What the title bar says after the title.
/// "24 GB · driver 610.62", and whether the default model's GPU build fits.
fn gpu_detail(gpu: &Gpu) -> String {
    let unified = catalog::by_id_or_default(catalog::DEFAULT_ID);
    let fits = match unified.variants.iter().find(|v| v.device == Device::Cuda) {
        Some(variant) if !variant.fits(gpu.vram_mb) => format!("  ·  {} needs {} GB on the GPU, so it runs on the CPU", unified.name, gb(variant.gpu_memory_needed_mb())),
        _ => String::new(),
    };
    format!("{} GB  ·  driver {}{fits}", gb(gpu.vram_mb), gpu.driver)
}

/// "24 threads · 32 GB of memory · GeForce RTX 3090, 24 GB".
fn describe_pc(hardware: &Hardware) -> String {
    let gpu = match (&hardware.gpu, hardware.gpu_problem()) {
        (Some(gpu), None) => format!("  ·  {}, {} GB", gpu.name, (gpu.vram_mb as f32 / 1024.0).round()),
        (_, Some(problem)) => format!("  ·  {problem}"),
        (None, None) => String::new(),
    };
    format!("{} threads  ·  {} GB of memory{gpu}", hardware.threads, (hardware.ram_mb as f32 / 1024.0).round())
}

/// A button in a row: its label, icon, what it does and how it stands out.
type RowButton = (&'static str, Option<Icon>, Action, Emphasis);

/// What a download's row offers: Download; Pause or Cancel while it runs; Resume, Retry and Cancel when
/// it stopped; and once installed Delete, which a second press confirms, or Remove for an imported model.
fn download_buttons(target: Download, installed: bool, panel: &Panel) -> Vec<RowButton> {
    let cancel = ("Cancel", None, Action::CancelDownload(target), Emphasis::Ghost);
    match panel.downloads.progress(target).map(|progress| progress.state) {
        Some(State::Running) => vec![("Pause", None, Action::PauseDownload(target), Emphasis::Secondary), cancel],
        Some(State::Queued) => vec![cancel],
        Some(State::Paused) => vec![("Resume", Some(Icon::Download), Action::Download(target), Emphasis::Secondary), cancel],
        Some(State::Failed(_)) => vec![("Retry", Some(Icon::Reset), Action::Download(target), Emphasis::Secondary), cancel],
        None if !installed => vec![("Download", Some(Icon::Download), Action::Download(target), Emphasis::Secondary)],
        None => match target {
            Download::Model(model, _) if model.imported() => vec![("Remove", None, Action::Delete(target), Emphasis::Ghost)],
            _ if panel.view.confirm_delete == Some(target) => vec![("Delete its files", Some(Icon::Trash), Action::Delete(target), Emphasis::Danger)],
            // A pack whose files were damaged is fixed by downloading it again.
            Download::Component(_) => vec![("Download again", Some(Icon::Reset), Action::Download(target), Emphasis::Ghost), ("Delete", Some(Icon::Trash), Action::Delete(target), Emphasis::Ghost)],
            Download::Model(..) => vec![("Delete", Some(Icon::Trash), Action::Delete(target), Emphasis::Ghost)],
        },
    }
}

/// What a model row says by its buttons: its download's progress, Loading, In use, or what downloading
/// it costs.
fn model_status(model: &Model, target: Download, in_use: bool, panel: &Panel, palette: &Palette) -> (String, D2D1_COLOR_F) {
    if let Some(progress) = panel.downloads.progress(target).filter(|p| !matches!(p.state, State::Failed(_))) {
        return (progress_text(&progress), palette.text_dim);
    }
    match () {
        _ if panel.model.pending.is_some_and(|c| c.model == model && download::runnable(c)) => ("Loading…".into(), palette.text_dim),
        _ if in_use => ("In use".into(), palette.accent),
        _ if !target.complete() => (megabytes(target.size()), palette.text_dim),
        _ => (String::new(), palette.text_dim),
    }
}

/// "English · 2.1% word errors": what a model row says about the model on `device`.
fn model_note(model: &Model, device: Device) -> String {
    let languages = match model.languages.as_slice() {
        [] => format!("Your own: {}", model.backend.description()),
        [only] if only == "en" => "English".to_owned(),
        all => format!("{} languages", all.len()),
    };
    let errors = model.wer_percent.map(|wer| format!("  ·  {wer:.1}% word errors")).unwrap_or_default();
    let precision = &model.variant(device).precision;
    let place = match device {
        Device::Cuda => format!("  ·  GPU, {precision}"),
        Device::Cpu if precision.is_empty() => String::new(),
        Device::Cpu => format!("  ·  {precision}"),
    };
    format!("{languages}{errors}{place}")
}

/// "640 MB", or "5.2 GB" from a gigabyte up.
fn megabytes(bytes: u64) -> String {
    match bytes as f64 / 1e6 {
        mb if mb >= 1000.0 => format!("{:.1} GB", mb / 1000.0),
        mb => format!("{} MB", mb.round()),
    }
}

/// "412/640 MB · 8 MB/s", "Paused at 1.2/5.2 GB", "Waiting".
fn progress_text(progress: &Progress) -> String {
    let (done, total) = match progress.total as f64 / 1e6 {
        mb if mb >= 1000.0 => (format!("{:.1}", progress.done as f64 / 1e9), format!("{:.1} GB", mb / 1000.0)),
        mb => (format!("{:.0}", progress.done as f64 / 1e6), format!("{mb:.0} MB")),
    };
    match &progress.state {
        State::Running => format!("{done}/{total}  ·  {:.0} MB/s", progress.bytes_per_second / 1e6),
        State::Queued => "Waiting".into(),
        State::Paused => format!("Paused at {done}/{total}"),
        State::Failed(_) => "Failed".into(),
    }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// What a prompt draws between its detail and its answers: given where, in a row of the height it asked for.
type PromptExtra<'a> = dyn Fn(&mut Renderer, f32, f32, &mut Regions) -> Result<()> + 'a;

struct Status {
    text: String,
    color: D2D1_COLOR_F,
    /// The state's colour, as a dot before the text; none for notices.
    dot: Option<D2D1_COLOR_F>,
    /// Clicking it stops or starts captioning.
    clickable: bool,
}

/// What the title bar says, in priority order. Collapsed, the newest line stands in for "Listening".
fn status_text(panel: &Panel, palette: &Palette) -> Status {
    let state = |text: String, dot, clickable| Status { text, color: palette.text_dim, dot: Some(dot), clickable };
    if let Some((text, color)) = notice(panel, palette) {
        return Status { text, color, dot: None, clickable: false };
    }
    // A pick that isn't downloaded waits rather than loads.
    if let Some(pending) = panel.model.pending.filter(|&choice| download::runnable(choice)) {
        return state(format!("Loading {}…", pending.model.name), palette.text_faint, false);
    }
    if let Some(failed) = panel.model.failed {
        let why = panel.model.failure.as_deref().unwrap_or("open settings");
        return Status { color: palette.error, ..state(format!("{} failed to load: {why}", failed.model.name), palette.error, false) };
    }
    if !panel.model.loaded && !panel.stopped {
        let wanted = panel.model.wanted();
        if let Some(progress) = panel.downloads.progress(Download::Model(wanted.model, wanted.variant())) {
            return state(format!("Downloading {}: {}", wanted.model.name, progress_text(&progress)), palette.text_faint, false);
        }
        // At start, or when the pick can't run, an installed model is on its way in.
        return match Some(wanted).filter(|&choice| download::runnable(choice)).or_else(download::fallback) {
            Some(choice) => state(format!("Loading {}…", choice.model.name), palette.text_faint, false),
            None => Status { color: palette.warn, ..state("No model yet: download one in Settings, Listening".into(), palette.warn, false) },
        };
    }
    if panel.dictating {
        return state(format!("Dictating: let go of {} to finish", panel.keybinds.hold_name()), palette.error, false);
    }
    if panel.stopped {
        return state("Stopped".into(), palette.warn, true);
    }
    let newest = || panel.captions.visible().filter_map(|(_, section)| section.captions.back()).max_by_key(|caption| caption.time);
    match newest().filter(|_| panel.view.collapsed) {
        Some(caption) => state(caption.display_text(), palette.accent, true),
        None => match &panel.model.remote {
            Some(address) => state(format!("Listening on {address}"), palette.accent, true),
            None => state("Listening".into(), palette.accent, true),
        },
    }
}

/// The notice showing, with its colour.
fn notice(panel: &Panel, palette: &Palette) -> Option<(String, D2D1_COLOR_F)> {
    let notice = panel.view.notice.as_ref()?;
    Some((notice.text.clone(), if notice.error { palette.error } else { palette.accent }))
}

/// The title bar's buttons: the app's, then the window's.
fn title_buttons(panel: &Panel) -> [Vec<(Icon, Action, Tone)>; 2] {
    let view = panel.view;
    let on = |showing: bool| if showing { Tone::On } else { Tone::Dim };
    let mut app = vec![
        (Icon::Power, Action::ToggleStop, if panel.stopped { Tone::Accent } else { Tone::Dim }),
        (Icon::Captions, Action::ShowSubtitles(true), Tone::Dim),
        (Icon::Sparkles, Action::ShareAll, Tone::Accent),
        (Icon::History, Action::ShowSaved, on(!matches!(view.screen, Screen::Live))),
        (Icon::Settings, Action::ShowSettings, on(panel.settings_open)),
    ];
    // Saving by hand is only offered in manual save mode.
    if !panel.auto_save {
        app.push((Icon::Save, Action::SaveAll, Tone::Dim));
    }
    let window = vec![
        (if view.locked { Icon::Lock } else { Icon::Unlock }, Action::ToggleLock, on(view.locked)),
        (if view.collapsed { Icon::ChevronDown } else { Icon::Minimize }, Action::Minimize, Tone::Dim),
        (Icon::Close, Action::Close, Tone::Close),
    ];
    [app, window]
}

fn tab_icon(tab: Tab) -> Icon {
    match tab {
        Tab::General => Icon::Sliders,
        Tab::Listening => Icon::Mic,
        Tab::Apps => Icon::Apps,
        Tab::Words => Icon::SpellCheck,
        Tab::Appearance => Icon::Palette,
        Tab::Shortcuts => Icon::Keyboard,
    }
}

fn tooltip(action: Action, panel: &Panel) -> Option<String> {
    let (view, binds) = (panel.view, panel.keybinds);
    let with_shortcut = |text: &str, command| format!("{text} ({})", binds.shortcut(command));
    Some(match action {
        Action::ShowSettings if panel.settings_open => "Close settings".into(),
        Action::ShowSettings => "Settings".into(),
        Action::ShowShortcuts => "Every shortcut".into(),
        Action::CloseSettings => "Close settings".into(),
        Action::ShowSaved if matches!(view.screen, Screen::SavedFile { .. }) => "Back to History".into(),
        Action::ShowSaved => "History: search what was said, and past sessions".into(),
        Action::ShowLive => "Back to the captions".into(),
        Action::SaveAll => "Save every app's transcript to History".into(),
        Action::SaveSection(_) => "Save this app's transcript to History".into(),
        Action::CopySection(_) => "Copy this app's transcript (or drag over text to copy just that)".into(),
        Action::ToggleFold(index) => {
            let section = &panel.captions.sections[index];
            match view.folds.folded(index, section, auto_fold_after(panel.auto_fold), Instant::now()) {
                true => "Open this app (double-click its name to show it alone)".into(),
                false => "Fold this app to one row (double-click its name to show it alone)".into(),
            }
        }
        Action::Hide(_) => "Hide this app from the panel; it is still captioned and saved".into(),
        Action::ShowHidden => "Show the hidden apps again".into(),
        Action::AutoFold(0) => "Never fold apps by themselves".into(),
        Action::AutoFold(minutes) => format!("Fold an app to one row once it has been quiet {minutes} min"),
        Action::Block(_) => "Block: stop capturing and captioning this app".into(),
        Action::BlockFor(false) => "Block it until Kiku quits".into(),
        Action::BlockFor(true) => "Block it now and whenever Kiku starts".into(),
        Action::CancelBlock => "Keep captioning it".into(),
        Action::DontAskToBlock => "Use this answer for every app from now on; Settings > Apps can ask again".into(),
        Action::Unblock(_) => "Capture and caption this app again".into(),
        Action::Pin(index) if panel.captions.is_pinned(index) => "Unpin: your lines go to the app you are in again".into(),
        Action::Pin(_) => "Pin: put every line you say in this app, whichever window is in front".into(),
        Action::Unpin(_) => "Your lines go to the app you are in again".into(),
        Action::SetBlockAnswer(BlockAnswer::Ask) => "Ask each time an app is blocked".into(),
        Action::SetBlockAnswer(BlockAnswer::Session) => "Block apps until Kiku quits, without asking".into(),
        Action::SetBlockAnswer(BlockAnswer::Always) => "Block apps for good, without asking".into(),
        Action::SetYourLines(YourLines::Italic) => "Your lines in an app are in italics".into(),
        Action::SetYourLines(YourLines::Highlight) => "Your lines in an app sit on a highlight with a bar beside them".into(),
        Action::SetYourLines(YourLines::Both) => "Your lines in an app are in italics, on a highlight".into(),
        Action::WordScope { transforms, scope } => {
            let what = if transforms { "Transforms change" } else { "Word rules change" };
            match scope {
                Scope::Captions => format!("{what} captions only, not what dictation types"),
                Scope::Dictation => format!("{what} only what dictation types or pastes"),
                Scope::Both => format!("{what} captions and what dictation types"),
            }
        }
        Action::WordHotwords(true) => "Help Parakeet Unified hear the @ words; it decodes a little slower while there are any".into(),
        Action::WordHotwords(false) => "Decode as if there were no @ words".into(),
        Action::ResetWordStrength(_) => "Listen for it as strongly as the other @ words again".into(),
        Action::RemoveWordEntry(_) => "Take it out of the file".into(),
        Action::OpenWords => "Open words.txt in your text editor".into(),
        Action::CopyWordInstructions => "Copy how the file works for ChatGPT, Claude or another assistant, then add what you want".into(),
        Action::ClearWord(_) => "Clear".into(),
        Action::CorrectLine(_) => "Correct a misheard word in this line: make it a rule or a hotword".into(),
        Action::CorrectSelection => "Correct the selected words: make them a rule or a hotword".into(),
        Action::CorrectOnlyApp(false) => "The rule applies in every app".into(),
        Action::CorrectOnlyApp(true) => "The rule applies only where the line came from".into(),
        Action::AddRule => "Swap the words whenever the model writes them".into(),
        Action::AddHotword => "Have the model listen for the right word".into(),
        Action::CopyLine(_) => "Copy this line".into(),
        Action::ScrollToLatest(_) => "Back to the newest line, and follow it again".into(),
        Action::ShareLine(_) => "Ask Claude about this line".into(),
        Action::ShareSection(_) => "Ask Claude about this app's transcript".into(),
        Action::ShareAll => "Ask Claude about everything (all apps and you)".into(),
        Action::ShareSelection => "Open a Claude chat with the selected text".into(),
        Action::ToggleLock => with_shortcut("Lock: clicks pass through", Command::Lock),
        Action::ToggleStop if panel.stopped => with_shortcut("Start captioning", Command::Stop),
        Action::ToggleStop => with_shortcut("Stop captioning and free the model", Command::Stop),
        Action::Minimize if view.has_tray => with_shortcut("Hide to the tray", Command::Hide),
        Action::Minimize if view.collapsed => "Expand".into(),
        Action::Minimize => "Collapse to the title bar".into(),
        Action::Close => with_shortcut("Quit", Command::Quit),
        Action::PickDevice(Device::Cuda) => "Run the model on the GPU (CUDA)".into(),
        Action::PickDevice(Device::Cpu) => "Run the model on the CPU".into(),
        Action::PickModel(model) => match &model.licence {
            Some(licence) => format!("Switch to {}  ·  {}", model.name, licence.attribution),
            None => format!("Switch to {}", model.name),
        },
        Action::Download(target) => match panel.downloads.progress(target) {
            Some(Progress { state: State::Failed(why), .. }) => format!("Download failed: {why}. Try again"),
            _ => "Download; captions go on with the current model meanwhile".into(),
        },
        Action::PauseDownload(_) => "Pause; it resumes where it stopped".into(),
        Action::CancelDownload(_) => "Stop and delete what was downloaded".into(),
        Action::Delete(Download::Model(model, _)) if model.imported() => "Forget this model; its folder stays as it is".into(),
        Action::Delete(target) if panel.view.confirm_delete == Some(target) => "Press again to delete; it can be downloaded again".into(),
        Action::Delete(_) => "Delete its files to free space".into(),
        Action::Import => "Pick a folder with a sherpa-onnx or Moonshine model; Kiku tries it on a short clip first".into(),
        Action::OpenSetup => "Walk through the first-run setup again".into(),
        Action::UseRecommended => "Use the model recommended for this PC".into(),
        Action::SetupNext | Action::SetupBack | Action::SkipSetup | Action::QuitAndPause | Action::KeepDownloading => return None,
        Action::InstallUpdate | Action::UpdateLater => return None,
        Action::SetEngineWhere(EngineWhere::ThisPc) => "Run models on this PC; nothing is sent anywhere".into(),
        Action::SetEngineWhere(EngineWhere::Host) => "Run models on another computer that shares its models; your audio is sent to it".into(),
        Action::ShareModels(true) => "Let other computers caption with this PC's models, using your code".into(),
        Action::ShareModels(false) => "Stop sharing this PC's models".into(),
        Action::CopyHostCode => "Copy the whole code, to paste on the other computer".into(),
        Action::NewHostCode if panel.view.confirm_new_code => "Press again: the old code stops working".into(),
        Action::NewHostCode => "Make a new code; the old one stops working".into(),
        Action::DictationOnly(false) => "Caption apps and your mic, and dictate while the key is held".into(),
        Action::DictationOnly(true) => "Only dictate: no captions, and the mic heard only while the key is held".into(),
        Action::CheckUpdatesNow => "Look for a newer Kiku now".into(),
        Action::CheckUpdates(true) => "Look once a day whether a newer Kiku is out".into(),
        Action::CheckUpdates(false) => "Never look for a newer Kiku".into(),
        Action::Rebind(_) => "Click, then press the new keys (Esc cancels)".into(),
        Action::SetAutoSave(true) => "Save every finished line as it's said".into(),
        Action::SetAutoSave(false) => "Save only when you press a save button".into(),
        Action::SaveMicOnly(true) => "Save only what you say, not what apps say".into(),
        Action::SaveMicOnly(false) => "Save what every app says, and you".into(),
        Action::KeepDays(0) => "Keep saved transcripts forever".into(),
        Action::KeepDays(days) => format!("Delete saved lines older than {days} day{}", if days == 1 { "" } else { "s" }),
        Action::ResetKeys => format!("Back to {} with V, Z, S, C, H, Q, B and T", Keybinds::default().hold_name()),
        Action::ShowSubtitles(true) => with_shortcut("Show captions like movie subtitles instead of this panel", Command::Subtitles),
        Action::ShowSubtitles(false) => "Show this panel".into(),
        Action::SubtitleRows(rows) => format!("Show at most {rows} row{} of captions", if rows == 1 { "" } else { "s" }),
        Action::SetTheme(Theme::Custom) if panel.themes.custom().is_none() => {
            format!("Make your own theme from {}'s colours", panel.themes.chosen().name())
        }
        Action::SetTheme(Theme::Custom) => "Your own theme, for the panel, settings and captions; changing a colour edits it".into(),
        Action::SetTheme(theme) => format!("The {} theme, for the panel, settings and captions", theme.name().to_lowercase()),
        Action::SubtitleAutoHide(true) => "Fade the captions out after a few quiet seconds".into(),
        Action::SubtitleAutoHide(false) => "Keep the last captions up until something new is said".into(),
        Action::ToggleDropdown(Dropdown::Colour(paint)) => {
            let what = match paint {
                Paint::Background => "The windows' background",
                Paint::Accent => "The accent: live, links, the chosen setting",
                Paint::Pill => "Captions mode's pills",
                Paint::Text => "Captions mode's text",
            };
            match panel.themes.chosen() {
                Theme::Custom => what.into(),
                _ => format!("{what}; changing it makes your Custom theme"),
            }
        }
        Action::SetColour(..) => "Use this colour".into(),
        Action::MoreColours(_) => "Any colour, with Windows' colour picker".into(),
        Action::ToggleDropdown(Dropdown::Font) => "The captions' font: designed picks first, then every installed font, which typing searches".into(),
        Action::SubtitleSize(size) => format!("Text {size} px at 100% scale"),
        Action::DeleteSession(index) if view.saved.get(index).map(|s| s.id) == view.finder.deleting => "Press again to delete it for good".into(),
        Action::DeleteSession(_) => "Delete this session's saved lines".into(),
        Action::ShowSql(false) => "Search what was said, by words, who and when".into(),
        Action::ShowSql(true) => "Ask your transcripts anything, in SQL".into(),
        Action::ClearSearch | Action::ClearFontSearch => "Clear the search".into(),
        Action::SetPeriod(Period::Today) => "Only what was said today".into(),
        Action::SetPeriod(Period::Week) => "Only the last 7 days".into(),
        Action::SetPeriod(Period::All) => "Any time".into(),
        Action::ToggleDropdown(Dropdown::Who) => "Whose lines: everyone, you, an app, or an app with your replies".into(),
        Action::CopyResult(_) => "Copy this line".into(),
        Action::ShareResult(_) => "Ask Claude about this line".into(),
        Action::CopyResults if view.finder.sql_mode => "Copy the rows, tab-separated".into(),
        Action::CopyResults => "Copy every line found".into(),
        Action::ShareResults if view.finder.sql_mode => "Ask Claude about these rows".into(),
        Action::ShareResults => "Ask Claude about every line found".into(),
        Action::RunSql => "Run the query (Enter)".into(),
        Action::Example(index) => EXAMPLES[index].1.into(),
        Action::OpenSaved(_) | Action::OpenResult(_) | Action::Type(_) | Action::TypeWord(_) | Action::SetWho(_) | Action::SubtitleFont(_) | Action::ShowTab(_) => return None,
    })
}

/// E.g. "CPU 0.4%  ·  GPU 2%  ·  2.1 GB"; the GPU part only once the app uses the GPU.
pub fn usage_text(usage: Usage) -> String {
    let cpu = format!("CPU {:.1}%", usage.cpu_percent);
    match usage.gpu_memory_bytes {
        0 => cpu,
        memory => format!("{cpu}  ·  GPU {:.0}%  ·  {:.1} GB", usage.gpu_percent, memory as f64 / 1e9),
    }
}

/// The font list's position when it opens, with font `row` in the middle of the whole rows shown.
pub fn font_list_scroll(row: usize) -> Scroll {
    Scroll::at(LIST_ROW * row.saturating_sub((DROPDOWN_ROWS - 1) / 2) as f32)
}

/// Where a list `tall` opens by its box `anchor`: under it, or over it when that would run past `bottom`.
fn list_top(anchor: D2D_RECT_F, tall: f32, bottom: f32) -> f32 {
    if anchor.bottom + 4.0 + tall <= bottom { anchor.bottom + 4.0 } else { anchor.top - 4.0 - tall }
}

/// Where a settings heading at `y` puts a control on its right: its right edge, in line with the rows of
/// the card under it, and its middle, level with the heading's capitals.
fn heading_control(content: D2D_RECT_F, y: f32) -> (f32, f32) {
    (content.right - GAP, y + 7.0)
}

/// Whether the pointer is on `action`'s control, and whether the mouse went down on it there.
fn pointer_on(panel: &Panel, action: Action) -> (bool, bool) {
    let hovered = panel.view.hovered == Some(action);
    (hovered, hovered && panel.view.pressed == Some(action))
}

fn inset(area: D2D_RECT_F, by: f32) -> D2D_RECT_F {
    rect(area.left + by, area.top + by, area.right - by, area.bottom - by)
}

fn expand(area: D2D_RECT_F, by: f32) -> D2D_RECT_F {
    inset(area, -by)
}

fn centre(area: D2D_RECT_F) -> (f32, f32) {
    ((area.left + area.right) / 2.0, (area.top + area.bottom) / 2.0)
}

fn contains(area: &D2D_RECT_F, (x, y): (f32, f32)) -> bool {
    x >= area.left && x < area.right && y >= area.top && y < area.bottom
}

/// Where `db` sits along a slider, from 0 (minimum) to 1 (maximum).
fn fraction_of(db: f32) -> f32 {
    ((db - THRESHOLD_MIN_DB) / (THRESHOLD_MAX_DB - THRESHOLD_MIN_DB)).clamp(0.0, 1.0)
}

/// A row's height before it is laid out: Segoe UI averages about half its size per character, and a
/// line is about 1.33 times its size.
fn estimated_height(text_len: usize, font_size: f32, width: f32) -> f32 {
    let lines = (text_len as f32 * font_size * 0.5 / width.max(1.0)).ceil().max(1.0);
    lines * font_size * 1.33
}

/// The height of each line of laid-out text.
fn line_heights(layout: &IDWriteTextLayout) -> Result<Vec<f32>> {
    let mut count = 0;
    // The first call only reports how many lines there are.
    let _ = unsafe { layout.GetLineMetrics(None, &mut count) };
    let mut lines = vec![DWRITE_LINE_METRICS::default(); count as usize];
    unsafe { layout.GetLineMetrics(Some(&mut lines), &mut count)? };
    Ok(lines.iter().map(|line| line.height).collect())
}

/// Finished lines are bright; lines still being transcribed are dimmed.
fn caption_color(caption: &Caption, palette: &Palette) -> D2D1_COLOR_F {
    if caption.complete { palette.text } else { palette.text_dim }
}

/// Which edge of a scroll area a fade is solid at.
#[derive(Clone, Copy, PartialEq)]
enum Edge {
    Top,
    Bottom,
}

/// What an icon button's colour says. Under the pointer the quiet ones brighten to full text; one that
/// is on keeps its plate, and the window's close button turns red.
#[derive(Clone, Copy)]
enum Tone {
    Dim,
    Faint,
    Accent,
    On,
    Close,
}

impl Tone {
    fn colour(self, palette: &Palette, hovered: bool) -> D2D1_COLOR_F {
        match self {
            Tone::Close if hovered => palette.on_accent,
            Tone::Dim | Tone::Faint | Tone::Close if hovered => palette.text,
            Tone::Dim | Tone::Close => palette.text_dim,
            Tone::Faint => palette.text_faint,
            Tone::Accent | Tone::On => palette.accent,
        }
    }
}

/// How much a text button stands out: the main action, another choice, a quiet way out, or one that
/// can't be undone.
#[derive(Clone, Copy)]
enum Emphasis {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

fn height_of(layout: &IDWriteTextLayout) -> Result<f32> {
    Ok(metrics(layout)?.height)
}

fn metrics(layout: &IDWriteTextLayout) -> Result<DWRITE_TEXT_METRICS> {
    let mut metrics = DWRITE_TEXT_METRICS::default();
    unsafe { layout.GetMetrics(&mut metrics)? };
    Ok(metrics)
}

/// Segoe UI Variable's text and display cuts where Windows has them (Windows 11), else Segoe UI for both.
fn ui_families(dwrite: &IDWriteFactory) -> Result<(&'static str, &'static str)> {
    let mut collection = None;
    unsafe { dwrite.GetSystemFontCollection(&mut collection, false)? };
    let (mut index, mut exists) = (0, BOOL(0));
    if let Some(collection) = collection {
        unsafe { collection.FindFamilyName(&HSTRING::from("Segoe UI Variable Text"), &mut index, &mut exists)? };
    }
    Ok(if exists.as_bool() { ("Segoe UI Variable Text", "Segoe UI Variable Display") } else { ("Segoe UI", "Segoe UI") })
}

fn text_format(dwrite: &IDWriteFactory, family: &str, size: f32, weight: DWRITE_FONT_WEIGHT, style: DWRITE_FONT_STYLE) -> Result<IDWriteTextFormat> {
    unsafe {
        dwrite.CreateTextFormat(
            &HSTRING::from(family),
            None,
            weight,
            style,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            &HSTRING::from("en-us"),
        )
    }
}

fn render_target_properties() -> D2D1_RENDER_TARGET_PROPERTIES {
    D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 0.0,
        dpiY: 0.0,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    }
}

fn blank_frame(width: i32, height: i32) -> Result<Frame> {
    let header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height, // top-down rows
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        ..Default::default()
    };
    let info = BITMAPINFO { bmiHeader: header, ..Default::default() };
    unsafe {
        let dc = CreateCompatibleDC(None);
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let previous = SelectObject(dc, bitmap.into());
        Ok(Frame { dc, width, height, bitmap, previous, dirty: None, alpha: u8::MAX })
    }
}

/// The pixels of the frame `whole` that `area`, in DIPs from `origin`, touches; `None` if it misses the frame.
/// The top-left corner falls on a whole DIP, so drawing there puts text on the same pixels as drawing
/// the whole frame does: baselines snap to pixels, and an offset like 221.33 DIPs could tip one over.
fn pixels_of(area: D2D_RECT_F, dpi: f32, origin: f32, whole: RECT) -> Option<RECT> {
    let scale = dpi / 96.0;
    // The fewest pixels that make whole DIPs, e.g. 3 at 144 DPI (3 pixels = 2 DIPs).
    let step = dpi as i32 / gcd(dpi as i32, 96);
    let snap = |pixel: f32| (pixel.floor() as i32).div_euclid(step) * step;
    let pixels = RECT {
        left: snap((area.left + origin) * scale).max(whole.left),
        top: snap(area.top * scale).max(whole.top),
        right: (((area.right + origin) * scale).ceil() as i32).min(whole.right),
        bottom: ((area.bottom * scale).ceil() as i32).min(whole.bottom),
    };
    (pixels.left < pixels.right && pixels.top < pixels.bottom).then_some(pixels)
}

fn gcd(a: i32, b: i32) -> i32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn union_of(a: RECT, b: RECT) -> RECT {
    RECT { left: a.left.min(b.left), top: a.top.min(b.top), right: a.right.max(b.right), bottom: a.bottom.max(b.bottom) }
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F { left, top, right, bottom }
}

fn clipped(inner: D2D_RECT_F, outer: D2D_RECT_F) -> D2D_RECT_F {
    rect(inner.left.max(outer.left), inner.top.max(outer.top), inner.right.min(outer.right), inner.bottom.min(outer.bottom))
}

fn width(area: &D2D_RECT_F) -> f32 {
    area.right - area.left
}

fn height(area: &D2D_RECT_F) -> f32 {
    area.bottom - area.top
}

#[cfg(test)]
mod tests {
    use super::share_space;

    #[test]
    fn short_sections_keep_their_size_and_long_ones_split_the_rest() {
        assert_eq!(share_space(&[50.0, 500.0, 500.0], 350.0), [50.0, 150.0, 150.0]);
    }

    #[test]
    fn leftover_space_goes_to_the_last_section() {
        assert_eq!(share_space(&[50.0, 60.0], 200.0), [50.0, 150.0]);
    }
}
