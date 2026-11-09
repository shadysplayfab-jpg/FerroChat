use std::path::PathBuf;

use reqwest::multipart::{Form, Part};
pub use reqwest::Method;
use reqwest::{Client, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use super::model::{snowflake, Channel, Command, CommandOption, GuildSettings, Message, Profile};

/// A GIF search result. Sending `url` (the Tenor page) makes Discord embed it.
#[derive(Debug, Clone, Deserialize)]
pub struct Gif {
    pub url: String,
    /// The GIF itself on media.tenor.com.
    #[serde(default)]
    pub src: String,
    /// A still frame.
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
}

/// A page of forum posts, with each post's opening message when Discord sends it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ForumPosts {
    #[serde(default)]
    pub threads: Vec<Channel>,
    #[serde(default)]
    pub first_messages: Vec<Message>,
}
use crate::images::MAX_IMAGE_BYTES;

/// Where a history request starts.
#[derive(Debug, Clone)]
pub enum Cursor {
    Latest,
    Before(String),
    /// Centred on a message (jumping to a reply, link or search result).
    Around(String),
}

/// A file to attach, read from disk only when the message is sent.
#[derive(Debug, Clone)]
pub struct Upload {
    pub path: PathBuf,
    pub name: String,
}

const API: &str = "https://discord.com/api/v10";
/// Messages per history request (Discord's maximum is 100).
pub const PAGE_SIZE: usize = 50;
/// Largest audio file played in the app (10 MB is Discord's upload limit without Nitro).
pub const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;

/// REST client. Deliberately not `Debug`: it holds the token.
#[derive(Clone)]
pub struct Rest {
    http: Client,
    auth: String,
}

impl Rest {
    /// `token` is sent verbatim as `Authorization`, so bot tokens keep their `Bot ` prefix.
    pub fn new(token: &str) -> Self {
        let http = Client::builder()
            .user_agent(concat!("ferrochat/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self { http, auth: token.to_owned() }
    }

    /// Up to [`PAGE_SIZE`] messages at `cursor`, oldest first.
    pub async fn messages(self, channel_id: String, cursor: Cursor) -> Result<Vec<Message>, String> {
        let mut path = format!("/channels/{channel_id}/messages?limit={PAGE_SIZE}");
        match cursor {
            Cursor::Latest => {}
            Cursor::Before(id) => path.push_str(&format!("&before={id}")),
            Cursor::Around(id) => path.push_str(&format!("&around={id}")),
        }
        let mut messages: Vec<Message> = self.call_as(Method::GET, path, None).await?;
        // Discord sends newest first; `around` results are not always sorted.
        messages.sort_by_key(|m| snowflake(&m.id));
        Ok(messages)
    }

    /// Message search in a server, or in one DM when `guild_id` is `None`. Newest first.
    pub async fn search(self, guild_id: Option<String>, channel_id: String, query: String) -> Result<Vec<Message>, String> {
        #[derive(Deserialize)]
        struct Results {
            /// Each hit comes wrapped in a list (once with surrounding context).
            #[serde(default)]
            messages: Vec<Vec<Message>>,
        }
        let url = match &guild_id {
            Some(guild_id) => format!("{API}/guilds/{guild_id}/messages/search"),
            None => format!("{API}/channels/{channel_id}/messages/search"),
        };
        let response = self
            .http
            .get(url)
            .query(&[("content", query.as_str())])
            .header("Authorization", &self.auth)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        // 202: Discord is still indexing this server.
        if response.status() == StatusCode::ACCEPTED {
            return Err("Discord is still indexing this server; try again in a few seconds.".to_owned());
        }
        let results: Results = parse(response).await?;
        Ok(results.messages.into_iter().filter_map(|hit| hit.into_iter().next()).collect())
    }

    /// Pinned messages, newest first.
    pub async fn pins(self, channel_id: String) -> Result<Vec<Message>, String> {
        #[derive(Deserialize)]
        struct Pin {
            message: Message,
        }
        #[derive(Deserialize)]
        struct Pins {
            #[serde(default)]
            items: Vec<Pin>,
        }
        // The paginated endpoint replaced the old list; fall back for older API behaviour.
        let current = self.clone().call_as::<Pins>(Method::GET, format!("/channels/{channel_id}/messages/pins?limit=50"), None).await;
        match current {
            Ok(pins) => Ok(pins.items.into_iter().map(|p| p.message).collect()),
            Err(_) => self.call_as(Method::GET, format!("/channels/{channel_id}/pins"), None).await,
        }
    }

    /// Bio and server membership (user logins only).
    pub async fn profile(self, user_id: String, guild_id: Option<String>) -> Result<Profile, String> {
        let mut path = format!("/users/{user_id}/profile?with_mutual_guilds=false");
        if let Some(guild_id) = guild_id {
            path.push_str(&format!("&guild_id={guild_id}"));
        }
        self.call_as(Method::GET, path, None).await
    }

    /// The DM channel with `user_id`, created if needed.
    pub async fn open_dm(self, user_id: String) -> Result<Channel, String> {
        self.call_as(Method::POST, "/users/@me/channels".to_owned(), Some(json!({ "recipient_id": user_id }))).await
    }

    /// Changes your notification settings for a server (`None`: direct messages).
    /// `changes` is a partial settings object; returns the full new settings.
    pub async fn guild_settings(self, guild_id: Option<String>, changes: Value) -> Result<GuildSettings, String> {
        let guild = guild_id.as_deref().unwrap_or("@me");
        self.call_as(Method::PATCH, format!("/users/@me/guilds/{guild}/settings"), Some(changes)).await
    }

    /// Saves status and custom status to your Discord settings, so they survive
    /// reconnects and show in other clients.
    pub async fn save_status(self, status: String, custom: Option<String>) -> Result<(), String> {
        let custom = custom.map(|text| json!({ "text": text }));
        let body = json!({ "status": status, "custom_status": custom });
        self.call(Method::PATCH, "/users/@me/settings".to_owned(), Some(body)).await.map(drop)
    }

    /// Sends a message built by the caller (polls, stickers).
    pub async fn send_json(self, channel_id: String, body: Value) -> Result<Message, String> {
        self.call_as(Method::POST, format!("/channels/{channel_id}/messages"), Some(body)).await
    }

    /// Replaces our poll votes (an empty list removes them).
    pub async fn vote(self, channel_id: String, message_id: String, answers: Vec<u32>) -> Result<(), String> {
        let answer_ids: Vec<String> = answers.iter().map(u32::to_string).collect();
        let path = format!("/channels/{channel_id}/polls/{message_id}/answers/@me");
        self.call(Method::PUT, path, Some(json!({ "answer_ids": answer_ids }))).await.map(drop)
    }

    /// Button presses, menu choices and slash commands. `payload` is the whole
    /// interaction; the result only says Discord accepted it, and the app answers
    /// with a message (or an INTERACTION_FAILURE event).
    pub async fn interact(self, payload: Value) -> Result<(), String> {
        let form = Form::new().text("payload_json", payload.to_string());
        let response = self
            .http
            .post(format!("{API}/interactions"))
            .header("Authorization", &self.auth)
            .multipart(form)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        check(response).await.map(drop)
    }

    /// Slash commands usable in a server (`guild_id`) or a DM (user logins).
    pub async fn commands(self, guild_id: Option<String>, channel_id: String) -> Result<Vec<Command>, String> {
        #[derive(Deserialize)]
        struct App {
            id: String,
            #[serde(default)]
            name: String,
        }
        #[derive(Deserialize)]
        struct Index {
            #[serde(default)]
            applications: Vec<App>,
            #[serde(default)]
            application_commands: Vec<Value>,
        }
        let path = match guild_id {
            Some(guild_id) => format!("/guilds/{guild_id}/application-command-index"),
            None => format!("/channels/{channel_id}/application-command-index"),
        };
        let index: Index = self.call_as(Method::GET, path, None).await?;
        let commands = index.application_commands.into_iter().filter_map(|raw| {
            #[derive(Deserialize)]
            struct Raw {
                id: String,
                application_id: String,
                #[serde(default)]
                version: String,
                name: String,
                #[serde(default)]
                description: String,
                /// 1 = slash command; user and message commands are context-menu only.
                #[serde(rename = "type", default = "chat_input")]
                kind: u8,
                #[serde(default)]
                options: Vec<CommandOption>,
            }
            fn chat_input() -> u8 {
                1
            }
            let parsed: Raw = serde_json::from_value(raw.clone()).ok()?;
            (parsed.kind == 1).then(|| Command {
                app_name: index
                    .applications
                    .iter()
                    .find(|a| a.id == parsed.application_id)
                    .map_or_else(String::new, |a| a.name.clone()),
                id: parsed.id,
                application_id: parsed.application_id,
                version: parsed.version,
                name: parsed.name,
                description: parsed.description,
                options: parsed.options,
                raw,
            })
        });
        Ok(commands.collect())
    }

    /// Open posts in a forum, most recently active first (user logins).
    pub async fn forum_posts(self, forum_id: String) -> Result<ForumPosts, String> {
        let path = format!("/channels/{forum_id}/threads/search?archived=false&sort_by=last_message_time&sort_order=desc&limit=25");
        self.call_as(Method::GET, path, None).await
    }

    /// Starts a forum post (a thread with its first message).
    pub async fn create_post(self, forum_id: String, title: String, content: String) -> Result<Channel, String> {
        let body = json!({ "name": title, "message": { "content": content } });
        self.call_as(Method::POST, format!("/channels/{forum_id}/threads"), Some(body)).await
    }

    /// GIFs from Discord's Tenor search; an empty query gives trending ones.
    pub async fn gifs(self, query: String) -> Result<Vec<Gif>, String> {
        let request = if query.trim().is_empty() {
            self.http.get(format!("{API}/gifs/trending-gifs"))
        } else {
            self.http.get(format!("{API}/gifs/search")).query(&[("q", query.as_str())])
        };
        let response = request
            .query(&[("media_format", "gif"), ("provider", "tenor"), ("locale", "en-US")])
            .header("Authorization", &self.auth)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        parse(response).await
    }

    /// One channel or thread (archived threads aren't in the guild data).
    pub async fn channel(self, channel_id: String) -> Result<Channel, String> {
        self.call_as(Method::GET, format!("/channels/{channel_id}"), None).await
    }

    /// Any other API call, by method and path below `/api/v10`. Returns the JSON
    /// answer, or `Null` for an empty one (204).
    pub async fn call(self, method: Method, path: String, body: Option<Value>) -> Result<Value, String> {
        let mut request = self.http.request(method, format!("{API}{path}")).header("Authorization", &self.auth);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = check(request.send().await.map_err(|e| e.to_string())?).await?;
        let bytes = response.bytes().await.map_err(|e| e.to_string())?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }

    /// [`Rest::call`], read into `T`.
    async fn call_as<T: DeserializeOwned>(self, method: Method, path: String, body: Option<Value>) -> Result<T, String> {
        serde_json::from_value(self.call(method, path, body).await?).map_err(|e| e.to_string())
    }

    /// Mentions of you across servers and DMs, newest first (user logins).
    pub async fn mentions(self) -> Result<Vec<Message>, String> {
        self.call_as(Method::GET, "/users/@me/mentions?limit=50&roles=true&everyone=true".to_owned(), None).await
    }

    /// Audio attachments for playback, at most [`MAX_AUDIO_BYTES`]. Sent without the token.
    pub async fn audio(self, url: String) -> Result<Vec<u8>, String> {
        self.download(url, MAX_AUDIO_BYTES, "file too large to play").await
    }

    /// Public CDN asset, at most [`MAX_IMAGE_BYTES`]. Sent without the token.
    pub async fn cdn(self, url: String) -> Result<Vec<u8>, String> {
        self.download(url, MAX_IMAGE_BYTES, "image too large").await
    }

    /// A file of at most `max` bytes, fetched without the token.
    async fn download(self, url: String, max: usize, too_large: &'static str) -> Result<Vec<u8>, String> {
        let response = check(self.http.get(url).send().await.map_err(|e| e.to_string())?).await?;
        if response.content_length().is_some_and(|n| n > max as u64) {
            return Err(too_large.to_owned());
        }
        let bytes = response.bytes().await.map_err(|e| e.to_string())?;
        if bytes.len() > max {
            return Err(too_large.to_owned());
        }
        Ok(bytes.to_vec())
    }

    /// Shows our typing indicator for ~10 seconds.
    pub async fn typing(self, channel_id: String) -> Result<(), String> {
        self.call(Method::POST, format!("/channels/{channel_id}/typing"), None).await.map(drop)
    }

    /// Marks the channel read up to `message_id` (user sessions only).
    pub async fn ack(self, channel_id: String, message_id: String) -> Result<(), String> {
        let path = format!("/channels/{channel_id}/messages/{message_id}/ack");
        self.call(Method::POST, path, Some(json!({ "token": null }))).await.map(drop)
    }

    /// `reply`: (message id, whether to ping its author).
    pub async fn send(
        self,
        channel_id: String,
        content: String,
        reply: Option<(String, bool)>,
        uploads: Vec<Upload>,
    ) -> Result<Message, String> {
        let mut body = json!({ "content": content });
        if let Some((message_id, ping)) = reply {
            body["message_reference"] = json!({ "message_id": message_id, "fail_if_not_exists": false });
            // Same mentions as a plain message, plus the replied-to author when pinging.
            body["allowed_mentions"] = json!({ "parse": ["users", "roles", "everyone"], "replied_user": ping });
        }
        let request = self
            .http
            .post(format!("{API}/channels/{channel_id}/messages"))
            .header("Authorization", &self.auth);
        let request = if uploads.is_empty() {
            request.json(&body)
        } else {
            let attachments: Vec<_> =
                uploads.iter().enumerate().map(|(id, u)| json!({ "id": id, "filename": u.name })).collect();
            body["attachments"] = json!(attachments);
            let mut form = Form::new().text("payload_json", body.to_string());
            for (index, upload) in uploads.into_iter().enumerate() {
                let bytes = tokio::fs::read(&upload.path)
                    .await
                    .map_err(|e| format!("could not read {}: {e}", upload.name))?;
                form = form.part(format!("files[{index}]"), Part::bytes(bytes).file_name(upload.name));
            }
            request.multipart(form)
        };
        parse(request.send().await.map_err(|e| e.to_string())?).await
    }

    /// Adds (`add`) or removes our reaction. `emoji` is from [`ReactionEmoji::api_name`].
    pub async fn react(self, channel_id: String, message_id: String, emoji: String, add: bool) -> Result<(), String> {
        let path = format!("/channels/{channel_id}/messages/{message_id}/reactions/{}/@me", encode_segment(&emoji));
        self.call(if add { Method::PUT } else { Method::DELETE }, path, None).await.map(drop)
    }

    pub async fn edit(self, channel_id: String, message_id: String, content: String) -> Result<Message, String> {
        let path = format!("/channels/{channel_id}/messages/{message_id}");
        self.call_as(Method::PATCH, path, Some(json!({ "content": content }))).await
    }

    pub async fn delete(self, channel_id: String, message_id: String) -> Result<(), String> {
        self.call(Method::DELETE, format!("/channels/{channel_id}/messages/{message_id}"), None).await.map(drop)
    }
}

/// What Discord answered to an email and password login.
#[derive(Debug, Clone)]
pub enum LoginStep {
    Token(String),
    /// Two-factor is on: send the code with this ticket and login instance.
    NeedsCode(CodeTicket),
    /// Discord wants an hCaptcha solved first; log in again with its answer.
    Captcha(CaptchaRequest),
}

/// What the two-factor step needs from the password step. Discord only
/// accepts the code together with both (the web app sends both too).
#[derive(Debug, Clone)]
pub struct CodeTicket {
    pub ticket: String,
    pub login_instance_id: Option<String>,
    fingerprint: Option<String>,
}

/// One connection for every login step, keeping Discord's cookies between
/// them, with the headers Discord's own web app sends.
fn login_client() -> &'static Client {
    static CLIENT: std::sync::OnceLock<Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        let properties = json!({
            "os": "Windows", "browser": "Chrome", "device": "", "system_locale": "en-US",
            "browser_user_agent": LOGIN_USER_AGENT, "browser_version": "131.0.0.0", "os_version": "10",
            "referrer": "", "referring_domain": "", "release_channel": "stable", "client_build_number": 9999,
        });
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&base64(properties.to_string().as_bytes())) {
            headers.insert("X-Super-Properties", value);
        }
        headers.insert("X-Discord-Locale", reqwest::header::HeaderValue::from_static("en-US"));
        Client::builder()
            .cookie_store(true)
            .user_agent(LOGIN_USER_AGENT)
            .default_headers(headers)
            .build()
            .unwrap_or_else(|_| Client::new())
    })
}

const LOGIN_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Standard base64 (for the X-Super-Properties header).
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { TABLE[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// The browser fingerprint Discord's web app gets before logging in, and
/// sends with each login step.
async fn fingerprint() -> Option<String> {
    let answer: Value = login_client().get(format!("{AUTH_API}/experiments")).send().await.ok()?.json().await.ok()?;
    answer["fingerprint"].as_str().map(str::to_owned)
}

/// The API version Discord's web app logs in with.
const AUTH_API: &str = "https://discord.com/api/v9";

/// A captcha Discord asked for, as it sent it.
#[derive(Debug, Clone)]
pub struct CaptchaRequest {
    pub sitekey: String,
    /// Extra data for hCaptcha, sent back as is.
    pub rqdata: Option<String>,
    /// Sent with the answer, so Discord matches the two.
    pub rqtoken: Option<String>,
}

/// A solved captcha to send along with a login.
#[derive(Debug, Clone)]
pub struct CaptchaAnswer {
    pub key: String,
    pub rqtoken: Option<String>,
}

/// Email (or phone) and password login, with a solved captcha when Discord
/// asked for one. The password only travels in this request to Discord; it's
/// never stored.
pub async fn login_with_password(login: String, password: String, captcha: Option<CaptchaAnswer>) -> Result<LoginStep, String> {
    let body = json!({ "login": login, "password": password, "undelete": false, "login_source": null, "gift_code_sku_id": null });
    let fingerprint = fingerprint().await;
    let answer = match auth_request("/auth/login", body, captcha, fingerprint.as_deref()).await {
        Ok(answer) => answer,
        Err((_, answer)) if answer["captcha_sitekey"].is_string() => {
            let text = |name: &str| answer[name].as_str().map(str::to_owned);
            return Ok(LoginStep::Captcha(CaptchaRequest {
                sitekey: text("captcha_sitekey").unwrap_or_default(),
                rqdata: text("captcha_rqdata"),
                rqtoken: text("captcha_rqtoken"),
            }));
        }
        Err((status, answer)) => return Err(login_error(status, &answer)),
    };
    if let Some(token) = answer["token"].as_str() {
        return Ok(LoginStep::Token(token.to_owned()));
    }
    match answer["ticket"].as_str() {
        Some(ticket) if answer["mfa"].as_bool() == Some(true) => Ok(LoginStep::NeedsCode(CodeTicket {
            ticket: ticket.to_owned(),
            login_instance_id: answer["login_instance_id"].as_str().map(str::to_owned),
            fingerprint,
        })),
        _ => Err("Discord didn't send a login token.".to_owned()),
    }
}

/// Where a two-factor code goes: 6 digits come from an authenticator app,
/// anything else is a backup code.
fn code_kind(code: &str) -> &'static str {
    if code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()) {
        "totp"
    } else {
        "backup"
    }
}

/// The 6-digit authenticator code (or an 8-character backup code) for a login ticket.
pub async fn login_code(ticket: CodeTicket, code: String) -> Result<String, String> {
    let code: String = code.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    let body = json!({
        "code": code,
        "ticket": ticket.ticket,
        "login_source": null,
        "gift_code_sku_id": null,
        "login_instance_id": ticket.login_instance_id,
    });
    let answer = auth_request(&format!("/auth/mfa/{}", code_kind(&code)), body, None, ticket.fingerprint.as_deref())
        .await
        .map_err(|(status, answer)| login_error(status, &answer))?;
    answer["token"].as_str().map(str::to_owned).ok_or_else(|| "Discord didn't send a login token.".to_owned())
}

/// Discord's answer, or its refusal (status and body).
async fn auth_request(path: &str, body: Value, captcha: Option<CaptchaAnswer>, fingerprint: Option<&str>) -> Result<Value, (StatusCode, Value)> {
    let mut request = login_client().post(format!("{AUTH_API}{path}")).json(&body);
    if let Some(fingerprint) = fingerprint {
        request = request.header("X-Fingerprint", fingerprint);
    }
    if let Some(captcha) = captcha {
        request = request.header("X-Captcha-Key", captcha.key);
        if let Some(rqtoken) = captcha.rqtoken {
            request = request.header("X-Captcha-Rqtoken", rqtoken);
        }
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => return Err((StatusCode::SERVICE_UNAVAILABLE, json!({ "message": format!("Couldn't reach Discord: {error}") }))),
    };
    let status = response.status();
    let answer: Value = response.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        Ok(answer)
    } else {
        Err((status, answer))
    }
}

/// Discord's refusal, in words someone can act on.
fn login_error(status: StatusCode, answer: &Value) -> String {
    if answer.get("captcha_key").is_some() {
        return "Discord didn't accept the captcha. Try again, or use the token box below.".to_owned();
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return "Too many tries. Wait a minute, then try again.".to_owned();
    }
    // Field errors: {"errors": {"login": {"_errors": [{"code": "...", "message": "..."}]}}}
    let field = ["login", "password", "code", "ticket"].iter().map(|name| &answer["errors"][name]["_errors"][0]).find(|e| e.is_object());
    let message = field.and_then(|e| e["message"].as_str()).or(answer["message"].as_str());
    // Discord's own code, so a report says exactly what it refused.
    let code = field.and_then(|e| e["code"].as_str().map(str::to_owned)).or(answer["code"].as_u64().map(|c| c.to_string()));
    match (message, code) {
        (Some(message), Some(code)) => format!("{message} ({code})"),
        (Some(message), None) => message.to_owned(),
        (None, _) => format!("Login failed (HTTP {status})."),
    }
}

#[derive(Deserialize, Default)]
struct ApiError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    retry_after: Option<f64>,
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
fn encode_segment(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

async fn parse<T: DeserializeOwned>(response: Response) -> Result<T, String> {
    check(response).await?.json().await.map_err(|e| e.to_string())
}

async fn check(response: Response) -> Result<Response, String> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let error: ApiError = response.json().await.unwrap_or_default();
    Err(match (status, error.retry_after) {
        (StatusCode::TOO_MANY_REQUESTS, Some(after)) => format!("rate limited, retry in {after:.1}s"),
        _ if error.message.is_empty() => format!("HTTP {status}"),
        _ => format!("HTTP {status}: {}", error.message),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_errors_are_explained() {
        let captcha = json!({ "captcha_key": ["captcha-required"], "captcha_sitekey": "x" });
        assert!(login_error(StatusCode::BAD_REQUEST, &captcha).contains("captcha"), "a captcha answer Discord refused");
        let wrong = json!({ "code": 50035, "errors": { "login": { "_errors": [{ "code": "INVALID_LOGIN", "message": "Login or password is invalid." }] } } });
        assert_eq!(login_error(StatusCode::BAD_REQUEST, &wrong), "Login or password is invalid. (INVALID_LOGIN)");
        let code = json!({ "message": "Invalid two-factor code", "code": 60008 });
        assert_eq!(login_error(StatusCode::BAD_REQUEST, &code), "Invalid two-factor code (60008)");
        let ticket = json!({ "message": "Invalid Form Body", "code": 50035, "errors": { "ticket": { "_errors": [{ "code": "MFA_INVALID_TICKET", "message": "Invalid two-factor auth ticket" }] } } });
        assert_eq!(login_error(StatusCode::BAD_REQUEST, &ticket), "Invalid two-factor auth ticket (MFA_INVALID_TICKET)");
        assert!(login_error(StatusCode::TOO_MANY_REQUESTS, &Value::Null).contains("Too many tries"));
        assert_eq!(login_error(StatusCode::INTERNAL_SERVER_ERROR, &Value::Null), "Login failed (HTTP 500 Internal Server Error).");
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(base64(b""), "");
    }

    #[test]
    fn authenticator_and_backup_codes_go_to_their_own_endpoints() {
        assert_eq!(code_kind("123456"), "totp");
        assert_eq!(code_kind("abcd1234"), "backup");
        assert_eq!(code_kind("12345678"), "backup", "backup codes can be all digits, but they're 8 long");
    }
}
