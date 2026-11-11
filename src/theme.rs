//! Colors, fonts and the sizes shared by every widget: a palette plus the overrides of the
//! `[theme]` table. The default palette, `windows`, follows Windows: light or dark as the apps are,
//! and the accent color of Settings, read again when Windows says they changed. The other 24 are
//! the palettes of the macOS RustyBar, all on Catppuccin's color names, so that `sky` or `mauve`
//! means the same kind of color in every one.

use std::collections::HashMap;

use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::w;

use crate::config::ThemeConfig;
use crate::log;

/// A color with straight (not premultiplied) alpha, each channel 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba(0.0, 0.0, 0.0, 0.0);

    /// This color `t` of the way to `other`.
    #[allow(dead_code)]
    pub fn mix(self, other: Rgba, t: f32) -> Rgba {
        let m = |a: f32, b: f32| a + (b - a) * t;
        Rgba(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2), m(self.3, other.3))
    }

    /// This color, as opaque as `a`.
    pub fn alpha(self, a: f32) -> Rgba {
        Rgba(self.0, self.1, self.2, a)
    }

    /// For telling two colors apart in a signature, without floats.
    pub fn bits(self) -> u32 {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        c(self.0) << 24 | c(self.1) << 16 | c(self.2) << 8 | c(self.3)
    }
}

/// The colors every preset fills, in the order of `Preset::colors`: Catppuccin's, so that `sky` or
/// `mauve` in a widget, in the config or in Lua is the same kind of color in every preset.
pub const NAMES: [&str; 26] = [
    "rosewater", "flamingo", "pink", "mauve", "red", "maroon", "peach", "yellow", "green", "teal", "sky", "sapphire", "blue", "lavender",
    "text", "subtext1", "subtext0", "overlay2", "overlay1", "overlay0", "surface2", "surface1", "surface0", "base", "mantle", "crust",
];

/// A built-in palette, from the theme's own repository. A theme with fewer accents than
/// Catppuccin repeats its nearest one (Dracula has no blue: its terminal blue is purple), and the
/// shades it lacks between the text and the background are blends of the two around them.
pub struct Preset {
    pub name: &'static str,
    #[allow(dead_code)]
    pub label: &'static str,
    /// The 14 accents, then the text down to the background, as in `NAMES`.
    colors: [u32; 26],
}

impl Preset {
    pub fn color(&self, name: &str) -> Option<Rgba> {
        NAMES.iter().position(|n| *n == name).map(|i| rgb(self.colors[i]))
    }
}

/// The palettes, each from the repository of its theme.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "catppuccin-mocha",
        label: "Catppuccin Mocha",
        colors: [
            0xf5e0dc, 0xf2cdcd, 0xf5c2e7, 0xcba6f7, 0xf38ba8, 0xeba0ac, 0xfab387, 0xf9e2af, 0xa6e3a1, 0x94e2d5, 0x89dceb, 0x74c7ec, 0x89b4fa, 0xb4befe,
            0xcdd6f4, 0xbac2de, 0xa6adc8, 0x9399b2, 0x7f849c, 0x6c7086, 0x585b70, 0x45475a, 0x313244, 0x1e1e2e, 0x181825, 0x11111b,
        ],
    },
    Preset {
        name: "catppuccin-macchiato",
        label: "Catppuccin Macchiato",
        colors: [
            0xf4dbd6, 0xf0c6c6, 0xf5bde6, 0xc6a0f6, 0xed8796, 0xee99a0, 0xf5a97f, 0xeed49f, 0xa6da95, 0x8bd5ca, 0x91d7e3, 0x7dc4e4, 0x8aadf4, 0xb7bdf8,
            0xcad3f5, 0xb8c0e0, 0xa5adcb, 0x939ab7, 0x8087a2, 0x6e738d, 0x5b6078, 0x494d64, 0x363a4f, 0x24273a, 0x1e2030, 0x181926,
        ],
    },
    Preset {
        name: "catppuccin-frappe",
        label: "Catppuccin Frappé",
        colors: [
            0xf2d5cf, 0xeebebe, 0xf4b8e4, 0xca9ee6, 0xe78284, 0xea999c, 0xef9f76, 0xe5c890, 0xa6d189, 0x81c8be, 0x99d1db, 0x85c1dc, 0x8caaee, 0xbabbf1,
            0xc6d0f5, 0xb5bfe2, 0xa5adce, 0x949cbb, 0x838ba7, 0x737994, 0x626880, 0x51576d, 0x414559, 0x303446, 0x292c3c, 0x232634,
        ],
    },
    Preset {
        name: "catppuccin-latte",
        label: "Catppuccin Latte",
        colors: [
            0xdc8a78, 0xdd7878, 0xea76cb, 0x8839ef, 0xd20f39, 0xe64553, 0xfe640b, 0xdf8e1d, 0x40a02b, 0x179299, 0x04a5e5, 0x209fb5, 0x1e66f5, 0x7287fd,
            0x4c4f69, 0x5c5f77, 0x6c6f85, 0x7c7f93, 0x8c8fa1, 0x9ca0b0, 0xacb0be, 0xbcc0cc, 0xccd0da, 0xeff1f5, 0xe6e9ef, 0xdce0e8,
        ],
    },
    // Dracula's bright colors are its softer accents
    Preset {
        name: "dracula",
        label: "Dracula",
        colors: [
            0xff92df, 0xff6e6e, 0xff79c6, 0xbd93f9, 0xff5555, 0xff6e6e, 0xffb86c, 0xf1fa8c, 0x50fa7b, 0x8be9fd, 0xa4ffff, 0x8be9fd, 0xbd93f9, 0xd6acff,
            0xf8f8f2, 0xdadde2, 0xbcc2d3, 0x9ea8c3, 0x808db4, 0x6272a4, 0x535c7f, 0x44475a, 0x343746, 0x282a36, 0x21222c, 0x191a21,
        ],
    },
    Preset {
        name: "everforest-dark",
        label: "Everforest Dark",
        colors: [
            0xd699b6, 0xe67e80, 0xd699b6, 0xd699b6, 0xe67e80, 0xe67e80, 0xe69875, 0xdbbc7f, 0xa7c080, 0x83c092, 0x7fbbb3, 0x7fbbb3, 0x7fbbb3, 0x7fbbb3,
            0xd3c6aa, 0xc1bca7, 0xafb3a3, 0x9da9a0, 0x859289, 0x7a8478, 0x4f585e, 0x475258, 0x3d484d, 0x2d353b, 0x232a2e, 0x1a2022,
        ],
    },
    Preset {
        name: "everforest-light",
        label: "Everforest Light",
        colors: [
            0xdf69ba, 0xf85552, 0xdf69ba, 0xdf69ba, 0xf85552, 0xf85552, 0xf57d26, 0xdfa000, 0x8da101, 0x35a77c, 0x3a94c5, 0x3a94c5, 0x3a94c5, 0x3a94c5,
            0x5c6a72, 0x697777, 0x75847c, 0x829181, 0x939f91, 0xa6b0a0, 0xb2baa8, 0xbdc3af, 0xe0dcc7, 0xfdf6e3, 0xf4f0d9, 0xefebd4,
        ],
    },
    // Gruvbox, medium contrast: its bright colors on the dark one, the faded ones on the light one
    Preset {
        name: "gruvbox-dark",
        label: "Gruvbox Dark",
        colors: [
            0xd3869b, 0xfb4934, 0xd3869b, 0xd3869b, 0xfb4934, 0xfb4934, 0xfe8019, 0xfabd2f, 0xb8bb26, 0x8ec07c, 0x83a598, 0x458588, 0x83a598, 0x83a598,
            0xebdbb2, 0xd5c4a1, 0xbdae93, 0xa89984, 0x928374, 0x7c6f64, 0x665c54, 0x504945, 0x3c3836, 0x282828, 0x1d2021, 0x161819,
        ],
    },
    Preset {
        name: "gruvbox-light",
        label: "Gruvbox Light",
        colors: [
            0x8f3f71, 0x9d0006, 0x8f3f71, 0x8f3f71, 0x9d0006, 0xcc241d, 0xaf3a03, 0xb57614, 0x79740e, 0x427b58, 0x076678, 0x458588, 0x076678, 0x076678,
            0x3c3836, 0x504945, 0x665c54, 0x7c6f64, 0x928374, 0xa89984, 0xbdae93, 0xd5c4a1, 0xebdbb2, 0xfbf1c7, 0xf2e5bc, 0xeee0b7,
        ],
    },
    Preset {
        name: "kanagawa-wave",
        label: "Kanagawa Wave",
        colors: [
            0xd27e99, 0xe46876, 0xd27e99, 0x957fb8, 0xe46876, 0xff5d62, 0xffa066, 0xe6c384, 0x98bb6c, 0x7aa89f, 0xa3d4d5, 0x7fb4ca, 0x7e9cd8, 0x9cabca,
            0xdcd7ba, 0xc8c093, 0xb2ac88, 0x9d987e, 0x888574, 0x727169, 0x54546d, 0x363646, 0x2a2a37, 0x1f1f28, 0x181820, 0x16161d,
        ],
    },
    Preset {
        name: "kanagawa-dragon",
        label: "Kanagawa Dragon",
        colors: [
            0xa292a3, 0xc4746e, 0xa292a3, 0x8992a7, 0xc4746e, 0xc4746e, 0xb6927b, 0xc4b28a, 0x87a987, 0x8ea4a2, 0x8ba4b0, 0x658594, 0x8ba4b0, 0x949fb5,
            0xc5c9c5, 0xb6b8b0, 0xa6a69c, 0x9e9b93, 0x7a8382, 0x737c73, 0x625e5a, 0x393836, 0x282727, 0x181616, 0x12120f, 0x0d0c0c,
        ],
    },
    Preset {
        name: "kanagawa-lotus",
        label: "Kanagawa Lotus",
        colors: [
            0xb35b79, 0xd7474b, 0xb35b79, 0x624c83, 0xc84053, 0xd7474b, 0xcc6d00, 0xde9800, 0x6f894e, 0x597b75, 0x4e8ca2, 0x6693bf, 0x4d699b, 0x5d57a3,
            0x545464, 0x5f5f6a, 0x6a696f, 0x747475, 0x7f7e7a, 0x8a8980, 0xa09cac, 0xbab5a8, 0xd5cea3, 0xf2ecbc, 0xe5ddb0, 0xdcd5ac,
        ],
    },
    // Nord has nothing darker than nord0: mantle and crust darken it
    Preset {
        name: "nord",
        label: "Nord",
        colors: [
            0xd08770, 0xbf616a, 0xb48ead, 0xb48ead, 0xbf616a, 0xbf616a, 0xd08770, 0xebcb8b, 0xa3be8c, 0x8fbcbb, 0x88c0d0, 0x81a1c1, 0x81a1c1, 0x5e81ac,
            0xd8dee9, 0xc0c8d6, 0xa8b1c2, 0x919baf, 0x79849b, 0x616e88, 0x4c566a, 0x434c5e, 0x3b4252, 0x2e3440, 0x272c36, 0x20242d,
        ],
    },
    Preset {
        name: "one-dark",
        label: "One Dark",
        colors: [
            0xe06c75, 0xe06c75, 0xc678dd, 0xc678dd, 0xe06c75, 0xbe5046, 0xd19a66, 0xe5c07b, 0x98c379, 0x56b6c2, 0x56b6c2, 0x61afef, 0x61afef, 0x528bff,
            0xabb2bf, 0x9da4b2, 0x9097a4, 0x828997, 0x6f7684, 0x5c6370, 0x545a6a, 0x4b5263, 0x3e4452, 0x282c34, 0x21252b, 0x181a1f,
        ],
    },
    Preset {
        name: "one-light",
        label: "One Light",
        colors: [
            0xe45649, 0xe45649, 0xa626a4, 0xa626a4, 0xe45649, 0xca1243, 0x986801, 0xc18401, 0x50a14f, 0x0184bc, 0x0184bc, 0x4078f2, 0x4078f2, 0x526fff,
            0x383a42, 0x484b54, 0x595b65, 0x696c77, 0x84868f, 0xa0a1a7, 0xc2c2c3, 0xd0d0d0, 0xdfdfdf, 0xfafafa, 0xf0f0f0, 0xe7e9e1,
        ],
    },
    // Rosé Pine has no orange (gold) and a single green (leaf)
    Preset {
        name: "rose-pine",
        label: "Rosé Pine",
        colors: [
            0xebbcba, 0xebbcba, 0xeb6f92, 0xc4a7e7, 0xeb6f92, 0xeb6f92, 0xf6c177, 0xf6c177, 0x95b1ac, 0x31748f, 0x9ccfd8, 0x31748f, 0x9ccfd8, 0xc4a7e7,
            0xe0def4, 0xc5c3db, 0xaba7c3, 0x908caa, 0x7f7b98, 0x6e6a86, 0x605c76, 0x524f67, 0x403d52, 0x191724, 0x16141f, 0x100f17,
        ],
    },
    Preset {
        name: "rose-pine-moon",
        label: "Rosé Pine Moon",
        colors: [
            0xea9a97, 0xea9a97, 0xeb6f92, 0xc4a7e7, 0xeb6f92, 0xeb6f92, 0xf6c177, 0xf6c177, 0x95b1ac, 0x3e8fb0, 0x9ccfd8, 0x3e8fb0, 0x9ccfd8, 0xc4a7e7,
            0xe0def4, 0xc5c3db, 0xaba7c3, 0x908caa, 0x7f7b98, 0x6e6a86, 0x625e7a, 0x56526e, 0x44415a, 0x232136, 0x1f1d30, 0x171624,
        ],
    },
    Preset {
        name: "rose-pine-dawn",
        label: "Rosé Pine Dawn",
        colors: [
            0xd7827e, 0xd7827e, 0xb4637a, 0x907aa9, 0xb4637a, 0xb4637a, 0xea9d34, 0xea9d34, 0x6d8f89, 0x286983, 0x56949f, 0x286983, 0x56949f, 0x907aa9,
            0x464261, 0x575372, 0x686482, 0x797593, 0x88849c, 0x9893a5, 0xb3aeb9, 0xcecacd, 0xdfdad9, 0xfaf4ed, 0xf8f0e7, 0xf4ede8,
        ],
    },
    // Solarized: the same accents on both
    Preset {
        name: "solarized-dark",
        label: "Solarized Dark",
        colors: [
            0xd33682, 0xdc322f, 0xd33682, 0x6c71c4, 0xdc322f, 0xdc322f, 0xcb4b16, 0xb58900, 0x859900, 0x2aa198, 0x2aa198, 0x268bd2, 0x268bd2, 0x6c71c4,
            0x93a1a1, 0x839496, 0x74888c, 0x657b83, 0x5e747c, 0x586e75, 0x3d5b64, 0x224953, 0x073642, 0x002b36, 0x00252e, 0x001e26,
        ],
    },
    Preset {
        name: "solarized-light",
        label: "Solarized Light",
        colors: [
            0xd33682, 0xdc322f, 0xd33682, 0x6c71c4, 0xdc322f, 0xdc322f, 0xcb4b16, 0xb58900, 0x859900, 0x2aa198, 0x2aa198, 0x268bd2, 0x268bd2, 0x6c71c4,
            0x586e75, 0x657b83, 0x74888c, 0x839496, 0x8b9a9c, 0x93a1a1, 0xb1b9b2, 0xd0d0c4, 0xeee8d5, 0xfdf6e3, 0xf8f1de, 0xf3edda,
        ],
    },
    Preset {
        name: "tokyo-night",
        label: "Tokyo Night",
        colors: [
            0xf7768e, 0xf7768e, 0xff007c, 0xbb9af7, 0xf7768e, 0xdb4b4b, 0xff9e64, 0xe0af68, 0x9ece6a, 0x73daca, 0x7dcfff, 0x2ac3de, 0x7aa2f7, 0x9d7cd8,
            0xc0caf5, 0xa9b1d6, 0x8e96bc, 0x737aa2, 0x565f89, 0x545c7e, 0x414868, 0x3b4261, 0x292e42, 0x1a1b26, 0x16161e, 0x0c0e14,
        ],
    },
    Preset {
        name: "tokyo-night-storm",
        label: "Tokyo Night Storm",
        colors: [
            0xf7768e, 0xf7768e, 0xff007c, 0xbb9af7, 0xf7768e, 0xdb4b4b, 0xff9e64, 0xe0af68, 0x9ece6a, 0x73daca, 0x7dcfff, 0x2ac3de, 0x7aa2f7, 0x9d7cd8,
            0xc0caf5, 0xa9b1d6, 0x8e96bc, 0x737aa2, 0x646c96, 0x565f89, 0x545c7e, 0x414868, 0x3b4261, 0x24283b, 0x1f2335, 0x1b1e2d,
        ],
    },
    Preset {
        name: "tokyo-night-moon",
        label: "Tokyo Night Moon",
        colors: [
            0xfca7ea, 0xff757f, 0xfca7ea, 0xc099ff, 0xff757f, 0xc53b53, 0xff966c, 0xffc777, 0xc3e88d, 0x4fd6be, 0x86e1fc, 0x65bcff, 0x82aaff, 0x82aaff,
            0xc8d3f5, 0xa5afd6, 0x828bb8, 0x737aa2, 0x636da6, 0x545c7e, 0x444a73, 0x3b4261, 0x2f334d, 0x222436, 0x1e2030, 0x191b29,
        ],
    },
    Preset {
        name: "tokyo-night-day",
        label: "Tokyo Night Day",
        colors: [
            0xf52a65, 0xf52a65, 0xd20065, 0x9854f1, 0xf52a65, 0xc64343, 0xb15c00, 0x8c6c3e, 0x587539, 0x387068, 0x007197, 0x188092, 0x2e7de9, 0x7847bd,
            0x3760bf, 0x68709a, 0x6172b0, 0x767ea8, 0x848cb5, 0x8990b3, 0xa1a6c5, 0xa8aecb, 0xc4c8da, 0xe1e2e7, 0xd0d5e3, 0xc1c9df,
        ],
    },
];

/// The palette that follows Windows, and the default.
pub const WINDOWS: &str = "windows";

/// A preset's name as written in the config, as its name.
fn normalize(name: &str) -> String {
    name.trim().to_lowercase().replace('é', "e").replace([' ', '_'], "-")
}

/// The built-in palette called `name`, also written as its label (`Rosé Pine Moon`), and
/// Catppuccin's flavors without the prefix (`latte`).
pub fn preset(name: &str) -> Option<&'static Preset> {
    let name = normalize(name);
    PRESETS.iter().find(|p| p.name == name || p.name.strip_prefix("catppuccin-") == Some(name.as_str()))
}

pub fn is_windows(name: &str) -> bool {
    normalize(name) == WINDOWS
}

pub fn known(name: &str) -> bool {
    is_windows(name) || preset(name).is_some()
}

fn rgb(v: u32) -> Rgba {
    let c = |shift: u32| ((v >> shift) & 0xff) as f32 / 255.0;
    Rgba(c(16), c(8), c(0), 1.0)
}

/// Whether apps are light, as Settings > Personalization > Colors has it.
fn light_mode() -> bool {
    let mut value: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    let found = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    found.is_err() || value != 0
}

/// The accent as Windows draws controls with it: a lighter shade on dark, a darker one on light
/// (the accent palette Windows derives from the color chosen in Settings).
fn accent(light: bool) -> Rgba {
    let mut palette = [0u8; 32];
    let mut size = palette.len() as u32;
    let found = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"),
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(palette.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if found.is_ok() && size >= 32 {
        // Eight RGBA shades from the lightest: dark mode fills with the second, light with the fifth
        let i = if light { 4 * 4 } else { 4 };
        let c = |b: u8| b as f32 / 255.0;
        return Rgba(c(palette[i]), c(palette[i + 1]), c(palette[i + 2]), 1.0);
    }
    if light { rgb(0x005fb8) } else { rgb(0x60cdff) }
}

/// Windows' colors on Catppuccin's names: opaque grays near those of Windows 11's controls, and
/// accents near its system colors. `crust` is the text on colored fills, as in the other palettes.
fn windows_palette() -> Vec<(&'static str, Rgba)> {
    let light = light_mode();
    let accent = accent(light);
    let colors: [u32; 26] = if light {
        [
            0xc239b3, 0xe3008c, 0xbf0077, 0x8764b8, 0xc42b1c, 0xa4262c, 0xca5010, 0x9d5d00, 0x0f7b0f, 0x038387, 0x0078d4, 0x005fb8, 0x005fb8, 0x6b69d6,
            0x1a1a1a, 0x3d3d3d, 0x5c5c5c, 0x6e6e6e, 0x7a7a7a, 0x8a8a8a, 0xc8c8c8, 0xd6d6d6, 0xe5e5e5, 0xf3f3f3, 0xeaeaea, 0xffffff,
        ]
    } else {
        [
            0xffb3c1, 0xff99a4, 0xff8ad8, 0xc8a4ff, 0xff99a4, 0xff7b86, 0xffb86c, 0xfce100, 0x6ccb5f, 0x5ad1c5, 0x60cdff, 0x4cc2ff, 0x60cdff, 0xb4a0ff,
            0xffffff, 0xe0e0e0, 0xc5c5c5, 0xa8a8a8, 0x9d9d9d, 0x8a8a8a, 0x4a4a4a, 0x3d3d3d, 0x2d2d2d, 0x202020, 0x1c1c1c, 0x000000,
        ]
    };
    let mut palette: Vec<(&str, Rgba)> = NAMES.iter().copied().zip(colors.iter().map(|v| rgb(*v))).collect();
    palette.push(("accent", accent));
    palette.push(("on_primary", if light { rgb(0xffffff) } else { rgb(0x000000) }));
    palette
}

/// The colors of the preset called `name`, by palette name, with `accent` (a palette's `mauve`).
fn palette(name: &str) -> Option<Vec<(&'static str, Rgba)>> {
    if is_windows(name) {
        return Some(windows_palette());
    }
    let p = preset(name)?;
    let mut colors: Vec<(&str, Rgba)> = NAMES.iter().copied().zip(p.colors.iter().map(|v| rgb(*v))).collect();
    colors.extend(p.color("mauve").map(|c| ("accent", c)));
    Some(colors)
}

/// A palette name or a hex color (`#rrggbb`, `#rrggbbaa`).
fn resolve(palette: &HashMap<String, Rgba>, s: &str) -> Option<Rgba> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        let v = u32::from_str_radix(hex, 16).ok()?;
        let c = |shift: u32| ((v >> shift) & 0xff) as f32 / 255.0;
        return match hex.len() {
            6 => Some(rgb(v)),
            8 => Some(Rgba(c(24), c(16), c(8), c(0))),
            _ => None,
        };
    }
    palette.get(s).copied()
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub palette: HashMap<String, Rgba>,
    pub primary: Rgba,
    pub on_primary: Rgba,
    pub text: Rgba,
    pub subtext: Rgba,
    /// Text that matters less: the weekends of the calendar.
    pub overlay: Rgba,
    pub surface: Rgba,
    pub base: Rgba,
    pub font: String,
    pub font_size: f32,
    pub small_size: f32,
    pub pill_height: f32,
    pub pill_radius: f32,
    pub pill_border: f32,
    pub pill_padding: f32,
    pub item_gap: f32,
    pub popup_radius: f32,
    pub popup_border: f32,
    /// The corners of what can be clicked in a popup: a button, a row.
    pub row_radius: f32,
    pub animations: bool,
    /// The palette follows Windows: read again when light/dark or the accent change.
    pub follows_windows: bool,
}

impl Theme {
    pub fn from_config(c: &ThemeConfig) -> Theme {
        let (colors, follows_windows) = match palette(&c.preset) {
            Some(colors) => (colors, is_windows(&c.preset)),
            None => {
                log!("unknown theme preset {:?}, using {WINDOWS}", c.preset);
                (windows_palette(), true)
            }
        };
        let mut palette: HashMap<String, Rgba> = colors.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        palette.insert("transparent".into(), Rgba::TRANSPARENT);
        palette.insert("white".into(), Rgba(1.0, 1.0, 1.0, 1.0));
        palette.insert("black".into(), Rgba(0.0, 0.0, 0.0, 1.0));
        for (name, value) in &c.colors {
            match resolve(&palette, value) {
                Some(color) => {
                    palette.insert(name.clone(), color);
                }
                None => log!("[theme.colors] {name} = {value:?}: not a palette name or a hex color"),
            }
        }
        let get = |name: &str, fallback: &str| {
            resolve(&palette, name).or_else(|| resolve(&palette, fallback)).unwrap_or(Rgba(1.0, 0.0, 1.0, 1.0))
        };
        let theme = Theme {
            primary: get(&c.primary, "accent"),
            on_primary: get("on_primary", "crust"),
            text: get("text", "text"),
            subtext: get("subtext", "subtext0"),
            overlay: get("overlay", "overlay0"),
            surface: get("surface", "surface0"),
            base: get("base", "base"),
            palette: HashMap::new(),
            font: c.font.clone(),
            font_size: c.font_size,
            small_size: c.small_size,
            pill_height: c.pill_height,
            pill_radius: c.pill_radius,
            pill_border: c.pill_border,
            pill_padding: c.pill_padding,
            item_gap: c.item_gap,
            popup_radius: c.popup_radius,
            popup_border: c.popup_border,
            row_radius: c.row_radius,
            animations: c.animations,
            follows_windows,
        };
        palette.insert("primary".into(), theme.primary);
        palette.insert("on_primary".into(), theme.on_primary);
        Theme { palette, ..theme }
    }

    /// A palette name or a hex color.
    pub fn color(&self, s: &str) -> Option<Rgba> {
        resolve(&self.palette, s)
    }

    /// `normal` below `warning`, yellow from it, red from `critical`.
    pub fn level(&self, value: f64, warning: f64, critical: f64, normal: Rgba) -> Rgba {
        if value >= critical {
            self.color("red").unwrap_or(normal)
        } else if value >= warning {
            self.color("yellow").unwrap_or(normal)
        } else {
            normal
        }
    }

    /// The background of a tile in a popup: the text color, faintly; tinted with `color` when it
    /// warns.
    pub fn tile(&self, color: Rgba, alert: bool) -> Rgba {
        if alert { color.alpha(0.16) } else { self.text.alpha(0.05) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_has_a_name_and_resolves() {
        assert_eq!(PRESETS.len(), 24);
        for p in PRESETS {
            assert!(preset(p.name).is_some(), "{}", p.name);
            assert!(preset(p.label).is_some(), "{}", p.label);
        }
        assert!(preset("latte").is_some());
        assert!(known("Windows"));
    }

    #[test]
    fn hex_colors() {
        let p = HashMap::new();
        assert_eq!(resolve(&p, "#ff0000"), Some(Rgba(1.0, 0.0, 0.0, 1.0)));
        assert_eq!(resolve(&p, "#00000000"), Some(Rgba(0.0, 0.0, 0.0, 0.0)));
        assert_eq!(resolve(&p, "#123"), None);
    }
}
