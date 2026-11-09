# Ferrochat

A native Rust (iced) text client for Discord. It doesn't embed a browser, so it uses
far less RAM and CPU than the Electron app.

> **Account risk:** Discord's Terms of Service forbid third-party clients that log
> in with a user token. Accounts that do can be banned. Use a test account. A bot
> token (`Bot xxxxx`) is fully allowed.

## What REA showed about the official client (desktop 1.0.9267)

- `resources/app.asar` (3.4 MB) holds only 5 files: `bundle.js`,
  `splashScreenPreload.js`, `splash/index.html`, `package.json` and a quotes file.
  It's the updater, bootstrap and splash screen.
- `modules/discord_desktop_core-1/core.asar` is the desktop host. It creates the
  window, then calls `loadURL(URL_TO_LOAD)` on `WEBAPP_ENDPOINT` (discord.com), and
  exposes a `DiscordNative` bridge via `contextBridge.exposeInMainWorld`.
- So the real UI is a remote React web app running in Chromium. Most of the
  RAM and CPU goes there. The other `discord_*` modules are native add-ons (voice,
  krisp, overlay, game utils) that this client doesn't load.
- REA's full JS parse failed on this checkout (missing
  `native/windows/build/rea-windows-x64.node`). Only the file inventory ran.

## How Ferrochat works

| Piece | File | What it does |
|---|---|---|
| Gateway | `src/discord/gateway.rs` | WebSocket v10 JSON: Hello, Identify, heartbeat with ack check, dispatch, reconnect with backoff, stop on auth failure (4004) |
| REST | `src/discord/rest.rs` | `GET /channels/{id}/messages`, `POST /channels/{id}/messages`, rate-limit errors |
| Model | `src/discord/model.rs` | User, guild, channel and message types; handles user-session READY quirks |
| UI | `src/main.rs` | Server rail, channel and DM list, chat with sticky scroll, composer |

How it stays light:
- Heavy features (images, avatars, animation, voice, video, the member list)
  can each be turned off in Settings → Performance.
- Messages are kept only for the 12 channels opened most recently, 120 each.
- Your token never touches a plain file (see [Logging in](#logging-in)).
- It renders in software (tiny-skia) by default. At the login screen that measured
  28 MB working set, against 208 MB with the GPU (wgpu) renderer and 965 MB for
  Discord's 6 processes. Set `ICED_BACKEND=wgpu` to use the GPU.

## Run

Needs rustup, the MSVC C++ build tools and the Windows 10/11 SDK. If `link.exe` isn't
found, run from a "x64 Native Tools" prompt (VS 2022's `vcvars64.bat`).

```bash
cargo run --release
```

The built exe is `target/release/ferrochat.exe`. It runs on 64-bit Windows 10 or
11 with nothing else installed: the C runtime is built in (`.cargo/config.toml`),
and Media Foundation is loaded only when video, screen share or audio playback
first need it (`build.rs`), so Windows "N" editions without the Media Feature
Pack still open it and just lose those. The exe isn't code-signed, so Windows
SmartScreen asks first (More info → Run anyway). Logging in with a user token
in a third-party client is against Discord's Terms of Service: each person
risks their own account. Release builds use full LTO and
one codegen unit for a smaller, faster exe, so they take several minutes; debug
builds (`cargo build`) stay quick.

Or skip the login screen:

```bash
$env:DISCORD_TOKEN = "<token>"; cargo run --release
```

## Logging in

Ferrochat logs in with a **token**, not your email and password. There are two
kinds. Pick one, paste it into the box on the login screen, and press Log in.

> **Your token is your account.** Anyone who has it can read and send your
> messages without your password. Never paste it into a chat, a screenshot, a
> GitHub issue, or a site that asks for it. If it leaks, change your Discord
> password: that makes Discord issue a new token and the old one stops working.

### Option 1: a bot token (allowed by Discord)

A bot is a separate account you create. It only sees servers you invite it to.

1. Open the [Discord Developer Portal](https://discord.com/developers/applications)
   and click **New Application**. Give it a name.
2. Open **Bot** in the left menu. Under **Privileged Gateway Intents**, turn on
   **Message Content Intent** and save. Without it Discord refuses the
   connection (close code 4014).
3. On the same page, click **Reset Token**, then **Copy**.
4. Invite the bot: open **OAuth2 → URL Generator**, tick `bot`, tick the
   permissions it needs (View Channels, Send Messages, Read Message History),
   open the generated link and pick your server.
5. In Ferrochat, paste the token with `Bot ` in front of it:
   `Bot MTIz...` (the word Bot, a space, then the token).

Bots have no friends list, DMs with friends, inbox or member sidebar.

### Option 2: your own account's token (against Discord's rules)

This gives the full experience: your servers, DMs, friends and calls. But
Discord's Terms of Service forbid third-party clients on user accounts, and an
account can be banned for it. **Use a spare account you can lose.**

1. Open <https://discord.com/app> in Chrome, Edge or Firefox and log in
   normally (this is where email, password, 2FA and captchas happen).
2. Press **F12** to open the developer tools and pick the **Network** tab.
3. Type `api` in the filter box, then click any channel in Discord so requests
   appear.
4. Click one of the requests (for example `messages?limit=50`). Under
   **Headers → Request Headers**, find **Authorization**. Its value is your
   token: a long string with two dots in it, with no `Bot ` in front.
5. Copy that value and paste it into Ferrochat's login box.

The token keeps working until you log out of Discord everywhere or change your
password.

### Staying logged in

- Tick **Remember me** before logging in. Ferrochat saves the token only after
  Discord accepts it, encrypted in **Windows Credential Manager** (entry
  `discord-token-<your user id>`, service `ferrochat`). It's never written to a
  file. Next start logs you in by itself.
- **Log out** (Settings) forgets the saved token. A token Discord rejects
  (close code 4004) is forgotten too, and you're back at the login screen.
- More than one account: Settings → **Add another account**, then **Switch**
  between saved accounts from there.
- For scripts, `DISCORD_TOKEN` (see [Run](#run)) skips the login screen and
  saves nothing.

### If logging in fails

| You see | It means |
|---|---|
| `gateway closed (4004): Authentication failed` | The token is wrong, expired, or the password was changed. Copy a fresh one. |
| `gateway closed (4014): Disallowed intent(s)` | Bot only: turn on Message Content Intent (step 2 above). |
| A request asks for a captcha | Joining servers and adding friends can need a captcha. Do that step in the browser. |
| Stuck on "Connecting…" | Discord can't be reached: check your internet or firewall. |

## Features

- Unread markers (bold channel, pill on the server icon) and red mention badges,
  seeded from Discord's read state at login. Opening a channel marks it read on
  the server too (user tokens).
- Typing indicators, both shown and sent (at most once per 8 s).
- Local timestamps ("Today at 14:32"). Messages are grouped by author within 7 minutes.
- "Load older messages" pages back 50 at a time, up to 1000 per channel.
- Channels you can't view are hidden, using roles, overwrites, owner and
  administrator. If your roles weren't sent, all channels are shown.
- Opening a server subscribes to its live events (gateway op 37). If Discord
  rejects that opcode (close code 4001 or 4002), subscriptions are turned off
  instead of causing a reconnect loop.

### Replies, edits and deletes

- Hover a message to get **Reply**, **Edit** (your own messages) and **Delete**
  (your own messages, or any message where you have Manage Messages). Delete
  needs a second click to confirm.
- Replies show a "↱ author: text" preview, or "Original message was deleted".
  While composing a reply, the banner's **@ on/off** button decides whether the
  author is pinged.
- **Up arrow** in an empty message box edits your last message. **Esc** cancels
  a reply or edit, and your unsent draft comes back afterwards.

### Notifications, login and settings

- Windows toasts for DMs and mentions while Ferrochat isn't focused, or for
  other channels too if you turn that on. Spoilers stay hidden in the toast. The
  toast's sender is shown as PowerShell, because the app doesn't register its own
  Windows app ID.
- **Remember me** saves the token encrypted in Windows Credential Manager
  (`ferrochat` / `discord-token`), only after Discord accepts it. **Log out**
  removes it, and so does a rejected token.
- The Settings panel (bottom-left) is saved to `%APPDATA%\ferrochat\settings.json`.
  It covers remember me, notifications, the typing indicator, read-state sync,
  12/24-hour clock, text size (12–20 px) and the GPU renderer (after restart). An
  unreadable settings file is moved to `settings.invalid.json`, not overwritten.

### Reactions, members, images and uploads

- **Reactions:** chips under a message, outlined when one is yours. Click a chip to
  toggle it. **React** (on hover) opens quick picks plus emoji search, including
  the server's custom emoji.
- **Member list** (user logins): the **Members** button in the channel header
  shows the open channel's first 100 rows, grouped by Online, Offline or hoisted
  role, with status dots. It's fed by `GUILD_MEMBER_LIST_UPDATE`.
- **Images:** avatars, image attachments and link embeds load as scaled WebP
  previews (max 400×300). Click one to open the original. Embeds show as cards
  with a provider, a linked title, a markdown description, fields and a footer.
- **Uploads:** the **+** button or drag-and-drop onto the window. Up to 10 files,
  each up to 10 MB (Discord's limit without Nitro). Files are read from disk only
  when you send.

All remote images go through `src/images.rs`:
- Only `https` URLs on Discord's CDN or media proxy are fetched, without your
  token, so viewing a channel never contacts third-party servers.
- At most 6 downloads run at once, and each is capped at 8 MB.
- Images are only fetched for the newest 60 messages in the open channel and the
  visible member rows.
- They're evicted least-recently-used beyond the **Image memory limit** (64 MB by
  default). The limit counts the decoded size, not just the downloaded bytes.

### Performance settings

Each of these can be turned off: image and link previews, avatars, custom emoji
images, the member list, and the GPU renderer. **Lightweight mode** turns them all
off at once. Turning something off frees its cached images immediately. Settings
also shows how many images are in memory right now.

Measured on the offline demo, with avatars, reactions, the member list and an
embed: **37 MB** with the software renderer, and **176–212 MB** with the GPU
renderer.

### Getting around

- **Ctrl+K** opens the quick switcher: type part of a channel, DM or server name and
  press Enter. With nothing typed it lists what's unread and your recent DMs.
- **Ctrl+F** or **Search** in the channel header searches the server (or the DM)
  through Discord's search. Click a result to jump to it.
- **Pins** lists the channel's pinned messages; click one to jump to it.
- Clicking a reply's preview, or a `discord.com/channels/…` link, jumps to that
  message. When it isn't loaded, the history around it is fetched and a
  **Jump to present** bar takes you back to the newest messages.
- Clicking a name or avatar (in messages or the member list) opens the profile:
  bio, roles, account age, when they joined the server, and a **Message** button.

### Status, muting and folders

- Click your name at the bottom left to choose Online, Idle, Do Not Disturb or
  Invisible, or to set a custom status. Both are saved to Discord.
- **Mute** in the server header and the channel header's picker change your
  Discord notification settings: all messages, only @mentions, nothing, or muted.
  Muted channels are dimmed and don't count as unread (mentions still show).
  Toasts follow these settings.
- Server folders from the official client show in the rail. Click one to open it.
  This uses the JSON user settings Discord still sends; if it stops sending
  them, servers show in a flat list.

### Threads, forums, polls, stickers and apps

- Active threads show under their channel (the newest five). Thread links and
  archived threads are fetched when you open them.
- Forum channels (☰) list open posts with their first message. Write a title
  and a message to start a new post; click a post to read and reply in it.
- Polls show a bar per answer; click to vote (or unvote). The **Poll** button
  beside the message box writes one: up to 10 answers, a length, and
  "allow several answers".
- Stickers in messages show as images (animated Lottie stickers as their name).
  **Sticker** sends one of the server's stickers.
- App messages: buttons, link buttons, string select menus, and components-v2
  layouts (text, sections, containers, separators, thumbnails, galleries).
  Other menu types (users, roles, channels) show a note instead.
- Slash commands: type `/` to list the commands the server's apps offer, pick
  one, and fill options as `name:value` (quote values with spaces). Text after
  the command with no `name:` goes to its first text option. Subcommands come
  first: `/role add role:@Mods`. Attachment options aren't supported yet.
  Buttons and commands need a user login.

### Friends and Home

- **Friends** (top of Direct Messages) opens Home: Online, All, Pending (with a
  count of incoming requests), Blocked, Add Friend and Inbox. Statuses update
  live (`PRESENCE_UPDATE`, friends only).
- Friend rows have Message, Add to group and Remove; requests have
  Accept/Ignore or Cancel; blocked users have Unblock. Pick 2–9 friends with
  **Add to group** and press **Create group DM**.
- **Add Friend** sends a request by username. Discord sometimes wants a captcha
  for this; Ferrochat doesn't solve captchas, so it says so and you can add them
  in the browser instead.
- **Inbox** lists your recent mentions across servers and DMs; click one to jump.
- Messages from blocked users fold to one line with **Show message**.
- Profiles have Add Friend / Accept / Remove and Block.
- New DMs and group DMs, and channels created, renamed or deleted elsewhere,
  show up live (`CHANNEL_CREATE/UPDATE/DELETE`).

### Right-click menus

Right-click a message, a name or avatar, a channel, a category or a server:

- **Message:** Add Reaction, Edit, Reply, Forward (pick a channel or DM),
  Pin/Unpin (with Manage Messages, or in DMs), Mark Unread, Copy Text, Copy
  Message Link, Copy Message ID, Delete (asks first).
- **User:** Profile, Message, Add/Remove Friend, Change Nickname (yourself),
  Copy Username/ID, Block/Unblock.
- **Channel:** Mark as Read, Mute, Invite People, Edit Channel, Delete Channel
  (with Manage Channels), Copy Link/ID, Close DM / Leave Group.
- **Category:** Collapse/Expand, Mark as Read, Create/Edit/Delete.
- **Server:** Mark as Read (one bulk request), Mute, Invite People, Change
  Nickname, Create Channel, Copy ID, Leave Server (asks first).

Menus only appear on a right-click, so they cost nothing otherwise. Click a
category's name to collapse it; unread channels and the open one stay visible.
Collapsed categories are remembered until you close Ferrochat.

### Servers and invites

- **+** at the bottom of the server rail joins a server from an invite link
  (`discord.gg/…`, `discord.com/invite/…` or a bare code), or **creates your
  own server**. Joining and creating may need a captcha on Discord's side;
  then **Open invite in browser** finishes it there.
- **Invite People** makes a link (1 day, unlimited uses) with a Copy button.
- Leaving a server, or being removed, takes it off the rail live (`GUILD_DELETE`).

### Voice channel stage

Clicking a voice channel joins it and opens its stage, like Discord: everyone
in the call as a tile (camera picture, or their avatar on a colour of their
own; a green outline while they talk; muted/deafened on the name tag), and a
tile for each Go Live stream. **Watch Stream** plays it in its tile; click any
tile to show it large with the others in a strip, and again to go back to the
grid. The grid fills the space and reflows with the window size. Along the
bottom: Mute, Deafen, Share Screen with its resolution and frame-rate pickers,
and Leave. The voice channel's own text chat sits on the right.

### Screen share (Go Live)

**Share Screen** on the stage (or **Share** in the voice panel) streams your
primary screen to the call, the way Go Live does. **Stop** (or leaving the
call) ends it.

- Pick **480p, 720p, 1080p or Source** and **5, 15, 30 or 60 fps** on the
  stage (saved in settings; 720p 15 fps by default). Changing either while
  sharing restarts the stream at the new quality. The screen is scaled down to
  fit, keeping its shape. Bitrate follows size and frame rate (0.12 bits per
  pixel per frame, 1.5–8 Mbit/s), H.264 constrained baseline.
- Captured with DXGI Desktop Duplication (1–4 ms a frame on a 3440×1440
  screen, where GDI took 60–70 ms and capped shares near 15 fps), scaled with
  an area average so text stays readable, and encoded with Windows' built-in
  H.264 encoder on its own thread, only while you share. GDI is the fallback
  when duplication isn't available. The mouse cursor isn't drawn yet.
- Your own share shows in its tile on the stage (click it to make it large),
  as a 960×540 preview at 15 fps. It's only made while the stage is on screen,
  so sharing from another channel costs nothing extra.
- Viewers' lost packets are resent from the last ~1000 sent (RTX), so a
  dropped packet no longer freezes their picture until the next keyframe.

**Watching** a stream or camera: packets are put back in order and lost ones
are asked for again (NACK) and taken from Discord's resends (RTX); a frame
waits up to 250 ms for them before it's given up and a keyframe is requested.
Socket buffers are 4 MB so keyframe bursts aren't dropped on arrival. Pictures
decode at up to 1920×1080 when a tile is shown large (960×540 in the grid),
up to 30 per second.

Decoding runs on the GPU's video engine (DXVA through Media Foundation) when
there is one: measured on a 1440p stream, 1.9 ms a frame against 14.5 ms in
software, which couldn't keep up with 60 fps and made the picture stall. Turning
the picture into RGBA for display is split across 4 cores (~11 ms → ~3.6 ms
at 1080p), and only happens for frames actually shown. Without a usable GPU it
decodes in software as before (`FERROCHAT_SOFTWARE_DECODE=1` forces that).
- Video only, no sound, and the whole primary screen (no window picker yet).
- A keyframe goes out every 2 seconds, and whenever a viewer asks for one.
- E2EE (DAVE) when the call uses it, like voice.
- Measured offline: the capture → encode path makes one frame per frame in,
  starting with a keyframe. It hasn't been tried in a live call yet.

### Pasting images and files

**Ctrl+V** in a channel attaches a copied picture (a screenshot, or "Copy image"
in a browser) as a PNG, or files copied in Explorer. Text still pastes into the
message box. Pasted pictures are kept in `%TEMP%\ferrochat-paste` until sent,
and cleared after a day.

### Channels you can't write in

Where your roles don't allow sending messages (or sending in threads, or in a
locked thread), the message box is replaced by "You can't talk in this
channel: your permissions don't allow sending messages here." The **+** upload
button and pasting files also hide without Attach Files. With the permission,
the box is there as usual. If Discord didn't send your roles, Ferrochat lets
you try and Discord decides.

### Voice messages and audio

Voice messages and audio attachments (`.ogg`, `.opus`, `.mp3`, `.wav`, `.m4a`,
`.aac`, `.flac`, `.wma`) get a **Play** button, the time and a progress bar.
Voice messages (Ogg Opus) are decoded with libopus, which the voice calls
already use; other formats go through Windows' Media Foundation. Playback uses
the voice speaker and volume settings. The file is downloaded when you press
Play (at most 25 MB, without your token) and decoded about 300 ms ahead, so a
long file never sits in memory as raw audio. Nothing runs until you press Play.

### Animated images and GIFs

- **Play animated GIFs, emoji and avatars** (Settings → Performance, off by
  default and in Lightweight mode) plays animated custom emoji, animated
  avatars, GIF attachments and Tenor GIF embeds. Frames advance only while one
  of those is in the open channel and the window is focused, at the GIF's own
  frame rate (at most 20 a second). Each step redraws the window. Measured on
  the demo with one animated emoji: 0% CPU when off, 1.7% of one core with an
  80 ms GIF (5.9% when it ticked at a fixed 50 ms). GIFs over 150 frames,
  640 px or 24 MB decoded show their first frame. Animation counts against the
  image memory limit.
- **GIF** beside the message box searches Tenor through Discord (trending GIFs
  first) and sends the one you click. The picker's previews, and animated Tenor
  embeds, load from media.tenor.com: the only non-Discord host Ferrochat
  contacts, and only with the **GIF picker** setting on.

### Multiple accounts

Every login you save (with **Remember me**) is listed in Settings → Account and
on the login screen. Each token is its own Windows Credential Manager entry;
the settings file only holds names and ids. **Switch** changes account,
**Add another account** goes to the login screen without forgetting anyone,
and **Remove** or **Log out** forgets that account's token. Turning
**Remember me** off forgets all of them.

### Voice input

Settings → Voice has **Push to talk** (hold Mouse 4/5, Caps Lock, `, Alt,
Right Ctrl, F8, F9 or Insert, even while Ferrochat is in the background) and
**Noise suppression** (RNNoise; a little CPU during calls, off by default and
in Lightweight mode).

### Themes and window

- Settings → Appearance: nine built-in themes (Dark, Midnight, Light, Slate,
  Mocha, Forest, Plum, Paper, High contrast), five accent colours, and
  **Compact mode** (one line per message, no avatars).
- **Custom themes** are `.css` files in `%APPDATA%\ferrochat\themes`.
  **Open themes folder** creates it with a commented `example.css`. Pick a file
  in the Theme list, and press **Reload** after editing it. Only CSS custom
  properties are read, because the UI isn't a web page:

  ```css
  :root {
    --base: midnight;          /* built-in theme for anything left out */
    --accent: #e05a8a;
    --chat: #14121a;
    --text: #eee8f5;
    --wallpaper: url("wallpaper.jpg");   /* a file in the themes folder */
    --wallpaper-opacity: 0.2;
  }
  ```

  Colours can be `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb()` or `rgba()`.
  Unknown properties and bad values are listed under the picker, and the rest
  of the theme still applies. Wallpapers must be local files (no web links)
  under 16 megapixels (5K fits; a full-size phone photo doesn't); they're
  scaled to at most 1920 px. A 1600×900 wallpaper measured about 10 MB extra.
  Themes only change what you see.
- Settings → Window: **Minimize to the notification area** keeps Ferrochat
  running in the tray when you close the window. Click the icon to bring it
  back, or right-click it to quit. **Start with Windows** adds Ferrochat to your
  sign-in apps (`HKCU\…\Run`); with the tray on, it starts hidden.

### Message formatting (`src/markup.rs`)

Content is parsed once per message revision, and names are resolved when drawn.

- **Bold**, *italic*, __underline__, ~~strike~~, `code`, code blocks, `> quotes`,
  `>>> ` block quotes, `#`/`##`/`###` headings, `-# subtext` and `- bullets`.
  Backslash escapes work, and `snake_case` and `2 * 3` stay literal.
- `||spoilers||` are hidden until clicked.
- Links can be clicked. Only `http(s)` links are opened, via the default browser.
- `<@user>`, `<@&role>` (in the role's colour), `<#channel>` (click to jump) and
  `@everyone`/`@here` show as pills. Messages that mention you are highlighted.
- `<t:unix:F>` timestamps show in local time, including the relative `R` style.
- Unicode emoji render in colour through Segoe UI Emoji. Messages that are only
  emoji are shown large.
- Custom emoji `<:name:id>` load from Discord's CDN without your token, as 48 px
  PNGs. At most 256 are cached, and on overflow ones outside the open channel are
  dropped. Animated ones show their first frame.
- In the message box, `:smile:` becomes 😄 and `:server_emoji:` becomes that
  server's emoji when you send. Typing `:fi` shows up to 8 suggestions.

Check rendering offline with sample messages (no token):

```bash
cargo run --release --features demo --target-dir target/demo
```

## Known limits

- Watching cameras and Go Live streams (Settings → Performance → Video) is
  built and tested offline, but not yet on a live call. Screen share sending
  is built the same way (also untested live); sending a camera isn't done. See
  docs/video-plan.md. GIF embeds from other sites than
  Tenor show a still.
- Video attachments open in the browser instead of playing in the app.
- Collapsed categories and the Home tab aren't saved between runs.
- Bot logins get no member list, which would need the privileged
  `GUILD_MEMBERS` and `GUILD_PRESENCES` intents. Bots do need the Message
  Content intent turned on in the Developer Portal; without it Discord closes
  the connection (4014), Ferrochat shows why, and the saved token is kept.
- The first message with a newer colour emoji (for example 🧵) adds about
  11 MB, measured on the demo, as the text engine loads the emoji font's data.
  It's a one-time cost per run.
- Jumping to a message scrolls by its position in the list, so very tall
  messages nearby can leave it slightly off-centre (it's highlighted).
- Only servers you've opened this session get live events. Unread badges for
  big servers you haven't opened may lag.
