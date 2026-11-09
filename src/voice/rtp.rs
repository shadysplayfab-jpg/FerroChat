//! Voice UDP: IP discovery, RTP framing and transport encryption
//! (`aead_aes256_gcm_rtpsize` / `aead_xchacha20_poly1305_rtpsize`).

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::Aes256Gcm;
use chacha20poly1305::XChaCha20Poly1305;

pub const RTP_HEADER_LEN: usize = 12;
const DISCOVERY_LEN: usize = 74;
/// Samples per 20 ms Opus frame at 48 kHz.
pub const FRAME_SAMPLES: u32 = 960;
/// Opus silence; Discord expects five of these when a speaker stops.
pub const SILENCE_FRAME: [u8; 3] = [0xF8, 0xFF, 0xFE];
const TAG_LEN: usize = 16;
/// The nonce counter is sent as 4 bytes at the end of each packet.
const NONCE_SUFFIX_LEN: usize = 4;
const RTP_VERSION_FLAGS: u8 = 0x80;
const OPUS_PAYLOAD_TYPE: u8 = 0x78;

/// IP discovery request: type 1, length 70, our SSRC, empty address and port.
pub fn discovery_request(ssrc: u32) -> [u8; DISCOVERY_LEN] {
    let mut packet = [0u8; DISCOVERY_LEN];
    packet[0..2].copy_from_slice(&1u16.to_be_bytes());
    packet[2..4].copy_from_slice(&70u16.to_be_bytes());
    packet[4..8].copy_from_slice(&ssrc.to_be_bytes());
    packet
}

/// Our external (address, port) from an IP discovery response.
pub fn parse_discovery_response(packet: &[u8]) -> Option<(String, u16)> {
    if packet.len() < DISCOVERY_LEN || u16::from_be_bytes([packet[0], packet[1]]) != 2 {
        return None;
    }
    let address = &packet[8..72];
    let end = address.iter().position(|&b| b == 0).unwrap_or(address.len());
    let ip = std::str::from_utf8(&address[..end]).ok()?.to_owned();
    let port = u16::from_be_bytes([packet[72], packet[73]]);
    (!ip.is_empty()).then_some((ip, port))
}

const RTCP_HEADER_LEN: usize = 8;

/// RTCP picture loss indication (payload-specific feedback, FMT 1): asks the
/// sender of `media_ssrc` for a keyframe.
pub fn picture_loss(our_ssrc: u32, media_ssrc: u32) -> [u8; 12] {
    let mut packet = [0u8; 12];
    packet[0] = RTP_VERSION_FLAGS | 1;
    packet[1] = 206;
    packet[2..4].copy_from_slice(&2u16.to_be_bytes()); // length in words, minus one
    packet[4..8].copy_from_slice(&our_ssrc.to_be_bytes());
    packet[8..12].copy_from_slice(&media_ssrc.to_be_bytes());
    packet
}

/// RTCP generic NACK (transport feedback, FMT 1): asks `media_ssrc`'s sender to
/// resend `lost` packets. Each entry covers a sequence number and the 16 after it.
pub fn nack(our_ssrc: u32, media_ssrc: u32, lost: &[u16]) -> Vec<u8> {
    let mut entries: Vec<(u16, u16)> = Vec::new();
    for &sequence in lost {
        match entries.last_mut() {
            Some((first, mask)) if sequence.wrapping_sub(*first).wrapping_sub(1) < 16 && sequence != *first => {
                *mask |= 1 << sequence.wrapping_sub(*first).wrapping_sub(1);
            }
            _ => entries.push((sequence, 0)),
        }
    }
    let mut packet = Vec::with_capacity(12 + entries.len() * 4);
    packet.push(RTP_VERSION_FLAGS | 1);
    packet.push(205);
    packet.extend_from_slice(&((2 + entries.len()) as u16).to_be_bytes());
    packet.extend_from_slice(&our_ssrc.to_be_bytes());
    packet.extend_from_slice(&media_ssrc.to_be_bytes());
    for (first, mask) in entries {
        packet.extend_from_slice(&first.to_be_bytes());
        packet.extend_from_slice(&mask.to_be_bytes());
    }
    packet
}

/// What a viewer asked of us in a (possibly compound) RTCP packet.
#[derive(Debug, Default, PartialEq)]
pub struct Feedback {
    pub keyframe: bool,
    /// Sequence numbers to resend.
    pub resend: Vec<u16>,
}

pub fn parse_feedback(mut rtcp: &[u8]) -> Feedback {
    let mut feedback = Feedback::default();
    while rtcp.len() >= 4 {
        let format = rtcp[0] & 0x1F;
        let kind = rtcp[1];
        let length = (usize::from(u16::from_be_bytes([rtcp[2], rtcp[3]])) + 1) * 4;
        let Some(packet) = rtcp.get(..length) else { break };
        match (kind, format) {
            (206, 1) => feedback.keyframe = true,
            (205, 1) => {
                for entry in packet.get(12..).unwrap_or_default().chunks_exact(4) {
                    let first = u16::from_be_bytes([entry[0], entry[1]]);
                    let mask = u16::from_be_bytes([entry[2], entry[3]]);
                    feedback.resend.push(first);
                    feedback.resend.extend((0..16).filter(|bit| mask & (1 << bit) != 0).map(|bit| first.wrapping_add(bit + 1)));
                }
            }
            _ => {}
        }
        rtcp = &rtcp[length..];
    }
    feedback
}

pub fn rtp_header(sequence: u16, timestamp: u32, ssrc: u32) -> [u8; RTP_HEADER_LEN] {
    let mut header = [0u8; RTP_HEADER_LEN];
    header[0] = RTP_VERSION_FLAGS;
    header[1] = OPUS_PAYLOAD_TYPE;
    header[2..4].copy_from_slice(&sequence.to_be_bytes());
    header[4..8].copy_from_slice(&timestamp.to_be_bytes());
    header[8..12].copy_from_slice(&ssrc.to_be_bytes());
    header
}

/// An H.264 packet's header: payload type 101, `marker` on a frame's last packet.
pub fn video_header(sequence: u16, timestamp: u32, ssrc: u32, marker: bool) -> [u8; RTP_HEADER_LEN] {
    let mut header = rtp_header(sequence, timestamp, ssrc);
    header[1] = super::video::H264_PAYLOAD_TYPE | if marker { 0x80 } else { 0 };
    header
}

/// A retransmission's header (RFC 4588): payload type 102 on the RTX SSRC, with
/// its own sequence numbers; the payload starts with the original's.
pub fn rtx_header(sequence: u16, timestamp: u32, rtx_ssrc: u32, marker: bool) -> [u8; RTP_HEADER_LEN] {
    let mut header = rtp_header(sequence, timestamp, rtx_ssrc);
    header[1] = super::video::RTX_PAYLOAD_TYPE | if marker { 0x80 } else { 0 };
    header
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Aes256Gcm,
    XChaCha20,
}

impl Mode {
    /// Preference order. Discord requires clients to support XChaCha20.
    const PREFERRED: [Mode; 2] = [Mode::Aes256Gcm, Mode::XChaCha20];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Aes256Gcm => "aead_aes256_gcm_rtpsize",
            Mode::XChaCha20 => "aead_xchacha20_poly1305_rtpsize",
        }
    }

    pub fn choose(offered: &[String]) -> Option<Mode> {
        Self::PREFERRED.into_iter().find(|mode| offered.iter().any(|o| o == mode.name()))
    }
}

/// A packet we received and decrypted.
#[derive(Debug, PartialEq)]
pub struct Received {
    /// 0x78 for Opus, 101 for H.264; RTCP packets have other types.
    pub payload_type: u8,
    /// Last packet of a video frame.
    pub marker: bool,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    /// The Opus frame (or DAVE-encrypted frame), header extension removed.
    pub payload: Vec<u8>,
}

pub enum Cipher {
    Aes(Box<Aes256Gcm>),
    XChaCha(Box<XChaCha20Poly1305>),
}

impl Cipher {
    pub fn new(mode: Mode, key: &[u8]) -> Option<Self> {
        if key.len() != 32 {
            return None;
        }
        Some(match mode {
            Mode::Aes256Gcm => Cipher::Aes(Box::new(Aes256Gcm::new_from_slice(key).ok()?)),
            Mode::XChaCha20 => Cipher::XChaCha(Box::new(XChaCha20Poly1305::new_from_slice(key).ok()?)),
        })
    }

    /// `header || ciphertext || tag || counter`. The RTP header is the associated
    /// data; the nonce is the 32-bit counter followed by zeros.
    pub fn seal(&self, header: &[u8], payload: &[u8], counter: u32) -> Result<Vec<u8>, String> {
        let mut buffer = Vec::with_capacity(payload.len() + TAG_LEN);
        buffer.extend_from_slice(payload);
        self.apply(header, &mut buffer, counter.to_be_bytes(), true)?;
        let mut packet = Vec::with_capacity(header.len() + buffer.len() + NONCE_SUFFIX_LEN);
        packet.extend_from_slice(header);
        packet.extend_from_slice(&buffer);
        packet.extend_from_slice(&counter.to_be_bytes());
        Ok(packet)
    }

    /// Decrypts a received packet. With the rtpsize modes, the fixed header, CSRCs
    /// and the 4-byte extension preamble are unencrypted associated data, while
    /// the extension body is encrypted and is stripped from the result.
    pub fn open(&self, packet: &[u8]) -> Option<Received> {
        if packet.len() < RTP_HEADER_LEN {
            return None;
        }
        let csrc_count = usize::from(packet[0] & 0x0F);
        let has_extension = packet[0] & 0x10 != 0;
        let mut aad_len = RTP_HEADER_LEN + 4 * csrc_count;
        if has_extension {
            aad_len += 4;
        }
        if packet.len() < aad_len + TAG_LEN + NONCE_SUFFIX_LEN {
            return None;
        }
        let extension_words =
            if has_extension { usize::from(u16::from_be_bytes([packet[aad_len - 2], packet[aad_len - 1]])) } else { 0 };
        let (body, nonce) = packet.split_at(packet.len() - NONCE_SUFFIX_LEN);
        let mut buffer = body[aad_len..].to_vec();
        self.apply(&body[..aad_len], &mut buffer, nonce.try_into().ok()?, false).ok()?;
        let payload = buffer.get(extension_words * 4..)?.to_vec();
        Some(Received {
            payload_type: packet[1] & 0x7F,
            marker: packet[1] & 0x80 != 0,
            sequence: u16::from_be_bytes([packet[2], packet[3]]),
            timestamp: u32::from_be_bytes(packet[4..8].try_into().ok()?),
            ssrc: u32::from_be_bytes(packet[8..12].try_into().ok()?),
            payload,
        })
    }

    /// Decrypts an RTCP packet; returns it whole (header and body), or `None`.
    pub fn open_rtcp(&self, packet: &[u8]) -> Option<Vec<u8>> {
        if packet.len() < RTCP_HEADER_LEN + TAG_LEN + NONCE_SUFFIX_LEN {
            return None;
        }
        let (body, nonce) = packet.split_at(packet.len() - NONCE_SUFFIX_LEN);
        let mut buffer = body[RTCP_HEADER_LEN..].to_vec();
        self.apply(&body[..RTCP_HEADER_LEN], &mut buffer, nonce.try_into().ok()?, false).ok()?;
        let mut rtcp = body[..RTCP_HEADER_LEN].to_vec();
        rtcp.extend_from_slice(&buffer);
        Some(rtcp)
    }

    /// An RTCP packet: the 8-byte header (with the sender SSRC) is associated
    /// data and the rest is encrypted, as with RTP in the rtpsize modes.
    pub fn seal_rtcp(&self, rtcp: &[u8], counter: u32) -> Result<Vec<u8>, String> {
        let (header, body) = rtcp.split_at(RTCP_HEADER_LEN.min(rtcp.len()));
        self.seal(header, body, counter)
    }

    fn apply(&self, aad: &[u8], buffer: &mut Vec<u8>, counter: [u8; 4], encrypt: bool) -> Result<(), String> {
        let failed = |_| if encrypt { "voice encryption failed" } else { "voice decryption failed" }.to_owned();
        match self {
            Cipher::Aes(cipher) => {
                let mut nonce = [0u8; 12];
                nonce[..4].copy_from_slice(&counter);
                let nonce = aes_gcm::Nonce::from_slice(&nonce);
                if encrypt {
                    cipher.encrypt_in_place(nonce, aad, buffer).map_err(failed)
                } else {
                    cipher.decrypt_in_place(nonce, aad, buffer).map_err(failed)
                }
            }
            Cipher::XChaCha(cipher) => {
                let mut nonce = [0u8; 24];
                nonce[..4].copy_from_slice(&counter);
                let nonce = chacha20poly1305::XNonce::from_slice(&nonce);
                if encrypt {
                    cipher.encrypt_in_place(nonce, aad, buffer).map_err(failed)
                } else {
                    cipher.decrypt_in_place(nonce, aad, buffer).map_err(failed)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn discovery_round_trip() {
        let request = discovery_request(0x01020304);
        assert_eq!(&request[..8], &[0, 1, 0, 70, 1, 2, 3, 4]);
        let mut response = [0u8; DISCOVERY_LEN];
        response[0..2].copy_from_slice(&2u16.to_be_bytes());
        response[8..8 + 11].copy_from_slice(b"203.0.113.9");
        response[72..74].copy_from_slice(&50004u16.to_be_bytes());
        assert_eq!(parse_discovery_response(&response), Some(("203.0.113.9".to_owned(), 50004)));
        assert_eq!(parse_discovery_response(&request), None, "a request is not a response");
    }

    #[test]
    fn chooses_preferred_mode() {
        let offered = vec!["aead_xchacha20_poly1305_rtpsize".to_owned(), "aead_aes256_gcm_rtpsize".to_owned()];
        assert_eq!(Mode::choose(&offered), Some(Mode::Aes256Gcm));
        assert_eq!(Mode::choose(&offered[..1]), Some(Mode::XChaCha20));
        assert_eq!(Mode::choose(&["xsalsa20_poly1305".to_owned()]), None);
    }

    #[test]
    fn seal_open_round_trip_both_modes() {
        for mode in [Mode::Aes256Gcm, Mode::XChaCha20] {
            let cipher = Cipher::new(mode, &KEY).unwrap();
            let header = rtp_header(5, 960 * 5, 42);
            let packet = cipher.seal(&header, &SILENCE_FRAME, 9).unwrap();
            assert_eq!(packet.len(), RTP_HEADER_LEN + 3 + TAG_LEN + 4);
            assert_eq!(&packet[packet.len() - 4..], &9u32.to_be_bytes());
            let received = cipher.open(&packet).unwrap();
            let expected =
                Received { payload_type: 0x78, marker: false, sequence: 5, timestamp: 4800, ssrc: 42, payload: SILENCE_FRAME.to_vec() };
            assert_eq!(received, expected);

            let mut tampered = packet.clone();
            tampered[1] ^= 1; // header is authenticated
            assert!(cipher.open(&tampered).is_none(), "{mode:?}");
        }
    }

    #[test]
    fn nack_round_trips_and_packs_neighbours() {
        let lost = [100, 101, 105, 116, 117, 300];
        let packet = nack(1, 2, &lost);
        // 100 (+101, 105, 116 in its mask), then 117, then 300.
        assert_eq!(packet.len(), 12 + 3 * 4);
        let feedback = parse_feedback(&packet);
        assert_eq!(feedback.resend, lost);
        assert!(!feedback.keyframe);
        let mut compound = picture_loss(1, 2).to_vec();
        compound.extend_from_slice(&packet);
        let both = parse_feedback(&compound);
        assert!(both.keyframe && both.resend.len() == lost.len());
    }

    #[test]
    fn rtcp_seal_open_round_trip() {
        let cipher = Cipher::new(Mode::XChaCha20, &KEY).unwrap();
        let rtcp = nack(1, 2, &[7, 9]);
        let sealed = cipher.seal_rtcp(&rtcp, 3).unwrap();
        assert_eq!(cipher.open_rtcp(&sealed).unwrap(), rtcp);
    }

    #[test]
    fn strips_encrypted_header_extension() {
        let cipher = Cipher::new(Mode::Aes256Gcm, &KEY).unwrap();
        let mut header = rtp_header(1, 960, 7).to_vec();
        header[0] |= 0x10; // extension present
        header.extend_from_slice(&[0xBE, 0xDE, 0x00, 0x01]); // profile, 1 word
        let mut plaintext = vec![0x10, 0xAA, 0x00, 0x00]; // extension body (encrypted)
        plaintext.extend_from_slice(b"opus");
        let packet = cipher.seal(&header, &plaintext, 1).unwrap();
        assert_eq!(cipher.open(&packet).unwrap().payload, b"opus");
    }
}
