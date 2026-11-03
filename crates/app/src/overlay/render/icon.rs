//! Kiku's line icons: stroked paths on a 24-unit grid with round caps and joins, drawn at any size
//! with the same stroke width.
//!
//! The path data is from Lucide (https://lucide.dev), ISC licence, Copyright (c) Lucide Contributors;
//! circles and rectangles are written out as paths.

use std::{cell::RefCell, collections::HashMap};

use windows::{
    core::Result,
    Win32::Graphics::{
        Direct2D::{
            Common::{
                D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BEZIER_SEGMENT, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED,
                D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U,
            },
            ID2D1Bitmap, ID2D1Factory, ID2D1GeometrySink, ID2D1PathGeometry, ID2D1RenderTarget, ID2D1StrokeStyle, D2D1_ARC_SEGMENT,
            D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_CAP_STYLE_ROUND,
            D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE, D2D1_DASH_STYLE_SOLID, D2D1_LINE_JOIN_ROUND, D2D1_QUADRATIC_BEZIER_SEGMENT, D2D1_STROKE_STYLE_PROPERTIES, D2D1_SWEEP_DIRECTION_CLOCKWISE,
            D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
        },
        Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
    },
};
use windows_numerics::{Matrix3x2, Vector2};

/// The grid the path data is drawn on.
const GRID: f32 = 24.0;
/// The stroke width in DIPs, whatever size an icon is drawn at.
const STROKE: f32 = 1.5;

macro_rules! icons {
    ($($name:ident => $path:literal,)*) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub enum Icon { $($name),* }

        impl Icon {
            pub const ALL: &[Icon] = &[$(Icon::$name),*];

            /// SVG path data on the 24-unit grid.
            fn path(self) -> &'static str {
                match self { $(Icon::$name => $path),* }
            }
        }
    };
}

icons! {
    Power => "M12 2v10M18.4 6.6a9 9 0 1 1-12.77.04",
    Sparkles => "M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0zM20 3v4M22 5h-4M4 17v2M5 18H3",
    Captions => "M5 5h14a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2zM7 15h4M15 15h2M7 11h2M13 11h4",
    Settings => "M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2zM9 12a3 3 0 1 0 6 0a3 3 0 1 0-6 0z",
    History => "M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8M3 3v5h5M12 7v5l4 2",
    Save => "M15.2 3a2 2 0 0 1 1.4.6l3.8 3.8a2 2 0 0 1 .6 1.4V19a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2zM17 21v-7a1 1 0 0 0-1-1H8a1 1 0 0 0-1 1v7M7 3v4a1 1 0 0 0 1 1h7",
    Lock => "M5 11h14a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2zM7 11V7a5 5 0 0 1 10 0v4",
    Unlock => "M5 11h14a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2zM7 11V7a5 5 0 0 1 9.9-1",
    Minimize => "M5 12h14",
    Close => "M18 6 6 18M6 6l12 12",
    Copy => "M10 8h10a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H10a2 2 0 0 1-2-2V10a2 2 0 0 1 2-2zM4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2",
    ChevronDown => "m6 9 6 6 6-6",
    ChevronRight => "m9 18 6-6-6-6",
    ChevronLeft => "m15 18-6-6 6-6",
    ChevronUp => "m18 15-6-6-6 6",
    Eye => "M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0M9 12a3 3 0 1 0 6 0a3 3 0 1 0-6 0z",
    EyeOff => "M10.733 5.076a10.744 10.744 0 0 1 11.205 6.575 1 1 0 0 1 0 .696 10.747 10.747 0 0 1-1.444 2.49M14.084 14.158a3 3 0 0 1-4.242-4.242M17.479 17.499a10.75 10.75 0 0 1-15.417-5.151 1 1 0 0 1 0-.696 10.75 10.75 0 0 1 4.446-5.143M2 2l20 20",
    Ban => "M2 12a10 10 0 1 0 20 0a10 10 0 1 0-20 0zM4.9 4.9l14.2 14.2",
    Pin => "M12 17v5M5 17h14v-1.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V6h1a2 2 0 0 0 0-4H8a2 2 0 0 0 0 4h1v4.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24z",
    Check => "M20 6 9 17l-5-5",
    Mic => "M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3zM19 10v2a7 7 0 0 1-14 0v-2M12 19v3",
    Speaker => "M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298zM16 9a5 5 0 0 1 0 6M19.364 18.364a9 9 0 0 0 0-12.728",
    Sliders => "M21 4h-7M10 4H3M21 12h-9M8 12H3M21 20h-5M12 20H3M14 2v4M8 10v4M16 18v4",
    Apps => "M4 3h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1zM15 3h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-5a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1zM15 14h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-5a1 1 0 0 1-1-1v-5a1 1 0 0 1 1-1zM4 14h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-5a1 1 0 0 1 1-1z",
    Palette => "M12 22a1 1 0 0 1 0-20 10 9 0 0 1 10 9 5 5 0 0 1-5 5h-2.25a1.75 1.75 0 0 0-1.4 2.8l.3.4a1.75 1.75 0 0 1-1.4 2.8zM13 6.5a.5.5 0 1 0 1 0a.5.5 0 1 0-1 0zM17 10.5a.5.5 0 1 0 1 0a.5.5 0 1 0-1 0zM6 12.5a.5.5 0 1 0 1 0a.5.5 0 1 0-1 0zM8 7.5a.5.5 0 1 0 1 0a.5.5 0 1 0-1 0z",
    SpellCheck => "m6 16 6-12 6 12M8 12h8M16 20l2 2 4-4",
    Keyboard => "M4 4h16a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zM6 8h.01M10 8h.01M14 8h.01M18 8h.01M8 12h.01M12 12h.01M16 12h.01M7 16h10",
    Grip => "M8 5a1 1 0 1 0 2 0a1 1 0 1 0-2 0zM8 12a1 1 0 1 0 2 0a1 1 0 1 0-2 0zM8 19a1 1 0 1 0 2 0a1 1 0 1 0-2 0zM14 5a1 1 0 1 0 2 0a1 1 0 1 0-2 0zM14 12a1 1 0 1 0 2 0a1 1 0 1 0-2 0zM14 19a1 1 0 1 0 2 0a1 1 0 1 0-2 0z",
    Download => "M12 15V3M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5",
    Plus => "M5 12h14M12 5v14",
    Reset => "M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8M3 3v5h5",
    Search => "M3 11a8 8 0 1 0 16 0a8 8 0 1 0-16 0zM21 21l-4.3-4.3",
    Trash => "M3 6h18M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2M10 11v6M14 11v6",
    Database => "M3 5a9 3 0 1 0 18 0a9 3 0 1 0-18 0zM3 5v14a9 3 0 0 0 18 0V5M3 12a9 3 0 0 0 18 0",
    Play => "M6 3l14 9-14 9z",
    Sun => "M8 12a4 4 0 1 0 8 0a4 4 0 1 0-8 0zM12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41",
    Moon => "M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9z",
}

/// Every icon's geometry, built once, the round-capped stroke they are drawn with, and each icon as
/// stroked once per size, DPI and colour.
pub struct IconSet {
    paths: Vec<ID2D1PathGeometry>,
    stroke: ID2D1StrokeStyle,
    stroked: RefCell<HashMap<Stroked, ID2D1Bitmap>>,
}

/// An icon's side in pixels, the DPI and its colour's bits.
type Stroked = (Icon, u32, u32, [u32; 4]);

impl IconSet {
    pub fn new(factory: &ID2D1Factory) -> Result<Self> {
        let paths = Icon::ALL.iter().map(|icon| geometry(factory, icon.path())).collect::<Result<_>>()?;
        let round = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 10.0,
            dashStyle: D2D1_DASH_STYLE_SOLID,
            dashOffset: 0.0,
        };
        let stroke = unsafe { factory.CreateStrokeStyle(&round, None)? };
        Ok(Self { paths, stroke, stroked: RefCell::default() })
    }

    /// Paints `icon` in `color` centred in the largest square that fits in `area`, on whole pixels. It is
    /// stroked once and then copied: stroking it in place antialiases its curves a little differently
    /// depending on where the band of the frame being drawn starts. The target's transform must only translate.
    pub fn draw(&self, target: &ID2D1RenderTarget, icon: Icon, area: &D2D_RECT_F, color: &D2D1_COLOR_F) -> Result<()> {
        let (mut dpi, mut dpi_y) = (0.0, 0.0);
        unsafe { target.GetDpi(&mut dpi, &mut dpi_y) };
        let scale = dpi / 96.0;
        let side = (area.right - area.left).min(area.bottom - area.top);
        let pixels = (side * scale).round().max(1.0);
        let bitmap = self.stroked(target, icon, pixels as u32, dpi, color)?;
        let mut before = Matrix3x2::default();
        unsafe { target.GetTransform(&mut before) };
        let corner = |middle: f32, offset: f32| ((middle + offset) * scale - pixels / 2.0).round() / scale;
        let (x, y) = (corner((area.left + area.right) / 2.0, before.M31), corner((area.top + area.bottom) / 2.0, before.M32));
        let place = D2D_RECT_F { left: x, top: y, right: x + pixels / scale, bottom: y + pixels / scale };
        unsafe {
            target.SetTransform(&Matrix3x2::identity());
            target.DrawBitmap(&bitmap, Some(&place), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
            target.SetTransform(&before);
        }
        Ok(())
    }

    /// `icon` stroked in `color` in a square `pixels` wide at `dpi`, made the first time it is needed.
    fn stroked(&self, target: &ID2D1RenderTarget, icon: Icon, pixels: u32, dpi: f32, color: &D2D1_COLOR_F) -> Result<ID2D1Bitmap> {
        let key = (icon, pixels, dpi as u32, [color.r, color.g, color.b, color.a].map(f32::to_bits));
        if let Some(bitmap) = self.stroked.borrow().get(&key) {
            return Ok(bitmap.clone());
        }
        let format = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };
        let size = D2D_SIZE_U { width: pixels, height: pixels };
        let scale = pixels as f32 / GRID;
        let bitmap = unsafe {
            let canvas = target.CreateCompatibleRenderTarget(None, Some(&size), Some(&format), D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE)?;
            canvas.SetDpi(96.0, 96.0);
            let brush = canvas.CreateSolidColorBrush(color, None)?;
            canvas.BeginDraw();
            canvas.Clear(None);
            canvas.SetTransform(&Matrix3x2::scale(scale, scale));
            canvas.DrawGeometry(&self.paths[icon as usize], &brush, STROKE * dpi / 96.0 / scale, &self.stroke);
            canvas.EndDraw(None, None)?;
            canvas.GetBitmap()?
        };
        self.stroked.borrow_mut().insert(key, bitmap.clone());
        Ok(bitmap)
    }
}

fn geometry(factory: &ID2D1Factory, data: &str) -> Result<ID2D1PathGeometry> {
    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        Pen { sink: &sink, at: Vector2::default(), start: Vector2::default(), control: None, drawing: false }.trace(data);
        sink.Close()?;
        Ok(geometry)
    }
}

/// Follows SVG path data into a geometry sink. The data is Kiku's own, so a mistake in it panics.
struct Pen<'a> {
    sink: &'a ID2D1GeometrySink,
    at: Vector2,
    /// Where the figure being drawn started, for `Z`.
    start: Vector2,
    /// The last cubic's second control point, which `S` mirrors.
    control: Option<Vector2>,
    drawing: bool,
}

impl Pen<'_> {
    fn trace(mut self, data: &str) {
        let mut numbers = Numbers { data: data.as_bytes(), next: 0 };
        let mut command = 0u8;
        while let Some(next) = numbers.peek() {
            if next.is_ascii_alphabetic() {
                command = next;
                numbers.next += 1;
            } else {
                assert!(!matches!(command, 0 | b'Z' | b'z'), "numbers with no command in {data:?}");
            }
            let origin = if command.is_ascii_lowercase() { self.at } else { Vector2::default() };
            let point = |numbers: &mut Numbers| {
                let x = numbers.number();
                Vector2 { X: origin.X + x, Y: origin.Y + numbers.number() }
            };
            let control = self.control.take();
            match command.to_ascii_uppercase() {
                b'M' => {
                    let to = point(&mut numbers);
                    self.move_to(to);
                    // Further pairs after a move are lines.
                    command = if command == b'm' { b'l' } else { b'L' };
                }
                b'L' => {
                    let to = point(&mut numbers);
                    self.line_to(to);
                }
                b'H' => {
                    let x = numbers.number() + origin.X;
                    self.line_to(Vector2 { X: x, Y: self.at.Y });
                }
                b'V' => {
                    let y = numbers.number() + origin.Y;
                    self.line_to(Vector2 { X: self.at.X, Y: y });
                }
                b'C' => {
                    let (first, second, to) = (point(&mut numbers), point(&mut numbers), point(&mut numbers));
                    self.cubic_to(first, second, to);
                }
                b'S' => {
                    let first = control.map_or(self.at, |c| Vector2 { X: 2.0 * self.at.X - c.X, Y: 2.0 * self.at.Y - c.Y });
                    let (second, to) = (point(&mut numbers), point(&mut numbers));
                    self.cubic_to(first, second, to);
                }
                b'Q' => {
                    let (control, to) = (point(&mut numbers), point(&mut numbers));
                    self.figure();
                    unsafe { self.sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: control, point2: to }) };
                    self.at = to;
                }
                b'A' => {
                    let (rx, ry, rotation) = (numbers.number(), numbers.number(), numbers.number());
                    let (large, sweep) = (numbers.flag(), numbers.flag());
                    let to = point(&mut numbers);
                    self.arc_to(rx, ry, rotation, large, sweep, to);
                }
                b'Z' => self.close(),
                _ => panic!("unknown path command {:?} in {data:?}", command as char),
            }
        }
        if self.drawing {
            unsafe { self.sink.EndFigure(D2D1_FIGURE_END_OPEN) };
        }
    }

    fn move_to(&mut self, to: Vector2) {
        if self.drawing {
            unsafe { self.sink.EndFigure(D2D1_FIGURE_END_OPEN) };
            self.drawing = false;
        }
        (self.at, self.start) = (to, to);
    }

    /// Begins a figure at the pen if none is open, as when drawing goes on after `Z`.
    fn figure(&mut self) {
        if !self.drawing {
            unsafe { self.sink.BeginFigure(self.at, D2D1_FIGURE_BEGIN_HOLLOW) };
            (self.start, self.drawing) = (self.at, true);
        }
    }

    fn line_to(&mut self, to: Vector2) {
        self.figure();
        unsafe { self.sink.AddLine(to) };
        self.at = to;
    }

    fn cubic_to(&mut self, first: Vector2, second: Vector2, to: Vector2) {
        self.figure();
        unsafe { self.sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: first, point2: second, point3: to }) };
        (self.at, self.control) = (to, Some(second));
    }

    /// An SVG arc: radii too small to reach `to` grow until they just do, as SVG specifies.
    fn arc_to(&mut self, rx: f32, ry: f32, rotation: f32, large: bool, sweep: bool, to: Vector2) {
        if to == self.at {
            return;
        }
        let (mut rx, mut ry) = (rx.abs(), ry.abs());
        if rx == 0.0 || ry == 0.0 {
            return self.line_to(to);
        }
        let (sin, cos) = rotation.to_radians().sin_cos();
        let (dx, dy) = ((self.at.X - to.X) / 2.0, (self.at.Y - to.Y) / 2.0);
        let (x, y) = (cos * dx + sin * dy, -sin * dx + cos * dy);
        let reach = (x / rx).powi(2) + (y / ry).powi(2);
        if reach > 1.0 {
            (rx, ry) = (rx * reach.sqrt(), ry * reach.sqrt());
        }
        self.figure();
        let arc = D2D1_ARC_SEGMENT {
            point: to,
            size: D2D_SIZE_F { width: rx, height: ry },
            rotationAngle: rotation,
            sweepDirection: if sweep { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
            arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
        };
        unsafe { self.sink.AddArc(&arc) };
        self.at = to;
    }

    fn close(&mut self) {
        if self.drawing {
            unsafe { self.sink.EndFigure(D2D1_FIGURE_END_CLOSED) };
            self.drawing = false;
        }
        self.at = self.start;
    }
}

/// Reads path data's numbers, which may run together: "1.1.9" is 1.1 then .9, "2-.9" is 2 then -.9.
struct Numbers<'a> {
    data: &'a [u8],
    next: usize,
}

impl Numbers<'_> {
    /// The next character after any separators, if any.
    fn peek(&mut self) -> Option<u8> {
        while self.data.get(self.next).is_some_and(|c| c.is_ascii_whitespace() || *c == b',') {
            self.next += 1;
        }
        self.data.get(self.next).copied()
    }

    fn number(&mut self) -> f32 {
        self.peek();
        let start = self.next;
        let at = |next: usize| self.data.get(next).copied().unwrap_or(0);
        let mut end = start;
        if matches!(at(end), b'+' | b'-') {
            end += 1;
        }
        let mut dot = false;
        while at(end).is_ascii_digit() || (at(end) == b'.' && !dot) {
            dot |= at(end) == b'.';
            end += 1;
        }
        if matches!(at(end), b'e' | b'E') {
            end += 1;
            if matches!(at(end), b'+' | b'-') {
                end += 1;
            }
            while at(end).is_ascii_digit() {
                end += 1;
            }
        }
        self.next = end;
        let text = std::str::from_utf8(&self.data[start..end]).unwrap();
        text.parse().unwrap_or_else(|_| panic!("expected a number at {start} in path data, found {text:?}"))
    }

    /// An arc's flag, a single 0 or 1 that needs no separator after it.
    fn flag(&mut self) -> bool {
        let flag = self.peek();
        self.next += 1;
        match flag {
            Some(b'0') => false,
            Some(b'1') => true,
            _ => panic!("expected an arc flag at {} in path data", self.next - 1),
        }
    }
}
