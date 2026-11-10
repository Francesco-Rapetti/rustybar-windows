//! What a widget puts in the bar: a pill of text items, measured with DirectWrite. Sizes are in
//! DIPs, the same on every screen; each bar scales them to its own.

use std::hash::{DefaultHasher, Hash, Hasher};

use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_MEDIUM,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteInlineObject, IDWriteTextFormat,
    IDWriteTextLayout,
};
use windows::core::{HSTRING, Result};

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
