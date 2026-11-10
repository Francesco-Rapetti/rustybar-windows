//! What a widget puts in the bar: a pill of text items, measured with DirectWrite. Sizes are in
//! DIPs, the same on every screen; each bar scales them to its own.

use std::cell::RefCell;
use std::hash::{DefaultHasher, Hash, Hasher};

use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_FONT_WEIGHT_REGULAR, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory,
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
}

impl Item {
    pub fn text(text: impl Into<String>, color: Rgba) -> Item {
        Item { text: text.into(), color, small: false, max_width: None, min_width: 0.0 }
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
}

impl Pill {
    pub fn new(items: Vec<Item>) -> Pill {
        Pill { items, gap: 6.0, highlight: None }
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
pub struct Placed {
    pub x: f32,
    pub width: f32,
    pub text_width: f32,
    pub text_height: f32,
    pub layout: IDWriteTextLayout,
}

/// A pill measured: its width and its items, plus a signature of what it draws.
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
    ellipsis: IDWriteInlineObject,
    small_ellipsis: IDWriteInlineObject,
    font_sig: u64,
}

impl Text {
    pub fn new(theme: &Theme) -> Result<Text> {
        unsafe {
            let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let format = |weight, size: f32| -> Result<IDWriteTextFormat> {
                let f = factory.CreateTextFormat(
                    &HSTRING::from(theme.font.as_str()),
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
            let regular = format(DWRITE_FONT_WEIGHT_SEMI_BOLD, theme.font_size)?;
            let small = format(DWRITE_FONT_WEIGHT_MEDIUM, theme.small_size)?;
            let ellipsis = factory.CreateEllipsisTrimmingSign(&regular)?;
            let small_ellipsis = factory.CreateEllipsisTrimmingSign(&small)?;
            let mut h = DefaultHasher::new();
            (theme.font.as_str(), theme.font_size.to_bits(), theme.small_size.to_bits()).hash(&mut h);
            Ok(Text { factory, regular, small, ellipsis, small_ellipsis, font_sig: h.finish() })
        }
    }

    fn layout(&self, item: &Item) -> Result<(IDWriteTextLayout, f32, f32)> {
        let wide: Vec<u16> = item.text.encode_utf16().collect();
        let (format, sign) = if item.small { (&self.small, &self.small_ellipsis) } else { (&self.regular, &self.ellipsis) };
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

    /// Lays out a widget's pill: its items side by side, `gap` apart, inside the theme's padding.
    pub fn measure(&self, key: &str, pill: Pill, theme: &Theme) -> Result<Measured> {
        let mut x = theme.pill_padding;
        let mut items = Vec::with_capacity(pill.items.len());
        let mut h = DefaultHasher::new();
        self.font_sig.hash(&mut h);
        pill.highlight.hash(&mut h);
        for (i, item) in pill.items.iter().enumerate() {
            if i > 0 {
                x += pill.gap;
            }
            let (layout, text_width, text_height) = self.layout(item)?;
            let width = text_width.max(item.min_width);
            (item.text.as_str(), item.color.bits(), item.small, width.to_bits()).hash(&mut h);
            items.push(Placed { x, width, text_width, text_height, layout });
            x += width;
        }
        let width = (x + theme.pill_padding).ceil();
        Ok(Measured { key: key.to_string(), pill, width, items, text_sig: h.finish() })
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
    #[allow(dead_code)] // For the popups to come: numbers on the right of a row
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

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Fill { rect: Rect, radius: f32, color: Rgba },
    Text { rect: Rect, text: String, font: Font, color: Rgba, align: Align },
}

/// A part of the popup that can be clicked: under the mouse it gets the hover tint, in its shape.
#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub rect: Rect,
    pub radius: f32,
    pub action: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub width: f32,
    pub height: f32,
    pub shapes: Vec<Shape>,
    pub zones: Vec<Zone>,
}

impl Scene {
    pub fn new(width: f32, height: f32) -> Scene {
        Scene { width, height, shapes: Vec::new(), zones: Vec::new() }
    }

    pub fn fill(&mut self, rect: Rect, radius: f32, color: Rgba) {
        self.shapes.push(Shape::Fill { rect, radius, color });
    }

    pub fn text(&mut self, rect: Rect, text: impl Into<String>, font: Font, color: Rgba, align: Align) {
        self.shapes.push(Shape::Text { rect, text: text.into(), font, color, align });
    }

    pub fn zone(&mut self, rect: Rect, radius: f32, action: impl Into<String>) {
        self.zones.push(Zone { rect, radius, action: action.into() });
    }

    /// The zone under a point, the last one added first.
    pub fn zone_at(&self, x: f32, y: f32) -> Option<usize> {
        self.zones.iter().rposition(|z| z.rect.contains(x, y))
    }

    /// What the scene draws, as a number: two scenes that draw the same have the same.
    pub fn signature(&self) -> u64 {
        let mut h = DefaultHasher::new();
        (self.width.to_bits(), self.height.to_bits()).hash(&mut h);
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
    formats: RefCell<Vec<(Font, IDWriteTextFormat)>>,
}

impl Fonts {
    pub fn new(theme: &Theme) -> Result<Fonts> {
        let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        // Windows 11's icon font, or Windows 10's: the same code points for the same icons
        let icons = if has_family(&factory, "Segoe Fluent Icons") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" };
        Ok(Fonts { factory, family: HSTRING::from(theme.font.as_str()), icons: HSTRING::from(icons), formats: RefCell::new(Vec::new()) })
    }

    fn format(&self, font: Font) -> Result<IDWriteTextFormat> {
        if let Some((_, f)) = self.formats.borrow().iter().find(|(k, _)| *k == font) {
            return Ok(f.clone());
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
        self.formats.borrow_mut().push((font, format.clone()));
        Ok(format)
    }

    /// `text` laid out on one line, cut at `max_width`, with its width and height.
    pub fn layout(&self, text: &str, font: Font, max_width: f32) -> Result<(IDWriteTextLayout, f32, f32)> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let format = self.format(font)?;
        unsafe {
            let layout = self.factory.CreateTextLayout(&wide, &format, max_width.max(1.0), 1_000.0)?;
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            layout.SetTrimming(&trimming, None)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            Ok((layout, m.widthIncludingTrailingWhitespace.min(max_width), m.height))
        }
    }

    /// How wide `text` is, for laying out a scene.
    pub fn width(&self, text: &str, font: Font) -> f32 {
        self.layout(text, font, 10_000.0).map(|(_, w, _)| w).unwrap_or(0.0)
    }
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
