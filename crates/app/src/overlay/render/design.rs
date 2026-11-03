//! Kiku's look in one place: a palette per app theme, and the radii, spacing and type sizes every theme
//! shares, for the panel, the settings window and captions mode. Surfaces are glass lit from above.
//! Presets are hand-tuned; the Custom theme is worked out from the preset it was made from.

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

use crate::overlay::scene::Glass;
use kiku_core::theme::{CustomTheme, Theme};

/// One theme's colours and surfaces.
#[derive(Clone)]
pub struct Palette {
    /// Opaque, so busy windows behind the panel don't compete with the captions, and scrolled text can fade into it.
    pub window: Glass,
    /// A hairline around the window, so a light window shows on a light desktop; clear on dark themes.
    pub edge: D2D1_COLOR_F,
    /// The band behind a title bar, where the light is brightest.
    pub title_band: Glass,
    /// A group of settings, a prompt, a chip.
    pub card: Glass,
    /// The settings window's bar of tabs: tinted glass below the window, so the open tab's raised capsule stands out.
    pub nav: Glass,
    /// The chosen segment of a control, raised towards the light.
    pub raised: Glass,
    /// Menus and palettes opened over other rows.
    pub popover: Glass,
    /// A button: bare until the pointer is on it, then a plate whose top edge catches the light.
    pub plate: Glass,
    /// A button held down, pushed in.
    pub pressed: Glass,
    /// The main action: accent glass, lit from above.
    pub primary: Glass,
    /// The window's close button under the pointer.
    pub danger: Glass,
    /// Recessed tracks: segmented controls, switches, sliders and meters.
    pub track: Glass,
    /// The chosen segment of a control, tinted with the accent.
    pub chosen: Glass,
    /// A slider's or switch's knob.
    pub knob: Glass,
    // Text, in three levels, and the one accent for state and focus: live, listening, links, chosen.
    pub text: D2D1_COLOR_F,
    pub text_dim: D2D1_COLOR_F,
    pub text_faint: D2D1_COLOR_F,
    pub accent: D2D1_COLOR_F,
    /// Text and icons on an accent fill.
    pub on_accent: D2D1_COLOR_F,
    pub warn: D2D1_COLOR_F,
    pub error: D2D1_COLOR_F,
    /// Washes over a row under the pointer.
    pub hover: D2D1_COLOR_F,
    /// Hairlines between rows and groups.
    pub divider: D2D1_COLOR_F,
    /// Selected text.
    pub selected: D2D1_COLOR_F,
    /// Behind your lines in an app's section, when they are highlighted.
    pub yours: D2D1_COLOR_F,
    /// Covers what a prompt sits over: darkens on dark themes, frosts on light ones.
    pub shade: D2D1_COLOR_F,
    pub shadow: D2D1_COLOR_F,
    pub grip: D2D1_COLOR_F,
    /// Around colour swatches, so colours close to the background keep an outline.
    pub swatch_rim: D2D1_COLOR_F,
}

impl Palette {
    /// This palette with the window's background at `window` opacity and the text and icons at `text`,
    /// from 0 to 1; surfaces drawn over the background keep theirs.
    pub fn see_through(&self, window: f32, text: f32) -> Palette {
        let glass = |glass: Glass| Glass {
            top: fade(glass.top, window),
            bottom: fade(glass.bottom, window),
            rim_top: fade(glass.rim_top, window),
            rim_bottom: fade(glass.rim_bottom, window),
        };
        Palette {
            window: glass(self.window),
            title_band: glass(self.title_band),
            edge: fade(self.edge, window),
            text: fade(self.text, text),
            text_dim: fade(self.text_dim, text),
            text_faint: fade(self.text_faint, text),
            accent: fade(self.accent, text),
            yours: fade(self.yours, text),
            ..self.clone()
        }
    }
}

const fn fade(colour: D2D1_COLOR_F, by: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: colour.a * by, ..colour }
}

/// The palette of a preset `theme`; Custom's is [`custom`]'s.
pub fn preset(theme: Theme) -> &'static Palette {
    &PRESETS[preset_index(theme)]
}

/// The Custom theme's palette: its base preset's, with its background and accent. A background of its
/// own brings surfaces, text and shading worked out from it, dark or light by its lightness.
pub fn custom(custom: &CustomTheme) -> Palette {
    let (original, colours) = (custom.base.colours(), custom.colours);
    let is_light = colours.is_light();
    let mut base = BASES[preset_index(custom.base)];
    if colours.background != original.background {
        base = on_background(base, colours.background, is_light);
    }
    if colours.accent != original.accent {
        (base.accent, base.on_accent) = (colours.accent, on_colour(colours.accent));
    }
    if (colours.background, colours.accent) != (original.background, original.accent) {
        base.nav = nav(colours.background, colours.accent, is_light);
    }
    if is_light { light(base) } else { dark(base) }
}

static PRESETS: [Palette; 6] = [dark(NIGHT), light(DAY), dark(LAGOON), light(MEADOW), light(BLOSSOM), dark(GRAPHITE)];
const BASES: [Base; 6] = [NIGHT, DAY, LAGOON, MEADOW, BLOSSOM, GRAPHITE];

/// Where `theme` is in [`Theme::ALL`]; Custom, never a base itself, gives Night's like [`Theme::colours`].
fn preset_index(theme: Theme) -> usize {
    Theme::ALL.iter().position(|&preset| preset == theme).unwrap_or(0)
}

/// Dark slate glass, aqua accent.
const NIGHT: Base = Base {
    top: Theme::Night.colours().background,
    bottom: 0x12171f,
    nav: (0x16343b, 0x0e252b),
    popover: (0x27313f, 0x1c232e),
    text: 0xeef3f7,
    dim: 0xaab6c2,
    faint: 0x8996a5,
    accent: Theme::Night.colours().accent,
    on_accent: 0x0b1a20,
    warn: 0xf2b85c,
    error: 0xf7a8a8,
    deep: 0x0b0f15,
};

/// Light frosted sky glass, deep-sea blue.
const DAY: Base = Base {
    top: Theme::Day.colours().background,
    bottom: 0xe0edf7,
    nav: (0xdce8f3, 0xcfdeeb),
    popover: (0xffffff, 0xf1f7fc),
    text: 0x0f2236,
    dim: 0x354c63,
    faint: 0x55697e,
    accent: Theme::Day.colours().accent,
    on_accent: 0xffffff,
    warn: 0x7a4600,
    error: 0xa1221b,
    deep: 0x0e2a47,
};

/// Deep teal glass, mint accent.
const LAGOON: Base = Base {
    top: Theme::Lagoon.colours().background,
    bottom: 0x0a242a,
    nav: (0x0e2f35, 0x081f24),
    popover: (0x18474e, 0x0f2f35),
    text: 0xe8f7f4,
    dim: 0xb4d8d2,
    faint: 0x8fbbb4,
    accent: Theme::Lagoon.colours().accent,
    on_accent: 0x062a24,
    warn: 0xf2c26b,
    error: 0xffb3ad,
    deep: 0x061518,
};

/// Light mint glass, fresh green.
const MEADOW: Base = Base {
    top: Theme::Meadow.colours().background,
    bottom: 0xdff2e5,
    nav: (0xdcefe2, 0xcde5d4),
    popover: (0xffffff, 0xeef8f1),
    text: 0x12301f,
    dim: 0x34553f,
    faint: 0x52735c,
    accent: Theme::Meadow.colours().accent,
    on_accent: 0xffffff,
    warn: 0x744b00,
    error: 0xa1221b,
    deep: 0x123d24,
};

/// Light rose glass, pink.
const BLOSSOM: Base = Base {
    top: Theme::Blossom.colours().background,
    bottom: 0xf6e2ea,
    nav: (0xf5dde7, 0xecceda),
    popover: (0xffffff, 0xfbeef3),
    text: 0x3a1424,
    dim: 0x63384b,
    faint: 0x815a69,
    accent: Theme::Blossom.colours().accent,
    on_accent: 0xffffff,
    warn: 0x7a4600,
    error: 0xa1221b,
    deep: 0x4a1730,
};

/// Neutral dark glass, silver-blue.
const GRAPHITE: Base = Base {
    top: Theme::Graphite.colours().background,
    bottom: 0x1b1c1f,
    nav: (0x1e2731, 0x151b22),
    popover: (0x303236, 0x232427),
    text: 0xececee,
    dim: 0xbcbfc5,
    faint: 0x9a9ea6,
    accent: Theme::Graphite.colours().accent,
    on_accent: 0x141a22,
    warn: 0xf2bf6b,
    error: 0xf7b0a9,
    deep: 0x0e0f11,
};

/// What sets a theme apart, as `0xRRGGBB`; [`dark`] and [`light`] work every surface out from it.
#[derive(Clone, Copy)]
struct Base {
    /// The window's gradient.
    top: u32,
    bottom: u32,
    nav: (u32, u32),
    popover: (u32, u32),
    text: u32,
    dim: u32,
    faint: u32,
    accent: u32,
    on_accent: u32,
    warn: u32,
    error: u32,
    /// A dark theme's deepest colour, to shade with; a light theme's ink, its deep hue, for shading, rims and shadows.
    deep: u32,
}

/// A dark theme: white light on the glass, black shadows, and its deepest colour to shade with.
const fn dark(base: Base) -> Palette {
    Palette {
        window: Glass { top: hex(base.top, 1.0), bottom: hex(base.bottom, 1.0), rim_top: white(0.16), rim_bottom: white(0.04) },
        edge: CLEAR,
        title_band: Glass { top: white(0.05), bottom: white(0.0), rim_top: CLEAR, rim_bottom: CLEAR },
        card: Glass { top: white(0.05), bottom: white(0.025), rim_top: white(0.09), rim_bottom: white(0.03) },
        nav: Glass { top: hex(base.nav.0, 1.0), bottom: hex(base.nav.1, 1.0), rim_top: white(0.12), rim_bottom: black(0.3) },
        raised: Glass { top: white(0.17), bottom: white(0.09), rim_top: white(0.24), rim_bottom: white(0.06) },
        popover: Glass { top: hex(base.popover.0, 1.0), bottom: hex(base.popover.1, 1.0), rim_top: white(0.16), rim_bottom: white(0.05) },
        plate: Glass { top: white(0.14), bottom: white(0.05), rim_top: white(0.22), rim_bottom: black(0.25) },
        pressed: Glass { top: black(0.22), bottom: white(0.03), rim_top: black(0.3), rim_bottom: white(0.08) },
        primary: accent_glass(hex(base.accent, 1.0)),
        danger: accent_glass(hex(base.error, 1.0)),
        track: Glass { top: black(0.28), bottom: black(0.16), rim_top: black(0.3), rim_bottom: white(0.07) },
        chosen: chosen(hex(base.accent, 1.0)),
        knob: Glass { top: white(0.95), bottom: white(0.72), rim_top: white(1.0), rim_bottom: black(0.3) },
        text: hex(base.text, 1.0),
        text_dim: hex(base.dim, 1.0),
        text_faint: hex(base.faint, 1.0),
        accent: hex(base.accent, 1.0),
        on_accent: hex(base.on_accent, 1.0),
        warn: hex(base.warn, 1.0),
        error: hex(base.error, 1.0),
        hover: white(0.07),
        divider: white(0.07),
        selected: hex(base.accent, 0.24),
        yours: hex(base.accent, 0.14),
        shade: hex(base.deep, 0.6),
        shadow: black(0.25),
        grip: white(0.22),
        swatch_rim: white(0.22),
    }
}

/// A light theme: frosted plates lighter than the window, shaded, rimmed and shadowed in its ink.
const fn light(base: Base) -> Palette {
    let ink = base.deep;
    let shaded = mix(hex(base.top, 1.0), hex(ink, 1.0), 0.04);
    Palette {
        window: Glass { top: hex(base.top, 1.0), bottom: hex(base.bottom, 1.0), rim_top: white(1.0), rim_bottom: hex(ink, 0.06) },
        edge: hex(ink, 0.2),
        title_band: Glass { top: white(0.7), bottom: white(0.0), rim_top: CLEAR, rim_bottom: CLEAR },
        card: Glass { top: white(0.6), bottom: white(0.35), rim_top: white(0.95), rim_bottom: hex(ink, 0.08) },
        nav: Glass { top: hex(base.nav.0, 1.0), bottom: hex(base.nav.1, 1.0), rim_top: white(0.7), rim_bottom: hex(ink, 0.12) },
        raised: Glass { top: white(1.0), bottom: shaded, rim_top: white(1.0), rim_bottom: hex(ink, 0.16) },
        popover: Glass { top: hex(base.popover.0, 1.0), bottom: hex(base.popover.1, 1.0), rim_top: white(1.0), rim_bottom: hex(ink, 0.14) },
        plate: Glass { top: white(0.9), bottom: white(0.5), rim_top: white(1.0), rim_bottom: hex(ink, 0.18) },
        pressed: Glass { top: hex(ink, 0.12), bottom: hex(ink, 0.04), rim_top: hex(ink, 0.2), rim_bottom: white(0.9) },
        primary: accent_glass(hex(base.accent, 1.0)),
        danger: accent_glass(hex(base.error, 1.0)),
        track: Glass { top: hex(ink, 0.11), bottom: hex(ink, 0.05), rim_top: hex(ink, 0.14), rim_bottom: white(0.9) },
        chosen: Glass { top: white(1.0), bottom: shaded, rim_top: white(1.0), rim_bottom: hex(ink, 0.2) },
        knob: Glass { top: white(1.0), bottom: shaded, rim_top: white(1.0), rim_bottom: hex(ink, 0.3) },
        text: hex(base.text, 1.0),
        text_dim: hex(base.dim, 1.0),
        text_faint: hex(base.faint, 1.0),
        accent: hex(base.accent, 1.0),
        on_accent: hex(base.on_accent, 1.0),
        warn: hex(base.warn, 1.0),
        error: hex(base.error, 1.0),
        hover: hex(ink, 0.06),
        divider: hex(ink, 0.09),
        selected: hex(base.accent, 0.18),
        yours: hex(base.accent, 0.12),
        shade: hex(base.top, 0.72),
        shadow: hex(ink, 0.10),
        grip: hex(ink, 0.25),
        swatch_rim: black(0.18),
    }
}

/// `base` on a `background` of the user's: the window's gradient, menus, text, warnings and shading worked out from it.
fn on_background(base: Base, background: u32, is_light: bool) -> Base {
    let (white, black) = (0xffffff, 0x000000);
    if is_light {
        let ink = blend(background, black, 0.82);
        Base {
            top: background,
            bottom: blend(background, ink, 0.07),
            popover: (white, blend(white, background, 0.6)),
            text: blend(background, black, 0.88),
            dim: blend(background, black, 0.72),
            faint: blend(background, black, 0.58),
            warn: 0x7a4600,
            error: 0xa1221b,
            deep: ink,
            ..base
        }
    } else {
        Base {
            top: background,
            bottom: blend(background, black, 0.35),
            popover: (blend(background, white, 0.07), blend(background, white, 0.02)),
            text: blend(white, background, 0.06),
            dim: blend(white, background, 0.32),
            faint: blend(white, background, 0.47),
            warn: 0xf2b85c,
            error: 0xf7a8a8,
            deep: blend(background, black, 0.6),
            ..base
        }
    }
}

/// The settings window's bar of tabs: below the window, tinted with the accent.
fn nav(background: u32, accent: u32, is_light: bool) -> (u32, u32) {
    let (deeper, tint) = if is_light { ((0.08, 0.13), 0.06) } else { ((0.2, 0.4), 0.12) };
    let shade = |amount| blend(blend(background, 0x000000, amount), accent, tint);
    (shade(deeper.0), shade(deeper.1))
}

/// Text on `fill`: white, or a near-black of its hue, whichever contrasts more (WCAG's ratio; even at
/// a relative luminance of 0.179).
fn on_colour(fill: u32) -> u32 {
    let linear = |shift: u32| {
        let c = ((fill >> shift) & 0xff) as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let luminance = 0.2126 * linear(16) + 0.7152 * linear(8) + 0.0722 * linear(0);
    if luminance < 0.179 { 0xffffff } else { blend(fill, 0x000000, 0.85) }
}

/// `amount` of the way from `a` to `b`, as `0xRRGGBB`.
fn blend(a: u32, b: u32, amount: f32) -> u32 {
    let channel = |shift: u32| {
        let (from, to) = (((a >> shift) & 0xff) as f32, ((b >> shift) & 0xff) as f32);
        ((from + (to - from) * amount).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

pub const RADIUS_WINDOW: f32 = 12.0;
pub const RADIUS_CARD: f32 = 12.0;
pub const RADIUS_CONTROL: f32 = 8.0;

// Spacing on a 4-DIP grid.
pub const PADDING: f32 = 16.0;
pub const GAP: f32 = 12.0;
pub const ROW_GAP: f32 = 6.0;
/// A row of settings, and the controls centred in it.
pub const SETTING_ROW: f32 = 40.0;
pub const CONTROL: f32 = 28.0;

// Type sizes; captions are the product, so they are the largest text on the panel after window titles.
pub const TITLE_SIZE: f32 = 17.0;
/// A screen's heading, a prompt's question.
pub const HEADING_SIZE: f32 = 15.0;
pub const LABEL_SIZE: f32 = 13.5;
pub const BODY_SIZE: f32 = 13.0;
pub const BUTTON_SIZE: f32 = 12.5;
pub const CAPTION_SIZE: f32 = 15.0;
pub const SMALL_SIZE: f32 = 11.5;
/// Settings headings, in capitals.
pub const OVERLINE_SIZE: f32 = 11.0;

/// The window's background at `y` of a window `height` tall, for fading things into it.
pub fn background_at(palette: &Palette, y: f32, height: f32) -> D2D1_COLOR_F {
    mix(palette.window.top, palette.window.bottom, (y / height.max(1.0)).clamp(0.0, 1.0))
}

pub const CLEAR: D2D1_COLOR_F = rgba(0, 0, 0, 0.0);

pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
}

/// `0xRRGGBB` at opacity `a`.
pub const fn hex(rgb: u32, a: f32) -> D2D1_COLOR_F {
    rgba((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, a)
}

/// `amount` of the way from `a` to `b`, opacity included.
pub const fn mix(a: D2D1_COLOR_F, b: D2D1_COLOR_F, amount: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: a.r + (b.r - a.r) * amount,
        g: a.g + (b.g - a.g) * amount,
        b: a.b + (b.b - a.b) * amount,
        a: a.a + (b.a - a.a) * amount,
    }
}

/// `colour` as glass: lighter at the top, deeper at the bottom, with a rim darker than its body.
const fn accent_glass(colour: D2D1_COLOR_F) -> Glass {
    Glass { top: mix(colour, white(1.0), 0.22), bottom: mix(colour, black(1.0), 0.12), rim_top: mix(colour, white(1.0), 0.45), rim_bottom: mix(colour, black(1.0), 0.35) }
}

/// A see-through wash of `accent`, for a chosen segment on a dark theme.
const fn chosen(accent: D2D1_COLOR_F) -> Glass {
    Glass { top: with_alpha(accent, 0.34), bottom: with_alpha(accent, 0.16), rim_top: with_alpha(accent, 0.7), rim_bottom: with_alpha(accent, 0.2) }
}

/// A rimless surface shading from `top` to `bottom`, e.g. a highlight or a fade.
pub const fn gradient(top: D2D1_COLOR_F, bottom: D2D1_COLOR_F) -> Glass {
    Glass { top, bottom, rim_top: CLEAR, rim_bottom: CLEAR }
}

/// `colour` at opacity `a`.
pub const fn with_alpha(colour: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a, ..colour }
}

pub const fn white(a: f32) -> D2D1_COLOR_F {
    hex(0xffffff, a)
}

pub const fn black(a: f32) -> D2D1_COLOR_F {
    hex(0x000000, a)
}
