//! A made-up player, for trying the `media` widget and taking its screenshots without real music:
//! it gives Windows a media session, as Spotify or a browser does, with made-up tracks and covers
//! drawn here, and plays a silent sound in a loop so that the volume mixer has an audio session for
//! it. ⏮ ⏯ ⏭, seeking, shuffle and repeat work. It ends by itself after `seconds` (120 if not
//! given).
//!
//! `cargo run --release --example fake_player -- [set] [seconds]`, where `set` (0 or 1) picks the
//! made-up tracks. The session's app is the executable's name: a copy under another name
//! (`Aurora.exe`) is another app, for the popup's switch.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows::Foundation::{TimeSpan, TypedEventHandler, Uri};
use windows::Media::Core::MediaSource;
use windows::Media::Playback::MediaPlayer;
use windows::Media::{
    MediaPlaybackAutoRepeatMode, MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsTimelineProperties,
};
use windows::Storage::StorageFile;
use windows::Storage::Streams::RandomAccessStreamReference;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::core::{HSTRING, Result};

/// A made-up track: its title, artist, album, length in seconds, and the colors of its cover.
struct Track {
    title: &'static str,
    artist: &'static str,
    album: &'static str,
    seconds: u32,
    colors: [[u8; 3]; 3],
}

const SETS: [&[Track]; 2] = [
    &[
        Track { title: "Lanterns Over the Bay", artist: "The Quiet Hours", album: "Northern Light", seconds: 204, colors: [[0x1b, 0x1f, 0x4a], [0x3a, 0x2c, 0x6b], [0xff, 0xc5, 0x5e]] },
        Track { title: "Paper Satellites", artist: "The Quiet Hours", album: "Northern Light", seconds: 187, colors: [[0x0f, 0x3d, 0x3e], [0x14, 0x6b, 0x5c], [0xf2, 0x6d, 0x5b]] },
        Track { title: "Slow Tide", artist: "Mira Okafor", album: "Saltwater", seconds: 251, colors: [[0xe8, 0xdc, 0xc8], [0xd4, 0xb8, 0x96], [0x2e, 0x5e, 0x8c]] },
    ],
    &[
        Track { title: "Episode 12: Gardens on the Roof", artist: "Field Notes", album: "Field Notes", seconds: 1834, colors: [[0x2d, 0x4a, 0x22], [0x5c, 0x7a, 0x3a], [0xf4, 0xe3, 0x8a]] },
        Track { title: "Episode 13: The Night Train", artist: "Field Notes", album: "Field Notes", seconds: 2011, colors: [[0x3b, 0x12, 0x1f], [0x6e, 0x1e, 0x3a], [0xf0, 0xa2, 0xb8]] },
    ],
];

enum Command {
    Button(SystemMediaTransportControlsButton),
    Seek(i64),
    Shuffle(bool),
    Repeat(MediaPlaybackAutoRepeatMode),
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let set = args.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0).min(SETS.len() - 1);
    let seconds = args.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(120);
    let tracks = SETS[set];
    unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };

    // The silent sound and the covers, in the temporary files
    let dir = std::env::temp_dir().join(format!("rustybar-fake-player-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e.to_string()))?;
    let sound = dir.join("silence.wav");
    let _ = std::fs::write(&sound, silence());
    let covers: Vec<std::path::PathBuf> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let path = dir.join(format!("cover{i}.bmp"));
            let _ = std::fs::write(&path, cover(t.colors, i));
            path
        })
        .collect();
    let uri = |p: &std::path::Path| Uri::CreateUri(&HSTRING::from(format!("file:///{}", p.display().to_string().replace('\\', "/"))));

    let player = MediaPlayer::new()?;
    player.CommandManager()?.SetIsEnabled(false)?;
    player.SetSource(&MediaSource::CreateFromUri(&uri(&sound)?)?)?;
    player.SetIsLoopingEnabled(true)?;
    player.Play()?;

    let smtc = player.SystemMediaTransportControls()?;
    smtc.SetIsEnabled(true)?;
    smtc.SetIsPlayEnabled(true)?;
    smtc.SetIsPauseEnabled(true)?;
    smtc.SetIsNextEnabled(true)?;
    smtc.SetIsPreviousEnabled(true)?;
    let (tx, rx) = mpsc::channel::<Command>();
    let send = tx.clone();
    smtc.ButtonPressed(&TypedEventHandler::new(move |_, args: windows::core::Ref<windows::Media::SystemMediaTransportControlsButtonPressedEventArgs>| {
        if let Ok(args) = args.ok() {
            let _ = send.send(Command::Button(args.Button()?));
        }
        Ok(())
    }))?;
    let send = tx.clone();
    smtc.PlaybackPositionChangeRequested(&TypedEventHandler::new(move |_, args: windows::core::Ref<windows::Media::PlaybackPositionChangeRequestedEventArgs>| {
        if let Ok(args) = args.ok() {
            let _ = send.send(Command::Seek(args.RequestedPlaybackPosition()?.Duration));
        }
        Ok(())
    }))?;
    let send = tx.clone();
    smtc.ShuffleEnabledChangeRequested(&TypedEventHandler::new(move |_, args: windows::core::Ref<windows::Media::ShuffleEnabledChangeRequestedEventArgs>| {
        if let Ok(args) = args.ok() {
            let _ = send.send(Command::Shuffle(args.RequestedShuffleEnabled()?));
        }
        Ok(())
    }))?;
    let send = tx;
    smtc.AutoRepeatModeChangeRequested(&TypedEventHandler::new(move |_, args: windows::core::Ref<windows::Media::AutoRepeatModeChangeRequestedEventArgs>| {
        if let Ok(args) = args.ok() {
            let _ = send.send(Command::Repeat(args.RequestedAutoRepeatMode()?));
        }
        Ok(())
    }))?;
    smtc.SetShuffleEnabled(false)?;
    smtc.SetAutoRepeatMode(MediaPlaybackAutoRepeatMode::List)?;

    // Where the made-up track is: its position (100 ns) when `since` was
    let mut index = 0;
    let mut playing = true;
    let mut position: i64 = 37 * 10_000_000;
    let mut since = Instant::now();
    let show = |index: usize, playing: bool, position: i64| -> Result<()> {
        let t = &tracks[index];
        let display = smtc.DisplayUpdater()?;
        display.SetType(MediaPlaybackType::Music)?;
        let music = display.MusicProperties()?;
        music.SetTitle(&HSTRING::from(t.title))?;
        music.SetArtist(&HSTRING::from(t.artist))?;
        music.SetAlbumTitle(&HSTRING::from(t.album))?;
        // From a file: a file:// address isn't taken
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(covers[index].as_os_str()))?.join()?;
        display.SetThumbnail(&RandomAccessStreamReference::CreateFromFile(&file)?)?;
        display.Update()?;
        timeline(&smtc, t.seconds, position)?;
        smtc.SetPlaybackStatus(if playing { MediaPlaybackStatus::Playing } else { MediaPlaybackStatus::Paused })
    };
    show(index, playing, position)?;
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        let now = |position: i64, since: Instant, playing: bool| if playing { position + since.elapsed().as_nanos() as i64 / 100 } else { position };
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(command) => {
                position = now(position, since, playing);
                since = Instant::now();
                match command {
                    Command::Button(b) if b == SystemMediaTransportControlsButton::Play => playing = true,
                    Command::Button(b) if b == SystemMediaTransportControlsButton::Pause => playing = false,
                    Command::Button(b) if b == SystemMediaTransportControlsButton::Next => {
                        index = (index + 1) % tracks.len();
                        position = 0;
                    }
                    Command::Button(b) if b == SystemMediaTransportControlsButton::Previous => {
                        index = (index + tracks.len() - 1) % tracks.len();
                        position = 0;
                    }
                    Command::Seek(to) => position = to,
                    Command::Shuffle(on) => smtc.SetShuffleEnabled(on)?,
                    Command::Repeat(mode) => smtc.SetAutoRepeatMode(mode)?,
                    Command::Button(_) => {}
                }
                show(index, playing, position)?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // At the end of a track, the next
                if now(position, since, playing) >= tracks[index].seconds as i64 * 10_000_000 {
                    index = (index + 1) % tracks.len();
                    position = 0;
                    since = Instant::now();
                    show(index, playing, position)?;
                }
            }
            Err(_) => break,
        }
    }
    smtc.SetIsEnabled(false)?;
    drop(player);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn timeline(smtc: &SystemMediaTransportControls, seconds: u32, position: i64) -> Result<()> {
    let span = |d: i64| TimeSpan { Duration: d };
    let length = seconds as i64 * 10_000_000;
    let t = SystemMediaTransportControlsTimelineProperties::new()?;
    t.SetStartTime(span(0))?;
    t.SetEndTime(span(length))?;
    t.SetMinSeekTime(span(0))?;
    t.SetMaxSeekTime(span(length))?;
    t.SetPosition(span(position.clamp(0, length)))?;
    smtc.UpdateTimelineProperties(&t)
}

/// A second of silence: a WAV, 8 kHz, 16 bits, mono.
fn silence() -> Vec<u8> {
    let data = 16_000u32;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&8000u32.to_le_bytes());
    w.extend_from_slice(&16000u32.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    w.resize(44 + data as usize, 0);
    w
}

/// A made-up cover, 256 pixels square, as a BMP: a diagonal blend of the first two colors, with a
/// sun of the third that sits somewhere else on each track.
fn cover(colors: [[u8; 3]; 3], n: usize) -> Vec<u8> {
    const SIDE: usize = 256;
    let size = 54 + SIDE * SIDE * 3;
    let mut b = Vec::with_capacity(size);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(size as u32).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(SIDE as i32).to_le_bytes());
    b.extend_from_slice(&(SIDE as i32).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]);
    let (cx, cy, r) = [(0.68, 0.62, 0.2), (0.3, 0.35, 0.16), (0.5, 0.5, 0.24)][n % 3];
    // Rows from the bottom up, each pixel BGR
    for row in (0..SIDE).rev() {
        for col in 0..SIDE {
            let (x, y) = (col as f32 / SIDE as f32, row as f32 / SIDE as f32);
            let t = (x + y) / 2.0;
            let mut c = [0f32; 3];
            for (k, v) in c.iter_mut().enumerate() {
                *v = colors[0][k] as f32 * (1.0 - t) + colors[1][k] as f32 * t;
            }
            let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
            if d < r {
                c = colors[2].map(|v| v as f32);
            }
            b.extend_from_slice(&[c[2] as u8, c[1] as u8, c[0] as u8]);
        }
    }
    b
}
