use std::time::Duration;

use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, Stream, StreamExt};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::member_list::{self, Update as MemberListUpdate};
use super::model::{
    Channel, Guild, GuildSettings, Message, MessageDelete, PollVote, Sticker, ThreadDelete, ThreadListSync, MessageUpdate, RawGuild, RawReady, ReactionClear, ReactionEvent,
    Ready, StreamCreate, StreamServer, TypingStart, UserSettings, VoiceServer, VoiceState, Relationship, presence_of,
};

const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
/// GUILDS | GUILD_VOICE_STATES | GUILD_MESSAGES | GUILD_MESSAGE_REACTIONS | GUILD_MESSAGE_TYPING | DIRECT_MESSAGES
/// | DIRECT_MESSAGE_REACTIONS | DIRECT_MESSAGE_TYPING | MESSAGE_CONTENT. Only bot sessions send intents.
const BOT_INTENTS: u64 =
    (1 << 0) | (1 << 7) | (1 << 9) | (1 << 10) | (1 << 11) | (1 << 12) | (1 << 13) | (1 << 14) | (1 << 15);

/// Requests from the UI, sent only while a session is READY.
#[derive(Debug, Clone)]
pub enum Command {
    /// User sessions: receive live events (messages, typing) for a large guild.
    SubscribeGuild(String),
    /// User sessions: like `SubscribeGuild`, plus the member sidebar for `channel_id`.
    SubscribeMembers { guild_id: String, channel_id: String },
    /// Op 4: join (`Some`), move, or leave (`None`) a voice channel.
    VoiceState { guild_id: String, channel_id: Option<String>, self_mute: bool, self_deaf: bool },
    /// Op 3: online / idle / dnd / invisible, with an optional custom status.
    Presence { status: String, custom: Option<String> },
    /// Op 20: start watching a Go Live stream.
    WatchStream(String),
    /// Op 19: stop watching it, or end our own.
    StopStream(String),
    /// Op 18: start our own Go Live stream in a voice channel.
    CreateStream { guild_id: String, channel_id: String },
    /// Op 22: pause or resume our stream.
    PauseStream { stream_key: String, paused: bool },
}

impl Command {
    /// Subscriptions are dropped if the gateway rejects them; voice and presence are always sent.
    fn is_subscription(&self) -> bool {
        matches!(self, Command::SubscribeGuild(_) | Command::SubscribeMembers { .. })
    }

    fn frame(&self) -> WsMessage {
        if let Command::VoiceState { guild_id, channel_id, self_mute, self_deaf } = self {
            let payload = json!({
                "op": 4,
                "d": { "guild_id": guild_id, "channel_id": channel_id, "self_mute": self_mute, "self_deaf": self_deaf },
            });
            return WsMessage::Text(payload.to_string());
        }
        if let Command::CreateStream { guild_id, channel_id } = self {
            let payload = json!({
                "op": 18,
                "d": { "type": "guild", "guild_id": guild_id, "channel_id": channel_id, "preferred_region": null },
            });
            return WsMessage::Text(payload.to_string());
        }
        if let Command::PauseStream { stream_key, paused } = self {
            return WsMessage::Text(json!({ "op": 22, "d": { "stream_key": stream_key, "paused": paused } }).to_string());
        }
        if let Command::WatchStream(key) | Command::StopStream(key) = self {
            let op = if matches!(self, Command::WatchStream(_)) { 20 } else { 19 };
            return WsMessage::Text(json!({ "op": op, "d": { "stream_key": key } }).to_string());
        }
        if let Command::Presence { status, custom } = self {
            let activities: Vec<Value> = custom
                .iter()
                .map(|text| json!({ "type": 4, "name": "Custom Status", "state": text }))
                .collect();
            let payload = json!({
                "op": 3,
                "d": { "status": status, "since": 0, "activities": activities, "afk": false },
            });
            return WsMessage::Text(payload.to_string());
        }
        // Op 37 GUILD_SUBSCRIPTIONS_BULK.
        let subscription = match self {
            Command::VoiceState { .. }
            | Command::Presence { .. }
            | Command::WatchStream(_)
            | Command::StopStream(_)
            | Command::CreateStream { .. }
            | Command::PauseStream { .. } => unreachable!("handled above"),
            // No member list ranges: also stops the sidebar updates of a server
            // you've switched away from (busy ones send several a second).
            Command::SubscribeGuild(guild_id) => json!({
                guild_id: { "typing": true, "activities": false, "threads": true, "channels": {} },
            }),
            Command::SubscribeMembers { guild_id, channel_id } => json!({
                guild_id: {
                    "typing": true,
                    "activities": false,
                    "threads": true,
                    "channels": { channel_id: [[0, member_list::LIMIT - 1]] },
                },
            }),
        };
        WsMessage::Text(json!({ "op": 37, "d": { "subscriptions": subscription } }).to_string())
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    /// Sent once per stream; queue [`Command`]s here.
    Commands(mpsc::Sender<Command>),
    Connecting,
    Ready(Ready),
    TypingStart(TypingStart),
    ReactionAdd(ReactionEvent),
    ReactionRemove(ReactionEvent),
    ReactionClear(ReactionClear),
    MemberList(MemberListUpdate),
    VoiceState(VoiceState),
    VoiceServer(VoiceServer),
    GuildCreate(Guild),
    MessageCreate(Message),
    MessageUpdate(MessageUpdate),
    MessageDelete(MessageDelete),
    GuildSettings(GuildSettings),
    UserSettings(UserSettings),
    /// THREAD_CREATE / THREAD_UPDATE.
    Thread(Channel),
    ThreadDelete(ThreadDelete),
    ThreadListSync(ThreadListSync),
    PollVote { vote: PollVote, add: bool },
    Stickers { guild_id: String, stickers: Vec<Sticker> },
    /// An app didn't answer our interaction (by nonce).
    InteractionFailed(String),
    StreamCreate(StreamCreate),
    StreamServer(StreamServer),
    /// The stream we watch ended (by stream key).
    StreamDelete(String),
    /// RELATIONSHIP_ADD / RELATIONSHIP_UPDATE (an update may leave `user` out).
    Relationship(Relationship),
    /// By the other user's id.
    RelationshipRemove(String),
    /// (user id, status).
    Presence(String, String),
    /// A DM, group DM or server channel appeared or changed.
    ChannelUpsert(Channel),
    ChannelDelete(Channel),
    /// You left (or were removed from) a server.
    GuildDelete(String),
    Warning(String),
    Disconnected(String),
    /// The gateway rejected the token; the stream ends after this.
    AuthFailed(String),
    /// A dropped connection was resumed: nothing was lost, and READY isn't sent again.
    Resumed,
}

enum End {
    /// Server asked us to reconnect (op 7, or op 9 when resumable).
    Reconnect,
    Retry(String),
    /// Closed for an unknown or malformed opcode (4001/4002).
    Rejected(String),
    /// The token was rejected (4004).
    Fatal(String),
    /// Bot shard or intent settings were rejected (4010–4014): retrying can't
    /// help, but the token is fine, so it isn't forgotten.
    Refused(String),
    UiGone,
}

/// The payload's data is kept as unparsed JSON text and read straight into
/// the event's own type, instead of first building a generic tree of it
/// (READY alone can be megabytes). Events Ferrochat doesn't use aren't parsed.
#[derive(Deserialize)]
struct Payload<'a> {
    op: u8,
    #[serde(borrow, default)]
    d: Option<&'a RawValue>,
    s: Option<u64>,
    t: Option<&'a str>,
}

/// What resuming a dropped session needs (from READY, and the last sequence number).
struct Resume {
    session_id: String,
    url: String,
    seq: Option<u64>,
}

/// Close codes after which the session can't be resumed: start a new one.
fn resumable(code: u16) -> bool {
    !matches!(code, 4004 | 4007 | 4009 | 4010..=4014)
}

/// Gateway connection that reconnects with backoff until the token is rejected
/// or the subscription is dropped.
pub fn connect(token: String) -> impl Stream<Item = Event> {
    iced::stream::channel(64, move |mut out: mpsc::Sender<Event>| async move {
        let (command_sender, mut commands) = mpsc::channel(128);
        if out.send(Event::Commands(command_sender)).await.is_err() {
            return;
        }
        // Cleared if the gateway rejects our commands, so they cannot cause a reconnect loop.
        let mut commands_enabled = true;
        let mut backoff = 1;
        // Set after READY: the next connection resumes this session instead of starting over.
        let mut resume: Option<Resume> = None;
        loop {
            if out.send(Event::Connecting).await.is_err() {
                return;
            }
            let (end, reached_ready) = session(&token, &mut out, &mut commands, commands_enabled, &mut resume).await;
            if reached_ready {
                backoff = 1;
            }
            match end {
                End::Reconnect => backoff = 1,
                End::Rejected(reason) if commands_enabled => {
                    commands_enabled = false;
                    let warning = format!("{reason}; live guild subscriptions disabled");
                    if out.send(Event::Warning(warning)).await.is_err() {
                        return;
                    }
                }
                End::Retry(reason) | End::Rejected(reason) => {
                    if out.send(Event::Disconnected(reason)).await.is_err() {
                        return;
                    }
                }
                End::Fatal(reason) => {
                    let _ = out.send(Event::AuthFailed(reason)).await;
                    return;
                }
                End::Refused(reason) => {
                    let _ = out.send(Event::Warning(reason)).await;
                    return;
                }
                End::UiGone => return,
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(60);
        }
    })
}

/// One websocket session. Returns why it ended and whether READY was received.
async fn session(
    token: &str,
    out: &mut mpsc::Sender<Event>,
    commands: &mut mpsc::Receiver<Command>,
    commands_enabled: bool,
    resume: &mut Option<Resume>,
) -> (End, bool) {
    let url = match resume.as_ref() {
        Some(r) => format!("{}/?v=10&encoding=json", r.url.trim_end_matches('/')),
        None => GATEWAY_URL.to_owned(),
    };
    let (socket, _) = match tokio_tungstenite::connect_async(url).await {
        Ok(ok) => ok,
        Err(err) => return (End::Retry(format!("connect failed: {err}")), false),
    };
    let (mut sink, mut stream) = socket.split();

    let interval = match stream.next().await {
        Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Payload>(&text) {
            Ok(hello) if hello.op == 10 => hello
                .d
                .and_then(|d| serde_json::from_str::<Value>(d.get()).ok())
                .and_then(|d| d["heartbeat_interval"].as_u64())
                .unwrap_or(41_250),
            _ => return (End::Retry("expected Hello from gateway".into()), false),
        },
        Some(Ok(WsMessage::Close(frame))) => return (closed(frame, resume), false),
        Some(Err(err)) => return (End::Retry(err.to_string()), false),
        _ => return (End::Retry("gateway closed before Hello".into()), false),
    };

    // Resume the last session when there is one (missed events are replayed),
    // otherwise log in afresh.
    let hello = match resume.as_ref() {
        Some(r) => json!({ "op": 6, "d": { "token": token, "session_id": r.session_id, "seq": r.seq } }),
        None => identify(token),
    };
    if let Err(err) = sink.send(WsMessage::Text(hello.to_string())).await {
        return (End::Retry(err.to_string()), false);
    }

    let mut ready = false;
    let mut seq: Option<u64> = resume.as_ref().and_then(|r| r.seq);
    let mut acked = true;
    let mut heartbeat = tokio::time::interval(Duration::from_millis(interval));
    heartbeat.tick().await; // the first tick completes immediately

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if !acked {
                    return (End::Retry("heartbeat not acknowledged".into()), ready);
                }
                acked = false;
                if let Err(err) = sink.send(heartbeat_frame(seq)).await {
                    return (End::Retry(err.to_string()), ready);
                }
            }
            command = commands.next(), if ready => {
                let Some(command) = command else { return (End::UiGone, ready) };
                if !commands_enabled && command.is_subscription() {
                    continue;
                }
                if let Err(err) = sink.send(command.frame()).await {
                    return (End::Retry(err.to_string()), ready);
                }
            }
            frame = stream.next() => {
                let text = match frame {
                    Some(Ok(WsMessage::Text(text))) => text,
                    Some(Ok(WsMessage::Close(frame))) => return (closed(frame, resume), ready),
                    Some(Ok(_)) => continue,
                    Some(Err(err)) => return (End::Retry(err.to_string()), ready),
                    None => return (End::Retry("gateway connection closed".into()), ready),
                };
                let Ok(payload) = serde_json::from_str::<Payload>(&text) else { continue };
                match payload.op {
                    0 => {
                        if payload.s.is_some() {
                            seq = payload.s;
                            if let Some(r) = resume.as_mut() {
                                r.seq = seq;
                            }
                        }
                        let raw = payload.d.map_or("null", RawValue::get);
                        match payload.t {
                            Some("READY") => {
                                #[derive(Deserialize)]
                                struct Session {
                                    session_id: String,
                                    #[serde(default)]
                                    resume_gateway_url: Option<String>,
                                }
                                *resume = serde_json::from_str::<Session>(raw).ok().map(|s| Resume {
                                    session_id: s.session_id,
                                    url: s.resume_gateway_url.unwrap_or_else(|| "wss://gateway.discord.gg".to_owned()),
                                    seq,
                                });
                            }
                            Some("RESUMED") => {
                                ready = true;
                                if out.send(Event::Resumed).await.is_err() {
                                    return (End::UiGone, ready);
                                }
                                continue;
                            }
                            _ => {}
                        }
                        let Some(event) = dispatch(payload.t, raw) else { continue };
                        ready |= matches!(event, Event::Ready(_));
                        if out.send(event).await.is_err() {
                            return (End::UiGone, ready);
                        }
                    }
                    1 => {
                        if let Err(err) = sink.send(heartbeat_frame(seq)).await {
                            return (End::Retry(err.to_string()), ready);
                        }
                    }
                    7 => return (End::Reconnect, ready),
                    // Invalid session: `true` may be resumed, `false` needs a new one.
                    9 => {
                        if payload.d.is_none_or(|d| d.get() != "true") {
                            *resume = None;
                        }
                        return (End::Reconnect, ready);
                    }
                    11 => acked = true,
                    _ => {}
                }
            }
        }
    }
}

fn identify(token: &str) -> Value {
    match token.strip_prefix("Bot ") {
        Some(bot) => json!({
            "op": 2,
            "d": {
                "token": bot,
                "intents": BOT_INTENTS,
                "properties": { "os": "windows", "browser": "ferrochat", "device": "ferrochat" },
            },
        }),
        None => json!({
            "op": 2,
            "d": {
                "token": token,
                "capabilities": 0,
                "compress": false,
                "properties": { "os": "Windows", "browser": "ferrochat", "device": "" },
                "presence": { "status": "online", "since": 0, "activities": [], "afk": false },
            },
        }),
    }
}

fn heartbeat_frame(seq: Option<u64>) -> WsMessage {
    WsMessage::Text(json!({ "op": 1, "d": seq }).to_string())
}

fn closed(frame: Option<CloseFrame<'_>>, resume: &mut Option<Resume>) -> End {
    let Some(frame) = frame else {
        return End::Retry("gateway closed".into());
    };
    let code = u16::from(frame.code);
    if !resumable(code) {
        *resume = None;
    }
    let reason = format!("gateway closed ({code}): {}", frame.reason);
    match code {
        4004 => End::Fatal(reason),
        // Bot shard or intent settings, e.g. Message Content not enabled for the bot.
        4010..=4014 => End::Refused(reason),
        4001 | 4002 => End::Rejected(reason),
        _ => End::Retry(reason),
    }
}

fn dispatch(kind: Option<&str>, raw: &str) -> Option<Event> {
    // Typed events read the JSON text directly; the few that pick single fields parse a tree.
    let tree =|| serde_json::from_str::<Value>(raw).ok();
    Some(match kind? {
        "READY" => match serde_json::from_str::<RawReady>(raw) {
            Ok(raw) => Event::Ready(raw.into()),
            Err(err) => Event::Warning(format!("could not read READY: {err}")),
        },
        "GUILD_CREATE" => Event::GuildCreate(serde_json::from_str::<RawGuild>(raw).ok()?.into()),
        "MESSAGE_CREATE" => Event::MessageCreate(serde_json::from_str(raw).ok()?),
        "MESSAGE_UPDATE" => Event::MessageUpdate(serde_json::from_str(raw).ok()?),
        "MESSAGE_DELETE" => Event::MessageDelete(serde_json::from_str(raw).ok()?),
        "TYPING_START" => Event::TypingStart(serde_json::from_str(raw).ok()?),
        "MESSAGE_REACTION_ADD" => Event::ReactionAdd(serde_json::from_str(raw).ok()?),
        "MESSAGE_REACTION_REMOVE" => Event::ReactionRemove(serde_json::from_str(raw).ok()?),
        "MESSAGE_REACTION_REMOVE_ALL" | "MESSAGE_REACTION_REMOVE_EMOJI" => {
            Event::ReactionClear(serde_json::from_str(raw).ok()?)
        }
        "GUILD_MEMBER_LIST_UPDATE" => Event::MemberList(serde_json::from_str(raw).ok()?),
        "VOICE_STATE_UPDATE" => Event::VoiceState(serde_json::from_str(raw).ok()?),
        "VOICE_SERVER_UPDATE" => Event::VoiceServer(serde_json::from_str(raw).ok()?),
        "USER_GUILD_SETTINGS_UPDATE" => Event::GuildSettings(serde_json::from_str(raw).ok()?),
        "USER_SETTINGS_UPDATE" => Event::UserSettings(serde_json::from_str(raw).ok()?),
        "THREAD_CREATE" | "THREAD_UPDATE" => Event::Thread(serde_json::from_str(raw).ok()?),
        "THREAD_DELETE" => Event::ThreadDelete(serde_json::from_str(raw).ok()?),
        "THREAD_LIST_SYNC" => Event::ThreadListSync(serde_json::from_str(raw).ok()?),
        "MESSAGE_POLL_VOTE_ADD" => Event::PollVote { vote: serde_json::from_str(raw).ok()?, add: true },
        "MESSAGE_POLL_VOTE_REMOVE" => Event::PollVote { vote: serde_json::from_str(raw).ok()?, add: false },
        "GUILD_STICKERS_UPDATE" => {
            #[derive(Deserialize)]
            struct Update {
                guild_id: String,
                #[serde(default)]
                stickers: Vec<Sticker>,
            }
            let update: Update = serde_json::from_str(raw).ok()?;
            Event::Stickers { guild_id: update.guild_id, stickers: update.stickers }
        }
        "INTERACTION_FAILURE" => Event::InteractionFailed(tree()?["nonce"].as_str()?.to_owned()),
        "STREAM_CREATE" | "STREAM_UPDATE" => Event::StreamCreate(serde_json::from_str(raw).ok()?),
        "STREAM_SERVER_UPDATE" => Event::StreamServer(serde_json::from_str(raw).ok()?),
        "STREAM_DELETE" => Event::StreamDelete(tree()?["stream_key"].as_str()?.to_owned()),
        "RELATIONSHIP_ADD" | "RELATIONSHIP_UPDATE" => Event::Relationship(serde_json::from_str(raw).ok()?),
        "RELATIONSHIP_REMOVE" => Event::RelationshipRemove(tree()?["id"].as_str()?.to_owned()),
        "PRESENCE_UPDATE" => {
            // Only friends' statuses are used. Server members' updates (busy
            // servers send many a second) carry a guild_id; they're dropped here
            // instead of waking the UI for each.
            let data = tree()?;
            if data.get("guild_id").is_some_and(|g| !g.is_null()) {
                return None;
            }
            let (id, status) = presence_of(&data)?;
            Event::Presence(id, status)
        }
        "CHANNEL_CREATE" | "CHANNEL_UPDATE" => Event::ChannelUpsert(serde_json::from_str(raw).ok()?),
        "CHANNEL_DELETE" => Event::ChannelDelete(serde_json::from_str(raw).ok()?),
        // `unavailable` means an outage, not that you left.
        "GUILD_DELETE" => {
            let data = tree()?;
            if data["unavailable"].as_bool().unwrap_or(false) {
                return None;
            }
            Event::GuildDelete(data["id"].as_str()?.to_owned())
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_events_from_raw_json() {
        let message = r#"{"id":"9","channel_id":"2","author":{"id":"1","username":"a"},"content":"hi","timestamp":"2026-11-05T09:15:00+00:00"}"#;
        assert!(matches!(dispatch(Some("MESSAGE_CREATE"), message), Some(Event::MessageCreate(m)) if m.content == "hi"));
        assert!(matches!(dispatch(Some("RELATIONSHIP_REMOVE"), r#"{"id":"5","type":1}"#), Some(Event::RelationshipRemove(id)) if id == "5"));
        // An outage isn't leaving the server.
        assert!(dispatch(Some("GUILD_DELETE"), r#"{"id":"7","unavailable":true}"#).is_none());
        assert!(matches!(dispatch(Some("GUILD_DELETE"), r#"{"id":"7"}"#), Some(Event::GuildDelete(id)) if id == "7"));
        assert!(dispatch(Some("SOMETHING_NEW"), "{}").is_none(), "unknown events aren't parsed");
        let friend = r#"{"user":{"id":"3"},"status":"idle"}"#;
        assert!(matches!(dispatch(Some("PRESENCE_UPDATE"), friend), Some(Event::Presence(id, s)) if id == "3" && s == "idle"));
        let member = r#"{"user":{"id":"3"},"status":"idle","guild_id":"100"}"#;
        assert!(dispatch(Some("PRESENCE_UPDATE"), member).is_none(), "server members' statuses aren't used");
        assert!(dispatch(Some("MESSAGE_CREATE"), "not json").is_none());
    }

    #[test]
    fn payload_keeps_data_unparsed() {
        let text = r#"{"op":0,"s":42,"t":"TYPING_START","d":{"channel_id":"1","user_id":"2","timestamp":5}}"#;
        let payload: Payload = serde_json::from_str(text).unwrap();
        assert_eq!((payload.op, payload.s, payload.t), (0, Some(42), Some("TYPING_START")));
        assert!(payload.d.unwrap().get().starts_with(r#"{"channel_id""#));
        let invalid: Payload = serde_json::from_str(r#"{"op":9,"d":false}"#).unwrap();
        assert_eq!(invalid.d.map(RawValue::get), Some("false"));
    }

    #[test]
    fn some_close_codes_end_the_session() {
        assert!(resumable(1000) && resumable(4000) && resumable(1006));
        assert!(!resumable(4007) && !resumable(4009) && !resumable(4004));
        let frame = |code: u16| Some(CloseFrame { code: code.into(), reason: "".into() });
        assert!(matches!(closed(frame(4004), &mut None), End::Fatal(_)), "a rejected token");
        assert!(matches!(closed(frame(4014), &mut None), End::Refused(_)), "a bot's intents: its token is kept");
        assert!(matches!(closed(frame(4000), &mut None), End::Retry(_)));
    }
}
