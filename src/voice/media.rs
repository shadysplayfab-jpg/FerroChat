//! Per-call media: Opus in and out, voice activity, per-speaker jitter buffers
//! and the playback mix. Network I/O stays in the voice task; this only turns
//! audio into frames and frames into audio.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use audiopus::coder::{Decoder, Encoder};
use audiopus::{Application, Bitrate, Channels, SampleRate};
use nnnoiseless::DenoiseState;

use super::audio::{AudioIo, MAX_PLAYBACK};
use super::dave::Dave;
use super::dsp::{self, JitterBuffer, VoiceGate, CHANNELS, FRAME_SAMPLES};
use super::rtp::{self, Received};
use super::Controls;

/// Discord's default voice bitrate.
const BITRATE: i32 = 64_000;
const OPUS_PAYLOAD_TYPE: u8 = 0x78;
/// Silence frames sent when we stop talking, as Discord asks.
const TRAILING_SILENCE: u8 = 5;
/// Largest Opus packet we produce, and the most samples one packet can decode to.
const MAX_OPUS_BYTES: usize = 4000;
const MAX_DECODED: usize = 5760 * CHANNELS; // 120 ms
/// Speakers silent for this long have their decoder and buffer freed.
const SPEAKER_IDLE: Duration = Duration::from_secs(10);

struct Speaker {
    decoder: Decoder,
    jitter: JitterBuffer,
    last_heard: Instant,
}

/// What one 20 ms tick produced.
#[derive(Default)]
pub struct Tick {
    /// An Opus frame to send (speech or trailing silence).
    pub frame: Option<Vec<u8>>,
    /// We started (`true`) or stopped (`false`) talking.
    pub speaking: Option<bool>,
}

pub struct Media {
    io: AudioIo,
    encoder: Encoder,
    speakers: HashMap<u32, Speaker>,
    gate: VoiceGate,
    speaking: bool,
    silence_left: u8,
    mono: Vec<f32>,
    stereo: Vec<f32>,
    mix: Vec<f32>,
    opus: Vec<u8>,
    decoded: Vec<f32>,
    ticks: u64,
    /// Created when noise suppression is on (~50 KB), dropped when it's turned off.
    denoiser: Option<Box<DenoiseState<'static>>>,
}

/// RNNoise over `mono` in its 10 ms blocks. It expects 16-bit sample values.
fn denoise(state: &mut DenoiseState<'static>, mono: &mut [f32]) {
    let mut input = [0f32; DenoiseState::FRAME_SIZE];
    let mut output = [0f32; DenoiseState::FRAME_SIZE];
    for block in mono.chunks_exact_mut(DenoiseState::FRAME_SIZE) {
        for (scaled, &sample) in input.iter_mut().zip(block.iter()) {
            *scaled = sample * 32_768.0;
        }
        state.process_frame(&mut output, &input);
        for (sample, &cleaned) in block.iter_mut().zip(&output) {
            *sample = cleaned / 32_768.0;
        }
    }
}

/// Whether a push-to-talk key (or mouse button) is held right now, wherever focus is.
#[cfg(windows)]
fn key_down(key: u32) -> bool {
    let Ok(key) = i32::try_from(key) else { return false };
    // The high bit of GetAsyncKeyState is "currently down".
    unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(key) as u16 & 0x8000 != 0 }
}

#[cfg(not(windows))]
fn key_down(_key: u32) -> bool {
    false
}

impl Media {
    pub fn new(io: AudioIo, controls: &Controls) -> Result<Self, String> {
        let mut encoder = Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::Voip)
            .map_err(|e| format!("Opus encoder: {e}"))?;
        encoder.set_bitrate(Bitrate::BitsPerSecond(BITRATE)).map_err(|e| format!("Opus bitrate: {e}"))?;
        Ok(Self {
            io,
            encoder,
            speakers: HashMap::new(),
            gate: VoiceGate::new(controls.threshold_db()),
            speaking: false,
            silence_left: 0,
            mono: Vec::with_capacity(FRAME_SAMPLES),
            stereo: vec![0.0; FRAME_SAMPLES * CHANNELS],
            mix: vec![0.0; FRAME_SAMPLES * CHANNELS],
            opus: vec![0; MAX_OPUS_BYTES],
            decoded: vec![0.0; MAX_DECODED],
            ticks: 0,
            denoiser: None,
        })
    }

    pub fn tick(&mut self, controls: &Controls) -> Result<Tick, String> {
        self.ticks += 1;
        let mut tick = Tick::default();

        // Send: 20 ms from the microphone, gated by voice activity.
        self.mono.clear();
        if let Ok(mut capture) = self.io.capture.lock() {
            if capture.len() >= FRAME_SAMPLES {
                self.mono.extend(capture.drain(..FRAME_SAMPLES));
            }
        }
        let gain = controls.input_gain();
        self.mono.iter_mut().for_each(|s| *s *= gain);
        let full = self.mono.len() == FRAME_SAMPLES;
        // Denoise before the gate, so background noise doesn't open it.
        if controls.noise_suppression() && full {
            self.denoise();
        } else if !controls.noise_suppression() {
            self.denoiser = None;
        }
        self.gate.set_threshold_db(controls.threshold_db());
        let voice = match controls.ptt_key() {
            Some(key) => key_down(key),
            None => self.gate.update(&self.mono),
        };
        let transmitting = voice && full && !controls.muted() && !controls.deafened();
        if transmitting {
            if !self.speaking {
                self.speaking = true;
                tick.speaking = Some(true);
            }
            self.silence_left = TRAILING_SILENCE;
            for (pair, &sample) in self.stereo.chunks_mut(CHANNELS).zip(&self.mono) {
                pair.fill(sample.clamp(-1.0, 1.0));
            }
            let len = self.encoder.encode_float(&self.stereo, &mut self.opus).map_err(|e| format!("Opus encode: {e}"))?;
            tick.frame = Some(self.opus[..len].to_vec());
        } else if self.speaking {
            tick.frame = Some(rtp::SILENCE_FRAME.to_vec());
            self.silence_left = self.silence_left.saturating_sub(1);
            if self.silence_left == 0 {
                self.speaking = false;
                tick.speaking = Some(false);
            }
        }

        // Play: one frame from each speaker, mixed.
        let deafened = controls.deafened();
        let volume = controls.output_volume();
        self.mix.fill(0.0);
        let mut audible = false;
        for speaker in self.speakers.values_mut() {
            if let Some(frame) = speaker.jitter.pop() {
                if !deafened {
                    dsp::mix_into(&mut self.mix, &frame, volume);
                    audible = true;
                }
            }
        }
        if audible {
            dsp::clamp(&mut self.mix);
            if let Ok(mut playback) = self.io.playback.lock() {
                playback.extend(&self.mix);
                let excess = playback.len().saturating_sub(MAX_PLAYBACK);
                playback.drain(..excess);
            }
        }

        if self.ticks % 250 == 0 {
            let now = Instant::now();
            self.speakers.retain(|_, s| now.duration_since(s.last_heard) < SPEAKER_IDLE);
        }
        Ok(tick)
    }

    fn denoise(&mut self) {
        let state = self.denoiser.get_or_insert_with(DenoiseState::new);
        denoise(state, &mut self.mono);
    }

    /// Handles one decrypted RTP packet. DAVE needs the sender's user id, which
    /// comes from the voice server's speaking events (`ssrc_users`).
    pub fn receive(&mut self, packet: Received, ssrc_users: &HashMap<u32, u64>, dave: &mut Dave, controls: &Controls) {
        if packet.payload_type != OPUS_PAYLOAD_TYPE || controls.deafened() {
            return; // RTCP, or nobody's listening
        }
        let Some(&user_id) = ssrc_users.get(&packet.ssrc) else { return };
        let Ok(opus) = dave.decrypt(user_id, &packet.payload) else { return };
        let speaker = match self.speakers.entry(packet.ssrc) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let Ok(decoder) = Decoder::new(SampleRate::Hz48000, Channels::Stereo) else { return };
                entry.insert(Speaker { decoder, jitter: JitterBuffer::default(), last_heard: Instant::now() })
            }
        };
        speaker.last_heard = Instant::now();
        let Ok(samples) = speaker.decoder.decode_float(Some(opus.as_slice()), self.decoded.as_mut_slice(), false) else {
            return;
        };
        speaker.jitter.push(packet.sequence, self.decoded[..samples * CHANNELS].to_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steady hiss (no speech) should come out much quieter once RNNoise settles.
    #[test]
    fn noise_suppression_quiets_hiss() {
        let mut state = DenoiseState::new();
        let mut seed = 0x1234_5678u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 0.1 - 0.05
        };
        let (mut before, mut after) = (0.0, 0.0);
        for frame in 0..100 {
            let mut mono: Vec<f32> = (0..FRAME_SAMPLES).map(|_| noise()).collect();
            let level = dsp::rms(&mono);
            denoise(&mut state, &mut mono);
            if frame >= 50 {
                before += level;
                after += dsp::rms(&mono);
            }
        }
        assert!(after < before * 0.5, "denoised level {after} vs {before}");
    }

    /// Opus round trip at the frame size and format we send.
    #[test]
    fn opus_round_trip() {
        let mut encoder = Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::Voip).unwrap();
        encoder.set_bitrate(Bitrate::BitsPerSecond(BITRATE)).unwrap();
        let mut decoder = Decoder::new(SampleRate::Hz48000, Channels::Stereo).unwrap();
        let tone: Vec<f32> = (0..FRAME_SAMPLES)
            .flat_map(|i| {
                let s = (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
                [s, s]
            })
            .collect();
        let mut packet = vec![0u8; MAX_OPUS_BYTES];
        let mut decoded = vec![0f32; MAX_DECODED];
        let mut energy = 0.0;
        // A few frames so the codec settles.
        for _ in 0..5 {
            let len = encoder.encode_float(&tone, &mut packet).unwrap();
            assert!(len > 1 && len < 400, "20 ms at 64 kbps is ~160 bytes, got {len}");
            let samples = decoder.decode_float(Some(&packet[..len]), &mut decoded[..], false).unwrap();
            assert_eq!(samples, FRAME_SAMPLES);
            energy = dsp::rms(&decoded[..samples * CHANNELS]);
        }
        assert!((energy - dsp::rms(&tone)).abs() < 0.1, "decoded level {energy} close to input");
    }
}
