//! What a widget puts in the bar: a pill of text items, measured with DirectWrite. Sizes are in
//! DIPs, the same on every screen; each bar scales them to its own.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_FONT_WEIGHT_REGULAR, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory, IDWriteFactory,
    IDWriteInlineObject, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::core::{BOOL, HSTRING, Result};

use crate::theme::{Rgba, Theme};

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub text: String,
    pub color: Rgba,
    pub small: bool,
    /// Longer text ends with an ellipsis.
    pub max_width: Option<f32>,
    /// Shorter text is centered in this width.
    pub min_width: f32,
    /// At least as wide as this text, and lined up on its left: a number that keeps its room
    /// when it loses a digit (`"100%"`), so the pill doesn't change size.
    pub fixed: Option<&'static str>,
    /// The room before the item, instead of the pill's gap.
    pub space: Option<f32>,
    /// A glyph of Windows' icon font (Segoe Fluent Icons) instead of text.
    pub icon: bool,
    /// The Windows logo, drawn as its four squares in the room of an icon (no font has it).
    pub logo: bool,
    /// A picture (a cover), a square as tall as the theme's font is big, with rounded corners.
    pub image: Option<Arc<Image>>,
    /// Drawn in place of text: a level as a small bar, a thin line between groups.
    pub drawn: Option<Drawn>,
}

/// What an item can be drawn as, in its color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Drawn {
    /// A level (0–100) as a small rounded bar: the item's color is the track, the level is filled
    /// in the other color (a battery).
    Gauge(u8, Rgba),
    /// A thin upright line, as tall as the text.
    Rule,
}

/// How wide a gauge is in a pill, in DIPs.
pub const GAUGE_WIDTH: f32 = 20.0;

impl Item {
    pub fn text(text: impl Into<String>, color: Rgba) -> Item {
        Item { text: text.into(), color, small: false, max_width: None, min_width: 0.0, fixed: None, space: None, icon: false, logo: false, image: None, drawn: None }
    }

    /// A level as a small bar, `fill` over a `track`.
    pub fn gauge(level: u8, fill: Rgba, track: Rgba) -> Item {
        Item { drawn: Some(Drawn::Gauge(level.min(100), fill)), min_width: GAUGE_WIDTH, ..Item::text("", track) }
    }

    /// A thin line between groups of items.
    pub fn rule(color: Rgba) -> Item {
        Item { drawn: Some(Drawn::Rule), min_width: 1.0, ..Item::text("", color) }
    }

    /// An icon: a glyph of Segoe Fluent Icons (`"\u{E767}"`, the speaker).
    pub fn icon(glyph: &str, color: Rgba) -> Item {
        Item { icon: true, ..Item::text(glyph, color) }
    }

    /// The Windows logo, in the room of an icon (measured as a four-square glyph of the same
    /// size), as the macOS RustyBar has Apple's.
    pub fn windows_logo(color: Rgba) -> Item {
        Item { logo: true, ..Item::icon("\u{E8A9}", color) }
    }

    /// A picture `size` DIPs on a side (a cover), in place of text.
    pub fn image(image: Arc<Image>, size: f32) -> Item {
        Item { image: Some(image), min_width: size, ..Item::text("", Rgba::TRANSPARENT) }
    }

    /// In the theme's small font.
    pub fn small(mut self) -> Item {
        self.small = true;
        self
    }

    pub fn fixed(mut self, template: &'static str) -> Item {
        self.fixed = Some(template);
        self
    }

    pub fn space(mut self, space: f32) -> Item {
        self.space = Some(space);
        self
    }

    pub fn max_width(mut self, width: f32) -> Item {
        self.max_width = Some(width);
        self
    }

    pub fn min_width(mut self, width: f32) -> Item {
        self.min_width = width;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pill {
    pub items: Vec<Item>,
    pub gap: f32,
    /// The item under the highlight, a rounded tile in the primary color that slides from one
    /// item to the next.
    pub highlight: Option<usize>,
    /// Drawn without the background and the border: the items stand on the desktop alone, as
    /// the Apple logo does in the macOS menu bar. The pill keeps its size, so its place and the
    /// area that takes a click stay the same.
    pub bare: bool,
}

impl Pill {
    pub fn new(items: Vec<Item>) -> Pill {
        Pill { items, gap: 6.0, highlight: None, bare: false }
    }

    pub fn bare(mut self) -> Pill {
        self.bare = true;
        self
    }

    pub fn gap(mut self, gap: f32) -> Pill {
        self.gap = gap;
        self
    }

    pub fn highlight(mut self, index: usize) -> Pill {
        self.highlight = Some(index);
        self
    }
}

/// A laid out item: where it starts in the pill, how wide it is, and its text ready to draw.
#[derive(Clone)]
pub struct Placed {
    pub x: f32,
    pub width: f32,
    /// Where the text starts in the item's width.
    pub text_x: f32,
    pub text_height: f32,
    pub layout: IDWriteTextLayout,
}

/// A pill measured: its width and its items, plus a signature of what it draws.
#[derive(Clone)]
pub struct Measured {
    pub key: String,
    pub pill: Pill,
    pub width: f32,
    pub items: Vec<Placed>,
    pub text_sig: u64,
}

/// The two fonts of the bar, and the ellipsis that ends text cut short.
pub struct Text {
    factory: IDWriteFactory,
    regular: IDWriteTextFormat,
    small: IDWriteTextFormat,
    icons: IDWriteTextFormat,
    ellipsis: IDWriteInlineObject,
    small_ellipsis: IDWriteInlineObject,
    font_sig: u64,
    /// The widths of the `fixed` templates, measured once.
    templates: RefCell<HashMap<(&'static str, bool), f32>>,
}

impl Text {
    pub fn new(theme: &Theme) -> Result<Text> {
        unsafe {
            let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let format = |family: &str, weight, size: f32| -> Result<IDWriteTextFormat> {
                let f = factory.CreateTextFormat(
                    &HSTRING::from(family),
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    &HSTRING::new(),
                )?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                Ok(f)
            };
            let regular = format(&theme.font, DWRITE_FONT_WEIGHT_SEMI_BOLD, theme.font_size)?;
            let small = format(&theme.font, DWRITE_FONT_WEIGHT_MEDIUM, theme.small_size)?;
            let icons = format(icon_family(&factory), DWRITE_FONT_WEIGHT_REGULAR, theme.font_size + 1.0)?;
            let ellipsis = factory.CreateEllipsisTrimmingSign(&regular)?;
            let small_ellipsis = factory.CreateEllipsisTrimmingSign(&small)?;
            let mut h = DefaultHasher::new();
            (theme.font.as_str(), theme.font_size.to_bits(), theme.small_size.to_bits()).hash(&mut h);
            Ok(Text { factory, regular, small, icons, ellipsis, small_ellipsis, font_sig: h.finish(), templates: RefCell::new(HashMap::new()) })
        }
    }

    fn layout(&self, item: &Item) -> Result<(IDWriteTextLayout, f32, f32)> {
        let wide: Vec<u16> = item.text.encode_utf16().collect();
        let (format, sign) = if item.icon {
            (&self.icons, &self.ellipsis)
        } else if item.small {
            (&self.small, &self.small_ellipsis)
        } else {
            (&self.regular, &self.ellipsis)
        };
        unsafe {
            let layout = self.factory.CreateTextLayout(&wide, format, item.max_width.unwrap_or(10_000.0), 1_000.0)?;
            if item.max_width.is_some() {
                let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                layout.SetTrimming(&trimming, sign)?;
            }
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            let width = match item.max_width {
                Some(max) => m.widthIncludingTrailingWhitespace.min(max),
                None => m.widthIncludingTrailingWhitespace,
            };
            Ok((layout, width, m.height))
        }
    }

    fn template_width(&self, template: &'static str, small: bool) -> f32 {
        if let Some(w) = self.templates.borrow().get(&(template, small)) {
            return *w;
        }
        let item = Item { small, ..Item::text(template, Rgba::TRANSPARENT) };
        let w = self.layout(&item).map(|(_, w, _)| w.ceil()).unwrap_or(0.0);
        self.templates.borrow_mut().insert((template, small), w);
        w
    }

    /// Lays out a widget's pill: its items side by side, `gap` apart, inside the theme's padding.
    pub fn measure(&self, key: &str, pill: Pill, theme: &Theme) -> Result<Measured> {
        let mut x = theme.pill_padding;
        let mut items = Vec::with_capacity(pill.items.len());
        let mut h = DefaultHasher::new();
        self.font_sig.hash(&mut h);
        pill.highlight.hash(&mut h);
        for (i, item) in pill.items.iter().enumerate() {
            if i > 0 {
                x += item.space.unwrap_or(pill.gap);
            }
            let (layout, text_width, text_height) = self.layout(item)?;
            let (width, text_x) = match item.fixed {
                Some(template) => (text_width.max(self.template_width(template, item.small)), 0.0),
                None => {
                    let width = text_width.max(item.min_width);
                    (width, (width - text_width) / 2.0)
                }
            };
            (item.text.as_str(), item.color.bits(), item.small, item.icon, item.logo, width.to_bits(), text_x.to_bits()).hash(&mut h);
            item.image.as_ref().map(|i| i.id).hash(&mut h);
            item.drawn.map(|d| match d {
                Drawn::Gauge(level, fill) => (1u8, level, fill.bits()),
                Drawn::Rule => (2u8, 0, 0),
            })
            .hash(&mut h);
            items.push(Placed { x, width, text_x, text_height, layout });
            x += width;
        }
        let width = (x + theme.pill_padding).ceil();
        Ok(Measured { key: key.to_string(), pill, width, items, text_sig: h.finish() })
    }
}

/// The Windows logo's four squares, `size` DIPs on a side, centered in the box `x`, `y`, `w`, `h`
/// and lined up with the pixels at `scale`, so its edges and the gaps between the squares stay
/// sharp.
pub fn logo(x: f32, y: f32, w: f32, h: f32, size: f32, scale: f32) -> [Rect; 4] {
    let side = (size * scale).round().max(3.0);
    let gap = (side / 12.0).round().max(1.0);
    let square = ((side - gap) / 2.0).floor();
    let side = 2.0 * square + gap;
    let left = ((x + (w - side / scale) / 2.0) * scale).round();
    let top = ((y + (h - side / scale) / 2.0) * scale).round();
    let at = |col: f32, row: f32| Rect::new((left + col * (square + gap)) / scale, (top + row * (square + gap)) / scale, square / scale, square / scale);
    [at(0.0, 0.0), at(1.0, 0.0), at(0.0, 1.0), at(1.0, 1.0)]
}

/// Where a drawn item goes in the box `x`, `y`, `w`, `h` (its room in a pill), lined up with the
/// pixels at `scale`: the track (or the rule) and its corner radius, and a gauge's fill. A gauge
/// is a bar 0.6 of the font's size tall; a rule one pixel wide and 1.1 of it tall.
pub fn drawn(drawn: Drawn, x: f32, y: f32, w: f32, h: f32, size: f32, scale: f32) -> (Rect, f32, Option<Rect>) {
    let px = |v: f32| (v * scale).round();
    match drawn {
        Drawn::Gauge(level, _) => {
            let height = px(size * 0.6).max(4.0);
            let width = px(w).max(height);
            let left = px(x);
            let top = px(y + (h - height / scale) / 2.0);
            let filled = if level == 0 { 0.0 } else { (width * level as f32 / 100.0).round().max(height.min(width)) };
            let track = Rect::new(left / scale, top / scale, width / scale, height / scale);
            let fill = (filled > 0.0).then(|| Rect::new(left / scale, top / scale, filled / scale, height / scale));
            (track, height / 2.0 / scale, fill)
        }
        Drawn::Rule => {
            let height = px(size * 1.1);
            let top = px(y + (h - height / scale) / 2.0);
            (Rect::new(px(x + w / 2.0) / scale, top / scale, 1.0 / scale, height / scale), 0.0, None)
        }
    }
}

// ---- popups ------------------------------------------------------------------------------------
//
// A popup is a scene: the shapes a widget lays out in DIPs (filled rounded rectangles and text),
// and the zones that can be clicked, each with the name of its action. The widget makes it again
// when its state changes; the popup draws it only if it differs from the one on screen.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular,
    Medium,
    SemiBold,
}

impl Weight {
    fn dwrite(self) -> DWRITE_FONT_WEIGHT {
        match self {
            Weight::Regular => DWRITE_FONT_WEIGHT_REGULAR,
            Weight::Medium => DWRITE_FONT_WEIGHT_MEDIUM,
            Weight::SemiBold => DWRITE_FONT_WEIGHT_SEMI_BOLD,
        }
    }
}

/// The theme's font at a size and weight, or Windows' icon font (Segoe Fluent Icons).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Font {
    pub size: f32,
    pub weight: Weight,
    pub icons: bool,
}

impl Font {
    pub fn new(size: f32, weight: Weight) -> Font {
        Font { size, weight, icons: false }
    }

    pub fn icons(size: f32) -> Font {
        Font { size, weight: Weight::Regular, icons: true }
    }
}

/// Where text goes across its rectangle; up and down it's always centered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Align {
    Start,
    Center,
    End,
}

/// A rectangle in DIPs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    fn hash_into(&self, h: &mut DefaultHasher) {
        (self.x.to_bits(), self.y.to_bits(), self.w.to_bits(), self.h.to_bits()).hash(h);
    }
}

/// A picture for a popup (a cover), decoded once: premultiplied BGRA, 4 bytes a pixel, row after
/// row. `id` tells two pictures apart in a scene's signature without comparing the pixels.
pub struct Image {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Image {} ({}×{})", self.id, self.width, self.height)
    }
}

impl PartialEq for Image {
    fn eq(&self, other: &Image) -> bool {
        self.id == other.id
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Fill { rect: Rect, radius: f32, color: Rgba },
    Text { rect: Rect, text: String, font: Font, color: Rgba, align: Align },
    /// A picture stretched over the rectangle, with its corners rounded.
    Image { rect: Rect, radius: f32, image: Arc<Image> },
    /// Text that wraps at the rectangle's width, from its top: a description, a paragraph.
    Para { rect: Rect, text: String, font: Font, color: Rgba },
}

/// A part of the popup that can be clicked: under the mouse it gets the hover tint, in its shape.
/// A slider gets no tint: a press on it, and a drag after the press, say where along it the
/// mouse is (`Input::Slide`).
#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub rect: Rect,
    pub radius: f32,
    pub action: String,
    pub slider: bool,
}

impl Zone {
    /// Where `x` is along the zone, 0 to 1.
    pub fn fraction(&self, x: f32) -> f32 {
        if self.rect.w <= 0.0 { 0.0 } else { ((x - self.rect.x) / self.rect.w).clamp(0.0, 1.0) }
    }
}

/// A popup's own colors instead of the theme's (a player in its cover's colors): its background,
/// its border, and the text color the hover tint is made of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Backdrop {
    pub base: Rgba,
    pub border: Rgba,
    pub text: Rgba,
}

impl Backdrop {
    pub fn of(theme: &Theme) -> Backdrop {
        Backdrop { base: theme.base, border: theme.surface, text: theme.text }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub width: f32,
    pub height: f32,
    pub shapes: Vec<Shape>,
    pub zones: Vec<Zone>,
    /// None: the theme's.
    pub backdrop: Option<Backdrop>,
}

impl Scene {
    pub fn new(width: f32, height: f32) -> Scene {
        Scene { width, height, shapes: Vec::new(), zones: Vec::new(), backdrop: None }
    }

    pub fn fill(&mut self, rect: Rect, radius: f32, color: Rgba) {
        self.shapes.push(Shape::Fill { rect, radius, color });
    }

    pub fn text(&mut self, rect: Rect, text: impl Into<String>, font: Font, color: Rgba, align: Align) {
        self.shapes.push(Shape::Text { rect, text: text.into(), font, color, align });
    }

    pub fn image(&mut self, rect: Rect, radius: f32, image: Arc<Image>) {
        self.shapes.push(Shape::Image { rect, radius, image });
    }

    /// Text that wraps at `rect`'s width, from its top (see `Fonts::wrapped` for its height).
    pub fn para(&mut self, rect: Rect, text: impl Into<String>, font: Font, color: Rgba) {
        self.shapes.push(Shape::Para { rect, text: text.into(), font, color });
    }

    pub fn zone(&mut self, rect: Rect, radius: f32, action: impl Into<String>) {
        self.zones.push(Zone { rect, radius, action: action.into(), slider: false });
    }

    /// A zone to press and drag along: a volume, a position.
    pub fn slider(&mut self, rect: Rect, action: impl Into<String>) {
        self.zones.push(Zone { rect, radius: 0.0, action: action.into(), slider: true });
    }

    /// The shapes and zones of `other`, moved by `dx`, `dy`, on top of these: a part laid out on
    /// its own (to know its height first) put in its place.
    pub fn append(&mut self, other: Scene, dx: f32, dy: f32) {
        let moved = |r: Rect| Rect::new(r.x + dx, r.y + dy, r.w, r.h);
        for shape in other.shapes {
            self.shapes.push(match shape {
                Shape::Fill { rect, radius, color } => Shape::Fill { rect: moved(rect), radius, color },
                Shape::Text { rect, text, font, color, align } => Shape::Text { rect: moved(rect), text, font, color, align },
                Shape::Image { rect, radius, image } => Shape::Image { rect: moved(rect), radius, image },
                Shape::Para { rect, text, font, color } => Shape::Para { rect: moved(rect), text, font, color },
            });
        }
        for zone in other.zones {
            self.zones.push(Zone { rect: moved(zone.rect), ..zone });
        }
    }

    /// The zone under a point, the last one added first.
    pub fn zone_at(&self, x: f32, y: f32) -> Option<usize> {
        self.zones.iter().rposition(|z| z.rect.contains(x, y))
    }

    /// What the scene draws, as a number: two scenes that draw the same have the same.
    pub fn signature(&self) -> u64 {
        let mut h = DefaultHasher::new();
        (self.width.to_bits(), self.height.to_bits()).hash(&mut h);
        self.backdrop.map(|b| (b.base.bits(), b.border.bits(), b.text.bits())).hash(&mut h);
        for shape in &self.shapes {
            match shape {
                Shape::Fill { rect, radius, color } => {
                    0u8.hash(&mut h);
                    rect.hash_into(&mut h);
                    (radius.to_bits(), color.bits()).hash(&mut h);
                }
                Shape::Text { rect, text, font, color, align } => {
                    1u8.hash(&mut h);
                    rect.hash_into(&mut h);
                    (text, font.size.to_bits(), font.weight, font.icons, color.bits(), align).hash(&mut h);
                }
                Shape::Image { rect, radius, image } => {
                    2u8.hash(&mut h);
                    rect.hash_into(&mut h);
                    (radius.to_bits(), image.id).hash(&mut h);
                }
                Shape::Para { rect, text, font, color } => {
                    3u8.hash(&mut h);
                    rect.hash_into(&mut h);
                    (text, font.size.to_bits(), font.weight, font.icons, color.bits()).hash(&mut h);
                }
            }
        }
        h.finish()
    }
}

/// The fonts of one popup, made as it needs them and dropped with it.
pub struct Fonts {
    factory: IDWriteFactory,
    family: HSTRING,
    icons: HSTRING,
    /// Each font's format, and the ellipsis that ends its text cut short.
    formats: RefCell<Vec<(Font, IDWriteTextFormat, Option<IDWriteInlineObject>)>>,
}

impl Fonts {
    pub fn new(theme: &Theme) -> Result<Fonts> {
        let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let icons = icon_family(&factory);
        Ok(Fonts { factory, family: HSTRING::from(theme.font.as_str()), icons: HSTRING::from(icons), formats: RefCell::new(Vec::new()) })
    }

    fn format(&self, font: Font) -> Result<(IDWriteTextFormat, Option<IDWriteInlineObject>)> {
        if let Some((_, f, sign)) = self.formats.borrow().iter().find(|(k, _, _)| *k == font) {
            return Ok((f.clone(), sign.clone()));
        }
        let family = if font.icons { &self.icons } else { &self.family };
        let format = unsafe {
            let f = self.factory.CreateTextFormat(
                family,
                None,
                font.weight.dwrite(),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                font.size,
                &HSTRING::new(),
            )?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            f
        };
        // Icons are never cut: no ellipsis for them
        let sign = if font.icons { None } else { unsafe { self.factory.CreateEllipsisTrimmingSign(&format) }.ok() };
        self.formats.borrow_mut().push((font, format.clone(), sign.clone()));
        Ok((format, sign))
    }

    /// `text` laid out on one line, cut at `max_width` with an ellipsis, with its width and height.
    pub fn layout(&self, text: &str, font: Font, max_width: f32) -> Result<(IDWriteTextLayout, f32, f32)> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let (format, sign) = self.format(font)?;
        unsafe {
            let layout = self.factory.CreateTextLayout(&wide, &format, max_width.max(1.0), 1_000.0)?;
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            layout.SetTrimming(&trimming, sign.as_ref())?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            Ok((layout, m.widthIncludingTrailingWhitespace.min(max_width), m.height))
        }
    }

    /// `text` laid out to wrap at `width`, with its height.
    pub fn wrapped(&self, text: &str, font: Font, width: f32) -> Result<(IDWriteTextLayout, f32)> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let (format, _) = self.format(font)?;
        unsafe {
            let layout = self.factory.CreateTextLayout(&wide, &format, width.max(1.0), 100_000.0)?;
            layout.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            Ok((layout, m.height))
        }
    }

    /// How tall `text` is when it wraps at `width`, for laying out a scene.
    pub fn height(&self, text: &str, font: Font, width: f32) -> f32 {
        self.wrapped(text, font, width).map(|(_, h)| h).unwrap_or(0.0)
    }

    /// How wide `text` is, for laying out a scene.
    pub fn width(&self, text: &str, font: Font) -> f32 {
        self.layout(text, font, 10_000.0).map(|(_, w, _)| w).unwrap_or(0.0)
    }
}

/// Windows 11's icon font, or Windows 10's: the same code points for the same icons.
fn icon_family(factory: &IDWriteFactory) -> &'static str {
    if has_family(factory, "Segoe Fluent Icons") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" }
}

fn has_family(factory: &IDWriteFactory, name: &str) -> bool {
    unsafe {
        let mut collection = None;
        if factory.GetSystemFontCollection(&mut collection, false).is_err() {
            return false;
        }
        let Some(collection) = collection else { return false };
        let (mut index, mut exists) = (0u32, BOOL(0));
        collection.FindFamilyName(&HSTRING::from(name), &mut index, &mut exists).is_ok() && exists.as_bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four equal squares on whole pixels, a gap between them, centered in the box.
    #[test]
    fn the_windows_logo() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let [a, b, c, d] = logo(10.0, 0.0, 20.0, 28.0, 13.0, scale);
            for r in [a, b, c, d] {
                assert_eq!((r.w, r.w * scale), (a.w, (a.w * scale).round()), "{scale}");
                assert_eq!(r.x * scale, (r.x * scale).round());
                assert_eq!(r.y * scale, (r.y * scale).round());
            }
            assert!(b.x > a.x + a.w && c.y > a.y + a.h, "{scale}");
            assert_eq!((b.y, c.x, d.x, d.y), (a.y, a.x, b.x, c.y));
            let center = (a.x + d.x + d.w) / 2.0;
            assert!((center - 20.0).abs() <= 1.0 / scale, "{scale}: {center}");
        }
    }

    #[test]
    fn zones_and_signatures() {
        let mut a = Scene::new(100.0, 50.0);
        a.zone(Rect::new(0.0, 0.0, 50.0, 50.0), 4.0, "left");
        a.zone(Rect::new(40.0, 0.0, 60.0, 50.0), 4.0, "right");
        // The later zone wins where two overlap
        assert_eq!(a.zone_at(45.0, 10.0), Some(1));
        assert_eq!(a.zone_at(10.0, 10.0), Some(0));
        assert_eq!(a.zone_at(10.0, 60.0), None);
        let mut b = a.clone();
        assert_eq!(a.signature(), b.signature());
        b.fill(Rect::new(0.0, 0.0, 10.0, 10.0), 0.0, Rgba(1.0, 0.0, 0.0, 1.0));
        assert_ne!(a.signature(), b.signature());
    }
}
