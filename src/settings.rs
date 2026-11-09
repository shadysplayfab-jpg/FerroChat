//! User preferences, stored as JSON in the user's config directory
//! (`%APPDATA%\ferrochat\settings.json` on Windows).

use std::ops::RangeInclusive;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::theme::{Accent, ThemeName};

pub const TEXT_SIZES: RangeInclusive<u8> = 12..=20;
pub const IMAGE_CACHE_MB: RangeInclusive<u16> = 16..=512;
pub const VOICE_VOLUME: RangeInclusive<u8> = 0..=200;
pub const VOICE_SENSITIVITY_DB: RangeInclusive<i16> = -80..=-10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Keep the token in Windows Credential Manager between runs.
    pub remember_login: bool,
    /// Toasts for DMs and mentions.
    pub notifications: bool,
    /// Also notify while the window is focused (for channels you're not viewing).
    pub notify_when_focused: bool,
    /// Show others that you're typing.
    pub send_typing: bool,
    /// Mark channels read on Discord so other clients stay in sync.
    pub sync_read_state: bool,
    pub clock_24h: bool,
    pub text_size: u8,
    pub theme: ThemeName,
    pub accent: Accent,
    /// A theme file in the themes folder (name without `.css`); wins over `theme`.
    pub custom_theme: Option<String>,
    /// One line per message, no avatars.
    pub compact: bool,
    /// Closing the window hides it to the notification area.
    pub tray: bool,
    /// Launch at sign-in (hidden to the tray when `tray` is on).
    pub start_with_windows: bool,
    /// GPU (wgpu) instead of the software renderer; applies after restart.
    pub gpu_renderer: bool,
    /// Attachment and link-embed previews.
    pub load_images: bool,
    pub load_avatars: bool,
    pub load_custom_emoji: bool,
    /// Play animated GIFs, emoji and avatars (redraws the window while they show).
    pub animate_media: bool,
    /// The GIF picker; its previews (and animated GIF embeds) load from Tenor.
    pub gif_picker: bool,
    /// Member sidebar with online status (user logins).
    pub member_list: bool,
    /// Memory budget for downloaded images, decoded size included.
    pub image_cache_mb: u16,
    /// Show voice channels and allow joining them.
    pub voice_enabled: bool,
    /// Receive and decode cameras and Go Live streams in calls.
    pub video: bool,
    /// Device names; `None` = system default. Apply from the next call.
    pub voice_input_device: Option<String>,
    pub voice_output_device: Option<String>,
    /// Percent.
    pub voice_input_volume: u8,
    pub voice_output_volume: u8,
    /// Voice activity threshold in dBFS; quieter input isn't sent.
    pub voice_sensitivity_db: i16,
    /// Send only while `ptt_key` is held, instead of on voice activity.
    pub push_to_talk: bool,
    pub ptt_key: PttKey,
    /// RNNoise on the microphone (a little CPU during calls).
    pub noise_suppression: bool,
    /// Screen share picture size and frames per second; changing them while
    /// sharing restarts the stream with the new values.
    pub share_resolution: ShareResolution,
    pub share_fps: u8,
    /// Logins saved in Windows Credential Manager (names only here; tokens stay there).
    pub accounts: Vec<SavedAccount>,
    /// Logged in automatically at start.
    pub last_account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAccount {
    pub id: String,
    pub name: String,
}

/// Keys (and mouse side buttons) that can be held for push-to-talk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PttKey {
    #[default]
    Mouse4,
    Mouse5,
    CapsLock,
    Backtick,
    LeftAlt,
    RightAlt,
    RightCtrl,
    F8,
    F9,
    Insert,
}

impl PttKey {
    pub const ALL: [PttKey; 10] = [
        PttKey::Mouse4,
        PttKey::Mouse5,
        PttKey::CapsLock,
        PttKey::Backtick,
        PttKey::LeftAlt,
        PttKey::RightAlt,
        PttKey::RightCtrl,
        PttKey::F8,
        PttKey::F9,
        PttKey::Insert,
    ];

    /// The Windows virtual-key code.
    pub fn code(self) -> u32 {
        match self {
            PttKey::Mouse4 => 0x05,
            PttKey::Mouse5 => 0x06,
            PttKey::CapsLock => 0x14,
            PttKey::Backtick => 0xC0,
            PttKey::LeftAlt => 0xA4,
            PttKey::RightAlt => 0xA5,
            PttKey::RightCtrl => 0xA3,
            PttKey::F8 => 0x77,
            PttKey::F9 => 0x78,
            PttKey::Insert => 0x2D,
        }
    }
}

impl std::fmt::Display for PttKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PttKey::Mouse4 => "Mouse 4 (back)",
            PttKey::Mouse5 => "Mouse 5 (forward)",
            PttKey::CapsLock => "Caps Lock",
            PttKey::Backtick => "` (backtick)",
            PttKey::LeftAlt => "Left Alt",
            PttKey::RightAlt => "Right Alt",
            PttKey::RightCtrl => "Right Ctrl",
            PttKey::F8 => "F8",
            PttKey::F9 => "F9",
            PttKey::Insert => "Insert",
        })
    }
}

pub const SHARE_FPS: [u8; 4] = [5, 15, 30, 60];

/// The largest picture a screen share sends; the screen is scaled down to fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ShareResolution {
    P480,
    #[default]
    P720,
    P1080,
    /// The screen's own size.
    Source,
}

impl ShareResolution {
    pub const ALL: [ShareResolution; 4] = [ShareResolution::P480, ShareResolution::P720, ShareResolution::P1080, ShareResolution::Source];

    /// Bounding box in pixels.
    pub fn max_size(self) -> (u32, u32) {
        match self {
            ShareResolution::P480 => (854, 480),
            ShareResolution::P720 => (1280, 720),
            ShareResolution::P1080 => (1920, 1080),
            ShareResolution::Source => (7680, 4320),
        }
    }
}

impl std::fmt::Display for ShareResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ShareResolution::P480 => "480p",
            ShareResolution::P720 => "720p",
            ShareResolution::P1080 => "1080p",
            ShareResolution::Source => "Source",
        })
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            remember_login: false,
            notifications: true,
            notify_when_focused: false,
            send_typing: true,
            sync_read_state: true,
            clock_24h: true,
            text_size: 15,
            theme: ThemeName::Dark,
            accent: Accent::Ember,
            custom_theme: None,
            compact: false,
            tray: false,
            start_with_windows: false,
            gpu_renderer: false,
            load_images: true,
            load_avatars: true,
            load_custom_emoji: true,
            animate_media: false,
            gif_picker: true,
            member_list: true,
            image_cache_mb: 64,
            voice_enabled: true,
            video: true,
            voice_input_device: None,
            voice_output_device: None,
            voice_input_volume: 100,
            voice_output_volume: 100,
            voice_sensitivity_db: -50,
            push_to_talk: false,
            ptt_key: PttKey::Mouse4,
            noise_suppression: false,
            share_resolution: ShareResolution::P720,
            share_fps: 15,
            accounts: Vec::new(),
            last_account: None,
        }
    }
}

impl Settings {
    pub fn text_size(&self) -> f32 {
        f32::from(self.text_size.clamp(*TEXT_SIZES.start(), *TEXT_SIZES.end()))
    }

    /// (input gain, output volume, gate threshold dB) for the voice engine.
    pub fn voice_levels(&self) -> (f32, f32, f32) {
        let percent = |p: u8| f32::from(p.min(*VOICE_VOLUME.end())) / 100.0;
        let db = self.voice_sensitivity_db.clamp(*VOICE_SENSITIVITY_DB.start(), *VOICE_SENSITIVITY_DB.end());
        (percent(self.voice_input_volume), percent(self.voice_output_volume), f32::from(db))
    }

    /// (largest width, largest height, frames per second) for screen sharing.
    pub fn share_quality(&self) -> (u32, u32, u32) {
        let (width, height) = self.share_resolution.max_size();
        let fps = if SHARE_FPS.contains(&self.share_fps) { self.share_fps } else { 15 };
        (width, height, u32::from(fps))
    }

    pub fn image_cache_bytes(&self) -> usize {
        usize::from(self.image_cache_mb.clamp(*IMAGE_CACHE_MB.start(), *IMAGE_CACHE_MB.end())) * 1024 * 1024
    }

    /// Turns every heavy feature off.
    pub fn lightweight(&mut self) {
        self.load_images = false;
        self.load_avatars = false;
        self.load_custom_emoji = false;
        self.animate_media = false;
        self.gif_picker = false;
        self.member_list = false;
        self.gpu_renderer = false;
        self.voice_enabled = false;
        self.video = false;
        self.noise_suppression = false;
    }
}

#[derive(Debug, Clone)]
pub enum Change {
    RememberLogin(bool),
    Notifications(bool),
    NotifyWhenFocused(bool),
    SendTyping(bool),
    SyncReadState(bool),
    Clock24h(bool),
    TextSize(u8),
    Theme(ThemeName),
    CustomTheme(String),
    Accent(Accent),
    Compact(bool),
    Tray(bool),
    StartWithWindows(bool),
    GpuRenderer(bool),
    LoadImages(bool),
    LoadAvatars(bool),
    LoadCustomEmoji(bool),
    AnimateMedia(bool),
    GifPicker(bool),
    MemberList(bool),
    ImageCacheMb(u16),
    Voice(bool),
    Video(bool),
    VoiceInputDevice(Option<String>),
    VoiceOutputDevice(Option<String>),
    VoiceInputVolume(u8),
    VoiceOutputVolume(u8),
    VoiceSensitivity(i16),
    PushToTalk(bool),
    PttKey(PttKey),
    NoiseSuppression(bool),
    ShareResolution(ShareResolution),
    ShareFps(u8),
    Lightweight,
}

impl Change {
    pub fn apply(&self, settings: &mut Settings) {
        match self {
            Change::VoiceInputDevice(name) => return settings.voice_input_device = name.clone(),
            Change::VoiceOutputDevice(name) => return settings.voice_output_device = name.clone(),
            Change::CustomTheme(name) => return settings.custom_theme = Some(name.clone()),
            _ => {}
        }
        match *self {
            Change::VoiceInputDevice(_) | Change::VoiceOutputDevice(_) | Change::CustomTheme(_) => {}
            Change::VoiceInputVolume(percent) => settings.voice_input_volume = percent,
            Change::VoiceOutputVolume(percent) => settings.voice_output_volume = percent,
            Change::VoiceSensitivity(db) => settings.voice_sensitivity_db = db,
            Change::PushToTalk(on) => settings.push_to_talk = on,
            Change::PttKey(key) => settings.ptt_key = key,
            Change::NoiseSuppression(on) => settings.noise_suppression = on,
            Change::ShareResolution(resolution) => settings.share_resolution = resolution,
            Change::ShareFps(fps) => settings.share_fps = fps,
            Change::RememberLogin(on) => settings.remember_login = on,
            Change::Notifications(on) => settings.notifications = on,
            Change::NotifyWhenFocused(on) => settings.notify_when_focused = on,
            Change::SendTyping(on) => settings.send_typing = on,
            Change::SyncReadState(on) => settings.sync_read_state = on,
            Change::Clock24h(on) => settings.clock_24h = on,
            Change::TextSize(size) => settings.text_size = size,
            Change::Theme(theme) => {
                settings.theme = theme;
                settings.custom_theme = None;
            }
            Change::Accent(accent) => settings.accent = accent,
            Change::Compact(on) => settings.compact = on,
            Change::Tray(on) => settings.tray = on,
            Change::StartWithWindows(on) => settings.start_with_windows = on,
            Change::GpuRenderer(on) => settings.gpu_renderer = on,
            Change::LoadImages(on) => settings.load_images = on,
            Change::LoadAvatars(on) => settings.load_avatars = on,
            Change::LoadCustomEmoji(on) => settings.load_custom_emoji = on,
            Change::AnimateMedia(on) => settings.animate_media = on,
            Change::GifPicker(on) => settings.gif_picker = on,
            Change::MemberList(on) => settings.member_list = on,
            Change::ImageCacheMb(mb) => settings.image_cache_mb = mb,
            Change::Voice(on) => settings.voice_enabled = on,
            Change::Video(on) => settings.video = on,
            Change::Lightweight => settings.lightweight(),
        }
    }
}

/// `%APPDATA%\ferrochat`, or `FERROCHAT_CONFIG_DIR` when set (a portable copy,
/// or a test run that mustn't touch your real settings).
pub fn config_dir() -> Option<PathBuf> {
    match std::env::var_os("FERROCHAT_CONFIG_DIR") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(dirs::config_dir()?.join("ferrochat")),
    }
}

fn path() -> Option<PathBuf> {
    Some(config_dir()?.join("settings.json"))
}

/// Defaults when the file doesn't exist. An unparsable file is moved aside to
/// `settings.invalid.json` rather than overwritten; the message says so.
pub fn load() -> (Settings, Option<String>) {
    let Some(path) = path() else { return (Settings::default(), None) };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (Settings::default(), None),
        Err(error) => return (Settings::default(), Some(format!("Could not read settings: {error}"))),
    };
    match serde_json::from_str(&text) {
        Ok(settings) => (settings, None),
        Err(error) => {
            let aside = path.with_file_name("settings.invalid.json");
            let warning = match std::fs::rename(&path, &aside) {
                Ok(()) => format!("Settings were invalid ({error}); moved to {}", aside.display()),
                Err(move_error) => format!("Settings were invalid ({error}) and could not be moved aside: {move_error}"),
            };
            (Settings::default(), Some(warning))
        }
    }
}

/// Writes via a temporary file so a crash can't leave a half-written file.
pub fn save(settings: &Settings) -> Result<(), String> {
    let path = path().ok_or("no config directory")?;
    let dir = path.parent().ok_or("invalid settings path")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, json).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, &path).map_err(|e| e.to_string())
}
