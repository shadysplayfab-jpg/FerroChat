//! H.264 over RTP (RFC 6184): single NAL units, STAP-A and FU-A packets back
//! into whole frames. Output is Annex B with 4-byte start codes, the layout
//! WebRTC's depacketizer produces, which DAVE's frame decryption expects.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const START_CODE: [u8; 4] = [0, 0, 0, 1];
const STAP_A: u8 = 24;
const FU_A: u8 = 28;
const NAL_TYPE_MASK: u8 = 0x1F;
const NAL_IDR: u8 = 5;
const NAL_SPS: u8 = 7;
/// Frames larger than this are treated as broken (no real camera frame is).
const MAX_FRAME: usize = 4 * 1024 * 1024;

/// Reassembles one sender's (one SSRC's) frames.
#[derive(Default)]
pub struct Depacketizer {
    timestamp: Option<u32>,
    next_sequence: Option<u16>,
    frame: Vec<u8>,
    /// A packet of the current frame was lost or unreadable.
    broken: bool,
    /// Inside a fragmented (FU-A) NAL unit.
    in_fragment: bool,
    /// Frames dropped as incomplete; the caller asks for a keyframe when it grows.
    pub losses: u32,
}

impl Depacketizer {
    /// Adds one RTP payload. Returns a complete frame when `marker` ends one.
    pub fn push(&mut self, sequence: u16, timestamp: u32, marker: bool, payload: &[u8]) -> Option<Vec<u8>> {
        if self.timestamp != Some(timestamp) {
            // A new frame began before the last one ended: that one is lost.
            if !self.frame.is_empty() {
                self.losses += 1;
            }
            self.frame.clear();
            self.broken = false;
            self.in_fragment = false;
            self.timestamp = Some(timestamp);
        }
        if self.next_sequence.is_some_and(|expected| expected != sequence) {
            self.broken = true;
        }
        self.next_sequence = Some(sequence.wrapping_add(1));
        if !self.broken && !self.append(payload) {
            self.broken = true;
        }
        if self.frame.len() > MAX_FRAME {
            self.broken = true;
            self.frame.clear();
        }
        if !marker {
            return None;
        }
        self.timestamp = None;
        let frame = std::mem::take(&mut self.frame);
        if std::mem::take(&mut self.broken) || frame.is_empty() {
            self.losses += 1;
            return None;
        }
        Some(frame)
    }

    /// `false` when the payload can't be read (or continues a lost fragment).
    fn append(&mut self, payload: &[u8]) -> bool {
        let Some(&header) = payload.first() else { return false };
        match header & NAL_TYPE_MASK {
            1..=23 => {
                self.frame.extend_from_slice(&START_CODE);
                self.frame.extend_from_slice(payload);
                true
            }
            STAP_A => {
                let mut rest = &payload[1..];
                while rest.len() >= 2 {
                    let size = usize::from(u16::from_be_bytes([rest[0], rest[1]]));
                    let Some(unit) = rest.get(2..2 + size).filter(|u| !u.is_empty()) else { return false };
                    self.frame.extend_from_slice(&START_CODE);
                    self.frame.extend_from_slice(unit);
                    rest = &rest[2 + size..];
                }
                rest.is_empty()
            }
            FU_A => {
                let Some(&fragment) = payload.get(1) else { return false };
                let (start, end) = (fragment & 0x80 != 0, fragment & 0x40 != 0);
                if start {
                    // The NAL header is split across the indicator and FU header.
                    self.frame.extend_from_slice(&START_CODE);
                    self.frame.push((header & 0xE0) | (fragment & NAL_TYPE_MASK));
                    self.in_fragment = true;
                } else if !self.in_fragment {
                    return false;
                }
                self.frame.extend_from_slice(&payload[2..]);
                if end {
                    self.in_fragment = false;
                }
                true
            }
            // STAP-B, MTAP and FU-B aren't used by WebRTC senders.
            _ => false,
        }
    }
}

/// How long an incomplete frame waits for late or resent packets.
const MAX_WAIT: Duration = Duration::from_millis(250);
/// Packets held at most (a few keyframes' worth).
const MAX_PACKETS: usize = 4096;
/// Missing packets asked for at once; a bigger gap means a keyframe is better.
const MAX_NACK: u64 = 96;

struct Held {
    timestamp: u32,
    marker: bool,
    payload: Vec<u8>,
    arrived: Instant,
}

/// What one packet led to.
#[derive(Debug, Default)]
pub struct Pushed {
    /// Complete frames, in order.
    pub frames: Vec<Vec<u8>>,
    /// Sequence numbers that are missing: ask the sender to resend them.
    pub missing: Vec<u16>,
}

/// Puts one sender's packets back in order, so a late packet or a resend (RTX)
/// still completes its frame. Only when a frame stays incomplete for
/// [`MAX_WAIT`] is it given up (and counted in `losses`, so the caller asks
/// for a keyframe).
#[derive(Default)]
pub struct Assembler {
    /// By extended (never wrapping) sequence number.
    packets: BTreeMap<u64, Held>,
    highest: Option<u64>,
    /// The first sequence number of the next frame to deliver.
    next: Option<u64>,
    pub losses: u32,
}

impl Assembler {
    pub fn push(&mut self, sequence: u16, timestamp: u32, marker: bool, payload: &[u8], now: Instant) -> Pushed {
        let extended = self.extend(sequence);
        let mut pushed = Pushed::default();
        let old = self.next.is_some_and(|next| extended < next);
        if old || self.packets.contains_key(&extended) {
            return pushed; // already delivered, given up, or a duplicate
        }
        if let Some(highest) = self.highest.filter(|&h| extended > h + 1) {
            let from = (highest + 1).max(extended.saturating_sub(MAX_NACK));
            pushed.missing = (from..extended).map(|s| s as u16).collect();
        }
        self.highest = Some(self.highest.map_or(extended, |h| h.max(extended)));
        self.packets.insert(extended, Held { timestamp, marker, payload: payload.to_vec(), arrived: now });
        while self.packets.len() > MAX_PACKETS {
            self.packets.pop_first();
        }
        pushed.frames = self.deliver(now);
        pushed
    }

    /// Places a 16-bit sequence number next to the highest one seen.
    fn extend(&self, sequence: u16) -> u64 {
        let Some(highest) = self.highest else { return (1 << 16) | u64::from(sequence) };
        let cycle = highest & !0xFFFF;
        [cycle.saturating_sub(1 << 16), cycle, cycle + (1 << 16)]
            .into_iter()
            .map(|base| base | u64::from(sequence))
            .min_by_key(|candidate| candidate.abs_diff(highest))
            .unwrap_or(cycle | u64::from(sequence))
    }

    fn deliver(&mut self, now: Instant) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        loop {
            let start = match self.next {
                Some(next) => next,
                // The very first frame starts at the first packet that begins one.
                None => match self.packets.iter().find(|(_, p)| starts_frame(&p.payload)) {
                    Some((&sequence, _)) => *self.next.insert(sequence),
                    None => break,
                },
            };
            let mut end = None;
            let mut sequence = start;
            while let Some(packet) = self.packets.get(&sequence) {
                if packet.marker {
                    end = Some(sequence);
                    break;
                }
                sequence += 1;
            }
            if let Some(end) = end {
                let mut depacketizer = Depacketizer::default();
                let mut frame = None;
                for sequence in start..=end {
                    if let Some(packet) = self.packets.remove(&sequence) {
                        frame = depacketizer.push(sequence as u16, packet.timestamp, sequence == end, &packet.payload);
                    }
                }
                match frame {
                    Some(frame) => frames.push(frame),
                    None => self.losses += 1,
                }
                self.next = Some(end + 1);
                continue;
            }
            // Incomplete: wait for the missing packets, but not forever.
            let oldest = self.packets.range(start..).next().map(|(_, p)| p.arrived);
            if oldest.is_none_or(|at| now.duration_since(at) < MAX_WAIT) {
                break;
            }
            self.losses += 1;
            let lost_timestamp = self.packets.get(&start).map(|p| p.timestamp);
            let resume = self
                .packets
                .range(start + 1..)
                .find(|(_, p)| Some(p.timestamp) != lost_timestamp && starts_frame(&p.payload))
                .map(|(&sequence, _)| sequence);
            match resume {
                Some(sequence) => {
                    self.packets.retain(|&s, _| s >= sequence);
                    self.next = Some(sequence);
                }
                None => {
                    self.packets.clear();
                    self.next = self.highest.map(|h| h + 1);
                    break;
                }
            }
        }
        frames
    }
}

/// A packet that can begin a frame: a whole NAL unit, an aggregate, or a fragment start.
fn starts_frame(payload: &[u8]) -> bool {
    match payload.first().map(|h| h & NAL_TYPE_MASK) {
        Some(1..=23 | STAP_A) => true,
        Some(FU_A) => payload.get(1).is_some_and(|f| f & 0x80 != 0),
        _ => false,
    }
}

/// Splits an Annex B frame into RTP payloads of at most `max` bytes: small NAL
/// units whole, big ones as FU-A fragments (what WebRTC senders do).
pub fn packetize(frame: &[u8], max: usize) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    for nal in nal_units(frame) {
        if nal.len() <= max {
            packets.push(nal.to_vec());
            continue;
        }
        let chunks: Vec<&[u8]> = nal[1..].chunks(max - 2).collect();
        for (index, chunk) in chunks.iter().enumerate() {
            let flags = match index {
                0 => 0x80,
                i if i == chunks.len() - 1 => 0x40,
                _ => 0,
            };
            let mut packet = Vec::with_capacity(chunk.len() + 2);
            packet.extend_from_slice(&[(nal[0] & 0xE0) | FU_A, flags | (nal[0] & NAL_TYPE_MASK)]);
            packet.extend_from_slice(chunk);
            packets.push(packet);
        }
    }
    packets
}

/// The NAL units between 3- or 4-byte start codes.
fn nal_units(frame: &[u8]) -> Vec<&[u8]> {
    let starts: Vec<usize> = (0..frame.len().saturating_sub(2)).filter(|&i| frame[i..i + 3] == [0, 0, 1]).map(|i| i + 3).collect();
    let mut units = Vec::with_capacity(starts.len());
    for (index, &start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).map_or(frame.len(), |&next| {
            // The next start code, and its leading zero when it's 4 bytes.
            let code = if next >= 4 && frame[next - 4] == 0 { 4 } else { 3 };
            next - code
        });
        if end > start {
            units.push(&frame[start..end]);
        }
    }
    units
}

/// A frame the decoder can start from: it has a parameter set or an IDR slice.
/// NAL headers stay readable under DAVE, so this works before decryption too.
pub fn is_keyframe(frame: &[u8]) -> bool {
    frame
        .windows(5)
        .filter(|w| w[..4] == START_CODE)
        .any(|w| matches!(w[4] & NAL_TYPE_MASK, NAL_IDR | NAL_SPS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_and_aggregated_nal_units() {
        let mut d = Depacketizer::default();
        // STAP-A carrying SPS (0x67) and PPS (0x68), then an IDR slice (0x65).
        let stap = [STAP_A | 0x60, 0, 2, 0x67, 1, 0, 2, 0x68, 2];
        assert_eq!(d.push(1, 90, false, &stap), None);
        let frame = d.push(2, 90, true, &[0x65, 9, 9]).unwrap();
        assert_eq!(frame, [0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 9, 9]);
        assert!(is_keyframe(&frame));
        assert!(!is_keyframe(&[0, 0, 0, 1, 0x41, 1]), "a P slice isn't");
    }

    #[test]
    fn fragmented_nal_unit() {
        let mut d = Depacketizer::default();
        // FU indicator (NRI 3, type 28), FU header start/end with type 5.
        assert_eq!(d.push(10, 7, false, &[0x7C, 0x85, 1, 2]), None);
        assert_eq!(d.push(11, 7, false, &[0x7C, 0x05, 3]), None);
        let frame = d.push(12, 7, true, &[0x7C, 0x45, 4]).unwrap();
        assert_eq!(frame, [0, 0, 0, 1, 0x65, 1, 2, 3, 4]);
    }

    #[test]
    fn packetize_round_trips_through_the_depacketizer() {
        let big: Vec<u8> = std::iter::once(0x65).chain((0..3000).map(|i| (i % 251) as u8 + 1)).collect();
        let mut frame = vec![0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3];
        frame.extend_from_slice(&START_CODE);
        frame.extend_from_slice(&big);
        let packets = packetize(&frame, 1100);
        assert!(packets.iter().all(|p| p.len() <= 1100));
        assert_eq!(packets.len(), 2 + 3, "SPS, PPS, then the slice in three fragments");
        let mut d = Depacketizer::default();
        let mut out = None;
        for (index, packet) in packets.iter().enumerate() {
            out = d.push(index as u16, 9000, index == packets.len() - 1, packet);
        }
        let mut expected = vec![0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3, 0, 0, 0, 1];
        expected.extend_from_slice(&big);
        assert_eq!(out.unwrap(), expected);
    }

    #[test]
    fn assembler_reorders_and_waits_for_resends() {
        let now = Instant::now();
        let mut a = Assembler::default();
        // Frame 1: packets 10, 11 (marker); frame 2: 12, 13 (marker).
        assert_eq!(a.push(10, 1, false, &[0x7C, 0x85, 1], now).frames.len(), 0);
        // 11 is late: 12 arrives first and 11 is reported missing.
        let pushed = a.push(12, 2, false, &[0x41, 2], now);
        assert_eq!(pushed.missing, vec![11]);
        assert!(pushed.frames.is_empty());
        assert!(a.push(13, 2, true, &[0x41, 3], now).frames.is_empty(), "frame 2 waits for frame 1");
        // The resend fills the gap: both frames come out, in order.
        let pushed = a.push(11, 1, true, &[0x7C, 0x45, 9], now);
        assert_eq!(pushed.frames, vec![vec![0, 0, 0, 1, 0x65, 1, 9], vec![0, 0, 0, 1, 0x41, 2, 0, 0, 0, 1, 0x41, 3]]);
        assert_eq!(a.losses, 0);
    }

    #[test]
    fn assembler_gives_up_after_waiting() {
        let start = Instant::now();
        let mut a = Assembler::default();
        a.push(65534, 1, false, &[0x41, 1], start);
        // 65535 never comes; the next frame wraps the sequence number.
        assert!(a.push(0, 2, true, &[0x41, 2], start).frames.is_empty());
        let later = start + MAX_WAIT + Duration::from_millis(1);
        let pushed = a.push(1, 3, true, &[0x41, 3], later);
        assert_eq!(a.losses, 1, "frame 1 was given up");
        assert_eq!(pushed.frames, vec![vec![0, 0, 0, 1, 0x41, 2], vec![0, 0, 0, 1, 0x41, 3]]);
    }

    #[test]
    fn losses_drop_the_frame() {
        let mut d = Depacketizer::default();
        d.push(1, 100, false, &[0x7C, 0x85, 1]);
        // Sequence 2 never arrives.
        assert_eq!(d.push(3, 100, true, &[0x7C, 0x45, 3]), None);
        assert_eq!(d.losses, 1);
        // The next frame is fine again.
        assert_eq!(d.push(4, 200, true, &[0x41, 5]).unwrap(), [0, 0, 0, 1, 0x41, 5]);
        // A frame cut off by the next timestamp counts as lost too.
        d.push(5, 300, false, &[0x41, 6]);
        assert!(d.push(6, 400, true, &[0x41, 7]).is_some());
        assert_eq!(d.losses, 2);
        // A middle fragment without its start is unusable.
        assert_eq!(d.push(7, 500, true, &[0x7C, 0x45, 8]), None);
        assert_eq!(d.losses, 3);
    }
}
