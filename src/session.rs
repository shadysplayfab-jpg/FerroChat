use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::futures::channel::mpsc;
use iced::widget::{scrollable, text_input};
use iced::Task;
use serde_json::json;

use crate::discord::gateway::{Command, Event};
use crate::discord::member_list::{self, MemberList};
use crate::commands;
use crate::discord::model::{
    snowflake, Channel, Command as SlashCommand, Component, CustomEmoji, Embed, Guild, GuildFolder, GuildSettings,
    Message, NotifyLevel, Profile, Reaction, ReactionEmoji, ReadState, Role, TypingStart, User, UserSettings,
    VoiceState,
};
use crate::discord::permissions::{self, MANAGE_MESSAGES};
use crate::discord::rest::{Cursor, Gif, Rest, Upload, PAGE_SIZE};
use crate::images::{self, ImageCache, Shape};
use crate::markup::{self, Block, Body, Kind, Pick};
use crate::notify;
use crate::settings::Settings;
use crate::voice::{ConnectInfo, Controls, Encryption, VoiceEvent};
use crate::Msg;

mod social;
pub use social::{invite_code, Dialog, DialogKind, Home, HomeTab, Menu, MenuTarget, SocialMsg};

/// Messages kept per channel from live traffic; older ones are dropped. Every
/// kept message is laid out again on each redraw, so this stays modest.
const LIVE_LIMIT: usize = 120;
/// Upper bound when the user explicitly loads older history.
const HISTORY_LIMIT: usize = 1000;
/// Channels whose messages stay in memory (the most recently opened).
const KEPT_LOGS: usize = 12;
const TYPING_TTL: Duration = Duration::from_secs(10);
const TYPING_RESEND: Duration = Duration::from_secs(8);
pub const SUGGESTION_LIMIT: usize = 8;
/// Images are fetched for this many of the newest messages in the open channel.
const MEDIA_RECENT: usize = 60;
/// Discord's upload limit without Nitro, and files per message.
pub const UPLOAD_LIMIT: u64 = 10 * 1024 * 1024;
const MAX_FILES: usize = 10;
const SWITCHER_LIMIT: usize = 10;
/// Widget ids the session focuses or scrolls.
pub const MESSAGES_ID: &str = "messages";
pub const SWITCHER_INPUT: &str = "switcher";
pub const SEARCH_INPUT: &str = "search";
pub const DIALOG_INPUT: &str = "dialog";
pub const STATUSES: [&str; 4] = ["online", "idle", "dnd", "invisible"];

/// Which history request a load answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadKind {
    Latest,
    Older,
    /// Around a message being jumped to.
    Around,
}

/// The right-hand panel, when it shows something other than members.
pub enum Panel {
    Search(SearchPanel),
    Pins(PinsPanel),
    Profile(ProfilePanel),
}

pub struct SearchPanel {
    pub query: String,
    /// Server name, or the DM's name.
    pub scope: String,
    pub guild_id: Option<String>,
    channel_id: String,
    pub loading: bool,
    pub results: Option<Result<Vec<Message>, String>>,
}

pub struct PinsPanel {
    pub channel_id: String,
    pub result: Option<Result<Vec<Message>, String>>,
}

pub struct ProfilePanel {
    pub user: User,
    /// `None` while loading (and for bot logins, which can't read profiles).
    pub profile: Option<Result<Profile, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchTarget {
    Channel(String),
    Guild(String),
}

pub struct SwitchItem {
    pub target: SwitchTarget,
    pub label: String,
    /// Where it is: the server name, "Server", "Direct message".
    pub context: String,
    pub unread: bool,
    pub mentions: u32,
}

/// A message with its content (and embed text) parsed once.
pub struct Entry {
    pub message: Message,
    pub body: Body,
    /// Per embed: description and field values.
    pub embeds: Vec<EmbedBodies>,
    /// Text displays in the message's components, in drawing (depth-first) order.
    pub component_texts: Vec<Body>,
}

pub struct EmbedBodies {
    pub description: Option<Body>,
    pub fields: Vec<Body>,
}

impl Entry {
    fn new(message: Message) -> Self {
        Self {
            body: markup::parse(&message.content),
            embeds: parse_embeds(&message.embeds),
            component_texts: parse_components(&message.components),
            message,
        }
    }
}

/// Depth-first, matching how the view walks them.
fn parse_components(components: &[Component]) -> Vec<Body> {
    fn walk(component: &Component, out: &mut Vec<Body>) {
        if let Some(content) = component.content.as_deref().filter(|_| component.kind == 10) {
            out.push(markup::parse(content));
        }
        for child in component.children() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    for component in components {
        walk(component, &mut out);
    }
    out
}

/// Poll being written above the message box.
pub struct PollDraft {
    pub question: String,
    pub answers: Vec<String>,
    pub hours: u32,
    pub multiselect: bool,
}

#[derive(Debug, Clone)]
pub enum PollEdit {
    Question(String),
    Answer(usize, String),
    AddAnswer,
    RemoveAnswer(usize),
    Hours(u32),
    Multiselect(bool),
}

pub const POLL_HOURS: [u32; 6] = [1, 4, 8, 24, 72, 168];
const POLL_MAX_ANSWERS: usize = 10;

/// A forum's post list, and the "new post" form.
#[derive(Default)]
pub struct Forum {
    pub loading: bool,
    pub error: Option<String>,
    /// Post (thread) ids, most recently active first. The threads themselves
    /// live in the guild's channels so opening one works like any channel.
    pub posts: Vec<String>,
    /// Opening message per post, when Discord sent it.
    pub first: HashMap<String, Message>,
    pub draft_title: String,
    pub draft_body: String,
    pub posting: bool,
}

/// Slash commands for a server or DM; `None` while loading.
type CommandList = Option<Result<Vec<SlashCommand>, String>>;

fn parse_embeds(embeds: &[Embed]) -> Vec<EmbedBodies> {
    embeds
        .iter()
        .map(|e| EmbedBodies {
            description: e.description.as_deref().map(markup::parse),
            fields: e.fields.iter().map(|f| markup::parse(&f.value)).collect(),
        })
        .collect()
}

pub struct PendingFile {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
}

/// A video picture to draw: a camera, or the watched stream.
pub struct Tile {
    pub image: iced::widget::image::Handle,
}

/// The tile key of the stream being watched.
pub const STREAM_TILE: &str = "stream";

pub fn camera_tile(user_id: &str) -> String {
    format!("camera:{user_id}")
}

/// A Go Live stream being watched: filled in as Discord answers op 20.
pub struct Watch {
    pub stream_key: String,
    /// Whose stream.
    pub user_id: String,
    rtc_server_id: Option<String>,
    dave_channel: Option<String>,
    /// (token, endpoint)
    server: Option<(String, String)>,
    pub status: String,
}

pub struct VoiceCall {
    pub guild_id: String,
    pub channel_id: String,
    session_id: Option<String>,
    /// (token, endpoint)
    server: Option<(String, String)>,
    pub status: String,
    pub connected: bool,
    /// Transport encryption mode, once connected.
    mode: Option<&'static str>,
    pub encryption: Option<Encryption>,
    pub self_mute: bool,
    pub self_deaf: bool,
    /// User ids currently speaking (from the voice server).
    pub speaking: HashSet<String>,
}

impl VoiceCall {
    fn describe(&mut self) {
        let Some(mode) = self.mode else { return };
        let e2ee = match &self.encryption {
            Some(e) if e.version > 0 && e.ready => format!("end-to-end encrypted (DAVE v{})", e.version),
            Some(e) if e.version > 0 => "setting up end-to-end encryption…".to_owned(),
            _ => "transport encryption only".to_owned(),
        };
        self.status = format!("{e2ee}; {mode}");
    }
}

pub struct ChannelLog {
    pub entries: Vec<Entry>,
    limit: usize,
    /// Older messages exist on the server (or were dropped from memory).
    pub has_more: bool,
    pub loading: bool,
    /// Showing a stretch of older history (after a jump), not the latest messages;
    /// new messages aren't appended until "Jump to present".
    pub detached: bool,
}

impl ChannelLog {
    fn new() -> Self {
        Self { entries: Vec::new(), limit: LIVE_LIMIT, has_more: false, loading: true, detached: false }
    }

    pub fn can_load_older(&self) -> bool {
        self.has_more && !self.loading && self.limit < HISTORY_LIMIT
    }

    fn contains(&self, message_id: &str) -> bool {
        self.entries.iter().any(|e| e.message.id == message_id)
    }

    fn push(&mut self, message: Message) {
        if self.detached || self.contains(&message.id) {
            return;
        }
        self.entries.push(Entry::new(message));
        if self.entries.len() > self.limit {
            let excess = self.entries.len() - self.limit;
            self.entries.drain(..excess);
            self.has_more = true;
        }
    }
}

pub struct Typist {
    user_id: String,
    name: String,
    until: Instant,
}

pub struct Editing {
    pub message_id: String,
    /// The composer text before editing started, restored afterwards.
    draft: String,
}

pub struct Session {
    pub token: String,
    /// Demo sessions never connect.
    pub offline: bool,
    rest: Rest,
    is_bot: bool,
    commands: Option<mpsc::Sender<Command>>,
    /// Guilds we asked to receive live events for; resent after every READY.
    subscribed: HashSet<String>,
    pub me: Option<User>,
    pub guilds: Vec<Guild>,
    pub dms: Vec<Channel>,
    /// `None` shows direct messages.
    pub selected_guild: Option<String>,
    pub selected_channel: Option<String>,
    /// Only channels that were opened are cached; gateway messages for others are ignored.
    pub logs: HashMap<String, ChannelLog>,
    /// Channels by when they were last opened, newest first (see [`KEPT_LOGS`]).
    opened: std::collections::VecDeque<String>,
    /// The channel last opened in each server, reopened when you switch back.
    last_channels: HashMap<String, String>,
    /// Channel id → its server's id (`None`: a DM), filled by [`Session::guild_of`].
    channel_guilds: std::cell::RefCell<HashMap<String, Option<String>>>,
    /// Newest known message id per channel.
    latest: HashMap<String, u64>,
    /// Last read message id per channel. Missing means "treat as read".
    read: HashMap<String, u64>,
    mentions: HashMap<String, u32>,
    typing: HashMap<String, Vec<Typist>>,
    last_typing_sent: Option<(String, Instant)>,
    /// (message id, spoiler index) the user clicked open.
    revealed: HashSet<(String, u16)>,
    pub images: ImageCache,
    /// Member sidebar per guild (user sessions).
    pub member_lists: HashMap<String, MemberList>,
    pub show_members: bool,
    /// Message with the reaction picker open, and its search text.
    pub reacting_to: Option<String>,
    pub reaction_query: String,
    pub pending_files: Vec<PendingFile>,
    pub voice: Option<VoiceCall>,
    /// The last-read message id when the open channel was opened with unreads:
    /// messages after it get a "New messages" divider.
    pub unread_from: Option<u64>,
    /// Shared with the voice task; updated by mute/deafen and settings.
    pub voice_controls: Arc<Controls>,
    pub composer: String,
    pub status: String,
    /// Gateway session is READY (and not since disconnected).
    pub online: bool,
    /// Read copy of the app's settings; the app owns, saves and updates them.
    pub settings: Settings,
    /// Message under the mouse; shows its actions.
    pub hovered: Option<String>,
    /// Message id the composer replies to.
    pub replying: Option<String>,
    /// Ping the author of the replied-to message.
    pub reply_ping: bool,
    pub editing: Option<Editing>,
    /// Message whose Delete was clicked once and awaits confirmation.
    pub confirm_delete: Option<String>,
    focused: bool,
    /// Search, pins or a profile, shown instead of the member list.
    pub panel: Option<Panel>,
    /// Quick switcher (Ctrl+K) and its query.
    pub switcher: Option<String>,
    /// Message jumped to (reply, link, search, pin); drawn highlighted.
    pub highlighted: Option<String>,
    /// Your notification settings, by guild id ("" for direct messages).
    guild_settings: HashMap<String, GuildSettings>,
    /// Server folders from your Discord settings; empty shows servers in plain order.
    pub folders: Vec<GuildFolder>,
    pub open_folders: HashSet<String>,
    /// online / idle / dnd / invisible.
    pub presence: String,
    pub custom_status: Option<String>,
    pub status_menu: bool,
    pub custom_status_input: String,
    /// From READY; interactions must name it.
    session_id: String,
    /// Slash commands by guild id (or DM channel id), fetched when you type `/`.
    slash_commands: HashMap<String, CommandList>,
    /// Interaction nonces waiting on an app, so failures can be reported.
    interactions: HashSet<String>,
    pub forums: HashMap<String, Forum>,
    pub poll_draft: Option<PollDraft>,
    pub sticker_picker: bool,
    /// Latest picture per camera (`camera:<user id>`) and of the watched stream.
    pub tiles: HashMap<String, Tile>,
    /// The tile shown large.
    pub focused_tile: Option<String>,
    /// The Go Live stream we're watching.
    pub watching: Option<Watch>,
    /// Our own Go Live stream (screen share), while it's on.
    pub sharing: Option<Watch>,
    pub gif_picker: Option<GifPicker>,
    /// Animation clock origin.
    anim_start: Instant,
    /// Friends, blocks and requests, and friends' statuses.
    pub relationships: Vec<crate::discord::model::Relationship>,
    pub presences: HashMap<String, String>,
    /// The Home page (Direct Messages with no channel open).
    pub home: Home,
    pub menu: Option<Menu>,
    pub dialog: Option<Dialog>,
    /// Where the last right-click was, and the window size then.
    pointer: iced::Point,
    pub window: iced::Size,
    /// Collapsed category ids (this run only).
    pub collapsed: HashSet<String>,
    /// A server joined or created, opened once its GUILD_CREATE arrives.
    pending_guild: Option<String>,
    playing: Option<crate::voice::player::Playback>,
    /// Audio being downloaded to play, by URL.
    loading_audio: Option<String>,
    /// The start-up screen, until Discord has answered and server icons are in.
    pub loading: Option<Loading>,
}

/// What the start-up screen shows. Each step is real: the gateway's READY,
/// then the server icons downloading.
pub struct Loading {
    pub started: Instant,
    /// When READY arrived.
    pub ready_at: Option<Instant>,
}

/// After READY, the screen waits at most this long for server icons.
const ICON_WAIT: Duration = Duration::from_secs(4);

/// Shown on the start-up screen, a new one every few seconds.
pub const FUN_FACTS: [&str; 14] = [
    "\"Ferro\" comes from ferrum, Latin for iron. Fe is iron's symbol on the periodic table.",
    "The Rust programming language is named after rust fungi, a famously tough family of plant parasites.",
    "Ferris the crab is the Rust community's unofficial mascot.",
    "The official Discord app is a web page running in its own copy of Chromium. Ferrochat draws every pixel itself.",
    "Every Discord ID (a \"snowflake\") has the moment it was created built into it.",
    "Discord's clock for those IDs starts on 1 January 2015, the year Discord launched.",
    "Discord voice uses the Opus audio codec, standardised in 2012.",
    "Discord encrypts voice and video calls end to end with a protocol called DAVE.",
    "Press Ctrl+K in Ferrochat to jump to any server, channel or DM.",
    "Paste a screenshot with Ctrl+V and Ferrochat attaches it to your message.",
    "Iron is the most common element on Earth by mass. Most of it is in the core.",
    "Settings → Performance can switch off images, voice and video to save even more memory.",
    "Your token never touches a file: Ferrochat keeps it in Windows Credential Manager.",
    "Ferrochat was written by Claude, an AI coding agent.",
];

impl Session {
    /// (step done, total) for the server icons after READY; `None` when icons are off.
    pub fn icons_progress(&self) -> Option<(usize, usize)> {
        if !self.settings.load_avatars {
            return None;
        }
        let urls: Vec<String> = self.guilds.iter().filter_map(|g| images::guild_icon_url(&g.id, g.icon.as_deref()?)).collect();
        Some((urls.iter().filter(|url| self.images.is_settled(url)).count(), urls.len()))
    }

    /// The start-up screen can go: READY is in, and the icons are too (or took too long).
    fn loading_finished(&self) -> bool {
        let Some(ready_at) = self.loading.as_ref().and_then(|l| l.ready_at) else { return false };
        let icons_in = self.icons_progress().is_none_or(|(done, total)| done >= total);
        icons_in || ready_at.elapsed() >= ICON_WAIT
    }
}

impl Session {
    pub fn new(token: String, settings: Settings) -> Self {
        let voice_controls = Arc::new(Controls::default());
        apply_voice_settings(&voice_controls, &settings);
        Self {
            voice_controls,
            images: ImageCache::new(settings.image_cache_bytes()),
            member_lists: HashMap::new(),
            show_members: settings.member_list,
            reacting_to: None,
            reaction_query: String::new(),
            pending_files: Vec::new(),
            voice: None,
            unread_from: None,
            settings,
            hovered: None,
            replying: None,
            reply_ping: true,
            editing: None,
            confirm_delete: None,
            focused: true,
            panel: None,
            switcher: None,
            highlighted: None,
            guild_settings: HashMap::new(),
            folders: Vec::new(),
            open_folders: HashSet::new(),
            presence: "online".to_owned(),
            custom_status: None,
            status_menu: false,
            custom_status_input: String::new(),
            session_id: String::new(),
            slash_commands: HashMap::new(),
            interactions: HashSet::new(),
            forums: HashMap::new(),
            poll_draft: None,
            sticker_picker: false,
            tiles: HashMap::new(),
            focused_tile: None,
            watching: None,
            sharing: None,
            gif_picker: None,
            anim_start: Instant::now(),
            relationships: Vec::new(),
            presences: HashMap::new(),
            home: Home::default(),
            menu: None,
            dialog: None,
            pointer: iced::Point::ORIGIN,
            window: iced::Size::new(1100.0, 720.0),
            collapsed: HashSet::new(),
            pending_guild: None,
            playing: None,
            loading_audio: None,
            rest: Rest::new(&token),
            is_bot: token.starts_with("Bot "),
            token,
            offline: false,
            commands: None,
            subscribed: HashSet::new(),
            me: None,
            guilds: Vec::new(),
            dms: Vec::new(),
            selected_guild: None,
            selected_channel: None,
            logs: HashMap::new(),
            opened: std::collections::VecDeque::new(),
            channel_guilds: std::cell::RefCell::new(HashMap::new()),
            last_channels: HashMap::new(),
            latest: HashMap::new(),
            read: HashMap::new(),
            mentions: HashMap::new(),
            typing: HashMap::new(),
            last_typing_sent: None,
            revealed: HashSet::new(),
            composer: String::new(),
            status: "Connecting…".to_owned(),
            online: false,
            loading: Some(Loading { started: Instant::now(), ready_at: None }),
        }
    }

    /// Offline session with sample messages for checking rendering (`--features demo`).
    #[cfg(feature = "demo")]
    pub fn demo(settings: Settings) -> Self {
        #[derive(serde::Deserialize)]
        struct Demo {
            #[serde(flatten)]
            ready: crate::discord::model::RawReady,
            messages: Vec<Message>,
            member_list: member_list::Update,
        }
        let demo: Demo = serde_json::from_str(include_str!("demo.json")).expect("valid demo.json");
        let mut session = Self::new("demo".to_owned(), settings);
        session.offline = true;
        let _ = session.on_gateway(Event::Ready(demo.ready.into()));
        // FERROCHAT_DEMO_LOADING keeps the start-up screen, waiting for Discord.
        session.loading = std::env::var_os("FERROCHAT_DEMO_LOADING").map(|_| Loading { started: Instant::now(), ready_at: None });
        session.selected_guild = Some("100".to_owned());
        session.selected_channel = Some("200".to_owned());
        let log = session.logs.entry("200".to_owned()).or_insert_with(ChannelLog::new);
        log.loading = false;
        // FERROCHAT_DEMO_SKIP=314,315 leaves messages out (for measuring what each costs).
        let skip = std::env::var("FERROCHAT_DEMO_SKIP").unwrap_or_default();
        let skip: Vec<&str> = skip.split(',').collect();
        log.entries = demo.messages.into_iter().filter(|m| !skip.contains(&m.id.as_str())).map(Entry::new).collect();
        session.member_lists.entry("100".to_owned()).or_default().apply(demo.member_list);
        session.reacting_to = std::env::var("FERROCHAT_DEMO_REACT").ok();
        session.unread_from = std::env::var("FERROCHAT_DEMO_UNREAD").ok().and_then(|id| id.parse().ok());
        if std::env::var_os("FERROCHAT_DEMO_VOICE").is_some() {
            session.voice = Some(VoiceCall {
                guild_id: "100".to_owned(),
                channel_id: "202".to_owned(),
                session_id: None,
                server: None,
                status: "end-to-end encrypted (DAVE v1); aead_aes256_gcm_rtpsize".to_owned(),
                connected: true,
                mode: Some("aead_aes256_gcm_rtpsize"),
                encryption: Some(Encryption { version: 1, ready: true, privacy_code: Some("12345 67890 13579 24680 11223 34455".to_owned()) }),
                self_mute: false,
                self_deaf: false,
                speaking: HashSet::from(["3".to_owned()]),
            });
        }
        // FERROCHAT_DEMO_STAGE: the Lounge's stage, with Sam streaming and a little chat.
        if std::env::var_os("FERROCHAT_DEMO_STAGE").is_some() {
            if let Some(guild) = session.guilds.iter_mut().find(|g| g.id == "100") {
                for state in guild.voice_states.iter_mut().filter(|s| s.user_id == "3") {
                    state.self_stream = true;
                }
                // You, in the call.
                guild.voice_states.push(VoiceState {
                    guild_id: None,
                    channel_id: Some("202".to_owned()),
                    user_id: "1".to_owned(),
                    session_id: "z".to_owned(),
                    self_mute: false,
                    self_deaf: false,
                    mute: false,
                    deaf: false,
                    self_video: false,
                    self_stream: std::env::var_os("FERROCHAT_DEMO_SHARE").is_some(),
                    member: None,
                });
            }
            session.selected_channel = Some("202".to_owned());
            let mut log = ChannelLog::new();
            log.loading = false;
            log.entries = session.logs["200"]
                .entries
                .iter()
                .take(3)
                .map(|e| Entry::new(Message { channel_id: "202".to_owned(), ..e.message.clone() }))
                .collect();
            session.logs.insert("202".to_owned(), log);
        }
        if session.voice.is_some() && std::env::var_os("FERROCHAT_DEMO_SHARE").is_some() {
            session.sharing = Some(Watch {
                stream_key: "guild:100:202:1".to_owned(),
                user_id: "1".to_owned(),
                rtc_server_id: None,
                dave_channel: None,
                server: None,
                status: "Sharing your screen at 1280×720, 15 fps".to_owned(),
            });
            // A stand-in for the preview of your screen (a gradient, 320×180).
            let rgba: Vec<u8> = (0..180u32)
                .flat_map(|y| (0..320u32).flat_map(move |x| [(x * 255 / 320) as u8, 60, (y * 255 / 180) as u8, 255]))
                .collect();
            let frame = crate::voice::video::VideoFrame { width: 320, height: 180, rgba };
            session.show_frame(crate::voice::screen::OWN_PREVIEW.to_owned(), Arc::new(frame));
        }
        session.status = "Demo (offline)".to_owned();
        session.composer = std::env::var("FERROCHAT_DEMO_COMPOSER").unwrap_or_default();
        session.hovered = std::env::var("FERROCHAT_DEMO_HOVER").ok();
        session.replying = std::env::var("FERROCHAT_DEMO_REPLY").ok();
        if let Ok(channel) = std::env::var("FERROCHAT_DEMO_CHANNEL") {
            session.selected_channel = Some(channel.clone());
            let forum = session.forums.entry(channel).or_default();
            let first = |id: &str, author: &str, content: &str| -> Message {
                serde_json::from_value(serde_json::json!({
                    "id": "900", "channel_id": id, "author": { "id": "2", "username": author },
                    "timestamp": "2026-11-05T09:00:00+00:00", "content": content,
                }))
                .expect("valid demo message")
            };
            forum.first.insert("230".into(), first("230", "alex", "The login card is very bright at night; could it follow the theme?"));
            forum.first.insert("231".into(), first("231", "sam", "Would love small Lua or WASM plugins for custom commands."));
        }
        if std::env::var_os("FERROCHAT_DEMO_POLL").is_some() {
            session.poll_draft = Some(PollDraft {
                question: "Movie night?".to_owned(),
                answers: vec!["Friday".to_owned(), "Saturday".to_owned(), String::new()],
                hours: 24,
                multiselect: true,
            });
        }
        session.sticker_picker = std::env::var_os("FERROCHAT_DEMO_STICKERS").is_some();
        // An animated emoji made here (the demo has no network for real ones): a
        // square cycling through colours, used by `<a:spin:424242>` in demo.json.
        {
            use image::{codecs::gif::GifEncoder, Delay, Frame, Rgba, RgbaImage};
            let mut bytes = Vec::new();
            {
                let mut encoder = GifEncoder::new(&mut bytes);
                for step in 0..12u8 {
                    let shade = |offset: u8| (((step + offset) % 12) as f32 / 11.0 * 255.0) as u8;
                    let frame = RgbaImage::from_pixel(48, 48, Rgba([shade(0), shade(4), shade(8), 255]));
                    let _ = encoder.encode_frame(Frame::from_parts(frame, 0, 0, Delay::from_numer_denom_ms(80, 1)));
                }
            }
            let url = session.emoji_image("424242", true);
            let shape = if session.settings.animate_media { Shape::Animated { round: false } } else { Shape::Square };
            session.images.want([(url.clone(), shape)]);
            session.images.loaded(&url, Ok(images::prepare(bytes, shape)));
        }
        if std::env::var_os("FERROCHAT_DEMO_GIFS").is_some() {
            let gif = |n: u32| Gif {
                url: format!("https://tenor.com/view/demo-{n}"),
                src: format!("https://media.tenor.com/demo00000{n}AAAAC/demo.gif"),
                preview: format!("https://media.tenor.com/demo00000{n}AAAAe/demo.png"),
                width: 160 + n * 20,
                height: 120,
            };
            session.gif_picker =
                Some(GifPicker { query: "cats".to_owned(), loading: false, results: Some(Ok((1..=6).map(gif).collect())) });
        }
        if session.composer.starts_with('/') {
            let options: Vec<crate::discord::model::CommandOption> = serde_json::from_value(serde_json::json!([
                { "type": 3, "name": "what", "description": "What to remind you about", "required": true },
                { "type": 4, "name": "minutes", "description": "How long from now" },
            ]))
            .expect("valid demo options");
            let command = |name: &str, description: &str, options: Vec<crate::discord::model::CommandOption>| SlashCommand {
                id: "1".into(),
                application_id: "9".into(),
                version: "1".into(),
                name: name.into(),
                description: description.into(),
                options,
                app_name: "Ferrobot".into(),
                raw: serde_json::Value::Null,
            };
            let list = vec![
                command("remind", "Set a reminder", options),
                command("roll", "Roll some dice", Vec::new()),
                command("role", "Manage your roles", Vec::new()),
            ];
            session.slash_commands.insert("100".into(), Some(Ok(list)));
        }
        session.switcher = std::env::var("FERROCHAT_DEMO_SWITCHER").ok();
        session.highlighted = std::env::var("FERROCHAT_DEMO_HIGHLIGHT").ok();
        session.status_menu = std::env::var_os("FERROCHAT_DEMO_STATUS").is_some();
        if std::env::var_os("FERROCHAT_DEMO_FOLDER").is_some() {
            session.open_folders = session.folders.iter().filter(|f| f.is_folder()).map(GuildFolder::key).collect();
        }
        // Search and pins show the demo channel's messages as results.
        let sample = || session.logs["200"].entries.iter().rev().take(4).map(|e| e.message.clone()).collect::<Vec<_>>();
        session.panel = match std::env::var("FERROCHAT_DEMO_PANEL").as_deref() {
            Ok("search") => Some(Panel::Search(SearchPanel {
                query: "rust".to_owned(),
                scope: "Demo Server".to_owned(),
                guild_id: Some("100".to_owned()),
                channel_id: "200".to_owned(),
                loading: false,
                results: Some(Ok(sample())),
            })),
            Ok("pins") => Some(Panel::Pins(PinsPanel { channel_id: "200".to_owned(), result: Some(Ok(sample())) })),
            Ok("profile") => {
                let user = session.logs["200"].entries[2].message.author.clone();
                let profile = Profile {
                    user_profile: Some(crate::discord::model::ProfileText {
                        bio: Some("Rust, synths and too many side projects.".to_owned()),
                        pronouns: None,
                    }),
                    guild_member: Some(crate::discord::model::ProfileMember {
                        nick: None,
                        roles: vec!["101".to_owned()],
                        joined_at: chrono::DateTime::parse_from_rfc3339("2024-03-02T10:00:00+00:00").ok(),
                    }),
                };
                Some(Panel::Profile(ProfilePanel { user, profile: Some(Ok(profile)) }))
            }
            _ => None,
        };
        session.demo_social();
        session
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Gateway(event) => return self.on_gateway(event),
            Msg::Tick => {
                if self.loading_finished() {
                    self.loading = None;
                }
                let now = Instant::now();
                self.typing.retain(|_, typists| {
                    typists.retain(|t| t.until > now);
                    !typists.is_empty()
                });
            }
            Msg::SelectGuild(id) => {
                self.select_guild(id.clone());
                // Like Discord: back to the channel you last read there, else the first one.
                let Some(guild) = id.and_then(|id| self.guilds.iter().find(|g| g.id == id)) else { return Task::none() };
                let remembered = self.last_channels.get(&guild.id).filter(|c| guild.channels.iter().any(|g| &g.id == *c)).cloned();
                let first = || guild.channels.iter().find(|c| c.is_text() && c.parent_id.is_none()).or_else(|| guild.channels.iter().find(|c| c.is_text())).map(|c| c.id.clone());
                if let Some(channel_id) = remembered.or_else(first) {
                    return self.open(channel_id);
                }
            }
            Msg::SelectChannel(id) => return self.open(id),
            Msg::JumpToChannel(id) => return self.jump_to_channel(id),
            Msg::JumpToMessage { channel_id, message_id } => return self.jump_to_message(channel_id, message_id),
            Msg::JumpToPresent => {
                let Some(channel_id) = self.selected_channel.clone() else { return Task::none() };
                self.highlighted = None;
                self.logs.insert(channel_id.clone(), ChannelLog::new());
                return self.fetch(channel_id, Cursor::Latest, LoadKind::Latest);
            }
            Msg::LoadOlder => return self.load_older(),
            Msg::MessagesLoaded { channel_id, kind, result } => return self.on_loaded(channel_id, kind, result),
            Msg::OpenSwitcher => {
                self.switcher = Some(String::new());
                return text_input::focus(text_input::Id::new(SWITCHER_INPUT));
            }
            Msg::SwitcherQuery(query) => self.switcher = Some(query),
            Msg::SwitcherSubmit => {
                let first = self.switcher_results().into_iter().next().map(|item| item.target);
                if let Some(target) = first {
                    return self.update(Msg::SwitcherPick(target));
                }
            }
            Msg::SwitcherPick(target) => {
                self.switcher = None;
                return match target {
                    SwitchTarget::Channel(id) => self.jump_to_channel(id),
                    SwitchTarget::Guild(id) => {
                        self.select_guild(Some(id));
                        Task::none()
                    }
                };
            }
            Msg::CloseSwitcher => self.switcher = None,
            Msg::OpenSearch => return self.open_search(),
            Msg::SearchQuery(query) => {
                if let Some(Panel::Search(search)) = &mut self.panel {
                    search.query = query;
                }
            }
            Msg::SearchSubmit => {
                let Some(Panel::Search(search)) = &mut self.panel else { return Task::none() };
                let query = search.query.trim().to_owned();
                if query.is_empty() || search.loading {
                    return Task::none();
                }
                search.loading = true;
                let request = self.rest.clone().search(search.guild_id.clone(), search.channel_id.clone(), query);
                return Task::perform(request, Msg::SearchDone);
            }
            Msg::SearchDone(result) => {
                if let Some(Panel::Search(search)) = &mut self.panel {
                    search.loading = false;
                    search.results = Some(result);
                }
            }
            Msg::ShowPins => {
                let Some(channel_id) = self.selected_channel.clone() else { return Task::none() };
                if matches!(&self.panel, Some(Panel::Pins(p)) if p.channel_id == channel_id) {
                    self.panel = None;
                    return Task::none();
                }
                self.panel = Some(Panel::Pins(PinsPanel { channel_id: channel_id.clone(), result: None }));
                let key = channel_id.clone();
                return Task::perform(self.rest.clone().pins(channel_id), move |result| Msg::PinsLoaded {
                    channel_id: key.clone(),
                    result,
                });
            }
            Msg::PinsLoaded { channel_id, result } => {
                if let Some(Panel::Pins(pins)) = &mut self.panel {
                    if pins.channel_id == channel_id {
                        pins.result = Some(result);
                    }
                }
            }
            Msg::ShowProfile(user) => return self.show_profile(user),
            Msg::ProfileLoaded { user_id, result } => {
                if let Some(Panel::Profile(panel)) = &mut self.panel {
                    if panel.user.id == user_id {
                        panel.profile = Some(result);
                    }
                }
            }
            Msg::OpenDm(user_id) => return Task::perform(self.rest.clone().open_dm(user_id), Msg::DmOpened),
            Msg::DmOpened(Ok(channel)) => {
                let id = channel.id.clone();
                if !self.dms.iter().any(|c| c.id == id) {
                    let latest = channel.last_message_id.as_deref().map_or(0, snowflake);
                    self.latest.insert(id.clone(), latest);
                    self.read.insert(id.clone(), latest);
                    self.dms.insert(0, channel);
                }
                self.panel = None;
                self.select_guild(None);
                return self.open(id);
            }
            Msg::DmOpened(Err(error)) => self.status = format!("Could not open a DM: {error}"),
            Msg::ClosePanel => self.panel = None,
            Msg::ToggleFolder(key) => {
                if !self.open_folders.remove(&key) {
                    self.open_folders.insert(key);
                }
            }
            Msg::ToggleStatusMenu => {
                self.status_menu = !self.status_menu;
                self.custom_status_input = self.custom_status.clone().unwrap_or_default();
            }
            Msg::SetStatus(status) => {
                self.presence = status;
                self.status_menu = false;
                return self.set_presence();
            }
            Msg::CustomStatusInput(value) => self.custom_status_input = value,
            Msg::SaveCustomStatus => {
                let text = self.custom_status_input.trim();
                self.custom_status = (!text.is_empty()).then(|| text.chars().take(128).collect());
                self.status_menu = false;
                return self.set_presence();
            }
            Msg::MuteChannel(channel_id, muted) => return self.mute_channel(channel_id, muted),
            Msg::ChannelNotify(channel_id, level) => return self.channel_notify(channel_id, level),
            Msg::MuteGuild(guild_id, muted) => return self.mute_guild(guild_id, muted),
            Msg::GuildNotify(guild_id, level) => return self.guild_notify(guild_id, level),
            Msg::GuildSettingsSaved(Ok(settings)) => {
                self.guild_settings.insert(settings.key(), settings);
            }
            Msg::GuildSettingsSaved(Err(error)) => self.status = format!("Could not save notification settings: {error}"),
            Msg::Vote { message_id, answer_id } => return self.vote(message_id, answer_id),
            Msg::Voted { channel_id, message_id, before, after, result } => match result {
                Ok(()) => {
                    let me = self.me.as_ref().map(|m| m.id.clone()).unwrap_or_default();
                    for answer in before.iter().filter(|a| !after.contains(a)) {
                        self.apply_vote(&channel_id, &message_id, *answer, &me, false);
                    }
                    for answer in after.iter().filter(|a| !before.contains(a)) {
                        self.apply_vote(&channel_id, &message_id, *answer, &me, true);
                    }
                }
                Err(error) => self.status = format!("Vote failed: {error}"),
            },
            Msg::OpenPollDraft => {
                self.sticker_picker = false;
                self.gif_picker = None;
                self.poll_draft = match self.poll_draft {
                    Some(_) => None,
                    None => Some(PollDraft { question: String::new(), answers: vec![String::new(); 2], hours: 24, multiselect: false }),
                };
            }
            Msg::EditPoll(edit) => {
                if let Some(draft) = &mut self.poll_draft {
                    match edit {
                        PollEdit::Question(text) => draft.question = text,
                        PollEdit::Answer(index, text) => {
                            if let Some(answer) = draft.answers.get_mut(index) {
                                *answer = text;
                            }
                        }
                        PollEdit::AddAnswer if draft.answers.len() < POLL_MAX_ANSWERS => draft.answers.push(String::new()),
                        PollEdit::AddAnswer => {}
                        PollEdit::RemoveAnswer(index) if draft.answers.len() > 1 && index < draft.answers.len() => {
                            draft.answers.remove(index);
                        }
                        PollEdit::RemoveAnswer(_) => {}
                        PollEdit::Hours(hours) => draft.hours = hours,
                        PollEdit::Multiselect(on) => draft.multiselect = on,
                    }
                }
            }
            Msg::SendPoll => return self.send_poll(),
            Msg::StreamVoice(event) => self.on_stream_voice(event),
            Msg::WatchStream(user_id) => self.watch_stream(user_id),
            Msg::StopWatching => self.stop_watching(),
            Msg::OpenVoice(channel_id) => return self.open_voice(channel_id),
            Msg::ShareScreen => self.share_screen(),
            Msg::StopSharing => self.stop_sharing(),
            Msg::ShareVoice(event) => self.on_share_voice(event),
            Msg::FocusTile(key) => {
                self.focused_tile = key;
                // A tile shown large gets full-size pictures; the grid gets smaller ones.
                self.voice_controls.set_video_large(self.focused_tile.is_some());
            }
            Msg::AnimationTick => {
                let elapsed = self.anim_start.elapsed().as_millis() as u64;
                self.images.set_clock(elapsed);
            }
            Msg::ToggleGifPicker => {
                self.poll_draft = None;
                self.sticker_picker = false;
                if self.gif_picker.take().is_some() {
                    return Task::none();
                }
                self.gif_picker = Some(GifPicker { query: String::new(), loading: false, results: None });
                return Task::batch([text_input::focus(text_input::Id::new(GIF_INPUT)), self.update(Msg::GifSearch)]);
            }
            Msg::GifQuery(query) => {
                if let Some(picker) = &mut self.gif_picker {
                    picker.query = query;
                }
            }
            Msg::GifSearch => {
                let Some(picker) = &mut self.gif_picker else { return Task::none() };
                if self.offline || self.is_bot {
                    picker.results = Some(Err("GIF search needs a user login.".to_owned()));
                    return Task::none();
                }
                picker.loading = true;
                let query = picker.query.clone();
                let key = query.clone();
                return Task::perform(self.rest.clone().gifs(query), move |result| Msg::GifsLoaded {
                    query: key.clone(),
                    result,
                });
            }
            Msg::GifsLoaded { query, result } => {
                // A slower answer to an older query is dropped.
                let Some(picker) = self.gif_picker.as_mut().filter(|p| p.query == query) else { return Task::none() };
                picker.loading = false;
                picker.results = Some(result);
                return self.fetch_media();
            }
            Msg::SendGif(url) => {
                self.gif_picker = None;
                let Some(channel_id) = self.selected_channel.clone() else { return Task::none() };
                let reply = self.replying.take().map(|id| (id, self.reply_ping));
                return Task::perform(self.rest.clone().send(channel_id, url, reply, Vec::new()), Msg::Sent);
            }
            Msg::ToggleStickerPicker => {
                self.gif_picker = None;
                self.poll_draft = None;
                self.sticker_picker = !self.sticker_picker;
                return self.fetch_media();
            }
            Msg::SendSticker(sticker_id) => {
                self.sticker_picker = false;
                let Some(channel_id) = self.selected_channel.clone() else { return Task::none() };
                let mut body = json!({ "sticker_ids": [sticker_id] });
                if let Some(reply) = self.replying.take() {
                    body["message_reference"] = json!({ "message_id": reply, "fail_if_not_exists": false });
                }
                return Task::perform(self.rest.clone().send_json(channel_id, body), Msg::Sent);
            }
            Msg::PressButton { message_id, custom_id } => {
                return self.interact_component(&message_id, json!({ "component_type": 2, "custom_id": custom_id }));
            }
            Msg::ChooseOption { message_id, custom_id, value } => {
                let data = json!({ "component_type": 3, "custom_id": custom_id, "type": 3, "values": [value] });
                return self.interact_component(&message_id, data);
            }
            Msg::Interacted { nonce, result } => {
                if let Err(error) = result {
                    self.interactions.remove(&nonce);
                    self.status = format!("The app didn't accept that: {error}");
                }
            }
            Msg::CommandsLoaded { key, result } => {
                self.slash_commands.insert(key, Some(result));
            }
            Msg::ForumLoaded { forum_id, result } => {
                let posts = match result {
                    Ok(posts) => posts,
                    Err(error) => {
                        let forum = self.forums.entry(forum_id).or_default();
                        forum.loading = false;
                        forum.error = Some(error);
                        return Task::none();
                    }
                };
                let ids: Vec<String> = posts.threads.iter().map(|t| t.id.clone()).collect();
                for thread in posts.threads {
                    self.upsert_thread(thread);
                }
                let forum = self.forums.entry(forum_id).or_default();
                forum.loading = false;
                forum.error = None;
                forum.posts = ids;
                forum.first = posts.first_messages.into_iter().map(|m| (m.channel_id.clone(), m)).collect();
                return self.fetch_media();
            }
            Msg::ForumTitle(title) => {
                if let Some(forum) = self.selected_channel.as_ref().and_then(|id| self.forums.get_mut(id)) {
                    forum.draft_title = title;
                }
            }
            Msg::ForumBody(body) => {
                if let Some(forum) = self.selected_channel.as_ref().and_then(|id| self.forums.get_mut(id)) {
                    forum.draft_body = body;
                }
            }
            Msg::CreatePost => {
                let Some(forum_id) = self.selected_channel.clone() else { return Task::none() };
                let Some(forum) = self.forums.get_mut(&forum_id) else { return Task::none() };
                let (title, body) = (forum.draft_title.trim().to_owned(), forum.draft_body.trim().to_owned());
                if title.is_empty() || body.is_empty() || forum.posting {
                    self.status = "A post needs a title and a message.".to_owned();
                    return Task::none();
                }
                forum.posting = true;
                return Task::perform(self.rest.clone().create_post(forum_id.clone(), title, body), move |result| {
                    Msg::PostCreated { forum_id: forum_id.clone(), result }
                });
            }
            Msg::PostCreated { forum_id, result } => {
                if let Some(forum) = self.forums.get_mut(&forum_id) {
                    forum.posting = false;
                }
                match result {
                    Ok(thread) => {
                        if let Some(forum) = self.forums.get_mut(&forum_id) {
                            forum.draft_title.clear();
                            forum.draft_body.clear();
                            forum.posts.insert(0, thread.id.clone());
                        }
                        let id = thread.id.clone();
                        self.upsert_thread(thread);
                        return self.open(id);
                    }
                    Err(error) => self.status = format!("Could not create the post: {error}"),
                }
            }
            Msg::ChannelFetched { message_id, result } => match result {
                // A thread we didn't have (archived, or from a link): add it and go.
                Ok(channel) if channel.is_thread() && channel.guild_id.as_ref().is_some_and(|g| self.guilds.iter().any(|x| &x.id == g)) => {
                    let id = channel.id.clone();
                    self.upsert_thread(channel);
                    return match message_id {
                        Some(message_id) => self.jump_to_message(id, message_id),
                        None => self.jump_to_channel(id),
                    };
                }
                _ => self.status = "That channel isn't available to you.".to_owned(),
            },
            Msg::RevealSpoiler(message_id, index) => {
                self.revealed.insert((message_id, index));
            }
            Msg::OpenLink(url) => {
                // Links to Discord messages and channels open here instead of the browser.
                match discord_link(&url) {
                    Some((channel_id, Some(message_id))) => return self.jump_to_message(channel_id, message_id),
                    Some((channel_id, None)) => return self.jump_to_channel(channel_id),
                    None => {}
                }
                // The parser only produces http(s) links; check again before handing to the shell.
                if url.starts_with("https://") || url.starts_with("http://") {
                    if let Err(error) = open::that_detached(&url) {
                        self.status = format!("Could not open link: {error}");
                    }
                }
            }
            Msg::ImageLoaded(url, result) => {
                // Failures fall back to text (emoji) or a file link (attachments).
                let next = self.images.loaded(&url, result);
                return self.download(next);
            }
            Msg::ToggleMembers => {
                // From another panel, "Members" brings the list back rather than hiding it.
                self.show_members = self.panel.take().is_some() || !self.show_members;
                return Task::batch([self.subscribe_members(), self.fetch_media()]);
            }
            Msg::OpenReactionPicker(message_id) => {
                let same = self.reacting_to.as_deref() == Some(message_id.as_str());
                self.reacting_to = (!same).then_some(message_id);
                self.reaction_query.clear();
            }
            Msg::ReactionQuery(query) => self.reaction_query = query,
            Msg::ToggleReaction(message_id, emoji) => {
                self.reacting_to = None;
                return self.toggle_reaction(message_id, emoji);
            }
            Msg::Reacted { channel_id, message_id, emoji, add, result } => match result {
                Ok(()) => {
                    if let Some(me) = self.me.as_ref().map(|m| m.id.clone()) {
                        self.apply_reaction(&channel_id, &message_id, emoji, &me, add);
                    }
                }
                Err(error) => self.status = format!("Reaction failed: {error}"),
            },
            Msg::PickFiles => return Task::perform(pick_files(), Msg::FilesPicked),
            Msg::FilesPicked(paths) => self.add_files(paths),
            Msg::FileDropped(path) => self.add_files(vec![path]),
            // Only while a message box is showing (not in dialogs or the switcher).
            Msg::Paste if self.selected_channel.is_some() && self.dialog.is_none() && self.switcher.is_none() => {
                match crate::clipboard::paste() {
                    Ok(Some(crate::clipboard::Pasted::Files(paths))) => self.add_files(paths),
                    Ok(Some(crate::clipboard::Pasted::Image(path))) => self.add_files(vec![path]),
                    Ok(None) => {} // text: the message box pastes it
                    Err(error) => self.status = format!("Could not paste: {error}"),
                }
            }
            Msg::Paste => {}
            Msg::RemoveFile(index) => {
                if index < self.pending_files.len() {
                    self.pending_files.remove(index);
                }
            }
            Msg::JoinVoice(channel_id) => self.join_voice(channel_id),
            Msg::LeaveVoice => self.leave_voice(),
            Msg::ToggleMute => {
                if let Some(call) = &mut self.voice {
                    call.self_mute = !call.self_mute;
                    self.send_voice_state();
                }
            }
            Msg::ToggleDeafen => {
                if let Some(call) = &mut self.voice {
                    call.self_deaf = !call.self_deaf;
                    // Deafening also mutes, like Discord.
                    call.self_mute |= call.self_deaf;
                    self.send_voice_state();
                }
            }
            Msg::Voice(event) => self.on_voice(event),
            Msg::ComposerChanged(value) => {
                self.composer = value;
                // Commands are fetched the first time `/` is typed in a server or DM.
                let commands = if self.composer.starts_with('/') { self.load_commands() } else { Task::none() };
                return Task::batch([commands, self.send_typing()]);
            }
            Msg::InsertCommand(name) => self.composer = format!("/{name} "),
            Msg::InsertEmoji(insert) => {
                if let Some(query) = markup::shortcode_query(&self.composer) {
                    let start = self.composer.len() - query.len() - 1;
                    self.composer.truncate(start);
                    self.composer.push_str(&insert);
                }
            }
            Msg::Send => return self.send(),
            Msg::Sent(Ok(message)) => return self.on_message(message),
            Msg::Sent(Err(error)) => self.status = format!("Send failed: {error}"),
            Msg::SendFailed { error, text } => {
                // Give the text back rather than losing it (unless something new was typed).
                if self.composer.trim().is_empty() {
                    self.composer = text;
                    self.status = format!("Send failed: {error}. Your message is back in the box.");
                } else {
                    self.status = format!("Send failed: {error}");
                }
            }
            Msg::HoverMessage(id) => {
                if self.confirm_delete.is_some() && self.confirm_delete != id {
                    self.confirm_delete = None;
                }
                self.hovered = id;
            }
            // Only the message being left: moving up, the one above is entered first.
            Msg::LeaveMessage(id) => {
                if self.hovered.as_deref() == Some(id.as_str()) {
                    self.hovered = None;
                }
            }
            Msg::StartReply(message_id) => {
                self.cancel_edit();
                self.replying = Some(message_id);
                self.reply_ping = true;
            }
            Msg::CancelReply => self.replying = None,
            Msg::ToggleReplyPing => self.reply_ping = !self.reply_ping,
            Msg::StartEdit(message_id) => self.start_edit(message_id),
            Msg::EditLast => {
                // Up arrow in another text box (search, a dialog, the switcher) isn't for this.
                let elsewhere = self.dialog.is_some() || self.switcher.is_some() || matches!(self.panel, Some(Panel::Search(_)));
                if self.composer.is_empty() && self.editing.is_none() && !elsewhere {
                    let mine = self.selected_log().and_then(|log| {
                        log.entries.iter().rev().find(|e| self.is_mine(&e.message)).map(|e| e.message.id.clone())
                    });
                    if let Some(message_id) = mine {
                        self.start_edit(message_id);
                    }
                }
            }
            Msg::Social(msg) => return self.social(msg),
            Msg::Escape => {
                if self.menu.is_some() {
                    self.menu = None;
                } else if self.dialog.is_some() {
                    self.dialog = None;
                } else if self.switcher.is_some() {
                    self.switcher = None;
                } else if self.status_menu {
                    self.status_menu = false;
                } else if self.editing.is_some() {
                    self.cancel_edit();
                } else if self.replying.is_some() {
                    self.replying = None;
                } else {
                    self.confirm_delete = None;
                    self.highlighted = None;
                }
            }
            Msg::Edited(Ok(message)) => {
                if let Some(entry) = self.entry_mut(&message.channel_id, &message.id) {
                    *entry = Entry::new(message);
                }
                return self.fetch_media();
            }
            Msg::Edited(Err(error)) => self.status = format!("Edit failed: {error}"),
            Msg::DeleteMessage(message_id) => {
                if self.confirm_delete.as_deref() != Some(message_id.as_str()) {
                    self.confirm_delete = Some(message_id);
                    return Task::none();
                }
                self.confirm_delete = None;
                let Some(channel_id) = self.selected_channel.clone() else { return Task::none() };
                let (channel, message) = (channel_id.clone(), message_id.clone());
                return Task::perform(self.rest.clone().delete(channel_id, message_id), move |result| {
                    Msg::Deleted { channel_id: channel.clone(), message_id: message.clone(), result }
                });
            }
            Msg::Deleted { channel_id, message_id, result } => match result {
                Ok(()) => {
                    if let Some(log) = self.logs.get_mut(&channel_id) {
                        log.entries.retain(|e| e.message.id != message_id);
                    }
                }
                Err(error) => self.status = format!("Delete failed: {error}"),
            },
            Msg::WindowFocused(focused) => self.focused = focused,
            Msg::Notified(Err(error)) => self.status = format!("Notification failed: {error}"),
            Msg::Notified(Ok(()))
            | Msg::Ignored
            | Msg::TokenChanged(_)
            | Msg::Login
            | Msg::LoginEmail(_)
            | Msg::LoginPassword(_)
            | Msg::LoginCode(_)
            | Msg::PasswordLogin
            | Msg::PasswordLoginDone(_)
            | Msg::CaptchaSolved(..)
            | Msg::Logout
            | Msg::SwitchAccount(_)
            | Msg::AddAccount
            | Msg::RemoveAccount(_)
            | Msg::ToggleSettings
            | Msg::Setting(_)
            | Msg::OpenThemesFolder
            | Msg::ReloadThemes
            | Msg::CloseRequested(_)
            | Msg::Tray(_)
            | Msg::AudioDevices(..) => {}
        }
        Task::none()
    }

    /// Applies new settings from the app; frees images for features turned off.
    pub fn settings_changed(&mut self, settings: Settings) -> Task<Msg> {
        let old = std::mem::replace(&mut self.settings, settings);
        let new = &self.settings;
        // Animation changes which URLs (and decodings) images use.
        let media_off = old.animate_media != new.animate_media
            || (old.load_images && !new.load_images)
            || (old.load_avatars && !new.load_avatars)
            || (old.load_custom_emoji && !new.load_custom_emoji);
        if media_off {
            self.images.clear();
        }
        self.images.set_budget(self.settings.image_cache_bytes());
        if !self.settings.member_list {
            self.member_lists.clear();
            self.show_members = false;
        } else if !old.member_list {
            self.show_members = true;
        }
        apply_voice_settings(&self.voice_controls, &self.settings);
        if !self.settings.voice_enabled {
            self.leave_voice();
        }
        if !self.settings.video {
            self.stop_watching();
            self.tiles.clear();
        }
        Task::batch([self.subscribe_members(), self.fetch_media()])
    }

    fn on_gateway(&mut self, event: Event) -> Task<Msg> {
        if matches!(
            event,
            Event::Ready(_)
                | Event::GuildCreate(_)
                | Event::Thread(_)
                | Event::ThreadListSync(_)
                | Event::ChannelUpsert(_)
                | Event::ChannelDelete(_)
                | Event::GuildDelete(_)
        ) {
            self.channels_changed();
        }
        match event {
            Event::Commands(sender) => self.commands = Some(sender),
            Event::Connecting => {
                self.online = false;
                self.status = "Connecting…".to_owned();
            }
            Event::Ready(ready) => {
                self.online = true;
                if let Some(loading) = &mut self.loading {
                    loading.ready_at.get_or_insert_with(Instant::now);
                }
                self.status = format!("Connected as {}", ready.user.display_name());
                self.me = Some(ready.user);
                self.guilds = ready.guilds;
                self.dms = ready.dms;
                self.session_id = ready.session_id;
                self.interactions.clear();
                self.forums.clear();
                self.reset_read_state(&ready.read_states);
                self.guild_settings = ready.guild_settings.into_iter().map(|s| (s.key(), s)).collect();
                self.relationships = ready.relationships;
                self.presences = ready.presences;
                self.apply_user_settings(ready.user_settings);
                // IDENTIFY always says "online"; restore what you chose.
                if self.presence != "online" || self.custom_status.is_some() {
                    self.send_command(Command::Presence {
                        status: self.presence.clone(),
                        custom: self.custom_status.clone(),
                    });
                }
                // Messages and typing may have been missed while disconnected.
                self.logs.clear();
                self.typing.clear();
                self.member_lists.clear();
                // A new gateway session ends any call the old one had.
                if self.voice.take().is_some() {
                    self.status = "Voice call ended (reconnected to Discord).".to_owned();
                }
                for guild_id in self.subscribed.clone() {
                    self.send_command(Command::SubscribeGuild(guild_id));
                }
                // Server icons, your avatar and friends' avatars are needed right away,
                // not only once a channel opens.
                let media = self.fetch_media();
                if let Some(channel) = self.selected_channel.clone() {
                    self.logs.insert(channel.clone(), ChannelLog::new());
                    return Task::batch([media, self.subscribe_members(), self.fetch(channel, Cursor::Latest, LoadKind::Latest)]);
                }
                return media;
            }
            Event::GuildCreate(mut guild) => {
                if let Some(me) = &self.me {
                    guild.finish(&me.id);
                }
                for channel in &guild.channels {
                    let latest = channel.last_message_id.as_deref().map_or(0, snowflake);
                    self.latest.insert(channel.id.clone(), latest);
                    self.read.entry(channel.id.clone()).or_insert(latest);
                }
                let id = guild.id.clone();
                match self.guilds.iter_mut().find(|g| g.id == guild.id) {
                    Some(existing) => *existing = guild,
                    None => self.guilds.push(guild),
                }
                self.guild_arrived(&id);
                // Its icon (and a newly joined server's) for the rail.
                return self.fetch_media();
            }
            event @ (Event::Relationship(_)
            | Event::RelationshipRemove(_)
            | Event::Presence(..)
            | Event::ChannelUpsert(_)
            | Event::ChannelDelete(_)
            | Event::GuildDelete(_)) => return self.on_social_event(event),
            Event::MessageCreate(message) => return self.on_message(message),
            Event::MessageUpdate(update) => {
                let entry = self
                    .logs
                    .get_mut(&update.channel_id)
                    .and_then(|log| log.entries.iter_mut().find(|e| e.message.id == update.id));
                let Some(entry) = entry else { return Task::none() };
                if let Some(content) = update.content {
                    entry.body = markup::parse(&content);
                    entry.message.content = content;
                    entry.message.edited_timestamp = update.edited_timestamp;
                }
                if let Some(embeds) = update.embeds {
                    entry.embeds = parse_embeds(&embeds);
                    entry.message.embeds = embeds;
                }
                if let Some(components) = update.components {
                    entry.component_texts = parse_components(&components);
                    entry.message.components = components;
                }
                if let Some(poll) = update.poll {
                    entry.message.poll = Some(poll);
                }
                return self.fetch_media();
            }
            Event::Thread(thread) => self.upsert_thread(thread),
            Event::ThreadDelete(delete) => {
                for guild in self.guilds.iter_mut().filter(|g| delete.guild_id.as_ref().is_none_or(|id| *id == g.id)) {
                    guild.channels.retain(|c| c.id != delete.id);
                }
            }
            Event::ThreadListSync(sync) => {
                for thread in sync.threads {
                    let thread = Channel { guild_id: Some(sync.guild_id.clone()), ..thread };
                    self.upsert_thread(thread);
                }
            }
            Event::PollVote { vote, add } => {
                self.apply_vote(&vote.channel_id, &vote.message_id, vote.answer_id, &vote.user_id, add);
            }
            Event::Stickers { guild_id, stickers } => {
                if let Some(guild) = self.guilds.iter_mut().find(|g| g.id == guild_id) {
                    guild.stickers = stickers.into_iter().filter(|s| s.available).collect();
                }
            }
            Event::StreamCreate(stream) => {
                if let Some(watch) = self.watching.as_mut().filter(|w| w.stream_key == stream.stream_key) {
                    watch.rtc_server_id = Some(stream.rtc_server_id.clone());
                    watch.dave_channel = stream.rtc_channel_id.clone();
                    watch.status = "Stream found; connecting…".to_owned();
                }
                if let Some(share) = self.sharing.as_mut().filter(|s| s.stream_key == stream.stream_key) {
                    share.rtc_server_id = Some(stream.rtc_server_id);
                    share.dave_channel = stream.rtc_channel_id;
                    share.status = "Stream created; connecting…".to_owned();
                    // New streams start paused until the client says otherwise.
                    let resume = Command::PauseStream { stream_key: stream.stream_key, paused: false };
                    self.send_command(resume);
                }
            }
            Event::StreamServer(server) => {
                if let Some(endpoint) = server.endpoint {
                    for stream in [self.watching.as_mut(), self.sharing.as_mut()].into_iter().flatten() {
                        if stream.stream_key == server.stream_key {
                            stream.server = Some((server.token.clone(), endpoint.clone()));
                        }
                    }
                }
            }
            Event::StreamDelete(key) => {
                if self.watching.as_ref().is_some_and(|w| w.stream_key == key) {
                    self.status = "The stream ended.".to_owned();
                    self.watching = None;
                    self.tiles.remove(STREAM_TILE);
                }
                if self.sharing.as_ref().is_some_and(|s| s.stream_key == key) {
                    self.status = "Your screen share ended.".to_owned();
                    self.sharing = None;
                }
            }
            Event::InteractionFailed(nonce) => {
                if self.interactions.remove(&nonce) {
                    self.status = "The app didn't respond.".to_owned();
                }
            }
            Event::ReactionAdd(r) => self.apply_reaction(&r.channel_id, &r.message_id, r.emoji, &r.user_id, true),
            Event::ReactionRemove(r) => self.apply_reaction(&r.channel_id, &r.message_id, r.emoji, &r.user_id, false),
            Event::ReactionClear(clear) => {
                if let Some(entry) = self.entry_mut(&clear.channel_id, &clear.message_id) {
                    match clear.emoji {
                        Some(emoji) => entry.message.reactions.retain(|r| !r.emoji.same(&emoji)),
                        None => entry.message.reactions.clear(),
                    }
                }
            }
            Event::VoiceState(state) => self.on_voice_state(state),
            Event::VoiceServer(server) => {
                if let (Some(call), Some(endpoint)) = (&mut self.voice, server.endpoint) {
                    if server.guild_id.as_deref() == Some(call.guild_id.as_str()) {
                        call.server = Some((server.token, endpoint));
                        call.status = "Voice server assigned…".to_owned();
                    }
                }
            }
            Event::MemberList(update) => {
                // Only the open server's sidebar is shown (late updates from one just left are dropped).
                if !self.settings.member_list || self.selected_guild.as_deref() != Some(update.guild_id.as_str()) {
                    return Task::none();
                }
                self.member_lists.entry(update.guild_id.clone()).or_default().apply(update);
                return self.fetch_media();
            }
            Event::MessageDelete(delete) => {
                if let Some(log) = self.logs.get_mut(&delete.channel_id) {
                    log.entries.retain(|e| e.message.id != delete.id);
                }
            }
            Event::TypingStart(typing) => self.on_typing(typing),
            Event::GuildSettings(settings) => {
                self.guild_settings.insert(settings.key(), settings);
            }
            Event::UserSettings(settings) => self.apply_user_settings(settings),
            Event::Warning(warning) => self.status = warning,
            Event::Disconnected(reason) => {
                self.online = false;
                self.status = format!("Disconnected: {reason}. Retrying…");
            }
            Event::AuthFailed(_) => {} // handled by App
            // Back after a short drop: channels, members and calls carry on.
            Event::Resumed => {
                self.online = true;
                if let Some(me) = &self.me {
                    self.status = format!("Connected as {}", me.display_name());
                }
            }
        }
        Task::none()
    }

    /// Channels without a server read state count as read up to their latest message.
    fn reset_read_state(&mut self, states: &HashMap<String, ReadState>) {
        self.latest.clear();
        self.read.clear();
        self.mentions.clear();
        for channel in self.guilds.iter().flat_map(|g| &g.channels).chain(&self.dms) {
            let latest = channel.last_message_id.as_deref().map_or(0, snowflake);
            self.latest.insert(channel.id.clone(), latest);
            let state = states.get(&channel.id);
            self.read.insert(channel.id.clone(), state.map_or(latest, |s| s.last_message_id));
            if let Some(count) = state.map(|s| s.mention_count).filter(|&n| n > 0) {
                self.mentions.insert(channel.id.clone(), count);
            }
        }
    }

    fn on_message(&mut self, message: Message) -> Task<Msg> {
        let id = snowflake(&message.id);
        let channel = message.channel_id.clone();
        let latest = self.latest.entry(channel.clone()).or_insert(0);
        *latest = (*latest).max(id);

        if let Some(typists) = self.typing.get_mut(&channel) {
            typists.retain(|t| t.user_id != message.author.id);
        }

        let mine = self.me.as_ref().is_some_and(|me| me.id == message.author.id);
        let viewing = self.selected_channel.as_deref() == Some(channel.as_str());
        let mut ack = Task::none();
        if mine || viewing {
            let read = self.read.entry(channel.clone()).or_insert(0);
            *read = (*read).max(id);
            if viewing && !mine && !self.is_bot {
                ack = self.ack(channel.clone(), message.id.clone());
            }
        } else {
            self.read.entry(channel.clone()).or_insert(0);
            if self.mentions_me(&message) {
                *self.mentions.entry(channel.clone()).or_insert(0) += 1;
            }
        }

        if let Some(index) = self.dms.iter().position(|c| c.id == channel) {
            let dm = self.dms.remove(index);
            self.dms.insert(0, dm);
        }
        let notification = if mine { Task::none() } else { self.notify(&message, viewing) };
        if let Some(log) = self.logs.get_mut(&channel) {
            log.push(message);
        }
        let media = if viewing { self.fetch_media() } else { Task::none() };
        Task::batch([ack, media, notification])
    }

    /// A toast as the channel's notification setting asks (by default DMs and
    /// mentions), unless it's muted or you're already looking at it.
    fn notify(&self, message: &Message, viewing: bool) -> Task<Msg> {
        let looking = self.focused && (viewing || !self.settings.notify_when_focused);
        if !self.settings.notifications || looking || self.channel_muted(&message.channel_id) {
            return Task::none();
        }
        let wanted = match self.notify_level(&message.channel_id) {
            NotifyLevel::All => true,
            NotifyLevel::Nothing => false,
            NotifyLevel::Mentions | NotifyLevel::Inherit => self.mentions_me(message),
        };
        if !wanted {
            return Task::none();
        }
        let author = message.author.display_name();
        let title = match self.guild_of(&message.channel_id) {
            Some(guild) => {
                let channel = self.channel(&message.channel_id).map_or_else(String::new, Channel::label);
                format!("{author} (#{channel}, {})", guild.name)
            }
            None => author.to_owned(),
        };
        Task::perform(notify::show(title, self.plain_text(message)), Msg::Notified)
    }

    fn send(&mut self) -> Task<Msg> {
        let content = markup::expand_shortcodes(self.composer.trim(), self.custom_emojis());
        let Some(channel_id) = self.selected_channel.clone() else { return Task::none() };
        if let Some(editing) = self.editing.take() {
            self.composer = editing.draft;
            if content.is_empty() {
                return Task::none(); // an emptied edit just cancels
            }
            return Task::perform(self.rest.clone().edit(channel_id, editing.message_id, content), Msg::Edited);
        }
        if content.is_empty() && self.pending_files.is_empty() {
            return Task::none();
        }
        if !self.can_send(&channel_id) {
            self.status = "You can't talk in this channel: your permissions don't allow it.".to_owned();
            return Task::none();
        }
        // `/name …` for a known command runs it instead of sending text.
        let invocation = self
            .channel_commands()
            .and_then(|list| commands::invocation(self.composer.trim(), list))
            .map(|result| result.map(|(command, options)| (command.clone(), options)));
        match invocation {
            Some(Ok((command, options))) => {
                let task = self.run_command(&channel_id, command, options);
                self.composer.clear();
                return task;
            }
            Some(Err(problem)) => {
                self.status = problem;
                return Task::none();
            }
            None => {}
        }
        // Each send carries its own text, so a failure gives back that one, not a later message's.
        let text = std::mem::take(&mut self.composer);
        self.last_typing_sent = None;
        let reply = self.replying.take().map(|id| (id, self.reply_ping));
        let uploads = std::mem::take(&mut self.pending_files)
            .into_iter()
            .map(|f| Upload { path: f.path, name: f.name })
            .collect();
        Task::perform(self.rest.clone().send(channel_id, content, reply, uploads), move |result| match result {
            Ok(message) => Msg::Sent(Ok(message)),
            Err(error) => Msg::SendFailed { error, text: text.clone() },
        })
    }

    fn join_voice(&mut self, channel_id: String) {
        if !self.settings.voice_enabled {
            return;
        }
        let (Some(me), Some(guild)) = (&self.me, self.guild_of(&channel_id)) else { return };
        let can_connect = guild
            .channels
            .iter()
            .find(|c| c.id == channel_id)
            .and_then(|channel| permissions::channel_permissions(guild, channel, &me.id))
            .is_none_or(|p| p & permissions::CONNECT != 0); // unknown: let Discord decide
        if !can_connect {
            self.status = "You don't have permission to join that voice channel.".to_owned();
            return;
        }
        let guild_id = guild.id.clone();
        let (self_mute, self_deaf) = self.voice.as_ref().map_or((false, false), |c| (c.self_mute, c.self_deaf));
        // Moving gets a fresh session and server, so nothing from the old call is reused.
        self.voice = Some(VoiceCall {
            guild_id,
            channel_id,
            session_id: None,
            server: None,
            status: "Joining…".to_owned(),
            connected: false,
            mode: None,
            encryption: None,
            self_mute,
            self_deaf,
            speaking: HashSet::new(),
        });
        self.send_voice_state();
    }

    fn leave_voice(&mut self) {
        self.stop_watching();
        self.stop_sharing();
        self.tiles.clear();
        self.focused_tile = None;
        if let Some(call) = self.voice.take() {
            let command = Command::VoiceState { guild_id: call.guild_id, channel_id: None, self_mute: false, self_deaf: false };
            self.send_command(command);
        }
    }

    /// Starts watching `user_id`'s Go Live stream in the call we're in.
    fn watch_stream(&mut self, user_id: String) {
        let Some(call) = &self.voice else {
            self.status = "Join the voice channel to watch a stream.".to_owned();
            return;
        };
        if !self.settings.video {
            self.status = "Video is turned off in Settings → Performance.".to_owned();
            return;
        }
        let stream_key = crate::discord::model::stream_key(&call.guild_id, &call.channel_id, &user_id);
        if self.watching.as_ref().is_some_and(|w| w.stream_key == stream_key) {
            return;
        }
        self.stop_watching();
        self.send_command(Command::WatchStream(stream_key.clone()));
        // The stage shows the stream large (its tile key is `stream:<user id>`).
        self.focused_tile = Some(format!("stream:{user_id}"));
        self.voice_controls.set_video_large(true);
        self.watching = Some(Watch {
            stream_key,
            user_id,
            rtc_server_id: None,
            dave_channel: None,
            server: None,
            status: "Asking Discord for the stream…".to_owned(),
        });
    }

    fn stop_watching(&mut self) {
        if let Some(watch) = self.watching.take() {
            self.send_command(Command::StopStream(watch.stream_key));
        }
        self.tiles.remove(STREAM_TILE);
        if self.focused_tile.as_deref().is_some_and(|key| key.starts_with("stream:")) {
            self.focused_tile = None;
            self.voice_controls.set_video_large(false);
        }
    }

    /// Events from the stream connection.
    fn on_stream_voice(&mut self, event: VoiceEvent) {
        let Some(watch) = &mut self.watching else { return };
        match event {
            VoiceEvent::Status(status) => watch.status = status,
            VoiceEvent::Connected { .. } => watch.status = "Connected; waiting for the picture…".to_owned(),
            VoiceEvent::Video { frame, .. } => self.show_frame(STREAM_TILE.to_owned(), frame),
            VoiceEvent::Ended(reason) => {
                self.status = format!("Stream: {reason}");
                self.stop_watching();
            }
            VoiceEvent::Encryption(_) | VoiceEvent::Speaking { .. } | VoiceEvent::VideoState { .. } => {}
        }
    }

    fn show_frame(&mut self, key: String, frame: Arc<crate::voice::video::VideoFrame>) {
        // The UI usually holds the only reference, so the picture moves in
        // without a copy (8 MB a frame at 1080p).
        let (width, height) = (frame.width, frame.height);
        let rgba = Arc::try_unwrap(frame).map_or_else(|shared| shared.rgba.clone(), |frame| frame.rgba);
        let image = iced::widget::image::Handle::from_rgba(width, height, rgba);
        self.tiles.insert(key, Tile { image });
    }

    /// The stream connection, once Discord has sent its server.
    pub fn stream_connect_info(&self) -> Option<ConnectInfo> {
        let call = self.voice.as_ref()?;
        let watch = self.watching.as_ref()?;
        let (token, endpoint) = watch.server.clone()?;
        let server_id = watch.rtc_server_id.clone()?;
        Some(ConnectInfo {
            endpoint,
            channel_id: watch.dave_channel.clone().unwrap_or_else(|| server_id.clone()),
            server_id,
            user_id: self.me.as_ref()?.id.clone(),
            session_id: call.session_id.clone()?,
            token,
            stream: true,
            video: true,
            share: false,
            share_quality: (0, 0, 0),
        })
    }

    /// Starts our own Go Live stream of the screen in the call we're in.
    fn share_screen(&mut self) {
        let (Some(call), Some(me)) = (&self.voice, &self.me) else {
            self.status = "Join a voice channel to share your screen.".to_owned();
            return;
        };
        if self.sharing.is_some() {
            return;
        }
        let stream_key = crate::discord::model::stream_key(&call.guild_id, &call.channel_id, &me.id);
        let command = Command::CreateStream { guild_id: call.guild_id.clone(), channel_id: call.channel_id.clone() };
        self.sharing = Some(Watch {
            stream_key,
            user_id: me.id.clone(),
            rtc_server_id: None,
            dave_channel: None,
            server: None,
            status: "Asking Discord to start your stream…".to_owned(),
        });
        self.send_command(command);
    }

    fn stop_sharing(&mut self) {
        if let Some(share) = self.sharing.take() {
            self.send_command(Command::StopStream(share.stream_key));
        }
        self.tiles.remove(crate::voice::screen::OWN_PREVIEW);
    }

    /// The capture thread sends a preview of your share only while you can see it.
    pub fn sync_own_preview(&self) {
        self.voice_controls.set_own_preview(self.sharing.is_some() && self.in_voice_view());
    }

    /// Events from our own stream's connection.
    fn on_share_voice(&mut self, event: VoiceEvent) {
        let Some(share) = &mut self.sharing else { return };
        match event {
            VoiceEvent::Status(status) => share.status = status,
            VoiceEvent::Connected { .. } => share.status = "Connected; starting capture…".to_owned(),
            VoiceEvent::Video { frame, .. } => self.show_frame(crate::voice::screen::OWN_PREVIEW.to_owned(), frame),
            VoiceEvent::Ended(reason) => {
                self.status = format!("Screen share: {reason}");
                self.stop_sharing();
            }
            _ => {}
        }
    }

    /// Our stream's connection, once Discord has sent its server.
    pub fn share_connect_info(&self) -> Option<ConnectInfo> {
        let call = self.voice.as_ref()?;
        let share = self.sharing.as_ref()?;
        let (token, endpoint) = share.server.clone()?;
        let server_id = share.rtc_server_id.clone()?;
        Some(ConnectInfo {
            endpoint,
            channel_id: share.dave_channel.clone().unwrap_or_else(|| server_id.clone()),
            server_id,
            user_id: self.me.as_ref()?.id.clone(),
            session_id: call.session_id.clone()?,
            token,
            stream: true,
            video: false,
            share: true,
            share_quality: self.settings.share_quality(),
        })
    }

    fn send_voice_state(&mut self) {
        let Some(call) = &self.voice else { return };
        // Locally, stop sending/playing right away; the server learns via op 4.
        self.voice_controls.set_mute(call.self_mute, call.self_deaf);
        let command = Command::VoiceState {
            guild_id: call.guild_id.clone(),
            channel_id: Some(call.channel_id.clone()),
            self_mute: call.self_mute,
            self_deaf: call.self_deaf,
        };
        self.send_command(command);
    }

    fn on_voice_state(&mut self, state: VoiceState) {
        let mine = self.me.as_ref().is_some_and(|me| me.id == state.user_id);
        if mine {
            match (&mut self.voice, &state.channel_id) {
                (Some(call), Some(channel_id)) if *channel_id == call.channel_id => {
                    call.session_id = Some(state.session_id.clone());
                }
                // Moved by someone else: follow, and wait for a new voice server.
                (Some(call), Some(channel_id)) if state.guild_id.as_deref() == Some(call.guild_id.as_str()) => {
                    call.channel_id = channel_id.clone();
                    call.session_id = Some(state.session_id.clone());
                    call.server = None;
                    call.connected = false;
                    call.mode = None;
                    call.encryption = None;
                }
                (Some(_), None) => {
                    self.voice = None;
                    self.status = "Left the voice channel.".to_owned();
                }
                _ => {}
            }
        }
        let Some(guild) = state.guild_id.as_deref().and_then(|id| self.guilds.iter_mut().find(|g| g.id == id)) else {
            return;
        };
        guild.voice_states.retain(|s| s.user_id != state.user_id);
        if state.channel_id.is_some() {
            guild.voice_states.push(state);
        }
    }

    fn on_voice(&mut self, event: VoiceEvent) {
        let Some(call) = &mut self.voice else { return };
        match event {
            VoiceEvent::Status(status) => call.status = status,
            VoiceEvent::Connected { mode } => {
                call.connected = true;
                call.mode = Some(mode);
                call.describe();
            }
            VoiceEvent::Encryption(encryption) => {
                call.encryption = Some(encryption);
                call.describe();
            }
            VoiceEvent::Speaking { user_id, speaking } => {
                if speaking {
                    call.speaking.insert(user_id);
                } else {
                    call.speaking.remove(&user_id);
                }
            }
            VoiceEvent::Video { user_id, frame } => self.show_frame(camera_tile(&user_id), frame),
            VoiceEvent::VideoState { user_id, active } => {
                if !active {
                    let key = camera_tile(&user_id);
                    self.tiles.remove(&key);
                    if self.focused_tile.as_ref() == Some(&key) {
                        self.focused_tile = None;
                    }
                }
            }
            VoiceEvent::Ended(reason) => {
                self.status = format!("Voice: {reason}");
                // Also leave on Discord's side so we don't linger in the channel.
                self.leave_voice();
            }
        }
    }

    /// Connection details once Discord has sent both halves.
    pub fn voice_connect_info(&self) -> Option<ConnectInfo> {
        let call = self.voice.as_ref()?;
        let (token, endpoint) = call.server.clone()?;
        Some(ConnectInfo {
            endpoint,
            server_id: call.guild_id.clone(),
            channel_id: call.channel_id.clone(),
            user_id: self.me.as_ref()?.id.clone(),
            session_id: call.session_id.clone()?,
            token,
            stream: false,
            video: self.settings.video,
            share: false,
            share_quality: (0, 0, 0),
        })
    }

    /// Display names of everyone in a voice channel, with whether they're speaking.
    pub fn voice_members<'a>(&self, guild: &'a Guild, channel_id: &str) -> Vec<(String, bool, &'a VoiceState)> {
        let speaking = self.voice.as_ref().map(|c| &c.speaking);
        guild
            .voice_states
            .iter()
            .filter(|s| s.channel_id.as_deref() == Some(channel_id))
            .map(|state| {
                let name = state
                    .member
                    .as_ref()
                    .map(|m| m.nick.clone().unwrap_or_else(|| m.user.display_name().to_owned()))
                    .or_else(|| self.me.as_ref().filter(|me| me.id == state.user_id).map(|me| me.display_name().to_owned()))
                    .or_else(|| self.known_name(&state.user_id))
                    .unwrap_or_else(|| "Unknown user".to_owned());
                (name, speaking.is_some_and(|s| s.contains(&state.user_id)), state)
            })
            .collect()
    }

    /// The user behind a voice state, for their avatar.
    pub fn voice_user(&self, state: &VoiceState) -> Option<User> {
        state.member.as_ref().map(|m| m.user.clone()).or_else(|| self.me.clone().filter(|me| me.id == state.user_id))
    }

    /// The open channel is a voice channel: show the call stage with its chat beside it.
    pub fn in_voice_view(&self) -> bool {
        self.selected_channel.as_deref().and_then(|id| self.channel(id)).is_some_and(Channel::is_voice)
    }

    /// Clicking a voice channel: join it (unless already in it) and show its stage.
    fn open_voice(&mut self, channel_id: String) -> Task<Msg> {
        if self.voice.as_ref().is_none_or(|call| call.channel_id != channel_id) {
            self.join_voice(channel_id.clone());
        }
        let task = self.open(channel_id);
        Task::batch([task, self.fetch_media()])
    }

    fn add_files(&mut self, paths: Vec<PathBuf>) {
        if self.selected_channel.as_deref().is_some_and(|c| !self.can_send(c) || !self.can_attach(c)) {
            self.status = "You can't attach files in this channel: your permissions don't allow it.".to_owned();
            return;
        }
        for path in paths {
            if self.pending_files.len() >= MAX_FILES {
                self.status = format!("At most {MAX_FILES} files per message.");
                return;
            }
            let name = path.file_name().map_or_else(|| "file".to_owned(), |n| n.to_string_lossy().into_owned());
            match std::fs::metadata(&path) {
                Ok(meta) if !meta.is_file() => self.status = format!("{name} is not a file."),
                Ok(meta) if meta.len() > UPLOAD_LIMIT => {
                    self.status = format!("{name} is over the {} upload limit.", human_size(UPLOAD_LIMIT));
                }
                Ok(meta) => self.pending_files.push(PendingFile { path, name, size: meta.len() }),
                Err(error) => self.status = format!("Could not attach {name}: {error}"),
            }
        }
    }

    fn toggle_reaction(&mut self, message_id: String, emoji: ReactionEmoji) -> Task<Msg> {
        let Some(channel_id) = self.selected_channel.clone() else { return Task::none() };
        let Some(api_name) = emoji.api_name() else { return Task::none() };
        let reacted = self
            .find_entry(&channel_id, &message_id)
            .and_then(|e| e.message.reactions.iter().find(|r| r.emoji.same(&emoji)))
            .is_some_and(|r| r.me);
        let add = !reacted;
        let (channel, message) = (channel_id.clone(), message_id.clone());
        Task::perform(self.rest.clone().react(channel_id, message_id, api_name, add), move |result| Msg::Reacted {
            channel_id: channel.clone(),
            message_id: message.clone(),
            emoji: emoji.clone(),
            add,
            result,
        })
    }

    /// Counts one user's reaction. Our own reactions arrive both as the REST result
    /// and as a gateway event, so applying ours is idempotent.
    fn apply_reaction(&mut self, channel_id: &str, message_id: &str, emoji: ReactionEmoji, user_id: &str, add: bool) {
        let mine = self.me.as_ref().is_some_and(|me| me.id == user_id);
        let Some(entry) = self.entry_mut(channel_id, message_id) else { return };
        let reactions = &mut entry.message.reactions;
        match reactions.iter().position(|r| r.emoji.same(&emoji)) {
            Some(index) => {
                let reaction = &mut reactions[index];
                if mine && reaction.me == add {
                    return;
                }
                if add {
                    reaction.count += 1;
                } else {
                    reaction.count = reaction.count.saturating_sub(1);
                }
                if mine {
                    reaction.me = add;
                }
                if reaction.count == 0 {
                    reactions.remove(index);
                }
            }
            None if add => reactions.push(Reaction { count: 1, me: mine, emoji }),
            None => {}
        }
    }

    fn start_edit(&mut self, message_id: String) {
        let Some(content) = self
            .selected_log()
            .and_then(|log| log.entries.iter().find(|e| e.message.id == message_id))
            .filter(|e| self.is_mine(&e.message))
            .map(|e| e.message.content.clone())
        else {
            return;
        };
        self.cancel_edit();
        self.replying = None;
        let draft = std::mem::replace(&mut self.composer, content);
        self.editing = Some(Editing { message_id, draft });
    }

    fn cancel_edit(&mut self) {
        if let Some(editing) = self.editing.take() {
            self.composer = editing.draft;
        }
    }

    /// Counts toward the mention badge: every DM, plus explicit mentions in servers.
    fn mentions_me(&self, message: &Message) -> bool {
        self.dms.iter().any(|c| c.id == message.channel_id) || self.mentions_me_explicitly(message)
    }

    /// Mentions us by user, role, `@everyone` or `@here`.
    pub fn mentions_me_explicitly(&self, message: &Message) -> bool {
        let Some(me) = &self.me else { return false };
        if message.mention_everyone || message.mentions.iter().any(|u| u.id == me.id) {
            return true;
        }
        let my_roles = message
            .guild_id
            .as_deref()
            .and_then(|id| self.guilds.iter().find(|g| g.id == id))
            .and_then(|g| g.my_roles.as_ref());
        my_roles.is_some_and(|roles| message.mention_roles.iter().any(|r| roles.contains(r)))
    }

    fn on_typing(&mut self, typing: TypingStart) {
        // Only the open channel shows who's typing; tracking every channel kept
        // a once-a-second timer (and redraw) running in busy servers.
        let open = self.selected_channel.as_deref() == Some(typing.channel_id.as_str());
        if !open || self.me.as_ref().is_some_and(|me| me.id == typing.user_id) {
            return;
        }
        let name = typing
            .member
            .map(|m| m.nick.unwrap_or_else(|| m.user.display_name().to_owned()))
            .or_else(|| self.known_name(&typing.user_id))
            .unwrap_or_else(|| "Someone".to_owned());
        let typists = self.typing.entry(typing.channel_id).or_default();
        typists.retain(|t| t.user_id != typing.user_id);
        typists.push(Typist { user_id: typing.user_id, name, until: Instant::now() + TYPING_TTL });
    }

    /// Display name for a user id from what we've seen: DM recipients, message
    /// authors and mentions.
    pub fn known_name(&self, user_id: &str) -> Option<String> {
        let recipients = self.dms.iter().flat_map(|c| &c.recipients);
        let seen = self.logs.values().flat_map(|log| {
            log.entries.iter().flat_map(|e| std::iter::once(&e.message.author).chain(&e.message.mentions))
        });
        recipients
            .chain(seen)
            .find(|u| u.id == user_id)
            .map(|u| u.display_name().to_owned())
    }

    fn select_guild(&mut self, id: Option<String>) {
        // Leaving a server: stop its member sidebar updates and free the list
        // (it syncs again on return). Messages and typing keep coming.
        if let Some(old) = self.selected_guild.clone().filter(|old| id.as_ref() != Some(old)) {
            if !self.is_bot && self.subscribed.contains(&old) {
                self.send_command(Command::SubscribeGuild(old.clone()));
            }
            self.member_lists.remove(&old);
        }
        if let Some(id) = &id {
            self.subscribe(id.clone());
        }
        self.selected_guild = id;
        self.selected_channel = None;
    }

    /// Keeps messages for the channels opened most recently and drops the rest
    /// (they load again when reopened), so a long session doesn't keep growing.
    fn forget_old_logs(&mut self, opening: &str) {
        self.opened.retain(|id| id != opening);
        self.opened.push_front(opening.to_owned());
        while self.opened.len() > KEPT_LOGS {
            let Some(old) = self.opened.pop_back() else { break };
            let in_call = self.voice.as_ref().is_some_and(|call| call.channel_id == old);
            if in_call {
                self.opened.push_front(old);
                break;
            }
            self.logs.remove(&old);
            self.forums.remove(&old);
        }
    }

    fn open(&mut self, channel_id: String) -> Task<Msg> {
        if self.selected_channel.as_deref() != Some(channel_id.as_str()) {
            self.highlighted = None;
            // Pins and DM search belong to the channel being left.
            if matches!(&self.panel, Some(Panel::Pins(_))) || matches!(&self.panel, Some(Panel::Search(s)) if s.guild_id.is_none()) {
                self.panel = None;
            }
        }
        self.selected_channel = Some(channel_id.clone());
        self.reacting_to = None;
        self.poll_draft = None;
        self.sticker_picker = false;
        self.gif_picker = None;
        self.forget_old_logs(&channel_id);
        self.typing.retain(|channel, _| *channel == channel_id);
        if let Some(guild_id) = self.guild_of(&channel_id).map(|g| g.id.clone()) {
            self.last_channels.insert(guild_id, channel_id.clone());
        }
        if self.channel(&channel_id).is_some_and(Channel::is_forum) {
            return Task::batch([self.subscribe_members(), self.load_forum(channel_id)]);
        }
        // Remember where the unread part began, for the "New messages" divider.
        self.unread_from = self.is_unread(&channel_id).then(|| self.read.get(&channel_id).copied()).flatten();
        let ack = self.mark_read(&channel_id);
        let members = self.subscribe_members();
        if self.logs.contains_key(&channel_id) {
            return Task::batch([ack, members, self.fetch_media()]);
        }
        self.logs.insert(channel_id.clone(), ChannelLog::new());
        Task::batch([ack, members, self.fetch(channel_id, Cursor::Latest, LoadKind::Latest)])
    }

    /// Refreshes a forum's post list each time it's opened.
    fn load_forum(&mut self, forum_id: String) -> Task<Msg> {
        let forum = self.forums.entry(forum_id.clone()).or_default();
        if forum.loading || self.offline {
            return Task::none();
        }
        forum.loading = true;
        let key = forum_id.clone();
        Task::perform(self.rest.clone().forum_posts(forum_id), move |result| Msg::ForumLoaded {
            forum_id: key.clone(),
            result,
        })
    }

    /// Posts to show for a forum: the fetched list, else the active threads we know.
    pub fn forum_posts(&self, forum_id: &str) -> Vec<&Channel> {
        let Some(guild) = self.guild_of(forum_id) else { return Vec::new() };
        let fetched = self.forums.get(forum_id).map(|f| f.posts.as_slice()).unwrap_or_default();
        if !fetched.is_empty() {
            return fetched.iter().filter_map(|id| guild.channels.iter().find(|c| &c.id == id)).collect();
        }
        let mut posts: Vec<&Channel> =
            guild.channels.iter().filter(|c| c.is_thread() && c.parent_id.as_deref() == Some(forum_id)).collect();
        posts.sort_by_key(|c| std::cmp::Reverse(c.last_message_id.as_deref().map_or(0, snowflake)));
        posts
    }

    /// Active threads under a text channel, most recently active first.
    pub fn threads_of<'a>(&self, guild: &'a Guild, channel_id: &str) -> Vec<&'a Channel> {
        let mut threads: Vec<&Channel> = guild
            .channels
            .iter()
            .filter(|c| c.is_thread() && !c.is_archived() && c.parent_id.as_deref() == Some(channel_id))
            .collect();
        threads.sort_by_key(|c| std::cmp::Reverse(c.last_message_id.as_deref().map_or(0, snowflake)));
        threads
    }

    /// Adds or replaces a thread in its server.
    fn upsert_thread(&mut self, thread: Channel) {
        let guild_id = thread
            .guild_id
            .clone()
            .or_else(|| thread.parent_id.as_deref().and_then(|p| self.guild_of(p)).map(|g| g.id.clone()));
        let Some(guild) = guild_id.and_then(|id| self.guilds.iter_mut().find(|g| g.id == id)) else { return };
        let latest = thread.last_message_id.as_deref().map_or(0, snowflake);
        let latest_entry = self.latest.entry(thread.id.clone()).or_insert(0);
        *latest_entry = (*latest_entry).max(latest);
        self.read.entry(thread.id.clone()).or_insert(latest);
        match guild.channels.iter_mut().find(|c| c.id == thread.id) {
            Some(existing) => *existing = thread,
            None => guild.channels.push(thread),
        }
        self.channels_changed();
    }

    fn vote(&mut self, message_id: String, answer_id: u32) -> Task<Msg> {
        let Some(channel_id) = self.selected_channel.clone() else { return Task::none() };
        let Some(poll) = self.find_entry(&channel_id, &message_id).and_then(|e| e.message.poll.as_ref()) else {
            return Task::none();
        };
        if poll.is_closed() {
            return Task::none();
        }
        let before = poll.my_votes();
        let after = match (poll.allow_multiselect, before.contains(&answer_id)) {
            (_, true) => before.iter().copied().filter(|a| *a != answer_id).collect(),
            (true, false) => before.iter().copied().chain([answer_id]).collect(),
            (false, false) => vec![answer_id],
        };
        let request = self.rest.clone().vote(channel_id.clone(), message_id.clone(), after.clone());
        Task::perform(request, move |result| Msg::Voted {
            channel_id: channel_id.clone(),
            message_id: message_id.clone(),
            before: before.clone(),
            after: after.clone(),
            result,
        })
    }

    fn apply_vote(&mut self, channel_id: &str, message_id: &str, answer_id: u32, user_id: &str, add: bool) {
        let mine = self.me.as_ref().is_some_and(|me| me.id == user_id);
        if let Some(poll) = self.entry_mut(channel_id, message_id).and_then(|e| e.message.poll.as_mut()) {
            poll.apply_vote(answer_id, add, mine);
        }
    }

    fn send_poll(&mut self) -> Task<Msg> {
        let (Some(channel_id), Some(draft)) = (self.selected_channel.clone(), &self.poll_draft) else { return Task::none() };
        let question = draft.question.trim();
        let answers: Vec<&str> = draft.answers.iter().map(|a| a.trim()).filter(|a| !a.is_empty()).collect();
        if question.is_empty() || answers.is_empty() {
            self.status = "A poll needs a question and at least one answer.".to_owned();
            return Task::none();
        }
        let body = json!({
            "poll": {
                "question": { "text": question },
                "answers": answers.iter().map(|text| json!({ "poll_media": { "text": text } })).collect::<Vec<_>>(),
                "duration": draft.hours,
                "allow_multiselect": draft.multiselect,
                "layout_type": 1,
            }
        });
        self.poll_draft = None;
        Task::perform(self.rest.clone().send_json(channel_id, body), Msg::Sent)
    }

    /// Where commands are cached: the server, or the DM channel.
    fn commands_key(&self) -> Option<String> {
        self.selected_guild.clone().or_else(|| self.selected_channel.clone())
    }

    /// Slash commands for the open channel, once loaded.
    pub fn channel_commands(&self) -> Option<&[SlashCommand]> {
        match self.slash_commands.get(&self.commands_key()?)? {
            Some(Ok(list)) => Some(list.as_slice()),
            _ => None,
        }
    }

    /// Whether the command list is still loading, or why it failed.
    pub fn commands_state(&self) -> Option<Result<(), &str>> {
        match self.slash_commands.get(&self.commands_key()?)? {
            None => Some(Ok(())),
            Some(Err(error)) => Some(Err(error.as_str())),
            Some(Ok(_)) => None,
        }
    }

    fn load_commands(&mut self) -> Task<Msg> {
        if self.is_bot || self.offline {
            return Task::none(); // bots can't run other apps' commands
        }
        let (Some(key), Some(channel_id)) = (self.commands_key(), self.selected_channel.clone()) else { return Task::none() };
        if self.slash_commands.contains_key(&key) {
            return Task::none();
        }
        self.slash_commands.insert(key.clone(), None);
        let guild_id = self.selected_guild.clone();
        Task::perform(self.rest.clone().commands(guild_id, channel_id), move |result| Msg::CommandsLoaded {
            key: key.clone(),
            result,
        })
    }

    fn run_command(&mut self, channel_id: &str, command: SlashCommand, options: serde_json::Value) -> Task<Msg> {
        let nonce = nonce();
        let payload = json!({
            "type": 2,
            "application_id": command.application_id,
            "guild_id": self.selected_guild,
            "channel_id": channel_id,
            "session_id": self.session_id,
            "nonce": nonce,
            "data": {
                "version": command.version,
                "id": command.id,
                "name": command.name,
                "type": 1,
                "options": options,
                "application_command": command.raw,
                "attachments": [],
            },
        });
        self.send_interaction(nonce, payload)
    }

    /// A button press or menu choice on a message in the open channel.
    fn interact_component(&mut self, message_id: &str, data: serde_json::Value) -> Task<Msg> {
        let Some(channel_id) = self.selected_channel.clone() else { return Task::none() };
        let Some(message) = self.find_entry(&channel_id, message_id).map(|e| &e.message) else { return Task::none() };
        let Some(application_id) = message.app_id().map(str::to_owned) else { return Task::none() };
        let nonce = nonce();
        let payload = json!({
            "type": 3,
            "nonce": nonce,
            "guild_id": self.selected_guild,
            "channel_id": channel_id,
            "message_flags": message.flags,
            "message_id": message_id,
            "application_id": application_id,
            "session_id": self.session_id,
            "data": data,
        });
        self.send_interaction(nonce, payload)
    }

    fn send_interaction(&mut self, nonce: String, payload: serde_json::Value) -> Task<Msg> {
        if self.is_bot || self.offline {
            self.status = "Buttons and commands work with user logins only.".to_owned();
            return Task::none();
        }
        self.interactions.insert(nonce.clone());
        Task::perform(self.rest.clone().interact(payload), move |result| Msg::Interacted { nonce: nonce.clone(), result })
    }

    /// Server (or `None` for DMs) of a channel we can see; `Err` if we can't.
    fn locate(&self, channel_id: &str) -> Result<Option<String>, ()> {
        match self.guild_of(channel_id) {
            Some(guild) => Ok(Some(guild.id.clone())),
            None if self.dms.iter().any(|c| c.id == channel_id) => Ok(None),
            None => Err(()),
        }
    }

    /// Asks Discord about a channel we don't have (usually an archived thread),
    /// then jumps there if it turns out to be one we can see.
    fn fetch_unknown(&self, channel_id: String, message_id: Option<String>) -> Task<Msg> {
        if self.offline {
            return Task::none();
        }
        Task::perform(self.rest.clone().channel(channel_id), move |result| Msg::ChannelFetched {
            message_id: message_id.clone(),
            result,
        })
    }

    fn jump_to_channel(&mut self, channel_id: String) -> Task<Msg> {
        let Ok(guild) = self.locate(&channel_id) else {
            self.status = "Looking for that channel…".to_owned();
            return self.fetch_unknown(channel_id, None);
        };
        if self.selected_guild != guild {
            self.select_guild(guild);
        }
        self.open(channel_id)
    }

    /// Opens the channel scrolled to the message, loading history around it
    /// when it isn't in memory.
    fn jump_to_message(&mut self, channel_id: String, message_id: String) -> Task<Msg> {
        let Ok(guild) = self.locate(&channel_id) else {
            self.status = "Looking for that message…".to_owned();
            return self.fetch_unknown(channel_id, Some(message_id));
        };
        if self.selected_guild != guild {
            self.select_guild(guild);
        }
        let cached = self.logs.get(&channel_id).is_some_and(|log| log.contains(&message_id));
        if !cached {
            let mut log = ChannelLog::new();
            log.detached = true;
            self.logs.insert(channel_id.clone(), log);
        }
        let open = self.open(channel_id.clone());
        self.highlighted = Some(message_id.clone());
        if cached {
            return Task::batch([open, self.scroll_to_highlight()]);
        }
        Task::batch([open, self.fetch(channel_id, Cursor::Around(message_id), LoadKind::Around)])
    }

    /// Scrolls the message list so the highlighted message is roughly in view.
    /// Messages differ in height, so this goes by position in the list.
    fn scroll_to_highlight(&self) -> Task<Msg> {
        let (Some(log), Some(id)) = (self.selected_log(), &self.highlighted) else { return Task::none() };
        let Some(index) = log.entries.iter().position(|e| &e.message.id == id) else { return Task::none() };
        let from_top = match log.entries.len() {
            0 | 1 => 0.0,
            n => index as f32 / (n - 1) as f32,
        };
        // The list is anchored to the bottom, so offsets count from there.
        let offset = scrollable::RelativeOffset { x: 0.0, y: 1.0 - from_top };
        scrollable::snap_to(scrollable::Id::new(MESSAGES_ID), offset)
    }

    /// Quick switcher matches: unread channels first when the query is empty.
    pub fn switcher_results(&self) -> Vec<SwitchItem> {
        self.places(self.switcher.as_deref().unwrap_or_default())
    }

    /// Servers, channels and DMs matching `query` (the switcher, and forwarding).
    pub fn places(&self, query: &str) -> Vec<SwitchItem> {
        let query = query.trim().to_lowercase();
        let rank = |name: &str| -> Option<u8> {
            let name = name.to_lowercase();
            if query.is_empty() {
                Some(0)
            } else if name.starts_with(&query) {
                Some(0)
            } else if name.split(['-', '_', ' ']).any(|word| word.starts_with(&query)) {
                Some(1)
            } else if name.contains(&query) {
                Some(2)
            } else {
                None
            }
        };
        let mut items: Vec<(u8, SwitchItem)> = Vec::new();
        for guild in &self.guilds {
            if let Some(score) = rank(&guild.name).filter(|_| !query.is_empty()) {
                let (unread, mentions) = self.badge(guild.channels.iter().filter(|c| c.is_text()));
                let target = SwitchTarget::Guild(guild.id.clone());
                items.push((score, SwitchItem { target, label: guild.name.clone(), context: "Server".to_owned(), unread, mentions }));
            }
            for channel in guild.channels.iter().filter(|c| c.is_text()) {
                let Some(score) = rank(&channel.label()) else { continue };
                let item = SwitchItem {
                    target: SwitchTarget::Channel(channel.id.clone()),
                    label: format!("# {}", channel.label()),
                    context: guild.name.clone(),
                    unread: self.shows_unread(&channel.id),
                    mentions: self.mention_count(&channel.id),
                };
                items.push((score, item));
            }
        }
        for dm in &self.dms {
            let Some(score) = rank(&dm.label()) else { continue };
            let item = SwitchItem {
                target: SwitchTarget::Channel(dm.id.clone()),
                label: format!("@ {}", dm.label()),
                context: "Direct message".to_owned(),
                unread: self.shows_unread(&dm.id),
                mentions: self.mention_count(&dm.id),
            };
            items.push((score, item));
        }
        if query.is_empty() {
            // Nothing typed: what needs attention, then recent DMs (already newest first).
            items.retain(|(_, item)| item.unread || item.mentions > 0 || item.context == "Direct message");
        }
        // Stable: equal ranks keep server/channel order.
        items.sort_by_key(|(score, item)| (*score, item.mentions == 0, !item.unread));
        items.into_iter().take(SWITCHER_LIMIT).map(|(_, item)| item).collect()
    }

    fn open_search(&mut self) -> Task<Msg> {
        let focus = text_input::focus(text_input::Id::new(SEARCH_INPUT));
        if matches!(self.panel, Some(Panel::Search(_))) {
            return focus;
        }
        let Some(channel_id) = self.selected_channel.clone() else { return Task::none() };
        let guild_id = self.selected_guild.clone();
        let scope = match self.selected_guild() {
            Some(guild) => guild.name.clone(),
            None => self.channel_name(&channel_id),
        };
        self.panel = Some(Panel::Search(SearchPanel {
            query: String::new(),
            scope,
            guild_id,
            channel_id,
            loading: false,
            results: None,
        }));
        focus
    }

    fn show_profile(&mut self, user: User) -> Task<Msg> {
        if matches!(&self.panel, Some(Panel::Profile(p)) if p.user.id == user.id) {
            self.panel = None;
            return Task::none();
        }
        let guild_id = self.selected_guild.clone();
        let user_id = user.id.clone();
        self.panel = Some(Panel::Profile(ProfilePanel { user, profile: None }));
        let media = self.fetch_media();
        if self.is_bot || self.offline {
            return media;
        }
        let key = user_id.clone();
        let fetch = Task::perform(self.rest.clone().profile(user_id, guild_id), move |result| Msg::ProfileLoaded {
            user_id: key.clone(),
            result,
        });
        Task::batch([media, fetch])
    }

    fn apply_user_settings(&mut self, settings: UserSettings) {
        if let Some(folders) = settings.guild_folders {
            self.folders = folders;
        }
        if let Some(status) = settings.status.filter(|s| STATUSES.contains(&s.as_str())) {
            self.presence = status;
        }
        if let Some(custom) = settings.custom_status {
            self.custom_status = custom;
        }
    }

    /// Applies the status now (op 3) and saves it to Discord (user logins).
    fn set_presence(&mut self) -> Task<Msg> {
        self.send_command(Command::Presence { status: self.presence.clone(), custom: self.custom_status.clone() });
        if self.is_bot || self.offline {
            return Task::none();
        }
        // If saving fails, the status still applies until the next reconnect.
        Task::perform(self.rest.clone().save_status(self.presence.clone(), self.custom_status.clone()), |_| Msg::Ignored)
    }

    fn settings_key(&self, channel_id: &str) -> String {
        self.guild_of(channel_id).map(|g| g.id.clone()).unwrap_or_default()
    }

    fn settings_mut(&mut self, key: &str) -> &mut GuildSettings {
        self.guild_settings
            .entry(key.to_owned())
            .or_insert_with(|| GuildSettings::new((!key.is_empty()).then(|| key.to_owned())))
    }

    fn override_mut(&mut self, key: &str, channel_id: &str) -> &mut crate::discord::model::ChannelOverride {
        let settings = self.settings_mut(key);
        if let Some(index) = settings.channel_overrides.iter().position(|o| o.channel_id == channel_id) {
            return &mut settings.channel_overrides[index];
        }
        settings.channel_overrides.push(crate::discord::model::ChannelOverride {
            channel_id: channel_id.to_owned(),
            muted: false,
            mute_config: None,
            message_notifications: NotifyLevel::Inherit.number(),
        });
        settings.channel_overrides.last_mut().expect("just pushed")
    }

    /// Sends a settings change; the local copy was already updated.
    fn save_guild_settings(&self, key: &str, changes: serde_json::Value) -> Task<Msg> {
        if self.is_bot || self.offline {
            return Task::none();
        }
        let guild_id = (!key.is_empty()).then(|| key.to_owned());
        Task::perform(self.rest.clone().guild_settings(guild_id, changes), Msg::GuildSettingsSaved)
    }

    fn mute_channel(&mut self, channel_id: String, muted: bool) -> Task<Msg> {
        let key = self.settings_key(&channel_id);
        let entry = self.override_mut(&key, &channel_id);
        entry.muted = muted;
        entry.mute_config = None;
        let changes = json!({ "channel_overrides": { channel_id: { "muted": muted, "mute_config": mute_config(muted) } } });
        self.save_guild_settings(&key, changes)
    }

    /// Sets the level and unmutes (the header picker treats "Muted" as one more level).
    fn channel_notify(&mut self, channel_id: String, level: NotifyLevel) -> Task<Msg> {
        let key = self.settings_key(&channel_id);
        let entry = self.override_mut(&key, &channel_id);
        entry.message_notifications = level.number();
        entry.muted = false;
        entry.mute_config = None;
        let change = json!({ "message_notifications": level.number(), "muted": false, "mute_config": null });
        self.save_guild_settings(&key, json!({ "channel_overrides": { channel_id: change } }))
    }

    fn mute_guild(&mut self, guild_id: String, muted: bool) -> Task<Msg> {
        let settings = self.settings_mut(&guild_id);
        settings.muted = muted;
        settings.mute_config = None;
        self.save_guild_settings(&guild_id, json!({ "muted": muted, "mute_config": mute_config(muted) }))
    }

    fn guild_notify(&mut self, guild_id: String, level: NotifyLevel) -> Task<Msg> {
        self.settings_mut(&guild_id).message_notifications = level.number();
        self.save_guild_settings(&guild_id, json!({ "message_notifications": level.number() }))
    }

    pub fn guild_muted(&self, guild_id: &str) -> bool {
        self.guild_settings.get(guild_id).is_some_and(GuildSettings::is_muted)
    }

    /// The channel, its category, or (for server channels) the whole server is muted.
    pub fn channel_muted(&self, channel_id: &str) -> bool {
        let guild = self.guild_of(channel_id);
        let key = guild.map(|g| g.id.as_str()).unwrap_or_default();
        let Some(settings) = self.guild_settings.get(key) else { return false };
        let parent = guild
            .and_then(|g| g.channels.iter().find(|c| c.id == channel_id))
            .and_then(|c| c.parent_id.as_deref());
        let muted = |id: &str| settings.channel(id).is_some_and(|o| o.is_muted());
        muted(channel_id) || parent.is_some_and(muted) || (guild.is_some() && settings.is_muted())
    }

    /// The channel's own setting (`Inherit` when it follows the server).
    pub fn channel_notify_setting(&self, channel_id: &str) -> NotifyLevel {
        let key = self.settings_key(channel_id);
        self.guild_settings
            .get(&key)
            .and_then(|s| s.channel(channel_id))
            .map_or(NotifyLevel::Inherit, |o| NotifyLevel::from_number(o.message_notifications))
    }

    /// The server's setting, falling back to its default.
    pub fn guild_notify_setting(&self, guild_id: &str) -> NotifyLevel {
        let chosen = self.guild_settings.get(guild_id).map(|s| NotifyLevel::from_number(s.message_notifications));
        match chosen {
            Some(level) if level != NotifyLevel::Inherit => level,
            _ => self.guilds.iter().find(|g| g.id == guild_id).map_or(NotifyLevel::Mentions, |g| g.default_notifications),
        }
    }

    /// What notifies in a channel: its own setting, its category's, then the server's.
    fn notify_level(&self, channel_id: &str) -> NotifyLevel {
        let Some(guild) = self.guild_of(channel_id) else {
            return match self.channel_notify_setting(channel_id) {
                NotifyLevel::Inherit => NotifyLevel::All, // DMs
                level => level,
            };
        };
        let parent = guild.channels.iter().find(|c| c.id == channel_id).and_then(|c| c.parent_id.clone());
        [Some(channel_id.to_owned()), parent]
            .into_iter()
            .flatten()
            .map(|id| self.channel_notify_setting(&id))
            .find(|level| *level != NotifyLevel::Inherit)
            .unwrap_or_else(|| self.guild_notify_setting(&guild.id))
    }

    /// Bold and counted as unread: unread and not muted.
    pub fn shows_unread(&self, channel_id: &str) -> bool {
        self.is_unread(channel_id) && !self.channel_muted(channel_id)
    }

    /// Asks for the open channel's member sidebar (user sessions, when shown).
    fn subscribe_members(&mut self) -> Task<Msg> {
        if self.is_bot || !self.settings.member_list || !self.show_members {
            return Task::none();
        }
        let (Some(guild_id), Some(channel_id)) = (self.selected_guild.clone(), self.selected_channel.clone()) else {
            return Task::none();
        };
        self.send_command(Command::SubscribeMembers { guild_id, channel_id });
        Task::none()
    }

    fn mark_read(&mut self, channel_id: &str) -> Task<Msg> {
        self.mentions.remove(channel_id);
        let Some(&latest) = self.latest.get(channel_id).filter(|&&id| id > 0) else {
            return Task::none();
        };
        let previous = self.read.insert(channel_id.to_owned(), latest);
        if self.is_bot || previous.is_some_and(|read| read >= latest) {
            return Task::none();
        }
        self.ack(channel_id.to_owned(), latest.to_string())
    }

    fn ack(&self, channel_id: String, message_id: String) -> Task<Msg> {
        if !self.settings.sync_read_state {
            return Task::none();
        }
        // A failed ack only leaves the channel unread in other clients.
        Task::perform(self.rest.clone().ack(channel_id, message_id), |_| Msg::Ignored)
    }

    fn fetch(&self, channel_id: String, cursor: Cursor, kind: LoadKind) -> Task<Msg> {
        // The offline demo has no account to ask (its token is a placeholder).
        if self.offline {
            return Task::done(Msg::MessagesLoaded { channel_id, kind, result: Ok(Vec::new()) });
        }
        let key = channel_id.clone();
        Task::perform(self.rest.clone().messages(channel_id, cursor), move |result| {
            Msg::MessagesLoaded { channel_id: key.clone(), kind, result }
        })
    }

    fn load_older(&mut self) -> Task<Msg> {
        let Some(channel_id) = self.selected_channel.clone() else { return Task::none() };
        let Some(log) = self.logs.get_mut(&channel_id) else { return Task::none() };
        if !log.can_load_older() {
            return Task::none();
        }
        let Some(oldest) = log.entries.first().map(|e| e.message.id.clone()) else { return Task::none() };
        log.loading = true;
        self.fetch(channel_id, Cursor::Before(oldest), LoadKind::Older)
    }

    fn on_loaded(&mut self, channel_id: String, kind: LoadKind, result: Result<Vec<Message>, String>) -> Task<Msg> {
        let Some(log) = self.logs.get_mut(&channel_id) else { return Task::none() };
        log.loading = false;
        let older = kind == LoadKind::Older;
        match result {
            Ok(fetched) => {
                // Around a message there is (almost always) older history too.
                log.has_more = fetched.len() == PAGE_SIZE || kind == LoadKind::Around;
                let mut fetched: Vec<Entry> = fetched
                    .into_iter()
                    .filter(|m| !(older && log.contains(&m.id)))
                    .map(Entry::new)
                    .collect();
                if older {
                    log.limit = (log.limit + PAGE_SIZE).min(HISTORY_LIMIT);
                    fetched.append(&mut log.entries);
                } else if kind == LoadKind::Around {
                    log.entries.clear();
                } else {
                    // Keep gateway messages that arrived while the fetch was in flight.
                    for entry in log.entries.drain(..) {
                        if !fetched.iter().any(|e| e.message.id == entry.message.id) {
                            fetched.push(entry);
                        }
                    }
                }
                log.entries = fetched;
                let scroll = if kind == LoadKind::Around { self.scroll_to_highlight() } else { Task::none() };
                Task::batch([self.fetch_media(), scroll])
            }
            Err(error) => {
                if !older {
                    self.logs.remove(&channel_id);
                }
                self.status = format!("Could not load messages: {error}");
                Task::none()
            }
        }
    }

    /// Images the open channel (and member sidebar) shows, as enabled in settings.
    /// Marks them wanted in the cache and starts any missing downloads.
    pub fn fetch_media(&mut self) -> Task<Msg> {
        let settings = &self.settings;
        // With animation on, images are asked for as possibly-animated; GIFs then play.
        let animate = settings.animate_media;
        let round = |url: String| (url, if animate { Shape::Animated { round: true } } else { Shape::Round });
        let square = |url: String| (url, if animate { Shape::Animated { round: false } } else { Shape::Square });
        let mut urls = Vec::new();
        if settings.load_avatars {
            let icons = self.guilds.iter().filter_map(|g| images::guild_icon_url(&g.id, g.icon.as_deref()?));
            urls.extend(icons.map(|url| (url, Shape::Round)));
            urls.extend(self.me.iter().map(|me| round(self.avatar_url(me))));
        }
        if let Some(log) = self.selected_log() {
            for entry in log.entries.iter().rev().take(MEDIA_RECENT) {
                let message = &entry.message;
                if settings.load_avatars {
                    urls.push(round(self.avatar_url(&message.author)));
                }
                if settings.load_custom_emoji {
                    let reaction_emoji = message.reactions.iter().filter_map(|r| Some((r.emoji.id.as_deref()?, r.emoji.animated)));
                    // Ids are validated digits, so they are safe in the URL.
                    let emoji = entry.body.emoji_ids().chain(reaction_emoji);
                    urls.extend(emoji.map(|(id, animated)| square(self.emoji_image(id, animated))));
                    urls.extend(message.sticker_items.iter().filter_map(|s| s.image_url()).map(square));
                }
                if settings.load_images {
                    urls.extend(self.media_previews(message).into_iter().map(|p| square(p.url)));
                }
            }
        }
        if let Some(picker) = self.gif_picker.as_ref().filter(|_| settings.gif_picker) {
            if let Some(Ok(gifs)) = &picker.results {
                urls.extend(gifs.iter().filter_map(|gif| self.gif_tile(gif)).map(|p| square(p.url)));
            }
        }
        if settings.load_custom_emoji && self.sticker_picker {
            let stickers = self.selected_guild().map(|g| g.stickers.as_slice()).unwrap_or_default();
            urls.extend(stickers.iter().filter_map(|s| s.item().image_url()).map(square));
        }
        if settings.load_avatars && self.selected_guild.is_none() && self.selected_channel.is_none() {
            // The Home page's friends (at most a few hundred small avatars).
            urls.extend(self.relationships.iter().filter_map(|r| r.user.as_ref()).map(|u| round(self.avatar_url(u))));
        }
        if settings.load_avatars && self.in_voice_view() {
            // The voice stage's tiles.
            let channel_id = self.selected_channel.as_deref().unwrap_or_default();
            if let Some(guild) = self.guild_of(channel_id) {
                let states = guild.voice_states.iter().filter(|s| s.channel_id.as_deref() == Some(channel_id));
                urls.extend(states.filter_map(|s| self.voice_user(s)).map(|u| round(self.avatar_url(&u))));
            }
        }
        if let (true, Some(Panel::Profile(profile))) = (settings.load_avatars, &self.panel) {
            urls.push(round(self.avatar_url(&profile.user)));
        }
        if settings.load_avatars && self.show_members {
            if let Some(list) = self.selected_guild.as_deref().and_then(|id| self.member_lists.get(id)) {
                urls.extend(list.rows.iter().flatten().filter_map(|row| match row {
                    member_list::Item::Member(m) => Some(round(self.avatar_url(&m.user))),
                    member_list::Item::Group(_) => None,
                }));
            }
        }
        let start = self.images.want(urls);
        self.download(start)
    }

    /// Downloads, then decodes on a worker thread, so a burst of avatars or
    /// GIFs never stalls the UI.
    fn download(&self, urls: Vec<String>) -> Task<Msg> {
        Task::batch(urls.into_iter().map(|url| {
            let key = url.clone();
            let shape = self.images.shape_of(&url);
            let rest = self.rest.clone();
            let work = async move {
                let bytes = rest.cdn(url).await?;
                tokio::task::spawn_blocking(move || images::prepare(bytes, shape)).await.map_err(|e| e.to_string())
            };
            Task::perform(work, move |result| Msg::ImageLoaded(key.clone(), result))
        }))
    }

    fn send_typing(&mut self) -> Task<Msg> {
        let Some(channel_id) = self.selected_channel.clone() else { return Task::none() };
        if !self.settings.send_typing || self.editing.is_some() || self.composer.trim().is_empty() {
            return Task::none();
        }
        let now = Instant::now();
        if let Some((last, at)) = &self.last_typing_sent {
            if *last == channel_id && now.duration_since(*at) < TYPING_RESEND {
                return Task::none();
            }
        }
        self.last_typing_sent = Some((channel_id.clone(), now));
        // A failure (e.g. no send permission) only affects what others see.
        Task::perform(self.rest.clone().typing(channel_id), |_| Msg::Ignored)
    }

    fn subscribe(&mut self, guild_id: String) {
        if !self.is_bot && self.subscribed.insert(guild_id.clone()) {
            self.send_command(Command::SubscribeGuild(guild_id));
        }
    }

    fn send_command(&mut self, command: Command) {
        if let Some(commands) = &mut self.commands {
            // A full or closed queue drops the command; READY resends subscriptions.
            let _ = commands.try_send(command);
        }
    }

    pub fn is_bot(&self) -> bool {
        self.is_bot
    }

    pub fn selected_log(&self) -> Option<&ChannelLog> {
        self.logs.get(self.selected_channel.as_deref()?)
    }

    fn entry_mut(&mut self, channel_id: &str, message_id: &str) -> Option<&mut Entry> {
        self.logs.get_mut(channel_id)?.entries.iter_mut().find(|e| e.message.id == message_id)
    }

    pub fn find_entry(&self, channel_id: &str, message_id: &str) -> Option<&Entry> {
        self.logs.get(channel_id)?.entries.iter().find(|e| e.message.id == message_id)
    }

    pub fn is_mine(&self, message: &Message) -> bool {
        self.me.as_ref().is_some_and(|me| me.id == message.author.id)
    }

    /// Own messages, or any message where we have Manage Messages.
    pub fn can_delete(&self, message: &Message) -> bool {
        if self.is_mine(message) {
            return true;
        }
        let (Some(me), Some(guild)) = (&self.me, self.guild_of(&message.channel_id)) else { return false };
        guild
            .channels
            .iter()
            .find(|c| c.id == message.channel_id)
            .and_then(|channel| permissions::channel_permissions(guild, channel, &me.id))
            .is_some_and(|p| p & MANAGE_MESSAGES != 0)
    }

    /// The server a channel belongs to. Called for every channel and message on
    /// each redraw, so answers are cached; a cached server is checked before use,
    /// and the cache is cleared whenever channels are added.
    pub fn guild_of(&self, channel_id: &str) -> Option<&Guild> {
        let cached = self.channel_guilds.borrow().get(channel_id).cloned();
        match cached {
            Some(None) => return None,
            Some(Some(guild_id)) => {
                if let Some(guild) = self.guilds.iter().find(|g| g.id == guild_id).filter(|g| g.channels.iter().any(|c| c.id == channel_id)) {
                    return Some(guild);
                }
            }
            None => {}
        }
        let found = self.guilds.iter().find(|g| g.channels.iter().any(|c| c.id == channel_id));
        self.channel_guilds.borrow_mut().insert(channel_id.to_owned(), found.map(|g| g.id.clone()));
        found
    }

    /// Channels were added or moved: forget cached owners (see [`Session::guild_of`]).
    fn channels_changed(&self) {
        self.channel_guilds.borrow_mut().clear();
    }

    pub fn user_name(&self, message: &Message, user_id: &str) -> String {
        message
            .mentions
            .iter()
            .find(|u| u.id == user_id)
            .map(|u| u.display_name().to_owned())
            .or_else(|| self.known_name(user_id))
            .unwrap_or_else(|| "unknown-user".to_owned())
    }

    pub fn role(&self, role_id: &str) -> Option<&Role> {
        self.guilds.iter().flat_map(|g| &g.roles).find(|r| r.id == role_id)
    }

    pub fn channel_name(&self, channel_id: &str) -> String {
        self.channel(channel_id).map_or_else(|| "unknown-channel".to_owned(), Channel::label)
    }

    /// Readable one-line text for previews and notifications; spoilers stay hidden.
    pub fn plain_text(&self, message: &Message) -> String {
        let mut out = String::new();
        for block in markup::parse(&message.content).blocks {
            match block {
                Block::Code(code) => out.push_str(&code),
                Block::Line { inlines, .. } => {
                    for inline in inlines {
                        if inline.style.spoiler.is_some() {
                            out.push_str("▒▒▒");
                            continue;
                        }
                        match inline.kind {
                            Kind::Text(text) | Kind::Code(text) | Kind::Link(text) => out.push_str(&text),
                            Kind::User(id) => out.push_str(&format!("@{}", self.user_name(message, &id))),
                            Kind::Role(id) => {
                                out.push_str(&format!("@{}", self.role(&id).map_or("unknown-role", |r| &r.name)))
                            }
                            Kind::Channel(id) => out.push_str(&format!("#{}", self.channel_name(&id))),
                            Kind::Everyone(mention) => out.push_str(mention),
                            Kind::Emoji { name, .. } => out.push_str(&format!(":{name}:")),
                            Kind::Timestamp { .. } => out.push_str("[time]"),
                        }
                    }
                }
            }
            out.push(' ');
        }
        let out = out.trim();
        match message.attachments.first() {
            Some(attachment) if out.is_empty() => format!("[file] {}", attachment.filename),
            _ => out.to_owned(),
        }
    }

    pub fn selected_guild(&self) -> Option<&Guild> {
        let id = self.selected_guild.as_deref()?;
        self.guilds.iter().find(|g| g.id == id)
    }

    /// Custom emoji usable in the composer: the open server's.
    pub fn custom_emojis(&self) -> &[CustomEmoji] {
        self.selected_guild().map_or(&[], |g| g.emojis.as_slice())
    }

    pub fn is_revealed(&self, message_id: &str, spoiler: u16) -> bool {
        self.revealed.contains(&(message_id.to_owned(), spoiler))
    }

    pub fn channel(&self, id: &str) -> Option<&Channel> {
        self.guilds.iter().flat_map(|g| &g.channels).chain(&self.dms).find(|c| c.id == id)
    }

    pub fn is_unread(&self, channel_id: &str) -> bool {
        match (self.latest.get(channel_id), self.read.get(channel_id)) {
            (Some(latest), Some(read)) => latest > read,
            _ => false,
        }
    }

    pub fn mention_count(&self, channel_id: &str) -> u32 {
        self.mentions.get(channel_id).copied().unwrap_or(0)
    }

    /// (any unread, total mentions) over `channels`; muted channels count mentions only.
    pub fn badge<'a>(&self, channels: impl IntoIterator<Item = &'a Channel>) -> (bool, u32) {
        channels.into_iter().fold((false, 0), |(unread, mentions), c| {
            (unread || self.shows_unread(&c.id), mentions + self.mention_count(&c.id))
        })
    }

    pub fn has_typing(&self) -> bool {
        !self.typing.is_empty()
    }

    pub fn typing_line(&self, channel_id: &str) -> Option<String> {
        let names: Vec<&str> = self.typing.get(channel_id)?.iter().map(|t| t.name.as_str()).collect();
        match names.as_slice() {
            [] => None,
            [one] => Some(format!("{one} is typing…")),
            [a, b] => Some(format!("{a} and {b} are typing…")),
            [a, b, c] => Some(format!("{a}, {b} and {c} are typing…")),
            _ => Some("Several people are typing…".to_owned()),
        }
    }
}

/// Image URLs that depend on settings. The loader and the view both use these,
/// so they always agree on which URL an image is cached under.
impl Session {
    pub fn avatar_url(&self, user: &User) -> String {
        user.avatar_url_with(self.settings.animate_media)
    }

    pub fn emoji_image(&self, id: &str, animated: bool) -> String {
        images::emoji_url(id, animated && self.settings.animate_media)
    }

    /// GIF attachments keep their format when animation is on.
    pub fn attachment_preview(&self, attachment: &crate::discord::model::Attachment) -> Option<images::Preview> {
        let (width, height) = attachment.dimensions()?;
        let gif = attachment.content_type.as_deref() == Some("image/gif");
        images::preview_with(attachment.proxy_url.as_deref()?, width, height, gif && self.settings.animate_media)
    }

    /// GIF embeds (Tenor) play as Tenor's small GIF when animation and the GIF
    /// feature are on; otherwise the still from Discord's proxy.
    pub fn embed_preview(&self, embed: &Embed) -> Option<images::Preview> {
        let media = embed.media()?;
        let (width, height) = (media.width?, media.height?);
        if embed.kind.as_deref() == Some("gifv") && self.settings.animate_media && self.settings.gif_picker {
            let tiny = media.url.as_deref().and_then(images::tenor_tinygif);
            if let Some(preview) = tiny.and_then(|url| images::tenor_preview(url, width, height)) {
                return Some(preview);
            }
        }
        images::preview(media.proxy_url.as_deref()?, width, height)
    }

    /// Previews for a message's image attachments, embeds and component media.
    fn media_previews(&self, message: &Message) -> Vec<images::Preview> {
        let attachments = message.attachments.iter().filter_map(|a| self.attachment_preview(a));
        let embeds = message.embeds.iter().filter_map(|e| self.embed_preview(e));
        let components = component_media(&message.components).into_iter().filter_map(|m| {
            images::preview(m.proxy_url.as_deref().or(m.url.as_deref())?, m.width?, m.height?)
        });
        attachments.chain(embeds).chain(components).collect()
    }

    /// A GIF picker tile, 110 px tall: the still, or the small GIF with animation on.
    pub fn gif_tile(&self, gif: &Gif) -> Option<images::Preview> {
        const TILE_HEIGHT: f32 = 110.0;
        let source = if self.settings.animate_media { images::tenor_tinygif(&gif.src) } else { None };
        let url = source.unwrap_or_else(|| gif.preview.clone());
        let mut preview = images::tenor_preview(url, gif.width.max(1), gif.height.max(1))?;
        let scale = TILE_HEIGHT / preview.height;
        preview.width = (preview.width * scale).min(260.0);
        preview.height = TILE_HEIGHT;
        Some(preview)
    }

    /// How often to advance animations: the shortest frame on screen, at most
    /// 20 times a second. `None` when nothing animates (or the window is in the
    /// background), so nothing redraws.
    pub fn animation_interval(&self) -> Option<Duration> {
        if !self.settings.animate_media || !self.focused {
            return None;
        }
        self.images.shortest_frame().map(|ms| Duration::from_millis(u64::from(ms.max(50))))
    }
}

/// The GIF picker above the message box.
pub struct GifPicker {
    pub query: String,
    pub loading: bool,
    pub results: Option<Result<Vec<Gif>, String>>,
}

pub const GIF_INPUT: &str = "gif-search";

/// Thumbnails and gallery pictures in a components-v2 layout.
pub fn component_media(components: &[Component]) -> Vec<&crate::discord::model::UnfurledMedia> {
    fn walk<'a>(component: &'a Component, out: &mut Vec<&'a crate::discord::model::UnfurledMedia>) {
        out.extend(component.media.iter());
        out.extend(component.items.iter().map(|item| &item.media));
        for child in component.children() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    for component in components {
        walk(component, &mut out);
    }
    out
}

/// A snowflake for now, which Discord uses to match an interaction to its outcome.
fn nonce() -> String {
    const DISCORD_EPOCH_MS: u128 = 1_420_070_400_000;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    ((now.saturating_sub(DISCORD_EPOCH_MS) as u64) << 22).to_string()
}

/// Indefinite mute, or none.
fn mute_config(muted: bool) -> serde_json::Value {
    if muted {
        json!({ "selected_time_window": -1, "end_time": null })
    } else {
        serde_json::Value::Null
    }
}

/// (channel id, message id) from a discord.com/channels/… link.
pub fn discord_link(url: &str) -> Option<(String, Option<String>)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    let host = host.strip_prefix("www.").unwrap_or(host);
    let host = host.strip_prefix("ptb.").or_else(|| host.strip_prefix("canary.")).unwrap_or(host);
    if host != "discord.com" && host != "discordapp.com" {
        return None;
    }
    let path = path.split(['?', '#']).next()?.trim_end_matches('/');
    let mut parts = path.split('/');
    let id = |part: &&str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if parts.next()? != "channels" {
        return None;
    }
    let guild = parts.next()?;
    if guild != "@me" && !id(&guild) {
        return None;
    }
    let channel = parts.next().filter(id)?;
    let message = match parts.next() {
        Some(part) if id(&part) => Some(part.to_owned()),
        Some(_) => return None,
        None => None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((channel.to_owned(), message))
}

fn apply_voice_settings(controls: &Controls, settings: &Settings) {
    let (gain, volume, threshold) = settings.voice_levels();
    controls.set_levels(gain, volume, threshold);
    controls.set_devices(settings.voice_input_device.clone(), settings.voice_output_device.clone());
    controls.set_input_mode(settings.push_to_talk.then(|| settings.ptt_key.code()), settings.noise_suppression);
}

pub fn reaction_emoji(pick: Pick) -> ReactionEmoji {
    match pick {
        Pick::Unicode(emoji) => ReactionEmoji { id: None, name: Some(emoji), animated: false },
        Pick::Custom(emoji) => ReactionEmoji { id: Some(emoji.id), name: Some(emoji.name), animated: emoji.animated },
    }
}

pub fn human_size(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.0} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

async fn pick_files() -> Vec<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title("Attach files")
        .pick_files()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|file| file.path().to_path_buf())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::discord_link;

    #[test]
    fn reads_discord_message_and_channel_links() {
        let message = discord_link("https://discord.com/channels/100/200/300");
        assert_eq!(message, Some(("200".to_owned(), Some("300".to_owned()))));
        let dm = discord_link("https://ptb.discord.com/channels/@me/200?foo=1");
        assert_eq!(dm, Some(("200".to_owned(), None)));
        assert_eq!(discord_link("https://canary.discordapp.com/channels/1/2/3/").map(|l| l.0), Some("2".to_owned()));
    }

    #[test]
    fn ignores_other_links() {
        assert_eq!(discord_link("https://example.com/channels/1/2/3"), None);
        assert_eq!(discord_link("https://discord.com.evil.example/channels/1/2/3"), None);
        assert_eq!(discord_link("http://discord.com/channels/1/2/3"), None);
        assert_eq!(discord_link("https://discord.com/channels/1/abc"), None);
        assert_eq!(discord_link("https://discord.com/channels/1/2/3/4"), None);
        assert_eq!(discord_link("https://discord.com/invite/abc"), None);
    }

    #[test]
    fn failed_send_gives_back_its_own_text() {
        use crate::Msg;
        let mut session = super::Session::new("token".to_owned(), crate::settings::Settings::default());
        let failed = |text: &str| Msg::SendFailed { error: "HTTP 429".to_owned(), text: text.to_owned() };
        // "hi" and "yo" were both in flight; "hi" failed.
        let _ = session.update(failed("hi"));
        assert_eq!(session.composer, "hi");
        // A second failure doesn't overwrite text that's already back in the box.
        let _ = session.update(failed("yo"));
        assert_eq!(session.composer, "hi");
        assert!(session.status.contains("Send failed"));
    }
}
