//! Keeps the two sides of a bar apart, as the macOS RustyBar does. The center group sits in the
//! middle of the screen while each side fits in its half; when one side needs more, the center
//! group moves aside, toward the other side, just as far as it must (`center_x`), so that the two
//! sides share all the room the screen has beside it. When the widgets on the left and those on
//! the right don't fit in that room, first the center group turns compact; then the widgets
//! farthest from their edge give way first: they switch to their compact form one at a time, and
//! when every one is compact they move, one at a time, the outermost last, to the popup of a `⋯`
//! pill that takes their place, so that every widget stays at hand. A side takes its bigger forms
//! back only with some room to spare, so that a widget whose width changes doesn't flip back and
//! forth at the limit.
//!
//! All of it is arithmetic on widths already measured: a bar that fits measures nothing more, and
//! the compact pills are measured only while some bar needs them.

/// How a widget is drawn in a bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Full,
    Compact,
    /// Not in the bar: in the popup of the `⋯` pill of its side.
    Hidden,
}

/// A widget of the left or right side.
#[derive(Clone, Debug)]
pub struct Item {
    pub left: bool,
    /// Its place counted from its edge: 0 is the leftmost of the left side, the rightmost of the
    /// right side.
    pub rank: usize,
    /// Whether it has a compact form narrower than its own.
    pub compact: bool,
}

/// The room the sides have, without the margins and the gaps around them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Room {
    /// Nothing between them: together they take at most this.
    Shared(f32),
    /// The center group between them: each side takes at most its own.
    Split { left: f32, right: f32 },
}

/// The room a side must have to spare before it takes a bigger form back.
pub const SLACK: f32 = 24.0;

/// The `⋯` pills of the left and right sides, by the names their pills have in the bar (and that
/// `rustybar msg popup` takes).
pub const MORE: [&str; 2] = ["⋯left", "⋯right"];

/// The side of a `⋯` pill: 0 left, 1 right.
pub fn more_side(name: &str) -> Option<usize> {
    MORE.iter().position(|m| *m == name)
}

/// How a bar's pills fit it, as the last fit left it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fitted {
    /// What `fit` returned, for the next one.
    pub levels: Vec<usize>,
    pub center_compact: bool,
    /// The center group in its wide form (see `Widget::wide`).
    pub center_wide: bool,
    /// The widgets drawn compact, and those in each side's `⋯` popup (left, right).
    pub compact: Vec<String>,
    pub hidden: [Vec<String>; 2],
}

impl Fitted {
    /// Every pill in its full form.
    pub fn full(&self) -> bool {
        self.levels.iter().all(|l| *l == 0) && !self.center_compact
    }

    /// The side whose `⋯` popup has the widget `name`.
    pub fn hiding(&self, name: &str) -> Option<usize> {
        self.hidden.iter().position(|h| h.iter().any(|n| n == name))
    }
}

/// One widget giving way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Step {
    item: usize,
    form: Form,
}

/// The order in which the widgets of a lane give way: those with a compact form switch to it,
/// then they go, the farthest from their edge first and, at the same distance, the right side
/// first.
fn steps(items: &[Item], lane: impl Fn(&Item) -> bool) -> Vec<Step> {
    let mut order: Vec<usize> = (0..items.len()).filter(|&i| lane(&items[i])).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(items[i].rank), items[i].left));
    let compact = order.iter().filter(|&&i| items[i].compact).map(|&item| Step { item, form: Form::Compact });
    let hide = order.iter().map(|&item| Step { item, form: Form::Hidden });
    compact.chain(hide).collect()
}

fn forms(items: &[Item], lanes: &[Vec<Step>], levels: &[usize]) -> Vec<Form> {
    let mut forms = vec![Form::Full; items.len()];
    for (steps, &level) in lanes.iter().zip(levels) {
        for s in &steps[..level.min(steps.len())] {
            forms[s.item] = s.form;
        }
    }
    forms
}

/// Whether the sides, `(left, right)` wide, fit the room with `slack` to spare.
pub fn fits(room: Room, (l, r): (f32, f32), slack: f32) -> bool {
    match room {
        Room::Shared(w) => l + r <= w - slack,
        Room::Split { left, right } => l <= left - slack && r <= right - slack,
    }
}

/// Where the center group, `center` wide, starts in a bar `width` DIPs wide whose sides are `left`
/// and `right` wide (0 for a side without pills): in the middle of the screen, or as close to it
/// as the sides leave it, `gap` away from them, when one of them takes more than its half. Sides
/// that leave it no room at all (only when no form fits them) leave it in the middle.
pub fn center_x(width: f32, margin: f32, gap: f32, (left, right): (f32, f32), center: f32) -> f32 {
    let middle = (width - center) / 2.0;
    let first = margin + if left > 0.0 { left + gap } else { 0.0 };
    let last = width - margin - center - if right > 0.0 { right + gap } else { 0.0 };
    if first <= last { middle.clamp(first, last) } else { middle }
}

/// Picks the form of every item. `prev` is what the last call returned for this bar (empty the
/// first time); `measure` gives the width of the left and right sides with the given forms, `⋯`
/// pills included. Returns the levels to pass as `prev` next time, and the forms.
pub fn fit(items: &[Item], room: Room, prev: &[usize], mut measure: impl FnMut(&[Form]) -> (f32, f32)) -> (Vec<usize>, Vec<Form>) {
    // A lane is a set of widgets that share their room: both sides, or each on its own
    let lanes: Vec<Vec<Step>> = match room {
        Room::Shared(_) => vec![steps(items, |_| true)],
        Room::Split { .. } => vec![steps(items, |i| i.left), steps(items, |i| !i.left)],
    };
    let lane_fits = |lane: usize, (l, r): (f32, f32), slack: f32| match room {
        Room::Shared(w) => l + r <= w - slack,
        Room::Split { left, .. } if lane == 0 => l <= left - slack,
        Room::Split { right, .. } => r <= right - slack,
    };
    let mut levels: Vec<usize> = if prev.len() == lanes.len() { prev.to_vec() } else { vec![0; lanes.len()] };
    for lane in 0..lanes.len() {
        let max = lanes[lane].len();
        let mut fits_at = |levels: &mut Vec<usize>, level: usize, slack: f32| {
            levels[lane] = level;
            lane_fits(lane, measure(&forms(items, &lanes, levels)), slack)
        };
        let mut level = levels[lane].min(max);
        if fits_at(&mut levels, level, 0.0) {
            // Bigger forms back, while they leave some room
            while level > 0 && fits_at(&mut levels, level - 1, SLACK) {
                level -= 1;
            }
        } else {
            // Smaller ones until they fit, or nothing left to give
            while level < max && !fits_at(&mut levels, level + 1, 0.0) {
                level += 1;
            }
            level = (level + 1).min(max);
        }
        levels[lane] = level;
    }
    let forms = forms(items, &lanes, &levels);
    (levels, forms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Form::*;

    /// A bar: (left, width, compact width) of each widget, in the order of the config; a compact
    /// width equal to the full one is no compact form.
    fn bar(widgets: &[(bool, f32, f32)]) -> (Vec<Item>, impl Fn(&[Form]) -> (f32, f32) + Clone + use<>) {
        let rights = widgets.iter().filter(|w| !w.0).count();
        let mut seen = [0, 0];
        let items = widgets
            .iter()
            .map(|&(left, full, compact)| {
                let k = &mut seen[left as usize];
                *k += 1;
                Item { left, rank: if left { *k - 1 } else { rights - *k }, compact: compact < full }
            })
            .collect();
        let widths = widgets.to_vec();
        let measure = move |forms: &[Form]| {
            let mut sides = (0.0, 0.0);
            for (&(left, full, compact), form) in widths.iter().zip(forms) {
                let w = match form {
                    Full => full,
                    Compact => compact,
                    Hidden => 0.0,
                };
                if left { sides.0 += w } else { sides.1 += w }
            }
            sides
        };
        (items, measure)
    }

    #[test]
    fn room_enough() {
        let (items, measure) = bar(&[(true, 100.0, 50.0), (false, 100.0, 50.0)]);
        let (levels, forms) = fit(&items, Room::Shared(300.0), &[], measure);
        assert_eq!(levels, vec![0]);
        assert_eq!(forms, vec![Full, Full]);
    }

    #[test]
    fn farthest_from_the_edge_first() {
        // power, workspaces | media (no compact form), audio, clock
        let widgets = [(true, 30.0, 30.0), (true, 300.0, 150.0), (false, 50.0, 50.0), (false, 200.0, 60.0), (false, 120.0, 60.0)];
        let (items, measure) = bar(&widgets);
        // 700 wide in 570: audio compact (2nd from the right edge), before workspaces (2nd from
        // the left one); media has no compact form to take
        let (_, forms) = fit(&items, Room::Shared(570.0), &[], measure.clone());
        assert_eq!(forms, vec![Full, Full, Full, Compact, Full]);
        // Less room: workspaces too, then the clock
        let (_, forms) = fit(&items, Room::Shared(400.0), &[], measure.clone());
        assert_eq!(forms, vec![Full, Compact, Full, Compact, Compact]);
        // Every one compact isn't enough: media goes first, the farthest from its edge
        let (_, forms) = fit(&items, Room::Shared(330.0), &[], measure.clone());
        assert_eq!(forms, vec![Full, Compact, Hidden, Compact, Compact]);
        // Nothing fits: nothing stays
        let (_, forms) = fit(&items, Room::Shared(10.0), &[], measure);
        assert_eq!(forms, vec![Hidden; 5]);
    }

    #[test]
    fn each_side_of_the_center_on_its_own() {
        let widgets = [(true, 30.0, 30.0), (true, 300.0, 150.0), (false, 200.0, 60.0), (false, 120.0, 60.0)];
        let (items, measure) = bar(&widgets);
        // The right side fits, the left one doesn't: only the left one gives way
        let (levels, forms) = fit(&items, Room::Split { left: 200.0, right: 400.0 }, &[], measure);
        assert_eq!(levels.len(), 2);
        assert_eq!(forms, vec![Full, Compact, Full, Full]);
    }

    #[test]
    fn a_long_side_takes_the_room_the_other_leaves() {
        // The bar of a 1920 screen that told the center group's halves apart: power, workspaces,
        // front_app | home, three claude, devices, tray, network, audio, system, clock (with the
        // gaps of 8 in their widths), the center group compact (156)
        let widgets = [
            (true, 42.0, 42.0),
            (true, 208.0, 208.0),
            (true, 113.0, 113.0),
            (false, 42.0, 42.0),
            (false, 94.0, 64.0),
            (false, 147.0, 71.0),
            (false, 92.0, 64.0),
            (false, 297.0, 116.0),
            (false, 42.0, 42.0),
            (false, 98.0, 42.0),
            (false, 404.0, 76.0),
            (false, 254.0, 88.0),
            (false, 116.0, 116.0),
        ];
        let (items, measure) = bar(&widgets);
        // Each side in its half (866): the right one gave up seven forms, the last for 21 DIPs
        let (_, forms) = fit(&items, Room::Split { left: 866.0, right: 866.0 }, &[], measure.clone());
        assert_eq!(forms.iter().filter(|f| **f == Compact).count(), 7);
        // The two sharing the room beside it: only the three claude and devices give way
        let (_, forms) = fit(&items, Room::Shared(1920.0 - 16.0 - 156.0 - 16.0), &[], measure);
        let compact: Vec<usize> = (0..forms.len()).filter(|&i| forms[i] == Compact).collect();
        assert_eq!(compact, vec![4, 5, 6, 7]);
    }

    #[test]
    fn the_center_group_moves_aside_only_as_far_as_it_must() {
        // Both sides in their halves: in the middle
        assert_eq!(center_x(1920.0, 8.0, 8.0, (363.0, 700.0), 156.0), 882.0);
        // The right side past its half: as close to the middle as it lets it, 8 from it
        assert_eq!(center_x(1920.0, 8.0, 8.0, (363.0, 1271.0), 156.0), 1920.0 - 8.0 - 1271.0 - 8.0 - 156.0);
        // The left side past its half
        assert_eq!(center_x(1920.0, 8.0, 8.0, (1271.0, 363.0), 156.0), 8.0 + 1271.0 + 8.0);
        // A side without pills needs no gap: the center group at the margin
        assert_eq!(center_x(1920.0, 8.0, 8.0, (0.0, 1740.0), 156.0), 8.0);
        // No room at all: the middle, as before
        assert_eq!(center_x(1920.0, 8.0, 8.0, (1000.0, 1000.0), 156.0), 882.0);
    }

    #[test]
    fn bigger_forms_back_only_with_room_to_spare() {
        let widgets = [(true, 100.0, 50.0), (false, 100.0, 50.0)];
        let (items, measure) = bar(&widgets);
        let (levels, _) = fit(&items, Room::Shared(190.0), &[], measure.clone());
        assert_eq!(levels, vec![1]);
        // Room for both full forms, but not much more: the right one stays compact
        let (levels, forms) = fit(&items, Room::Shared(200.0 + SLACK / 2.0), &levels, measure.clone());
        assert_eq!((levels.clone(), forms), (vec![1], vec![Full, Compact]));
        // With room to spare it takes its full form back
        let (levels, forms) = fit(&items, Room::Shared(200.0 + SLACK), &levels, measure);
        assert_eq!((levels, forms), (vec![0], vec![Full, Full]));
    }

    #[test]
    fn levels_of_a_bar_that_lost_widgets() {
        // The last call had more steps than there are now: no step past the end
        let (items, measure) = bar(&[(true, 100.0, 50.0)]);
        let (levels, forms) = fit(&items, Room::Shared(1000.0), &[7], measure);
        assert_eq!((levels, forms), (vec![0], vec![Full]));
    }
}
