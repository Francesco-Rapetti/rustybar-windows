# RustyBar for Windows

A status bar at the top of every screen, for a gaming PC: it must cost no frames, and no CPU, GPU
or memory you could measure. The sibling of [RustyBar for macOS](https://github.com/Francesco-Rapetti/rustybar):
the same ideas, built on Windows' own pieces.

- **One small native process.** Rust on Win32, no webview, no .NET, no framework that redraws
  every frame.
- **Asleep when nothing changes.** Data comes from Windows' events (the app in front, the
  registry, the shell). What has no events (CPU, GPU, memory) is read in-process on a slow timer,
  every 5 seconds, and drawn only when a number changes; the clock wakes once a minute. Both
  stop while no bar shows.
- **Drawn only when something changes, animated by the compositor.** Pills are DirectComposition
  surfaces drawn with Direct2D on WARP, Windows' software rasterizer, on the bar's own thread: no
  GPU device at all, and no thread pool to wake up. Slides run inside DWM, so the bar does no
  work during an animation.
- **Popups that exist only while open.** A click on a pill opens its popup under it, as the
  taskbar's flyouts do: a window made when it opens and destroyed when it closes, with a click
  anywhere else, Esc or the pill again. A click on the bar itself never takes the focus from the
  app in front.
- **Out of the way of games.** A full screen app (exclusive or borderless) hides the bar of its
  screen; with every bar hidden, the bar lets go of its devices and sleeps until the game leaves.
  Listed games turn animations off even in a window.

## What it costs

Measured with `scripts\measure.ps1` on Windows 11 (5120×1440 and 1920×1080 screens; workspaces,
front_app, media, network, audio, system and clock):

| | at rest |
|---|---|
| Memory | 5–6 MB private working set |
| CPU | 16–31 ms in 60 s (system's readings, every 5 s) |
| Wake-ups | ~2.5 context switches/s |
| Threads | 2–5 |
| GPU | none: 0% engines, 0 MB of video memory |
| Child processes | 0 (claude, if you add it: one for under a second every 5 minutes) |
| First frame | ~55 ms after start |
| Binary | ~890 KB, no runtime to install |

Core Audio costs ~0.3 MB and the media sessions ~0.8 MB; both say when something changes, so
neither wakes the bar on its own.

With a GPU device instead of WARP (`RUSTYBAR_GPU=hardware`) the bar takes 37 MB, 9.5 MB of video
memory and 24 threads: that's why it draws in software.

A reading of system costs ~0.7 ms, about 0.3 ms of it in the GPU driver (for the 3D engine; all
the engines, as Task Manager reads them, would be 4 ms on a Radeon). An open popup adds
0.3–2 MB; system's reads the processes every 2 seconds while open, ~6 ms each. When a popup is
gone the bar lets go of what drawing it left cached and gives the freed pages back to Windows:
after 50 popups the bar is back where it started.

## Build and install

Rust with the MSVC toolchain:

```
cargo build --release
target\release\rustybar.exe install
```

`rustybar install`, for your user alone and without administrator rights:

- copies the executable to `%LOCALAPPDATA%\Programs\RustyBar` (closing the installed bar first,
  so the same command updates it);
- writes the example config to `%APPDATA%\rustybar\rustybar.toml`, if there's none;
- starts the bar with Windows (the `Run` key: Settings > Apps > Startup shows it, with its
  switch);
- lists it in Settings > Apps, whose *Uninstall* runs `rustybar uninstall`;
- adds a *RustyBar* shortcut to the Start menu, and starts the bar.

`rustybar uninstall` undoes all of it and leaves the config and the logs. Both do their work in
a second run started by File Explorer, as the bar is at sign-in: from the terminal of an app
package (the Claude app's, for one) Windows would keep the registry, and maybe the config, private
to that app, and the bar wouldn't start with Windows. Other commands:

- `rustybar quit` closes the running bar (of the same config dir); the Start menu shortcut
  starts it again.
- `rustybar check` checks the config: that it parses, the theme exists and every name in
  `[layout]` is a widget.
- The log is `%LOCALAPPDATA%\rustybar\rustybar.log`.

## Configuration

`%APPDATA%\rustybar\rustybar.toml`, reloaded when you save it. Every key has a default, so the
file is optional; [`config/rustybar.toml`](config/rustybar.toml) lists them all.

Environment variables:

- `RUSTYBAR_CONFIG_DIR`: another config dir. A bar with its own dir runs next to the real one,
  with its own log (`rustybar-<dir name>.log`); with `[bar] reserve = false` it doesn't move the
  windows.
- `RUSTYBAR_GPU=hardware`: draw with the GPU instead of WARP (see above).

## Widgets

- **clock**: the date and time in the formats of Windows (`Mar 10/11  21:13`). Its popup is a
  calendar, a month at a time: today in a dot, the week from the region's first day, the weekend
  dimmer; ‹ ›, the wheel and the arrow keys go through the months, *Today* (or Home) back. A
  click on a day opens it in Google Calendar. Options: `date`, a Windows date picture, `""` for
  the time alone; `open`, the page a click on a day opens, with `{year}`, `{month}` and `{day}`
  in it, `""` for days that can't be clicked.
- **front_app**: the name of the app in front, from its executable's version information
  (`Google Chrome`), cut at 260 DIPs. The taskbar and the desktop leave it as it was.
- **workspaces**: the virtual desktops, the current one under a highlight that slides; a click
  on one goes there, with Windows' own Ctrl+Win+←/→ (simulated, so not while a listed game is in
  front). Hidden with a single desktop (`single = true` keeps it).
- **audio**: the output's icon (headphones, a display, speakers by how loud; crossed out when
  muted) and its volume, and the battery of a Bluetooth output. Its popup: the volume of the
  output and of the input, with a slider (click or drag) and mute on the icon; the devices to
  switch to (the one in use lit); the sound settings and the volume mixer. Core Audio tells
  every change, so nothing is read on a timer, except the battery of a Bluetooth device in use
  (Windows doesn't announce it): every 5 minutes. A click on a device makes it the default
  through `IPolicyConfig`, the undocumented interface Windows' own sound settings use.
- **media**: what's playing in any app with a player (`♪ Title  Artist`), from the media
  sessions Windows keeps (the ones of the volume flyout); dimmed with a pause sign while paused
  (`paused = false` hides it), hidden when nothing plays. Its popup is a player: the cover, the
  title, the artist and the app, where the track is (click or drag to seek) and ⏮ ⏯ ⏭. All on
  events; the position moves once a second only while the popup is open and the track plays.
  The cover is read only for the open popup (`cover = false` leaves it out).
- **claude** (not in the default layout: it costs a process): how much of the Claude plan's
  limits the account has used, `✻ 42% · 31% · 17:07` (the 5-hour session, the week, when the
  session starts again), yellow from 75%, red from 90%. Its popup: the account, the session and
  the week with when they reset, the usage page, sign out; logged out, *Sign in* (opens the
  browser). The numbers come from Claude Code itself (`claude -p` answering the `get_usage`
  request, without sending a message), found on the PATH or carried by the Claude desktop app;
  its CLI needs its own login, done once with *Sign in*. **Cost:** the only child process of the
  bar, every 5 minutes while a bar shows and no listed game is in front: ~0.4 s of CPU and
  ~190 MB for under half a second, at idle priority with EcoQoS, ended with the bar. Logged out
  it runs only when the popup opens. Options: `dir` (another Claude Code config dir, for a
  second account as a second widget with `type = "claude"`), `color`, `path`, `warning`,
  `critical`.
- **network**: the connection's icon and name (`Ethernet`, the Wi-Fi network), yellow with *No
  internet* when only the local network answers, red with *Disconnected* without one, `VPN` in
  front while a VPN is up. Its popup: the connection (adapter, link speed; signal, band and
  Wi-Fi generation), download and upload with the last 30 seconds as bars, the VPN, the nearby
  Wi-Fi networks to join (a secured new one opens Windows' own list, which asks the password),
  and the details (local IP, router with its ping, DNS, MAC, IPv6; a click copies one). Windows
  tells every change, so nothing is read at rest; the counters (every second) and the ping
  (every 5 seconds, in-process) run only while the popup is open. Since Windows 11 24H2 the
  Wi-Fi network's name and the list need the location permission for desktop apps.
- **system**: CPU, GPU (its 3D engines, and its temperature when the driver tells it) and memory
  (`CPU 12%  GPU 34% 62°  RAM 43%`), yellow from 75%, red from 90%. Read every 5 seconds from
  Windows itself (no WMI, no performance counters); not at all while a listed game is in front.
  Its popup: CPU and GPU with the last 30 readings as bars, memory and video memory, every fixed
  disk (a click opens it), the five apps that use the most CPU or memory, Task Manager and the
  storage settings. Options: `interval`, `warning`, `critical`, `disk_warning`, `disk_critical`,
  `gpu = false`.

## Themes

`windows` (the default) follows Windows: light or dark as the apps are, with the accent color of
Settings, both followed as they change. The other 24 palettes are those of the macOS RustyBar:
Catppuccin (4), Dracula, Everforest (2), Gruvbox (2), Kanagawa (3), Nord, One (2), Rosé Pine (3),
Solarized (2), Tokyo Night (4). Every color can be overridden in `[theme.colors]`.
