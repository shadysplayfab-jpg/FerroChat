//! The voice channel stage: everyone in the call as tiles (camera, or avatar
//! on a colour), streams beside them (click to watch, shown in place), call
//! controls along the bottom, and the channel's text chat to the right.

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, column, container, image, pick_list, row, stack, text, Column, Row, Space};
use iced::{Alignment, Border, Color, ContentFit, Element, Length};

use super::{blend, fill, label, quiet, thin_scroll, BOLD};
use crate::discord::model::{User, VoiceState};
use crate::session::{camera_tile, Session, STREAM_TILE};
use crate::settings::{Change, ShareResolution, SHARE_FPS};
use crate::voice::screen::OWN_PREVIEW;
use crate::theme::p;
use crate::Msg;

/// The chat column beside the stage: narrower in small windows so the tiles keep room.
pub fn stage_chat_width(window_width: f32) -> f32 {
    if window_width < 1300.0 { 300.0 } else { 380.0 }
}
/// Rail and channel list, to the stage's left.
const LEFT_COLUMNS: f32 = 72.0 + 240.0;
const GAP: f32 = 8.0;
const PADDING: f32 = 16.0;
const MIN_TILE_WIDTH: f32 = 160.0;
const AVATAR: f32 = 80.0;

/// Muted tile colours; each person keeps theirs (by user id).
const TILE_TINTS: [(u8, u8, u8); 6] = [(88, 101, 242), (59, 165, 93), (237, 66, 69), (250, 166, 26), (155, 89, 182), (26, 188, 156)];

fn tint(user_id: &str) -> Color {
    // Ids are numbers; mixing all digits spreads small and similar ones apart.
    let mix = user_id.bytes().fold(0u32, |hash, byte| hash.wrapping_mul(31).wrapping_add(u32::from(byte)));
    let (r, g, b) = TILE_TINTS[mix as usize % TILE_TINTS.len()];
    blend(p().rail, Color::from_rgb8(r, g, b), 0.22)
}

/// What one tile shows.
enum TileKind<'a> {
    Person { state: &'a VoiceState, name: String, speaking: bool, user: Option<User> },
    /// Someone's Go Live stream (ours included).
    Stream { user_id: &'a str, name: String },
}

impl Session {
    pub(super) fn voice_stage_view<'a>(&'a self, channel_id: &'a str) -> Element<'a, Msg> {
        let channel_name = self.channel(channel_id).map(|c| c.label()).unwrap_or_default();
        let joined = self.voice.as_ref().is_some_and(|call| call.channel_id == channel_id);
        let mut header = row![label(format!("🔊 {channel_name}")).size(15).font(BOLD).width(Length::Fill)].align_y(Alignment::Center);
        if let Some(e2ee) = self.voice.as_ref().filter(|_| joined).and_then(|c| c.encryption.as_ref()).filter(|e| e.version > 0 && e.ready) {
            let note = format!("End-to-end encrypted · DAVE v{}", e2ee.version);
            header = header.push(container(text(note).size(11).color(p().muted)).padding([2, 8]).style(fill(blend(p().rail, p().text, 0.08), 4.0)));
        }

        let members = self.guild_of(channel_id).map(|guild| self.voice_members(guild, channel_id)).unwrap_or_default();
        let my_id = self.me.as_ref().map_or("", |me| me.id.as_str());
        let mut tiles: Vec<(String, TileKind<'_>)> = Vec::new();
        for (name, speaking, state) in &members {
            let user = self.voice_user(state);
            tiles.push((format!("user:{}", state.user_id), TileKind::Person { state, name: name.clone(), speaking: *speaking, user }));
        }
        for (name, _, state) in &members {
            let ours = state.user_id == my_id && self.sharing.is_some();
            if state.self_stream || ours {
                tiles.push((format!("stream:{}", state.user_id), TileKind::Stream { user_id: &state.user_id, name: name.clone() }));
            }
        }

        let width = (self.window.width - LEFT_COLUMNS - stage_chat_width(self.window.width) - PADDING * 2.0).max(MIN_TILE_WIDTH);
        let height = (self.window.height - 150.0).max(200.0);
        let focused = self.focused_tile.as_deref().filter(|key| tiles.iter().any(|(k, _)| k == key));
        let body: Element<'_, Msg> = if tiles.is_empty() {
            let hint = if joined { "Connecting…" } else { "No one is here yet." };
            container(text(hint).size(14).color(p().muted)).center(Length::Fill).into()
        } else if let Some(key) = focused {
            // One large tile, the rest small along the bottom.
            let strip_height = 110.0;
            let large_width = width.min((height - strip_height - GAP) * 16.0 / 9.0);
            let mut large = None;
            let mut strip = Row::new().spacing(GAP);
            for (tile_key, kind) in &tiles {
                if tile_key == key {
                    large = Some(self.stage_tile(tile_key, kind, large_width, large_width * 9.0 / 16.0));
                } else {
                    strip = strip.push(self.stage_tile(tile_key, kind, strip_height * 16.0 / 9.0, strip_height));
                }
            }
            column![container(large.unwrap_or_else(|| Space::new(0, 0).into())).center_x(Length::Fill), container(strip).center_x(Length::Fill)]
                .spacing(GAP)
                .into()
        } else {
            // As many columns as fit, filling the width like Discord's grid.
            let count = tiles.len() as f32;
            let mut columns = count.sqrt().ceil().max(1.0);
            while columns > 1.0 && (width - GAP * (columns - 1.0)) / columns < MIN_TILE_WIDTH {
                columns -= 1.0;
            }
            let rows = (count / columns).ceil();
            let by_width = (width - GAP * (columns - 1.0)) / columns;
            let by_height = (height - GAP * (rows - 1.0)) / rows * 16.0 / 9.0;
            let tile_width = by_width.min(by_height).max(MIN_TILE_WIDTH);
            let mut grid = Column::new().spacing(GAP).align_x(Alignment::Center);
            let mut line = Row::new().spacing(GAP);
            for (index, (key, kind)) in tiles.iter().enumerate() {
                line = line.push(self.stage_tile(key, kind, tile_width, tile_width * 9.0 / 16.0));
                if (index + 1) as f32 % columns == 0.0 {
                    grid = grid.push(std::mem::replace(&mut line, Row::new().spacing(GAP)));
                }
            }
            grid = grid.push(line);
            container(thin_scroll(container(grid).center_x(Length::Fill))).center(Length::Fill).into()
        };

        let controls: Element<'_, Msg> = if joined {
            self.stage_controls()
        } else {
            let join = button(text("Join Voice").size(14).font(BOLD))
                .padding([10, 22])
                .style(button::success)
                .on_press(Msg::JoinVoice(channel_id.to_owned()));
            container(join).center_x(Length::Fill).into()
        };
        let header = container(header).padding([12.0, PADDING]);
        let body = container(body).padding([0.0, PADDING]).height(Length::Fill);
        container(column![header, body, controls].spacing(8))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(fill(blend(p().rail, Color::BLACK, 0.35), 0.0))
            .into()
    }

    fn stage_tile<'a>(&'a self, key: &str, kind: &TileKind<'a>, width: f32, height: f32) -> Element<'a, Msg> {
        let (background, content, name, border, click): (Color, Element<'a, Msg>, String, Option<Color>, Msg) = match kind {
            TileKind::Person { state, name, speaking, user } => {
                let camera = self.tiles.get(&camera_tile(&state.user_id));
                let picture: Element<'a, Msg> = match (camera, user) {
                    (Some(tile), _) => image(tile.image.clone()).width(width).height(height).content_fit(ContentFit::Cover).into(),
                    (None, Some(user)) if self.settings.load_avatars => {
                        let size = AVATAR.min(height * 0.45);
                        container(self.avatar(user, size)).center(Length::Fill).into()
                    }
                    _ => container(label(super::initials(name)).size(28).font(BOLD)).center(Length::Fill).into(),
                };
                let mut flags = String::new();
                if state.self_deaf || state.deaf {
                    flags.push_str("  · deafened");
                } else if state.self_mute || state.mute {
                    flags.push_str("  · muted");
                }
                let focus = Msg::FocusTile(if self.focused_tile.as_deref() == Some(key) { None } else { Some(key.to_owned()) });
                (tint(&state.user_id), picture, format!("{name}{flags}"), speaking.then(|| p().online), focus)
            }
            TileKind::Stream { user_id, name } => {
                let mine = self.me.as_ref().is_some_and(|me| me.id == *user_id);
                let watching = self.watching.as_ref().is_some_and(|w| w.user_id == *user_id);
                let picture: Element<'a, Msg> = match (mine, watching, self.tiles.get(STREAM_TILE)) {
                    (false, true, Some(tile)) => image(tile.image.clone()).width(width).height(height).content_fit(ContentFit::Contain).into(),
                    (false, true, None) => {
                        let status = self.watching.as_ref().map_or("", |w| w.status.as_str());
                        container(text(status).size(12).color(p().muted)).center(Length::Fill).into()
                    }
                    // Your own share: what you're sending, as you send it.
                    (true, _, _) => match self.tiles.get(OWN_PREVIEW) {
                        Some(tile) => image(tile.image.clone()).width(width).height(height).content_fit(ContentFit::Contain).into(),
                        None => {
                            let status = self.sharing.as_ref().map_or("You're sharing your screen", |s| s.status.as_str());
                            let lines = column![label("You're sharing your screen").size(14).font(BOLD), text(status).size(11).color(p().muted)];
                            container(lines.spacing(4).align_x(Alignment::Center)).center(Length::Fill).into()
                        }
                    },
                    (false, false, _) => {
                        let watch = button(text("Watch Stream").size(13).font(BOLD)).padding([8, 16]).on_press(Msg::WatchStream(user_id.to_string()));
                        let watch: Element<'a, Msg> = if self.settings.video {
                            watch.into()
                        } else {
                            text("Turn on Video in Settings → Performance to watch").size(12).color(p().muted).into()
                        };
                        container(watch).center(Length::Fill).into()
                    }
                };
                let click = if watching || mine {
                    Msg::FocusTile(if self.focused_tile.as_deref() == Some(key) { None } else { Some(key.to_owned()) })
                } else {
                    Msg::WatchStream(user_id.to_string())
                };
                (Color::BLACK, picture, format!("{name} · LIVE"), None, click)
            }
        };
        let tag = container(text(name).size(12).color(Color::WHITE)).padding([3, 8]).style(fill(Color { a: 0.55, ..Color::BLACK }, 4.0));
        let layers = stack![
            container(Space::new(width, height)).style(fill(background, 8.0)),
            container(content).width(width).height(height).clip(true),
            container(tag).width(width).height(height).padding(8).align_x(Horizontal::Left).align_y(Vertical::Bottom),
        ];
        let framed = container(layers).style(move |_| container::Style {
            border: Border { color: border.unwrap_or(Color::TRANSPARENT), width: if border.is_some() { 2.0 } else { 0.0 }, radius: 8.0.into() },
            ..container::Style::default()
        });
        button(framed).padding(0).style(button::text).on_press(click).into()
    }

    /// Mute, Deafen, Share (with its quality), Leave: centred under the tiles.
    fn stage_controls(&self) -> Element<'_, Msg> {
        let Some(call) = &self.voice else { return Space::new(0, 0).into() };
        let toggle = |name: &'static str, on: bool, msg: Msg| {
            button(text(name).size(13)).padding([7, 12]).style(if on { button::danger } else { quiet }).on_press(msg)
        };
        let sharing = self.sharing.is_some();
        let resolution = pick_list(ShareResolution::ALL, Some(self.settings.share_resolution), |r| Msg::Setting(Change::ShareResolution(r)))
            .text_size(12)
            .padding([7, 8]);
        let fps_choices: Vec<String> = SHARE_FPS.iter().map(|fps| format!("{fps} fps")).collect();
        let fps_now = format!("{} fps", self.settings.share_quality().2);
        let fps = pick_list(fps_choices, Some(fps_now), |choice: String| {
            Msg::Setting(Change::ShareFps(choice.trim_end_matches(" fps").parse().unwrap_or(15)))
        })
        .text_size(12)
        .padding([7, 8]);
        // Three groups that wrap onto a second line in narrow windows.
        let group = |items: Vec<Element<'static, Msg>>| -> Element<'static, Msg> {
            Row::with_children(items).spacing(6).align_y(Alignment::Center).into()
        };
        let share = toggle(if sharing { "Stop Sharing" } else { "Share Screen" }, sharing, if sharing { Msg::StopSharing } else { Msg::ShareScreen });
        let controls = Row::new()
            .spacing(14)
            .push(group(vec![
                toggle(if call.self_mute { "Unmute" } else { "Mute" }, call.self_mute, Msg::ToggleMute).into(),
                toggle(if call.self_deaf { "Undeafen" } else { "Deafen" }, call.self_deaf, Msg::ToggleDeafen).into(),
            ]))
            .push(group(vec![share.into(), resolution.into(), fps.into()]))
            .push(toggle("Leave", true, Msg::LeaveVoice))
            .wrap();
        container(controls).center_x(Length::Fill).padding([10.0, PADDING]).into()
    }
}
