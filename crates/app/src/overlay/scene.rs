//! A frame's drawing recorded as a list of operations. Comparing it with the last frame's list says
//! where the panel changed, so only that part is drawn again and pushed to the window.

use windows::{
    core::Result,
    Win32::Graphics::{
        Direct2D::{
            Common::{D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D_RECT_F},
            ID2D1Bitmap, ID2D1Brush, ID2D1RenderTarget, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2,
            D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT,
        },
        DirectWrite::{IDWriteTextLayout, DWRITE_TEXT_METRICS},
    },
};
use windows_numerics::Vector2;

use super::render::{Icon, IconSet};

/// How far antialiasing and glyphs reaching past their advance can spill outside a shape or text, in DIPs.
const SPILL: f32 = 3.0;

#[derive(Clone, PartialEq)]
enum Op {
    Text { layout: IDWriteTextLayout, at: Vector2, color: D2D1_COLOR_F },
    RoundedRect { shape: D2D1_ROUNDED_RECT, color: D2D1_COLOR_F },
    Ellipse { shape: D2D1_ELLIPSE, color: D2D1_COLOR_F },
    Ring { shape: D2D1_ELLIPSE, stroke: f32, color: D2D1_COLOR_F },
    Bitmap { bitmap: ID2D1Bitmap, area: D2D_RECT_F },
    Pill { shape: D2D1_ROUNDED_RECT, gloss: Gloss },
    Glass { shape: D2D1_ROUNDED_RECT, glass: Glass },
    Icon { icon: Icon, area: D2D_RECT_F, color: D2D1_COLOR_F },
    PushClip(D2D_RECT_F),
    PopClip,
}

/// The colours of a glossy pill: a rim, a body shading from `top` to `bottom`, and a highlight across its upper half.
#[derive(Clone, Copy, PartialEq)]
pub struct Gloss {
    pub top: D2D1_COLOR_F,
    pub bottom: D2D1_COLOR_F,
    pub rim: D2D1_COLOR_F,
    pub highlight: D2D1_COLOR_F,
}

/// A surface lit from above: a body shading from `top` to `bottom`, and a one-DIP rim fading from `rim_top` to `rim_bottom`.
#[derive(Clone, Copy, PartialEq)]
pub struct Glass {
    pub top: D2D1_COLOR_F,
    pub bottom: D2D1_COLOR_F,
    pub rim_top: D2D1_COLOR_F,
    pub rim_bottom: D2D1_COLOR_F,
}

/// One frame's drawing operations, each with the part of the panel it can change, in DIPs.
#[derive(Default)]
pub struct Scene {
    ops: Vec<(Op, D2D_RECT_F)>,
    /// The clips pushed while recording, innermost last.
    clips: Vec<D2D_RECT_F>,
}

impl Scene {
    pub fn text(&mut self, layout: &IDWriteTextLayout, x: f32, y: f32, color: D2D1_COLOR_F) -> Result<()> {
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)? };
        let (left, top) = (x + metrics.left, y + metrics.top);
        let ink = D2D_RECT_F { left, top, right: left + metrics.widthIncludingTrailingWhitespace, bottom: top + metrics.height };
        self.add(Op::Text { layout: layout.clone(), at: Vector2 { X: x, Y: y }, color }, ink);
        Ok(())
    }

    pub fn fill_rounded(&mut self, area: D2D_RECT_F, radius: f32, color: D2D1_COLOR_F) {
        self.add(Op::RoundedRect { shape: D2D1_ROUNDED_RECT { rect: area, radiusX: radius, radiusY: radius }, color }, area);
    }

    pub fn fill_dot(&mut self, x: f32, y: f32, radius: f32, color: D2D1_COLOR_F) {
        let shape = D2D1_ELLIPSE { point: Vector2 { X: x, Y: y }, radiusX: radius, radiusY: radius };
        self.add(Op::Ellipse { shape, color }, D2D_RECT_F { left: x - radius, top: y - radius, right: x + radius, bottom: y + radius });
    }

    /// A circle's outline, `stroke` DIPs wide, centred on `radius`.
    pub fn ring(&mut self, x: f32, y: f32, radius: f32, stroke: f32, color: D2D1_COLOR_F) {
        let shape = D2D1_ELLIPSE { point: Vector2 { X: x, Y: y }, radiusX: radius, radiusY: radius };
        let reach = radius + stroke / 2.0;
        self.add(Op::Ring { shape, stroke, color }, D2D_RECT_F { left: x - reach, top: y - reach, right: x + reach, bottom: y + reach });
    }

    /// A rounded bar with a rim, a vertical gradient and a soft highlight.
    pub fn pill(&mut self, area: D2D_RECT_F, radius: f32, gloss: Gloss) {
        self.add(Op::Pill { shape: D2D1_ROUNDED_RECT { rect: area, radiusX: radius, radiusY: radius }, gloss }, area);
    }

    pub fn glass(&mut self, area: D2D_RECT_F, radius: f32, glass: Glass) {
        self.add(Op::Glass { shape: D2D1_ROUNDED_RECT { rect: area, radiusX: radius, radiusY: radius }, glass }, area);
    }

    /// A line icon centred in `area`, stroked 1.5 DIPs wide at any size.
    pub fn icon(&mut self, icon: Icon, area: D2D_RECT_F, color: D2D1_COLOR_F) {
        self.add(Op::Icon { icon, area, color }, area);
    }

    pub fn bitmap(&mut self, bitmap: &ID2D1Bitmap, area: D2D_RECT_F) {
        self.add(Op::Bitmap { bitmap: bitmap.clone(), area }, area);
    }

    pub fn push_clip(&mut self, area: D2D_RECT_F) {
        let clip = self.clips.last().map_or(area, |outer| intersection(area, *outer));
        self.ops.push((Op::PushClip(area), clip));
        self.clips.push(clip);
    }

    pub fn pop_clip(&mut self) {
        self.clips.pop();
        self.ops.push((Op::PopClip, D2D_RECT_F::default()));
    }

    /// Where the next operation goes, for drawing beneath what follows it with [`Scene::under`].
    pub fn mark(&self) -> usize {
        self.ops.len()
    }

    /// Records `draw` beneath everything recorded since `mark`, e.g. a card sized to the rows drawn on it.
    /// The clips must be the same as at `mark`.
    pub fn under(&mut self, mark: usize, draw: impl FnOnce(&mut Scene)) {
        let mut layer = Scene { ops: Vec::new(), clips: self.clips.clone() };
        draw(&mut layer);
        self.ops.splice(mark..mark, layer.ops);
    }

    /// Records `op`, unless the clips hide all of it.
    fn add(&mut self, op: Op, bounds: D2D_RECT_F) {
        let spilled = D2D_RECT_F { left: bounds.left - SPILL, top: bounds.top - SPILL, right: bounds.right + SPILL, bottom: bounds.bottom + SPILL };
        let visible = self.clips.last().map_or(spilled, |clip| intersection(spilled, *clip));
        if !is_empty(visible) {
            self.ops.push((op, visible));
        }
    }

    /// Where this scene can look different from `before`, as bands from top to bottom; none when the
    /// two are the same. A pixel no changed operation reaches is drawn by the same operations in the
    /// same order in both.
    pub fn changes_since(&self, before: &Scene) -> Vec<D2D_RECT_F> {
        let start = self.ops.iter().zip(&before.ops).take_while(|(new, old)| new == old).count();
        let (new, old) = (&self.ops[start..], &before.ops[start..]);
        let end = new.iter().rev().zip(old.iter().rev()).take_while(|(new, old)| new == old).count();
        let (new, old) = (&new[..new.len() - end], &old[..old.len() - end]);
        let mut changed: Vec<D2D_RECT_F> = if new.len() == old.len() {
            // Lined up one to one, as when a line's text changed: only the operations that differ count.
            new.iter().zip(old).filter(|(new, old)| new != old).flat_map(|((_, new), (_, old))| [*new, *old]).collect()
        } else {
            new.iter().chain(old).map(|(_, bounds)| *bounds).collect()
        };
        changed.retain(|area| !is_empty(*area));
        changed.sort_by(|a, b| a.top.total_cmp(&b.top));
        let mut bands: Vec<D2D_RECT_F> = Vec::new();
        for area in changed {
            match bands.last_mut() {
                Some(band) if area.top <= band.bottom => *band = union(*band, area),
                _ => bands.push(area),
            }
        }
        bands
    }

    /// Draws the operations that reach into `area` (in DIPs) on `target`, recoloring `brush` as it goes.
    /// Each operation's bounds hold every pixel it can change, so skipping the rest draws the same pixels.
    pub fn draw(&self, target: &ID2D1RenderTarget, brush: &ID2D1SolidColorBrush, icons: &IconSet, area: D2D_RECT_F) -> Result<()> {
        let paint = |color: &D2D1_COLOR_F| {
            unsafe { brush.SetColor(color) };
            brush
        };
        for (op, bounds) in &self.ops {
            let clip = matches!(op, Op::PushClip(_) | Op::PopClip);
            if !clip && is_empty(intersection(*bounds, area)) {
                continue;
            }
            unsafe {
                match op {
                    Op::Text { layout, at, color } => target.DrawTextLayout(*at, layout, paint(color), D2D1_DRAW_TEXT_OPTIONS_NONE),
                    Op::RoundedRect { shape, color } => target.FillRoundedRectangle(shape, paint(color)),
                    Op::Ellipse { shape, color } => target.FillEllipse(shape, paint(color)),
                    Op::Ring { shape, stroke, color } => target.DrawEllipse(shape, paint(color), *stroke, None),
                    Op::Bitmap { bitmap, area } => target.DrawBitmap(bitmap, Some(area), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None),
                    Op::Pill { shape, gloss } => draw_pill(target, shape, gloss, paint(&gloss.rim)),
                    Op::Glass { shape, glass } => draw_glass(target, shape, glass),
                    Op::Icon { icon, area, color } => icons.draw(target, *icon, area, color)?,
                    Op::PushClip(area) => target.PushAxisAlignedClip(area, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE),
                    Op::PopClip => target.PopAxisAlignedClip(),
                }
            }
        }
        Ok(())
    }
}

/// The rim is the pill itself; the body and highlight are drawn inside it. Gradients are made per pill,
/// which is cheap next to filling it, and pills are only drawn when they change.
fn draw_pill(target: &ID2D1RenderTarget, pill: &D2D1_ROUNDED_RECT, gloss: &Gloss, rim: &ID2D1SolidColorBrush) {
    const RIM: f32 = 1.5;
    let inset = |shape: &D2D1_ROUNDED_RECT, x: f32, y: f32| D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F { left: shape.rect.left + x, top: shape.rect.top + y, right: shape.rect.right - x, bottom: shape.rect.bottom - y },
        radiusX: (shape.radiusX - x).max(0.0),
        radiusY: (shape.radiusY - y).max(0.0),
    };
    let body = inset(pill, RIM, RIM);
    let mut shine = inset(&body, body.radiusX * 0.35 + 2.0, 2.0);
    shine.rect.bottom = shine.rect.top + (body.rect.bottom - body.rect.top) * 0.45;
    shine.radiusY = shine.radiusY.min((shine.rect.bottom - shine.rect.top) / 2.0);
    let clear = D2D1_COLOR_F { a: 0.0, ..gloss.highlight };
    unsafe {
        target.FillRoundedRectangle(pill, rim);
        if let Ok(brush) = vertical_gradient(target, &body.rect, gloss.top, gloss.bottom) {
            target.FillRoundedRectangle(&body, &brush);
        }
        if let Ok(brush) = vertical_gradient(target, &shine.rect, gloss.highlight, clear) {
            target.FillRoundedRectangle(&shine, &brush);
        }
    }
}

/// Gradients are absolute, so drawing any part of the surface gives the same pixels as drawing all of it.
fn draw_glass(target: &ID2D1RenderTarget, shape: &D2D1_ROUNDED_RECT, glass: &Glass) {
    let r = shape.rect;
    let rim = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F { left: r.left + 0.5, top: r.top + 0.5, right: r.right - 0.5, bottom: r.bottom - 0.5 },
        radiusX: (shape.radiusX - 0.5).max(0.0),
        radiusY: (shape.radiusY - 0.5).max(0.0),
    };
    unsafe {
        if let Ok(brush) = vertical_gradient(target, &r, glass.top, glass.bottom) {
            target.FillRoundedRectangle(shape, &brush);
        }
        if (glass.rim_top.a > 0.0 || glass.rim_bottom.a > 0.0)
            && let Ok(brush) = vertical_gradient(target, &r, glass.rim_top, glass.rim_bottom)
        {
            target.DrawRoundedRectangle(&rim, &brush, 1.0, None);
        }
    }
}

fn vertical_gradient(target: &ID2D1RenderTarget, area: &D2D_RECT_F, top: D2D1_COLOR_F, bottom: D2D1_COLOR_F) -> Result<ID2D1Brush> {
    let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: top }, D2D1_GRADIENT_STOP { position: 1.0, color: bottom }];
    let line = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
        startPoint: Vector2 { X: area.left, Y: area.top },
        endPoint: Vector2 { X: area.left, Y: area.bottom },
    };
    unsafe {
        let stops = target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
        Ok(target.CreateLinearGradientBrush(&line, None, &stops)?.into())
    }
}

fn intersection(a: D2D_RECT_F, b: D2D_RECT_F) -> D2D_RECT_F {
    D2D_RECT_F { left: a.left.max(b.left), top: a.top.max(b.top), right: a.right.min(b.right), bottom: a.bottom.min(b.bottom) }
}

fn union(a: D2D_RECT_F, b: D2D_RECT_F) -> D2D_RECT_F {
    D2D_RECT_F { left: a.left.min(b.left), top: a.top.min(b.top), right: a.right.max(b.right), bottom: a.bottom.max(b.bottom) }
}

fn is_empty(area: D2D_RECT_F) -> bool {
    area.right <= area.left || area.bottom <= area.top
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };

    fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
        D2D_RECT_F { left, top, right, bottom }
    }

    fn scene(dots: &[(f32, f32)]) -> Scene {
        let mut scene = Scene::default();
        scene.fill_rounded(rect(0.0, 0.0, 100.0, 100.0), 4.0, WHITE);
        dots.iter().for_each(|&(x, y)| scene.fill_dot(x, y, 2.0, WHITE));
        scene
    }

    #[test]
    fn the_same_drawing_changes_nothing() {
        assert_eq!(scene(&[(10.0, 10.0)]).changes_since(&scene(&[(10.0, 10.0)])), []);
    }

    #[test]
    fn a_moved_shape_changes_where_it_was_and_where_it_is() {
        let changed = scene(&[(10.0, 10.0), (50.0, 50.0)]).changes_since(&scene(&[(10.0, 20.0), (50.0, 50.0)]));
        assert_eq!(changed, [rect(5.0, 5.0, 15.0, 25.0)]);
    }

    #[test]
    fn changes_far_apart_make_separate_bands() {
        let changed = scene(&[(10.0, 10.0), (50.0, 50.0), (80.0, 90.0)]).changes_since(&scene(&[(12.0, 10.0), (50.0, 50.0), (82.0, 90.0)]));
        assert_eq!(changed, [rect(5.0, 5.0, 17.0, 15.0), rect(75.0, 85.0, 87.0, 95.0)]);
    }

    #[test]
    fn an_added_shape_changes_everything_after_the_common_start_and_end() {
        let changed = scene(&[(10.0, 10.0), (90.0, 90.0)]).changes_since(&scene(&[(10.0, 10.0), (50.0, 50.0), (90.0, 90.0)]));
        assert_eq!(changed, [rect(45.0, 45.0, 55.0, 55.0)]);
    }

    #[test]
    fn what_is_drawn_under_goes_before_what_followed_the_mark() {
        let mut drawn = scene(&[(10.0, 10.0)]);
        let mark = drawn.mark();
        drawn.fill_dot(50.0, 50.0, 2.0, WHITE);
        drawn.under(mark, |layer| layer.fill_dot(80.0, 80.0, 2.0, WHITE));
        assert!(drawn.ops == scene(&[(10.0, 10.0), (80.0, 80.0), (50.0, 50.0)]).ops);
    }

    #[test]
    fn what_a_clip_hides_changes_nothing() {
        let clipped = |y| {
            let mut scene = Scene::default();
            scene.push_clip(rect(0.0, 0.0, 100.0, 50.0));
            scene.fill_dot(10.0, y, 2.0, WHITE);
            scene.pop_clip();
            scene
        };
        assert_eq!(clipped(80.0).changes_since(&clipped(90.0)), []);
        assert_eq!(clipped(10.0).changes_since(&clipped(90.0)), [rect(5.0, 5.0, 15.0, 15.0)]);
    }
}
