//! The widgets, one file each, registered in `create`. A widget keeps its own state, says what its
//! pill shows, and wakes up only for its own reasons: a handle the app waits on (a timer, a
//! registry change) or an event the app hands to every widget (the app in front, the PC waking).
//! A widget with a popup lays it out as a `Scene` and answers what the user does in it; what only
//! the popup needs is made when it opens and dropped when it closes (`popup_shown`).

use windows::Win32::Foundation::HANDLE;

use crate::config::Options;
use crate::theme::Theme;
use crate::ui::{Fonts, Pill, Scene};

pub mod audio;
pub mod claude;
pub mod clock;
pub mod devices;
pub mod display;
pub mod docker;
pub mod events;
pub mod front_app;
mod hid;
pub mod home;
pub mod lua;
pub mod media;
mod mixer;
pub mod network;
pub mod power;
mod privacy;
pub mod system;
pub mod workspaces;

/// The kinds `create` knows.
pub const KINDS: &[&str] = &["audio", "claude", "clock", "devices", "display", "docker", "events", "front_app", "home", "media", "network", "power", "system", "workspaces"];

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
    /// Whether a game of `[games] apps` is in front: timers stop but the clock's.
    Game(bool),
    /// A device came, went or changed (`WM_DEVICECHANGE` with `DBT_DEVNODES_CHANGED`), often several
    /// times in a row: a Bluetooth device that connects, a USB headset.
    DevicesChanged,
    /// The screens changed: one came or went, a resolution, a refresh rate, HDR.
    Displays,
    /// `rustybar msg trigger <name> [key=value…]`, or `rb.trigger`: for the Lua widgets that
    /// listen to it.
    Trigger { name: &'a str, args: &'a [(String, String)] },
}

pub trait Widget {
    /// The pill in the bar, or None to hide the widget.
    fn pill(&self, theme: &Theme) -> Option<Pill>;

    /// The pill in its compact form, for a bar without room for the full one (see `fit`): None
    /// for a widget that has no other.
    fn compact(&self, _theme: &Theme) -> Option<Pill> {
        None
    }

    /// A wider form, for a widget of the center group when the bar has room to spare for it (the
    /// title of `media` that would be cut): None for a widget that has no other.
    fn wide(&self, _theme: &Theme) -> Option<Pill> {
        None
    }

    /// A handle for the app to wait on, if the widget has one.
    fn handle(&self) -> Option<HANDLE> {
        None
    }

    /// The handles for the app to wait on, for a widget with more than one (an event its
    /// callbacks set, a timer).
    fn handles(&self) -> Vec<HANDLE> {
        self.handle().into_iter().collect()
    }

    /// The handle was signaled. True if what the widget shows changed: its pill, or its popup
    /// while it's open.
    fn signaled(&mut self) -> bool {
        false
    }

    /// The handle at `index` of `handles` was signaled; as `signaled`.
    fn signaled_at(&mut self, _index: usize) -> bool {
        self.signaled()
    }

    /// True if the pill changed.
    fn event(&mut self, _signal: &Signal) -> bool {
        false
    }

    /// Whether a click on the pill opens a popup.
    fn has_popup(&self) -> bool {
        false
    }

    /// Whether a click on its pill does something without a popup (see `click`): its items are
    /// clickable in a `⋯` popup too.
    fn clickable(&self) -> bool {
        false
    }

    /// A click on the pill of a widget without a popup, on its item `item` (by its index in the
    /// pill) or between items. True if the pill changed.
    fn click(&mut self, _item: Option<usize>) -> bool {
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
    /// A press on a slider, or a drag after it: its action and where along it, 0 to 1.
    Slide(&'a str, f32),
    /// The mouse went up after a press on a slider.
    Release(&'a str),
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
    Close,
    /// The widget opened something (a page, an app) that takes the front: the popup closes and
    /// leaves the front to it.
    Opened,
}

pub fn create(kind: &str, options: &Options) -> Option<Box<dyn Widget>> {
    match kind {
        "clock" => Some(Box::new(clock::Clock::new(options))),
        "audio" => Some(Box::new(audio::Audio::new(options))),
        "claude" => Some(Box::new(claude::Claude::new(options))),
        "media" => Some(Box::new(media::Media::new(options))),
        "network" => Some(Box::new(network::Network::new(options))),
        "power" => Some(Box::new(power::Power::new())),
        "front_app" => Some(Box::new(front_app::FrontApp::new())),
        "docker" => Some(Box::new(docker::Docker::new(options))),
        "devices" => Some(Box::new(devices::Devices::new(options))),
        "display" => Some(Box::new(display::Display::new(options))),
        "events" => Some(Box::new(events::Events::new(options))),
        "home" => Some(Box::new(home::Home::new(options))),
        "system" => Some(Box::new(system::System::new(options))),
        "workspaces" => Some(Box::new(workspaces::Workspaces::new(options))),
        _ => None,
    }
}
