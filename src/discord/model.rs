use std::collections::HashMap;

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::permissions;

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub global_name: Option<String>,
    /// Avatar hash; `None` uses a default avatar.
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub bot: bool,
}

impl User {
    pub fn display_name(&self) -> &str {
        self.global_name.as_deref().unwrap_or(&self.username)
    }

    pub fn avatar_url(&self) -> String {
        self.avatar_url_with(false)
    }

    /// `animate`: animated avatars (`a_` hashes) as GIFs.
    pub fn avatar_url_with(&self, animate: bool) -> String {
        match &self.avatar {
            Some(hash) if animate && hash.starts_with("a_") && hash.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                format!("https://cdn.discordapp.com/avatars/{}/{hash}.gif?size=64", self.id)
            }
            // Hashes are hex (optionally "a_" for animated); a static WebP frame is enough.
            Some(hash) if hash.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                format!("https://cdn.discordapp.com/avatars/{}/{hash}.webp?size=64", self.id)
            }
            _ => format!("https://cdn.discordapp.com/embed/avatars/{}.png", (snowflake(&self.id) >> 22) % 6),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub url: String,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub size: u64,
    /// Voice messages: length and a base64 loudness outline (one byte per sample).
    #[serde(default)]
    pub duration_secs: Option<f32>,
    #[serde(default)]
    pub waveform: Option<String>,
}

impl Attachment {
    /// Images (and videos, which Discord gives dimensions and a still frame).
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        Some((self.width?, self.height?))
    }

    /// Sound Ferrochat can play: voice messages (Ogg Opus) and common audio files.
    pub fn is_audio(&self) -> bool {
        let lower = self.filename.to_ascii_lowercase();
        self.content_type.as_deref().is_some_and(|t| t.starts_with("audio/"))
            || [".ogg", ".opus", ".mp3", ".wav", ".m4a", ".aac", ".flac", ".wma"].iter().any(|ext| lower.ends_with(ext))
    }
}

/// A friend, a block, or a friend request (user logins).
#[derive(Debug, Clone, Deserialize)]
pub struct Relationship {
    /// The other user's id.
    pub id: String,
    /// One of the `RELATIONSHIP_*` kinds.
    #[serde(rename = "type")]
    pub kind: u8,
    /// Missing when READY sends users separately; filled in from that list.
    #[serde(default)]
    pub user: Option<User>,
    /// Your private nickname for a friend.
    #[serde(default)]
    pub nickname: Option<String>,
}

pub const RELATIONSHIP_FRIEND: u8 = 1;
pub const RELATIONSHIP_BLOCKED: u8 = 2;
pub const RELATIONSHIP_INCOMING: u8 = 3;
pub const RELATIONSHIP_OUTGOING: u8 = 4;

impl Relationship {
    pub fn name(&self) -> String {
        match (&self.nickname, &self.user) {
            (Some(nick), _) if !nick.is_empty() => nick.clone(),
            (_, Some(user)) => user.display_name().to_owned(),
            _ => "Unknown user".to_owned(),
        }
    }
}

/// (user id, status) from a presence object: `{ user: { id }, status }` or `{ user_id, status }`.
pub fn presence_of(value: &Value) -> Option<(String, String)> {
    let id = value["user"]["id"].as_str().or_else(|| value["user_id"].as_str())?;
    let status = value["status"].as_str().unwrap_or("offline");
    Some((id.to_owned(), status.to_owned()))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmbedMedia {
    /// The original (for Tenor, its own media host).
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmbedName {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmbedFooter {
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmbedField {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Embed {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub color: Option<u32>,
    #[serde(default)]
    pub author: Option<EmbedName>,
    #[serde(default)]
    pub provider: Option<EmbedName>,
    #[serde(default)]
    pub footer: Option<EmbedFooter>,
    #[serde(default)]
    pub fields: Vec<EmbedField>,
    #[serde(default)]
    pub image: Option<EmbedMedia>,
    #[serde(default)]
    pub thumbnail: Option<EmbedMedia>,
    /// GIF embeds (`gifv`) are MP4 videos; we show the thumbnail or a GIF instead.
    #[serde(default)]
    pub video: Option<EmbedMedia>,
}

impl Embed {
    /// A bare image/GIF link: shown as just the picture, like Discord does.
    pub fn is_media_only(&self) -> bool {
        matches!(self.kind.as_deref(), Some("image" | "gifv"))
    }

    /// The picture to preview: the large image, else the thumbnail.
    pub fn media(&self) -> Option<&EmbedMedia> {
        self.image.as_ref().or(self.thumbnail.as_ref())
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ReactionEmoji {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub animated: bool,
}

impl ReactionEmoji {
    pub fn same(&self, other: &ReactionEmoji) -> bool {
        match (&self.id, &other.id) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self.name == other.name,
            _ => false,
        }
    }

    /// The form the reactions endpoint takes: a Unicode emoji, or `name:id`.
    pub fn api_name(&self) -> Option<String> {
        match &self.id {
            Some(id) => Some(format!("{}:{id}", self.name.as_deref().unwrap_or("_"))),
            None => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Reaction {
    pub count: u32,
    #[serde(default)]
    pub me: bool,
    pub emoji: ReactionEmoji,
}

/// MESSAGE_REACTION_ADD / MESSAGE_REACTION_REMOVE.
#[derive(Debug, Clone, Deserialize)]
pub struct ReactionEvent {
    pub user_id: String,
    pub channel_id: String,
    pub message_id: String,
    pub emoji: ReactionEmoji,
}

/// MESSAGE_REACTION_REMOVE_ALL (no emoji) / MESSAGE_REACTION_REMOVE_EMOJI.
#[derive(Debug, Clone, Deserialize)]
pub struct ReactionClear {
    pub channel_id: String,
    pub message_id: String,
    #[serde(default)]
    pub emoji: Option<ReactionEmoji>,
}

/// Message type 19.
pub const REPLY: u8 = 19;

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: u8,
    pub channel_id: String,
    /// The message this replies to; `None` on a reply means it was deleted.
    #[serde(default)]
    pub referenced_message: Option<Box<Message>>,
    #[serde(default)]
    pub guild_id: Option<String>,
    pub author: User,
    #[serde(default)]
    pub content: String,
    pub timestamp: DateTime<FixedOffset>,
    #[serde(default)]
    pub edited_timestamp: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub mentions: Vec<User>,
    #[serde(default)]
    pub mention_everyone: bool,
    #[serde(default)]
    pub mention_roles: Vec<String>,
    #[serde(default)]
    pub embeds: Vec<Embed>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub poll: Option<Poll>,
    #[serde(default)]
    pub sticker_items: Vec<StickerItem>,
    /// Buttons, menus and (components v2) whole layouts from apps.
    #[serde(default)]
    pub components: Vec<Component>,
    /// The app that sent it (bot replies, interaction responses).
    #[serde(default)]
    pub application_id: Option<String>,
    #[serde(default)]
    pub flags: u64,
    #[serde(default)]
    pub pinned: bool,
}

impl Message {
    /// A system message (see [`Message::system_text`]).
    pub fn is_system(&self) -> bool {
        matches!(self.kind, 1..=12 | 18 | 24 | 46)
    }

    /// What a system message (a join, boost, pin, thread…) says, written out:
    /// these carry no text of their own. `None` for ordinary messages.
    pub fn system_text(&self) -> Option<String> {
        let who = self.author.display_name();
        let other = self.mentions.first().map_or("someone", User::display_name);
        Some(match self.kind {
            1 => format!("{who} added {other} to the group."),
            2 if self.mentions.first().is_some_and(|m| m.id == self.author.id) => format!("{who} left the group."),
            2 => format!("{who} removed {other} from the group."),
            3 => format!("{who} started a call."),
            4 => format!("{who} changed the channel name: {}", self.content),
            5 => format!("{who} changed the channel icon."),
            6 => format!("{who} pinned a message to this channel."),
            7 => format!("{who} joined the server."),
            8 => format!("{who} boosted the server!"),
            9..=11 => format!("{who} boosted the server! It reached Level {}.", self.kind - 8),
            12 => format!("{who} added {} to this channel.", self.content),
            18 => format!("{who} started a thread: {}", self.content),
            24 => "AutoMod blocked a message.".to_owned(),
            46 => "A poll has closed.".to_owned(),
            _ => return None,
        })
    }

    /// The app to address when pressing this message's buttons.
    pub fn app_id(&self) -> Option<&str> {
        self.application_id.as_deref().or(self.author.bot.then_some(self.author.id.as_str()))
    }
}

/// MESSAGE_UPDATE payloads may be partial; only fields we render are kept.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageUpdate {
    pub id: String,
    pub channel_id: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub edited_timestamp: Option<String>,
    /// Link previews often arrive in a later update.
    #[serde(default)]
    pub embeds: Option<Vec<Embed>>,
    /// Apps edit their buttons and layouts.
    #[serde(default)]
    pub components: Option<Vec<Component>>,
    #[serde(default)]
    pub poll: Option<Poll>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PollMedia {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub emoji: Option<ReactionEmoji>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PollAnswer {
    pub answer_id: u32,
    #[serde(default)]
    pub poll_media: PollMedia,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnswerCount {
    pub id: u32,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub me_voted: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PollResults {
    #[serde(default)]
    pub is_finalized: bool,
    #[serde(default)]
    pub answer_counts: Vec<AnswerCount>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Poll {
    #[serde(default)]
    pub question: PollMedia,
    #[serde(default)]
    pub answers: Vec<PollAnswer>,
    #[serde(default)]
    pub expiry: Option<DateTime<FixedOffset>>,
    #[serde(default)]
    pub allow_multiselect: bool,
    /// Missing until someone votes (and in some payloads).
    #[serde(default)]
    pub results: Option<PollResults>,
}

impl Poll {
    pub fn is_closed(&self) -> bool {
        self.results.as_ref().is_some_and(|r| r.is_finalized) || self.expiry.is_some_and(|end| end <= chrono::Utc::now())
    }

    pub fn count(&self, answer_id: u32) -> Option<&AnswerCount> {
        self.results.as_ref()?.answer_counts.iter().find(|c| c.id == answer_id)
    }

    pub fn total(&self) -> u32 {
        self.results.as_ref().map_or(0, |r| r.answer_counts.iter().map(|c| c.count).sum())
    }

    pub fn my_votes(&self) -> Vec<u32> {
        self.results.as_ref().map_or_else(Vec::new, |r| {
            r.answer_counts.iter().filter(|c| c.me_voted).map(|c| c.id).collect()
        })
    }

    /// Counts one vote (gateway event or our own). Our own votes are applied
    /// both from the REST result and the gateway, so `mine` is idempotent.
    pub fn apply_vote(&mut self, answer_id: u32, add: bool, mine: bool) {
        let results = self.results.get_or_insert_with(PollResults::default);
        let index = match results.answer_counts.iter().position(|c| c.id == answer_id) {
            Some(index) => index,
            None => {
                results.answer_counts.push(AnswerCount { id: answer_id, count: 0, me_voted: false });
                results.answer_counts.len() - 1
            }
        };
        let count = &mut results.answer_counts[index];
        if mine {
            if count.me_voted == add {
                return;
            }
            count.me_voted = add;
        }
        count.count = if add { count.count + 1 } else { count.count.saturating_sub(1) };
    }
}

/// MESSAGE_POLL_VOTE_ADD / _REMOVE.
#[derive(Debug, Clone, Deserialize)]
pub struct PollVote {
    pub user_id: String,
    pub channel_id: String,
    pub message_id: String,
    pub answer_id: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StickerItem {
    pub id: String,
    pub name: String,
    /// 1 PNG, 2 APNG, 3 Lottie, 4 GIF.
    #[serde(default)]
    pub format_type: u8,
}

impl StickerItem {
    /// A still image of the sticker; Lottie stickers have none we can draw.
    pub fn image_url(&self) -> Option<String> {
        let valid = !self.id.is_empty() && self.id.bytes().all(|b| b.is_ascii_digit());
        (valid && self.format_type != 3)
            .then(|| format!("https://media.discordapp.net/stickers/{}.png?size=160&passthrough=false", self.id))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SelectOption {
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UnfurledMedia {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GalleryItem {
    #[serde(default)]
    pub media: UnfurledMedia,
}

/// One message component. Kinds: 1 row, 2 button, 3 string select, 5–8 other
/// selects, 9 section, 10 text, 11 thumbnail, 12 gallery, 14 separator, 17 container.
#[derive(Debug, Clone, Deserialize)]
pub struct Component {
    #[serde(rename = "type")]
    pub kind: u8,
    #[serde(default)]
    pub components: Vec<Component>,
    /// Buttons: 1 primary, 2 secondary, 3 success, 4 danger, 5 link, 6 premium.
    #[serde(default)]
    pub style: Option<u8>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub emoji: Option<ReactionEmoji>,
    #[serde(default)]
    pub custom_id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub options: Vec<SelectOption>,
    /// Text display (10).
    #[serde(default)]
    pub content: Option<String>,
    /// Section (9): a button or thumbnail beside the text.
    #[serde(default)]
    pub accessory: Option<Box<Component>>,
    /// Thumbnail (11).
    #[serde(default)]
    pub media: Option<UnfurledMedia>,
    /// Media gallery (12).
    #[serde(default)]
    pub items: Vec<GalleryItem>,
    #[serde(default)]
    pub accent_color: Option<u32>,
    #[serde(default)]
    pub divider: Option<bool>,
}

impl Component {
    /// Children in drawing order (accessory last), for walking text displays.
    pub fn children(&self) -> impl Iterator<Item = &Component> {
        self.components.iter().chain(self.accessory.as_deref())
    }
}

/// An application command option (or subcommand) as the command index sends it.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandOption {
    /// 1 subcommand, 2 group, 3 string, 4 integer, 5 boolean, 6 user, 7 channel,
    /// 8 role, 9 mentionable, 10 number, 11 attachment.
    #[serde(rename = "type")]
    pub kind: u8,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub options: Vec<CommandOption>,
}

/// A slash command available in a channel.
#[derive(Debug, Clone)]
pub struct Command {
    pub id: String,
    pub application_id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub options: Vec<CommandOption>,
    /// The app's name, for the suggestion list.
    pub app_name: String,
    /// The command as received; interactions send it back.
    pub raw: Value,
}

/// THREAD_DELETE.
#[derive(Debug, Clone, Deserialize)]
pub struct ThreadDelete {
    pub id: String,
    #[serde(default)]
    pub guild_id: Option<String>,
}

/// THREAD_LIST_SYNC: the active threads of (some channels of) a guild.
#[derive(Debug, Clone, Deserialize)]
pub struct ThreadListSync {
    pub guild_id: String,
    #[serde(default)]
    pub threads: Vec<Channel>,
}

/// GUILD_STICKERS_UPDATE, and the guild's own list.
#[derive(Debug, Clone, Deserialize)]
pub struct Sticker {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub format_type: u8,
    #[serde(default = "available")]
    pub available: bool,
}

impl Sticker {
    pub fn item(&self) -> StickerItem {
        StickerItem { id: self.id.clone(), name: self.name.clone(), format_type: self.format_type }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageDelete {
    pub id: String,
    pub channel_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TypingMember {
    pub user: User,
    #[serde(default)]
    pub nick: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TypingStart {
    pub channel_id: String,
    pub user_id: String,
    /// Present in guilds only.
    #[serde(default)]
    pub member: Option<TypingMember>,
}

/// Who is in which voice channel. `channel_id: None` means they left.
#[derive(Debug, Clone, Deserialize)]
pub struct VoiceState {
    /// Absent inside a guild's own `voice_states` list.
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
    pub user_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub self_mute: bool,
    #[serde(default)]
    pub self_deaf: bool,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub deaf: bool,
    /// Camera on.
    #[serde(default)]
    pub self_video: bool,
    /// Streaming (Go Live).
    #[serde(default)]
    pub self_stream: bool,
    #[serde(default)]
    pub member: Option<TypingMember>,
}

/// STREAM_CREATE / STREAM_UPDATE: a Go Live stream we watch got its server.
#[derive(Debug, Clone, Deserialize)]
pub struct StreamCreate {
    pub stream_key: String,
    pub rtc_server_id: String,
    /// The stream's own channel id when Discord sends it (its DAVE group).
    #[serde(default)]
    pub rtc_channel_id: Option<String>,
}

/// STREAM_SERVER_UPDATE: where to connect for a stream.
#[derive(Debug, Clone, Deserialize)]
pub struct StreamServer {
    pub stream_key: String,
    pub token: String,
    #[serde(default)]
    pub endpoint: Option<String>,
}

/// `guild:<guild>:<channel>:<user>` (the only kind Ferrochat watches).
pub fn stream_key(guild_id: &str, channel_id: &str, user_id: &str) -> String {
    format!("guild:{guild_id}:{channel_id}:{user_id}")
}

/// VOICE_SERVER_UPDATE. `endpoint: None` means the server is still being allocated.
#[derive(Debug, Clone, Deserialize)]
pub struct VoiceServer {
    pub token: String,
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Overwrite {
    pub id: String,
    /// 0 = role, 1 = member.
    #[serde(rename = "type")]
    pub kind: u8,
    #[serde(deserialize_with = "u64_any")]
    pub allow: u64,
    #[serde(deserialize_with = "u64_any")]
    pub deny: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: u8,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub position: i32,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub permission_overwrites: Vec<Overwrite>,
    #[serde(default)]
    pub recipients: Vec<User>,
    #[serde(default)]
    pub recipient_ids: Vec<String>,
    #[serde(default)]
    pub last_message_id: Option<String>,
    /// Sent on threads (and channels from REST).
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default)]
    pub thread_metadata: Option<ThreadMetadata>,
    /// Threads: messages sent (approximate).
    #[serde(default)]
    pub message_count: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ThreadMetadata {
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub locked: bool,
}

impl Channel {
    /// GUILD_TEXT (0) and GUILD_ANNOUNCEMENT (5).
    pub fn is_text(&self) -> bool {
        matches!(self.kind, 0 | 5)
    }

    /// Forum (15) and media (16) channels: a list of posts, each a thread.
    pub fn is_forum(&self) -> bool {
        matches!(self.kind, 15 | 16)
    }

    /// Announcement (10), public (11) and private (12) threads, forum posts included.
    pub fn is_thread(&self) -> bool {
        matches!(self.kind, 10..=12)
    }

    /// Text you can read and send: channels, threads and DMs (everything but voice,
    /// categories and forums).
    pub fn has_messages(&self) -> bool {
        !(self.is_voice() || self.is_category() || self.is_forum())
    }

    pub fn is_archived(&self) -> bool {
        self.thread_metadata.as_ref().is_some_and(|m| m.archived)
    }

    pub fn is_category(&self) -> bool {
        self.kind == 4
    }

    /// GUILD_VOICE (2). Stage channels are left out.
    pub fn is_voice(&self) -> bool {
        self.kind == 2
    }

    pub fn label(&self) -> String {
        match self.name.as_deref() {
            Some(name) if !name.is_empty() => name.to_owned(),
            _ if self.recipients.is_empty() => "Unknown".to_owned(),
            _ => self
                .recipients
                .iter()
                .map(User::display_name)
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Role {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// 0 means "no color".
    #[serde(default)]
    pub color: u32,
    #[serde(deserialize_with = "u64_any")]
    pub permissions: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CustomEmoji {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub animated: bool,
}

impl CustomEmoji {
    /// Message syntax: `<:name:id>` or `<a:name:id>`.
    pub fn token(&self) -> String {
        format!("<{}:{}:{}>", if self.animated { "a" } else { "" }, self.name, self.id)
    }
}

#[derive(Deserialize)]
struct RawEmoji {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    animated: bool,
    #[serde(default = "available")]
    available: bool,
}

fn available() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
struct UserRef {
    id: String,
}

/// Member as sent by the gateway: bots get `user`, user sessions get `user_id`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawMember {
    #[serde(default)]
    user: Option<UserRef>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    roles: Vec<String>,
}

impl RawMember {
    fn id(&self) -> Option<&str> {
        self.user.as_ref().map(|u| u.id.as_str()).or(self.user_id.as_deref())
    }
}

#[derive(Debug, Clone)]
pub struct Guild {
    pub id: String,
    pub name: String,
    /// Icon hash; `None` shows initials.
    pub icon: Option<String>,
    pub owner_id: Option<String>,
    pub roles: Vec<Role>,
    /// Usable custom emoji.
    pub emojis: Vec<CustomEmoji>,
    /// Categories and viewable text and voice channels, ordered by position.
    pub channels: Vec<Channel>,
    /// Members currently in voice channels.
    pub voice_states: Vec<VoiceState>,
    /// Usable server stickers.
    pub stickers: Vec<Sticker>,
    /// `None` when our membership was not sent; permissions are then unknown.
    pub my_roles: Option<Vec<String>>,
    /// The server's notification default for members who haven't chosen.
    pub default_notifications: NotifyLevel,
    /// Members as received, consumed by [`Guild::finish`].
    members: Vec<RawMember>,
}

impl Guild {
    /// Resolves our roles and hides channels we cannot view. Call once our user id is known.
    pub fn finish(&mut self, my_id: &str) {
        if let Some(member) = self.members.iter().find(|m| m.id() == Some(my_id)) {
            self.my_roles = Some(member.roles.clone());
        }
        self.members = Vec::new();
        permissions::hide_unviewable(self, my_id);
    }
}

#[derive(Default, Deserialize)]
struct GuildProperties {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    owner_id: Option<String>,
    #[serde(default)]
    default_message_notifications: Option<u8>,
}

/// Guild as sent by the gateway: user sessions may nest fields under `properties`.
#[derive(Deserialize)]
pub struct RawGuild {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    owner_id: Option<String>,
    #[serde(default)]
    default_message_notifications: Option<u8>,
    #[serde(default)]
    properties: Option<GuildProperties>,
    #[serde(default)]
    channels: Vec<Channel>,
    #[serde(default)]
    roles: Vec<Role>,
    #[serde(default)]
    emojis: Vec<RawEmoji>,
    #[serde(default)]
    voice_states: Vec<VoiceState>,
    #[serde(default)]
    members: Vec<RawMember>,
    /// Active threads (user sessions get the ones they can see).
    #[serde(default)]
    threads: Vec<Channel>,
    #[serde(default)]
    stickers: Vec<Sticker>,
    #[serde(default)]
    unavailable: bool,
}

impl From<RawGuild> for Guild {
    fn from(raw: RawGuild) -> Self {
        let properties = raw.properties.unwrap_or_default();
        let mut channels: Vec<Channel> = raw
            .channels
            .into_iter()
            .chain(raw.threads)
            .filter(|c| c.is_text() || c.is_category() || c.is_voice() || c.is_forum() || c.is_thread())
            .collect();
        channels.sort_by_key(|c| (c.position, snowflake(&c.id)));
        Guild {
            id: raw.id,
            name: raw
                .name
                .or(properties.name)
                .unwrap_or_else(|| "Unknown server".to_owned()),
            icon: raw.icon.or(properties.icon),
            owner_id: raw.owner_id.or(properties.owner_id),
            roles: raw.roles,
            voice_states: raw.voice_states,
            stickers: raw.stickers.into_iter().filter(|s| s.available).collect(),
            emojis: raw
                .emojis
                .into_iter()
                .filter(|e| e.available)
                .filter_map(|e| Some(CustomEmoji { id: e.id?, name: e.name?, animated: e.animated }))
                .collect(),
            channels,
            my_roles: None,
            default_notifications: NotifyLevel::from_number(
                raw.default_message_notifications.or(properties.default_message_notifications).unwrap_or(0),
            ),
            members: raw.members,
        }
    }
}

/// Which messages notify. Discord's `message_notifications` numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyLevel {
    All,
    Mentions,
    Nothing,
    /// Channels: use the server's setting. Servers: use the server's default.
    Inherit,
}

impl NotifyLevel {
    pub const CHANNEL: [NotifyLevel; 4] =
        [NotifyLevel::Inherit, NotifyLevel::All, NotifyLevel::Mentions, NotifyLevel::Nothing];
    pub const SERVER: [NotifyLevel; 3] = [NotifyLevel::All, NotifyLevel::Mentions, NotifyLevel::Nothing];

    pub fn from_number(n: u8) -> Self {
        match n {
            0 => NotifyLevel::All,
            1 => NotifyLevel::Mentions,
            2 => NotifyLevel::Nothing,
            _ => NotifyLevel::Inherit,
        }
    }

    pub fn number(self) -> u8 {
        match self {
            NotifyLevel::All => 0,
            NotifyLevel::Mentions => 1,
            NotifyLevel::Nothing => 2,
            NotifyLevel::Inherit => 3,
        }
    }
}

impl std::fmt::Display for NotifyLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            NotifyLevel::All => "All messages",
            NotifyLevel::Mentions => "Only @mentions",
            NotifyLevel::Nothing => "Nothing",
            NotifyLevel::Inherit => "Server default",
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MuteConfig {
    /// `None`: until unmuted.
    #[serde(default)]
    pub end_time: Option<DateTime<FixedOffset>>,
}

fn muted_now(muted: bool, config: &Option<MuteConfig>) -> bool {
    let until = config.as_ref().and_then(|c| c.end_time);
    muted && until.is_none_or(|end| end > chrono::Utc::now())
}

fn notify_number() -> u8 {
    3
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelOverride {
    pub channel_id: String,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub mute_config: Option<MuteConfig>,
    #[serde(default = "notify_number")]
    pub message_notifications: u8,
}

impl ChannelOverride {
    pub fn is_muted(&self) -> bool {
        muted_now(self.muted, &self.mute_config)
    }
}

/// Your notification settings for one server (`guild_id: None` is direct messages).
#[derive(Debug, Clone, Deserialize)]
pub struct GuildSettings {
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub mute_config: Option<MuteConfig>,
    #[serde(default = "notify_number")]
    pub message_notifications: u8,
    /// A list in current payloads; older ones sent an object keyed by channel.
    #[serde(default, deserialize_with = "list_or_map")]
    pub channel_overrides: Vec<ChannelOverride>,
}

impl GuildSettings {
    pub fn new(guild_id: Option<String>) -> Self {
        Self { guild_id, muted: false, mute_config: None, message_notifications: 3, channel_overrides: Vec::new() }
    }

    pub fn is_muted(&self) -> bool {
        muted_now(self.muted, &self.mute_config)
    }

    pub fn channel(&self, channel_id: &str) -> Option<&ChannelOverride> {
        self.channel_overrides.iter().find(|o| o.channel_id == channel_id)
    }

    /// Our key for it: the guild id, or "" for direct messages.
    pub fn key(&self) -> String {
        self.guild_id.clone().unwrap_or_default()
    }
}

fn list_or_map<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<ChannelOverride>, D::Error> {
    let values = match Value::deserialize(deserializer)? {
        Value::Array(values) => values,
        Value::Object(map) => map.into_iter().map(|(_, v)| v).collect(),
        _ => Vec::new(),
    };
    Ok(values.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

/// A server folder from your Discord settings. Single servers outside any
/// folder come as folders without an id.
#[derive(Debug, Clone, Deserialize)]
pub struct GuildFolder {
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub color: Option<u32>,
    #[serde(default)]
    pub guild_ids: Vec<String>,
}

impl GuildFolder {
    /// A real folder (rather than one ungrouped server).
    pub fn is_folder(&self) -> bool {
        self.id.as_ref().is_some_and(|id| !id.is_null())
    }

    pub fn key(&self) -> String {
        match &self.id {
            Some(Value::String(id)) => id.clone(),
            Some(Value::Number(id)) => id.to_string(),
            _ => self.guild_ids.join(","),
        }
    }
}

/// The parts of Discord's (JSON) user settings we use, from READY and
/// USER_SETTINGS_UPDATE. Every field is optional because updates are partial.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserSettings {
    #[serde(default)]
    pub guild_folders: Option<Vec<GuildFolder>>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, deserialize_with = "custom_status_text")]
    pub custom_status: Option<Option<String>>,
}

/// `custom_status` is `null` (cleared) or `{ "text": ... }`; absent means unchanged.
fn custom_status_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(match Value::deserialize(deserializer)? {
        Value::Object(map) => map.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_owned),
        _ => None,
    }))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProfileMember {
    #[serde(default)]
    pub nick: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub joined_at: Option<DateTime<FixedOffset>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProfileText {
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub pronouns: Option<String>,
}

/// GET /users/{id}/profile (user logins).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub user_profile: Option<ProfileText>,
    #[serde(default)]
    pub guild_member: Option<ProfileMember>,
}

/// When a Discord id was created (account age, message time).
pub fn snowflake_time(id: &str) -> Option<DateTime<chrono::Utc>> {
    const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;
    let ms = i64::try_from(snowflake(id) >> 22).ok()? + DISCORD_EPOCH_MS;
    DateTime::from_timestamp_millis(ms)
}

#[derive(Debug, Clone, Copy)]
pub struct ReadState {
    pub last_message_id: u64,
    pub mention_count: u32,
}

#[derive(Debug, Clone)]
pub struct Ready {
    pub user: User,
    /// Interactions (buttons, slash commands) name the session they come from.
    pub session_id: String,
    pub guilds: Vec<Guild>,
    pub dms: Vec<Channel>,
    /// By channel id. Only user sessions receive read states.
    pub read_states: HashMap<String, ReadState>,
    /// Notification settings per server (user sessions).
    pub guild_settings: Vec<GuildSettings>,
    pub user_settings: UserSettings,
    pub relationships: Vec<Relationship>,
    /// Friends' statuses by user id.
    pub presences: HashMap<String, String>,
}

#[derive(Deserialize)]
pub struct RawReady {
    #[serde(default)]
    relationships: Vec<Relationship>,
    /// Friends' presences, without the deduplicating capability.
    #[serde(default)]
    presences: Vec<Value>,
    /// `{ friends: [...], guilds: [...] }` with it.
    #[serde(default)]
    merged_presences: Value,
    user: User,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    guilds: Vec<RawGuild>,
    #[serde(default)]
    private_channels: Vec<Channel>,
    /// User sessions send DM recipients as ids that refer to this list.
    #[serde(default)]
    users: Vec<User>,
    /// Our own member per guild, aligned with `guilds` (user sessions).
    #[serde(default)]
    merged_members: Vec<Vec<RawMember>>,
    /// Either a list of entries or `{ entries, version, partial }`.
    #[serde(default)]
    read_state: Value,
    /// Same shapes as `read_state`.
    #[serde(default)]
    user_guild_settings: Value,
    #[serde(default)]
    user_settings: Option<UserSettings>,
}

#[derive(Deserialize)]
struct RawReadState {
    id: String,
    #[serde(default, deserialize_with = "u64_any")]
    last_message_id: u64,
    #[serde(default)]
    mention_count: u32,
}

impl From<RawReady> for Ready {
    fn from(raw: RawReady) -> Self {
        let mut dms = raw.private_channels;
        for dm in &mut dms {
            if dm.recipients.is_empty() {
                dm.recipients = dm
                    .recipient_ids
                    .iter()
                    .filter_map(|id| raw.users.iter().find(|u| &u.id == id).cloned())
                    .collect();
            }
        }
        dms.sort_by_key(|c| std::cmp::Reverse(c.last_message_id.as_deref().map_or(0, snowflake)));

        let mut relationships = raw.relationships;
        for relationship in &mut relationships {
            if relationship.user.is_none() {
                relationship.user = raw.users.iter().find(|u| u.id == relationship.id).cloned();
            }
        }
        let friends = raw.merged_presences["friends"].as_array().cloned().unwrap_or_default();
        let presences = raw.presences.iter().chain(&friends).filter_map(presence_of).collect();

        let mut merged_members = raw.merged_members;
        let mut guilds = Vec::new();
        for (index, raw_guild) in raw.guilds.into_iter().enumerate() {
            if raw_guild.unavailable {
                continue;
            }
            let mut guild = Guild::from(raw_guild);
            if let Some(members) = merged_members.get_mut(index) {
                guild.members.append(members);
            }
            guild.finish(&raw.user.id);
            guilds.push(guild);
        }

        Ready {
            user: raw.user,
            session_id: raw.session_id,
            guilds,
            dms,
            read_states: read_states(raw.read_state),
            guild_settings: entries(raw.user_guild_settings)
                .into_iter()
                .filter_map(|entry| serde_json::from_value(entry).ok())
                .collect(),
            user_settings: raw.user_settings.unwrap_or_default(),
            relationships,
            presences,
        }
    }
}

/// A list of entries, or `{ entries, version, partial }`.
fn entries(value: Value) -> Vec<Value> {
    match value {
        Value::Array(entries) => entries,
        Value::Object(mut object) => match object.remove("entries") {
            Some(Value::Array(entries)) => entries,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Entries that fail to parse are skipped; their channels fall back to "read".
fn read_states(value: Value) -> HashMap<String, ReadState> {
    entries(value)
        .into_iter()
        .filter_map(|entry| serde_json::from_value::<RawReadState>(entry).ok())
        .map(|entry| {
            let state = ReadState { last_message_id: entry.last_message_id, mention_count: entry.mention_count };
            (entry.id, state)
        })
        .collect()
}

pub fn snowflake(id: &str) -> u64 {
    id.parse().unwrap_or(0)
}

/// Discord sends snowflakes and permission bitsets as strings, sometimes as numbers.
fn u64_any<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    use serde::de::Error;
    match Value::deserialize(deserializer)? {
        Value::Null => Ok(0),
        Value::Number(n) => n.as_u64().ok_or_else(|| D::Error::custom("expected an unsigned integer")),
        Value::String(s) => s.parse().map_err(D::Error::custom),
        other => Err(D::Error::custom(format!("expected an integer, got {other}"))),
    }
}

#[cfg(test)]
mod system_message_tests {
    use super::*;

    fn message(kind: u8, content: &str) -> Message {
        serde_json::from_value(serde_json::json!({
            "id": "1", "type": kind, "channel_id": "2", "content": content,
            "author": { "id": "3", "username": "sam" }, "timestamp": "2026-11-05T09:15:00+00:00",
        }))
        .unwrap()
    }

    #[test]
    fn system_messages_are_written_out() {
        assert_eq!(message(7, "").system_text().as_deref(), Some("sam joined the server."));
        assert_eq!(message(10, "").system_text().as_deref(), Some("sam boosted the server! It reached Level 2."));
        assert_eq!(message(18, "plans").system_text().as_deref(), Some("sam started a thread: plans"));
        assert!(message(7, "").is_system());
        assert!(!message(0, "hi").is_system() && message(0, "hi").system_text().is_none());
        assert!(!message(REPLY, "hi").is_system(), "replies are ordinary messages");
    }
}
