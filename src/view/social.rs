//! The Home page (friends, requests, inbox), right-click menus, dialogs, audio
//! attachments and folded messages from blocked users.

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, column, container, mouse_area, opaque, progress_bar, row, stack, text, text_input, Column, Row, Space};
use iced::{Alignment, Background, Border, Color, Element, Length};

use super::{bordered, fill, label, quiet, rule, section_label, status_color, status_name, subtle, thin_scroll, BOLD};
use crate::discord::model::{
    Attachment, Message, Relationship, RELATIONSHIP_BLOCKED, RELATIONSHIP_FRIEND, RELATIONSHIP_INCOMING, RELATIONSHIP_OUTGOING,
};
use crate::discord::permissions::{CHANGE_NICKNAME, CREATE_INSTANT_INVITE, MANAGE_CHANNELS, MANAGE_MESSAGES};
use crate::session::{human_size, invite_code, DialogKind, HomeTab, MenuTarget, Session, SocialMsg, SwitchTarget};
use crate::theme::p;
use crate::Msg;

const MENU_WIDTH: f32 = 220.0;
/// One menu row's height, for keeping the menu inside the window.
const MENU_ROW: f32 = 29.0;
const FRIEND_AVATAR: f32 = 32.0;
const FORWARD_RESULTS: usize = 8;

fn social(msg: SocialMsg) -> Msg {
    Msg::Social(msg)
}

enum MenuItem {
    Action(String, Msg, bool),
    Gap,
}

fn action(name: &str, msg: Msg) -> MenuItem {
    MenuItem::Action(name.to_owned(), msg, false)
}

fn danger(name: &str, msg: Msg) -> MenuItem {
    MenuItem::Action(name.to_owned(), msg, true)
}

fn copy(name: &str, value: String) -> MenuItem {
    action(name, social(SocialMsg::Copy(value)))
}

fn dialog(kind: DialogKind) -> Msg {
    social(SocialMsg::OpenDialog(kind))
}

/// A tab or toggle chip, like the header's small buttons but owning its label.
fn chip(name: String, on: bool, msg: Msg) -> Element<'static, Msg> {
    button(text(name).size(13))
        .padding([4, 10])
        .style(move |theme, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let mut style = button::text(theme, status);
            style.text_color = if on || hovered { p().text } else { p().muted };
            style.background = if on {
                Some(Background::Color(p().selected))
            } else if hovered {
                Some(Background::Color(Color { a: 0.5, ..p().selected }))
            } else {
                None
            };
            style.border = Border { radius: 6.0.into(), ..Border::default() };
            style
        })
        .on_press(msg)
        .into()
}

fn row_button(name: &str, kind: fn(&iced::Theme, button::Status) -> button::Style, msg: Msg) -> Element<'static, Msg> {
    button(text(name.to_owned()).size(12)).padding([4, 10]).style(kind).on_press(msg).into()
}

fn minutes(ms: u32) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

impl Session {
    /// Friends and Inbox, above the DM list.
    pub(super) fn home_entries(&self) -> Element<'_, Msg> {
        let at_home = self.selected_guild.is_none() && self.selected_channel.is_none();
        let inbox = self.home.tab == HomeTab::Inbox;
        let friends_msg = social(SocialMsg::Tab(if inbox { HomeTab::Online } else { self.home.tab }));
        let entry = |name: &'static str, active: bool, msg: Msg, badge: usize| -> Element<'static, Msg> {
            let mut line = row![label(name).size(14).width(Length::Fill)].align_y(Alignment::Center);
            if badge > 0 {
                line = line.push(super::mention_badge(badge as u32));
            }
            button(line)
                .width(Length::Fill)
                .padding([6, 8])
                .style(move |theme, status| {
                    let mut style = subtle(theme, status);
                    if active {
                        style.background = Some(Background::Color(p().selected));
                    }
                    style
                })
                .on_press(msg)
                .into()
        };
        column![
            entry("Friends", at_home && !inbox, friends_msg, self.incoming_requests()),
            entry("Inbox", at_home && inbox, social(SocialMsg::Tab(HomeTab::Inbox)), 0),
            container(text("DIRECT MESSAGES").size(11).color(p().muted)).padding([10, 8]),
        ]
        .spacing(2)
        .into()
    }

    /// Direct Messages with no conversation open.
    pub(super) fn home_view(&self) -> Element<'_, Msg> {
        let mut tabs = Row::new()
            .spacing(4)
            .align_y(Alignment::Center)
            .push(label("Friends").size(15).font(BOLD))
            .push(Space::with_width(12));
        let pending = self.incoming_requests();
        for tab in HomeTab::ALL {
            let name = match tab {
                HomeTab::Pending if pending > 0 => format!("Pending ({pending})"),
                tab => tab.name().to_owned(),
            };
            tabs = tabs.push(chip(name, self.home.tab == tab, social(SocialMsg::Tab(tab))));
        }
        if !self.home.group.is_empty() {
            let create = button(text(format!("Create group DM ({})", self.home.group.len())).size(12))
                .padding([5, 12])
                .on_press(social(SocialMsg::CreateGroup));
            tabs = tabs.push(Space::with_width(Length::Fill)).push(create);
        }
        let body: Element<'_, Msg> = match self.home.tab {
            HomeTab::Add => self.add_friend_view(),
            HomeTab::Inbox => self.inbox_view(),
            tab => self.friend_list(tab),
        };
        container(column![
            container(tabs).padding([10, 16]).width(Length::Fill),
            rule(),
            thin_scroll(container(body).padding([14, 20]).width(Length::Fill)).height(Length::Fill),
        ])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(fill(p().chat, 0.0))
        .into()
    }

    fn friend_list(&self, tab: HomeTab) -> Element<'_, Msg> {
        let friends = self.friends(tab);
        let search = text_input("Search", &self.home.filter).on_input(|t| social(SocialMsg::Filter(t))).padding(8);
        let heading = text(format!("{} — {}", tab.name().to_uppercase(), friends.len())).size(11).color(p().muted);
        let mut list = Column::new().spacing(2).push(search).push(container(heading).padding([10, 0]));
        if friends.is_empty() {
            let hint = match tab {
                HomeTab::Online => "No one's around to play with right now.",
                HomeTab::Pending => "There are no pending friend requests.",
                HomeTab::Blocked => "You haven't blocked anyone.",
                _ if self.is_bot() => "Bot logins have no friends list.",
                _ => "No friends yet. Add some with Add Friend.",
            };
            list = list.push(container(text(hint).size(13).color(p().muted)).padding([20, 0]));
        }
        for relationship in friends {
            list = list.push(rule()).push(self.friend_row(relationship));
        }
        list.into()
    }

    fn friend_row<'a>(&'a self, relationship: &'a Relationship) -> Element<'a, Msg> {
        let id = &relationship.id;
        let status = self.friend_status(id);
        let avatar: Element<'_, Msg> = match &relationship.user {
            Some(user) if self.settings.load_avatars => self.avatar(user, FRIEND_AVATAR),
            _ => container(Space::new(FRIEND_AVATAR, FRIEND_AVATAR)).style(fill(p().selected, FRIEND_AVATAR / 2.0)).into(),
        };
        let mut avatar = stack![avatar];
        if relationship.kind == RELATIONSHIP_FRIEND {
            let dot = container(Space::new(10, 10)).style(fill(status_color(status), 5.0));
            avatar = avatar.push(
                container(dot).width(FRIEND_AVATAR).height(FRIEND_AVATAR).align_x(Horizontal::Right).align_y(Vertical::Bottom),
            );
        }
        let subtitle = match relationship.kind {
            RELATIONSHIP_INCOMING => "Incoming friend request",
            RELATIONSHIP_OUTGOING => "Outgoing friend request",
            RELATIONSHIP_BLOCKED => "Blocked",
            _ => status_name(status),
        };
        let username = relationship.user.as_ref().map(|u| format!("@{}", u.username)).unwrap_or_default();
        let who = column![
            row![label(relationship.name()).size(14).font(BOLD), text(username).size(12).color(p().muted)]
                .spacing(6)
                .align_y(Alignment::Center),
            text(subtitle).size(11).color(p().muted),
        ]
        .spacing(1)
        .width(Length::Fill);
        let mut line = row![avatar, who].spacing(10).align_y(Alignment::Center);
        let remove = || social(SocialMsg::Remove(id.clone()));
        match relationship.kind {
            RELATIONSHIP_FRIEND => {
                let grouped = self.home.group.contains(id);
                line = line
                    .push(row_button("Message", quiet, Msg::OpenDm(id.clone())))
                    .push(row_button(if grouped { "In group" } else { "Add to group" }, quiet, social(SocialMsg::PickForGroup(id.clone()))))
                    .push(row_button("Remove", button::danger, remove()));
            }
            RELATIONSHIP_INCOMING => {
                line = line
                    .push(row_button("Accept", button::success, social(SocialMsg::Accept(id.clone()))))
                    .push(row_button("Ignore", quiet, remove()));
            }
            RELATIONSHIP_OUTGOING => line = line.push(row_button("Cancel", quiet, remove())),
            RELATIONSHIP_BLOCKED => line = line.push(row_button("Unblock", quiet, remove())),
            _ => {}
        }
        let content = container(line).padding([8, 6]).width(Length::Fill);
        match &relationship.user {
            Some(user) => mouse_area(content).on_right_press(social(SocialMsg::OpenMenu(MenuTarget::User(user.clone(), None)))).into(),
            None => content.into(),
        }
    }

    fn add_friend_view(&self) -> Element<'_, Msg> {
        let typed = !self.home.add_input.trim().is_empty();
        let input = text_input("Enter a username", &self.home.add_input)
            .on_input(|t| social(SocialMsg::AddInput(t)))
            .on_submit(social(SocialMsg::SendFriendRequest))
            .padding(10)
            .width(Length::Fill);
        let send = button(text("Send Friend Request").size(13))
            .padding([10, 14])
            .on_press_maybe(typed.then(|| social(SocialMsg::SendFriendRequest)));
        let mut view = column![
            label("ADD FRIEND").size(14).font(BOLD),
            text("You can add friends with their Discord username.").size(13).color(p().muted),
            row![input, send].spacing(8).align_y(Alignment::Center),
        ]
        .spacing(10);
        match &self.home.add_result {
            Some(Ok(done)) => view = view.push(text(done.as_str()).size(13).color(p().online)),
            Some(Err(error)) => view = view.push(text(error.as_str()).size(13).color(p().error)),
            None => {}
        }
        view.into()
    }

    fn inbox_view(&self) -> Element<'_, Msg> {
        let status = |line: &'static str| text(line).size(13).color(p().muted).into();
        let messages: Element<'_, Msg> = match &self.home.inbox {
            None => status("Loading mentions…"),
            Some(Err(error)) => text(error.as_str()).size(13).color(p().error).into(),
            Some(Ok(messages)) if messages.is_empty() => status("You're all caught up: no recent mentions."),
            Some(Ok(messages)) => self.message_results(messages, true),
        };
        column![label("MENTIONS").size(14).font(BOLD), messages].spacing(12).into()
    }

    /// The open right-click menu, placed at the cursor and kept inside the window.
    pub(super) fn menu_overlay(&self) -> Option<Element<'_, Msg>> {
        let menu = self.menu.as_ref()?;
        let items = self.menu_items(&menu.target);
        if items.is_empty() {
            return None;
        }
        let height = items.len() as f32 * MENU_ROW + 12.0;
        let mut list = Column::new().spacing(1);
        for item in items {
            list = list.push(match item {
                MenuItem::Gap => container(Space::new(Length::Fill, 1)).padding([4, 6]).into(),
                MenuItem::Action(name, msg, risky) => {
                    let element: Element<'static, Msg> = button(text(name).size(13))
                        .width(Length::Fill)
                        .padding([5, 10])
                        .style(move |theme, status| {
                            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
                            let mut style = button::text(theme, status);
                            style.text_color = match (risky, hovered) {
                                (_, true) => Color::WHITE,
                                (true, false) => p().mention,
                                (false, false) => p().text,
                            };
                            let hover = if risky { p().mention } else { p().accent };
                            style.background = hovered.then_some(Background::Color(hover));
                            style.border = Border { radius: 4.0.into(), ..Border::default() };
                            style
                        })
                        .on_press(social(SocialMsg::Pick(Box::new(msg))))
                        .into();
                    element
                }
            });
        }
        let x = menu.at.x.min(self.window.width - MENU_WIDTH - 8.0).max(0.0);
        let y = menu.at.y.min(self.window.height - height - 8.0).max(0.0);
        let card = container(list).padding(6).width(MENU_WIDTH).style(bordered(p().rail, 6.0));
        let placed = column![Space::with_height(y), row![Space::with_width(x), opaque(card)]];
        // Any click outside closes it.
        let backdrop = mouse_area(container(placed).width(Length::Fill).height(Length::Fill))
            .on_press(social(SocialMsg::CloseMenu))
            .on_right_press(social(SocialMsg::CloseMenu));
        Some(opaque(backdrop))
    }

    fn menu_items(&self, target: &MenuTarget) -> Vec<MenuItem> {
        let me = self.me.as_ref().map_or("", |me| me.id.as_str());
        let link = |guild: Option<&str>, rest: String| format!("https://discord.com/channels/{}/{rest}", guild.unwrap_or("@me"));
        let mut items = Vec::new();
        match target {
            MenuTarget::Message { channel_id, message_id } => {
                let Some(entry) = self.find_entry(channel_id, message_id) else { return items };
                let message = &entry.message;
                let guild = self.guild_of(channel_id).map(|g| g.id.as_str());
                items.push(action("Add Reaction", Msg::OpenReactionPicker(message_id.clone())));
                if self.is_mine(message) {
                    items.push(action("Edit Message", Msg::StartEdit(message_id.clone())));
                }
                items.push(action("Reply", Msg::StartReply(message_id.clone())));
                if !self.is_bot() {
                    let forward = DialogKind::Forward { channel_id: channel_id.clone(), message_id: message_id.clone() };
                    items.push(action("Forward", dialog(forward)));
                }
                if guild.is_none() || self.can_in_channel(channel_id, MANAGE_MESSAGES) {
                    let pin = SocialMsg::Pin { channel_id: channel_id.clone(), message_id: message_id.clone(), pin: !message.pinned };
                    items.push(action(if message.pinned { "Unpin Message" } else { "Pin Message" }, social(pin)));
                }
                if !self.is_bot() {
                    let unread = SocialMsg::MarkUnread { channel_id: channel_id.clone(), message_id: message_id.clone() };
                    items.push(action("Mark Unread", social(unread)));
                }
                items.push(MenuItem::Gap);
                let plain = self.plain_text(message);
                if !plain.is_empty() {
                    items.push(copy("Copy Text", plain));
                }
                items.push(copy("Copy Message Link", link(guild, format!("{channel_id}/{message_id}"))));
                items.push(copy("Copy Message ID", message_id.clone()));
                if self.can_delete(message) {
                    items.push(MenuItem::Gap);
                    items.push(danger("Delete Message", dialog(DialogKind::DeleteMessage { message_id: message_id.clone() })));
                }
            }
            MenuTarget::User(user, guild_id) => {
                let mine = user.id == me;
                items.push(action("Profile", Msg::ShowProfile(user.clone())));
                if !mine && !self.is_bot() {
                    items.push(action("Message", Msg::OpenDm(user.id.clone())));
                    match self.relationship(&user.id) {
                        None if !user.bot => items.push(action("Add Friend", social(SocialMsg::AddFriend(user.id.clone())))),
                        Some(RELATIONSHIP_FRIEND) => items.push(danger("Remove Friend", social(SocialMsg::Remove(user.id.clone())))),
                        Some(RELATIONSHIP_INCOMING) => items.push(action("Accept Friend Request", social(SocialMsg::Accept(user.id.clone())))),
                        Some(RELATIONSHIP_OUTGOING) => items.push(action("Cancel Friend Request", social(SocialMsg::Remove(user.id.clone())))),
                        _ => {}
                    }
                }
                if let Some(guild_id) = guild_id.as_ref().filter(|g| mine && self.can_in_guild(g, CHANGE_NICKNAME)) {
                    items.push(action("Change Nickname", dialog(DialogKind::Nickname { guild_id: guild_id.clone() })));
                }
                items.push(MenuItem::Gap);
                items.push(copy("Copy Username", user.username.clone()));
                items.push(copy("Copy User ID", user.id.clone()));
                if !mine && !self.is_bot() {
                    items.push(MenuItem::Gap);
                    items.push(match self.is_blocked(&user.id) {
                        true => action("Unblock", social(SocialMsg::Remove(user.id.clone()))),
                        false => danger("Block", social(SocialMsg::Block(user.id.clone()))),
                    });
                }
            }
            MenuTarget::Channel(channel_id) => {
                let Some(channel) = self.channel(channel_id) else { return items };
                let guild = self.guild_of(channel_id).map(|g| g.id.clone());
                let manage = guild.is_some() && self.can_in_channel(channel_id, MANAGE_CHANNELS);
                if channel.is_category() {
                    let collapsed = self.collapsed.contains(channel_id);
                    items.push(action(if collapsed { "Expand Category" } else { "Collapse Category" }, social(SocialMsg::ToggleCategory(channel_id.clone()))));
                    let children: Vec<String> = self
                        .guild_of(channel_id)
                        .map(|g| g.channels.iter().filter(|c| c.parent_id.as_ref() == Some(channel_id)).map(|c| c.id.clone()).collect())
                        .unwrap_or_default();
                    items.push(action("Mark as Read", social(SocialMsg::MarkRead(children))));
                    if let (true, Some(guild_id)) = (manage, &guild) {
                        let create = DialogKind::CreateChannel { guild_id: guild_id.clone(), parent_id: Some(channel_id.clone()) };
                        items.push(action("Create Channel", dialog(create)));
                        items.push(action("Edit Category", dialog(DialogKind::EditChannel { channel_id: channel_id.clone() })));
                        items.push(danger("Delete Category", dialog(DialogKind::DeleteChannel { channel_id: channel_id.clone() })));
                    }
                    items.push(MenuItem::Gap);
                    items.push(copy("Copy Category ID", channel_id.clone()));
                    return items;
                }
                items.push(action("Mark as Read", social(SocialMsg::MarkRead(vec![channel_id.clone()]))));
                let muted = self.channel_muted(channel_id);
                if !self.is_bot() {
                    let label = match (muted, guild.is_some()) {
                        (true, true) => "Unmute Channel",
                        (false, true) => "Mute Channel",
                        (true, false) => "Unmute Conversation",
                        (false, false) => "Mute Conversation",
                    };
                    items.push(action(label, Msg::MuteChannel(channel_id.clone(), !muted)));
                }
                if guild.is_some() && channel.is_text() && self.can_in_channel(channel_id, CREATE_INSTANT_INVITE) {
                    items.push(action("Invite People", dialog(DialogKind::Invite { channel_id: channel_id.clone() })));
                }
                if manage {
                    items.push(action("Edit Channel", dialog(DialogKind::EditChannel { channel_id: channel_id.clone() })));
                }
                items.push(MenuItem::Gap);
                items.push(copy("Copy Link", link(guild.as_deref(), channel_id.clone())));
                items.push(copy("Copy Channel ID", channel_id.clone()));
                match channel.kind {
                    1 => {
                        items.push(MenuItem::Gap);
                        items.push(action("Close DM", social(SocialMsg::CloseDm(channel_id.clone()))));
                    }
                    3 => {
                        items.push(MenuItem::Gap);
                        items.push(danger("Leave Group", social(SocialMsg::CloseDm(channel_id.clone()))));
                    }
                    _ if manage => {
                        items.push(MenuItem::Gap);
                        items.push(danger("Delete Channel", dialog(DialogKind::DeleteChannel { channel_id: channel_id.clone() })));
                    }
                    _ => {}
                }
            }
            MenuTarget::Guild(guild_id) => {
                let Some(guild) = self.guilds.iter().find(|g| &g.id == guild_id) else { return items };
                items.push(action("Mark as Read", social(SocialMsg::MarkRead(self.guild_text_channels(guild_id)))));
                if !self.is_bot() {
                    let muted = self.guild_muted(guild_id);
                    items.push(action(if muted { "Unmute Server" } else { "Mute Server" }, Msg::MuteGuild(guild_id.clone(), !muted)));
                }
                // Invites go to the open channel when it's in this server, else the first one allowed.
                let open = self.selected_channel.as_ref().filter(|id| guild.channels.iter().any(|c| &c.id == *id && c.is_text()));
                let invite_channel = open.cloned().or_else(|| {
                    guild.channels.iter().find(|c| c.is_text() && self.can_in_channel(&c.id, CREATE_INSTANT_INVITE)).map(|c| c.id.clone())
                });
                if let Some(channel_id) = invite_channel.filter(|id| self.can_in_channel(id, CREATE_INSTANT_INVITE)) {
                    items.push(action("Invite People", dialog(DialogKind::Invite { channel_id })));
                }
                if self.can_in_guild(guild_id, CHANGE_NICKNAME) {
                    items.push(action("Change Nickname", dialog(DialogKind::Nickname { guild_id: guild_id.clone() })));
                }
                if self.can_in_guild(guild_id, MANAGE_CHANNELS) {
                    items.push(action("Create Channel", dialog(DialogKind::CreateChannel { guild_id: guild_id.clone(), parent_id: None })));
                }
                items.push(MenuItem::Gap);
                items.push(copy("Copy Server ID", guild_id.clone()));
                if guild.owner_id.as_deref() != Some(me) && !self.is_bot() {
                    items.push(MenuItem::Gap);
                    items.push(danger("Leave Server", dialog(DialogKind::LeaveServer { guild_id: guild_id.clone() })));
                }
            }
        }
        // No two gaps in a row, and none at either end.
        let mut tidy: Vec<MenuItem> = Vec::with_capacity(items.len());
        for item in items {
            let gap = matches!(item, MenuItem::Gap);
            if gap && tidy.last().is_none_or(|last| matches!(last, MenuItem::Gap)) {
                continue;
            }
            tidy.push(item);
        }
        if matches!(tidy.last(), Some(MenuItem::Gap)) {
            tidy.pop();
        }
        tidy
    }

    /// The open dialog, centred over a dimmed window.
    pub(super) fn dialog_overlay(&self) -> Option<Element<'_, Msg>> {
        let dialog = self.dialog.as_ref()?;
        let muted = |line: String| text(line).size(13).color(p().muted);
        let input = |placeholder: &'static str| {
            text_input(placeholder, &dialog.input)
                .id(text_input::Id::new(crate::session::DIALOG_INPUT))
                .on_input(|t| social(SocialMsg::DialogInput(t)))
                .on_submit(social(SocialMsg::SubmitDialog))
                .padding(10)
        };
        let channel_name = |id: &str| self.channel(id).map(|c| c.label()).unwrap_or_else(|| "this channel".to_owned());
        let mut body = Column::new().spacing(10).push(label(dialog.kind.title()).size(18).font(BOLD));
        // (primary button, is it destructive)
        let mut submit: Option<(&'static str, bool)> = None;
        match &dialog.kind {
            DialogKind::JoinServer => {
                body = body
                    .push(muted("Enter an invite below to join an existing server.".into()))
                    .push(input("https://discord.gg/hTKzmak"))
                    .push(
                        button(text("Create my own server instead").size(12))
                            .padding(0)
                            .style(button::text)
                            .on_press(dialog_msg(DialogKind::CreateServer)),
                    );
                if let (Some(_), Some(code)) = (&dialog.error, invite_code(&dialog.input)) {
                    let open = Msg::OpenLink(format!("https://discord.com/invite/{code}"));
                    body = body.push(button(text("Open invite in browser").size(12)).padding([4, 10]).style(quiet).on_press(open));
                }
                submit = Some(("Join Server", false));
            }
            DialogKind::CreateServer => {
                body = body.push(muted("Give your new server a name. You can always change it later.".into())).push(input("My server"));
                if dialog.error.is_some() {
                    let open = Msg::OpenLink("https://discord.com/channels/@me".to_owned());
                    body = body.push(button(text("Open Discord in browser").size(12)).padding([4, 10]).style(quiet).on_press(open));
                }
                submit = Some(("Create", false));
            }
            DialogKind::Invite { channel_id } => {
                body = body.push(muted(format!("Send a server invite link to a friend (#{}).", channel_name(channel_id))));
                match &dialog.result {
                    Some(link) => {
                        let field = text_input("", link).padding(10).width(Length::Fill);
                        let copy = button(text("Copy").size(13)).padding([10, 14]).on_press(social(SocialMsg::Copy(link.clone())));
                        body = body
                            .push(row![field, copy].spacing(8).align_y(Alignment::Center))
                            .push(text("Your invite link expires in 1 day.").size(11).color(p().muted));
                    }
                    None if dialog.busy => body = body.push(muted("Creating an invite…".into())),
                    None => {}
                }
            }
            DialogKind::Nickname { .. } => {
                body = body.push(muted("Everyone in this server sees it. Leave it empty to reset.".into())).push(input("Nickname"));
                submit = Some(("Save", false));
            }
            DialogKind::CreateChannel { .. } => {
                let kinds = row![
                    chip("# Text".to_owned(), !dialog.flag, social(SocialMsg::DialogFlag(false))),
                    chip("Voice".to_owned(), dialog.flag, social(SocialMsg::DialogFlag(true))),
                ]
                .spacing(6);
                body = body.push(section_label("CHANNEL TYPE")).push(kinds).push(section_label("CHANNEL NAME")).push(input("new-channel"));
                submit = Some(("Create Channel", false));
            }
            DialogKind::EditChannel { .. } => {
                let topic = text_input("Topic (optional)", &dialog.second).on_input(|t| social(SocialMsg::DialogSecond(t))).padding(10);
                body = body.push(section_label("NAME")).push(input("Name")).push(section_label("TOPIC")).push(topic);
                submit = Some(("Save Changes", false));
            }
            DialogKind::Forward { .. } => {
                body = body.push(input("Search for a channel or DM"));
                let mut results = Column::new().spacing(2);
                let places = self.places(&dialog.input);
                for place in places.into_iter().filter(|p| matches!(p.target, SwitchTarget::Channel(_))).take(FORWARD_RESULTS) {
                    let SwitchTarget::Channel(id) = place.target else { continue };
                    let line = row![label(place.label).size(13).width(Length::Fill), text(place.context).size(11).color(p().muted)];
                    let mut pick = button(line.align_y(Alignment::Center)).width(Length::Fill).padding([6, 8]).style(subtle);
                    if !dialog.busy {
                        pick = pick.on_press(social(SocialMsg::ForwardTo(id)));
                    }
                    results = results.push(pick);
                }
                body = body.push(results);
            }
            DialogKind::LeaveServer { guild_id } => {
                let name = self.guilds.iter().find(|g| &g.id == guild_id).map_or("this server", |g| g.name.as_str());
                body = body.push(muted(format!("Are you sure you want to leave {name}? You won't be able to rejoin unless you are re-invited.")));
                submit = Some(("Leave Server", true));
            }
            DialogKind::DeleteChannel { channel_id } => {
                body = body.push(muted(format!("Are you sure you want to delete #{}? This cannot be undone.", channel_name(channel_id))));
                submit = Some(("Delete", true));
            }
            DialogKind::DeleteMessage { message_id } => {
                let preview = self
                    .selected_channel
                    .as_deref()
                    .and_then(|channel| self.find_entry(channel, message_id))
                    .map(|e| crate::notify::truncate(&self.plain_text(&e.message), 160))
                    .unwrap_or_default();
                body = body.push(muted("Are you sure you want to delete this message? This cannot be undone.".into()));
                if !preview.is_empty() {
                    body = body.push(container(text(preview).size(13)).padding(8).width(Length::Fill).style(fill(p().chat, 6.0)));
                }
                submit = Some(("Delete", true));
            }
        }
        if let Some(error) = &dialog.error {
            body = body.push(text(error.as_str()).size(12).color(p().error));
        }
        let close = if submit.is_some() { "Cancel" } else { "Close" };
        let mut buttons = row![Space::with_width(Length::Fill), button(text(close).size(13)).padding([8, 14]).style(button::text).on_press(social(SocialMsg::CloseDialog))]
            .spacing(8);
        if let Some((name, risky)) = submit {
            let name = if dialog.busy { "Working…" } else { name };
            let primary = button(text(name).size(13))
                .padding([8, 16])
                .style(if risky { button::danger } else { button::primary })
                .on_press_maybe((!dialog.busy).then(|| social(SocialMsg::SubmitDialog)));
            buttons = buttons.push(primary);
        }
        let card = container(body.push(buttons)).padding(20).width(440).style(bordered(p().sidebar, 10.0));
        let card = container(opaque(card)).center_x(Length::Fill).padding([110, 0]);
        let backdrop = container(card).width(Length::Fill).height(Length::Fill).style(fill(p().backdrop, 0.0));
        Some(opaque(mouse_area(backdrop).on_press(social(SocialMsg::CloseDialog))))
    }

    /// Voice messages and audio files: play/stop, time, and a progress bar.
    pub(super) fn audio_attachment(&self, attachment: &Attachment) -> Element<'static, Msg> {
        let playing = self.playback().filter(|p| p.url == attachment.url);
        let loading = self.is_loading_audio(&attachment.url);
        let title = if attachment.waveform.is_some() { "Voice message".to_owned() } else { attachment.filename.clone() };
        let total = attachment.duration_secs.map(|s| (s * 1000.0) as u32).filter(|&ms| ms > 0);
        let position = playing.map_or(0, |p| p.position_ms());
        let (name, msg) = if playing.is_some() || loading {
            ("Stop", social(SocialMsg::StopAudio))
        } else {
            ("Play", social(SocialMsg::Play { url: attachment.url.clone(), name: attachment.filename.clone() }))
        };
        let time = match (loading, total) {
            (true, _) => "Loading…".to_owned(),
            (false, Some(total)) => format!("{} / {}", minutes(position), minutes(total)),
            (false, None) if playing.is_some() => minutes(position),
            (false, None) => human_size(attachment.size),
        };
        let mut meta = Row::new().spacing(8).align_y(Alignment::Center);
        if let Some(total) = total {
            meta = meta.push(progress_bar(0.0..=total as f32, position.min(total) as f32).height(6).width(160));
        }
        meta = meta.push(text(time).size(11).color(p().muted));
        let play = button(text(name).size(12).font(BOLD)).padding([6, 12]).on_press(msg);
        let open = button(text("Open").size(11)).padding([3, 8]).style(button::text).on_press(Msg::OpenLink(attachment.url.clone()));
        let content = row![play, column![label(title).size(13).font(BOLD), meta].spacing(4).width(Length::Fill), open]
            .spacing(10)
            .align_y(Alignment::Center);
        container(content).padding(8).max_width(440).style(bordered(p().sidebar, 8.0)).into()
    }

    /// One line in place of a blocked user's message.
    pub(super) fn blocked_view(&self, message: &Message) -> Element<'static, Msg> {
        let show = button(text("Show message").size(11)).padding([2, 8]).style(quiet).on_press(social(SocialMsg::ShowBlocked(message.id.clone())));
        container(row![text("Blocked message").size(12).color(p().muted), show].spacing(10).align_y(Alignment::Center))
            .padding([3, 8])
            .into()
    }
}

/// [`dialog`] under another name, for where a local `dialog` shadows it.
fn dialog_msg(kind: DialogKind) -> Msg {
    dialog(kind)
}
