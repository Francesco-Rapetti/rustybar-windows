//! The widgets, one file each, registered in `create`. A widget keeps its own state, says what its
//! pill shows, and wakes up only for its own reasons: a handle the app waits on (a timer, a
//! registry change) or an event the app hands to every widget (the app in front, the PC waking).

use windows::Win32::Foundation::HANDLE;

use crate::config::Options;
use crate::theme::Theme;
use crate::ui::Pill;

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
}

pub fn create(kind: &str, options: &Options) -> Option<Box<dyn Widget>> {
    match kind {
        "clock" => Some(Box::new(clock::Clock::new(options))),
        "front_app" => Some(Box::new(front_app::FrontApp::default())),
        "workspaces" => Some(Box::new(workspaces::Workspaces::new(options))),
        _ => None,
    }
}
