//! Call audio processing: resampling, voice activity, jitter buffering, mixing.
//! Everything runs at Discord's format: 48 kHz, 20 ms frames.

use std::collections::BTreeMap;

pub const SAMPLE_RATE: u32 = 48_000;
/// Samples per channel in one 20 ms frame.
pub const FRAME_SAMPLES: usize = 960;
pub const CHANNELS: usize = 2;
/// Speech keeps the gate open this many frames after it drops below the threshold.
const GATE_HOLD_FRAMES: u32 = 15; // 300 ms
/// Frames buffered before a speaker starts playing, and the most kept.
const JITTER_START: usize = 2; // 40 ms
const JITTER_MAX: usize = 10; // 200 ms

/// Streaming linear resampler for interleaved audio. Good enough for speech,
/// cheap, and keeps state across callback-sized chunks.
pub struct Resampler {
    channels: usize,
    /// Input frames advanced per output frame.
    step: f64,
    /// Position in the current chunk; -1 is the last frame of the previous one.
    position: f64,
    previous: Vec<f32>,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32, channels: usize) -> Self {
        Self {
            channels,
            step: f64::from(from_rate) / f64::from(to_rate),
            position: 0.0,
            previous: vec![0.0; channels],
        }
    }

    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        let channels = self.channels;
        if self.step == 1.0 {
            output.extend_from_slice(input);
            return;
        }
        let frames = input.len() / channels;
        if frames == 0 {
            return;
        }
        let sample = |frame: isize, channel: usize| -> f32 {
            if frame < 0 { self.previous[channel] } else { input[frame as usize * channels + channel] }
        };
        while self.position < (frames - 1) as f64 {
            let index = self.position.floor();
            let fraction = (self.position - index) as f32;
            let index = index as isize;
            for channel in 0..channels {
                let (a, b) = (sample(index, channel), sample(index + 1, channel));
                output.push(a + (b - a) * fraction);
            }
            self.position += self.step;
        }
        self.position -= frames as f64;
        self.previous.copy_from_slice(&input[(frames - 1) * channels..frames * channels]);
    }
}

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Voice activity: open while the level is above the threshold, plus a short hold.
pub struct VoiceGate {
    threshold: f32,
    remaining: u32,
}

impl VoiceGate {
    pub fn new(threshold_db: f32) -> Self {
        Self { threshold: db_to_linear(threshold_db), remaining: 0 }
    }

    pub fn set_threshold_db(&mut self, threshold_db: f32) {
        self.threshold = db_to_linear(threshold_db);
    }

    /// Feeds one frame; returns whether it should be transmitted.
    pub fn update(&mut self, frame: &[f32]) -> bool {
        if rms(frame) >= self.threshold {
            self.remaining = GATE_HOLD_FRAMES;
        } else {
            self.remaining = self.remaining.saturating_sub(1);
        }
        self.remaining > 0
    }
}

/// Orders one speaker's decoded frames by RTP sequence and plays them out at a
/// steady rate, rebuffering after an underrun.
#[derive(Default)]
pub struct JitterBuffer {
    frames: BTreeMap<i64, Vec<f32>>,
    /// Extended (wrap-free) sequence of the last pushed packet.
    last: Option<i64>,
    /// Next sequence to play; `None` while (re)buffering.
    next: Option<i64>,
}

impl JitterBuffer {
    pub fn push(&mut self, sequence: u16, frame: Vec<f32>) {
        let extended = self.extend(sequence);
        if self.next.is_some_and(|next| extended < next) {
            return; // arrived after its turn
        }
        self.frames.insert(extended, frame);
        while self.frames.len() > JITTER_MAX {
            self.frames.pop_first();
            // Skip playback ahead past what was dropped.
            if let (Some(next), Some(&first)) = (self.next.as_mut(), self.frames.keys().next()) {
                *next = (*next).max(first);
            }
        }
    }

    /// The frame for this 20 ms tick, or `None` (silence) while buffering or on loss.
    pub fn pop(&mut self) -> Option<Vec<f32>> {
        let next = match self.next {
            Some(next) => next,
            None if self.frames.len() >= JITTER_START => *self.frames.keys().next()?,
            None => return None,
        };
        if self.frames.is_empty() {
            self.next = None; // underrun: rebuffer
            return None;
        }
        self.next = Some(next + 1);
        self.frames.remove(&next)
    }

    /// Maps a 16-bit sequence to the extended value closest to the previous one.
    fn extend(&mut self, sequence: u16) -> i64 {
        let extended = match self.last {
            None => i64::from(sequence),
            Some(last) => {
                let base = last - (last & 0xFFFF);
                [base - 0x1_0000, base, base + 0x1_0000]
                    .into_iter()
                    .map(|b| b + i64::from(sequence))
                    .min_by_key(|candidate| (candidate - last).abs())
                    .unwrap_or(i64::from(sequence))
            }
        };
        self.last = Some(self.last.map_or(extended, |last| last.max(extended)));
        extended
    }
}

/// Adds `frame` (scaled by `gain`) into `mix`.
pub fn mix_into(mix: &mut [f32], frame: &[f32], gain: f32) {
    for (out, sample) in mix.iter_mut().zip(frame) {
        *out += sample * gain;
    }
}

pub fn clamp(samples: &mut [f32]) {
    for sample in samples {
        *sample = sample.clamp(-1.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_identity_and_ratio() {
        let mut same = Resampler::new(48_000, 48_000, 2);
        let mut out = Vec::new();
        same.process(&[0.1, 0.2, 0.3, 0.4], &mut out);
        assert_eq!(out, [0.1, 0.2, 0.3, 0.4]);

        // 44.1 kHz -> 48 kHz over one second, fed in 10 ms chunks.
        let mut up = Resampler::new(44_100, 48_000, 1);
        let mut out = Vec::new();
        for _ in 0..100 {
            up.process(&[0.5; 441], &mut out);
        }
        assert!((out.len() as i64 - 48_000).abs() <= 2, "{}", out.len());
        assert!(out[100..].iter().all(|s| (s - 0.5).abs() < 1e-6), "constant signal stays constant");
    }

    #[test]
    fn resampler_interpolates_per_channel() {
        let mut down = Resampler::new(2, 1, 2);
        let mut out = Vec::new();
        // L ramps 0,1,2,3; R stays 9.
        down.process(&[0.0, 9.0, 1.0, 9.0, 2.0, 9.0, 3.0, 9.0], &mut out);
        assert_eq!(out, [0.0, 9.0, 2.0, 9.0]);
    }

    #[test]
    fn gate_holds_after_speech() {
        let mut gate = VoiceGate::new(-40.0);
        assert!(!gate.update(&[0.0; 960]));
        assert!(gate.update(&[0.5; 960]));
        for _ in 0..GATE_HOLD_FRAMES - 1 {
            assert!(gate.update(&[0.0; 960]));
        }
        assert!(!gate.update(&[0.0; 960]));
    }

    fn frame(tag: f32) -> Vec<f32> {
        vec![tag; 4]
    }

    #[test]
    fn jitter_reorders_buffers_and_rebuffers() {
        let mut jitter = JitterBuffer::default();
        jitter.push(11, frame(11.0));
        assert_eq!(jitter.pop(), None, "buffering");
        jitter.push(10, frame(10.0));
        assert_eq!(jitter.pop(), Some(frame(10.0)), "reordered");
        assert_eq!(jitter.pop(), Some(frame(11.0)));
        jitter.push(9, frame(9.0)); // too late
        assert_eq!(jitter.pop(), None, "underrun");
        jitter.push(12, frame(12.0));
        jitter.push(13, frame(13.0));
        assert_eq!(jitter.pop(), Some(frame(12.0)), "rebuffered");
    }

    #[test]
    fn jitter_handles_wraparound_and_loss() {
        let mut jitter = JitterBuffer::default();
        jitter.push(65_534, frame(1.0));
        jitter.push(65_535, frame(2.0));
        jitter.push(1, frame(4.0)); // 0 was lost
        assert_eq!(jitter.pop(), Some(frame(1.0)));
        assert_eq!(jitter.pop(), Some(frame(2.0)));
        assert_eq!(jitter.pop(), None, "lost packet plays as silence");
        assert_eq!(jitter.pop(), Some(frame(4.0)), "after the wrap");
    }

    #[test]
    fn mixing_sums_and_clamps() {
        let mut mix = vec![0.0; 2];
        mix_into(&mut mix, &[0.6, -0.6], 1.0);
        mix_into(&mut mix, &[0.6, -0.6], 1.0);
        clamp(&mut mix);
        assert_eq!(mix, [1.0, -1.0]);
    }
}
