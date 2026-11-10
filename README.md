# RustyBar for Windows

A status bar at the top of every screen, for a gaming PC: it must cost no frames, and no CPU, GPU
or memory you could measure. The sibling of [RustyBar for macOS](https://github.com/Francesco-Rapetti/rustybar):
the same ideas, built on Windows' own pieces.

- **One small native process.** Rust on Win32, no webview, no .NET, no framework that redraws
  every frame.
- **Asleep when nothing changes.** Data comes from Windows' events (the app in front, the
  registry, the shell); the only timer is the clock's, once a minute, and it stops while no bar
  shows.
- **Drawn only when something changes, animated by the compositor.** Pills are DirectComposition
  surfaces drawn with Direct2D on WARP, Windows' software rasterizer: no GPU device at all.
  Slides run inside DWM, so the bar does no work during an animation.
- **Popups that exist only while open.** A click on a pill opens its popup under it, as the
  taskbar's flyouts do: a window made when it opens and destroyed when it closes, with a click
  anywhere else, Esc or the pill again. A click on the bar itself never takes the focus from the
  app in front.
- **Out of the way of games.** A full screen app (exclusive or borderless) hides the bar of its
  screen; with every bar hidden, the bar lets go of its devices and sleeps until the game leaves.
  Listed games turn animations off even in a window.

## What it costs

Measured with `scripts\measure.ps1` on Windows 11 (5120×1440 and 1920×1080 screens, three
widgets):

| | at rest |
|---|---|
| Memory | 4–5 MB private working set |
| CPU | 0 ms in 60 s |
| Wake-ups | ~0.6 context switches/s (the minute's redraw) |
| GPU | none: 0% engines, 0 MB of video memory |
| Child processes | 0 |
| First frame | ~35 ms after start |
| Binary | ~680 KB, no runtime to install |

With a GPU device instead of WARP (`RUSTYBAR_GPU=hardware`) the same bar takes 37 MB, 9.5 MB of
video memory and 24 threads: that's why it draws in software.

An open popup (the calendar) adds 0.3–0.7 MB. When it's gone the bar lets go of what drawing it
left cached and gives the freed pages back to Windows: after 50 popups the bar is back where it
started.

## Build and run

Rust with the MSVC toolchain:

```
cargo build --release
target\release\rustybar.exe
```

- `rustybar quit` closes the running bar (of the same config dir).
- `rustybar check` checks the config: that it parses, the theme exists and every name in
  `[layout]` is a widget.
- The log is `%LOCALAPPDATA%\rustybar\rustybar.log`.

There is no installer yet: to start it with Windows, put a shortcut to `rustybar.exe` in
`shell:startup`.

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
- **workspaces**: the virtual desktops, the current one under a highlight that slides. Hidden with
  a single desktop (`single = true` keeps it).

## Themes

`windows` (the default) follows Windows: light or dark as the apps are, with the accent color of
Settings, both followed as they change. The other 24 palettes are those of the macOS RustyBar:
Catppuccin (4), Dracula, Everforest (2), Gruvbox (2), Kanagawa (3), Nord, One (2), Rosé Pine (3),
Solarized (2), Tokyo Night (4). Every color can be overridden in `[theme.colors]`.
