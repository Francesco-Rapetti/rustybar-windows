//! The pages of the settings window are built anew from the config every time something changes,
//! as a `Page`: a `ui::Scene` (what to draw, in DIPs) with zones whose action is the index of a
//! `Do` (what a click there does). A `Form` lays them out in the style of Windows 11's Settings:
//! a title, cards of rows with a title, a description and a control on the right, groups with a
//! title of their own. The controls are drawn here; what they do is a closure that writes the
//! config (see `store`), after which the window builds the page again and draws it only if the
//! scene changed.

use std::cell::RefCell;
use std::rc::Rc;

use crate::i18n::t;
use crate::theme::{Rgba, Theme};
use crate::ui::{Align, Font, Fonts, Rect, Scene, Weight};

/// One of a menu's choices: its value, what it shows, and a color swatch or a font to show it in.
#[derive(Clone, Debug, PartialEq)]
pub struct Opt {
    pub value: String,
    pub label: String,
    pub swatch: Option<Rgba>,
    pub family: Option<String>,
}

impl Opt {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Opt {
        Opt { value: value.into(), label: label.into(), swatch: None, family: None }
    }

    pub fn swatch(mut self, color: Option<Rgba>) -> Opt {
        self.swatch = color;
        self
    }
}

/// The limits of a number: the smallest, the largest, the step of − and +, the decimals shown.
#[derive(Clone, Copy, Debug)]
pub struct Num {
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub decimals: u8,
}

impl Num {
    pub fn new(min: f64, max: f64, step: f64, decimals: u8) -> Num {
        Num { min, max, step, decimals }
    }

    pub fn clamp(&self, v: f64) -> f64 {
        let p = 10f64.powi(self.decimals as i32);
        ((v.clamp(self.min, self.max)) * p).round() / p
    }

    /// The number as the region writes it, without decimals when it has none: `13`, `12,5`.
    pub fn show(&self, v: f64) -> String {
        let digits = if (v - v.round()).abs() < 1e-9 { 0 } else { self.decimals as usize };
        crate::i18n::number(v, digits)
    }
}

/// What a zone does.
pub enum Do {
    /// Something to run at once (a switch, a button).
    Run(Rc<dyn Fn()>),
    /// A menu opens under the control: picking a choice runs `on` with its value.
    Menu { rect: Rect, options: Rc<Vec<Opt>>, current: String, on: Rc<dyn Fn(String)> },
    /// A text field: the window puts an edit box over `rect` (`secret`: its text hidden), and
    /// runs `on` with the text at Enter or when the box loses the keyboard.
    Field { rect: Rect, value: String, secret: bool, on: Rc<dyn Fn(String)> },
    /// A slider: a press and a drag move it (shown at once, under `key`), the release runs `on`.
    Slide { key: String, num: Num, on: Rc<dyn Fn(f64)> },
    /// A chip of the layout editor, that can be dragged: side and place.
    Chip { side: usize, index: usize },
}

/// What a page knows about the layout editor's three columns, for a drag of a chip.
pub struct Columns {
    /// Each column's area, and its chips' rectangles in order.
    pub areas: [Rect; 3],
    pub chips: [Vec<Rect>; 3],
    pub names: [Vec<String>; 3],
    /// A chip moved from (side, place) to (side, place): the place among the chips left.
    pub on_move: Rc<dyn Fn(usize, usize, usize, usize)>,
    /// A chip clicked without being dragged.
    pub on_open: Rc<dyn Fn(usize, usize)>,
}

pub struct Page {
    pub scene: Scene,
    pub dos: Vec<Do>,
    pub columns: Option<Columns>,
}

impl Page {
    /// The `Do` of a zone's action.
    pub fn action(&self, action: &str) -> Option<&Do> {
        action.parse::<usize>().ok().and_then(|i| self.dos.get(i))
    }
}

/// What a click asks of the window beyond the config: another page, a file or a page to open.
pub enum Request {
    Go(String),
    Open(String),
}

thread_local! {
    static REQUESTS: RefCell<Vec<Request>> = const { RefCell::new(Vec::new()) };
    /// A slider being dragged: its key and the value shown meanwhile.
    static PREVIEW: RefCell<Option<(String, f64)>> = const { RefCell::new(None) };
    /// A row waiting for its second click (a removal), by its key.
    static CONFIRM: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub fn go(page: &str) {
    REQUESTS.with(|r| r.borrow_mut().push(Request::Go(page.to_string())));
}

pub fn open(target: impl Into<String>) {
    REQUESTS.with(|r| r.borrow_mut().push(Request::Open(target.into())));
}

pub fn take_requests() -> Vec<Request> {
    REQUESTS.with(|r| std::mem::take(&mut *r.borrow_mut()))
}

pub fn set_preview(preview: Option<(String, f64)>) {
    PREVIEW.with(|p| *p.borrow_mut() = preview);
}

fn preview(key: &str) -> Option<f64> {
    PREVIEW.with(|p| p.borrow().as_ref().filter(|(k, _)| k == key).map(|(_, v)| *v))
}

/// Whether `key` is waiting for its second click; the first click sets it and returns false.
pub fn confirmed(key: &str) -> bool {
    CONFIRM.with(|c| {
        let mut c = c.borrow_mut();
        if c.as_deref() == Some(key) {
            *c = None;
            true
        } else {
            *c = Some(key.to_string());
            false
        }
    })
}

pub fn confirming(key: &str) -> bool {
    CONFIRM.with(|c| c.borrow().as_deref() == Some(key))
}

/// Forgets a click waiting for its second (another page, another click).
pub fn forget_confirm() {
    CONFIRM.with(|c| *c.borrow_mut() = None);
}

/// The window's colors, from the bar's theme: as Windows 11's Settings, cards a shade off the
/// background, controls a shade off the cards.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub light: bool,
    pub bg: Rgba,
    pub card: Rgba,
    pub card_border: Rgba,
    pub control: Rgba,
    pub control_border: Rgba,
    pub text: Rgba,
    pub sub: Rgba,
    pub faint: Rgba,
    pub accent: Rgba,
    pub on_accent: Rgba,
    pub red: Rgba,
    pub yellow: Rgba,
    pub green: Rgba,
    pub selected: Rgba,
}

fn luminance(c: Rgba) -> f32 {
    0.2126 * c.0 + 0.7152 * c.1 + 0.0722 * c.2
}

impl Look {
    pub fn of(theme: &Theme) -> Look {
        let light = luminance(theme.base) > 0.5;
        let white = Rgba(1.0, 1.0, 1.0, 1.0);
        let black = Rgba(0.0, 0.0, 0.0, 1.0);
        let bg = theme.base;
        let card = if light { bg.mix(white, 0.65) } else { bg.mix(theme.text, 0.05) };
        let control = if light { card.mix(white, 0.6) } else { card.mix(theme.text, 0.05) };
        let color = |n: &str, fallback: Rgba| theme.color(n).unwrap_or(fallback);
        Look {
            light,
            bg,
            card,
            card_border: if light { black.alpha(0.07) } else { black.alpha(0.22) },
            control,
            control_border: theme.text.alpha(if light { 0.14 } else { 0.09 }),
            text: theme.text,
            sub: theme.subtext,
            faint: theme.overlay,
            accent: theme.primary,
            on_accent: theme.on_primary,
            red: color("red", Rgba(0.9, 0.3, 0.3, 1.0)),
            yellow: color("yellow", Rgba(0.9, 0.7, 0.2, 1.0)),
            green: color("green", Rgba(0.4, 0.8, 0.4, 1.0)),
            selected: theme.text.alpha(if light { 0.06 } else { 0.07 }),
        }
    }
}

// Sizes, in DIPs, near those of Windows 11's Settings
pub const BODY: f32 = 14.0;
pub const CAPTION: f32 = 12.0;
pub const ROW_PAD: f32 = 16.0;
pub const ROW_GAP: f32 = 4.0;
pub const RADIUS: f32 = 6.0;
pub const CONTROL_H: f32 = 32.0;
pub const CONTROL_R: f32 = 4.0;

pub fn body() -> Font {
    Font::new(BODY, Weight::Regular)
}

pub fn strong() -> Font {
    Font::new(BODY, Weight::SemiBold)
}

pub fn caption() -> Font {
    Font::new(CAPTION, Weight::Regular)
}

pub fn icons(size: f32) -> Font {
    Font::icons(size)
}

/// A control on the right of a row.
pub enum Control {
    Toggle { on: bool, run: Rc<dyn Fn(bool)> },
    Number { value: f64, num: Num, run: Rc<dyn Fn(f64)> },
    Choice { options: Vec<Opt>, current: String, run: Rc<dyn Fn(String)> },
    Text { value: String, placeholder: String, width: f32, run: Rc<dyn Fn(String)> },
    /// A secret: whether one is saved; the field writes a new one, × removes it.
    Secret { saved: bool, run: Rc<dyn Fn(String)>, clear: Rc<dyn Fn()> },
    /// A color: a palette name or a hex value; `default` is what an empty one means.
    Color { value: String, default: String, run: Rc<dyn Fn(String)> },
    Slider { key: String, value: f64, num: Num, run: Rc<dyn Fn(f64)> },
    Button { label: String, accent: bool, run: Rc<dyn Fn()> },
    /// The whole row opens something: a chevron on the right.
    Link { run: Rc<dyn Fn()> },
    /// Text on the right, nothing to click (a version).
    Label(String),
    /// A square button with a glyph of Segoe Fluent Icons (a bin).
    Icon { glyph: &'static str, run: Rc<dyn Fn()> },
    /// Room as wide as an icon's button, empty: what keeps a row's controls lined up with the rows
    /// around it (an arrow a row doesn't have).
    Gap,
}

pub struct Form<'a> {
    pub fonts: &'a Fonts,
    pub look: Look,
    /// The bar's theme, for the colors a control shows.
    pub theme: &'a Theme,
    pub x: f32,
    pub width: f32,
    pub y: f32,
    /// Pixels per DIP: lines one pixel thick.
    pub scale: f32,
    scene: Scene,
    dos: Vec<Do>,
    columns: Option<Columns>,
}

impl<'a> Form<'a> {
    pub fn new(fonts: &'a Fonts, theme: &'a Theme, x: f32, width: f32, scale: f32) -> Form<'a> {
        Form { fonts, look: Look::of(theme), theme, x, width, y: 28.0, scale, scene: Scene::new(0.0, 0.0), dos: Vec::new(), columns: None }
    }

    pub fn finish(mut self) -> Page {
        self.scene.width = self.x * 2.0 + self.width;
        self.scene.height = self.y + 36.0;
        Page { scene: self.scene, dos: self.dos, columns: self.columns }
    }

    /// A thing to do, by the action its zone gets.
    pub fn add(&mut self, d: Do) -> String {
        self.dos.push(d);
        (self.dos.len() - 1).to_string()
    }

    pub fn scene(&mut self) -> &mut Scene {
        &mut self.scene
    }

    pub fn set_columns(&mut self, columns: Columns) {
        self.columns = Some(columns);
    }

    /// One pixel, in DIPs.
    pub fn px(&self) -> f32 {
        1.0 / self.scale
    }

    pub fn title(&mut self, text: &str) {
        let r = Rect::new(self.x, self.y, self.width, 40.0);
        self.scene.text(r, text, Font::new(28.0, Weight::SemiBold), self.look.text, Align::Start);
        self.y += 48.0;
    }

    /// A paragraph under the title.
    pub fn intro(&mut self, text: &str) {
        let h = self.fonts.height(text, body(), self.width);
        self.scene.para(Rect::new(self.x, self.y, self.width, h), text, body(), self.look.sub);
        self.y += h + 20.0;
    }

    /// A strip that says something is wrong (red) or worth knowing (yellow), at the top.
    pub fn banner(&mut self, text: &str, error: bool) {
        let color = if error { self.look.red } else { self.look.yellow };
        let icon = if error { "\u{E783}" } else { "\u{E7BA}" };
        let w = self.width - 2.0 * ROW_PAD - 28.0;
        let h = self.fonts.height(text, body(), w).max(20.0);
        let rect = Rect::new(self.x, self.y, self.width, h + 24.0);
        self.scene.fill(rect, RADIUS, color.alpha(if self.look.light { 0.14 } else { 0.18 }));
        self.scene.text(Rect::new(self.x + ROW_PAD, self.y + 12.0, 20.0, 20.0), icon, icons(16.0), color, Align::Start);
        self.scene.para(Rect::new(self.x + ROW_PAD + 28.0, self.y + 12.0, w, h), text, body(), self.look.text);
        self.y += rect.h + 12.0;
    }

    /// The start of a group of rows, with its title.
    pub fn group(&mut self, title: Option<&str>) {
        self.y += 8.0;
        if let Some(title) = title {
            self.scene.text(Rect::new(self.x + 2.0, self.y, self.width, 24.0), title, strong(), self.look.text, Align::Start);
            self.y += 30.0;
        }
    }

    /// Text under a group, in small letters.
    pub fn footer(&mut self, text: &str) {
        let h = self.fonts.height(text, caption(), self.width - 4.0);
        self.scene.para(Rect::new(self.x + 2.0, self.y + 2.0, self.width - 4.0, h), text, caption(), self.look.sub);
        self.y += h + 14.0;
    }

    /// A card's background, `h` tall from `y`: a fill inside a one-pixel border.
    pub fn card(&mut self, y: f32, h: f32) {
        let r = Rect::new(self.x, y, self.width, h);
        let px = self.px();
        card(&mut self.scene, &self.look, r, px);
    }

    /// A row: its title and description on the left, the control on the right.
    pub fn row(&mut self, title: &str, desc: Option<&str>, control: Control) {
        self.row_icon(None, title, desc, control);
    }

    /// A row with an icon before its title (Segoe Fluent Icons).
    pub fn row_icon(&mut self, icon: Option<&str>, title: &str, desc: Option<&str>, control: Control) {
        self.row_controls(icon, title, desc, vec![control]);
    }

    /// A row with more controls side by side on the right (a value and a × that removes it).
    pub fn row_controls(&mut self, icon: Option<&str>, title: &str, desc: Option<&str>, controls: Vec<Control>) {
        let widths: Vec<f32> = controls.iter().map(|c| self.control_width(c)).collect();
        let control_w = widths.iter().sum::<f32>() + 8.0 * widths.len().saturating_sub(1) as f32;
        let text_x = self.x + ROW_PAD + if icon.is_some() { 36.0 } else { 0.0 };
        let text_w = (self.x + self.width - ROW_PAD - if control_w > 0.0 { control_w + 24.0 } else { 0.0 } - text_x).max(80.0);
        let desc_h = desc.map(|d| self.fonts.height(d, caption(), text_w)).unwrap_or(0.0);
        let content = 20.0 + if desc.is_some() { 2.0 + desc_h } else { 0.0 };
        let h = (content + 26.0).max(if desc.is_some() { 64.0 } else { 52.0 });
        let y = self.y;
        self.card(y, h);
        if let Some(Control::Link { run }) = controls.first() {
            let action = self.add(Do::Run(run.clone()));
            self.scene.zone(Rect::new(self.x, y, self.width, h), RADIUS, action);
        }
        if let Some(icon) = icon {
            self.scene.text(Rect::new(self.x + ROW_PAD, y, 24.0, h), icon, icons(18.0), self.look.text, Align::Start);
        }
        let top = y + (h - content) / 2.0;
        self.scene.text(Rect::new(text_x, top, text_w, 20.0), title, body(), self.look.text, Align::Start);
        if let Some(d) = desc {
            self.scene.para(Rect::new(text_x, top + 22.0, text_w, desc_h), d, caption(), self.look.sub);
        }
        let mut cx = self.x + self.width - ROW_PAD - control_w;
        let cy = y + (h - CONTROL_H) / 2.0;
        for (control, w) in controls.into_iter().zip(widths) {
            self.control(control, cx, cy, w);
            cx += w + 8.0;
        }
        self.y += h + ROW_GAP;
    }

    /// A row whose controls go under its title, left to right, on more lines if they need them:
    /// for a thing with many parts (a calendar: its name, color, address and bin).
    pub fn row_stacked(&mut self, icon: Option<&str>, title: &str, desc: Option<&str>, controls: Vec<Control>) {
        let text_x = self.x + ROW_PAD + if icon.is_some() { 36.0 } else { 0.0 };
        let right = self.x + self.width - ROW_PAD;
        let text_w = (right - text_x).max(80.0);
        let desc_h = desc.map(|d| self.fonts.height(d, caption(), text_w)).unwrap_or(0.0);
        let head = 20.0 + if desc.is_some() { 2.0 + desc_h } else { 0.0 };
        // Where each control goes, from the left, on as many lines as it takes
        let mut places = Vec::new();
        let (mut cx, mut line) = (text_x, 0usize);
        for control in &controls {
            let w = self.control_width(control);
            if cx + w > right && cx > text_x {
                cx = text_x;
                line += 1;
            }
            places.push((cx, line, w));
            cx += w + 8.0;
        }
        let lines = if controls.is_empty() { 0 } else { line + 1 };
        let h = 14.0 + head + lines as f32 * (CONTROL_H + 8.0) + 8.0;
        let y = self.y;
        self.card(y, h);
        if let Some(icon) = icon {
            self.scene.text(Rect::new(self.x + ROW_PAD, y + 14.0, 24.0, 20.0), icon, icons(18.0), self.look.text, Align::Start);
        }
        self.scene.text(Rect::new(text_x, y + 14.0, text_w, 20.0), title, body(), self.look.text, Align::Start);
        if let Some(d) = desc {
            self.scene.para(Rect::new(text_x, y + 14.0 + 22.0, text_w, desc_h), d, caption(), self.look.sub);
        }
        let top = y + 14.0 + head + 10.0;
        for (control, (cx, line, w)) in controls.into_iter().zip(places) {
            self.control(control, cx, top + line as f32 * (CONTROL_H + 8.0), w);
        }
        self.y += h + ROW_GAP;
    }

    /// How wide a control is.
    pub fn control_width(&self, control: &Control) -> f32 {
        let f = self.fonts;
        match control {
            Control::Toggle { .. } => 40.0 + 12.0 + f.width(t("Disattivato", "Off"), body()).max(f.width(t("Attivato", "On"), body())).ceil(),
            Control::Number { .. } => 32.0 * 2.0 + 72.0 + 8.0,
            Control::Choice { options, .. } => choice_width(f, options),
            Control::Text { width, .. } => *width,
            Control::Secret { saved, .. } => 220.0 + if *saved { 40.0 } else { 0.0 },
            Control::Color { .. } => 32.0 + 8.0 + 150.0 + 8.0 + 100.0,
            Control::Slider { .. } => 200.0 + 12.0 + 48.0,
            Control::Button { label, .. } => button_width(f, label),
            Control::Link { .. } => 16.0,
            Control::Label(text) => f.width(text, body()).ceil().min(360.0),
            Control::Icon { .. } | Control::Gap => CONTROL_H,
        }
    }

    /// Draws a control `w` wide at `x`, `y` (its top, `CONTROL_H` tall), with its zones.
    pub fn control(&mut self, control: Control, x: f32, y: f32, w: f32) {
        let look = self.look;
        let px = self.px();
        match control {
            Control::Toggle { on, run } => {
                let label = if on { t("Attivato", "On") } else { t("Disattivato", "Off") };
                let track = Rect::new(x + w - 40.0, y + 6.0, 40.0, 20.0);
                self.scene.text(Rect::new(x, y, w - 52.0, CONTROL_H), label, body(), look.text, Align::End);
                toggle(&mut self.scene, &look, track, on, px);
                let action = self.add(Do::Run(Rc::new(move || run(!on))));
                self.scene.zone(Rect::new(x, y, w, CONTROL_H), CONTROL_R, action);
            }
            Control::Number { value, num, run } => {
                let minus = Rect::new(x, y, 32.0, CONTROL_H);
                let field = Rect::new(x + 36.0, y, w - 72.0, CONTROL_H);
                let plus = Rect::new(x + w - 32.0, y, 32.0, CONTROL_H);
                for (r, glyph, enabled, next) in
                    [(minus, "\u{E738}", value > num.min + 1e-9, num.clamp(value - num.step)), (plus, "\u{E710}", value < num.max - 1e-9, num.clamp(value + num.step))]
                {
                    button_box(&mut self.scene, &look, r, px);
                    self.scene.text(r, glyph, icons(12.0), if enabled { look.text } else { look.faint }, Align::Center);
                    if enabled {
                        let run = run.clone();
                        let action = self.add(Do::Run(Rc::new(move || run(next))));
                        self.scene.zone(r, CONTROL_R, action);
                    }
                }
                field_box(&mut self.scene, &look, field, px);
                self.scene.text(field, num.show(value), body(), look.text, Align::Center);
                let action = self.add(Do::Field {
                    rect: field,
                    value: num.show(value),
                    secret: false,
                    on: Rc::new(move |s: String| {
                        if let Some(v) = parse_number(&s) {
                            run(num.clamp(v));
                        }
                    }),
                });
                self.scene.zone(field, CONTROL_R, action);
            }
            Control::Choice { options, current, run } => {
                let r = Rect::new(x, y, w, CONTROL_H);
                self.choice_button(r, &options, &current);
                let action = self.add(Do::Menu { rect: r, options: Rc::new(options), current, on: run });
                self.scene.zone(r, CONTROL_R, action);
            }
            Control::Text { value, placeholder, width: _, run } => {
                let r = Rect::new(x, y, w, CONTROL_H);
                field_box(&mut self.scene, &look, r, px);
                let inner = Rect::new(r.x + 10.0, r.y, r.w - 20.0, r.h);
                if value.is_empty() {
                    self.scene.text(inner, placeholder, body(), look.faint, Align::Start);
                } else {
                    self.scene.text(inner, value.clone(), body(), look.text, Align::Start);
                }
                let action = self.add(Do::Field { rect: r, value, secret: false, on: run });
                self.scene.zone(r, CONTROL_R, action);
            }
            Control::Secret { saved, run, clear } => {
                let field_w = if saved { w - 40.0 } else { w };
                let r = Rect::new(x, y, field_w, CONTROL_H);
                field_box(&mut self.scene, &look, r, px);
                let inner = Rect::new(r.x + 10.0, r.y, r.w - 20.0, r.h);
                if saved {
                    self.scene.text(inner, "••••••••", body(), look.text, Align::Start);
                    self.scene.text(inner, t("Salvato", "Saved"), caption(), look.green, Align::End);
                } else {
                    self.scene.text(inner, t("Non impostato", "Not set"), body(), look.faint, Align::Start);
                }
                let action = self.add(Do::Field { rect: r, value: String::new(), secret: true, on: run });
                self.scene.zone(r, CONTROL_R, action);
                if saved {
                    let b = Rect::new(x + w - 32.0, y, 32.0, CONTROL_H);
                    button_box(&mut self.scene, &look, b, px);
                    // × takes the secret away (a bin is for the thing it belongs to)
                    self.scene.text(b, "\u{E711}", icons(11.0), look.text, Align::Center);
                    let action = self.add(Do::Run(clear));
                    self.scene.zone(b, CONTROL_R, action);
                }
            }
            Control::Color { value, default, run } => {
                let shown = if value.is_empty() { default.clone() } else { value.clone() };
                let color = self.theme.color(&shown);
                let swatch = Rect::new(x, y, 32.0, CONTROL_H);
                button_box(&mut self.scene, &look, swatch, px);
                match color {
                    Some(c) => self.scene.fill(Rect::new(swatch.x + 6.0, swatch.y + 6.0, 20.0, 20.0), 3.0, c),
                    None => self.scene.text(swatch, "?", body(), look.red, Align::Center),
                }
                let named = !shown.starts_with('#');
                let mut options: Vec<Opt> = crate::theme::NAMES
                    .iter()
                    .chain(["accent"].iter())
                    .map(|n| Opt::new(*n, *n).swatch(self.theme.color(n)))
                    .collect();
                options.insert(0, Opt::new("", t("Predefinito", "Default")).swatch(self.theme.color(&default)));
                let menu = Rect::new(x + 40.0, y, 150.0, CONTROL_H);
                let label_current = if value.is_empty() { String::new() } else if named { value.clone() } else { "#".into() };
                if !named {
                    options.push(Opt::new("#", t("Esadecimale", "Hex")));
                }
                self.choice_button(menu, &options, &label_current);
                let run2 = run.clone();
                let action = self.add(Do::Menu { rect: menu, options: Rc::new(options), current: label_current, on: Rc::new(move |v: String| if v != "#" { run2(v) }) });
                self.scene.zone(menu, CONTROL_R, action);
                let hex = Rect::new(x + w - 100.0, y, 100.0, CONTROL_H);
                field_box(&mut self.scene, &look, hex, px);
                let hex_text = color.map(crate::lua::hex).unwrap_or_default();
                self.scene.text(Rect::new(hex.x + 10.0, hex.y, hex.w - 20.0, hex.h), hex_text.clone(), body(), if named { look.sub } else { look.text }, Align::Start);
                let action = self.add(Do::Field {
                    rect: hex,
                    value: hex_text,
                    secret: false,
                    on: Rc::new(move |s: String| {
                        let s = s.trim();
                        let s = if s.starts_with('#') { s.to_string() } else { format!("#{s}") };
                        if valid_hex(&s) {
                            run(s.to_lowercase());
                        }
                    }),
                });
                self.scene.zone(hex, CONTROL_R, action);
            }
            Control::Slider { key, value, num, run } => {
                let value = preview(&key).unwrap_or(value);
                let track = Rect::new(x, y, 200.0, CONTROL_H);
                slider(&mut self.scene, &look, track, ((value - num.min) / (num.max - num.min).max(1e-9)) as f32);
                self.scene.text(Rect::new(x + 212.0, y, w - 212.0, CONTROL_H), num.show(value), body(), look.text, Align::End);
                let action = self.add(Do::Slide { key, num, on: run });
                // As wide as the thumb on either end, so its center reaches both
                self.scene.slider(Rect::new(track.x + 10.0, track.y, track.w - 20.0, track.h), action);
            }
            Control::Button { label, accent, run } => {
                let r = Rect::new(x, y, w, CONTROL_H);
                if accent {
                    self.scene.fill(r, CONTROL_R, look.accent);
                    self.scene.text(r, label, body(), look.on_accent, Align::Center);
                } else {
                    button_box(&mut self.scene, &look, r, px);
                    self.scene.text(r, label, body(), look.text, Align::Center);
                }
                let action = self.add(Do::Run(run));
                self.scene.zone(r, CONTROL_R, action);
            }
            Control::Link { .. } => {
                self.scene.text(Rect::new(x, y, w, CONTROL_H), "\u{E76C}", icons(12.0), look.sub, Align::End);
            }
            Control::Label(text) => {
                self.scene.text(Rect::new(x, y, w, CONTROL_H), text, body(), look.sub, Align::End);
            }
            Control::Gap => {}
            Control::Icon { glyph, run } => {
                let r = Rect::new(x, y, CONTROL_H, CONTROL_H);
                button_box(&mut self.scene, &look, r, px);
                self.scene.text(r, glyph, icons(14.0), look.text, Align::Center);
                let action = self.add(Do::Run(run));
                self.scene.zone(r, CONTROL_R, action);
            }
        }
    }

    /// A menu's button: the current choice's swatch and label, a chevron.
    pub fn choice_button(&mut self, r: Rect, options: &[Opt], current: &str) {
        let look = self.look;
        let px = self.px();
        button_box(&mut self.scene, &look, r, px);
        let chosen = options.iter().find(|o| o.value == current);
        let mut tx = r.x + 12.0;
        if let Some(c) = chosen.and_then(|o| o.swatch) {
            self.scene.fill(Rect::new(tx, r.y + (r.h - 14.0) / 2.0, 14.0, 14.0), 7.0, c);
            tx += 22.0;
        }
        let label = chosen.map(|o| o.label.clone()).unwrap_or_else(|| current.to_string());
        self.scene.text(Rect::new(tx, r.y, r.x + r.w - 34.0 - tx, r.h), label, body(), look.text, Align::Start);
        self.scene.text(Rect::new(r.x + r.w - 30.0, r.y, 18.0, r.h), "\u{E70D}", icons(12.0), look.sub, Align::End);
    }

    /// A list of words (executables, names): a row with a field that adds one, the words as
    /// chips with × under it, in the same card.
    pub fn tokens(&mut self, title: &str, desc: Option<&str>, items: &[String], placeholder: &str, add: Rc<dyn Fn(String)>, remove: Rc<dyn Fn(usize)>) {
        let look = self.look;
        let px = self.px();
        let field_w = 240.0;
        let text_x = self.x + ROW_PAD;
        let text_w = self.width - 2.0 * ROW_PAD - field_w - 24.0;
        let desc_h = desc.map(|d| self.fonts.height(d, caption(), text_w)).unwrap_or(0.0);
        let content = 20.0 + if desc.is_some() { 2.0 + desc_h } else { 0.0 };
        let head = (content + 26.0).max(if desc.is_some() { 64.0 } else { 52.0 });
        // The chips, laid out first to know how tall the card is
        let mut chips: Vec<(Rect, String)> = Vec::new();
        let (mut cx, mut cy) = (text_x, 0.0);
        let chip_h = 28.0;
        for item in items {
            let w = self.fonts.width(item, body()).ceil() + 12.0 + 28.0;
            if cx + w > self.x + self.width - ROW_PAD && cx > text_x {
                cx = text_x;
                cy += chip_h + 6.0;
            }
            chips.push((Rect::new(cx, cy, w, chip_h), item.clone()));
            cx += w + 6.0;
        }
        let body_h = if items.is_empty() { 0.0 } else { cy + chip_h + 14.0 };
        let y = self.y;
        self.card(y, head + body_h);
        let top = y + (head - content) / 2.0;
        self.scene.text(Rect::new(text_x, top, text_w, 20.0), title, body(), look.text, Align::Start);
        if let Some(d) = desc {
            self.scene.para(Rect::new(text_x, top + 22.0, text_w, desc_h), d, caption(), look.sub);
        }
        let field = Rect::new(self.x + self.width - ROW_PAD - field_w, y + (head - CONTROL_H) / 2.0, field_w, CONTROL_H);
        field_box(&mut self.scene, &look, field, px);
        self.scene.text(Rect::new(field.x + 10.0, field.y, field.w - 40.0, field.h), placeholder, body(), look.faint, Align::Start);
        self.scene.text(Rect::new(field.x, field.y, field.w - 10.0, field.h), "\u{E710}", icons(12.0), look.sub, Align::End);
        let action = self.add(Do::Field { rect: field, value: String::new(), secret: false, on: add });
        self.scene.zone(field, CONTROL_R, action);
        for (i, (r, item)) in chips.into_iter().enumerate() {
            let r = Rect::new(r.x, y + head + r.y, r.w, r.h);
            button_box(&mut self.scene, &look, r, px);
            self.scene.text(Rect::new(r.x + 12.0, r.y, r.w - 40.0, r.h), item, body(), look.text, Align::Start);
            let x_box = Rect::new(r.x + r.w - 28.0, r.y + 2.0, 24.0, 24.0);
            self.scene.text(x_box, "\u{E711}", icons(10.0), look.sub, Align::Center);
            let remove = remove.clone();
            let action = self.add(Do::Run(Rc::new(move || remove(i))));
            self.scene.zone(x_box, CONTROL_R, action);
        }
        self.y += head + body_h + ROW_GAP;
    }
}

/// `#rrggbb` or `#rrggbbaa`.
pub fn valid_hex(s: &str) -> bool {
    s.strip_prefix('#').is_some_and(|h| (h.len() == 6 || h.len() == 8) && h.chars().all(|c| c.is_ascii_hexdigit()))
}

/// A number as typed: with a comma or a point.
pub fn parse_number(s: &str) -> Option<f64> {
    s.trim().replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite())
}

pub fn choice_width(f: &Fonts, options: &[Opt]) -> f32 {
    let widest = options.iter().map(|o| f.width(&o.label, body()) + if o.swatch.is_some() { 22.0 } else { 0.0 }).fold(0.0, f32::max);
    (widest + 12.0 + 34.0).ceil().clamp(160.0, 280.0)
}

pub fn button_width(f: &Fonts, label: &str) -> f32 {
    (f.width(label, body()) + 32.0).ceil().max(96.0)
}

/// A card: a fill inside a one-pixel border.
pub fn card(scene: &mut Scene, look: &Look, r: Rect, px: f32) {
    scene.fill(r, RADIUS, look.card_border);
    scene.fill(Rect::new(r.x + px, r.y + px, r.w - 2.0 * px, r.h - 2.0 * px), RADIUS - px, look.card);
}

/// A button's box, as Windows 11's: a fill a shade off the card, a faint border.
pub fn button_box(scene: &mut Scene, look: &Look, r: Rect, px: f32) {
    scene.fill(r, CONTROL_R, look.control_border);
    scene.fill(Rect::new(r.x + px, r.y + px, r.w - 2.0 * px, r.h - 2.0 * px), CONTROL_R - px, look.control);
}

/// A text field's box: a button's, with a darker line along its bottom.
pub fn field_box(scene: &mut Scene, look: &Look, r: Rect, px: f32) {
    button_box(scene, look, r, px);
    scene.fill(Rect::new(r.x + CONTROL_R, r.y + r.h - px, r.w - 2.0 * CONTROL_R, px), 0.0, look.text.alpha(0.45));
}

/// A switch, 40 × 20: on, filled with the accent and the knob on the right; off, an outline.
pub fn toggle(scene: &mut Scene, look: &Look, r: Rect, on: bool, px: f32) {
    let radius = r.h / 2.0;
    if on {
        scene.fill(r, radius, look.accent);
        let k = 12.0;
        scene.fill(Rect::new(r.x + r.w - 4.0 - k, r.y + (r.h - k) / 2.0, k, k), k / 2.0, look.on_accent);
    } else {
        scene.fill(r, radius, look.text.alpha(0.6));
        scene.fill(Rect::new(r.x + px, r.y + px, r.w - 2.0 * px, r.h - 2.0 * px), radius - px, look.card);
        let k = 12.0;
        scene.fill(Rect::new(r.x + 4.0, r.y + (r.h - k) / 2.0, k, k), k / 2.0, look.text.alpha(0.8));
    }
}

/// A slider across `r` at `fraction`: the track, its filled part in the accent, the thumb.
pub fn slider(scene: &mut Scene, look: &Look, r: Rect, fraction: f32) {
    let f = fraction.clamp(0.0, 1.0);
    let (x0, x1) = (r.x + 10.0, r.x + r.w - 10.0);
    let at = x0 + (x1 - x0) * f;
    let cy = r.y + r.h / 2.0;
    scene.fill(Rect::new(x0, cy - 2.0, x1 - x0, 4.0), 2.0, look.text.alpha(0.4));
    scene.fill(Rect::new(x0, cy - 2.0, at - x0, 4.0), 2.0, look.accent);
    scene.fill(Rect::new(at - 10.0, cy - 10.0, 20.0, 20.0), 10.0, look.control_border.alpha(0.5).mix(look.card, 0.0));
    scene.fill(Rect::new(at - 9.0, cy - 9.0, 18.0, 18.0), 9.0, look.control);
    scene.fill(Rect::new(at - 5.0, cy - 5.0, 10.0, 10.0), 5.0, look.accent);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        let n = Num::new(0.0, 10.0, 0.5, 1);
        assert_eq!(n.clamp(12.0), 10.0);
        assert_eq!(n.clamp(2.468), 2.5);
        assert_eq!(parse_number(" 2,5 "), Some(2.5));
        assert_eq!(parse_number("x"), None);
    }

    #[test]
    fn a_second_click_confirms() {
        forget_confirm();
        assert!(!confirmed("a"));
        assert!(confirming("a"));
        assert!(confirmed("a"));
        assert!(!confirming("a"));
        assert!(!confirmed("b"));
        forget_confirm();
    }
}
