//! Voice connection: voice gateway v8 + UDP transport. One stream per call; the
//! WebSocket and UDP socket live inside it, so dropping the subscription (leaving
//! the call) closes both.
//!
//! Signalling, encrypted transport, DAVE end-to-end encryption and audio.
//! See docs/voice-plan.md.

pub mod audio;
pub mod dave;
pub mod decoder;
pub mod dsp;
pub mod h264;
pub mod media;
pub mod player;
pub mod rtp;
pub mod screen;
pub mod video;

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::UdpSocket;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use dave::{Dave, Outgoing};
use media::Media;
use rtp::{Cipher, Mode};
use video::{VideoFrame, VideoReceiver, H264_PAYLOAD_TYPE, RTX_PAYLOAD_TYPE};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Silence frames sent after connecting, to open the media path.
const PRIMING_FRAMES: usize = 5;
const FRAME_INTERVAL: Duration = Duration::from_millis(20);
/// Largest UDP datagram we read.
const MAX_PACKET: usize = 1500;
/// Consecutive UDP receive errors before the call is considered broken.
const MAX_RECV_ERRORS: u32 = 50;

/// Live call settings shared between the UI and the voice task, read every 20 ms.
pub struct Controls {
    muted: AtomicBool,
    deafened: AtomicBool,
    input_gain: AtomicU32,
    output_volume: AtomicU32,
    threshold_db: AtomicU32,
    /// Push-to-talk key (Windows virtual-key code); 0 = voice activity.
    ptt_key: AtomicU32,
    noise_suppression: AtomicBool,
    /// (microphone, speakers) names; `None` = system default. Read when a call starts.
    devices: Mutex<(Option<String>, Option<String>)>,
    /// Largest video picture to decode for the UI, width << 32 | height:
    /// bigger while a tile is shown large.
    video_size: AtomicU64,
    /// The voice stage is on screen: send a preview of our own screen share.
    own_preview: AtomicBool,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            video_size: AtomicU64::new(pack(video::TILE_PICTURE)),
            own_preview: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            deafened: AtomicBool::new(false),
            input_gain: AtomicU32::new(1f32.to_bits()),
            output_volume: AtomicU32::new(1f32.to_bits()),
            threshold_db: AtomicU32::new((-50f32).to_bits()),
            ptt_key: AtomicU32::new(0),
            noise_suppression: AtomicBool::new(false),
            devices: Mutex::new((None, None)),
        }
    }
}

impl Controls {
    pub fn set_mute(&self, muted: bool, deafened: bool) {
        self.muted.store(muted, Ordering::Relaxed);
        self.deafened.store(deafened, Ordering::Relaxed);
    }

    pub fn set_levels(&self, input_gain: f32, output_volume: f32, threshold_db: f32) {
        self.input_gain.store(input_gain.to_bits(), Ordering::Relaxed);
        self.output_volume.store(output_volume.to_bits(), Ordering::Relaxed);
        self.threshold_db.store(threshold_db.to_bits(), Ordering::Relaxed);
    }

    /// `None` sends whenever you speak; `Some(key)` only while it's held.
    pub fn set_input_mode(&self, ptt_key: Option<u32>, noise_suppression: bool) {
        self.ptt_key.store(ptt_key.unwrap_or(0), Ordering::Relaxed);
        self.noise_suppression.store(noise_suppression, Ordering::Relaxed);
    }

    pub fn ptt_key(&self) -> Option<u32> {
        Some(self.ptt_key.load(Ordering::Relaxed)).filter(|&key| key != 0)
    }

    pub fn noise_suppression(&self) -> bool {
        self.noise_suppression.load(Ordering::Relaxed)
    }

    pub fn set_devices(&self, input: Option<String>, output: Option<String>) {
        if let Ok(mut devices) = self.devices.lock() {
            *devices = (input, output);
        }
    }

    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    pub fn deafened(&self) -> bool {
        self.deafened.load(Ordering::Relaxed)
    }

    pub fn input_gain(&self) -> f32 {
        f32::from_bits(self.input_gain.load(Ordering::Relaxed))
    }

    pub fn output_volume(&self) -> f32 {
        f32::from_bits(self.output_volume.load(Ordering::Relaxed))
    }

    pub fn threshold_db(&self) -> f32 {
        f32::from_bits(self.threshold_db.load(Ordering::Relaxed))
    }

    fn devices(&self) -> (Option<String>, Option<String>) {
        self.devices.lock().map(|d| d.clone()).unwrap_or_default()
    }

    /// The UI shows video large (`true`) or as tiles; decoding sizes follow.
    pub fn set_video_large(&self, large: bool) {
        let size = if large { video::LARGE_PICTURE } else { video::TILE_PICTURE };
        self.video_size.store(pack(size), Ordering::Relaxed);
    }

    pub fn set_own_preview(&self, wanted: bool) {
        self.own_preview.store(wanted, Ordering::Relaxed);
    }

    pub fn own_preview(&self) -> bool {
        self.own_preview.load(Ordering::Relaxed)
    }

    pub fn video_size(&self) -> (u32, u32) {
        let packed = self.video_size.load(Ordering::Relaxed);
        ((packed >> 32) as u32, packed as u32)
    }
}

fn pack((width, height): (u32, u32)) -> u64 {
    (u64::from(width) << 32) | u64::from(height)
}

/// Video bursts (a keyframe is hundreds of packets) overflow Windows' default
/// 64 KB socket buffers; bigger ones keep them from being dropped on our side.
#[cfg(windows)]
fn enlarge_buffers(socket: &UdpSocket) {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Networking::WinSock::{setsockopt, SOCKET, SOL_SOCKET, SO_RCVBUF, SO_SNDBUF};
    const RECEIVE: i32 = 4 * 1024 * 1024;
    const SEND: i32 = 1024 * 1024;
    let raw = SOCKET(socket.as_raw_socket() as usize);
    unsafe {
        let _ = setsockopt(raw, SOL_SOCKET, SO_RCVBUF, Some(&RECEIVE.to_ne_bytes()));
        let _ = setsockopt(raw, SOL_SOCKET, SO_SNDBUF, Some(&SEND.to_ne_bytes()));
    }
}

#[cfg(not(windows))]
fn enlarge_buffers(_socket: &UdpSocket) {}

/// Everything needed to connect, from VOICE_STATE_UPDATE and VOICE_SERVER_UPDATE.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ConnectInfo {
    /// Host (and port), without a scheme.
    pub endpoint: String,
    pub server_id: String,
    /// The voice channel; it is also the DAVE (MLS) group id.
    pub channel_id: String,
    pub user_id: String,
    pub session_id: String,
    pub token: String,
    /// A Go Live stream we watch: receive only, no microphone.
    pub stream: bool,
    /// Ask for video (cameras, or the stream) and decode it.
    pub video: bool,
    /// Our own Go Live stream: capture the screen and send it (no audio).
    pub share: bool,
    /// (largest width, largest height, fps) to share at. Part of the identity:
    /// changing it reconnects the stream with the new quality.
    pub share_quality: (u32, u32, u32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Encryption {
    /// DAVE protocol version in effect; 0 = transport encryption only.
    pub version: u16,
    /// Keys are in place, so our audio is end-to-end encrypted.
    pub ready: bool,
    pub privacy_code: Option<String>,
}

#[derive(Debug, Clone)]
pub enum VoiceEvent {
    Status(String),
    /// Transport is ready.
    Connected { mode: &'static str },
    /// DAVE state changed.
    Encryption(Encryption),
    Speaking { user_id: String, speaking: bool },
    /// A decoded picture from someone's camera (or the stream being watched).
    Video { user_id: String, frame: Arc<VideoFrame> },
    /// Someone's camera turned on or off (op 12).
    VideoState { user_id: String, active: bool },
    /// The connection is over; the stream ends after this.
    Ended(String),
}

#[derive(Deserialize)]
struct Payload {
    op: u8,
    #[serde(default)]
    d: Value,
    /// v8: server message sequence, acknowledged in heartbeats.
    #[serde(default)]
    seq: Option<i64>,
}

#[derive(Deserialize)]
struct Ready {
    ssrc: u32,
    ip: String,
    port: u16,
    modes: Vec<String>,
    /// The video SSRCs we may send on, one per stream asked for in Identify.
    #[serde(default)]
    streams: Vec<ReadyStream>,
}

#[derive(Deserialize)]
struct ReadyStream {
    #[serde(default)]
    ssrc: u32,
    #[serde(default)]
    rtx_ssrc: u32,
}

#[derive(Deserialize)]
struct SessionDescription {
    secret_key: Vec<u8>,
    #[serde(default)]
    dave_protocol_version: u16,
    #[serde(default)]
    video_codec: Option<String>,
}
#[derive(Deserialize)]
struct Speaking {
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    speaking: u32,
    #[serde(default)]
    ssrc: Option<u32>,
}

/// Transport state once IP discovery has run.
struct Transport {
    socket: Arc<UdpSocket>,
    ssrc: u32,
    mode: Mode,
    cipher: Option<Cipher>,
    sequence: u16,
    timestamp: u32,
    nonce: u32,
    /// Our video SSRCs (screen share), from Ready.
    video_ssrc: u32,
    rtx_ssrc: u32,
    video_sequence: u16,
    rtx_sequence: u16,
    /// Recently sent video packets, for resending what viewers lost.
    sent: VecDeque<SentPacket>,
}

/// A video packet as sent (before transport encryption).
struct SentPacket {
    sequence: u16,
    timestamp: u32,
    marker: bool,
    payload: Vec<u8>,
}

/// Video RTP payloads stay under a typical path MTU with room for encryption.
const VIDEO_PAYLOAD: usize = 1100;
/// Sent packets kept for resends (about a second of a busy stream).
const SENT_HISTORY: usize = 1024;

impl Transport {
    /// Resends packets a viewer lost, as RTX on our retransmission SSRC.
    async fn resend(&mut self, sequences: &[u16]) -> Result<(), String> {
        let Some(cipher) = &self.cipher else { return Ok(()) };
        if self.rtx_ssrc == 0 {
            return Ok(());
        }
        for &sequence in sequences {
            let Some(sent) = self.sent.iter().find(|p| p.sequence == sequence) else { continue };
            let header = rtp::rtx_header(self.rtx_sequence, sent.timestamp, self.rtx_ssrc, sent.marker);
            let mut payload = Vec::with_capacity(sent.payload.len() + 2);
            payload.extend_from_slice(&sent.sequence.to_be_bytes());
            payload.extend_from_slice(&sent.payload);
            let packet = cipher.seal(&header, &payload, self.nonce)?;
            let _ = self.socket.send(&packet).await;
            self.rtx_sequence = self.rtx_sequence.wrapping_add(1);
            self.nonce = self.nonce.wrapping_add(1);
        }
        Ok(())
    }

    /// Asks `media_ssrc`'s sender to resend packets we're missing (RTCP NACK).
    async fn request_resend(&mut self, media_ssrc: u32, lost: &[u16]) -> Result<(), String> {
        let Some(cipher) = &self.cipher else { return Ok(()) };
        let packet = cipher.seal_rtcp(&rtp::nack(self.ssrc, media_ssrc, lost), self.nonce)?;
        self.nonce = self.nonce.wrapping_add(1);
        let _ = self.socket.send(&packet).await;
        Ok(())
    }

    /// E2EE-encrypts one encoded frame, then sends it as H.264 RTP packets.
    async fn send_video(&mut self, dave: &mut Dave, frame: &screen::Encoded) -> Result<(), String> {
        let Some(cipher) = &self.cipher else { return Ok(()) };
        if self.video_ssrc == 0 {
            return Ok(());
        }
        let Some(sealed) = dave.encrypt_video(&frame.data)? else { return Ok(()) };
        let packets = h264::packetize(&sealed, VIDEO_PAYLOAD);
        for (index, payload) in packets.iter().enumerate() {
            let last = index == packets.len() - 1;
            let header = rtp::video_header(self.video_sequence, frame.timestamp, self.video_ssrc, last);
            let packet = cipher.seal(&header, payload, self.nonce)?;
            self.socket.send(&packet).await.map_err(|e| format!("video UDP send failed: {e}"))?;
            if self.sent.len() >= SENT_HISTORY {
                self.sent.pop_front();
            }
            self.sent.push_back(SentPacket { sequence: self.video_sequence, timestamp: frame.timestamp, marker: last, payload: payload.clone() });
            self.video_sequence = self.video_sequence.wrapping_add(1);
            self.nonce = self.nonce.wrapping_add(1);
        }
        Ok(())
    }

    /// E2EE (when the call uses DAVE), then transport-encrypt and send one frame.
    /// Frames are held back while DAVE keys aren't ready.
    async fn send_frame(&mut self, dave: &mut Dave, opus: &[u8]) -> Result<(), String> {
        let Some(cipher) = &self.cipher else { return Ok(()) };
        let Some(frame) = dave.encrypt(opus)? else { return Ok(()) };
        let header = rtp::rtp_header(self.sequence, self.timestamp, self.ssrc);
        let packet = cipher.seal(&header, &frame, self.nonce)?;
        self.socket.send(&packet).await.map_err(|e| format!("voice UDP send failed: {e}"))?;
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(rtp::FRAME_SAMPLES);
        self.nonce = self.nonce.wrapping_add(1);
        Ok(())
    }

    /// Asks `media_ssrc`'s sender for a keyframe (RTCP PLI).
    async fn request_keyframe(&mut self, media_ssrc: u32) -> Result<(), String> {
        let Some(cipher) = &self.cipher else { return Ok(()) };
        let packet = cipher.seal_rtcp(&rtp::picture_loss(self.ssrc, media_ssrc), self.nonce)?;
        self.nonce = self.nonce.wrapping_add(1);
        // A lost request is retried by the receiver a second later.
        let _ = self.socket.send(&packet).await;
        Ok(())
    }
}

pub fn connect(info: ConnectInfo, controls: Arc<Controls>) -> impl Stream<Item = VoiceEvent> {
    iced::stream::channel(32, move |mut out: mpsc::Sender<VoiceEvent>| async move {
        let reason = run(&info, &controls, &mut out).await.err().unwrap_or_else(|| "Voice disconnected".to_owned());
        let _ = out.send(VoiceEvent::Ended(reason)).await;
    })
}

async fn run(info: &ConnectInfo, controls: &Arc<Controls>, out: &mut mpsc::Sender<VoiceEvent>) -> Result<(), String> {
    let status = |text: &str| VoiceEvent::Status(text.to_owned());
    out.send(status("Connecting to voice server…")).await.map_err(|_| "closed")?;
    let host = info.endpoint.trim_start_matches("wss://");
    let (socket, _) = tokio_tungstenite::connect_async(format!("wss://{host}/?v=8"))
        .await
        .map_err(|e| format!("voice connect failed: {e}"))?;
    let (mut sink, mut stream) = socket.split();

    let identify = json!({
        "op": 0,
        "d": {
            "server_id": info.server_id,
            "user_id": info.user_id,
            "session_id": info.session_id,
            "token": info.token,
            "max_dave_protocol_version": dave::MAX_PROTOCOL_VERSION,
            "video": info.video || info.share,
            "streams": match (info.share, info.video) {
                (true, _) => json!([{ "type": "screen", "rid": "100", "quality": 100 }]),
                (false, true) => json!([{ "type": "video", "rid": "100", "quality": 100 }]),
                (false, false) => json!([]),
            },
        },
    });
    send(&mut sink, identify).await?;
    let mut video = info.video.then(|| VideoReceiver::new(out.clone(), controls.clone()));

    let user_id: u64 = info.user_id.parse().map_err(|_| "invalid user id")?;
    let channel_id: u64 = info.channel_id.parse().map_err(|_| "invalid channel id")?;
    let mut dave = Dave::new(user_id, channel_id);
    let mut reported: Option<Encryption> = None;
    let mut seq_ack: i64 = -1;
    let mut heartbeat: Option<tokio::time::Interval> = None;
    let mut transport: Option<Transport> = None;
    // Audio starts once the transport is keyed; the 20 ms clock drives it.
    let mut media: Option<Media> = None;
    let mut media_clock: Option<tokio::time::Interval> = None;
    let mut udp: Option<Arc<UdpSocket>> = None;
    let mut packet = vec![0u8; MAX_PACKET];
    let mut recv_errors = 0u32;
    // Who sends on which SSRC, from speaking events; DAVE decrypts per user.
    let mut ssrc_users: HashMap<u32, u64> = HashMap::new();
    // Screen share: the capture thread and the frames it encodes.
    let mut share: Option<screen::Share> = None;
    let mut shared_frames: Option<mpsc::Receiver<screen::Encoded>> = None;

    loop {
        tokio::select! {
            _ = tick(&mut heartbeat) => {
                send(&mut sink, json!({ "op": 3, "d": { "t": now_millis(), "seq_ack": seq_ack } })).await?;
            }
            _ = tick(&mut media_clock) => {
                let (Some(media), Some(transport)) = (media.as_mut(), transport.as_mut()) else { continue };
                let result = media.tick(controls)?;
                if let Some(speaking) = result.speaking {
                    let flag = u32::from(speaking);
                    send(&mut sink, json!({ "op": 5, "d": { "speaking": flag, "delay": 0, "ssrc": transport.ssrc } })).await?;
                    out.send(VoiceEvent::Speaking { user_id: info.user_id.clone(), speaking }).await.map_err(|_| "closed")?;
                }
                if let Some(frame) = result.frame {
                    transport.send_frame(&mut dave, &frame).await?;
                }
            }
            encoded = next_frame(&mut shared_frames) => {
                let (Some(frame), Some(transport)) = (encoded, transport.as_mut()) else { continue };
                transport.send_video(&mut dave, &frame).await?;
            }
            received = recv(&udp, &mut packet) => {
                let len = match received {
                    Ok(len) => {
                        recv_errors = 0;
                        len
                    }
                    // Windows reports ICMP errors on connected UDP sockets; tolerate a few.
                    Err(e) => {
                        recv_errors += 1;
                        if recv_errors > MAX_RECV_ERRORS {
                            return Err(format!("voice UDP receive failed: {e}"));
                        }
                        continue;
                    }
                };
                // Sharing: viewers' RTCP feedback. A lost picture (PLI) gets a
                // keyframe; lost packets (NACK) are resent from our history.
                if let Some(share) = &share {
                    let rtcp = matches!(packet.get(1), Some(200..=207));
                    let opened = transport.as_ref().and_then(|t| t.cipher.as_ref()).filter(|_| rtcp).and_then(|c| c.open_rtcp(&packet[..len]));
                    if let (Some(rtcp), Some(transport)) = (opened, transport.as_mut()) {
                        let feedback = rtp::parse_feedback(&rtcp);
                        if feedback.keyframe {
                            share.request_keyframe();
                        }
                        transport.resend(&feedback.resend).await?;
                    }
                    continue;
                }
                let (Some(media), Some(transport)) = (media.as_mut(), transport.as_mut()) else { continue };
                let Some(cipher) = &transport.cipher else { continue };
                let Some(received) = cipher.open(&packet[..len]) else { continue };
                if matches!(received.payload_type, H264_PAYLOAD_TYPE | RTX_PAYLOAD_TYPE) {
                    let Some(video) = video.as_mut() else { continue };
                    let requests = video.receive(&received, &mut dave);
                    if let Some((sender, lost)) = requests.resend {
                        transport.request_resend(sender, &lost).await?;
                    }
                    if let Some(sender) = requests.keyframe {
                        transport.request_keyframe(sender).await?;
                    }
                } else {
                    media.receive(received, &ssrc_users, &mut dave, controls);
                }
            }
            frame = stream.next() => {
                let text = match frame {
                    Some(Ok(WsMessage::Text(text))) => text,
                    // Binary frames are DAVE MLS opcodes.
                    Some(Ok(WsMessage::Binary(frame))) => {
                        let Some((seq, opcode, data)) = dave::parse_server_frame(&frame) else { continue };
                        seq_ack = i64::from(seq);
                        let result = dave.on_binary(opcode, data);
                        handle_dave(result, &mut sink, out).await?;
                        report_encryption(&dave, &mut reported, out).await?;
                        continue;
                    }
                    Some(Ok(WsMessage::Close(frame))) => return Err(close_reason(frame)),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(format!("voice connection error: {e}")),
                    None => return Err("voice connection closed".to_owned()),
                };
                let Ok(payload) = serde_json::from_str::<Payload>(&text) else { continue };
                if let Some(seq) = payload.seq {
                    seq_ack = seq;
                }
                match payload.op {
                    // Hello
                    8 => {
                        let interval = payload.d["heartbeat_interval"].as_f64().unwrap_or(13_750.0);
                        let mut timer = tokio::time::interval(Duration::from_millis(interval as u64));
                        timer.tick().await; // first tick is immediate
                        heartbeat = Some(timer);
                    }
                    // Ready
                    2 => {
                        let ready: Ready = serde_json::from_value(payload.d).map_err(|e| format!("bad voice Ready: {e}"))?;
                        let mode = Mode::choose(&ready.modes)
                            .ok_or_else(|| format!("no supported encryption mode in {:?}", ready.modes))?;
                        out.send(status("Finding route…")).await.map_err(|_| "closed")?;
                        let (socket, address, port) = discover(&ready).await?;
                        let mut codecs = vec![json!({ "name": "opus", "type": "audio", "priority": 1000, "payload_type": 120 })];
                        if info.video || info.share {
                            codecs.push(json!({
                                "name": "H264", "type": "video", "priority": 1000,
                                "payload_type": H264_PAYLOAD_TYPE, "rtx_payload_type": RTX_PAYLOAD_TYPE,
                                "encode": info.share, "decode": info.video,
                            }));
                        }
                        let (video_ssrc, rtx_ssrc) = ready.streams.first().map_or((0, 0), |s| (s.ssrc, s.rtx_ssrc));
                        let select = json!({
                            "op": 1,
                            "d": {
                                "protocol": "udp",
                                "data": { "address": address, "port": port, "mode": mode.name() },
                                "codecs": codecs,
                            },
                        });
                        send(&mut sink, select).await?;
                        enlarge_buffers(&socket);
                        let socket = Arc::new(socket);
                        udp = Some(socket.clone());
                        transport = Some(Transport {
                            socket,
                            ssrc: ready.ssrc,
                            mode,
                            cipher: None,
                            sequence: 0,
                            timestamp: 0,
                            nonce: 0,
                            video_ssrc,
                            rtx_ssrc,
                            video_sequence: 0,
                            rtx_sequence: 0,
                            sent: VecDeque::new(),
                        });
                    }
                    // Session Description
                    4 => {
                        let description: SessionDescription =
                            serde_json::from_value(payload.d).map_err(|e| format!("bad session description: {e}"))?;
                        let Some(transport) = transport.as_mut() else { return Err("session description before Ready".into()) };
                        transport.cipher = Some(
                            Cipher::new(transport.mode, &description.secret_key).ok_or("invalid voice secret key")?,
                        );
                        out.send(VoiceEvent::Connected { mode: transport.mode.name() }).await.map_err(|_| "closed")?;
                        if info.video {
                            if let Some(codec) = description.video_codec.as_deref().filter(|c| !c.eq_ignore_ascii_case("H264")) {
                                out.send(status(&format!("Video uses {codec}, which Ferrochat can't play yet"))).await.map_err(|_| "closed")?;
                            }
                            // Media sink wants: receive every video stream at full quality.
                            send(&mut sink, json!({ "op": 15, "d": { "any": 100 } })).await?;
                        }
                        // A non-zero version starts the MLS handshake with our key package.
                        handle_dave(dave.start(description.dave_protocol_version), &mut sink, out).await?;
                        report_encryption(&dave, &mut reported, out).await?;
                        send(&mut sink, json!({ "op": 5, "d": { "speaking": 1, "delay": 0, "ssrc": transport.ssrc } })).await?;
                        for _ in 0..PRIMING_FRAMES {
                            transport.send_frame(&mut dave, &rtp::SILENCE_FRAME).await?;
                            tokio::time::sleep(FRAME_INTERVAL).await;
                        }
                        send(&mut sink, json!({ "op": 5, "d": { "speaking": 0, "delay": 0, "ssrc": transport.ssrc } })).await?;
                        if info.share && share.is_none() {
                            if transport.video_ssrc == 0 {
                                return Err("the voice server gave no video stream to send on".to_owned());
                            }
                            out.send(status("Starting screen capture…")).await.map_err(|_| "closed")?;
                            let (sender, receiver) = mpsc::channel(2);
                            let (max_width, max_height, fps) = info.share_quality;
                            let quality = screen::Quality { max: (max_width, max_height), fps };
                            let preview = screen::Preview { out: out.clone(), controls: controls.clone() };
                            let started = tokio::task::spawn_blocking(move || screen::start(sender, quality, Some(preview)))
                                .await
                                .map_err(|e| format!("screen capture failed: {e}"))??;
                            let (width, height) = started.size;
                            let (fps, bitrate) = (started.fps, started.bitrate);
                            // Op 12: what we send, so viewers can subscribe to it.
                            let stream = json!({
                                "type": "screen", "rid": "100", "quality": 100, "active": true,
                                "ssrc": transport.video_ssrc, "rtx_ssrc": transport.rtx_ssrc,
                                "max_bitrate": bitrate, "max_framerate": fps,
                                "max_resolution": { "type": "fixed", "width": width, "height": height },
                            });
                            let video = json!({
                                "op": 12,
                                "d": {
                                    "audio_ssrc": transport.ssrc,
                                    "video_ssrc": transport.video_ssrc,
                                    "rtx_ssrc": transport.rtx_ssrc,
                                    "streams": [stream],
                                },
                            });
                            send(&mut sink, video).await?;
                            share = Some(started);
                            shared_frames = Some(receiver);
                            out.send(status(&format!("Sharing your screen at {width}×{height}, {fps} fps")))
                                .await
                                .map_err(|_| "closed")?;
                        }
                        // Sharing sends only video: no microphone, no speakers.
                        if media.is_none() && !info.share {
                            out.send(status("Opening microphone and speakers…")).await.map_err(|_| "closed")?;
                            let (input, output) = controls.devices();
                            let microphone = !info.stream;
                            let (io, input_error, output_error) =
                                tokio::task::spawn_blocking(move || audio::start(input, output, microphone))
                                    .await
                                    .map_err(|e| format!("audio start failed: {e}"))?;
                            for problem in [input_error, output_error].into_iter().flatten() {
                                out.send(VoiceEvent::Status(format!("Audio: {problem}"))).await.map_err(|_| "closed")?;
                            }
                            media = Some(Media::new(io, controls)?);
                            media_clock = Some(tokio::time::interval(FRAME_INTERVAL));
                        }
                    }
                    // Speaking (others): also tells us their SSRC.
                    5 => {
                        if let Ok(Speaking { user_id: Some(user_id), speaking, ssrc }) = serde_json::from_value(payload.d) {
                            if let (Some(ssrc), Ok(id)) = (ssrc, user_id.parse::<u64>()) {
                                ssrc_users.insert(ssrc, id);
                            }
                            let event = VoiceEvent::Speaking { user_id, speaking: speaking != 0 };
                            out.send(event).await.map_err(|_| "closed")?;
                        }
                    }
                    // Video: someone's camera (or the stream) and its SSRCs.
                    12 => {
                        let Some(user_id) = payload.d["user_id"].as_str().map(str::to_owned) else { continue };
                        let Ok(id) = user_id.parse::<u64>() else { continue };
                        let number = |value: &Value| value.as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
                        // (video SSRC, its retransmission SSRC) per active stream.
                        let mut ssrcs: Vec<(u32, u32)> = payload.d["streams"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|s| s["active"].as_bool().unwrap_or(true))
                            .map(|s| (number(&s["ssrc"]), number(&s["rtx_ssrc"])))
                            .collect();
                        let video_ssrc = number(&payload.d["video_ssrc"]);
                        if video_ssrc != 0 && !ssrcs.iter().any(|(s, _)| *s == video_ssrc) {
                            ssrcs.push((video_ssrc, number(&payload.d["rtx_ssrc"])));
                        }
                        ssrcs.retain(|(s, _)| *s != 0);
                        if let Some(audio) = payload.d["audio_ssrc"].as_u64().and_then(|n| u32::try_from(n).ok()) {
                            ssrc_users.insert(audio, id);
                        }
                        if let Some(video) = video.as_mut() {
                            video.set_user(id, &ssrcs);
                        }
                        out.send(VoiceEvent::VideoState { user_id, active: !ssrcs.is_empty() }).await.map_err(|_| "closed")?;
                    }
                    // Clients connect / client disconnect: who may be in the MLS group.
                    11 => {
                        let ids = payload.d["user_ids"].as_array().into_iter().flatten();
                        dave.users_connected(ids.filter_map(|id| id.as_str()?.parse().ok()));
                    }
                    13 => {
                        if let Some(id) = payload.d["user_id"].as_str().and_then(|id| id.parse().ok()) {
                            dave.user_disconnected(id);
                        }
                    }
                    // DAVE transitions and epochs.
                    dave::op::PREPARE_TRANSITION | dave::op::EXECUTE_TRANSITION | dave::op::PREPARE_EPOCH => {
                        let result = dave.on_json(payload.op, &payload.d);
                        handle_dave(result, &mut sink, out).await?;
                        report_encryption(&dave, &mut reported, out).await?;
                    }
                    // Heartbeat ACK, Resumed, client connect/disconnect, etc.
                    _ => {}
                }
            }
        }
    }
}

/// Sends the IP discovery packet and waits for our external address.
async fn discover(ready: &Ready) -> Result<(UdpSocket, String, u16), String> {
    let socket = UdpSocket::bind("0.0.0.0:0").await.map_err(|e| format!("could not open UDP socket: {e}"))?;
    socket
        .connect((ready.ip.as_str(), ready.port))
        .await
        .map_err(|e| format!("could not reach voice server: {e}"))?;
    socket.send(&rtp::discovery_request(ready.ssrc)).await.map_err(|e| format!("IP discovery failed: {e}"))?;
    let mut buffer = [0u8; 128];
    let len = tokio::time::timeout(DISCOVERY_TIMEOUT, socket.recv(&mut buffer))
        .await
        .map_err(|_| "IP discovery timed out (UDP blocked by a firewall?)".to_owned())?
        .map_err(|e| format!("IP discovery failed: {e}"))?;
    let (address, port) = rtp::parse_discovery_response(&buffer[..len]).ok_or("invalid IP discovery response")?;
    Ok((socket, address, port))
}

/// Sends DAVE replies. A DAVE error is shown but doesn't end the call: the
/// gateway re-adds members whose group state broke.
async fn handle_dave<S>(
    result: Result<Vec<Outgoing>, String>,
    sink: &mut S,
    out: &mut mpsc::Sender<VoiceEvent>,
) -> Result<(), String>
where
    S: iced::futures::Sink<WsMessage> + Unpin,
{
    match result {
        Ok(messages) => {
            for message in messages {
                let frame = match message {
                    Outgoing::Json(value) => WsMessage::Text(value.to_string()),
                    Outgoing::Binary(bytes) => WsMessage::Binary(bytes),
                };
                sink.send(frame).await.map_err(|_| "voice send failed".to_owned())?;
            }
            Ok(())
        }
        Err(error) => {
            out.send(VoiceEvent::Status(format!("Encryption problem: {error}"))).await.map_err(|_| "closed".to_owned())
        }
    }
}

/// Tells the UI when the encryption state or privacy code changes.
async fn report_encryption(
    dave: &Dave,
    reported: &mut Option<Encryption>,
    out: &mut mpsc::Sender<VoiceEvent>,
) -> Result<(), String> {
    let now = Encryption { version: dave.protocol_version(), ready: dave.is_ready(), privacy_code: dave.privacy_code() };
    if reported.as_ref() != Some(&now) {
        *reported = Some(now.clone());
        out.send(VoiceEvent::Encryption(now)).await.map_err(|_| "closed".to_owned())?;
    }
    Ok(())
}

async fn send<S>(sink: &mut S, payload: Value) -> Result<(), String>
where
    S: iced::futures::Sink<WsMessage> + Unpin,
{
    sink.send(WsMessage::Text(payload.to_string())).await.map_err(|_| "voice send failed".to_owned())
}

async fn recv(socket: &Option<Arc<UdpSocket>>, buffer: &mut [u8]) -> std::io::Result<usize> {
    match socket {
        Some(socket) => socket.recv(buffer).await,
        None => std::future::pending().await,
    }
}

/// The next encoded screen frame; pending forever when not sharing.
async fn next_frame(frames: &mut Option<mpsc::Receiver<screen::Encoded>>) -> Option<screen::Encoded> {
    match frames {
        Some(frames) => frames.next().await,
        None => std::future::pending().await,
    }
}

async fn tick(heartbeat: &mut Option<tokio::time::Interval>) {
    match heartbeat {
        Some(timer) => {
            timer.tick().await;
        }
        None => std::future::pending().await,
    }
}

fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Voice close codes from Discord's documentation.
fn close_reason(frame: Option<CloseFrame<'_>>) -> String {
    let Some(frame) = frame else { return "voice server closed the connection".to_owned() };
    let code = u16::from(frame.code);
    let meaning = match code {
        4001 => "unknown opcode",
        4002 => "invalid payload",
        4003 => "not authenticated",
        4004 => "authentication failed",
        4005 => "already authenticated",
        4006 => "session no longer valid",
        4009 => "session timed out",
        4011 => "voice server not found",
        4012 => "unknown protocol",
        4014 => "disconnected (kicked, moved, or channel deleted)",
        4015 => "voice server crashed",
        4016 => "unknown encryption mode",
        4017 => "end-to-end encryption (DAVE) is required",
        4020 => "bad request",
        4021 => "rate limited",
        4022 => "call terminated",
        _ => "",
    };
    match (meaning, frame.reason.is_empty()) {
        ("", true) => format!("voice closed ({code})"),
        ("", false) => format!("voice closed ({code}): {}", frame.reason),
        (meaning, _) => format!("voice closed ({code}): {meaning}"),
    }
}
