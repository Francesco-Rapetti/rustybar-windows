//! Shapes from SVG path data (the `d` of a `<path>`), read once into segments and filled with
//! Direct2D at any size: the icons of `home` (Material Design Icons, the ones Home Assistant
//! shows) and the logo Home Assistant serves, which no font has. Only paths, filled in one color:
//! no strokes, gradients, transforms or other elements.

use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::Graphics::Direct2D::Common::{
    D2D_SIZE_F, D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_FILL_MODE_WINDING,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_QUADRATIC_BEZIER_SEGMENT, D2D1_SWEEP_DIRECTION_CLOCKWISE,
    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, ID2D1DeviceContext, ID2D1SolidColorBrush,
};
use windows::core::Result;
use windows_numerics::Vector2;

use crate::ui::Rect;

/// A piece of a path, in absolute coordinates of its view box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Quad(f32, f32, f32, f32),
    /// Radii already large enough for the ends, as SVG wants them (Direct2D doesn't fix them).
    Arc { rx: f32, ry: f32, rotation: f32, large: bool, sweep: bool, x: f32, y: f32 },
    Close,
}

/// A shape to fill: its segments in its view box (x, y, width, height).
#[derive(Debug, PartialEq)]
pub struct Outline {
    pub segs: Vec<Seg>,
    pub view: [f32; 4],
    /// Tells outlines apart in a signature without hashing their segments.
    pub id: u64,
}

impl Outline {
    /// The path data `d` in a view box; None if it can't be read.
    pub fn new(d: &str, view: [f32; 4]) -> Option<Outline> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let segs = parse(d)?;
        (!segs.is_empty() && view[2] > 0.0 && view[3] > 0.0).then(|| Outline { segs, view, id: NEXT.fetch_add(1, Ordering::Relaxed) })
    }

    /// Every `<path d="…">` of an SVG document in its `viewBox` (or its width and height), as one
    /// outline: a logo of one color.
    pub fn from_svg(svg: &str) -> Option<Outline> {
        let open = svg.find("<svg")?;
        let tag = &svg[open..open + svg[open..].find('>')?];
        let view = match attribute(tag, "viewBox") {
            Some(v) => {
                let n: Vec<f32> = v.split(|c: char| c == ',' || c.is_whitespace()).filter_map(|s| s.parse().ok()).collect();
                if n.len() != 4 {
                    return None;
                }
                [n[0], n[1], n[2], n[3]]
            }
            None => {
                let size = |name| attribute(tag, name).and_then(|v| v.trim_end_matches("px").parse::<f32>().ok());
                [0.0, 0.0, size("width")?, size("height")?]
            }
        };
        let mut d = String::new();
        let mut rest = &svg[open..];
        while let Some(at) = rest.find("<path") {
            rest = &rest[at..];
            let end = rest.find('>')?;
            if let Some(path) = attribute(&rest[..end], "d") {
                d.push_str(path);
                d.push(' ');
            }
            rest = &rest[end..];
        }
        Outline::new(&d, view)
    }
}

/// The value of attribute `name` in a tag, between its quotes.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(at) = tag[from..].find(name) {
        let start = from + at;
        from = start + name.len();
        // A whole name: `d=` but not `id=`
        if start > 0 && !tag.as_bytes()[start - 1].is_ascii_whitespace() {
            continue;
        }
        let after = tag[from..].trim_start();
        let Some(after) = after.strip_prefix('=') else { continue };
        let after = after.trim_start();
        let quote = after.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let value = &after[1..];
        return Some(&value[..value.find(quote)?]);
    }
    None
}

/// Reads path data: numbers with or without separators, flags of arcs written together.
struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

impl Lexer<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len() && (self.s[self.i].is_ascii_whitespace() || self.s[self.i] == b',') {
            self.i += 1;
        }
    }

    fn done(&mut self) -> bool {
        self.skip();
        self.i >= self.s.len()
    }

    /// The command letter here, if there's one.
    fn letter(&mut self) -> Option<u8> {
        self.skip();
        let c = *self.s.get(self.i)?;
        (c.is_ascii_alphabetic() && c != b'e' && c != b'E').then(|| {
            self.i += 1;
            c
        })
    }

    fn number(&mut self) -> Option<f32> {
        self.skip();
        let start = self.i;
        let at = |i: usize| self.s.get(i).copied().unwrap_or(0);
        let mut i = self.i;
        if at(i) == b'+' || at(i) == b'-' {
            i += 1;
        }
        let digits = |i: &mut usize| {
            let from = *i;
            while at(*i).is_ascii_digit() {
                *i += 1;
            }
            *i > from
        };
        let whole = digits(&mut i);
        let mut fraction = false;
        if at(i) == b'.' {
            i += 1;
            fraction = digits(&mut i);
        }
        if !whole && !fraction {
            return None;
        }
        if at(i) == b'e' || at(i) == b'E' {
            let mut j = i + 1;
            if at(j) == b'+' || at(j) == b'-' {
                j += 1;
            }
            if digits(&mut j) {
                i = j;
            }
        }
        self.i = i;
        std::str::from_utf8(&self.s[start..i]).ok()?.parse().ok()
    }

    /// A flag of an arc: one character, `0` or `1`, even with nothing after it.
    fn flag(&mut self) -> Option<bool> {
        self.skip();
        let c = *self.s.get(self.i)?;
        self.i += 1;
        match c {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        }
    }
}

/// Path data as absolute segments; None if it can't be read.
pub fn parse(d: &str) -> Option<Vec<Seg>> {
    let mut lx = Lexer { s: d.as_bytes(), i: 0 };
    let mut segs = Vec::new();
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    // The last control points, for the smooth curves that reflect them
    let mut cubic: Option<(f32, f32)> = None;
    let mut quad: Option<(f32, f32)> = None;
    let mut cmd = 0u8;
    while !lx.done() {
        match lx.letter() {
            Some(c) => cmd = c,
            // Numbers go on with the command before (none after `Z`)
            None if cmd == 0 || cmd.eq_ignore_ascii_case(&b'z') => return None,
            None => {}
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
        let (mut next_cubic, mut next_quad) = (None, None);
        match cmd.to_ascii_uppercase() {
            b'M' => {
                let (x, y) = (lx.number()? + ox, lx.number()? + oy);
                segs.push(Seg::Move(x, y));
                (cx, cy, sx, sy) = (x, y, x, y);
                // More pairs after a move are lines
                cmd = if rel { b'l' } else { b'L' };
            }
            b'L' => {
                let (x, y) = (lx.number()? + ox, lx.number()? + oy);
                segs.push(Seg::Line(x, y));
                (cx, cy) = (x, y);
            }
            b'H' => {
                cx = lx.number()? + ox;
                segs.push(Seg::Line(cx, cy));
            }
            b'V' => {
                cy = lx.number()? + oy;
                segs.push(Seg::Line(cx, cy));
            }
            b'C' | b'S' => {
                let (x1, y1) = if cmd.eq_ignore_ascii_case(&b'c') {
                    (lx.number()? + ox, lx.number()? + oy)
                } else {
                    cubic.map_or((cx, cy), |(px, py)| (2.0 * cx - px, 2.0 * cy - py))
                };
                let (x2, y2) = (lx.number()? + ox, lx.number()? + oy);
                let (x, y) = (lx.number()? + ox, lx.number()? + oy);
                segs.push(Seg::Cubic(x1, y1, x2, y2, x, y));
                next_cubic = Some((x2, y2));
                (cx, cy) = (x, y);
            }
            b'Q' | b'T' => {
                let (x1, y1) = if cmd.eq_ignore_ascii_case(&b'q') {
                    (lx.number()? + ox, lx.number()? + oy)
                } else {
                    quad.map_or((cx, cy), |(px, py)| (2.0 * cx - px, 2.0 * cy - py))
                };
                let (x, y) = (lx.number()? + ox, lx.number()? + oy);
                segs.push(Seg::Quad(x1, y1, x, y));
                next_quad = Some((x1, y1));
                (cx, cy) = (x, y);
            }
            b'A' => {
                let (rx, ry, rotation) = (lx.number()?.abs(), lx.number()?.abs(), lx.number()?);
                let (large, sweep) = (lx.flag()?, lx.flag()?);
                let (x, y) = (lx.number()? + ox, lx.number()? + oy);
                if rx == 0.0 || ry == 0.0 {
                    segs.push(Seg::Line(x, y));
                } else {
                    let (rx, ry) = arc_radii(cx, cy, x, y, rx, ry, rotation);
                    segs.push(Seg::Arc { rx, ry, rotation, large, sweep, x, y });
                }
                (cx, cy) = (x, y);
            }
            b'Z' => {
                segs.push(Seg::Close);
                (cx, cy) = (sx, sy);
            }
            _ => return None,
        }
        (cubic, quad) = (next_cubic, next_quad);
    }
    Some(segs)
}

/// An arc's radii, scaled up when they can't reach from one end to the other (SVG's rule).
fn arc_radii(x1: f32, y1: f32, x2: f32, y2: f32, rx: f32, ry: f32, rotation: f32) -> (f32, f32) {
    let (sin, cos) = rotation.to_radians().sin_cos();
    let (dx, dy) = ((x1 - x2) / 2.0, (y1 - y2) / 2.0);
    let (px, py) = (cos * dx + sin * dy, -sin * dx + cos * dy);
    let lambda = (px * px) / (rx * rx) + (py * py) / (ry * ry);
    if lambda > 1.0 {
        let k = lambda.sqrt();
        (rx * k, ry * k)
    } else {
        (rx, ry)
    }
}

/// Fills `outline` with `brush`, as large as fits in `rect` (DIPs) and centered in it.
pub fn fill(ctx: &ID2D1DeviceContext, brush: &ID2D1SolidColorBrush, outline: &Outline, rect: Rect) -> Result<()> {
    let [vx, vy, vw, vh] = outline.view;
    let k = (rect.w / vw).min(rect.h / vh);
    let (ox, oy) = (rect.x + (rect.w - vw * k) / 2.0 - vx * k, rect.y + (rect.h - vh * k) / 2.0 - vy * k);
    let p = |x: f32, y: f32| Vector2 { X: ox + x * k, Y: oy + y * k };
    unsafe {
        let geometry = ctx.GetFactory()?.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        sink.SetFillMode(D2D1_FILL_MODE_WINDING);
        let mut open = false;
        let (mut start, mut at) = ((0.0, 0.0), (0.0, 0.0));
        for seg in &outline.segs {
            if let Seg::Move(x, y) = *seg {
                if open {
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                }
                sink.BeginFigure(p(x, y), D2D1_FIGURE_BEGIN_FILLED);
                (open, start, at) = (true, (x, y), (x, y));
                continue;
            }
            // A segment after `Z` starts again where the figure did
            if !open {
                sink.BeginFigure(p(at.0, at.1), D2D1_FIGURE_BEGIN_FILLED);
                (open, start) = (true, at);
            }
            at = match *seg {
                Seg::Line(x, y) => {
                    sink.AddLine(p(x, y));
                    (x, y)
                }
                Seg::Cubic(x1, y1, x2, y2, x, y) => {
                    sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: p(x1, y1), point2: p(x2, y2), point3: p(x, y) });
                    (x, y)
                }
                Seg::Quad(x1, y1, x, y) => {
                    sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: p(x1, y1), point2: p(x, y) });
                    (x, y)
                }
                Seg::Arc { rx, ry, rotation, large, sweep, x, y } => {
                    sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: p(x, y),
                        size: D2D_SIZE_F { width: rx * k, height: ry * k },
                        rotationAngle: rotation,
                        // SVG's y goes down, as Direct2D's: a positive sweep is clockwise
                        sweepDirection: if sweep { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                        arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                    });
                    (x, y)
                }
                Seg::Close => {
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    open = false;
                    start
                }
                Seg::Move(..) => unreachable!(),
            };
        }
        if open {
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
        }
        sink.Close()?;
        ctx.FillGeometry(&geometry, brush, None);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_without_separators() {
        let segs = parse("M120.001 0c-3.787 0-7.573 1.499-10.444 4.49L.5.25h-2V3z").unwrap();
        assert_eq!(segs[0], Seg::Move(120.001, 0.0));
        let Seg::Cubic(x1, y1, _, _, x, y) = segs[1] else { panic!("{:?}", segs[1]) };
        assert!((x1 - 116.214).abs() < 1e-3 && y1 == 0.0);
        assert!((x - 109.557).abs() < 1e-3 && (y - 4.49).abs() < 1e-3);
        assert_eq!(segs[2], Seg::Line(0.5, 0.25));
        assert_eq!(segs[3], Seg::Line(-1.5, 0.25));
        assert_eq!(segs[4], Seg::Line(-1.5, 3.0));
        assert_eq!(segs[5], Seg::Close);
    }

    #[test]
    fn pairs_after_a_move_are_lines() {
        let segs = parse("m1 1 2 0 0 2z m1 1").unwrap();
        assert_eq!(segs, vec![Seg::Move(1.0, 1.0), Seg::Line(3.0, 1.0), Seg::Line(3.0, 3.0), Seg::Close, Seg::Move(2.0, 2.0)]);
    }

    #[test]
    fn arcs_with_their_flags_together() {
        // The logo Home Assistant serves writes them so: `a20.2 20.2 90 1 1 12.542-13.06`
        let segs = parse("M0 0a5 5 0 1012 0A5,5,0,0,1,0,0").unwrap();
        let Seg::Arc { rx, large, sweep, x, y, .. } = segs[1] else { panic!("{:?}", segs[1]) };
        // Radii too short for the ends grow to reach them
        assert!((rx - 6.0).abs() < 1e-4);
        assert!(large && !sweep);
        assert_eq!((x, y), (12.0, 0.0));
        let Seg::Arc { rx, large, sweep, .. } = segs[2] else { panic!() };
        assert!((rx - 6.0).abs() < 1e-4 && !large && sweep);
    }

    #[test]
    fn smooth_curves_reflect_the_control_point() {
        let segs = parse("M0 0C1 2 3 2 4 0S7 -2 8 0Q9 2 10 0T12 0").unwrap();
        assert_eq!(segs[2], Seg::Cubic(5.0, -2.0, 7.0, -2.0, 8.0, 0.0));
        assert_eq!(segs[4], Seg::Quad(11.0, -2.0, 12.0, 0.0));
    }

    #[test]
    fn what_cant_be_read() {
        assert_eq!(parse("1 2"), None);
        assert_eq!(parse("M1"), None);
        assert_eq!(parse("M0 0 Z 1 2"), None);
        assert_eq!(parse("M0 0 X1 2"), None);
    }

    #[test]
    fn a_logo_from_its_document() {
        let svg = r#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" width="640" height="640" viewBox="0 0 240 240"><path id="a" d="M0 0h240v240z"/><path fill="red" d='M10 10h5v5z'/></svg>"#;
        let o = Outline::from_svg(svg).unwrap();
        assert_eq!(o.view, [0.0, 0.0, 240.0, 240.0]);
        assert_eq!(o.segs.iter().filter(|s| matches!(s, Seg::Move(..))).count(), 2);
        let no_view = r#"<svg width="24px" height="24"><path d="M0 0h1v1z"/></svg>"#;
        assert_eq!(Outline::from_svg(no_view).unwrap().view, [0.0, 0.0, 24.0, 24.0]);
        assert!(Outline::from_svg("<html>not found</html>").is_none());
    }
}
