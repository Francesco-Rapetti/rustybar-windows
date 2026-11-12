//! The volume mixer in the `audio` popup: the apps that have sound on the default output, each
//! with its volume and its mute, as Windows' mixer has them (every session of an executable
//! together: set together, read from the first). Made when the popup opens and dropped when it
//! closes, so it costs nothing at rest: the output's session manager (`IAudioSessionManager2`),
//! its word of new sessions (`IAudioSessionNotification`) and of each session's volume and state
//! (`IAudioSessionEvents`). The callbacks, on Core Audio's threads, only set a flag and the
//! widget's event; the widget reads again on the main thread.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::Audio::{
    AudioSessionDisconnectReason, AudioSessionState, AudioSessionStateActive, AudioSessionStateExpired, IAudioSessionControl, IAudioSessionControl2,
    IAudioSessionEvents, IAudioSessionEvents_Impl, IAudioSessionManager2, IAudioSessionNotification, IAudioSessionNotification_Impl, IMMDevice,
    ISimpleAudioVolume,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::core::{BOOL, GUID, Interface, PCWSTR, implement};

use super::front_app;
use crate::i18n::t;
use crate::wake::Wake;

/// At most this many apps, so the popup stays on the screen.
pub const MOST: usize = 8;

/// What the callbacks tell the widget.
struct Flags {
    /// A session came or went: list them again.
    listed: AtomicBool,
    /// A volume, a mute or a state changed: read them again.
    volumes: AtomicBool,
    wake: Arc<Wake>,
}

#[implement(IAudioSessionNotification)]
struct NewSession {
    flags: Arc<Flags>,
}

impl IAudioSessionNotification_Impl for NewSession_Impl {
    fn OnSessionCreated(&self, _: windows::core::Ref<IAudioSessionControl>) -> windows::core::Result<()> {
        self.flags.listed.store(true, Ordering::Release);
        self.flags.wake.set();
        Ok(())
    }
}

#[implement(IAudioSessionEvents)]
struct SessionEvents {
    flags: Arc<Flags>,
}

impl SessionEvents_Impl {
    fn poke(&self, listed: bool) {
        let flag = if listed { &self.flags.listed } else { &self.flags.volumes };
        flag.store(true, Ordering::Release);
        self.flags.wake.set();
    }
}

impl IAudioSessionEvents_Impl for SessionEvents_Impl {
    fn OnDisplayNameChanged(&self, _: &PCWSTR, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnIconPathChanged(&self, _: &PCWSTR, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnSimpleVolumeChanged(&self, _: f32, _: BOOL, _: *const GUID) -> windows::core::Result<()> {
        self.poke(false);
        Ok(())
    }

    fn OnChannelVolumeChanged(&self, _: u32, _: *const f32, _: u32, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnGroupingParamChanged(&self, _: *const GUID, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnStateChanged(&self, state: AudioSessionState) -> windows::core::Result<()> {
        self.poke(state == AudioSessionStateExpired);
        Ok(())
    }

    fn OnSessionDisconnected(&self, _: AudioSessionDisconnectReason) -> windows::core::Result<()> {
        self.poke(true);
        Ok(())
    }
}

/// An app in the mixer.
pub struct App {
    pub name: String,
    /// Its executable's path, or `system` for Windows' sounds.
    key: String,
    sessions: Vec<(IAudioSessionControl, ISimpleAudioVolume)>,
    pub level: f32,
    pub muted: bool,
    /// Some sound plays now.
    pub active: bool,
}

impl App {
    /// A made-up app (see `mock`), with no session behind it.
    pub fn mock(name: &str, level: f32, muted: bool, active: bool) -> App {
        App { name: name.to_string(), key: name.to_string(), sessions: Vec::new(), level, muted, active }
    }

    fn read(&mut self) -> bool {
        let before = (self.level, self.muted, self.active);
        if let Some((_, volume)) = self.sessions.first() {
            unsafe {
                self.level = volume.GetMasterVolume().unwrap_or(self.level);
                self.muted = volume.GetMute().map(|m| m.as_bool()).unwrap_or(self.muted);
            }
        }
        self.active = self.sessions.iter().any(|(c, _)| unsafe { c.GetState() }.is_ok_and(|s| s == AudioSessionStateActive));
        before != (self.level, self.muted, self.active)
    }

    /// Sets the level; raising it takes the mute away, as Windows' sliders do.
    pub fn set_level(&mut self, level: f32) {
        let level = level.clamp(0.0, 1.0);
        let unmute = self.muted && level > self.level;
        for (_, volume) in &self.sessions {
            unsafe {
                let _ = volume.SetMasterVolume(level, std::ptr::null());
                if unmute {
                    let _ = volume.SetMute(false, std::ptr::null());
                }
            }
        }
        self.muted &= !unmute;
        self.level = level;
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        for (_, volume) in &self.sessions {
            unsafe {
                let _ = volume.SetMute(self.muted, std::ptr::null());
            }
        }
    }
}

pub struct Mixer {
    manager: Option<IAudioSessionManager2>,
    notification: Option<IAudioSessionNotification>,
    events: IAudioSessionEvents,
    flags: Arc<Flags>,
    pub apps: Vec<App>,
    /// The apps' names, by executable, for this popup.
    names: HashMap<String, String>,
}

impl Mixer {
    /// The mixer of `device` (the default output), told by Core Audio of what changes.
    pub fn open(device: &IMMDevice, wake: &Arc<Wake>) -> Mixer {
        let flags = Arc::new(Flags { listed: AtomicBool::new(false), volumes: AtomicBool::new(false), wake: wake.clone() });
        let events: IAudioSessionEvents = SessionEvents { flags: flags.clone() }.into();
        let manager: Option<IAudioSessionManager2> = unsafe { device.Activate(CLSCTX_ALL, None) }.ok();
        let mut mixer = Mixer { manager, notification: None, events, flags, apps: Vec::new(), names: HashMap::new() };
        // The sessions are listed once before Core Audio sends new ones
        mixer.list();
        if let Some(manager) = &mixer.manager {
            let notification: IAudioSessionNotification = NewSession { flags: mixer.flags.clone() }.into();
            if unsafe { manager.RegisterSessionNotification(&notification) }.is_ok() {
                mixer.notification = Some(notification);
            }
        }
        mixer
    }

    /// A made-up mixer (see `mock`).
    pub fn mock(apps: Vec<App>, wake: &Arc<Wake>) -> Mixer {
        let flags = Arc::new(Flags { listed: AtomicBool::new(false), volumes: AtomicBool::new(false), wake: wake.clone() });
        Mixer { manager: None, notification: None, events: SessionEvents { flags: flags.clone() }.into(), flags, apps, names: HashMap::new() }
    }

    /// What the callbacks said, read again. True if the mixer changed.
    pub fn update(&mut self) -> bool {
        if self.flags.listed.swap(false, Ordering::AcqRel) {
            self.flags.volumes.store(false, Ordering::Release);
            self.list();
            return true;
        }
        if self.flags.volumes.swap(false, Ordering::AcqRel) {
            let mut changed = false;
            for app in &mut self.apps {
                changed |= app.read();
            }
            return changed;
        }
        false
    }

    fn unwatch(&mut self) {
        for app in &self.apps {
            for (control, _) in &app.sessions {
                unsafe {
                    let _ = control.UnregisterAudioSessionNotification(&self.events);
                }
            }
        }
    }

    /// The apps with a session that hasn't ended, by name, Windows' sounds last.
    fn list(&mut self) {
        let Some(manager) = &self.manager else { return };
        let Ok(list) = (unsafe { manager.GetSessionEnumerator() }) else { return };
        let mut apps: Vec<App> = Vec::new();
        for j in 0..unsafe { list.GetCount() }.unwrap_or(0) {
            let Ok(control) = (unsafe { list.GetSession(j) }) else { continue };
            if unsafe { control.GetState() }.is_ok_and(|s| s == AudioSessionStateExpired) {
                continue;
            }
            let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
            let (key, name) = if unsafe { control2.IsSystemSoundsSession() } == S_OK {
                ("system".to_string(), t("Suoni di sistema", "System sounds").to_string())
            } else {
                let pid = unsafe { control2.GetProcessId() }.unwrap_or(0);
                match front_app::process_path(pid) {
                    Some(path) => {
                        let name = self.names.entry(path.to_lowercase()).or_insert_with(|| front_app::describe(&path)).clone();
                        (path.to_lowercase(), name)
                    }
                    // A process this one can't open (an elevated one): its own name for the session, if it gives one
                    None => match session_name(&control) {
                        Some(name) => (format!("pid:{pid}"), name),
                        None => continue,
                    },
                }
            };
            let Ok(volume) = control.cast::<ISimpleAudioVolume>() else { continue };
            match apps.iter_mut().find(|a| a.key == key) {
                Some(app) => app.sessions.push((control, volume)),
                None => apps.push(App { name, key, sessions: vec![(control, volume)], level: 0.0, muted: false, active: false }),
            }
        }
        apps.sort_by_key(|a| (a.key == "system", a.name.to_lowercase()));
        apps.truncate(MOST);
        self.unwatch();
        for app in &mut apps {
            for (control, _) in &app.sessions {
                unsafe {
                    let _ = control.RegisterAudioSessionNotification(&self.events);
                }
            }
            app.read();
        }
        self.apps = apps;
    }
}

impl Drop for Mixer {
    fn drop(&mut self) {
        self.unwatch();
        if let (Some(manager), Some(notification)) = (&self.manager, &self.notification) {
            unsafe {
                let _ = manager.UnregisterSessionNotification(notification);
            }
        }
    }
}

/// The name a session gives itself, if it's a name and not a resource (`@%SystemRoot%\…`).
fn session_name(control: &IAudioSessionControl) -> Option<String> {
    let name = unsafe { control.GetDisplayName() }.ok()?;
    let text = unsafe { name.to_string() }.ok();
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(name.0 as *const std::ffi::c_void)) };
    text.filter(|n| !n.is_empty() && !n.starts_with('@'))
}
