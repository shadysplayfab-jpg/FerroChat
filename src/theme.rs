//! Colour palettes. The view reads the active one through [`p`]; Settings
//! switches it with [`set`] (built-in) or [`set_custom`] (a CSS file).
//!
//! Custom themes are `.css` files in `%APPDATA%\ferrochat\themes`. iced can't
//! run real CSS, so only custom properties are read: `--chat: #14121a;` and so
//! on, anywhere in the file (selectors are ignored). Themes are local: only
//! you see them.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock};

use iced::widget::image::Handle;
use iced::Color;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
    #[default]
    Dark,
    /// Near-black, for OLED screens and dark rooms.
    Midnight,
    Light,
    /// Cool blue-grey.
    Slate,
    /// Warm brown.
    Mocha,
    /// Deep green.
    Forest,
    /// Dark purple.
    Plum,
    /// Warm off-white, easy on the eyes in daylight.
    Paper,
    /// Black and white with strong edges.
    #[serde(rename = "high-contrast")]
    HighContrast,
}

impl ThemeName {
    pub const ALL: [ThemeName; 9] = [
        ThemeName::Dark,
        ThemeName::Midnight,
        ThemeName::Light,
        ThemeName::Slate,
        ThemeName::Mocha,
        ThemeName::Forest,
        ThemeName::Plum,
        ThemeName::Paper,
        ThemeName::HighContrast,
    ];

    fn is_light(self) -> bool {
        matches!(self, ThemeName::Light | ThemeName::Paper)
    }

    /// The name custom themes use for `--base`.
    fn key(self) -> &'static str {
        match self {
            ThemeName::Dark => "dark",
            ThemeName::Midnight => "midnight",
            ThemeName::Light => "light",
            ThemeName::Slate => "slate",
            ThemeName::Mocha => "mocha",
            ThemeName::Forest => "forest",
            ThemeName::Plum => "plum",
            ThemeName::Paper => "paper",
            ThemeName::HighContrast => "high-contrast",
        }
    }
}

impl fmt::Display for ThemeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ThemeName::Dark => "Dark",
            ThemeName::Midnight => "Midnight",
            ThemeName::Light => "Light",
            ThemeName::Slate => "Slate",
            ThemeName::Mocha => "Mocha",
            ThemeName::Forest => "Forest",
            ThemeName::Plum => "Plum",
            ThemeName::Paper => "Paper",
            ThemeName::HighContrast => "High contrast",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Accent {
    #[default]
    Ember,
    Ocean,
    Forest,
    Violet,
    Rose,
}

impl Accent {
    pub const ALL: [Accent; 5] = [Accent::Ember, Accent::Ocean, Accent::Forest, Accent::Violet, Accent::Rose];

    pub fn color(self) -> Color {
        // Each accent names a material, not a stock hue:
        //   Ember   forge-red copper (not terracotta)
        //   Ocean   steel teal (not Discord blurple)
        //   Forest  mossy pine (not acid green)
        //   Violet  twilight amethyst
        //   Rose    oxidized rose-copper (not candy pink)
        match self {
            Accent::Ember => rgb(0xc0, 0x5a, 0x3c),
            Accent::Ocean => rgb(0x4a, 0x9c, 0xb8),
            Accent::Forest => rgb(0x5a, 0x8a, 0x4a),
            Accent::Violet => rgb(0x7b, 0x5b, 0xc8),
            Accent::Rose => rgb(0xc8, 0x5a, 0x7a),
        }
    }
}

impl fmt::Display for Accent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Accent::Ember => "Ember",
            Accent::Ocean => "Ocean",
            Accent::Forest => "Forest",
            Accent::Violet => "Violet",
            Accent::Rose => "Rose",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub rail: Color,
    pub sidebar: Color,
    pub chat: Color,
    pub selected: Color,
    pub accent: Color,
    pub text: Color,
    pub muted: Color,
    pub mention: Color,
    pub error: Color,
    pub link: Color,
    pub code_bg: Color,
    pub pill_bg: Color,
    pub pill_fg: Color,
    pub spoiler: Color,
    pub spoiler_open: Color,
    pub mentioned_row: Color,
    pub highlight_row: Color,
    pub embed_bg: Color,
    pub online: Color,
    pub idle: Color,
    pub dnd: Color,
    pub backdrop: Color,
}

impl Palette {
    /// The colour a custom-theme property sets (`--code-bg` → `code_bg`).
    fn field(&mut self, name: &str) -> Option<&mut Color> {
        Some(match name {
            "rail" => &mut self.rail,
            "sidebar" => &mut self.sidebar,
            "chat" => &mut self.chat,
            "selected" => &mut self.selected,
            "accent" => &mut self.accent,
            "text" => &mut self.text,
            "muted" => &mut self.muted,
            "mention" => &mut self.mention,
            "error" => &mut self.error,
            "link" => &mut self.link,
            "code-bg" => &mut self.code_bg,
            "pill-bg" => &mut self.pill_bg,
            "pill-fg" => &mut self.pill_fg,
            "spoiler" => &mut self.spoiler,
            "spoiler-open" => &mut self.spoiler_open,
            "mentioned-row" => &mut self.mentioned_row,
            "highlight-row" => &mut self.highlight_row,
            "embed-bg" => &mut self.embed_bg,
            "online" => &mut self.online,
            "idle" => &mut self.idle,
            "dnd" => &mut self.dnd,
            "backdrop" => &mut self.backdrop,
            _ => return None,
        })
    }

    /// Colours derived from chat, text and accent, unless the theme set them.
    fn derive(&mut self, light: bool, set: &[String]) {
        let unset = |name: &str| !set.iter().any(|s| s == name);
        if unset("pill-bg") {
            self.pill_bg = mix(self.chat, self.accent, 0.25);
        }
        if unset("pill-fg") {
            // Readable against the pill in either brightness.
            self.pill_fg = if light { mix(self.accent, Color::BLACK, 0.35) } else { mix(self.accent, Color::WHITE, 0.45) };
        }
        if unset("mentioned-row") {
            self.mentioned_row = mix(self.chat, self.accent, 0.12);
        }
        if unset("highlight-row") {
            self.highlight_row = mix(self.chat, self.accent, 0.22);
        }
    }
}

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

/// `a` moved `t` of the way towards `b`.
fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::from_rgb(a.r + (b.r - a.r) * t, a.g + (b.g - a.g) * t, a.b + (b.b - a.b) * t)
}

/// Surfaces per theme: (rail, sidebar, chat, selected, text, muted, code, embed, link).
fn surfaces(theme: ThemeName) -> [Color; 9] {
    let blue = rgb(0x6c, 0xb4, 0xf5);
    match theme {
        // "Iron Dusk": cool-cast iron surfaces, a warm off-white, a desaturated
        // blue link. The accent lands warm against the cool bg (that contrast
        // is the identity, not a decorative choice).
        ThemeName::Dark => [
            rgb(0x0f, 0x12, 0x17), rgb(0x18, 0x1b, 0x22), rgb(0x20, 0x24, 0x2c), rgb(0x31, 0x37, 0x44),
            rgb(0xec, 0xe7, 0xdd), rgb(0x8d, 0x93, 0xa0), rgb(0x17, 0x1a, 0x20), rgb(0x1b, 0x1f, 0x25),
            rgb(0x86, 0xb4, 0xe2),
        ],
        // "Obsidian": true-black rail for OLED, faintest cool cast on the
        // surfaces so they feel glassy; warm off-white text against cold black.
        ThemeName::Midnight => [
            rgb(0x00, 0x00, 0x00), rgb(0x07, 0x07, 0x0a), rgb(0x0e, 0x0f, 0x14), rgb(0x22, 0x23, 0x2a),
            rgb(0xe8, 0xe4, 0xd9), rgb(0x7c, 0x80, 0x8a), rgb(0x05, 0x05, 0x07), rgb(0x0a, 0x0b, 0x0f), blue,
        ],
        // "Daylight": cool-cast neutrals, white as the focus, a deeper link
        // blue so hyperlinks actually read as hyperlinks on light bg.
        ThemeName::Light => [
            rgb(0xde, 0xdf, 0xe3), rgb(0xed, 0xef, 0xf2), rgb(0xff, 0xff, 0xff), rgb(0xd3, 0xd7, 0xdf),
            rgb(0x1c, 0x1f, 0x26), rgb(0x5a, 0x60, 0x72), rgb(0xee, 0xf0, 0xf3), rgb(0xf4, 0xf6, 0xf9),
            rgb(0x00, 0x57, 0xc7),
        ],
        // "Harbor Twilight": pushes bluer and deeper than ordinary slate; the
        // chat surface is the warmest middle, so it reads as the focus.
        ThemeName::Slate => [
            rgb(0x0d, 0x13, 0x1a), rgb(0x18, 0x21, 0x30), rgb(0x21, 0x2b, 0x3c), rgb(0x35, 0x45, 0x5e),
            rgb(0xea, 0xf0, 0xf7), rgb(0x8d, 0x9b, 0xae), rgb(0x14, 0x1c, 0x28), rgb(0x1a, 0x23, 0x32), rgb(0x7c, 0xc4, 0xff),
        ],
        // "Roastery": dark roast → medium roast → crema, with cream text.
        ThemeName::Mocha => [
            rgb(0x18, 0x11, 0x08), rgb(0x24, 0x1a, 0x10), rgb(0x2e, 0x22, 0x1a), rgb(0x4a, 0x3a, 0x2d),
            rgb(0xf3, 0xe8, 0xd9), rgb(0xa8, 0x95, 0x83), rgb(0x1f, 0x17, 0x0e), rgb(0x28, 0x1e, 0x14), rgb(0x8f, 0xc1, 0xe8),
        ],
        // "Pine Shade": deep pine floor → moss, slight cool undertone in the
        // deeps to keep it from reading as fake saturated green.
        ThemeName::Forest => [
            rgb(0x0b, 0x14, 0x0e), rgb(0x14, 0x20, 0x19), rgb(0x1c, 0x2a, 0x22), rgb(0x2e, 0x45, 0x36),
            rgb(0xe3, 0xf0, 0xe6), rgb(0x8a, 0xa3, 0x94), rgb(0x10, 0x1a, 0x13), rgb(0x16, 0x21, 0x1a), rgb(0x7f, 0xcf, 0xd8),
        ],
        // "Twilight Amethyst": deepest twilight → amethyst shadow; no pink lean.
        ThemeName::Plum => [
            rgb(0x13, 0x0d, 0x1c), rgb(0x1e, 0x16, 0x28), rgb(0x28, 0x1f, 0x35), rgb(0x3f, 0x32, 0x54),
            rgb(0xf0, 0xe8, 0xfa), rgb(0xa0, 0x92, 0xb5), rgb(0x18, 0x11, 0x23), rgb(0x22, 0x18, 0x2e), rgb(0x9c, 0xb8, 0xff),
        ],
        // "Fieldbook": manila rail → cream page → sepia ink. Reads as an aged
        // notebook, not the generic "warm light theme" cream everyone makes.
        ThemeName::Paper => [
            rgb(0xe4, 0xdd, 0xd0), rgb(0xef, 0xe9, 0xdd), rgb(0xfa, 0xf5, 0xec), rgb(0xdb, 0xd0, 0xbe),
            rgb(0x2a, 0x24, 0x1c), rgb(0x6c, 0x60, 0x4e), rgb(0xed, 0xe6, 0xd8), rgb(0xef, 0xe9, 0xdd), rgb(0x1f, 0x5f, 0xa8),
        ],
        ThemeName::HighContrast => [
            rgb(0x00, 0x00, 0x00), rgb(0x00, 0x00, 0x00), rgb(0x00, 0x00, 0x00), rgb(0x40, 0x40, 0x40),
            Color::WHITE, rgb(0xc8, 0xc8, 0xc8), rgb(0x14, 0x14, 0x14), rgb(0x0a, 0x0a, 0x0a), rgb(0x66, 0xcc, 0xff),
        ],
    }
}

fn build_with(theme: ThemeName, accent: Color) -> Palette {
    let [rail, sidebar, chat, selected, text, muted, code_bg, embed_bg, link] = surfaces(theme);
    let light = theme.is_light();
    let mut palette = Palette {
        rail,
        sidebar,
        chat,
        selected,
        accent,
        text,
        muted,
        mention: rgb(0xd9, 0x3f, 0x3f),
        error: if light { rgb(0xc8, 0x2f, 0x2f) } else { rgb(0xf0, 0x70, 0x70) },
        link,
        code_bg,
        pill_bg: Color::BLACK,
        pill_fg: Color::BLACK,
        // Hidden spoilers are a solid block in the text colour's opposite.
        spoiler: if light { text } else { mix(code_bg, Color::BLACK, 0.2) },
        spoiler_open: selected,
        mentioned_row: Color::BLACK,
        highlight_row: Color::BLACK,
        embed_bg,
        online: rgb(0x3b, 0xa5, 0x5d),
        idle: rgb(0xfa, 0xa8, 0x1a),
        dnd: rgb(0xed, 0x42, 0x45),
        backdrop: Color { a: 0.6, ..Color::BLACK },
    };
    palette.derive(light, &[]);
    palette
}

const COMBINATIONS: usize = ThemeName::ALL.len() * Accent::ALL.len();
static PALETTES: OnceLock<[Palette; COMBINATIONS]> = OnceLock::new();
/// The active palette: a built-in one, or a leaked custom one (a few hundred
/// bytes per theme change, so the view can borrow it for `'static`).
static CURRENT: AtomicPtr<Palette> = AtomicPtr::new(std::ptr::null_mut());
static WALLPAPER: Mutex<Option<Wallpaper>> = Mutex::new(None);

#[derive(Debug, Clone)]
pub struct Wallpaper {
    pub image: Handle,
    pub opacity: f32,
}

fn index(theme: ThemeName, accent: Accent) -> usize {
    ThemeName::ALL.iter().position(|t| *t == theme).unwrap_or(0) * Accent::ALL.len()
        + Accent::ALL.iter().position(|a| *a == accent).unwrap_or(0)
}

fn palettes() -> &'static [Palette; COMBINATIONS] {
    PALETTES.get_or_init(|| {
        std::array::from_fn(|i| build_with(ThemeName::ALL[i / Accent::ALL.len()], Accent::ALL[i % Accent::ALL.len()].color()))
    })
}

/// The active palette.
pub fn p() -> &'static Palette {
    let current = CURRENT.load(Ordering::Acquire);
    if current.is_null() {
        return &palettes()[0];
    }
    // Only ever points at the static table or a leaked (never freed) palette.
    unsafe { &*current }
}

/// The wallpaper behind the chat, if the custom theme has one.
pub fn wallpaper() -> Option<Wallpaper> {
    WALLPAPER.lock().ok()?.clone()
}

fn activate(palette: &'static Palette, name: String) -> iced::Theme {
    CURRENT.store(palette as *const Palette as *mut Palette, Ordering::Release);
    iced::Theme::custom(
        name,
        iced::theme::Palette {
            background: palette.chat,
            text: palette.text,
            primary: palette.accent,
            success: palette.online,
            danger: palette.mention,
        },
    )
}

/// Switches to a built-in palette and returns the matching iced theme.
pub fn set(theme: ThemeName, accent: Accent) -> iced::Theme {
    if let Ok(mut wallpaper) = WALLPAPER.lock() {
        *wallpaper = None;
    }
    activate(&palettes()[index(theme, accent)], format!("Ferrochat {theme} {accent}"))
}

/// Folder of custom themes; `None` if Windows has no app-data folder.
pub fn themes_dir() -> Option<PathBuf> {
    // The offline demo can point at a scratch folder instead of the real one.
    #[cfg(feature = "demo")]
    if let Some(dir) = std::env::var_os("FERROCHAT_DEMO_THEMES_DIR") {
        return Some(PathBuf::from(dir));
    }
    Some(crate::settings::config_dir()?.join("themes"))
}

/// Custom theme names (file names without `.css`), sorted.
pub fn custom_themes() -> Vec<String> {
    let Some(dir) = themes_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("css")))
        .filter_map(|p| Some(p.file_stem()?.to_str()?.to_owned()))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// Creates the themes folder with an example theme the first time, and returns it.
pub fn ensure_themes_dir() -> Result<PathBuf, String> {
    let dir = themes_dir().ok_or("no app-data folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let example = dir.join("example.css");
    if custom_themes().is_empty() && !example.exists() {
        std::fs::write(&example, EXAMPLE).map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

/// Loads `<themes>/<name>.css` and switches to it. Returns the iced theme and
/// problems found in the file (unknown properties, bad colours, a missing
/// wallpaper), which don't stop the rest of the theme from applying.
pub fn set_custom(name: &str, accent: Accent) -> Result<(iced::Theme, Vec<String>), String> {
    let dir = themes_dir().ok_or("no app-data folder")?;
    if name.contains(['/', '\\']) || name.contains("..") {
        return Err(format!("invalid theme name: {name}"));
    }
    let text = std::fs::read_to_string(dir.join(format!("{name}.css"))).map_err(|e| format!("could not read {name}.css: {e}"))?;
    let mut sheet = parse(&text, accent);
    let wallpaper = match &sheet.wallpaper {
        Some(file) => match load_wallpaper(&resolve(&dir, file)) {
            Ok(image) => Some(Wallpaper { image, opacity: sheet.wallpaper_opacity }),
            Err(error) => {
                sheet.warnings.push(format!("wallpaper {file}: {error}"));
                None
            }
        },
        None => None,
    };
    if let Ok(mut current) = WALLPAPER.lock() {
        *current = wallpaper;
    }
    let palette: &'static Palette = Box::leak(Box::new(sheet.palette));
    Ok((activate(palette, format!("Ferrochat custom {name}")), sheet.warnings))
}

/// A parsed custom theme.
struct Sheet {
    palette: Palette,
    wallpaper: Option<String>,
    wallpaper_opacity: f32,
    warnings: Vec<String>,
}

/// Reads custom properties. `--base` picks the built-in theme the rest starts
/// from; `--accent` overrides the accent setting.
fn parse(text: &str, accent: Accent) -> Sheet {
    let mut warnings = Vec::new();
    let mut properties: Vec<(String, String)> = Vec::new();
    for piece in strip_comments(text).split([';', '{', '}']) {
        let piece = piece.trim();
        let Some(rest) = piece.strip_prefix("--") else { continue };
        match rest.split_once(':') {
            Some((name, value)) => properties.push((name.trim().to_lowercase(), value.trim().to_owned())),
            None => warnings.push(format!("--{rest}: missing value")),
        }
    }
    let value = |name: &str| properties.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
    let base = match value("base") {
        None => ThemeName::Dark,
        Some(base) => ThemeName::ALL.into_iter().find(|t| t.key() == base.to_lowercase()).unwrap_or_else(|| {
            warnings.push(format!("--base: unknown theme \"{base}\" (using dark)"));
            ThemeName::Dark
        }),
    };
    let accent = match value("accent").map(|v| (v, color(v))) {
        Some((_, Some(color))) => color,
        _ => accent.color(),
    };
    let mut palette = build_with(base, accent);
    let mut set = Vec::new();
    let (mut wallpaper, mut wallpaper_opacity) = (None, 0.25);
    for (name, raw) in &properties {
        match name.as_str() {
            "base" => {}
            "wallpaper" => match url(raw) {
                Some(file) if file.contains("://") => warnings.push("--wallpaper: only files on this PC".to_owned()),
                Some(file) => wallpaper = Some(file),
                None if raw == "none" => wallpaper = None,
                None => warnings.push(format!("--wallpaper: expected url(\"file\"), got {raw}")),
            },
            "wallpaper-opacity" => match raw.trim_end_matches('%').parse::<f32>() {
                Ok(n) if raw.ends_with('%') => wallpaper_opacity = (n / 100.0).clamp(0.0, 1.0),
                Ok(n) => wallpaper_opacity = n.clamp(0.0, 1.0),
                Err(_) => warnings.push(format!("--wallpaper-opacity: expected 0–1 or a percentage, got {raw}")),
            },
            _ => match (palette.field(name), color(raw)) {
                (Some(field), Some(color)) => {
                    *field = color;
                    set.push(name.clone());
                }
                (Some(_), None) => warnings.push(format!("--{name}: not a colour: {raw}")),
                (None, _) => warnings.push(format!("--{name}: unknown property")),
            },
        }
    }
    palette.derive(base.is_light(), &set);
    Sheet { palette, wallpaper, wallpaper_opacity, warnings }
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = rest[start + 2..].split_once("*/").map_or("", |(_, after)| after);
    }
    out.push_str(rest);
    out
}

/// `url("a.png")`, `url('a.png')` or `url(a.png)`.
fn url(value: &str) -> Option<String> {
    let inner = value.strip_prefix("url(")?.strip_suffix(')')?.trim();
    let inner = inner.trim_matches(|c| c == '"' || c == '\'');
    (!inner.is_empty()).then(|| inner.to_owned())
}

/// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb(…)`, `rgba(…)`, and a few names.
fn color(value: &str) -> Option<Color> {
    let value = value.trim().to_lowercase();
    match value.as_str() {
        "white" => return Some(Color::WHITE),
        "black" => return Some(Color::BLACK),
        "transparent" => return Some(Color::TRANSPARENT),
        _ => {}
    }
    if let Some(hex) = value.strip_prefix('#') {
        let digit = |i: usize| u8::from_str_radix(hex.get(i..i + 1)?, 16).ok();
        let pair = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
        let [r, g, b, a] = match hex.len() {
            3 | 4 => [digit(0)? * 17, digit(1)? * 17, digit(2)? * 17, if hex.len() == 4 { digit(3)? * 17 } else { 255 }],
            6 | 8 => [pair(0)?, pair(2)?, pair(4)?, if hex.len() == 8 { pair(6)? } else { 255 }],
            _ => return None,
        };
        return Some(Color::from_rgba8(r, g, b, f32::from(a) / 255.0));
    }
    let inner = value.strip_prefix("rgba(").or_else(|| value.strip_prefix("rgb("))?.strip_suffix(')')?;
    let parts: Vec<&str> = inner.split([',', ' ', '/']).map(str::trim).filter(|s| !s.is_empty()).collect();
    let channel = |s: &str| -> Option<u8> { s.parse::<f32>().ok().filter(|n| (0.0..=255.0).contains(n)).map(|n| n as u8) };
    let alpha = |s: &str| -> Option<f32> {
        match s.strip_suffix('%') {
            Some(percent) => percent.parse::<f32>().ok().map(|n| n / 100.0),
            None => s.parse::<f32>().ok(),
        }
        .filter(|a| (0.0..=1.0).contains(a))
    };
    match parts.as_slice() {
        [r, g, b] => Some(Color::from_rgb8(channel(r)?, channel(g)?, channel(b)?)),
        [r, g, b, a] => Some(Color::from_rgba8(channel(r)?, channel(g)?, channel(b)?, alpha(a)?)),
        _ => None,
    }
}

fn resolve(dir: &Path, file: &str) -> PathBuf {
    let path = Path::new(file);
    if path.is_absolute() {
        path.to_owned()
    } else {
        dir.join(path)
    }
}

/// Largest wallpaper side kept in memory; bigger pictures are scaled down
/// (1920 × 1080 is about 8 MB as RGBA).
const MAX_WALLPAPER_SIDE: u32 = 1920;
const MAX_WALLPAPER_FILE: u64 = 32 * 1024 * 1024;
/// Pictures are decoded at full size before shrinking, on the UI thread: a
/// 48-megapixel photo would briefly take ~190 MB. This allows 5K (~60 MB).
const MAX_WALLPAPER_PIXELS: u64 = 16_000_000;

fn load_wallpaper(path: &Path) -> Result<Handle, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_WALLPAPER_FILE {
        return Err("file is over 32 MB".to_owned());
    }
    // Reads only the header.
    let (width, height) = image::image_dimensions(path).map_err(|e| e.to_string())?;
    if u64::from(width) * u64::from(height) > MAX_WALLPAPER_PIXELS {
        return Err(format!("picture is {width}×{height}; use one under 16 megapixels (e.g. 3840×2160)"));
    }
    let mut picture = image::open(path).map_err(|e| e.to_string())?;
    if picture.width().max(picture.height()) > MAX_WALLPAPER_SIDE {
        picture = picture.resize(MAX_WALLPAPER_SIDE, MAX_WALLPAPER_SIDE, image::imageops::FilterType::Triangle);
    }
    let rgba = picture.to_rgba8();
    Ok(Handle::from_rgba(rgba.width(), rgba.height(), rgba.into_raw()))
}

const EXAMPLE: &str = r#"/* A Ferrochat theme. Themes are local: only you see them.

   Pick it in Settings > Appearance > Theme. After editing, press Reload there.
   Colours: #rgb, #rrggbb, #rrggbbaa, rgb(r, g, b), rgba(r, g, b, a), white, black.
   Anything you leave out comes from --base.

   Properties:
     --base           dark, midnight, light, slate, mocha, forest, plum, paper, high-contrast
     --accent         buttons, names, highlights (overrides the accent setting)
     --rail --sidebar --chat --selected --embed-bg --code-bg    backgrounds
     --text --muted --link                                      text
     --mention --error --online --idle --dnd                    status colours
     --pill-bg --pill-fg --mentioned-row --highlight-row        (derived from accent if unset)
     --spoiler --spoiler-open --backdrop
     --wallpaper          url("picture.jpg"), a file in this folder, behind the chat
     --wallpaper-opacity  0 to 1 (default 0.25)
*/
:root {
  --base: midnight;
  --accent: #e05a8a;
  --chat: #14121a;
  --sidebar: #0e0c13;
  --rail: #08070b;
  --selected: #2a2433;
  --text: #eee8f5;
  --muted: #9a90aa;
  --link: #8ab4ff;
  /* --wallpaper: url("wallpaper.jpg"); */
  --wallpaper-opacity: 0.2;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_combination_has_its_own_palette() {
        for theme in ThemeName::ALL {
            for accent in Accent::ALL {
                let palette = &palettes()[index(theme, accent)];
                assert_eq!(palette.accent, accent.color());
                assert_eq!(palette.text == Color::WHITE, matches!(theme, ThemeName::HighContrast));
            }
        }
    }

    #[test]
    fn colours_parse() {
        assert_eq!(color("#fff"), Some(Color::WHITE));
        assert_eq!(color("#FF0000"), Some(Color::from_rgb8(255, 0, 0)));
        assert_eq!(color("#00ff0080").map(|c| (c.g, (c.a * 100.0).round())), Some((1.0, 50.0)));
        assert_eq!(color("rgb(0, 0, 255)"), Some(Color::from_rgb8(0, 0, 255)));
        assert_eq!(color("rgba(0 0 0 / 50%)").map(|c| c.a), Some(0.5));
        assert_eq!(color("#12"), None);
        assert_eq!(color("rgb(300, 0, 0)"), None);
        assert_eq!(color("blue"), None);
    }

    #[test]
    fn sheets_override_a_base_and_report_problems() {
        let css = r#"
            /* comment with --chat: #000; inside */
            :root {
              --base: light;
              --chat: #102030;
              --accent: rgb(255, 0, 0);
              --bogus: #fff;
              --text: nope;
              --wallpaper: url("bg.png");
              --wallpaper-opacity: 40%;
            }"#;
        let sheet = parse(css, Accent::Ocean);
        let light = build_with(ThemeName::Light, Color::from_rgb8(255, 0, 0));
        assert_eq!(sheet.palette.chat, Color::from_rgb8(0x10, 0x20, 0x30));
        assert_eq!(sheet.palette.accent, Color::from_rgb8(255, 0, 0));
        assert_eq!(sheet.palette.sidebar, light.sidebar, "unset colours come from --base");
        assert_eq!(sheet.palette.text, light.text, "a bad value keeps the base colour");
        assert_eq!(sheet.palette.pill_bg, mix(sheet.palette.chat, sheet.palette.accent, 0.25), "derived from the new chat");
        assert_eq!(sheet.wallpaper.as_deref(), Some("bg.png"));
        assert!((sheet.wallpaper_opacity - 0.4).abs() < 1e-6);
        assert_eq!(sheet.warnings.len(), 2, "{:?}", sheet.warnings);
        assert!(parse("--wallpaper: url(https://x.example/a.png);", Accent::Ember).wallpaper.is_none());
    }

    #[test]
    fn example_theme_is_clean() {
        let sheet = parse(EXAMPLE, Accent::Ember);
        assert!(sheet.warnings.is_empty(), "{:?}", sheet.warnings);
        assert_eq!(sheet.palette.accent, Color::from_rgb8(0xe0, 0x5a, 0x8a));
    }
}
