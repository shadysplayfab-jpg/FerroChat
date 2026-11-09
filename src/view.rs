use chrono::{DateTime, FixedOffset, Local, NaiveDate, Utc};
use iced::alignment::{Horizontal, Vertical};
use iced::font::{Style as FontStyle, Weight};
use iced::widget::text::{Shaping, Span, Text};
use iced::widget::{
    button, checkbox, column, container, image, mouse_area, opaque, pick_list, progress_bar, rich_text, row, scrollable, slider,
    span, stack, text, text_input, tooltip, Column, Row, Scrollable, Space,
};
use iced::{Alignment, Background, Border, Color, Element, Font, Length, Theme};

use crate::discord::member_list::Item;
use crate::commands;
use crate::discord::model::{
    snowflake, snowflake_time, Channel, Component, Embed, Guild, GuildFolder, Message, NotifyLevel, Poll,
    ReactionEmoji, StickerItem, UnfurledMedia, User, REPLY,
};
use crate::images;
use crate::markup::{self, Block, Body, Inline, Kind, LineKind, Style};
use crate::notify;
use crate::session::{
    human_size, reaction_emoji, DialogKind, EmbedBodies, Entry, Forum, MenuTarget, Panel, PollEdit, Session, SocialMsg, GIF_INPUT,
    MESSAGES_ID, POLL_HOURS, SEARCH_INPUT, STATUSES,
    SUGGESTION_LIMIT, SWITCHER_INPUT, UPLOAD_LIMIT,
};
use crate::settings::{Change, PttKey, Settings, IMAGE_CACHE_MB, TEXT_SIZES, VOICE_SENSITIVITY_DB, VOICE_VOLUME};
use crate::theme::{self, p, rgb, Accent, ThemeName};
use crate::Msg;

mod social;
mod stage;

const BOLD: Font = Font { weight: Weight::Bold, ..Font::DEFAULT };

/// Consecutive messages from one author within this gap share a header.
const GROUP_MINUTES: i64 = 7;
const REPLY_PREVIEW_CHARS: usize = 100;
const EMOJI_SIZE: f32 = 22.0;
const JUMBO_EMOJI_SIZE: f32 = 48.0;
const JUMBO_TEXT_SIZE: f32 = 40.0;
const AVATAR_SIZE: f32 = 36.0;
const MEMBER_AVATAR_SIZE: f32 = 24.0;
const REACTION_EMOJI_SIZE: f32 = 16.0;
/// Active threads listed under a channel.
const THREADS_SHOWN: usize = 5;
const STICKER_SIZE: f32 = 160.0;
const STICKER_PICKER_SIZE: f32 = 64.0;
const QUICK_REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "😮", "😢", "🎉"];
/// Characters shown before "…" where a name must stay on one line (iced 0.13.0
/// wraps text regardless of its wrapping setting): your name, your status,
/// channel and DM names, member list names.
const NAME_CHARS: usize = 13;
const STATUS_CHARS: usize = 22;
const CHANNEL_CHARS: usize = 26;
const MEMBER_CHARS: usize = 18;

pub fn login<'a>(token: &'a str, error: Option<&'a str>, settings: &'a Settings) -> Element<'a, Msg> {
    let remember = settings.remember_login;
    let note = if remember {
        "Your token is saved encrypted in Windows Credential Manager. Log out to remove it."
    } else {
        "Your token is kept in memory only and never written to disk."
    };
    let mut form = column![
        text("Ferrochat").size(32).font(BOLD).color(p().accent),
        text("A lightweight client. Paste your token to log in.").size(14).color(p().muted),
        text_input("Token", token)
            .on_input(Msg::TokenChanged)
            .on_submit(Msg::Login)
            .secure(true)
            .padding(12),
        checkbox("Remember me", remember).on_toggle(|on| Msg::Setting(Change::RememberLogin(on))),
        text(note).size(12).color(p().muted),
        button(container(text("Log in").font(BOLD)).center_x(Length::Fill)).width(Length::Fill).on_press(Msg::Login).padding(10),
    ]
    .spacing(14)
    .width(380);
    if let Some(error) = error {
        form = form.push(text(error).size(13).color(p().error));
    }
    if !settings.accounts.is_empty() {
        let mut saved = column![section_label("Saved accounts")].spacing(6);
        for account in &settings.accounts {
            saved = saved.push(
                button(label(account.name.as_str()).size(14).font(BOLD))
                    .width(Length::Fill)
                    .padding([8, 12])
                    .style(quiet)
                    .on_press(Msg::SwitchAccount(account.id.clone())),
            );
        }
        form = form.push(rule()).push(saved);
    }
    let card = container(form).padding(32).style(fill(p().sidebar, 14.0));
    container(card).center(Length::Fill).style(fill(p().rail, 0.0)).into()
}

/// "System default" first, then the devices; picking it stores `None`.
fn device_picker<'a>(
    current: &Option<String>,
    devices: &[String],
    change: fn(Option<String>) -> Change,
) -> Element<'a, Msg> {
    const DEFAULT: &str = "System default";
    let options: Vec<String> = std::iter::once(DEFAULT.to_owned()).chain(devices.iter().cloned()).collect();
    let selected = Some(current.clone().unwrap_or_else(|| DEFAULT.to_owned()));
    pick_list(options, selected, move |name: String| Msg::Setting(change((name != DEFAULT).then_some(name))))
        .width(260)
        .into()
}

/// Custom themes and the current one's problems, for Settings.
pub struct ThemesInfo<'a> {
    pub custom: &'a [String],
    pub problems: &'a [String],
}

/// A theme in the picker: built-in or a file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ThemeChoice {
    BuiltIn(ThemeName),
    Custom(String),
}

impl std::fmt::Display for ThemeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThemeChoice::BuiltIn(theme) => theme.fmt(f),
            ThemeChoice::Custom(name) => write!(f, "{name} (custom)"),
        }
    }
}

fn theme_picker<'a>(settings: &Settings, custom: &[String]) -> Element<'a, Msg> {
    let choices: Vec<ThemeChoice> = ThemeName::ALL
        .into_iter()
        .map(ThemeChoice::BuiltIn)
        .chain(custom.iter().cloned().map(ThemeChoice::Custom))
        .collect();
    let current = match &settings.custom_theme {
        Some(name) => ThemeChoice::Custom(name.clone()),
        None => ThemeChoice::BuiltIn(settings.theme),
    };
    pick_list(choices, Some(current), |choice| match choice {
        ThemeChoice::BuiltIn(theme) => Msg::Setting(Change::Theme(theme)),
        ThemeChoice::Custom(name) => Msg::Setting(Change::CustomTheme(name)),
    })
    .width(220)
    .into()
}

/// Problems in the custom theme file (the rest of it still applies).
fn theme_problems<'a>(problems: &'a [String]) -> Element<'a, Msg> {
    if problems.is_empty() {
        return Space::new(0, 0).into();
    }
    let mut list = Column::new().spacing(2);
    for problem in problems.iter().take(8) {
        list = list.push(text(problem.as_str()).size(12).color(p().error));
    }
    if problems.len() > 8 {
        list = list.push(text(format!("…and {} more", problems.len() - 8)).size(12).color(p().error));
    }
    list.into()
}

/// One coloured dot per accent; the current one is ringed.
fn accent_swatches<'a>(current: Accent) -> Element<'a, Msg> {
    let mut swatches = Row::new().spacing(8);
    for accent in Accent::ALL {
        let color = accent.color();
        let chosen = accent == current;
        let swatch = button(Space::new(20, 20))
            .padding(0)
            .style(move |_, _| button::Style {
                background: Some(Background::Color(color)),
                border: Border { color: if chosen { p().text } else { color }, width: 2.0, radius: 12.0.into() },
                ..button::Style::default()
            })
            .on_press(Msg::Setting(Change::Accent(accent)));
        let tip = container(text(accent.to_string()).size(12)).padding([3, 8]).style(fill(p().rail, 6.0));
        swatches = swatches.push(tooltip(swatch, tip, tooltip::Position::Top));
    }
    swatches.into()
}

/// `cache`: (cached images, bytes) when logged in.
pub fn settings_panel<'a>(
    settings: &'a Settings,
    cache: Option<(usize, usize)>,
    devices: &'a (Vec<String>, Vec<String>),
    current_account: Option<&'a str>,
    themes: ThemesInfo<'a>,
) -> Element<'a, Msg> {
    let toggle = |label: &'static str, value: bool, change: fn(bool) -> Change| {
        checkbox(label, value).on_toggle(move |on| Msg::Setting(change(on)))
    };
    // Card title: the card's own name, in the body colour. Keeping the accent
    // for actual accents (buttons, pills) so it doesn't drown the palette.
    let section = |title: &'static str| text(title).size(15).font(BOLD).color(p().text);
    let text_size = row![
        text("Message text size").width(Length::Fill),
        slider(TEXT_SIZES, settings.text_size, |size| Msg::Setting(Change::TextSize(size))).width(160),
        text(format!("{} px", settings.text_size)).width(48),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    let image_cache = row![
        text("Image memory limit").width(Length::Fill),
        slider(IMAGE_CACHE_MB, settings.image_cache_mb, |mb| Msg::Setting(Change::ImageCacheMb(mb))).step(16u16).width(160),
        text(format!("{} MB", settings.image_cache_mb)).width(64),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    let labelled = |name: &'static str, control: Element<'a, Msg>, value: String| {
        row![text(name).width(Length::Fill), control, text(value).width(64)].spacing(12).align_y(Alignment::Center)
    };
    let percent_slider = |value: u8, change: fn(u8) -> Change| -> Element<'a, Msg> {
        slider(VOICE_VOLUME, value, move |v| Msg::Setting(change(v))).width(160).into()
    };
    let voice = column![
        labelled("Microphone", device_picker(&settings.voice_input_device, &devices.0, Change::VoiceInputDevice), String::new()),
        labelled("Speakers", device_picker(&settings.voice_output_device, &devices.1, Change::VoiceOutputDevice), String::new()),
        labelled(
            "Microphone volume",
            percent_slider(settings.voice_input_volume, Change::VoiceInputVolume),
            format!("{}%", settings.voice_input_volume)
        ),
        labelled(
            "Speaker volume",
            percent_slider(settings.voice_output_volume, Change::VoiceOutputVolume),
            format!("{}%", settings.voice_output_volume)
        ),
        labelled(
            "Voice activity threshold",
            slider(VOICE_SENSITIVITY_DB, settings.voice_sensitivity_db, |db| Msg::Setting(Change::VoiceSensitivity(db)))
                .width(160)
                .into(),
            format!("{} dB", settings.voice_sensitivity_db)
        ),
        text("Lower thresholds pick up quieter speech (and more background noise). Device changes apply to the next call.")
            .size(12)
            .color(p().muted),
        row![
            checkbox("Push to talk", settings.push_to_talk)
                .on_toggle(|on| Msg::Setting(Change::PushToTalk(on)))
                .width(Length::Fill),
            pick_list(PttKey::ALL, Some(settings.ptt_key), |key| Msg::Setting(Change::PttKey(key))).width(180),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
        text("Hold the key (it works while Ferrochat is in the background) to talk; the threshold is ignored.")
            .size(12)
            .color(p().muted),
        checkbox("Noise suppression (RNNoise; a little extra CPU during calls)", settings.noise_suppression)
            .on_toggle(|on| Msg::Setting(Change::NoiseSuppression(on))),
    ]
    .spacing(10);
    let cache_stats = match cache {
        Some((count, bytes)) => format!("Images in memory now: {count} ({})", human_size(bytes as u64)),
        None => "Images in memory now: none (not logged in)".to_owned(),
    };

    let card = |title: &'static str, content: Column<'a, Msg>| {
        container(column![section(title), content.spacing(10)].spacing(12))
            .padding(18)
            .width(Length::Fill)
            .style(fill(p().sidebar, 10.0))
    };
    let mut account = column![toggle(
        "Remember me on this PC (Windows Credential Manager)",
        settings.remember_login,
        Change::RememberLogin
    )];
    for saved in &settings.accounts {
        let current = current_account == Some(saved.id.as_str());
        let name = label(if current { format!("{} (logged in)", saved.name) } else { saved.name.clone() })
            .font(if current { BOLD } else { Font::DEFAULT })
            .width(Length::Fill);
        let mut line = row![name].spacing(8).align_y(Alignment::Center);
        if !current {
            line = line.push(small_button("Switch", false, Msg::SwitchAccount(saved.id.clone())));
        }
        line = line.push(
            button(text("Remove").size(12)).padding([4, 10]).style(button::danger).on_press(Msg::RemoveAccount(saved.id.clone())),
        );
        account = account.push(line);
    }
    if cache.is_some() {
        let add = small_button("Add another account", false, Msg::AddAccount);
        let log_out = button(text("Log out").size(13)).style(button::danger).on_press(Msg::Logout);
        account = account.push(
            row![text("Logging out also removes this account's saved login.").size(12).color(p().muted).width(Length::Fill), add, log_out]
                .spacing(8)
                .align_y(Alignment::Center),
        );
    }

    let panel = column![
        row![
            text("Settings").size(26).font(BOLD).width(Length::Fill),
            button(text("Close")).padding([6, 16]).on_press(Msg::ToggleSettings).style(quiet),
        ]
        .align_y(Alignment::Center),
        card("Account", account),
        card(
            "Notifications",
            column![
                toggle("Desktop notifications for DMs and mentions", settings.notifications, Change::Notifications),
                toggle(
                    "Also notify while Ferrochat is focused (other channels)",
                    settings.notify_when_focused,
                    Change::NotifyWhenFocused,
                ),
            ]
        ),
        card(
            "Privacy",
            column![
                toggle("Show others when I'm typing", settings.send_typing, Change::SendTyping),
                toggle("Sync read state with Discord", settings.sync_read_state, Change::SyncReadState),
            ]
        ),
        card(
            "Appearance",
            column![
                labelled("Theme", theme_picker(settings, themes.custom), String::new()),
                row![
                    text("Your own themes are .css files in the themes folder. They only change what you see.")
                        .size(12)
                        .color(p().muted)
                        .width(Length::Fill),
                    small_button("Open themes folder", false, Msg::OpenThemesFolder),
                    small_button("Reload", false, Msg::ReloadThemes),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                theme_problems(themes.problems),
                row![text("Accent colour").width(Length::Fill), accent_swatches(settings.accent)]
                    .spacing(12)
                    .align_y(Alignment::Center),
                toggle("Compact mode (one line per message, no avatars)", settings.compact, Change::Compact),
                toggle("24-hour clock", settings.clock_24h, Change::Clock24h),
                text_size,
            ]
        ),
        card(
            "Window",
            column![
                toggle("Minimize to the notification area when closed", settings.tray, Change::Tray),
                toggle("Start with Windows", settings.start_with_windows, Change::StartWithWindows),
                text("With both on, Ferrochat starts hidden in the notification area. Right-click its icon to quit.")
                    .size(12)
                    .color(p().muted),
            ]
        ),
        card("Voice", voice),
        card(
            "Performance",
            column![
                text("Turn these off to save memory and data.").size(12).color(p().muted),
                toggle("Load image and link previews", settings.load_images, Change::LoadImages),
                toggle("Load avatars and server icons", settings.load_avatars, Change::LoadAvatars),
                toggle("Load custom emoji and sticker images", settings.load_custom_emoji, Change::LoadCustomEmoji),
                toggle(
                    "Play animated GIFs, emoji and avatars (redraws 20 times a second while one is on screen)",
                    settings.animate_media,
                    Change::AnimateMedia
                ),
                toggle("GIF picker (previews and animated GIF embeds load from Tenor)", settings.gif_picker, Change::GifPicker),
                toggle("Member list with online status", settings.member_list, Change::MemberList),
                toggle("Voice channels", settings.voice_enabled, Change::Voice),
                toggle(
                    "Video in calls: cameras and Go Live streams (decoding uses CPU; applies to the next call)",
                    settings.video,
                    Change::Video
                ),
                toggle("Use the GPU renderer (more memory; applies after restart)", settings.gpu_renderer, Change::GpuRenderer),
                image_cache,
                text(cache_stats).size(12).color(p().muted),
                button(text("Lightweight mode: turn all of these off").size(13))
                    .style(quiet)
                    .on_press(Msg::Setting(Change::Lightweight)),
            ]
        ),
        text(
            "Shortcuts: Ctrl+K jumps to a channel, Ctrl+F searches, Esc cancels a reply or edit, \
             Up arrow edits your last message."
        )
        .size(12)
        .color(p().muted),
    ]
    .spacing(16)
    .max_width(640);
    let centered = container(panel).center_x(Length::Fill).padding([32, 24]);
    container(thin_scroll(centered)).height(Length::Fill).style(fill(p().chat, 0.0)).into()
}

impl Session {
    /// The start-up screen: logo, the real loading steps, and a fun fact.
    fn loading_view(&self, loading: &crate::session::Loading) -> Element<'_, Msg> {
        static LOGO: std::sync::OnceLock<image::Handle> = std::sync::OnceLock::new();
        let logo = LOGO.get_or_init(|| image::Handle::from_bytes(&include_bytes!("../assets/icon.png")[..]));
        let ready = loading.ready_at.is_some();
        let icons = self.icons_progress();
        // ✓ done, ● working on it, ○ not yet.
        let step = |mark: &'static str, label: String, colour: Color| {
            // Advanced shaping falls back to a font that has ✓.
            let mark = text(mark).size(14).color(colour).width(20).shaping(Shaping::Advanced);
            row![mark, text(label).size(14)].spacing(6).align_y(Alignment::Center)
        };
        let (done, working, waiting) = (p().online, p().accent, p().muted);
        let mut steps = column![step("✓", "Settings and saved login".to_owned(), done)].spacing(8);
        steps = steps.push(if ready {
            step("✓", "Connected to Discord".to_owned(), done)
        } else {
            step("●", "Connecting to Discord…".to_owned(), working)
        });
        let mut progress = if ready { 0.7 } else { 0.25 };
        match icons {
            Some((loaded, total)) if ready => {
                progress += 0.3 * if total == 0 { 1.0 } else { loaded as f32 / total as f32 };
                steps = steps.push(step("●", format!("Loading server icons ({loaded}/{total})"), working));
            }
            Some(_) => steps = steps.push(step("○", "Loading server icons".to_owned(), waiting)),
            None => {}
        }
        let offset = std::process::id() as usize;
        let fact = crate::session::FUN_FACTS[(offset + (loading.started.elapsed().as_secs() / 6) as usize) % crate::session::FUN_FACTS.len()];
        let mut card = column![
            image(logo.clone()).width(64).height(64),
            text("Ferrochat").size(28).font(BOLD).color(p().accent),
            progress_bar(0.0..=1.0, progress).height(6).width(320),
            steps,
        ]
        .spacing(16)
        .align_x(Alignment::Center)
        .width(380);
        // Retries and connection problems, while still waiting on Discord.
        if !ready && self.status != "Connecting…" {
            card = card.push(text(self.status.as_str()).size(12).color(p().muted));
        }
        card = card.push(rule()).push(
            column![
                text("Did you know?").size(12).font(BOLD).color(p().accent),
                text(fact).size(13).color(p().muted).width(Length::Fill).align_x(Horizontal::Center),
            ]
            .spacing(4)
            .align_x(Alignment::Center),
        );
        let card = container(card).padding(32).style(fill(p().sidebar, 14.0));
        container(card).center(Length::Fill).style(fill(p().rail, 0.0)).into()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        if let Some(loading) = &self.loading {
            return self.loading_view(loading);
        }
        let mut layout = row![self.guild_rail(), self.channel_list()];
        match self.selected_channel.as_deref() {
            // A voice channel: the call's tiles, with its text chat beside them.
            Some(channel_id) if self.in_voice_view() => {
                layout = layout
                    .push(self.voice_stage_view(channel_id))
                    .push(container(self.chat()).width(stage::stage_chat_width(self.window.width)).height(Length::Fill));
            }
            selected => {
                layout = layout.push(self.chat());
                let in_channel = selected.is_some();
                match &self.panel {
                    Some(panel) if in_channel => layout = layout.push(self.side_panel(panel)),
                    _ if self.show_members && self.selected_guild.is_some() && in_channel => {
                        layout = layout.push(self.member_panel());
                    }
                    _ => {}
                }
            }
        }
        let mut layers = stack![layout];
        if let Some(query) = &self.switcher {
            layers = layers.push(self.switcher_view(query));
        }
        if let Some(dialog) = self.dialog_overlay() {
            layers = layers.push(dialog);
        }
        if let Some(menu) = self.menu_overlay() {
            layers = layers.push(menu);
        }
        crate::pointer::pointer(layers, |at, window| Msg::Social(SocialMsg::Pointer(at, window))).into()
    }

    /// Right-click opens `target`'s menu.
    fn with_menu<'a>(content: impl Into<Element<'a, Msg>>, target: MenuTarget) -> Element<'a, Msg> {
        mouse_area(content).on_right_press(Msg::Social(SocialMsg::OpenMenu(target))).into()
    }

    fn guild_rail(&self) -> Element<'_, Msg> {
        let (unread, mentions) = self.badge(&self.dms);
        let home = RailItem {
            content: RailContent::Initials("DM".to_owned()),
            name: "Direct Messages".to_owned(),
            active: self.selected_guild.is_none(),
            unread,
            mentions,
        };
        let mut rail = Column::new()
            .spacing(8)
            .padding([12, 4])
            .align_x(Alignment::Center)
            .push(rail_item(home, Msg::SelectGuild(None)))
            .push(container(Space::new(32, 2)).style(fill(p().selected, 1.0)));
        for entry in self.rail_entries() {
            rail = rail.push(match entry {
                RailEntry::Guild(guild) => self.guild_rail_item(guild),
                RailEntry::Folder(folder, guilds) => self.folder_view(folder, &guilds),
            });
        }
        if !self.is_bot() {
            let add = RailItem {
                content: RailContent::Initials("+".to_owned()),
                name: "Add a Server".to_owned(),
                active: false,
                unread: false,
                mentions: 0,
            };
            rail = rail.push(rail_item(add, Msg::Social(SocialMsg::OpenDialog(DialogKind::JoinServer))));
        }
        container(thin_scroll(rail))
            .width(72)
            .height(Length::Fill)
            .style(fill(p().rail, 0.0))
            .into()
    }

    /// Servers in your folder order; servers no folder lists (just joined) go first.
    fn rail_entries(&self) -> Vec<RailEntry<'_>> {
        if self.folders.is_empty() {
            return self.guilds.iter().map(RailEntry::Guild).collect();
        }
        let listed = |id: &str| self.folders.iter().any(|f| f.guild_ids.iter().any(|g| g == id));
        let mut entries: Vec<RailEntry<'_>> =
            self.guilds.iter().filter(|g| !listed(&g.id)).map(RailEntry::Guild).collect();
        for folder in &self.folders {
            let guilds: Vec<&Guild> =
                folder.guild_ids.iter().filter_map(|id| self.guilds.iter().find(|g| &g.id == id)).collect();
            match guilds.as_slice() {
                [] => {}
                [guild] if !folder.is_folder() => entries.push(RailEntry::Guild(guild)),
                _ if !folder.is_folder() => entries.extend(guilds.into_iter().map(RailEntry::Guild)),
                _ => entries.push(RailEntry::Folder(folder, guilds)),
            }
        }
        entries
    }

    fn guild_icon(&self, guild: &Guild) -> Option<image::Handle> {
        guild
            .icon
            .as_deref()
            .filter(|_| self.settings.load_avatars)
            .and_then(|hash| images::guild_icon_url(&guild.id, hash))
            .and_then(|url| self.images.get(&url).cloned())
    }

    /// (unread, mentions) for a server; muted servers only count mentions.
    fn guild_badge(&self, guild: &Guild) -> (bool, u32) {
        let (unread, mentions) = self.badge(guild.channels.iter().filter(|c| c.is_text()));
        (unread && !self.guild_muted(&guild.id), mentions)
    }

    fn guild_item(&self, guild: &Guild) -> RailItem {
        let (unread, mentions) = self.guild_badge(guild);
        RailItem {
            content: match self.guild_icon(guild) {
                Some(handle) => RailContent::Image(handle),
                None => RailContent::Initials(initials(&guild.name)),
            },
            name: guild.name.clone(),
            active: self.selected_guild.as_deref() == Some(guild.id.as_str()),
            unread,
            mentions,
        }
    }

    fn guild_rail_item(&self, guild: &Guild) -> Element<'static, Msg> {
        let item = rail_item(self.guild_item(guild), Msg::SelectGuild(Some(guild.id.clone())));
        Self::with_menu(item, MenuTarget::Guild(guild.id.clone()))
    }

    /// Closed: a 2×2 preview of its servers. Open: the servers on a tinted strip.
    fn folder_view(&self, folder: &GuildFolder, guilds: &[&Guild]) -> Element<'static, Msg> {
        let key = folder.key();
        let open = self.open_folders.contains(&key);
        let tint = folder.color.map_or(p().accent, |c| rgb((c >> 16) as u8, (c >> 8) as u8, c as u8));
        let name = folder.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| {
            let names: Vec<&str> = guilds.iter().map(|g| g.name.as_str()).collect();
            notify::truncate(&names.join(", "), 60)
        });
        let (unread, mentions) = guilds
            .iter()
            .map(|g| self.guild_badge(g))
            .fold((false, 0), |(u, m), (gu, gm)| (u || gu, m + gm));
        let active = guilds.iter().any(|g| self.selected_guild.as_deref() == Some(g.id.as_str()));
        if !open {
            let minis: Vec<Element<'static, Msg>> = guilds
                .iter()
                .take(4)
                .map(|guild| match self.guild_icon(guild) {
                    Some(handle) => image(handle).width(18).height(18).into(),
                    None => container(label(initials(&guild.name).chars().take(1).collect::<String>()).size(9).font(BOLD))
                        .center(18)
                        .style(fill(p().sidebar, 9.0))
                        .into(),
                })
                .collect();
            let mut minis = minis.into_iter();
            let mut grid = Column::new().spacing(2);
            for _ in 0..2 {
                let row = Row::new().spacing(2).extend(minis.by_ref().take(2));
                grid = grid.push(row);
            }
            let tinted = Color { a: 0.35, ..tint };
            let item = RailItem { content: RailContent::Custom(grid.into(), tinted), name, active, unread, mentions };
            return rail_item(item, Msg::ToggleFolder(key));
        }
        let head = RailItem {
            content: RailContent::Custom(label("▾").size(18).color(tint).into(), Color { a: 0.35, ..tint }),
            name: format!("{name} (click to close)"),
            active: false,
            unread: false,
            mentions: 0,
        };
        let mut strip = Column::new().spacing(8).align_x(Alignment::Center).push(rail_item(head, Msg::ToggleFolder(key)));
        for guild in guilds {
            strip = strip.push(self.guild_rail_item(guild));
        }
        container(strip).padding([6, 0]).style(fill(Color { a: 0.18, ..tint }, 24.0)).into()
    }

    fn channel_list(&self) -> Element<'_, Msg> {
        let mut list = Column::new().spacing(2).padding(8);
        let title = self.selected_guild().map_or("Direct Messages", |g| g.name.as_str());
        let mut heading = row![label(title).size(15).font(BOLD).width(Length::Fill)].spacing(6).align_y(Alignment::Center);
        let mut notify_row: Element<'_, Msg> = Space::new(0, 0).into();
        if let (Some(guild), false) = (self.selected_guild(), self.is_bot()) {
            let muted = self.guild_muted(&guild.id);
            heading = heading.push(small_button(if muted { "Unmute" } else { "Mute" }, muted, Msg::MuteGuild(guild.id.clone(), !muted)));
            let id = guild.id.clone();
            let picker = pick_list(NotifyLevel::SERVER, Some(self.guild_notify_setting(&guild.id)), move |level| {
                Msg::GuildNotify(id.clone(), level)
            })
            .text_size(12)
            .padding([2, 6]);
            notify_row = row![text("Notify").size(11).color(p().muted).width(Length::Fill), picker]
                .spacing(6)
                .align_y(Alignment::Center)
                .into();
        }
        let top = column![container(column![heading, notify_row].spacing(6)).padding([12, 14]).width(Length::Fill), rule()];
        match self.selected_guild() {
            Some(guild) => {
                let voice = self.settings.voice_enabled;
                let listed = |c: &&Channel| c.is_text() || c.is_forum() || (voice && c.is_voice());
                for channel in guild.channels.iter().filter(listed).filter(|c| c.parent_id.is_none()) {
                    list = list.push(self.channel_entry(guild, channel));
                }
                for category in guild.channels.iter().filter(|c| c.is_category()) {
                    let mut children = guild
                        .channels
                        .iter()
                        .filter(listed)
                        .filter(|c| c.parent_id.as_deref() == Some(category.id.as_str()))
                        .peekable();
                    if children.peek().is_none() {
                        continue;
                    }
                    // Collapsed: only the open channel, unread ones and calls you're in stay.
                    let collapsed = self.collapsed.contains(&category.id);
                    let arrow = if collapsed { "›" } else { "⌄" };
                    let heading = button(label(format!("{arrow} {}", category.label().to_uppercase())).size(11).color(p().muted))
                        .width(Length::Fill)
                        .padding([10, 4])
                        .style(button::text)
                        .on_press(Msg::Social(SocialMsg::ToggleCategory(category.id.clone())));
                    list = list.push(Self::with_menu(heading, MenuTarget::Channel(category.id.clone())));
                    for channel in children {
                        let keep = self.selected_channel.as_deref() == Some(channel.id.as_str())
                            || self.shows_unread(&channel.id)
                            || self.mention_count(&channel.id) > 0
                            || self.voice.as_ref().is_some_and(|call| call.channel_id == channel.id);
                        if !collapsed || keep {
                            list = list.push(self.channel_entry(guild, channel));
                        }
                    }
                }
            }
            None => {
                if !self.is_bot() {
                    list = list.push(self.home_entries());
                }
                for dm in &self.dms {
                    list = list.push(self.channel_button(dm, "@"));
                }
            }
        }

        let menu: Element<'_, Msg> = if self.status_menu { self.status_menu_view() } else { Space::new(0, 0).into() };
        container(column![top, thin_scroll(list).height(Length::Fill), self.voice_panel(), menu, self.user_panel()])
            .width(240)
            .height(Length::Fill)
            .style(fill(p().sidebar, 0.0))
            .into()
    }

    /// You: avatar, name, status (click to change), and Settings.
    fn user_panel(&self) -> Element<'_, Msg> {
        let name = self.me.as_ref().map_or("…", User::display_name);
        let avatar: Element<'_, Msg> = match &self.me {
            Some(me) if self.settings.load_avatars => self.avatar(me, 32.0),
            _ => container(Space::new(32, 32)).style(fill(p().selected, 16.0)).into(),
        };
        let (color, status) = if self.online {
            let line = self.custom_status.clone().unwrap_or_else(|| status_name(&self.presence).to_owned());
            (status_color(&self.presence), notify::truncate(&line, 40))
        } else {
            (p().idle, notify::truncate(&self.status, 40))
        };
        let dot = container(Space::new(8, 8)).style(fill(color, 4.0));
        // Shortened to one line each (iced 0.13.0 ignores no-wrap on text, so
        // long names used to wrap onto two lines).
        let who = column![
            label(notify::truncate(name, NAME_CHARS)).size(13).font(BOLD).color(p().text),
            row![dot, label(notify::truncate(&status, STATUS_CHARS)).size(11).color(p().muted)].spacing(5).align_y(Alignment::Center),
        ]
        .spacing(2);
        let who = button(who).width(Length::Fill).padding([2, 4]).style(subtle).on_press(Msg::ToggleStatusMenu);
        // Settings reads as a utility link, not a primary button.
        let settings = button(text("Settings").size(12))
            .padding([4, 8])
            .style(|theme, status| {
                let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
                let mut style = button::text(theme, status);
                style.text_color = if hovered { p().text } else { p().muted };
                style.background = hovered.then_some(Background::Color(p().selected));
                style.border = Border { radius: 6.0.into(), ..Border::default() };
                style
            })
            .on_press(Msg::ToggleSettings);
        let card = container(
            row![avatar, who, settings]
                .spacing(6)
                .align_y(Alignment::Center),
        )
        .padding([8, 10])
        .style(fill(p().rail, 0.0));
        // A 1 px rule separates the card from the channel list above it.
        column![rule(), card].into()
    }

    /// Status choices and custom status, above the user panel.
    fn status_menu_view(&self) -> Element<'_, Msg> {
        let mut menu = Column::new().spacing(2);
        for status in STATUSES {
            let dot = container(Space::new(10, 10)).style(fill(status_color(status), 5.0));
            let chosen = self.presence == status;
            let name = label(status_name(status)).size(13).font(if chosen { BOLD } else { Font::DEFAULT });
            menu = menu.push(
                button(row![dot, name].spacing(8).align_y(Alignment::Center))
                    .width(Length::Fill)
                    .padding([5, 8])
                    .style(subtle)
                    .on_press(Msg::SetStatus(status.to_owned())),
            );
        }
        let custom = text_input("Custom status (empty clears)", &self.custom_status_input)
            .on_input(Msg::CustomStatusInput)
            .on_submit(Msg::SaveCustomStatus)
            .size(12)
            .padding(6);
        let save = button(text("Save").size(12)).padding([5, 10]).on_press(Msg::SaveCustomStatus);
        menu = menu.push(row![custom, save].spacing(6).align_y(Alignment::Center));
        container(menu).padding(8).width(Length::Fill).style(bordered(p().rail, 8.0)).into()
    }

    fn channel_entry<'a>(&'a self, guild: &'a Guild, channel: &'a Channel) -> Element<'a, Msg> {
        if channel.is_forum() {
            return self.channel_button(channel, "☰");
        }
        if !channel.is_voice() {
            let button = self.channel_button(channel, "#");
            let threads = self.threads_of(guild, &channel.id);
            if threads.is_empty() {
                return button;
            }
            // Active threads under their channel; the newest few keep the list short.
            let mut column = column![button].spacing(1);
            for thread in threads.into_iter().take(THREADS_SHOWN) {
                column = column.push(row![Space::new(14, 0), self.channel_button(thread, "↳")]);
            }
            return column.into();
        }
        let joined = self.voice.as_ref().is_some_and(|c| c.channel_id == channel.id);
        let viewing = self.selected_channel.as_deref() == Some(channel.id.as_str());
        let name = label(format!("🔊 {}", channel.label())).size(14).width(Length::Fill);
        // Clicking joins and opens the stage, like Discord.
        let entry = button(name)
            .width(Length::Fill)
            .padding([4, 8])
            .style(move |theme, status| {
                let mut style = button::text(theme, status);
                let hovered = matches!(status, button::Status::Hovered);
                style.text_color = if joined || viewing || hovered { p().text } else { p().muted };
                if viewing {
                    style.background = Some(Background::Color(p().selected));
                }
                style.border = Border { radius: 4.0.into(), ..Border::default() };
                style
            })
            .on_press(Msg::OpenVoice(channel.id.clone()));
        let entry = Self::with_menu(entry, MenuTarget::Channel(channel.id.clone()));
        let mut column = column![entry].spacing(1);
        for (name, speaking, state) in self.voice_members(guild, &channel.id) {
            let mut flags = String::new();
            if state.self_deaf || state.deaf {
                flags.push_str(" (deafened)");
            } else if state.self_mute || state.mute {
                flags.push_str(" (muted)");
            }
            if state.self_video {
                flags.push_str(" · camera");
            }
            let color = if speaking { p().online } else { p().muted };
            let weight = if speaking { BOLD } else { Font::DEFAULT };
            let mut line = row![Space::new(28, 0), label(format!("{name}{flags}")).size(12).color(color).font(weight)]
                .spacing(6)
                .align_y(Alignment::Center);
            if state.self_stream {
                line = line.push(container(text("LIVE").size(9).font(BOLD).color(Color::WHITE)).padding([1, 4]).style(fill(p().mention, 3.0)));
                // You can watch streams in the call you're in.
                let mine = self.me.as_ref().is_some_and(|me| me.id == state.user_id);
                if joined && !mine && self.settings.video {
                    let watching = self.watching.as_ref().is_some_and(|w| w.user_id == state.user_id);
                    let (name, msg) =
                        if watching { ("Stop", Msg::StopWatching) } else { ("Watch", Msg::WatchStream(state.user_id.clone())) };
                    line = line.push(button(text(name).size(10)).padding([1, 6]).style(quiet).on_press(msg));
                }
            }
            column = column.push(line);
        }
        column.into()
    }

    /// The call you're in: status, Mute, Deafen, Disconnect.
    fn voice_panel(&self) -> Element<'_, Msg> {
        let Some(call) = &self.voice else { return Space::new(0, 0).into() };
        let channel = self.channel_name(&call.channel_id);
        let heading_color = if call.connected { p().online } else { p().idle };
        // Equal-width controls in one row; "on" states (muted, sharing) read in red.
        let toggle = |name: &'static str, on: bool, msg: Msg| {
            button(container(text(name).size(11)).center_x(Length::Fill))
                .width(Length::Fill)
                .padding([5, 4])
                .style(if on { button::danger } else { quiet })
                .on_press(msg)
        };
        let sharing = self.sharing.is_some();
        let share = if sharing { ("Stop", Msg::StopSharing) } else { ("Share", Msg::ShareScreen) };
        let controls = row![
            toggle(if call.self_mute { "Unmute" } else { "Mute" }, call.self_mute, Msg::ToggleMute),
            toggle(if call.self_deaf { "Undeafen" } else { "Deafen" }, call.self_deaf, Msg::ToggleDeafen),
            toggle(share.0, sharing, share.1),
            toggle("Leave", true, Msg::LeaveVoice),
        ]
        .spacing(4);
        let share_status: Element<'_, Msg> = match &self.sharing {
            Some(share) => row![
                container(text("LIVE").size(9).font(BOLD).color(Color::WHITE)).padding([1, 4]).style(fill(p().mention, 3.0)),
                label(share.status.as_str()).size(11).color(p().muted),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into(),
            None => Space::new(0, 0).into(),
        };
        // Everyone in the call sees the same code; compare it to confirm E2EE.
        let privacy: Element<'_, Msg> = match call.encryption.as_ref().and_then(|e| e.privacy_code.as_deref()) {
            Some(code) => column![
                text("Voice privacy code").size(10).color(p().muted),
                text(code).size(11).font(Font::MONOSPACE).color(p().online),
            ]
            .into(),
            None => Space::new(0, 0).into(),
        };
        container(
            column![
                label(if call.connected { "Voice connected" } else { "Voice connecting" })
                    .size(12)
                    .font(BOLD)
                    .color(heading_color),
                label(format!("🔊 {channel}")).size(12),
                label(call.status.as_str()).size(11).color(p().muted),
                privacy,
                share_status,
                Space::with_height(2),
                controls,
            ]
            .spacing(4),
        )
        .padding([10, 10])
        .width(Length::Fill)
        .style(fill(p().rail, 0.0))
        .into()
    }

    fn channel_button<'a>(&self, channel: &'a Channel, prefix: &str) -> Element<'a, Msg> {
        let active = self.selected_channel.as_deref() == Some(channel.id.as_str());
        let mentions = self.mention_count(&channel.id);
        let unread = mentions > 0 || self.shows_unread(&channel.id);
        let muted = self.channel_muted(&channel.id);

        let mut name = label(notify::truncate(&format!("{prefix} {}", channel.label()), CHANNEL_CHARS)).size(14).width(Length::Fill);
        if unread {
            name = name.font(BOLD);
        }
        let mut content = row![name].spacing(4).align_y(Alignment::Center);
        if mentions > 0 {
            content = content.push(mention_badge(mentions));
        }

        let entry = button(content)
            .width(Length::Fill)
            .padding([4, 8])
            .style(move |theme, status| {
                let mut style = button::text(theme, status);
                let hovered = matches!(status, button::Status::Hovered);
                style.text_color = match (active || hovered || unread, muted) {
                    (true, _) => p().text,
                    // Muted channels fade further back.
                    (false, true) => Color { a: 0.45, ..p().muted },
                    (false, false) => p().muted,
                };
                if active {
                    style.background = Some(Background::Color(p().selected));
                }
                style.border = Border { radius: 4.0.into(), ..Border::default() };
                style
            })
            .on_press(Msg::SelectChannel(channel.id.clone()));
        Self::with_menu(entry, MenuTarget::Channel(channel.id.clone()))
    }

    fn chat(&self) -> Element<'_, Msg> {
        let Some(channel_id) = self.selected_channel.as_deref() else {
            if self.selected_guild.is_none() && !self.is_bot() {
                return self.home_view();
            }
            return container(
                column![
                    text("Pick a channel").color(p().muted),
                    text(self.status.as_str()).size(12).color(p().muted),
                ]
                .spacing(6)
                .align_x(Alignment::Center),
            )
            .center(Length::Fill)
            .style(fill(p().chat, 0.0))
            .into();
        };
        let title = match self.channel(channel_id) {
            Some(c) if c.is_text() => format!("# {}", c.label()),
            Some(c) if c.is_forum() => format!("☰ {}", c.label()),
            Some(c) if c.is_thread() => {
                let parent = c.parent_id.as_deref().map(|p| self.channel_name(p)).unwrap_or_default();
                format!("# {parent} › {}", c.label())
            }
            Some(c) if c.is_voice() => format!("🔊 {}", c.label()),
            Some(c) => format!("@ {}", c.label()),
            None => "Unknown channel".to_owned(),
        };
        // Beside the voice stage the chat is narrow: title only in its header.
        let narrow = self.channel(channel_id).is_some_and(Channel::is_voice);

        let topic = self
            .channel(channel_id)
            .and_then(|c| c.topic.as_deref())
            .map(|t| notify::truncate(t, 120))
            .unwrap_or_default();
        // Title and topic sit side by side; a 1 px rule separates them when
        // there's a topic (quieter than the "|" glyph, more intentional).
        let divider: Element<'_, Msg> = if topic.is_empty() {
            Space::new(0, 0).into()
        } else {
            container(Space::new(1, 18)).style(fill(p().selected, 0.0)).into()
        };
        let mut header = row![
            label(title.clone()).size(16).font(BOLD),
            divider,
            // One line, cut off at the buttons; the full topic is in the tooltip.
            tooltip(
                container(label(topic.clone()).size(13).color(p().muted).wrapping(text::Wrapping::None))
                    .width(Length::Fill)
                    .height(18)
                    .clip(true),
                tip(topic),
                tooltip::Position::Bottom,
            ),
        ]
        .spacing(10)
        .padding([12, 16])
        .align_y(Alignment::Center);
        if !self.is_bot() && !narrow {
            // One picker for mute and notification level keeps the header short.
            let choices: Vec<ChannelChoice> = match self.selected_guild {
                Some(_) => NotifyLevel::CHANNEL.into_iter().map(ChannelChoice::Level).chain([ChannelChoice::Muted]).collect(),
                None => vec![ChannelChoice::Level(NotifyLevel::All), ChannelChoice::Muted],
            };
            let current = if self.channel_muted(channel_id) {
                ChannelChoice::Muted
            } else {
                match self.channel_notify_setting(channel_id) {
                    NotifyLevel::Inherit if self.selected_guild.is_none() => ChannelChoice::Level(NotifyLevel::All),
                    level => ChannelChoice::Level(level),
                }
            };
            let id = channel_id.to_owned();
            let picker = pick_list(choices, Some(current), move |choice| match choice {
                ChannelChoice::Muted => Msg::MuteChannel(id.clone(), true),
                ChannelChoice::Level(level) => Msg::ChannelNotify(id.clone(), level),
            })
            .text_size(12)
            .padding([4, 8]);
            header = header.push(tooltip(picker, tip("Notifications for this channel"), tooltip::Position::Bottom));
        }
        let panel_open = |check: fn(&Panel) -> bool| self.panel.as_ref().is_some_and(check);
        if !narrow {
            header = header
                .push(small_button("Pins", panel_open(|p| matches!(p, Panel::Pins(_))), Msg::ShowPins))
                .push(small_button("Search", panel_open(|p| matches!(p, Panel::Search(_))), Msg::OpenSearch));
        }
        if self.selected_guild.is_some() && self.settings.member_list && !narrow {
            let showing = self.show_members && self.panel.is_none();
            header = header.push(small_button("Members", showing, Msg::ToggleMembers));
        }
        let header = column![header, rule()];

        if self.channel(channel_id).is_some_and(Channel::is_forum) {
            return container(column![header, self.forum_view(channel_id)])
                .width(Length::Fill)
                .height(Length::Fill)
                .style(fill(p().chat, 0.0))
                .into();
        }

        let body: Element<'_, Msg> = match self.logs.get(channel_id) {
            None => centered("No messages loaded"),
            Some(log) if log.entries.is_empty() && log.loading => centered("Loading…"),
            Some(log) if log.entries.is_empty() => centered("No messages yet"),
            Some(log) => {
                let mut list = Column::new().spacing(4).padding([8, 16]);
                if log.loading {
                    list = list.push(text("Loading older messages…").size(12).color(p().muted));
                } else if log.can_load_older() {
                    list = list.push(
                        button(text("Load older messages").size(12))
                            .on_press(Msg::LoadOlder)
                            .style(quiet),
                    );
                } else if log.has_more {
                    list = list.push(text("Older history is not kept in memory.").size(12).color(p().muted));
                }
                let now = Local::now();
                let today = now.date_naive();
                let mut previous: Option<&Entry> = None;
                let mut divider_shown = false;
                for entry in &log.entries {
                    let day = entry.message.timestamp.with_timezone(&Local).date_naive();
                    let new_day = previous.is_none_or(|p| p.message.timestamp.with_timezone(&Local).date_naive() != day);
                    if new_day {
                        list = list.push(separator(day_label(day, today), p().muted));
                    }
                    let first_unread = !divider_shown
                        && !self.is_mine(&entry.message)
                        && self.unread_from.is_some_and(|read| snowflake(&entry.message.id) > read);
                    if first_unread {
                        divider_shown = true;
                        list = list.push(separator("New messages".to_owned(), p().mention));
                    }
                    let new_group = new_day
                        || first_unread
                        || previous.is_none_or(|p| {
                            p.message.is_system()
                                || p.message.author.id != entry.message.author.id
                                || (entry.message.timestamp - p.message.timestamp).num_minutes() > GROUP_MINUTES
                        });
                    list = list.push(self.message_view(entry, new_group, now));
                    previous = Some(entry);
                }
                let messages = thin_scroll(list)
                    .id(scrollable::Id::new(MESSAGES_ID))
                    .anchor_bottom()
                    .width(Length::Fill)
                    .height(Length::Fill);
                if log.detached {
                    let bar = container(
                        row![
                            text("You're viewing older messages").size(12).color(p().muted).width(Length::Fill),
                            button(text("Jump to present").size(12)).padding([3, 10]).on_press(Msg::JumpToPresent),
                        ]
                        .align_y(Alignment::Center),
                    )
                    .padding([6, 16])
                    .style(fill(p().sidebar, 0.0));
                    column![bar, messages].into()
                } else {
                    messages.into()
                }
            }
        };

        let placeholder = match (self.editing.is_some(), narrow) {
            (true, _) => "Edit message".to_owned(),
            (false, true) => "Message".to_owned(),
            (false, false) => format!("Message {title}"),
        };
        // Borderless: the filled bar around it (with + and the tools) is the box.
        let composer = text_input(&placeholder, &self.composer)
            .on_input(Msg::ComposerChanged)
            .on_submit(Msg::Send)
            .padding([12, 4])
            .style(|theme, status| {
                let mut style = text_input::default(theme, status);
                style.background = Background::Color(Color::TRANSPARENT);
                style.border = Border::default();
                style.placeholder = p().muted;
                style.value = p().text;
                style.selection = Color { a: 0.35, ..p().accent };
                style
            });
        // Quiet icon button; the text field is the hero, not the attach glyph.
        let attach = button(container(text("+").size(20).color(p().muted)).center(Length::Fill))
            .width(36)
            .height(44)
            .padding(0)
            .style(|theme, status| {
                let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
                let mut style = button::text(theme, status);
                style.text_color = p().text;
                style.background = hovered.then_some(Background::Color(p().selected));
                style.border = Border { radius: 6.0.into(), ..Border::default() };
                style
            })
            .on_press(Msg::PickFiles);
        let typing = label(self.typing_line(channel_id).unwrap_or_default()).size(12).color(p().muted);
        let can_send = self.can_send(channel_id);
        let mut tools = Row::new().spacing(4).align_y(Alignment::Center);
        if can_send {
            if self.can_attach(channel_id) {
                tools = tools.push(attach);
            }
            tools = tools.push(composer);
        } else {
            // In place of the message box, like Discord's read-only channels.
            let notice = text("You can't talk in this channel: your permissions don't allow sending messages here.").size(13).color(p().muted);
            tools = tools.push(container(notice).padding([13, 10]).width(Length::Fill));
        }
        // Beside the voice stage only GIF fits next to the message box.
        if can_send && !narrow && self.selected_guild.is_some() && self.selected_guild().is_some_and(|g| !g.stickers.is_empty()) {
            tools = tools.push(composer_tool("Sticker", self.sticker_picker, Msg::ToggleStickerPicker));
        }
        if can_send && self.settings.gif_picker && !self.is_bot() {
            tools = tools.push(composer_tool("GIF", self.gif_picker.is_some(), Msg::ToggleGifPicker));
        }
        if can_send && !narrow {
            tools = tools.push(composer_tool("Poll", self.poll_draft.is_some(), Msg::OpenPollDraft));
        }
        // Fixed height so the composer doesn't jump when someone starts typing.
        let footer = column![
            self.suggestions(),
            self.command_hint(),
            self.poll_editor(),
            self.sticker_picker_view(),
            self.gif_picker_view(),
            self.pending_files_view(),
            self.composer_banner(channel_id),
            container(tools).padding([0, 6]).width(Length::Fill).style(fill(raised(), 8.0)),
            container(typing).height(18).padding([2, 4]),
        ]
        .padding([0, 16]);

        let chat = container(column![header, body, footer]).width(Length::Fill).height(Length::Fill);
        let Some(wallpaper) = theme::wallpaper() else { return chat.style(fill(p().chat, 0.0)).into() };
        // Chat colour, then the picture faded over it, then the messages.
        let picture = image(wallpaper.image)
            .width(Length::Fill)
            .height(Length::Fill)
            .content_fit(iced::ContentFit::Cover)
            .opacity(wallpaper.opacity);
        stack![container(Space::new(Length::Fill, Length::Fill)).style(fill(p().chat, 0.0)), picture, chat].into()
    }

    /// Files waiting to be sent with the next message.
    fn pending_files_view(&self) -> Element<'_, Msg> {
        if self.pending_files.is_empty() {
            return Space::new(0, 0).into();
        }
        let mut files = Row::new().spacing(6);
        for (index, file) in self.pending_files.iter().enumerate() {
            let chip = row![
                label(format!("{} ({})", file.name, human_size(file.size))).size(12),
                button(text("Remove").size(11)).padding([1, 6]).style(button::text).on_press(Msg::RemoveFile(index)),
            ]
            .spacing(4)
            .align_y(Alignment::Center);
            files = files.push(container(chip).padding([3, 8]).style(fill(p().sidebar, 6.0)));
        }
        let limit = text(format!("Up to {} per file", human_size(UPLOAD_LIMIT))).size(11).color(p().muted);
        column![files.wrap(), limit, Space::new(0, 2)].spacing(4).into()
    }

    fn member_panel(&self) -> Element<'_, Msg> {
        let mut list = Column::new().spacing(2).padding(10);
        let members = self.selected_guild.as_deref().and_then(|id| self.member_lists.get(id));
        match members {
            None if self.is_bot() => {
                list = list.push(text("Member lists aren't available for bot logins.").size(12).color(p().muted));
            }
            None => list = list.push(text("Loading members…").size(12).color(p().muted)),
            Some(members) => {
                for row in members.rows.iter().flatten() {
                    list = list.push(match row {
                        Item::Group(group) => {
                            let name = match group.id.as_str() {
                                "online" => "Online".to_owned(),
                                "offline" => "Offline".to_owned(),
                                id => self.role(id).map_or_else(|| "Members".to_owned(), |r| r.name.clone()),
                            };
                            let heading = row![
                                label(name).size(12).color(p().muted).width(Length::Fill),
                                text(members.count(group).to_string()).size(12).color(p().muted),
                            ]
                            .align_y(Alignment::Center);
                            container(heading).padding([8, 0]).into()
                        }
                        Item::Member(member) => self.member_row(member),
                    });
                }
            }
        }
        container(thin_scroll(list)).width(220).height(Length::Fill).style(fill(p().sidebar, 0.0)).into()
    }

    fn member_row<'a>(&'a self, member: &'a crate::discord::member_list::Member) -> Element<'a, Msg> {
        let status = member.status();
        let dot = container(Space::new(8, 8)).style(fill(status_color(status), 4.0));
        let name_color = if status == "offline" { p().muted } else { p().text };
        let mut line = Row::new().spacing(8).align_y(Alignment::Center);
        if self.settings.load_avatars {
            line = line.push(self.avatar(&member.user, MEMBER_AVATAR_SIZE));
        }
        let name = label(notify::truncate(member.name(), MEMBER_CHARS)).size(13).color(name_color).width(Length::Fill);
        let line = line.push(name).push(dot);
        let row = button(line).width(Length::Fill).padding([2, 4]).style(subtle).on_press(Msg::ShowProfile(member.user.clone()));
        Self::with_menu(row, MenuTarget::User(member.user.clone(), self.selected_guild.clone()))
    }

    fn side_panel<'a>(&'a self, panel: &'a Panel) -> Element<'a, Msg> {
        let (title, content): (String, Element<'a, Msg>) = match panel {
            Panel::Search(search) => {
                let input = text_input("Search messages…", &search.query)
                    .id(text_input::Id::new(SEARCH_INPUT))
                    .on_input(Msg::SearchQuery)
                    .on_submit(Msg::SearchSubmit)
                    .padding(8);
                let results: Element<'a, Msg> = match &search.results {
                    _ if search.loading => text("Searching…").size(12).color(p().muted).into(),
                    None => text("Press Enter to search.").size(12).color(p().muted).into(),
                    Some(Err(error)) => text(error.as_str()).size(12).color(p().error).into(),
                    Some(Ok(messages)) if messages.is_empty() => text("No results.").size(12).color(p().muted).into(),
                    Some(Ok(messages)) => self.message_results(messages, search.guild_id.is_some()),
                };
                (format!("Search {}", search.scope), column![input, results].spacing(10).into())
            }
            Panel::Pins(pins) => {
                let content: Element<'a, Msg> = match &pins.result {
                    None => text("Loading pins…").size(12).color(p().muted).into(),
                    Some(Err(error)) => text(format!("Could not load pins: {error}")).size(12).color(p().error).into(),
                    Some(Ok(messages)) if messages.is_empty() => {
                        text("No pinned messages in this channel.").size(12).color(p().muted).into()
                    }
                    Some(Ok(messages)) => self.message_results(messages, false),
                };
                ("Pinned messages".to_owned(), content)
            }
            Panel::Profile(profile) => ("Profile".to_owned(), self.profile_view(profile)),
        };
        let close = button(text("Close").size(12)).padding([3, 8]).style(quiet).on_press(Msg::ClosePanel);
        let heading = row![label(title).size(14).font(BOLD).width(Length::Fill), close].align_y(Alignment::Center);
        container(column![heading, thin_scroll(content).height(Length::Fill)].spacing(12))
            .padding(12)
            .width(300)
            .height(Length::Fill)
            .style(fill(p().sidebar, 0.0))
            .into()
    }

    /// Clickable result cards (search, pins); a click jumps to the message.
    fn message_results<'a>(&'a self, messages: &'a [Message], show_channel: bool) -> Element<'a, Msg> {
        let today = Local::now().date_naive();
        let mut list = Column::new().spacing(6);
        for message in messages {
            let mut meta = row![label(message.author.display_name()).size(12).font(BOLD).color(p().accent)].spacing(6);
            if show_channel {
                meta = meta.push(label(format!("#{}", self.channel_name(&message.channel_id))).size(11).color(p().muted));
            }
            meta = meta.push(text(message_time(&message.timestamp, today, self.settings.clock_24h)).size(11).color(p().muted));
            let body = label(notify::truncate(&self.plain_text(message), 220)).size(13);
            let card = column![meta.align_y(Alignment::Center), body].spacing(3);
            let jump = Msg::JumpToMessage { channel_id: message.channel_id.clone(), message_id: message.id.clone() };
            list = list.push(
                button(container(card).padding(8).width(Length::Fill).style(fill(p().chat, 6.0)))
                    .padding(0)
                    .style(subtle)
                    .on_press(jump),
            );
        }
        list.into()
    }

    fn profile_view<'a>(&'a self, panel: &'a crate::session::ProfilePanel) -> Element<'a, Msg> {
        let user = &panel.user;
        let mut card = Column::new().spacing(8).align_x(Alignment::Center);
        if self.settings.load_avatars {
            card = card.push(self.avatar(user, 80.0));
        }
        let member = panel.profile.as_ref().and_then(|r| r.as_ref().ok()).and_then(|p| p.guild_member.as_ref());
        let shown = member.and_then(|m| m.nick.as_deref()).unwrap_or(user.display_name());
        card = card
            .push(label(shown).size(18).font(BOLD))
            .push(label(format!("@{}", user.username)).size(12).color(p().muted));
        let mut details = Column::new().spacing(8).width(Length::Fill);
        let about = panel.profile.as_ref().and_then(|r| r.as_ref().ok()).and_then(|p| p.user_profile.as_ref());
        if let Some(pronouns) = about.and_then(|a| a.pronouns.as_deref()).filter(|t| !t.is_empty()) {
            card = card.push(label(pronouns).size(12).color(p().muted));
        }
        if let Some(bio) = about.and_then(|a| a.bio.as_deref()).filter(|t| !t.is_empty()) {
            details = details.push(section_label("About me")).push(label(bio).size(13));
        }
        if let Some(created) = snowflake_time(&user.id) {
            details = details.push(section_label("Discord member since")).push(text(created.format("%-d %B %Y").to_string()).size(13));
        }
        if let Some(joined) = member.and_then(|m| m.joined_at) {
            details = details.push(section_label("Joined this server")).push(text(joined.format("%-d %B %Y").to_string()).size(13));
        }
        if let Some(member) = member.filter(|m| !m.roles.is_empty()) {
            let mut roles = Row::new().spacing(4);
            for role in member.roles.iter().filter_map(|id| self.role(id)) {
                let color = match role.color {
                    0 => p().muted,
                    c => rgb((c >> 16) as u8, (c >> 8) as u8, c as u8),
                };
                let dot = container(Space::new(8, 8)).style(fill(color, 4.0));
                let chip = row![dot, label(role.name.as_str()).size(12)].spacing(4).align_y(Alignment::Center);
                roles = roles.push(container(chip).padding([2, 8]).style(fill(p().chat, 8.0)));
            }
            details = details.push(section_label("Roles")).push(roles.wrap());
        }
        match &panel.profile {
            None if !self.is_bot() && !self.offline => details = details.push(text("Loading profile…").size(12).color(p().muted)),
            Some(Err(error)) => details = details.push(text(format!("Could not load profile: {error}")).size(12).color(p().error)),
            _ => {}
        }
        let mine = self.me.as_ref().is_some_and(|me| me.id == user.id);
        if !mine && !self.is_bot() {
            details = details.push(
                button(container(text("Message").font(BOLD)).center_x(Length::Fill))
                    .width(Length::Fill)
                    .padding(8)
                    .on_press(Msg::OpenDm(user.id.clone())),
            );
            use crate::discord::model::{RELATIONSHIP_BLOCKED, RELATIONSHIP_FRIEND, RELATIONSHIP_INCOMING, RELATIONSHIP_OUTGOING};
            let id = user.id.clone();
            let friend: Option<(&str, Msg)> = match self.relationship(&id) {
                None if !user.bot => Some(("Add Friend", SocialMsg::AddFriend(id.clone()))),
                Some(RELATIONSHIP_INCOMING) => Some(("Accept Friend Request", SocialMsg::Accept(id.clone()))),
                Some(RELATIONSHIP_OUTGOING) => Some(("Cancel Friend Request", SocialMsg::Remove(id.clone()))),
                Some(RELATIONSHIP_FRIEND) => Some(("Remove Friend", SocialMsg::Remove(id.clone()))),
                Some(RELATIONSHIP_BLOCKED) => Some(("Unblock", SocialMsg::Remove(id.clone()))),
                _ => None,
            }
            .map(|(name, msg)| (name, Msg::Social(msg)));
            let mut actions = Row::new().spacing(6);
            if let Some((name, msg)) = friend {
                actions = actions.push(button(text(name).size(12)).padding([5, 10]).style(quiet).on_press(msg));
            }
            if !self.is_blocked(&id) {
                let block = Msg::Social(SocialMsg::Block(id));
                actions = actions.push(button(text("Block").size(12)).padding([5, 10]).style(button::danger).on_press(block));
            }
            details = details.push(actions);
        }
        column![card, details].spacing(16).into()
    }

    /// Ctrl+K: a centred search box over a dimmed window.
    fn switcher_view<'a>(&'a self, query: &'a str) -> Element<'a, Msg> {
        let input = text_input("Where would you like to go?", query)
            .id(text_input::Id::new(SWITCHER_INPUT))
            .on_input(Msg::SwitcherQuery)
            .on_submit(Msg::SwitcherSubmit)
            .size(16)
            .padding(12);
        let mut results = Column::new().spacing(2);
        let items = self.switcher_results();
        if items.is_empty() {
            let hint = if query.trim().is_empty() { "Type a channel, DM or server name." } else { "No matches." };
            results = results.push(container(text(hint).size(13).color(p().muted)).padding(8));
        }
        for (index, item) in items.into_iter().enumerate() {
            let mut line = row![label(item.label).size(14).font(if item.unread { BOLD } else { Font::DEFAULT })]
                .spacing(8)
                .align_y(Alignment::Center);
            if item.mentions > 0 {
                line = line.push(mention_badge(item.mentions));
            }
            line = line.push(Space::with_width(Length::Fill)).push(label(item.context).size(12).color(p().muted));
            // Enter picks the first result; show which that is.
            let first = index == 0;
            results = results.push(
                button(line)
                    .width(Length::Fill)
                    .padding([6, 10])
                    .style(move |theme, status| {
                        let mut style = subtle(theme, status);
                        if first && style.background.is_none() {
                            style.background = Some(Background::Color(p().selected));
                        }
                        style
                    })
                    .on_press(Msg::SwitcherPick(item.target)),
            );
        }
        let footer = text("Enter opens the highlighted result · Esc closes").size(11).color(p().muted);
        let card = container(column![input, results, footer].spacing(10))
            .padding(16)
            .width(520)
            .style(bordered(p().sidebar, 10.0));
        // The card swallows its own clicks; a click outside closes.
        let card = container(opaque(card)).center_x(Length::Fill).padding([90, 0]);
        opaque(mouse_area(container(card).width(Length::Fill).height(Length::Fill).style(fill(p().backdrop, 0.0))).on_press(Msg::CloseSwitcher))
    }

    fn avatar(&self, user: &User, size: f32) -> Element<'static, Msg> {
        match self.images.get(&self.avatar_url(user)) {
            Some(handle) => image(handle.clone()).width(size).height(size).into(),
            None => container(Space::new(size, size)).style(fill(p().selected, size / 2.0)).into(),
        }
    }

    /// "Replying to …" / "Editing message" above the composer.
    fn composer_banner(&self, channel_id: &str) -> Element<'_, Msg> {
        let (description, cancel): (Element<'_, Msg>, Msg) = if self.editing.is_some() {
            (text("Editing message · Enter to save, Esc to cancel").size(12).color(p().muted).into(), Msg::Escape)
        } else if let Some(reply_id) = &self.replying {
            let author = self
                .find_entry(channel_id, reply_id)
                .map_or_else(|| "a message".to_owned(), |e| e.message.author.display_name().to_owned());
            let ping = if self.reply_ping { "@ on" } else { "@ off" };
            let description = row![
                label(format!("Replying to {author}")).size(12).color(p().muted).width(Length::Fill),
                button(text(ping).size(11)).padding([2, 8]).style(quiet).on_press(Msg::ToggleReplyPing),
            ]
            .spacing(8)
            .align_y(Alignment::Center);
            (description.into(), Msg::CancelReply)
        } else {
            return Space::new(0, 0).into();
        };
        container(
            row![description, button(text("Cancel").size(11)).padding([2, 8]).style(quiet).on_press(cancel)]
                .spacing(8)
                .align_y(Alignment::Center),
        )
        .padding([4, 10])
        .width(Length::Fill)
        .style(fill(p().sidebar, 6.0))
        .into()
    }

    /// Emoji autocomplete for a trailing `:query`.
    fn suggestions(&self) -> Element<'_, Msg> {
        let Some(query) = markup::shortcode_query(&self.composer) else {
            return Space::new(0, 0).into();
        };
        let suggestions = markup::suggestions(query, self.custom_emojis(), SUGGESTION_LIMIT);
        if suggestions.is_empty() {
            return Space::new(0, 0).into();
        }
        let mut list = Column::new().spacing(2).padding(6);
        for suggestion in suggestions {
            list = list.push(
                button(label(suggestion.label).size(14))
                    .width(Length::Fill)
                    .padding([3, 8])
                    .style(button::text)
                    .on_press(Msg::InsertEmoji(suggestion.insert)),
            );
        }
        container(list).width(Length::Fill).style(fill(p().sidebar, 6.0)).into()
    }

    fn message_view<'a>(&'a self, entry: &'a Entry, new_group: bool, now: DateTime<Local>) -> Element<'a, Msg> {
        let message = &entry.message;
        if self.hides_blocked(message) {
            return self.blocked_view(message);
        }
        if let Some(line) = message.is_system().then(|| message.system_text()).flatten() {
            // One quiet line, like Discord's join and boost notices.
            let time = message_time(&message.timestamp, now.date_naive(), self.settings.clock_24h);
            let row = row![
                text("→").size(14).color(p().online),
                label(line).size(13).color(p().muted),
                text(time).size(11).color(p().muted).wrapping(text::Wrapping::None),
            ]
            .spacing(10)
            .align_y(Alignment::Center);
            let indent = if self.settings.compact { 6.0 } else { AVATAR_SIZE + 16.0 - 14.0 };
            return container(row).padding(iced::Padding { top: 4.0, bottom: 4.0, left: indent, right: 6.0 }).width(Length::Fill).into();
        }
        let is_reply = message.kind == REPLY;
        let guild_id = self.guild_of(&message.channel_id).map(|g| g.id.clone());
        let author_menu = MenuTarget::User(message.author.clone(), guild_id);
        // Replies always get a header so the preview sits above an author.
        let new_group = new_group || is_reply;
        let mut content = Column::new().spacing(2);
        if is_reply {
            content = content.push(self.reply_preview(message));
        }
        let compact = self.settings.compact;
        // The name opens the author's profile.
        let author = button(label(message.author.display_name()).font(BOLD).color(p().accent))
            .padding(0)
            .style(button::text)
            .on_press(Msg::ShowProfile(message.author.clone()));
        let author = Self::with_menu(author, author_menu.clone());
        let body = self.body_view(entry, &entry.body, now);
        if compact {
            let time = message.timestamp.with_timezone(&Local).format(clock(self.settings.clock_24h, false)).to_string();
            content = content.push(
                row![text(time).size(11).color(p().muted).width(52), author, container(body).width(Length::Fill)]
                    .spacing(8)
                    .align_y(Alignment::Start),
            );
        } else {
            if new_group {
                let time = message_time(&message.timestamp, now.date_naive(), self.settings.clock_24h);
                // The time stays on one line however narrow the chat is.
                let time = text(time).size(11).color(p().muted).wrapping(text::Wrapping::None);
                content = content.push(row![author, time].spacing(8).align_y(Alignment::Center));
            }
            content = content.push(body);
        }
        for attachment in &message.attachments {
            if attachment.is_audio() {
                content = content.push(self.audio_attachment(attachment));
                continue;
            }
            let preview = self.attachment_preview(attachment).filter(|_| self.settings.load_images);
            let file = format!("[file] {} ({})", attachment.filename, human_size(attachment.size));
            content = content.push(match preview {
                Some(preview) => self.preview_image(preview, attachment.url.clone(), file),
                None => link_text(file, attachment.url.clone()),
            });
        }
        for (embed, bodies) in message.embeds.iter().zip(&entry.embeds) {
            content = content.push(self.embed_view(embed, bodies, now));
        }
        for sticker in &message.sticker_items {
            content = content.push(self.sticker_view(sticker, STICKER_SIZE));
        }
        if let Some(poll) = &message.poll {
            content = content.push(self.poll_view(message, poll, now));
        }
        if !message.components.is_empty() {
            let mut texts = 0;
            let mut parts = Column::new().spacing(6);
            for component in &message.components {
                parts = parts.push(self.component_view(entry, component, &mut texts, now));
            }
            content = content.push(parts);
        }
        if message.edited_timestamp.is_some() {
            content = content.push(text("(edited)").size(11).color(p().muted));
        }
        if !message.reactions.is_empty() {
            content = content.push(self.reactions_view(message));
        }
        if self.reacting_to.as_deref() == Some(message.id.as_str()) {
            content = content.push(self.reaction_picker(message));
        }
        let editing = self.editing.as_ref().is_some_and(|e| e.message_id == message.id);
        let replying = self.replying.as_deref() == Some(message.id.as_str());
        // Avatar column; continuation messages are indented to line up.
        let gutter: Element<'_, Msg> = match (self.settings.load_avatars && !compact, new_group) {
            (false, _) => Space::new(0, 0).into(),
            (true, true) => Self::with_menu(
                button(self.avatar(&message.author, AVATAR_SIZE))
                    .padding(0)
                    .style(button::text)
                    .on_press(Msg::ShowProfile(message.author.clone())),
                author_menu,
            ),
            (true, false) => Space::new(AVATAR_SIZE, 0).into(),
        };
        let line = row![gutter, content.width(Length::Fill)].spacing(10);
        let vertical = match (compact, new_group) {
            (true, _) => 1,
            (false, true) => 6,
            (false, false) => 2,
        };
        let padded = container(line).padding([vertical, 6]).width(Length::Fill);
        let highlighted = self.highlighted.as_deref() == Some(message.id.as_str());
        let padded = if highlighted {
            padded.style(fill(p().highlight_row, 4.0))
        } else if editing || replying {
            padded.style(fill(p().selected, 4.0))
        } else if self.mentions_me_explicitly(message) {
            padded.style(fill(p().mentioned_row, 4.0))
        } else {
            padded
        };
        // The hover toolbar floats over the message's top-right corner, like
        // Discord's, so it never takes width from the message (narrow chats).
        let padded: Element<'a, Msg> = if self.hovered.as_deref() == Some(message.id.as_str()) {
            let actions = container(self.message_actions(message)).width(Length::Fill).align_x(Horizontal::Right).padding([2, 8]);
            stack![padded, actions].into()
        } else {
            padded.into()
        };
        let target = MenuTarget::Message { channel_id: message.channel_id.clone(), message_id: message.id.clone() };
        mouse_area(padded)
            .on_enter(Msg::HoverMessage(Some(message.id.clone())))
            .on_exit(Msg::LeaveMessage(message.id.clone()))
            .on_right_press(Msg::Social(SocialMsg::OpenMenu(target)))
            .into()
    }

    /// Reply / Edit / Delete, shown while hovering.
    /// A compact toolbar: React, Reply, Edit, Delete.
    fn message_actions(&self, message: &Message) -> Element<'static, Msg> {
        let action = |name: &'static str, danger: bool, msg: Msg| {
            button(text(name).size(12))
                .padding([3, 8])
                .style(move |theme, status| {
                    let mut style = button::text(theme, status);
                    let hovered = matches!(status, button::Status::Hovered);
                    style.text_color = match (danger, hovered) {
                        (true, _) => p().mention,
                        (false, true) => p().text,
                        (false, false) => p().muted,
                    };
                    style.background = hovered.then_some(Background::Color(p().selected));
                    style.border = Border { radius: 4.0.into(), ..Border::default() };
                    style
                })
                .on_press(msg)
        };
        let mut actions = row![
            action("React", false, Msg::OpenReactionPicker(message.id.clone())),
            action("Reply", false, Msg::StartReply(message.id.clone())),
        ];
        if self.is_mine(message) {
            actions = actions.push(action("Edit", false, Msg::StartEdit(message.id.clone())));
        }
        if self.can_delete(message) {
            let confirming = self.confirm_delete.as_deref() == Some(message.id.as_str());
            let label = if confirming { "Confirm delete" } else { "Delete" };
            actions = actions.push(action(label, true, Msg::DeleteMessage(message.id.clone())));
        }
        // Sidebar (not rail) so the bar always reads lifted over the chat
        // surface — in light themes rail is lighter than chat.
        container(actions)
            .padding(2)
            .style(|_| container::Style {
                background: Some(Background::Color(p().sidebar)),
                border: Border { color: p().selected, width: 1.0, radius: 6.0.into() },
                ..container::Style::default()
            })
            .into()
    }

    fn body_view<'a>(&'a self, entry: &'a Entry, body: &'a Body, now: DateTime<Local>) -> Element<'a, Msg> {
        let mut column = Column::new().spacing(2);
        for block in &body.blocks {
            column = column.push(self.block_view(entry, body, block, now));
        }
        column.into()
    }

    /// A clickable image preview that opens the original in the browser.
    fn preview_image(&self, preview: images::Preview, original: String, fallback: String) -> Element<'static, Msg> {
        match self.images.get(&preview.url) {
            Some(handle) => button(image(handle.clone()).width(preview.width).height(preview.height))
                .padding(0)
                .style(button::text)
                .on_press(Msg::OpenLink(original))
                .into(),
            // Same size while loading so the list doesn't jump.
            None => container(label(fallback).size(12).color(p().muted))
                .width(preview.width)
                .height(preview.height)
                .padding(8)
                .style(fill(p().embed_bg, 4.0))
                .into(),
        }
    }

    fn embed_view<'a>(&'a self, embed: &'a Embed, bodies: &'a EmbedBodies, now: DateTime<Local>) -> Element<'a, Msg> {
        let media = self.embed_preview(embed).filter(|_| self.settings.load_images);
        let original = embed.url.clone().unwrap_or_default();
        if embed.is_media_only() {
            return match media {
                Some(preview) => self.preview_image(preview, original.clone(), original),
                None => Space::new(0, 0).into(),
            };
        }
        let mut card = Column::new().spacing(4);
        if let Some(name) = embed.provider.as_ref().and_then(|p| p.name.as_deref()) {
            card = card.push(label(name).size(11).color(p().muted));
        }
        if let Some(name) = embed.author.as_ref().and_then(|a| a.name.as_deref()) {
            card = card.push(label(name).size(12).font(BOLD));
        }
        if let Some(title) = &embed.title {
            let title = label(title.as_str()).size(14).font(BOLD).color(p().link);
            card = card.push(match embed.url.clone().filter(|u| u.starts_with("http")) {
                Some(url) => Element::from(button(title).padding(0).style(button::text).on_press(Msg::OpenLink(url))),
                None => title.into(),
            });
        }
        if let Some(description) = &bodies.description {
            card = card.push(self.embed_body(description, now));
        }
        for (field, value) in embed.fields.iter().zip(&bodies.fields) {
            card = card.push(label(field.name.as_str()).size(12).font(BOLD));
            card = card.push(self.embed_body(value, now));
        }
        if let Some(preview) = media {
            card = card.push(self.preview_image(preview, original.clone(), original));
        }
        if let Some(footer) = embed.footer.as_ref().filter(|f| !f.text.is_empty()) {
            card = card.push(label(footer.text.as_str()).size(11).color(p().muted));
        }
        let accent = embed.color.map_or(p().selected, |c| rgb((c >> 16) as u8, (c >> 8) as u8, c as u8));
        container(card)
            .padding(10)
            .max_width(520)
            .style(move |_| container::Style {
                background: Some(Background::Color(p().embed_bg)),
                border: Border { color: accent, width: 1.0, radius: 4.0.into() },
                ..container::Style::default()
            })
            .into()
    }

    /// Embed text: markup without message-specific state (spoilers stay hidden).
    fn embed_body<'a>(&'a self, body: &'a Body, now: DateTime<Local>) -> Element<'a, Msg> {
        let mut column = Column::new().spacing(2);
        for block in &body.blocks {
            if let Block::Line { inlines, .. } = block {
                let spans: Vec<Span<'a, Msg>> = inlines
                    .iter()
                    .map(|inline| self.embed_span(inline, now).size(self.settings.text_size() - 1.0))
                    .collect();
                column = column.push(rich_text(spans));
            } else if let Block::Code(code) = block {
                column = column.push(label(code.as_str()).font(Font::MONOSPACE).size(12));
            }
        }
        column.into()
    }

    fn embed_span<'a>(&'a self, inline: &'a Inline, now: DateTime<Local>) -> Span<'a, Msg> {
        let span = match &inline.kind {
            Kind::Text(text) | Kind::Code(text) => span(text.as_str()),
            Kind::Link(url) => span(url.as_str()).color(p().link).underline(true).link(Msg::OpenLink(url.clone())),
            Kind::Emoji { name, .. } => span(format!(":{name}:")),
            Kind::Timestamp { unix, format } => span(discord_timestamp(*unix, *format, now, self.settings.clock_24h)),
            Kind::User(_) | Kind::Role(_) | Kind::Channel(_) | Kind::Everyone(_) => span("@mention").color(p().pill_fg),
        };
        let mut font = Font::DEFAULT;
        if inline.style.bold {
            font.weight = Weight::Bold;
        }
        if inline.style.italic {
            font.style = FontStyle::Italic;
        }
        let span = span.font(font).strikethrough(inline.style.strike);
        if inline.style.spoiler.is_some() {
            span.color(p().spoiler).background(p().spoiler)
        } else {
            span
        }
    }

    fn reactions_view(&self, message: &Message) -> Element<'static, Msg> {
        let mut chips = Row::new().spacing(4);
        for reaction in &message.reactions {
            let emoji = self.reaction_emoji_view(&reaction.emoji);
            let me = reaction.me;
            let chip = button(row![emoji, text(reaction.count.to_string()).size(12)].spacing(4).align_y(Alignment::Center))
                .padding([2, 8])
                .style(move |theme, status| {
                    let mut style = quiet(theme, status);
                    style.background = Some(Background::Color(if me { p().pill_bg } else { p().sidebar }));
                    // Give neutral chips a visible edge so they read as discrete pills.
                    style.border = Border { color: if me { p().accent } else { p().selected }, width: 1.0, radius: 8.0.into() };
                    style.text_color = p().text;
                    style
                })
                .on_press(Msg::ToggleReaction(message.id.clone(), reaction.emoji.clone()));
            chips = chips.push(chip);
        }
        chips.wrap().into()
    }

    fn reaction_emoji_view(&self, emoji: &ReactionEmoji) -> Element<'static, Msg> {
        let image = emoji
            .id
            .as_deref()
            .filter(|_| self.settings.load_custom_emoji)
            .and_then(|id| self.images.get(&self.emoji_image(id, emoji.animated)));
        match (image, &emoji.id) {
            (Some(handle), _) => image_widget(handle.clone(), REACTION_EMOJI_SIZE),
            (None, Some(_)) => label(format!(":{}:", emoji.name.as_deref().unwrap_or("emoji"))).size(12).into(),
            (None, None) => label(emoji.name.clone().unwrap_or_default()).size(14).into(),
        }
    }

    fn reaction_picker<'a>(&'a self, message: &'a Message) -> Element<'a, Msg> {
        let react = |emoji: ReactionEmoji| Msg::ToggleReaction(message.id.clone(), emoji);
        let mut quick = Row::new().spacing(4);
        for emoji in QUICK_REACTIONS {
            let pick = ReactionEmoji { id: None, name: Some(emoji.to_owned()), animated: false };
            quick = quick.push(button(label(emoji).size(16)).padding([2, 6]).style(button::text).on_press(react(pick)));
        }
        let search = text_input("Search emoji…", &self.reaction_query).on_input(Msg::ReactionQuery).padding(6).width(200);
        let mut picker = column![row![quick, search].spacing(8).align_y(Alignment::Center)].spacing(4);
        if self.reaction_query.chars().count() >= 2 {
            let mut results = Row::new().spacing(4);
            for suggestion in markup::suggestions(&self.reaction_query, self.custom_emojis(), SUGGESTION_LIMIT) {
                let msg = react(reaction_emoji(suggestion.pick));
                results = results.push(button(label(suggestion.label).size(13)).padding([2, 6]).style(quiet).on_press(msg));
            }
            picker = picker.push(results.wrap());
        }
        container(picker).padding(6).style(bordered(p().sidebar, 6.0)).into()
    }

    /// "↱ author: text" above a reply.
    fn sticker_view(&self, sticker: &StickerItem, size: f32) -> Element<'static, Msg> {
        let image = sticker
            .image_url()
            .filter(|_| self.settings.load_custom_emoji)
            .and_then(|url| self.images.get(&url).cloned());
        match image {
            Some(handle) => image_widget(handle, size),
            None => label(format!("[sticker: {}]", sticker.name)).size(12).color(p().muted).into(),
        }
    }

    /// Question, one bar per answer (click to vote), and the vote count.
    fn poll_view<'a>(&'a self, message: &'a Message, poll: &'a Poll, now: DateTime<Local>) -> Element<'a, Msg> {
        let total = poll.total();
        let closed = poll.is_closed();
        let mut card = Column::new().spacing(8);
        card = card.push(label(poll.question.text.as_deref().unwrap_or("Poll")).size(15).font(BOLD));
        for answer in &poll.answers {
            let count = poll.count(answer.answer_id);
            let votes = count.map_or(0, |c| c.count);
            let mine = count.is_some_and(|c| c.me_voted);
            let percent = if total == 0 { 0.0 } else { votes as f32 * 100.0 / total as f32 };
            let emoji = answer.poll_media.emoji.as_ref().and_then(|e| e.name.clone()).map(|e| format!("{e} ")).unwrap_or_default();
            let text_label = format!("{emoji}{}", answer.poll_media.text.as_deref().unwrap_or(""));
            let yours = if mine { "your vote · " } else { "" };
            let line = column![
                row![
                    label(text_label).size(13).font(if mine { BOLD } else { Font::DEFAULT }).width(Length::Fill),
                    text(format!("{yours}{votes} · {percent:.0}%")).size(12).color(p().muted),
                ]
                .align_y(Alignment::Center),
                progress_bar(0.0..=100.0, percent).height(6),
            ]
            .spacing(4);
            let vote = Msg::Vote { message_id: message.id.clone(), answer_id: answer.answer_id };
            // "Mine" gets an accent border over the same soft fill: a clear
            // "you chose this" signal without the salmon-slab fill alone.
            let row_style = move |_: &Theme| container::Style {
                background: Some(Background::Color(if mine { p().pill_bg } else { p().chat })),
                border: Border {
                    color: if mine { p().accent } else { p().selected },
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..container::Style::default()
            };
            card = card.push(
                button(container(line).padding([6, 8]).style(row_style))
                    .padding(0)
                    .style(subtle)
                    .on_press_maybe((!closed).then_some(vote)),
            );
        }
        let plural = if total == 1 { "vote" } else { "votes" };
        let status = match poll.expiry {
            _ if closed => "Poll closed".to_owned(),
            Some(end) => format!("ends {}", relative(end.timestamp() - now.timestamp())),
            None => "open".to_owned(),
        };
        let mode = if poll.allow_multiselect { " · pick any" } else { "" };
        card = card.push(text(format!("{total} {plural} · {status}{mode}")).size(11).color(p().muted));
        container(card).padding(12).max_width(460).style(bordered(p().embed_bg, 8.0)).into()
    }

    /// Buttons, menus and components-v2 layouts. `texts` walks `entry.component_texts`
    /// in the same depth-first order they were parsed.
    fn component_view<'a>(
        &'a self,
        entry: &'a Entry,
        component: &'a Component,
        texts: &mut usize,
        now: DateTime<Local>,
    ) -> Element<'a, Msg> {
        let message = &entry.message;
        match component.kind {
            // Action row
            1 => {
                let mut row = Row::new().spacing(6);
                for child in &component.components {
                    row = row.push(self.component_view(entry, child, texts, now));
                }
                row.wrap().into()
            }
            2 => self.button_component(message, component),
            3 => {
                let choices: Vec<(String, String)> =
                    component.options.iter().map(|o| (o.label.clone(), o.value.clone())).collect();
                let labels: Vec<String> = choices.iter().map(|(l, _)| l.clone()).collect();
                let (message_id, custom_id) = (message.id.clone(), component.custom_id.clone().unwrap_or_default());
                let mut picker = pick_list(labels, None::<String>, move |picked| {
                    let value = choices.iter().find(|(l, _)| *l == picked).map(|(_, v)| v.clone()).unwrap_or_default();
                    Msg::ChooseOption { message_id: message_id.clone(), custom_id: custom_id.clone(), value }
                })
                .text_size(13)
                .width(320);
                if let Some(placeholder) = &component.placeholder {
                    picker = picker.placeholder(placeholder.clone());
                }
                picker.into()
            }
            5..=8 => {
                let name = component.placeholder.as_deref().unwrap_or("Menu");
                text(format!("{name} (this kind of menu isn't supported yet)")).size(12).color(p().muted).into()
            }
            // Section: text with a button or thumbnail beside it.
            9 => {
                let mut texts_column = Column::new().spacing(4).width(Length::Fill);
                for child in &component.components {
                    texts_column = texts_column.push(self.component_view(entry, child, texts, now));
                }
                let mut line = row![texts_column].spacing(10).align_y(Alignment::Center);
                if let Some(accessory) = &component.accessory {
                    line = line.push(self.component_view(entry, accessory, texts, now));
                }
                line.into()
            }
            10 => {
                let index = *texts;
                *texts += 1;
                match entry.component_texts.get(index) {
                    Some(body) => self.body_view(entry, body, now),
                    None => Space::new(0, 0).into(),
                }
            }
            11 => match component.media.as_ref() {
                Some(media) => self.unfurled(media, 80),
                None => Space::new(0, 0).into(),
            },
            12 => {
                let mut gallery = Row::new().spacing(6);
                for item in &component.items {
                    gallery = gallery.push(self.unfurled(&item.media, 400));
                }
                gallery.wrap().into()
            }
            14 => match component.divider {
                Some(false) => Space::new(0, 6).into(),
                _ => container(Space::new(Length::Fill, 1)).style(fill(p().selected, 0.0)).into(),
            },
            // Container: a card with an accent edge.
            17 => {
                let mut inner = Column::new().spacing(8);
                for child in &component.components {
                    inner = inner.push(self.component_view(entry, child, texts, now));
                }
                let accent = component.accent_color.map_or(p().selected, |c| rgb((c >> 16) as u8, (c >> 8) as u8, c as u8));
                container(inner)
                    .padding(12)
                    .max_width(560)
                    .style(move |_| container::Style {
                        background: Some(Background::Color(p().embed_bg)),
                        border: Border { color: accent, width: 1.0, radius: 6.0.into() },
                        ..container::Style::default()
                    })
                    .into()
            }
            _ => Space::new(0, 0).into(),
        }
    }

    fn button_component(&self, message: &Message, component: &Component) -> Element<'static, Msg> {
        let emoji = component.emoji.as_ref().and_then(|e| e.name.clone()).filter(|_| {
            component.emoji.as_ref().is_some_and(|e| e.id.is_none()) // custom emoji show as nothing
        });
        let link = component.style == Some(5);
        let name = match (emoji, component.label.as_deref()) {
            (Some(emoji), Some(label)) => format!("{emoji} {label}"),
            (Some(emoji), None) => emoji,
            (None, Some(label)) => label.to_owned(),
            (None, None) => "Button".to_owned(),
        };
        let name = if link { format!("{name} ↗") } else { name };
        let press = match (link, &component.url, &component.custom_id) {
            _ if component.disabled || component.style == Some(6) => None,
            (true, Some(url), _) => Some(Msg::OpenLink(url.clone())),
            (false, _, Some(custom_id)) => Some(Msg::PressButton { message_id: message.id.clone(), custom_id: custom_id.clone() }),
            _ => None,
        };
        let style = match component.style {
            Some(1) => button::primary,
            Some(3) => button::success,
            Some(4) => button::danger,
            _ => quiet,
        };
        button(label(name).size(13)).padding([5, 12]).style(style).on_press_maybe(press).into()
    }

    /// A picture from a components layout, sized to fit `max` px wide.
    fn unfurled(&self, media: &UnfurledMedia, max: u32) -> Element<'static, Msg> {
        let original = media.url.clone().unwrap_or_default();
        let preview = media
            .proxy_url
            .as_deref()
            .or(media.url.as_deref())
            .zip(media.width.zip(media.height))
            .and_then(|(url, (w, h))| images::preview(url, w, h))
            .filter(|_| self.settings.load_images);
        match preview {
            Some(mut preview) => {
                let scale = (max as f32 / preview.width).min(1.0);
                preview.width *= scale;
                preview.height *= scale;
                self.preview_image(preview, original.clone(), original)
            }
            None if original.starts_with("https://") => link_text("[image]".to_owned(), original),
            None => Space::new(0, 0).into(),
        }
    }

    /// While typing `/name`: matching commands. After it: what the command takes.
    fn command_hint(&self) -> Element<'_, Msg> {
        if !self.composer.starts_with('/') {
            return Space::new(0, 0).into();
        }
        let note = |message: String| -> Element<'_, Msg> {
            container(text(message).size(12).color(p().muted)).padding([4, 8]).into()
        };
        match self.commands_state() {
            Some(Ok(())) if commands::typed_name(&self.composer).is_some() => return note("Loading commands…".to_owned()),
            Some(Err(error)) => return note(format!("Commands unavailable: {error}")),
            _ => {}
        }
        let Some(list) = self.channel_commands() else { return Space::new(0, 0).into() };
        let mut hint = Column::new().spacing(2).padding(6);
        if commands::typed_name(&self.composer).is_some() {
            let found = commands::suggestions(&self.composer, list, SUGGESTION_LIMIT);
            if found.is_empty() {
                return Space::new(0, 0).into();
            }
            for command in found {
                let line = row![
                    label(format!("/{}", command.name)).size(14).font(BOLD),
                    label(notify::truncate(&command.description, 80)).size(12).color(p().muted).width(Length::Fill),
                    label(command.app_name.as_str()).size(11).color(p().muted),
                ]
                .spacing(10)
                .align_y(Alignment::Center);
                hint = hint.push(
                    button(line).width(Length::Fill).padding([4, 8]).style(subtle).on_press(Msg::InsertCommand(command.name.clone())),
                );
            }
        } else if let Some((command, options)) = commands::current(&self.composer, list) {
            hint = hint.push(
                row![label(format!("/{}", command.name)).size(13).font(BOLD), label(command.description.as_str()).size(12).color(p().muted)]
                    .spacing(8),
            );
            let mut chips = Row::new().spacing(6);
            for option in options {
                let name = match option.kind {
                    1 | 2 => option.name.clone(),
                    _ if option.required => format!("{}:", option.name),
                    _ => format!("[{}:]", option.name),
                };
                chips = chips.push(tooltip(
                    container(label(name).size(12).font(Font::MONOSPACE)).padding([2, 8]).style(fill(p().chat, 6.0)),
                    tip(option.description.as_str()),
                    tooltip::Position::Top,
                ));
            }
            hint = hint.push(chips.wrap());
            hint = hint.push(text("Enter runs it. Write option:value; quote values with spaces.").size(11).color(p().muted));
        } else {
            return Space::new(0, 0).into();
        }
        container(hint).width(Length::Fill).style(fill(p().sidebar, 6.0)).into()
    }

    fn poll_editor(&self) -> Element<'_, Msg> {
        let Some(draft) = &self.poll_draft else { return Space::new(0, 0).into() };
        let mut form = column![
            text_input("Question", &draft.question).on_input(|t| Msg::EditPoll(PollEdit::Question(t))).padding(8),
        ]
        .spacing(6);
        for (index, answer) in draft.answers.iter().enumerate() {
            form = form.push(
                row![
                    text_input(&format!("Answer {}", index + 1), answer)
                        .on_input(move |t| Msg::EditPoll(PollEdit::Answer(index, t)))
                        .padding(6),
                    small_button("Remove", false, Msg::EditPoll(PollEdit::RemoveAnswer(index))),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        }
        let lengths: Vec<PollLength> = POLL_HOURS.into_iter().map(PollLength).collect();
        let controls = row![
            small_button("Add answer", false, Msg::EditPoll(PollEdit::AddAnswer)),
            pick_list(lengths, Some(PollLength(draft.hours)), |l| Msg::EditPoll(PollEdit::Hours(l.0))).text_size(12),
            checkbox("Allow several answers", draft.multiselect).on_toggle(|on| Msg::EditPoll(PollEdit::Multiselect(on))).size(16),
            Space::with_width(Length::Fill),
            small_button("Cancel", false, Msg::OpenPollDraft),
            button(text("Post poll").size(12)).padding([4, 12]).on_press(Msg::SendPoll),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        container(form.push(controls)).padding(10).width(Length::Fill).style(fill(p().sidebar, 6.0)).into()
    }

    fn sticker_picker_view(&self) -> Element<'_, Msg> {
        let Some(guild) = self.selected_guild().filter(|_| self.sticker_picker) else { return Space::new(0, 0).into() };
        let mut grid = Row::new().spacing(6);
        for sticker in &guild.stickers {
            let item = sticker.item();
            grid = grid.push(tooltip(
                button(self.sticker_view(&item, STICKER_PICKER_SIZE)).padding(4).style(subtle).on_press(Msg::SendSticker(sticker.id.clone())),
                tip(sticker.name.as_str()),
                tooltip::Position::Top,
            ));
        }
        let list = thin_scroll(grid.wrap()).height(Length::Shrink);
        container(container(list).max_height(220)).padding(8).width(Length::Fill).style(fill(p().sidebar, 6.0)).into()
    }

    /// Search Tenor (through Discord); trending GIFs before you type.
    fn gif_picker_view(&self) -> Element<'_, Msg> {
        let Some(picker) = &self.gif_picker else { return Space::new(0, 0).into() };
        let search = text_input("Search Tenor", &picker.query)
            .id(text_input::Id::new(GIF_INPUT))
            .on_input(Msg::GifQuery)
            .on_submit(Msg::GifSearch)
            .padding(6);
        let results: Element<'_, Msg> = match &picker.results {
            _ if picker.loading => text("Loading GIFs…").size(12).color(p().muted).into(),
            None => Space::new(0, 0).into(),
            Some(Err(error)) => text(format!("GIFs unavailable: {error}")).size(12).color(p().error).into(),
            Some(Ok(gifs)) if gifs.is_empty() => text("No GIFs found.").size(12).color(p().muted).into(),
            Some(Ok(gifs)) => {
                let mut grid = Row::new().spacing(6);
                for gif in gifs {
                    let Some(tile) = self.gif_tile(gif) else { continue };
                    let face: Element<'_, Msg> = match self.images.get(&tile.url) {
                        Some(handle) => image(handle.clone()).width(tile.width).height(tile.height).into(),
                        None => container(Space::new(tile.width, tile.height)).style(fill(p().chat, 6.0)).into(),
                    };
                    grid = grid.push(button(face).padding(0).style(subtle).on_press(Msg::SendGif(gif.url.clone())));
                }
                container(thin_scroll(grid.wrap())).max_height(250).into()
            }
        };
        let note = text("Powered by Tenor. Previews load from media.tenor.com.").size(11).color(p().muted);
        container(column![search, results, note].spacing(8)).padding(8).width(Length::Fill).style(fill(p().sidebar, 6.0)).into()
    }

    /// A forum: start a post, or open one (each post is a thread).
    fn forum_view<'a>(&'a self, forum_id: &'a str) -> Element<'a, Msg> {
        let forum = self.forums.get(forum_id);
        let (title, body, posting) = forum.map_or(("", "", false), |f| (f.draft_title.as_str(), f.draft_body.as_str(), f.posting));
        let new_post = column![
            section_label("New post"),
            text_input("Title", title).on_input(Msg::ForumTitle).padding(8),
            row![
                text_input("Message", body).on_input(Msg::ForumBody).on_submit(Msg::CreatePost).padding(8),
                button(text(if posting { "Posting…" } else { "Post" })).padding([8, 16]).on_press_maybe((!posting).then_some(Msg::CreatePost)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(6);
        let mut list = Column::new().spacing(8);
        match forum {
            Some(f) if f.loading => list = list.push(text("Loading posts…").size(12).color(p().muted)),
            Some(Forum { error: Some(error), .. }) => {
                list = list.push(text(format!("Could not load posts: {error}")).size(12).color(p().error));
            }
            _ => {}
        }
        let posts = self.forum_posts(forum_id);
        if posts.is_empty() && forum.is_some_and(|f| !f.loading) {
            list = list.push(text("No open posts.").size(12).color(p().muted));
        }
        let today = Local::now().date_naive();
        for post in posts {
            let unread = self.shows_unread(&post.id) || self.mention_count(&post.id) > 0;
            let mut meta = Row::new().spacing(10);
            if let Some(count) = post.message_count {
                meta = meta.push(text(format!("{count} messages")).size(11).color(p().muted));
            }
            if let Some(time) = post.last_message_id.as_deref().and_then(snowflake_time) {
                let time = message_time(&time.fixed_offset(), today, self.settings.clock_24h);
                meta = meta.push(text(format!("last activity {time}")).size(11).color(p().muted));
            }
            let mut card = column![label(post.label()).size(15).font(if unread { BOLD } else { Font::DEFAULT }), meta].spacing(4);
            if let Some(first) = forum.and_then(|f| f.first.get(&post.id)) {
                let preview = format!("{}: {}", first.author.display_name(), notify::truncate(&self.plain_text(first), 160));
                card = card.push(label(preview).size(13).color(p().muted));
            }
            list = list.push(
                button(container(card).padding(12).width(Length::Fill).style(fill(p().sidebar, 8.0)))
                    .padding(0)
                    .style(subtle)
                    .on_press(Msg::SelectChannel(post.id.clone())),
            );
        }
        let page = column![new_post, rule(), list].spacing(14).padding([14, 16]);
        thin_scroll(page).height(Length::Fill).into()
    }

    /// Click to jump to the original.
    fn reply_preview(&self, message: &Message) -> Element<'_, Msg> {
        let Some(original) = &message.referenced_message else {
            return label("↱ Original message was deleted").size(12).color(p().muted).into();
        };
        let preview = format!(
            "↱ {}: {}",
            original.author.display_name(),
            notify::truncate(&self.plain_text(original), REPLY_PREVIEW_CHARS)
        );
        let jump = Msg::JumpToMessage { channel_id: original.channel_id.clone(), message_id: original.id.clone() };
        button(label(preview).size(12))
            .padding(0)
            .style(|theme, status| {
                let mut style = button::text(theme, status);
                style.text_color = if matches!(status, button::Status::Hovered) { p().text } else { p().muted };
                style
            })
            .on_press(jump)
            .into()
    }

    fn block_view<'a>(&'a self, entry: &'a Entry, body: &'a Body, block: &'a Block, now: DateTime<Local>) -> Element<'a, Msg> {
        let (kind, inlines) = match block {
            Block::Code(code) => {
                return container(label(code.as_str()).font(Font::MONOSPACE).size(self.settings.text_size() - 2.0))
                    .padding(8)
                    .width(Length::Fill)
                    .style(fill(p().code_bg, 4.0))
                    .into();
            }
            Block::Line { kind, inlines } => (*kind, inlines),
        };
        let base = self.settings.text_size();
        let size = match kind {
            _ if body.jumbo => JUMBO_TEXT_SIZE,
            LineKind::Heading(1) => base + 7.0,
            LineKind::Heading(2) => base + 4.0,
            LineKind::Heading(_) => base + 2.0,
            LineKind::Subtext => base - 3.0,
            LineKind::Plain | LineKind::Quote | LineKind::Bullet => base,
        };
        let prefix: Option<Span<'a, Msg>> = match kind {
            LineKind::Quote => Some(span("▍ ").color(p().muted)),
            LineKind::Bullet => Some(span("  •  ")),
            _ => None,
        };
        let line_style = |span: Span<'a, Msg>| -> Span<'a, Msg> {
            let span = span.size(size);
            match kind {
                LineKind::Heading(_) => span.font(BOLD),
                LineKind::Subtext => span.color(p().muted),
                _ => span,
            }
        };
        if inlines.is_empty() {
            return Space::new(0, size * 0.6).into(); // blank line
        }

        let has_images = inlines.iter().any(|i| matches!(i.kind, Kind::Emoji { .. }));
        if !has_images {
            let spans: Vec<Span<'a, Msg>> = prefix
                .into_iter()
                .chain(inlines.iter().map(|inline| line_style(self.inline_span(entry, inline, now))))
                .collect();
            return rich_text(spans).size(size).into();
        }

        // Custom emoji can't sit inside rich text, so flow words and images in a wrapping row.
        let emoji_size = if body.jumbo { JUMBO_EMOJI_SIZE } else { EMOJI_SIZE };
        let mut flow = Row::new().align_y(Alignment::Center);
        if let Some(prefix) = prefix {
            flow = flow.push(rich_text([prefix]).size(size));
        }
        for inline in inlines {
            match &inline.kind {
                Kind::Emoji { id, name, animated } if !self.hidden(entry, inline.style) => {
                    let cached =
                        self.settings.load_custom_emoji.then(|| self.images.get(&self.emoji_image(id, *animated))).flatten();
                    let piece: Element<'a, Msg> = match cached {
                        Some(handle) => image_widget(handle.clone(), emoji_size),
                        None => rich_text([line_style(span(format!(":{name}:")))]).size(size).into(),
                    };
                    flow = flow.push(container(piece).padding([0, 1]));
                }
                Kind::Text(words) => {
                    for word in words.split_inclusive(' ') {
                        let piece = self.styled(entry, span(word), inline.style, false);
                        flow = flow.push(rich_text([line_style(piece)]).size(size));
                    }
                }
                _ => flow = flow.push(rich_text([line_style(self.inline_span(entry, inline, now))]).size(size)),
            }
        }
        flow.wrap().into()
    }

    fn hidden(&self, entry: &Entry, style: Style) -> bool {
        style.spoiler.is_some_and(|index| !self.is_revealed(&entry.message.id, index))
    }

    fn inline_span<'a>(&'a self, entry: &'a Entry, inline: &'a Inline, now: DateTime<Local>) -> Span<'a, Msg> {
        let (base, code) = match &inline.kind {
            Kind::Text(text) => (span(text.as_str()), false),
            Kind::Code(code) => (span(code.as_str()).background(p().code_bg), true),
            Kind::Link(url) => (span(url.as_str()).color(p().link).underline(true).link(Msg::OpenLink(url.clone())), false),
            Kind::User(id) => (pill(format!("@{}", self.user_name(&entry.message, id))), false),
            Kind::Role(id) => {
                let role = self.role(id);
                let name = role.map_or("unknown-role", |r| r.name.as_str());
                let mut span = pill(format!("@{name}"));
                if let Some(color) = role.map(|r| r.color).filter(|&c| c != 0) {
                    span = span.color(rgb((color >> 16) as u8, (color >> 8) as u8, color as u8));
                }
                (span, false)
            }
            Kind::Channel(id) => (pill(format!("#{}", self.channel_name(id))).link(Msg::JumpToChannel(id.clone())), false),
            Kind::Everyone(mention) => (pill(*mention), false),
            Kind::Emoji { name, .. } => (span(format!(":{name}:")), false),
            Kind::Timestamp { unix, format } => {
                let formatted = discord_timestamp(*unix, *format, now, self.settings.clock_24h);
                (span(formatted).background(p().code_bg), false)
            }
        };
        self.styled(entry, base, inline.style, code)
    }

    /// Applies emphasis and spoiler state to a span.
    fn styled<'a>(&self, entry: &Entry, span: Span<'a, Msg>, style: Style, code: bool) -> Span<'a, Msg> {
        let mut font = if code { Font::MONOSPACE } else { Font::DEFAULT };
        if style.bold {
            font.weight = Weight::Bold;
        }
        if style.italic {
            font.style = FontStyle::Italic;
        }
        let underline = style.underline || span.underline; // links are already underlined
        let span = span.font(font).underline(underline).strikethrough(style.strike);
        match style.spoiler {
            Some(index) if !self.is_revealed(&entry.message.id, index) => span
                .color(p().spoiler)
                .background(p().spoiler)
                .link(Msg::RevealSpoiler(entry.message.id.clone(), index)),
            Some(_) => span.background(p().spoiler_open),
            None => span,
        }
    }
}

/// Text from Discord (names, content). Advanced shaping enables font fallback,
/// which emoji and non-Latin scripts need.
fn label<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content).shaping(Shaping::Advanced)
}

/// A full-width 1 px divider.
fn rule() -> Element<'static, Msg> {
    container(Space::new(Length::Fill, 1)).style(fill(p().rail, 0.0)).into()
}

/// A thin line with a label in the middle: "Today", "New messages".
fn separator(label: String, color: Color) -> Element<'static, Msg> {
    // The flanking rule is dim enough that the label doesn't need BOLD to lead.
    let line = || container(Space::new(Length::Fill, 1)).style(fill(Color { a: 0.35, ..color }, 0.0));
    row![line(), text(label).size(12).color(color), line()]
        .spacing(10)
        .padding([6, 0])
        .align_y(Alignment::Center)
        .into()
}

fn day_label(day: NaiveDate, today: NaiveDate) -> String {
    if day == today {
        "Today".to_owned()
    } else if today.pred_opt() == Some(day) {
        "Yesterday".to_owned()
    } else {
        day.format("%A, %-d %B %Y").to_string()
    }
}

fn image_widget(handle: image::Handle, size: f32) -> Element<'static, Msg> {
    image(handle).width(size).height(size).into()
}

/// A muted, clickable line that opens `url` (Discord CDN links are https).
fn link_text(content: String, url: String) -> Element<'static, Msg> {
    button(label(content).size(13).color(p().link)).padding(0).style(button::text).on_press(Msg::OpenLink(url)).into()
}

fn pill<'a>(label: impl text::IntoFragment<'a>) -> Span<'a, Msg> {
    span(label).color(p().pill_fg).background(p().pill_bg)
}

/// Poll length choices, in hours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PollLength(u32);

impl std::fmt::Display for PollLength {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            1 => f.write_str("1 hour"),
            h if h % 168 == 0 => write!(f, "{} week{}", h / 168, if h == 168 { "" } else { "s" }),
            h if h % 24 == 0 => write!(f, "{} day{}", h / 24, if h == 24 { "" } else { "s" }),
            h => write!(f, "{h} hours"),
        }
    }
}

/// Sticker / GIF / Poll beside the message box. Off-state is quiet so the three
/// tools don't compete with the text field; on-state is an accent-outlined pill.
fn composer_tool(name: &str, on: bool, msg: Msg) -> Element<'_, Msg> {
    button(container(text(name).size(12)).center_y(Length::Fill))
        .height(44)
        .padding([0, 10])
        .style(move |theme, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let mut style = button::text(theme, status);
            style.text_color = if on || hovered { p().text } else { p().muted };
            style.background = if on {
                Some(Background::Color(p().pill_bg))
            } else if hovered {
                Some(Background::Color(p().selected))
            } else {
                None
            };
            style.border = Border {
                color: if on { p().accent } else { Color::TRANSPARENT },
                width: if on { 1.0 } else { 0.0 },
                radius: 6.0.into(),
            };
            style
        })
        .on_press(msg)
        .into()
}

/// The channel header's notification picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelChoice {
    Level(NotifyLevel),
    Muted,
}

impl std::fmt::Display for ChannelChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelChoice::Level(level) => level.fmt(f),
            ChannelChoice::Muted => f.write_str("Muted"),
        }
    }
}

enum RailEntry<'a> {
    Guild(&'a Guild),
    Folder(&'a GuildFolder, Vec<&'a Guild>),
}

enum RailContent {
    Image(image::Handle),
    /// Shown on a disc when there's no icon.
    Initials(String),
    /// Anything else (a folder preview) on a disc of this colour.
    Custom(Element<'static, Msg>, Color),
}

struct RailItem {
    content: RailContent,
    /// Full name, in the tooltip.
    name: String,
    active: bool,
    unread: bool,
    mentions: u32,
}

fn rail_item(item: RailItem, msg: Msg) -> Element<'static, Msg> {
    let RailItem { content, name, active, unread, mentions } = item;
    // Icons are already round; everything else sits on a coloured disc.
    let disc: Option<Color> = match &content {
        RailContent::Image(_) => None,
        RailContent::Initials(_) => Some(p().sidebar),
        RailContent::Custom(_, color) => Some(*color),
    };
    let initials = matches!(content, RailContent::Initials(_));
    let content: Element<'static, Msg> = match content {
        RailContent::Image(handle) => image(handle).width(48).height(48).into(),
        RailContent::Initials(text) => container(label(text).size(14).font(BOLD)).center(48).into(),
        RailContent::Custom(element, _) => container(element).center(48).into(),
    };
    let icon = button(content)
        .width(48)
        .height(48)
        .padding(0)
        .style(move |theme, status| {
            let mut style = button::primary(theme, status);
            let hovered = matches!(status, button::Status::Hovered);
            let lit = initials && (active || hovered);
            style.background = disc.map(|color| Background::Color(if lit { p().accent } else { color }));
            style.text_color = if lit { Color::WHITE } else { p().text };
            style.border = Border { radius: (if active || hovered { 16.0 } else { 24.0 }).into(), ..Border::default() };
            style
        })
        .on_press(msg);
    let icon = tooltip(icon, tip(name), tooltip::Position::Right).gap(8);

    let badge: Element<'static, Msg> = if mentions > 0 {
        mention_badge(mentions)
    } else {
        Space::new(0, 0).into()
    };
    let icon = stack![
        icon,
        container(badge)
            .width(48)
            .height(48)
            .align_x(Horizontal::Right)
            .align_y(Vertical::Bottom),
    ];

    // Active: a tall pill in your accent — the one branded moment in the rail.
    // Unread: a small neutral stub, so the two states read apart by colour and size.
    let (pill_height, pill_color) = if active {
        (36.0, p().accent)
    } else if unread {
        (8.0, p().text)
    } else {
        (0.0, p().text)
    };
    let pill = container(Space::new(4, pill_height)).style(fill(pill_color, 2.0));
    row![container(pill).width(4).height(48).align_y(Vertical::Center), icon]
        .spacing(4)
        .align_y(Alignment::Center)
        .into()
}

fn mention_badge(count: u32) -> Element<'static, Msg> {
    let label = if count > 99 { "99+".to_owned() } else { count.to_string() };
    container(text(label).size(11).font(BOLD).color(Color::WHITE))
        .padding([1, 6])
        .style(fill(p().mention, 8.0))
        .into()
}

fn tip<'a>(content: impl text::IntoFragment<'a>) -> Element<'a, Msg> {
    container(label(content).size(13).color(p().text)).padding([4, 8]).style(bordered(p().rail, 6.0)).into()
}

/// Small quiet label above a block of content.
fn section_label(name: &str) -> Element<'_, Msg> {
    text(name).size(11).color(p().muted).into()
}

/// A header-sized toggle button. Off: text-style muted with a subtle hover fill.
/// On: accent-outlined pill. Reads as a utility toggle, not a primary button.
fn small_button(name: &str, on: bool, msg: Msg) -> Element<'_, Msg> {
    button(text(name).size(12))
        .padding([4, 10])
        .style(move |theme, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let mut style = button::text(theme, status);
            style.text_color = if on || hovered { p().text } else { p().muted };
            style.background = if on {
                Some(Background::Color(p().pill_bg))
            } else if hovered {
                Some(Background::Color(p().selected))
            } else {
                None
            };
            style.border = Border {
                color: if on { p().accent } else { Color::TRANSPARENT },
                width: if on { 1.0 } else { 0.0 },
                radius: 6.0.into(),
            };
            style
        })
        .on_press(msg)
        .into()
}

/// A colour `amount` of the way from `from` towards `to`.
fn blend(from: Color, to: Color, amount: f32) -> Color {
    let mix = |a: f32, b: f32| a + (b - a) * amount;
    Color { r: mix(from.r, to.r), g: mix(from.g, to.g), b: mix(from.b, to.b), a: from.a }
}

/// A surface a step up from the chat (the message box), in any theme.
fn raised() -> Color {
    blend(p().chat, p().text, 0.06)
}

/// The everyday button: a themed fill instead of iced's stock grey.
fn quiet(_theme: &Theme, status: button::Status) -> button::Style {
    let base = blend(p().chat, p().text, 0.11);
    let background = match status {
        button::Status::Hovered => blend(base, p().text, 0.08),
        button::Status::Pressed => blend(base, p().text, 0.14),
        _ => base,
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: if matches!(status, button::Status::Disabled) { p().muted } else { p().text },
        border: Border { radius: 4.0.into(), ..Border::default() },
        ..button::Style::default()
    }
}

/// Scrollable with a slim scrollbar that stays out of the way until used.
fn thin_scroll<'a>(content: impl Into<Element<'a, Msg>>) -> Scrollable<'a, Msg> {
    let bar = scrollable::Scrollbar::new().width(6).scroller_width(6).margin(2);
    scrollable(content).direction(scrollable::Direction::Vertical(bar)).style(|theme, status| {
        let mut style = scrollable::default(theme, status);
        let active = matches!(status, scrollable::Status::Hovered { is_vertical_scrollbar_hovered: true, .. } | scrollable::Status::Dragged { .. });
        style.vertical_rail.background = None;
        style.vertical_rail.border = Border::default();
        style.vertical_rail.scroller.color = blend(p().chat, p().text, if active { 0.35 } else { 0.16 });
        style.vertical_rail.scroller.border = Border { radius: 3.0.into(), ..Border::default() };
        style
    })
}

/// A flat button that only shows itself on hover (list rows, menus).
fn subtle(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::text(theme, status);
    style.text_color = p().text;
    if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        style.background = Some(Background::Color(p().selected));
    }
    style.border = Border { radius: 6.0.into(), ..Border::default() };
    style
}

fn bordered(color: Color, radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(color)),
        border: Border { color: p().selected, width: 1.0, radius: radius.into() },
        ..container::Style::default()
    }
}

fn status_name(status: &str) -> &'static str {
    match status {
        "online" => "Online",
        "idle" => "Idle",
        "dnd" => "Do Not Disturb",
        "invisible" => "Invisible",
        _ => "Offline",
    }
}

fn status_color(status: &str) -> Color {
    match status {
        "online" => p().online,
        "idle" => p().idle,
        "dnd" => p().dnd,
        _ => p().muted,
    }
}

fn centered(label: &str) -> Element<'_, Msg> {
    container(text(label).color(p().muted)).center(Length::Fill).into()
}

fn fill(color: Color, radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(color)),
        border: Border { radius: radius.into(), ..Border::default() },
        ..container::Style::default()
    }
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(3)
        .flat_map(char::to_uppercase)
        .collect()
}

fn clock(h24: bool, seconds: bool) -> &'static str {
    match (h24, seconds) {
        (true, false) => "%H:%M",
        (true, true) => "%H:%M:%S",
        (false, false) => "%-I:%M %p",
        (false, true) => "%-I:%M:%S %p",
    }
}

fn message_time(timestamp: &DateTime<FixedOffset>, today: NaiveDate, h24: bool) -> String {
    let local = timestamp.with_timezone(&Local);
    let date = local.date_naive();
    let time = local.format(clock(h24, false));
    if date == today {
        format!("Today at {time}")
    } else if today.pred_opt() == Some(date) {
        format!("Yesterday at {time}")
    } else {
        format!("{} {time}", local.format("%Y-%m-%d"))
    }
}

/// Formats `<t:unix:format>` in local time.
fn discord_timestamp(unix: i64, format: char, now: DateTime<Local>, h24: bool) -> String {
    let Some(time) = DateTime::<Utc>::from_timestamp(unix, 0).map(|t| t.with_timezone(&Local)) else {
        return format!("<t:{unix}>");
    };
    let date = match format {
        't' => return time.format(clock(h24, false)).to_string(),
        'T' => return time.format(clock(h24, true)).to_string(),
        'd' => return time.format("%d/%m/%Y").to_string(),
        'D' => return time.format("%-d %B %Y").to_string(),
        'R' => return relative(time.timestamp() - now.timestamp()),
        'F' => "%A, %-d %B %Y",
        _ => "%-d %B %Y",
    };
    format!("{} {}", time.format(date), time.format(clock(h24, false)))
}

fn relative(seconds: i64) -> String {
    let units = [(31_536_000, "year"), (2_592_000, "month"), (86_400, "day"), (3_600, "hour"), (60, "minute")];
    let magnitude = seconds.abs();
    let (count, unit) = units
        .iter()
        .find(|(size, _)| magnitude >= *size)
        .map_or((magnitude, "second"), |&(size, unit)| (magnitude / size, unit));
    let plural = if count == 1 { "" } else { "s" };
    if seconds >= 0 {
        format!("in {count} {unit}{plural}")
    } else {
        format!("{count} {unit}{plural} ago")
    }
}
