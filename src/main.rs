#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clipboard;
mod commands;
mod credentials;
mod discord;
mod images;
mod markup;
mod notify;
mod pointer;
mod session;
mod settings;
mod theme;
mod tray;
mod view;
mod voice;

use std::path::PathBuf;
use std::time::Duration;

use iced::keyboard::{self, key::Named, Key};
use iced::{event, window, Element, Size, Subscription, Task, Theme};

use discord::gateway::{self, Event};
use discord::model::{Channel, Command as SlashCommand, GuildSettings, Message, NotifyLevel, Profile, ReactionEmoji, User};
use discord::rest::{ForumPosts, Gif};
use session::{LoadKind, PollEdit, Session, SwitchTarget};
use settings::Settings;
use tray::TrayEvent;

fn main() -> iced::Result {
    let (settings, warning) = settings::load();
    // The software renderer measured ~28 MB vs ~208 MB for wgpu at the login screen.
    // ICED_BACKEND, when set, wins over the setting.
    if std::env::var_os("ICED_BACKEND").is_none() && !settings.gpu_renderer {
        std::env::set_var("ICED_BACKEND", "tiny-skia");
    }
    // Started from the Run key: go straight to the tray (if there is one).
    // FERROCHAT_START_HIDDEN: the window starts invisible, for automated tests
    // that show it without taking focus.
    let hidden = (settings.tray && std::env::args().any(|arg| arg == tray::MINIMIZED_ARG))
        || std::env::var_os("FERROCHAT_START_HIDDEN").is_some();
    let window = window::Settings {
        size: Size::new(1100.0, 720.0),
        visible: !hidden,
        // Closing may hide to the tray instead; see `Msg::CloseRequested`.
        exit_on_close_request: false,
        ..window::Settings::default()
    };
    iced::application("Ferrochat", App::update, App::view)
        .subscription(App::subscription)
        .theme(|app: &App| app.theme.clone())
        .window(window)
        .run_with(move || App::new(settings, warning))
}

#[derive(Debug, Clone)]
pub enum Msg {
    TokenChanged(String),
    Login,
    Logout,
    /// Saved accounts (user id).
    SwitchAccount(String),
    AddAccount,
    RemoveAccount(String),
    Gateway(Event),
    /// Expires typing indicators; only subscribed while someone is typing.
    Tick,
    SelectGuild(Option<String>),
    SelectChannel(String),
    /// A `#channel` mention or channel link: switches server if needed.
    JumpToChannel(String),
    /// A reply, message link, search result or pin.
    JumpToMessage { channel_id: String, message_id: String },
    /// Leaves older history shown after a jump and reloads the latest messages.
    JumpToPresent,
    LoadOlder,
    MessagesLoaded { channel_id: String, kind: LoadKind, result: Result<Vec<Message>, String> },
    /// Ctrl+K.
    OpenSwitcher,
    SwitcherQuery(String),
    /// Enter: the first result.
    SwitcherSubmit,
    SwitcherPick(SwitchTarget),
    CloseSwitcher,
    /// Ctrl+F.
    OpenSearch,
    SearchQuery(String),
    SearchSubmit,
    SearchDone(Result<Vec<Message>, String>),
    ShowPins,
    PinsLoaded { channel_id: String, result: Result<Vec<Message>, String> },
    ShowProfile(User),
    ProfileLoaded { user_id: String, result: Result<Profile, String> },
    OpenDm(String),
    DmOpened(Result<Channel, String>),
    ClosePanel,
    ToggleFolder(String),
    ToggleStatusMenu,
    SetStatus(String),
    CustomStatusInput(String),
    /// Saves the typed custom status (empty clears it).
    SaveCustomStatus,
    MuteChannel(String, bool),
    ChannelNotify(String, NotifyLevel),
    MuteGuild(String, bool),
    GuildNotify(String, NotifyLevel),
    GuildSettingsSaved(Result<GuildSettings, String>),
    Vote { message_id: String, answer_id: u32 },
    Voted { channel_id: String, message_id: String, before: Vec<u32>, after: Vec<u32>, result: Result<(), String> },
    OpenPollDraft,
    EditPoll(PollEdit),
    SendPoll,
    ToggleStickerPicker,
    SendSticker(String),
    /// Advances animated images (only subscribed while some are on screen).
    AnimationTick,
    ToggleGifPicker,
    GifQuery(String),
    GifSearch,
    GifsLoaded { query: String, result: Result<Vec<Gif>, String> },
    /// Sends a GIF's Tenor link; Discord embeds it.
    SendGif(String),
    PressButton { message_id: String, custom_id: String },
    ChooseOption { message_id: String, custom_id: String, value: String },
    Interacted { nonce: String, result: Result<(), String> },
    CommandsLoaded { key: String, result: Result<Vec<SlashCommand>, String> },
    /// Picked from the `/` suggestions.
    InsertCommand(String),
    ForumLoaded { forum_id: String, result: Result<ForumPosts, String> },
    ForumTitle(String),
    ForumBody(String),
    CreatePost,
    PostCreated { forum_id: String, result: Result<Channel, String> },
    /// A channel looked up for a jump (e.g. an archived thread).
    ChannelFetched { message_id: Option<String>, result: Result<Channel, String> },
    RevealSpoiler(String, u16),
    OpenLink(String),
    ImageLoaded(String, Result<images::Decoded, String>),
    ToggleMembers,
    OpenReactionPicker(String),
    ReactionQuery(String),
    ToggleReaction(String, ReactionEmoji),
    Reacted { channel_id: String, message_id: String, emoji: ReactionEmoji, add: bool, result: Result<(), String> },
    PickFiles,
    FilesPicked(Vec<PathBuf>),
    FileDropped(PathBuf),
    /// Ctrl+V: attaches a copied image or files (text goes to the message box).
    Paste,
    RemoveFile(usize),
    JoinVoice(String),
    LeaveVoice,
    ToggleMute,
    ToggleDeafen,
    Voice(voice::VoiceEvent),
    /// Events from the Go Live stream connection.
    StreamVoice(voice::VoiceEvent),
    /// Watch a user's Go Live stream in the call.
    WatchStream(String),
    StopWatching,
    /// A voice channel was clicked: join it and show its stage and chat.
    OpenVoice(String),
    /// Go Live: share your screen in the call, and events from that connection.
    ShareScreen,
    StopSharing,
    ShareVoice(voice::VoiceEvent),
    /// Show a video tile large (`None`: back to the grid).
    FocusTile(Option<String>),
    ComposerChanged(String),
    /// Replaces the `:query` being typed with this text.
    InsertEmoji(String),
    Send,
    Sent(Result<Message, String>),
    /// A typed message couldn't be sent; its text goes back in the message box.
    SendFailed { error: String, text: String },
    HoverMessage(Option<String>),
    /// The mouse left this message.
    LeaveMessage(String),
    StartReply(String),
    CancelReply,
    ToggleReplyPing,
    StartEdit(String),
    /// Up arrow in an empty composer: edit your latest message.
    EditLast,
    Edited(Result<Message, String>),
    /// First press asks for confirmation, the second deletes.
    DeleteMessage(String),
    Deleted { channel_id: String, message_id: String, result: Result<(), String> },
    Escape,
    WindowFocused(bool),
    /// The window's close button: hide to the tray, or quit.
    CloseRequested(window::Id),
    Tray(TrayEvent),
    Notified(Result<(), String>),
    ToggleSettings,
    Setting(settings::Change),
    /// Creates the themes folder (with an example) and opens it in Explorer.
    OpenThemesFolder,
    /// Re-reads the theme files and the current theme.
    ReloadThemes,
    /// (microphones, speakers) for the Settings pickers.
    AudioDevices(Vec<String>, Vec<String>),
    /// Result of a best-effort request (typing, read ack).
    Ignored,
    /// Friends, Home, menus, dialogs, servers and audio playback.
    Social(session::SocialMsg),
}

struct App {
    settings: Settings,
    /// Built from the theme and accent settings.
    theme: Theme,
    /// The notification-area icon, while "Minimize to tray" is on.
    tray: Option<tray::Tray>,
    token_input: String,
    /// Login problems and settings warnings shown on the login screen.
    login_error: Option<String>,
    session: Option<Session>,
    show_settings: bool,
    /// Listed each time Settings opens (devices come and go).
    audio_devices: (Vec<String>, Vec<String>),
    /// Save the token once READY proves it valid.
    save_token_on_ready: bool,
    /// The saved account the session was started from (or saved as), by user id.
    current_account: Option<String>,
    /// Theme files found in the themes folder (refreshed when Settings opens).
    custom_themes: Vec<String>,
    /// Problems in the current custom theme file, shown under the picker.
    theme_problems: Vec<String>,
}

impl App {
    fn new(settings: Settings, warning: Option<String>) -> (Self, Task<Msg>) {
        #[cfg(feature = "demo")]
        let settings = demo_overrides(settings);
        let mut app = Self {
            theme: theme::set(settings.theme, settings.accent),
            tray: None,
            settings,
            token_input: String::new(),
            login_error: warning,
            session: None,
            show_settings: false,
            audio_devices: (Vec::new(), Vec::new()),
            save_token_on_ready: false,
            current_account: None,
            custom_themes: Vec::new(),
            theme_problems: Vec::new(),
        };
        app.apply_theme();
        if app.settings.tray {
            match tray::create() {
                Ok(tray) => app.tray = Some(tray),
                Err(error) => app.login_error = Some(format!("Could not create the tray icon: {error}")),
            }
        }
        if app.settings.start_with_windows {
            // Refreshes the path in case the exe moved.
            if let Err(error) = tray::set_autostart(true) {
                app.login_error = Some(format!("Could not update Start with Windows: {error}"));
            }
        }
        #[cfg(feature = "demo")]
        {
            let mut session = Session::demo(app.settings.clone());
            // Public default avatars from Discord's CDN (no token) exercise the image path.
            let media = session.fetch_media();
            app.session = Some(session);
            app.show_settings = std::env::var_os("FERROCHAT_DEMO_SETTINGS").is_some();
            return (app, media);
        }
        #[allow(unreachable_code)]
        if let Ok(token) = std::env::var("DISCORD_TOKEN") {
            app.start(token, false);
        } else if app.settings.remember_login {
            // The last account used, else the first saved one, else a login saved
            // before accounts existed (moved to its own entry once READY names it).
            let account = app.settings.last_account.clone().or_else(|| app.settings.accounts.first().map(|a| a.id.clone()));
            let saved = match &account {
                Some(id) => credentials::load(Some(id)),
                None => credentials::load(None),
            };
            // Settings that don't list the account (older, or reset) still find a
            // login saved under its id; READY then lists it again.
            let (account, saved) = match (account, saved) {
                (None, Ok(None)) => match credentials::saved_ids().into_iter().next() {
                    Some(id) => {
                        let saved = credentials::load(Some(&id));
                        (Some(id), saved)
                    }
                    None => (None, Ok(None)),
                },
                other => other,
            };
            match saved {
                Ok(Some(token)) => {
                    app.start(token, true);
                    app.current_account = account;
                }
                Ok(None) => {}
                Err(error) => app.login_error = Some(format!("Could not read saved login: {error}")),
            }
        }
        (app, Task::none())
    }

    fn start(&mut self, token: String, save_on_ready: bool) {
        let token = token.trim();
        if !token.is_empty() {
            self.session = Some(Session::new(token.to_owned(), self.settings.clone()));
            self.login_error = None;
            self.save_token_on_ready = save_on_ready;
            self.current_account = None;
        }
    }

    fn switch_account(&mut self, id: String) {
        match credentials::load(Some(&id)) {
            Ok(Some(token)) => {
                self.show_settings = false;
                self.start(token, true);
                self.current_account = Some(id);
            }
            Ok(None) => {
                self.forget_account(&id);
                self.report("That account's saved login is gone; log in again to add it.".to_owned());
            }
            Err(error) => self.report(format!("Could not read saved login: {error}")),
        }
    }

    /// Removes a saved account (token and list entry).
    fn forget_account(&mut self, id: &str) {
        if let Err(error) = credentials::forget(Some(id)) {
            self.report(format!("Could not remove saved login: {error}"));
        }
        self.settings.accounts.retain(|a| a.id != id);
        if self.settings.last_account.as_deref() == Some(id) {
            self.settings.last_account = None;
        }
        self.save_settings();
    }

    fn save_settings(&mut self) {
        if let Err(error) = settings::save(&self.settings) {
            self.report(format!("Could not save settings: {error}"));
        }
    }

    fn update(&mut self, msg: Msg) -> Task<Msg> {
        // READY names the account, so the token is saved after the session sees it.
        if let (Msg::Gateway(Event::Ready(_)), Some(session)) = (&msg, &mut self.session) {
            let task = session.update(msg);
            if std::mem::take(&mut self.save_token_on_ready) {
                self.store_token();
            }
            return task;
        }
        match msg {
            Msg::TokenChanged(value) => self.token_input = value,
            Msg::Login => {
                let token = std::mem::take(&mut self.token_input);
                self.start(token, self.settings.remember_login);
            }
            Msg::Logout => {
                // Logging out also forgets this account's saved login.
                let id = self.session.as_ref().and_then(|s| s.me.as_ref()).map(|me| me.id.clone()).or(self.current_account.take());
                self.session = None;
                self.show_settings = false;
                if let Some(id) = id {
                    self.forget_account(&id);
                }
                let _ = credentials::forget(None);
            }
            Msg::SwitchAccount(id) => self.switch_account(id),
            Msg::AddAccount => {
                // Back to the login screen; saved accounts stay saved.
                self.session = None;
                self.show_settings = false;
                self.current_account = None;
            }
            Msg::RemoveAccount(id) => {
                let current = self.session.as_ref().and_then(|s| s.me.as_ref()).is_some_and(|me| me.id == id);
                self.forget_account(&id);
                if current {
                    self.session = None;
                    self.show_settings = false;
                }
            }
            Msg::Gateway(Event::AuthFailed(reason)) => {
                self.session = None;
                self.login_error = Some(reason);
                // A rejected token is useless; don't retry it on next start.
                if let Some(id) = self.current_account.take() {
                    self.forget_account(&id);
                }
                let _ = credentials::forget(None);
            }
            Msg::OpenThemesFolder => match theme::ensure_themes_dir() {
                Ok(dir) => {
                    self.custom_themes = theme::custom_themes();
                    if let Err(error) = open::that_detached(&dir) {
                        self.theme_problems = vec![format!("Could not open {}: {error}", dir.display())];
                    }
                }
                Err(error) => self.theme_problems = vec![format!("Could not create the themes folder: {error}")],
            },
            Msg::ReloadThemes => {
                self.custom_themes = theme::custom_themes();
                self.apply_theme();
            }
            Msg::ToggleSettings => {
                self.show_settings = !self.show_settings;
                if self.show_settings {
                    self.custom_themes = theme::custom_themes();
                    // Device enumeration can take a moment; keep it off the UI thread.
                    let list = async { tokio::task::spawn_blocking(voice::audio::device_names).await.unwrap_or_default() };
                    return Task::perform(list, |(inputs, outputs)| Msg::AudioDevices(inputs, outputs));
                }
            }
            Msg::AudioDevices(inputs, outputs) => self.audio_devices = (inputs, outputs),
            Msg::Escape if self.show_settings => self.show_settings = false,
            Msg::Setting(change) => return self.change_setting(change),
            Msg::CloseRequested(id) => {
                return if self.tray.is_some() { window::change_mode(id, window::Mode::Hidden) } else { iced::exit() };
            }
            Msg::Tray(TrayEvent::Show) => {
                return window::get_oldest().then(|id| match id {
                    Some(id) => Task::batch([window::change_mode(id, window::Mode::Windowed), window::gain_focus(id)]),
                    None => Task::none(),
                });
            }
            Msg::Tray(TrayEvent::Quit) => return iced::exit(),
            msg => {
                if let Some(session) = &mut self.session {
                    let task = session.update(msg);
                    session.sync_own_preview();
                    return task;
                }
            }
        }
        Task::none()
    }

    fn change_setting(&mut self, change: settings::Change) -> Task<Msg> {
        change.apply(&mut self.settings);
        let mut problems = Vec::new();
        match change {
            settings::Change::RememberLogin(remember) => {
                let result = match (&self.session, remember) {
                    (Some(_), true) => {
                        self.store_token();
                        Ok(())
                    }
                    // Forget every saved account.
                    (_, false) => {
                        let ids: Vec<String> = self.settings.accounts.drain(..).map(|a| a.id).collect();
                        self.settings.last_account = None;
                        ids.iter().map(|id| credentials::forget(Some(id))).chain([credentials::forget(None)]).collect()
                    }
                    (None, true) => Ok(()), // saved after the next successful login
                };
                problems.extend(result.err().map(|e| format!("Could not update saved login: {e}")));
            }
            settings::Change::Theme(_) | settings::Change::Accent(_) | settings::Change::CustomTheme(_) => {
                self.apply_theme();
            }
            settings::Change::Tray(on) => {
                self.tray = None;
                if on {
                    match tray::create() {
                        Ok(tray) => self.tray = Some(tray),
                        Err(error) => {
                            self.settings.tray = false;
                            problems.push(format!("Could not create the tray icon: {error}"));
                        }
                    }
                }
            }
            settings::Change::StartWithWindows(on) => {
                if let Err(error) = tray::set_autostart(on) {
                    self.settings.start_with_windows = !on;
                    problems.push(format!("Could not change Start with Windows: {error}"));
                }
            }
            _ => {}
        }
        problems.extend(settings::save(&self.settings).err().map(|e| format!("Could not save settings: {e}")));
        let task = match &mut self.session {
            Some(session) => session.settings_changed(self.settings.clone()),
            None => Task::none(),
        };
        if let Some(problem) = problems.pop() {
            self.report(problem);
        }
        task
    }

    /// Saves the session's token under its account and lists the account.
    /// Needs READY (the user id); before that it does nothing.
    fn store_token(&mut self) {
        let Some(session) = &self.session else { return };
        let Some(me) = &session.me else { return };
        let (id, name, token) = (me.id.clone(), me.display_name().to_owned(), session.token.clone());
        if let Err(error) = credentials::save(&id, &token) {
            self.report(format!("Could not save login: {error}"));
            return;
        }
        match self.settings.accounts.iter_mut().find(|a| a.id == id) {
            Some(account) => account.name = name,
            None => self.settings.accounts.push(settings::SavedAccount { id: id.clone(), name }),
        }
        self.settings.last_account = Some(id.clone());
        self.current_account = Some(id);
        self.save_settings();
    }

    /// Switches to the theme in settings. A custom theme that can't be read
    /// falls back to the built-in one; its problems show in Settings.
    fn apply_theme(&mut self) {
        self.theme_problems.clear();
        let built_in = |settings: &Settings| theme::set(settings.theme, settings.accent);
        self.theme = match &self.settings.custom_theme {
            Some(name) => match theme::set_custom(name, self.settings.accent) {
                Ok((theme, problems)) => {
                    self.theme_problems = problems;
                    theme
                }
                Err(error) => {
                    self.theme_problems = vec![error];
                    built_in(&self.settings)
                }
            },
            None => built_in(&self.settings),
        };
    }

    fn report(&mut self, problem: String) {
        match &mut self.session {
            Some(session) => session.status = problem,
            None => self.login_error = Some(problem),
        }
    }

    fn subscription(&self) -> Subscription<Msg> {
        // Window events (close, focus, files) and shortcuts apply on every screen.
        let mut subscriptions = vec![event::listen_with(app_event)];
        if self.tray.is_some() {
            subscriptions.push(Subscription::run(tray::events).map(Msg::Tray));
        }
        let Some(session) = &self.session else { return Subscription::batch(subscriptions) };
        if !session.offline {
            subscriptions.push(
                Subscription::run_with_id(session.token.clone(), gateway::connect(session.token.clone()))
                    .map(Msg::Gateway),
            );
        }
        if let Some(info) = session.voice_connect_info() {
            // A new endpoint, session or token (e.g. after moving channel) is a new connection.
            let controls = session.voice_controls.clone();
            subscriptions.push(Subscription::run_with_id(info.clone(), voice::connect(info, controls)).map(Msg::Voice));
        }
        if let Some(info) = session.stream_connect_info() {
            let controls = session.voice_controls.clone();
            subscriptions.push(Subscription::run_with_id(info.clone(), voice::connect(info, controls)).map(Msg::StreamVoice));
        }
        if let Some(info) = session.share_connect_info() {
            let controls = session.voice_controls.clone();
            subscriptions.push(Subscription::run_with_id(info.clone(), voice::connect(info, controls)).map(Msg::ShareVoice));
        }
        if let Some(interval) = session.animation_interval() {
            // Each tick redraws the window, so it runs only as fast as the GIFs need.
            subscriptions.push(iced::time::every(interval).map(|_| Msg::AnimationTick));
        }
        if session.playback().is_some() {
            // Progress and the end of playback; only while something plays.
            subscriptions.push(iced::time::every(Duration::from_millis(250)).map(|_| Msg::Social(session::SocialMsg::PlayerTick)));
        }
        if session.has_typing() {
            subscriptions.push(iced::time::every(Duration::from_secs(1)).map(|_| Msg::Tick));
        }
        Subscription::batch(subscriptions)
    }

    fn view(&self) -> Element<'_, Msg> {
        if self.show_settings {
            let cache = self.session.as_ref().map(|s| s.images.stats());
            let current = self.session.as_ref().and_then(|s| s.me.as_ref()).map(|me| me.id.as_str());
            let themes = view::ThemesInfo { custom: &self.custom_themes, problems: &self.theme_problems };
            return view::settings_panel(&self.settings, cache, &self.audio_devices, current, themes);
        }
        match &self.session {
            Some(session) => session.view(),
            None => view::login(&self.token_input, self.login_error.as_deref(), &self.settings),
        }
    }
}

/// Keys and window events, whether or not a widget handled them (the composer
/// captures Escape).
fn app_event(event: iced::Event, _status: event::Status, window: window::Id) -> Option<Msg> {
    match event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => match key.as_ref() {
            Key::Named(Named::Escape) => Some(Msg::Escape),
            Key::Named(Named::ArrowUp) => Some(Msg::EditLast),
            Key::Character("k") if modifiers.command() => Some(Msg::OpenSwitcher),
            Key::Character("f") if modifiers.command() => Some(Msg::OpenSearch),
            Key::Character("v") if modifiers.command() => Some(Msg::Paste),
            _ => None,
        },
        iced::Event::Window(window::Event::FileDropped(path)) => Some(Msg::FileDropped(path)),
        // The voice stage sizes its tiles to the window.
        iced::Event::Window(window::Event::Resized(size) | window::Event::Opened { size, .. }) => {
            Some(Msg::Social(session::SocialMsg::WindowSize(size)))
        }
        iced::Event::Window(window::Event::Focused) => Some(Msg::WindowFocused(true)),
        iced::Event::Window(window::Event::Unfocused) => Some(Msg::WindowFocused(false)),
        iced::Event::Window(window::Event::CloseRequested) => Some(Msg::CloseRequested(window)),
        _ => None,
    }
}

/// FERROCHAT_DEMO_THEME / _ACCENT / _COMPACT, for checking looks offline.
#[cfg(feature = "demo")]
fn demo_overrides(mut settings: Settings) -> Settings {
    let pick = |name: &str| std::env::var(name).ok().map(|v| v.to_lowercase());
    if let Some(name) = pick("FERROCHAT_DEMO_THEME") {
        if let Some(theme) = theme::ThemeName::ALL.into_iter().find(|t| t.to_string().to_lowercase() == name) {
            settings.theme = theme;
        }
    }
    if let Some(name) = pick("FERROCHAT_DEMO_ACCENT") {
        if let Some(accent) = theme::Accent::ALL.into_iter().find(|a| a.to_string().to_lowercase() == name) {
            settings.accent = accent;
        }
    }
    settings.compact |= std::env::var_os("FERROCHAT_DEMO_COMPACT").is_some();
    if let Ok(name) = std::env::var("FERROCHAT_DEMO_CUSTOM_THEME") {
        settings.custom_theme = Some(name);
    }
    if let Ok(value) = std::env::var("FERROCHAT_DEMO_ANIMATE") {
        settings.animate_media = value == "1";
    }
    settings
}
