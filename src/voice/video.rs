//! Receiving video: RTP packets from the voice connection are put back in
//! order per sender (asking for resends of lost ones), DAVE-decrypted, and
//! handed to a decoding thread that sends RGBA pictures to the UI, at the size
//! the UI shows them.

use std::collections::HashMap;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use iced::futures::channel::mpsc;

use super::dave::Dave;
use super::decoder::{self, Decoder};
use super::h264::{self, Assembler};
use super::rtp::Received;
use super::{Controls, VoiceEvent};

/// RTP payload types we advertise for H.264 and its retransmissions.
pub const H264_PAYLOAD_TYPE: u8 = 101;
pub const RTX_PAYLOAD_TYPE: u8 = 102;
/// Picture size for small tiles; a tile shown large asks for more (see [`Controls::set_video_size`]).
pub const TILE_PICTURE: (u32, u32) = (960, 540);
/// The largest picture decoded for the UI (a stream shown large).
pub const LARGE_PICTURE: (u32, u32) = (1920, 1080);
/// Pictures sent to the UI per sender per second, at most; each one redraws.
const MAX_FPS: u32 = 30;
/// Frames waiting for the decoder (a second at 60 fps, so a short stall doesn't
/// overflow it); beyond this the sender needs a keyframe.
const QUEUE: usize = 60;
/// Keyframe requests per sender are at most this often.
const KEYFRAME_RETRY: Duration = Duration::from_secs(1);

/// A picture for the UI.
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VideoFrame({}x{})", self.width, self.height)
    }
}

enum Job {
    /// `max`: the picture size to scale down to.
    Frame { user_id: u64, data: Vec<u8>, max: (u32, u32) },
    Stop(u64),
}

/// The decoding thread; stopping it (drop) ends the thread.
struct Worker {
    jobs: SyncSender<Job>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Dropping the sender ends the thread's loop.
        let (closed, _) = sync_channel(0);
        drop(std::mem::replace(&mut self.jobs, closed));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn decode_loop(jobs: Receiver<Job>, mut out: mpsc::Sender<VoiceEvent>) {
    if let Err(error) = decoder::startup() {
        let _ = out.try_send(VoiceEvent::Status(format!("Video unavailable: {error}")));
        return;
    }
    let mut decoders: HashMap<u64, (Decoder, Instant)> = HashMap::new();
    let interval = Duration::from_secs(1) / MAX_FPS;
    while let Ok(job) = jobs.recv() {
        let (user_id, data, max) = match job {
            Job::Frame { user_id, data, max } => (user_id, data, max),
            Job::Stop(user_id) => {
                decoders.remove(&user_id);
                continue;
            }
        };
        let entry = match decoders.entry(user_id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => match Decoder::new() {
                Ok(decoder) => {
                    if !decoder.is_hardware() {
                        // Worth knowing when a big stream stutters.
                        let _ = out.try_send(VoiceEvent::Status("Video decodes on the CPU here (no GPU video decoder)".to_owned()));
                    }
                    entry.insert((decoder, Instant::now() - interval))
                }
                Err(error) => {
                    let _ = out.try_send(VoiceEvent::Status(format!("Video unavailable: {error}")));
                    continue;
                }
            },
        };
        let (decoder, last_shown) = entry;
        let show = last_shown.elapsed() >= interval;
        match decoder.decode(&data, show, max) {
            Ok(pictures) => {
                if let Some(picture) = pictures.into_iter().last() {
                    *last_shown = Instant::now();
                    let frame = Arc::new(VideoFrame { width: picture.width, height: picture.height, rgba: picture.rgba });
                    // A full UI queue just skips this picture.
                    let _ = out.try_send(VoiceEvent::Video { user_id: user_id.to_string(), frame });
                }
            }
            Err(error) => {
                let _ = out.try_send(VoiceEvent::Status(format!("Video: {error}")));
            }
        }
    }
}

struct Incoming {
    assembler: Assembler,
    /// Until a keyframe arrives, frames can't be decoded.
    waiting_for_keyframe: bool,
    last_request: Option<Instant>,
    losses_seen: u32,
}

/// What to send back to a video sender after a packet.
#[derive(Debug, Default)]
pub struct Requests {
    /// Ask this SSRC for a keyframe (RTCP PLI).
    pub keyframe: Option<u32>,
    /// Ask this SSRC to resend these packets (RTCP NACK).
    pub resend: Option<(u32, Vec<u16>)>,
}

/// Per-connection video receiving state, owned by the voice task.
pub struct VideoReceiver {
    worker: Worker,
    controls: Arc<Controls>,
    /// Video SSRC → user, from the voice server's op 12.
    users: HashMap<u32, u64>,
    /// Retransmission SSRC → the video SSRC it repairs.
    repairs: HashMap<u32, u32>,
    incoming: HashMap<u32, Incoming>,
}

impl VideoReceiver {
    pub fn new(out: mpsc::Sender<VoiceEvent>, controls: Arc<Controls>) -> Self {
        let (jobs, queue) = sync_channel(QUEUE);
        let thread = std::thread::Builder::new()
            .name("ferrochat-video".to_owned())
            .spawn(move || decode_loop(queue, out))
            .ok();
        Self { worker: Worker { jobs, thread }, controls, users: HashMap::new(), repairs: HashMap::new(), incoming: HashMap::new() }
    }

    /// Op 12: `streams` (video SSRC, retransmission SSRC) now carry `user_id`'s
    /// video (none: their video stopped).
    pub fn set_user(&mut self, user_id: u64, streams: &[(u32, u32)]) {
        let gone: Vec<u32> = self.users.iter().filter(|(_, &u)| u == user_id).map(|(&s, _)| s).collect();
        for ssrc in gone.into_iter().filter(|s| !streams.iter().any(|(video, _)| video == s)) {
            self.users.remove(&ssrc);
            self.incoming.remove(&ssrc);
            self.repairs.retain(|_, video| *video != ssrc);
        }
        for &(video, rtx) in streams.iter().filter(|(video, _)| *video != 0) {
            self.users.insert(video, user_id);
            if rtx != 0 {
                self.repairs.insert(rtx, video);
            }
        }
        if streams.iter().all(|(video, _)| *video == 0) {
            let _ = self.worker.jobs.try_send(Job::Stop(user_id));
        }
    }

    /// One video packet, or a resend of one (RTX). Says what to ask the sender for.
    pub fn receive(&mut self, packet: &Received, dave: &mut Dave) -> Requests {
        let mut requests = Requests::default();
        // A resend carries the original sequence number first, then the original payload.
        let (ssrc, sequence, payload) = match packet.payload_type {
            H264_PAYLOAD_TYPE => (packet.ssrc, packet.sequence, packet.payload.as_slice()),
            RTX_PAYLOAD_TYPE if packet.payload.len() > 2 => match self.repairs.get(&packet.ssrc) {
                Some(&video) => (video, u16::from_be_bytes([packet.payload[0], packet.payload[1]]), &packet.payload[2..]),
                None => return requests,
            },
            _ => return requests,
        };
        let Some(&user_id) = self.users.get(&ssrc) else { return requests };
        let incoming = self.incoming.entry(ssrc).or_insert_with(|| Incoming {
            assembler: Assembler::default(),
            waiting_for_keyframe: true,
            last_request: None,
            losses_seen: 0,
        });
        let pushed = incoming.assembler.push(sequence, packet.timestamp, packet.marker, payload, Instant::now());
        if !pushed.missing.is_empty() {
            requests.resend = Some((ssrc, pushed.missing));
        }
        if incoming.assembler.losses != incoming.losses_seen {
            incoming.losses_seen = incoming.assembler.losses;
            incoming.waiting_for_keyframe = true;
        }
        let ask = |incoming: &mut Incoming| -> Option<u32> {
            let due = incoming.last_request.is_none_or(|at| at.elapsed() >= KEYFRAME_RETRY);
            due.then(|| {
                incoming.last_request = Some(Instant::now());
                ssrc
            })
        };
        let max = self.controls.video_size();
        for frame in pushed.frames {
            if incoming.waiting_for_keyframe {
                if !h264::is_keyframe(&frame) {
                    continue;
                }
                incoming.waiting_for_keyframe = false;
            }
            let Ok(data) = dave.decrypt_video(user_id, &frame) else {
                // Keys not in place yet (or a ratchet change): try again from a keyframe.
                incoming.waiting_for_keyframe = true;
                continue;
            };
            if let Err(TrySendError::Full(_)) = self.worker.jobs.try_send(Job::Frame { user_id, data, max }) {
                // The decoder is behind; skipping breaks the picture until a keyframe.
                incoming.waiting_for_keyframe = true;
            }
        }
        if incoming.waiting_for_keyframe {
            requests.keyframe = ask(incoming);
        }
        requests
    }
}
