//! The widgets, one file each, registered in `create`. A widget keeps its own state, says what its
//! pill shows, and wakes up only for its own reasons: a handle the app waits on (a timer, a
//! registry change) or an event the app hands to every widget (the app in front, the PC waking).
//! A widget with a popup lays it out as a `Scene` and answers what the user does in it; what only
//! the popup needs is made when it opens and dropped when it closes (`popup_shown`).

use windows::Win32::Foundation::HANDLE;

use crate::config::Options;
use crate::theme::Theme;
use crate::ui::{Fonts, Pill, Scene};

pub mod clock;
pub mod front_app;
pub mod workspaces;

/// The kinds `create` knows.
pub const KINDS: &[&str] = &["clock", "front_app", "workspaces"];

/// What happened, for every widget.
pub enum Signal<'a> {
    /// Another app came in front.
    Foreground(&'a front_app::Foreground),
    /// The PC woke from sleep.
    Resume,
    /// The time, the time zone or the region's formats changed.
    TimeChanged,
    /// Whether some bar shows: none while a game is full screen on every screen.
    Shown(bool),
}

pub trait Widget {
    /// The pill in the bar, or None to hide the widget.
    fn pill(&self, theme: &Theme) -> Option<Pill>;

    /// A handle for the app to wait on, if the widget has one.
    fn handle(&self) -> Option<HANDLE> {
        None
    }

    /// The handle was signaled. True if the pill changed.
    fn signaled(&mut self) -> bool {
        false
    }

    /// True if the pill changed.
    fn event(&mut self, _signal: &Signal) -> bool {
        false
    }

    /// Whether a click on the pill opens a popup.
    fn has_popup(&self) -> bool {
        false
    }

    /// The popup opened (true) or closed (false): the widget makes, or drops, what only the
    /// popup needs.
    fn popup_shown(&mut self, _open: bool) {}

    /// What the popup shows, laid out in DIPs; None closes it.
    fn popup(&self, _theme: &Theme, _fonts: &Fonts) -> Option<Scene> {
        None
    }

    /// Something happened in the popup.
    fn popup_input(&mut self, _input: &Input) -> Reply {
        Reply::Nothing
    }
}

/// What the user did in a popup.
pub enum Input<'a> {
    /// A click on a zone, by its action.
    Action(&'a str),
    /// Wheel notches: positive away from the user (up), negative toward.
    Wheel(i32),
    /// A key, as Windows' virtual-key code (Esc closes the popup before it gets here).
    Key(u16),
}

/// What the popup does after an input.
#[derive(Debug, PartialEq)]
pub enum Reply {
    Nothing,
    /// The widget changed what the popup shows.
    Redraw,
    /// Done: the popup closes and the app that was in front comes back.
    #[allow(dead_code)] // For the popups to come: a choice that ends them
    Close,
    /// The widget opened something (a page, an app) that takes the front: the popup closes and
    /// leaves the front to it.
    Opened,
}

pub fn create(kind: &str, options: &Options) -> Option<Box<dyn Widget>> {
    match kind {
        "clock" => Some(Box::new(clock::Clock::new(options))),
        "front_app" => Some(Box::new(front_app::FrontApp::default())),
        "workspaces" => Some(Box::new(workspaces::Workspaces::new(options))),
        _ => None,
    }
}
