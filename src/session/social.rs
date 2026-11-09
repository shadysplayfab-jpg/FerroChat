//! Friends and the Home page, the mentions inbox, right-click menus, server
//! and invite dialogs, collapsed categories, blocked messages, and audio
//! playback. Most actions are one REST call answered by [`SocialMsg::Done`].

use std::collections::HashSet;

use iced::{Point, Size, Task};
use serde_json::{json, Value};

use super::Session;
use crate::discord::gateway::Event;
use crate::discord::model::{
    snowflake, Channel, Message, Relationship, User, RELATIONSHIP_BLOCKED, RELATIONSHIP_FRIEND, RELATIONSHIP_INCOMING,
    RELATIONSHIP_OUTGOING,
};
use crate::discord::permissions;
use crate::discord::rest::Method;
use crate::voice::player::{self, Playback};
use crate::Msg;

/// The Home page's tabs (Direct Messages with no channel open).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HomeTab {
    #[default]
    Online,
    All,
    Pending,
    Blocked,
    Add,
    Inbox,
}

impl HomeTab {
    pub const ALL: [HomeTab; 6] = [HomeTab::Online, HomeTab::All, HomeTab::Pending, HomeTab::Blocked, HomeTab::Add, HomeTab::Inbox];

    pub fn name(self) -> &'static str {
        match self {
            HomeTab::Online => "Online",
            HomeTab::All => "All",
            HomeTab::Pending => "Pending",
            HomeTab::Blocked => "Blocked",
            HomeTab::Add => "Add Friend",
            HomeTab::Inbox => "Inbox",
        }
    }
}

#[derive(Default)]
pub struct Home {
    pub tab: HomeTab,
    pub filter: String,
    pub add_input: String,
    /// The last friend request: what Discord said.
    pub add_result: Option<Result<String, String>>,
    /// Friends picked for a new group DM.
    pub group: HashSet<String>,
    /// Mentions of you; `None` until loaded.
    pub inbox: Option<Result<Vec<Message>, String>>,
}

/// What a right-click menu belongs to.
#[derive(Debug, Clone)]
pub enum MenuTarget {
    Message { channel_id: String, message_id: String },
    /// Someone, and the server you saw them in.
    User(User, Option<String>),
    Channel(String),
    Guild(String),
}

pub struct Menu {
    pub target: MenuTarget,
    pub at: Point,
}

/// A small form over the window.
pub struct Dialog {
    pub kind: DialogKind,
    pub input: String,
    pub second: String,
    pub flag: bool,
    pub busy: bool,
    pub error: Option<String>,
    /// Shown when done (the invite link).
    pub result: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DialogKind {
    JoinServer,
    CreateServer,
    Invite { channel_id: String },
    Nickname { guild_id: String },
    CreateChannel { guild_id: String, parent_id: Option<String> },
    EditChannel { channel_id: String },
    Forward { channel_id: String, message_id: String },
    LeaveServer { guild_id: String },
    DeleteChannel { channel_id: String },
    DeleteMessage { message_id: String },
}

impl DialogKind {
    pub fn title(&self) -> &'static str {
        match self {
            DialogKind::JoinServer => "Join a server",
            DialogKind::CreateServer => "Create a server",
            DialogKind::Invite { .. } => "Invite friends",
            DialogKind::Nickname { .. } => "Change nickname",
            DialogKind::CreateChannel { .. } => "Create channel",
            DialogKind::EditChannel { .. } => "Edit channel",
            DialogKind::Forward { .. } => "Forward to…",
            DialogKind::LeaveServer { .. } => "Leave server",
            DialogKind::DeleteChannel { .. } => "Delete channel",
            DialogKind::DeleteMessage { .. } => "Delete message",
        }
    }
}

/// Which request a [`SocialMsg::Done`] answers.
#[derive(Debug, Clone)]
pub enum Action {
    FriendRequest,
    Relationship,
    Group,
    Dialog,
    /// A menu action with no result to show beyond errors.
    Quiet(&'static str),
}

#[derive(Debug, Clone)]
pub enum SocialMsg {
    OpenHome,
    Tab(HomeTab),
    Filter(String),
    AddInput(String),
    SendFriendRequest,
    /// Sends a request to someone by id (from a menu or profile).
    AddFriend(String),
    Accept(String),
    /// Unfriend, decline, cancel or unblock.
    Remove(String),
    Block(String),
    PickForGroup(String),
    CreateGroup,
    InboxLoaded(Result<Vec<Message>, String>),
    /// Right-click position and window size, from the root widget.
    Pointer(Point, Size),
    WindowSize(Size),
    OpenMenu(MenuTarget),
    CloseMenu,
    /// A menu item: closes the menu, then does this.
    Pick(Box<Msg>),
    Copy(String),
    MarkUnread { channel_id: String, message_id: String },
    MarkRead(Vec<String>),
    Pin { channel_id: String, message_id: String, pin: bool },
    CloseDm(String),
    OpenDialog(DialogKind),
    DialogInput(String),
    DialogSecond(String),
    DialogFlag(bool),
    SubmitDialog,
    ForwardTo(String),
    CloseDialog,
    ToggleCategory(String),
    ShowBlocked(String),
    Play { url: String, name: String },
    AudioLoaded { url: String, name: String, result: Result<Vec<u8>, String> },
    StopAudio,
    PlayerTick,
    Done { action: Action, result: Result<Value, String> },
}

const CAPTCHA_HINT: &str = "Discord may want a captcha for this; open it in your browser instead.";

impl Session {
    pub(super) fn social(&mut self, msg: SocialMsg) -> Task<Msg> {
        match msg {
            SocialMsg::OpenHome => return self.social(SocialMsg::Tab(self.home.tab)),
            SocialMsg::Tab(tab) => {
                self.home.tab = tab;
                if self.selected_guild.is_some() || self.selected_channel.is_some() {
                    self.select_guild(None);
                    self.panel = None;
                }
                let inbox = if tab == HomeTab::Inbox { self.load_inbox() } else { Task::none() };
                return Task::batch([inbox, self.fetch_media()]);
            }
            SocialMsg::Filter(text) => self.home.filter = text,
            SocialMsg::AddInput(text) => {
                self.home.add_input = text;
                self.home.add_result = None;
            }
            SocialMsg::SendFriendRequest => {
                let username = self.home.add_input.trim().trim_start_matches('@').to_lowercase();
                if username.is_empty() {
                    return Task::none();
                }
                let body = json!({ "username": username, "discriminator": null });
                return self.api(Action::FriendRequest, Method::POST, "/users/@me/relationships".to_owned(), Some(body));
            }
            SocialMsg::AddFriend(id) | SocialMsg::Accept(id) => {
                self.menu = None;
                let path = format!("/users/@me/relationships/{id}");
                return self.api(Action::Relationship, Method::PUT, path, Some(json!({})));
            }
            SocialMsg::Remove(id) => {
                self.menu = None;
                return self.api(Action::Relationship, Method::DELETE, format!("/users/@me/relationships/{id}"), None);
            }
            SocialMsg::Block(id) => {
                self.menu = None;
                let body = json!({ "type": RELATIONSHIP_BLOCKED });
                return self.api(Action::Relationship, Method::PUT, format!("/users/@me/relationships/{id}"), Some(body));
            }
            SocialMsg::PickForGroup(id) => {
                if !self.home.group.remove(&id) && self.home.group.len() < 9 {
                    self.home.group.insert(id);
                }
            }
            SocialMsg::CreateGroup => {
                let recipients: Vec<String> = self.home.group.drain().collect();
                if recipients.is_empty() {
                    return Task::none();
                }
                let body = json!({ "recipients": recipients });
                return self.api(Action::Group, Method::POST, "/users/@me/channels".to_owned(), Some(body));
            }
            SocialMsg::InboxLoaded(result) => self.home.inbox = Some(result),
            SocialMsg::Pointer(at, window) => {
                self.pointer = at;
                self.window = window;
            }
            SocialMsg::WindowSize(window) => self.window = window,
            SocialMsg::OpenMenu(target) => {
                self.menu = Some(super::Menu { target, at: self.pointer });
                self.reacting_to = None;
            }
            SocialMsg::CloseMenu => self.menu = None,
            SocialMsg::Pick(msg) => {
                self.menu = None;
                return self.update(*msg);
            }
            SocialMsg::Copy(text) => {
                self.menu = None;
                return iced::clipboard::write(text);
            }
            SocialMsg::MarkUnread { channel_id, message_id } => {
                self.menu = None;
                // Read up to the message before it.
                let before = snowflake(&message_id).saturating_sub(1);
                self.read.insert(channel_id.clone(), before);
                if self.is_bot || self.offline {
                    return Task::none();
                }
                let body = json!({ "manual": true, "mention_count": 0 });
                let path = format!("/channels/{channel_id}/messages/{before}/ack");
                if self.selected_channel.as_deref() == Some(channel_id.as_str()) {
                    // Leave it, or opening it again would mark it read straight away.
                    self.selected_channel = None;
                }
                return self.api(Action::Quiet("Mark unread"), Method::POST, path, Some(body));
            }
            SocialMsg::MarkRead(channels) => {
                self.menu = None;
                let mut states = Vec::new();
                for channel_id in channels {
                    self.mentions.remove(&channel_id);
                    let Some(&latest) = self.latest.get(&channel_id).filter(|&&id| id > 0) else { continue };
                    if self.read.insert(channel_id.clone(), latest).is_some_and(|read| read >= latest) {
                        continue;
                    }
                    states.push(json!({ "channel_id": channel_id, "message_id": latest.to_string(), "read_state_type": 0 }));
                }
                if states.is_empty() || self.is_bot || self.offline || !self.settings.sync_read_state {
                    return Task::none();
                }
                let body = json!({ "read_states": states });
                return self.api(Action::Quiet("Mark as read"), Method::POST, "/read-states/ack-bulk".to_owned(), Some(body));
            }
            SocialMsg::Pin { channel_id, message_id, pin } => {
                self.menu = None;
                let method = if pin { Method::PUT } else { Method::DELETE };
                return self.api(Action::Quiet("Pin"), method, format!("/channels/{channel_id}/pins/{message_id}"), None);
            }
            SocialMsg::CloseDm(channel_id) => {
                self.menu = None;
                return self.api(Action::Quiet("Close DM"), Method::DELETE, format!("/channels/{channel_id}"), None);
            }
            SocialMsg::OpenDialog(kind) => return self.open_dialog(kind),
            SocialMsg::DialogInput(text) => {
                if let Some(dialog) = &mut self.dialog {
                    dialog.input = text;
                    dialog.error = None;
                }
            }
            SocialMsg::DialogSecond(text) => {
                if let Some(dialog) = &mut self.dialog {
                    dialog.second = text;
                }
            }
            SocialMsg::DialogFlag(flag) => {
                if let Some(dialog) = &mut self.dialog {
                    dialog.flag = flag;
                }
            }
            SocialMsg::SubmitDialog => return self.submit_dialog(),
            SocialMsg::ForwardTo(target) => {
                let Some(dialog) = &mut self.dialog else { return Task::none() };
                let DialogKind::Forward { channel_id, message_id } = dialog.kind.clone() else { return Task::none() };
                dialog.busy = true;
                let guild_id = self.guild_of(&channel_id).map(|g| g.id.clone());
                // Type 1: a forward, which copies the message as a snapshot.
                let body = json!({
                    "message_reference": { "type": 1, "message_id": message_id, "channel_id": channel_id, "guild_id": guild_id },
                });
                return self.api(Action::Dialog, Method::POST, format!("/channels/{target}/messages"), Some(body));
            }
            SocialMsg::CloseDialog => self.dialog = None,
            SocialMsg::ToggleCategory(id) => {
                if !self.collapsed.remove(&id) {
                    self.collapsed.insert(id);
                }
                self.menu = None;
            }
            SocialMsg::ShowBlocked(message_id) => {
                self.revealed.insert((message_id, u16::MAX));
            }
            SocialMsg::Play { url, name } => {
                self.playing = None;
                self.loading_audio = Some(url.clone());
                return Task::perform(self.rest.clone().audio(url.clone()), move |result| {
                    Msg::Social(SocialMsg::AudioLoaded { url: url.clone(), name: name.clone(), result })
                });
            }
            SocialMsg::AudioLoaded { url, name, result } => {
                if self.loading_audio.as_deref() != Some(url.as_str()) {
                    return Task::none(); // something else was played since
                }
                self.loading_audio = None;
                match result {
                    Ok(bytes) => {
                        let (output, volume) = (self.settings.voice_output_device.clone(), self.settings.voice_levels().1);
                        self.playing = Some(player::play(url, &name, bytes, output, volume));
                    }
                    Err(error) => self.status = format!("Could not play {name}: {error}"),
                }
            }
            SocialMsg::StopAudio => {
                self.playing = None;
                self.loading_audio = None;
            }
            SocialMsg::PlayerTick => {
                if let Some(playback) = self.playing.as_ref().filter(|p| p.is_finished()) {
                    if let Some(error) = playback.error() {
                        self.status = format!("Playback: {error}");
                    }
                    self.playing = None;
                }
            }
            SocialMsg::Done { action, result } => return self.on_done(action, result),
        }
        Task::none()
    }

    fn api(&self, action: Action, method: Method, path: String, body: Option<Value>) -> Task<Msg> {
        if self.offline {
            return Task::none();
        }
        Task::perform(self.rest.clone().call(method, path, body), move |result| {
            Msg::Social(SocialMsg::Done { action: action.clone(), result })
        })
    }

    fn load_inbox(&mut self) -> Task<Msg> {
        if self.offline || self.is_bot {
            if self.home.inbox.is_none() {
                self.home.inbox = Some(Err("The inbox needs a user login.".to_owned()));
            }
            return Task::none();
        }
        self.home.inbox = None;
        Task::perform(self.rest.clone().mentions(), |result| Msg::Social(SocialMsg::InboxLoaded(result)))
    }

    fn open_dialog(&mut self, kind: DialogKind) -> Task<Msg> {
        self.menu = None;
        let input = match &kind {
            DialogKind::EditChannel { channel_id } => self.channel(channel_id).map(Channel::label).unwrap_or_default(),
            _ => String::new(),
        };
        let second = match &kind {
            DialogKind::EditChannel { channel_id } => self.channel(channel_id).and_then(|c| c.topic.clone()).unwrap_or_default(),
            _ => String::new(),
        };
        let invite = match &kind {
            DialogKind::Invite { channel_id } => Some(channel_id.clone()),
            _ => None,
        };
        self.dialog = Some(super::Dialog { kind, input, second, flag: false, busy: invite.is_some(), error: None, result: None });
        // Invites are made as soon as the dialog opens: one day, unlimited uses.
        let request = match invite {
            Some(channel_id) => {
                let body = json!({ "max_age": 86_400, "max_uses": 0, "temporary": false });
                self.api(Action::Dialog, Method::POST, format!("/channels/{channel_id}/invites"), Some(body))
            }
            None => Task::none(),
        };
        // Ready to type, like Discord's dialogs.
        Task::batch([request, iced::widget::text_input::focus(iced::widget::text_input::Id::new(super::DIALOG_INPUT))])
    }

    fn submit_dialog(&mut self) -> Task<Msg> {
        let Some(dialog) = &mut self.dialog else { return Task::none() };
        if dialog.busy {
            return Task::none();
        }
        if let DialogKind::DeleteMessage { message_id } = &dialog.kind {
            // The existing delete path, already confirmed here.
            let message_id = message_id.clone();
            self.dialog = None;
            self.confirm_delete = Some(message_id.clone());
            return self.update(Msg::DeleteMessage(message_id));
        }
        let input = dialog.input.trim().to_owned();
        let (method, path, body) = match &dialog.kind {
            DialogKind::JoinServer => {
                let Some(code) = invite_code(&input) else {
                    dialog.error = Some("Paste an invite link like discord.gg/abc123.".to_owned());
                    return Task::none();
                };
                (Method::POST, format!("/invites/{code}"), Some(json!({})))
            }
            DialogKind::CreateServer if !input.is_empty() => (Method::POST, "/guilds".to_owned(), Some(json!({ "name": input }))),
            DialogKind::Nickname { guild_id } => {
                (Method::PATCH, format!("/guilds/{guild_id}/members/@me"), Some(json!({ "nick": input })))
            }
            DialogKind::CreateChannel { guild_id, parent_id } if !input.is_empty() => {
                let kind = if dialog.flag { 2 } else { 0 };
                let body = json!({ "name": input, "type": kind, "parent_id": parent_id });
                (Method::POST, format!("/guilds/{guild_id}/channels"), Some(body))
            }
            DialogKind::EditChannel { channel_id } if !input.is_empty() => {
                let body = json!({ "name": input, "topic": dialog.second.trim() });
                (Method::PATCH, format!("/channels/{channel_id}"), Some(body))
            }
            DialogKind::LeaveServer { guild_id } => {
                (Method::DELETE, format!("/users/@me/guilds/{guild_id}"), Some(json!({ "lurking": false })))
            }
            DialogKind::DeleteChannel { channel_id } => (Method::DELETE, format!("/channels/{channel_id}"), None),
            // Invites, forwards and empty names have nothing to submit.
            _ => return Task::none(),
        };
        dialog.busy = true;
        self.api(Action::Dialog, method, path, body)
    }

    fn on_done(&mut self, action: Action, result: Result<Value, String>) -> Task<Msg> {
        match action {
            Action::FriendRequest => {
                self.home.add_result = Some(match result {
                    Ok(_) => Ok(format!("Friend request sent to {}.", self.home.add_input.trim())),
                    Err(error) => Err(format!("{error}. Check the username; {}", CAPTCHA_HINT.to_lowercase())),
                });
                if matches!(self.home.add_result, Some(Ok(_))) {
                    self.home.add_input.clear();
                }
            }
            Action::Relationship | Action::Quiet(_) => {
                if let Err(error) = result {
                    let what = if let Action::Quiet(what) = action { what } else { "Friends" };
                    self.status = format!("{what}: {error}");
                }
            }
            Action::Group => match result.and_then(|v| serde_json::from_value::<Channel>(v).map_err(|e| e.to_string())) {
                Ok(channel) => return self.update(Msg::DmOpened(Ok(channel))),
                Err(error) => self.status = format!("Could not create the group: {error}"),
            },
            Action::Dialog => return self.dialog_done(result),
        }
        Task::none()
    }

    fn dialog_done(&mut self, result: Result<Value, String>) -> Task<Msg> {
        let Some(dialog) = &mut self.dialog else { return Task::none() };
        dialog.busy = false;
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                let captcha = matches!(dialog.kind, DialogKind::JoinServer | DialogKind::CreateServer);
                dialog.error = Some(if captcha { format!("{error}. {CAPTCHA_HINT}") } else { error });
                return Task::none();
            }
        };
        match dialog.kind.clone() {
            DialogKind::Invite { .. } => {
                dialog.result = value["code"].as_str().map(|code| format!("https://discord.gg/{code}"));
                if dialog.result.is_none() {
                    dialog.error = Some("Discord didn't return an invite.".to_owned());
                }
                return Task::none();
            }
            DialogKind::JoinServer => {
                // The server itself arrives as GUILD_CREATE; open it when it's known.
                self.dialog = None;
                if let Some(guild_id) = value["guild"]["id"].as_str().or(value["guild_id"].as_str()) {
                    if self.guilds.iter().any(|g| g.id == guild_id) {
                        self.select_guild(Some(guild_id.to_owned()));
                    } else {
                        self.pending_guild = Some(guild_id.to_owned());
                    }
                }
            }
            DialogKind::CreateServer => {
                self.dialog = None;
                self.pending_guild = value["id"].as_str().map(str::to_owned);
            }
            DialogKind::Forward { .. } => {
                self.dialog = None;
                self.status = "Forwarded.".to_owned();
            }
            DialogKind::CreateChannel { .. } => {
                self.dialog = None;
                if let Ok(channel) = serde_json::from_value::<Channel>(value) {
                    let id = channel.id.clone();
                    self.upsert_channel(channel);
                    return self.open(id);
                }
            }
            _ => self.dialog = None,
        }
        Task::none()
    }

    /// Gateway events for friends, presences and channel lists.
    pub(super) fn on_social_event(&mut self, event: Event) -> Task<Msg> {
        match event {
            Event::Relationship(mut relationship) => {
                match self.relationships.iter_mut().find(|r| r.id == relationship.id) {
                    Some(existing) => {
                        if relationship.user.is_none() {
                            relationship.user = existing.user.take();
                        }
                        *existing = relationship;
                    }
                    None => self.relationships.push(relationship),
                }
            }
            Event::RelationshipRemove(id) => {
                self.relationships.retain(|r| r.id != id);
                self.presences.remove(&id);
            }
            Event::Presence(user_id, status) => {
                if self.relationships.iter().any(|r| r.id == user_id) {
                    self.presences.insert(user_id, status);
                }
            }
            Event::ChannelUpsert(channel) => self.upsert_channel(channel),
            Event::ChannelDelete(channel) => {
                self.dms.retain(|c| c.id != channel.id);
                for guild in &mut self.guilds {
                    guild.channels.retain(|c| c.id != channel.id);
                }
                self.logs.remove(&channel.id);
                if self.selected_channel.as_deref() == Some(channel.id.as_str()) {
                    self.selected_channel = None;
                }
            }
            Event::GuildDelete(guild_id) => {
                self.guilds.retain(|g| g.id != guild_id);
                self.subscribed.remove(&guild_id);
                if self.selected_guild.as_deref() == Some(guild_id.as_str()) {
                    self.select_guild(None);
                }
            }
            Event::GuildCreate(_) => {}
            _ => {}
        }
        Task::none()
    }

    /// After GUILD_CREATE: opens a server you just joined or made.
    pub(super) fn guild_arrived(&mut self, guild_id: &str) {
        if self.pending_guild.as_deref() == Some(guild_id) {
            self.pending_guild = None;
            self.select_guild(Some(guild_id.to_owned()));
        }
    }

    fn upsert_channel(&mut self, mut channel: Channel) {
        let latest = channel.last_message_id.as_deref().map_or(0, snowflake);
        self.latest.entry(channel.id.clone()).or_insert(latest);
        self.read.entry(channel.id.clone()).or_insert(latest);
        if matches!(channel.kind, 1 | 3) {
            if let Some(existing) = self.dms.iter_mut().find(|c| c.id == channel.id) {
                if channel.recipients.is_empty() {
                    channel.recipients = std::mem::take(&mut existing.recipients);
                }
                *existing = channel;
            } else {
                self.dms.insert(0, channel);
            }
            return;
        }
        if channel.is_thread() {
            return self.upsert_thread(channel);
        }
        let Some(guild_id) = channel.guild_id.clone() else { return };
        let my_id = self.me.as_ref().map(|me| me.id.clone()).unwrap_or_default();
        let Some(guild) = self.guilds.iter_mut().find(|g| g.id == guild_id) else { return };
        match guild.channels.iter_mut().find(|c| c.id == channel.id) {
            Some(existing) => *existing = channel,
            None => guild.channels.push(channel),
        }
        guild.channels.sort_by_key(|c| (c.position, snowflake(&c.id)));
        permissions::hide_unviewable(guild, &my_id);
        self.channels_changed();
    }

    pub fn friends(&self, tab: HomeTab) -> Vec<&Relationship> {
        let filter = self.home.filter.trim().to_lowercase();
        let mut list: Vec<&Relationship> = self
            .relationships
            .iter()
            .filter(|r| match tab {
                HomeTab::Online => r.kind == RELATIONSHIP_FRIEND && self.friend_status(&r.id) != "offline",
                HomeTab::All => r.kind == RELATIONSHIP_FRIEND,
                HomeTab::Pending => matches!(r.kind, RELATIONSHIP_INCOMING | RELATIONSHIP_OUTGOING),
                HomeTab::Blocked => r.kind == RELATIONSHIP_BLOCKED,
                HomeTab::Add | HomeTab::Inbox => false,
            })
            .filter(|r| {
                filter.is_empty()
                    || r.name().to_lowercase().contains(&filter)
                    || r.user.as_ref().is_some_and(|u| u.username.to_lowercase().contains(&filter))
            })
            .collect();
        list.sort_by_key(|r| (r.kind != RELATIONSHIP_INCOMING, r.name().to_lowercase()));
        list
    }

    /// online / idle / dnd / offline (invisible friends show offline).
    pub fn friend_status(&self, user_id: &str) -> &str {
        match self.presences.get(user_id).map(String::as_str) {
            Some("invisible") | None => "offline",
            Some(status) => status,
        }
    }

    pub fn relationship(&self, user_id: &str) -> Option<u8> {
        self.relationships.iter().find(|r| r.id == user_id).map(|r| r.kind)
    }

    pub fn incoming_requests(&self) -> usize {
        self.relationships.iter().filter(|r| r.kind == RELATIONSHIP_INCOMING).count()
    }

    pub fn is_blocked(&self, user_id: &str) -> bool {
        self.relationship(user_id) == Some(RELATIONSHIP_BLOCKED)
    }

    /// Blocked authors' messages stay folded until clicked.
    pub fn hides_blocked(&self, message: &Message) -> bool {
        self.is_blocked(&message.author.id) && !self.revealed.contains(&(message.id.clone(), u16::MAX))
    }

    pub fn can_in_guild(&self, guild_id: &str, permission: u64) -> bool {
        let my_id = self.me.as_ref().map_or("", |me| me.id.as_str());
        self.guilds
            .iter()
            .find(|g| g.id == guild_id)
            .and_then(|g| permissions::guild_permissions(g, my_id))
            .is_some_and(|p| p & permission == permission)
    }

    pub fn can_in_channel(&self, channel_id: &str, permission: u64) -> bool {
        let my_id = self.me.as_ref().map_or("", |me| me.id.as_str());
        let (Some(guild), Some(channel)) = (self.guild_of(channel_id), self.channel(channel_id)) else { return false };
        permissions::channel_permissions(guild, channel, my_id).is_some_and(|p| p & permission == permission)
    }

    /// Whether you may write in a channel. DMs, and servers whose roles weren't
    /// sent, count as allowed (Discord still has the last word when sending).
    pub fn can_send(&self, channel_id: &str) -> bool {
        // The demo's owner may do anything; FERROCHAT_DEMO_READONLY shows the locked box.
        #[cfg(feature = "demo")]
        if self.offline && std::env::var_os("FERROCHAT_DEMO_READONLY").is_some() {
            return false;
        }
        let my_id = self.me.as_ref().map_or("", |me| me.id.as_str());
        let (Some(guild), Some(channel)) = (self.guild_of(channel_id), self.channel(channel_id)) else { return true };
        let needed = if channel.is_thread() { permissions::SEND_MESSAGES_IN_THREADS } else { permissions::SEND_MESSAGES };
        let locked = channel.is_thread() && channel.thread_metadata.as_ref().is_some_and(|m| m.locked);
        match permissions::channel_permissions(guild, channel, my_id) {
            Some(granted) => granted & needed != 0 && !(locked && granted & permissions::MANAGE_THREADS == 0),
            None => true,
        }
    }

    /// Uploads (and pasted images) need Attach Files in servers.
    pub fn can_attach(&self, channel_id: &str) -> bool {
        self.guild_of(channel_id).is_none() || !self.has_known_permissions(channel_id) || self.can_in_channel(channel_id, permissions::ATTACH_FILES)
    }

    fn has_known_permissions(&self, channel_id: &str) -> bool {
        let my_id = self.me.as_ref().map_or("", |me| me.id.as_str());
        let (Some(guild), Some(channel)) = (self.guild_of(channel_id), self.channel(channel_id)) else { return false };
        permissions::channel_permissions(guild, channel, my_id).is_some()
    }

    /// Text channels of a server, for "Mark as read".
    pub fn guild_text_channels(&self, guild_id: &str) -> Vec<String> {
        self.guilds
            .iter()
            .filter(|g| g.id == guild_id)
            .flat_map(|g| &g.channels)
            .filter(|c| c.has_messages())
            .map(|c| c.id.clone())
            .collect()
    }

    pub fn playback(&self) -> Option<&Playback> {
        self.playing.as_ref()
    }

    pub fn is_loading_audio(&self, url: &str) -> bool {
        self.loading_audio.as_deref() == Some(url)
    }

    /// Demo: friends, a request and a block, without a network.
    #[cfg(feature = "demo")]
    pub(super) fn demo_social(&mut self) {
        let user = |id: &str, name: &str| User {
            id: id.to_owned(),
            username: name.to_owned(),
            global_name: None,
            avatar: None,
            bot: false,
        };
        let relation = |id: &str, name: &str, kind: u8| Relationship { id: id.to_owned(), kind, user: Some(user(id, name)), nickname: None };
        self.relationships = vec![
            relation("2", "alex", RELATIONSHIP_FRIEND),
            relation("3", "sam", RELATIONSHIP_FRIEND),
            relation("4", "jordan", RELATIONSHIP_FRIEND),
            relation("5", "riley", RELATIONSHIP_INCOMING),
            relation("6", "spammer", RELATIONSHIP_BLOCKED),
        ];
        self.presences.insert("2".into(), "online".into());
        self.presences.insert("3".into(), "dnd".into());
        if let Ok(tab) = std::env::var("FERROCHAT_DEMO_HOME") {
            self.selected_guild = None;
            self.selected_channel = None;
            self.home.tab = HomeTab::ALL.into_iter().find(|t| t.name().eq_ignore_ascii_case(&tab)).unwrap_or_default();
            if self.home.tab == HomeTab::Inbox {
                let sample = self.logs["200"].entries.iter().rev().take(3).map(|e| e.message.clone()).collect();
                self.home.inbox = Some(Ok(sample));
            }
        }
        self.menu = match std::env::var("FERROCHAT_DEMO_MENU").as_deref() {
            Ok("message") => {
                let message_id = self.logs["200"].entries[1].message.id.clone();
                Some(super::Menu { target: MenuTarget::Message { channel_id: "200".into(), message_id }, at: Point::new(520.0, 200.0) })
            }
            Ok("guild") => Some(super::Menu { target: MenuTarget::Guild("100".into()), at: Point::new(40.0, 80.0) }),
            Ok("channel") => Some(super::Menu { target: MenuTarget::Channel("200".into()), at: Point::new(120.0, 120.0) }),
            Ok("user") => {
                let target = MenuTarget::User(user("3", "sam"), Some("100".into()));
                Some(super::Menu { target, at: Point::new(500.0, 160.0) })
            }
            _ => None,
        };
        if let Ok(kind) = std::env::var("FERROCHAT_DEMO_DIALOG") {
            let kind = match kind.as_str() {
                "invite" => DialogKind::Invite { channel_id: "200".into() },
                "create" => DialogKind::CreateChannel { guild_id: "100".into(), parent_id: None },
                "forward" => DialogKind::Forward { channel_id: "200".into(), message_id: "1".into() },
                _ => DialogKind::JoinServer,
            };
            let result = matches!(kind, DialogKind::Invite { .. }).then(|| "https://discord.gg/ferro42".to_owned());
            self.dialog = Some(super::Dialog { kind, input: String::new(), second: String::new(), flag: false, busy: false, error: None, result });
        }
    }
}

/// The code from `discord.gg/x`, `discord.com/invite/x`, or a bare code.
pub fn invite_code(input: &str) -> Option<String> {
    let input = input.trim().trim_end_matches('/');
    let code = ["discord.gg/", "discord.com/invite/", "discordapp.com/invite/"]
        .iter()
        .find_map(|prefix| input.find(prefix).map(|at| &input[at + prefix.len()..]))
        .unwrap_or(input);
    let code = code.split(['?', '#', '/']).next().unwrap_or_default();
    (!code.is_empty() && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then(|| code.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_invite_codes() {
        assert_eq!(invite_code("https://discord.gg/abc123").as_deref(), Some("abc123"));
        assert_eq!(invite_code("discord.com/invite/rust-lang?event=1").as_deref(), Some("rust-lang"));
        assert_eq!(invite_code("  xyz  ").as_deref(), Some("xyz"));
        assert_eq!(invite_code("https://example.com/a b"), None);
        assert_eq!(invite_code(""), None);
    }
}
