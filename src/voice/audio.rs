//! Microphone and speakers for a call. The cpal streams live on their own
//! thread for the duration of the call; dropping [`AudioIo`] stops and joins it.
//! Audio crosses the boundary as 48 kHz samples in two bounded buffers.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};

use super::dsp::{Resampler, CHANNELS, FRAME_SAMPLES, SAMPLE_RATE};

/// Captured mono audio kept at most (latency bound if sending stalls).
const MAX_CAPTURE: usize = FRAME_SAMPLES * 5; // 100 ms
/// Playback audio (stereo samples) kept at most.
pub const MAX_PLAYBACK: usize = FRAME_SAMPLES * CHANNELS * 10; // 200 ms

pub type Buffer = Arc<Mutex<VecDeque<f32>>>;

pub struct AudioIo {
    /// 48 kHz mono from the microphone.
    pub capture: Buffer,
    /// 48 kHz interleaved stereo for the speakers.
    pub playback: Buffer,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for AudioIo {
    fn drop(&mut self) {
        drop(self.stop.take()); // wakes the audio thread
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Opens the devices (by name, or the system defaults). Either side may fail
/// independently; the call continues with whichever works.
/// `microphone: false` (watching a stream) opens only the speakers.
pub fn start(input: Option<String>, output: Option<String>, microphone: bool) -> (AudioIo, Option<String>, Option<String>) {
    let capture: Buffer = Arc::default();
    let playback: Buffer = Arc::default();
    let (stop, stopped) = mpsc::channel::<()>();
    let (ready, opened) = mpsc::channel();
    let (capture_in, playback_out) = (capture.clone(), playback.clone());
    let thread = std::thread::Builder::new()
        .name("ferrochat-audio".to_owned())
        .spawn(move || {
            let host = cpal::default_host();
            let input = if microphone { Some(open_input(&host, input.as_deref(), capture_in)) } else { None };
            let output = open_output(&host, output.as_deref(), playback_out);
            let errors = (input.as_ref().and_then(|i| i.as_ref().err().cloned()), output.as_ref().err().cloned());
            let _ = ready.send(errors);
            // Keep the streams alive until the call ends (sender dropped).
            let _ = stopped.recv();
            drop((input, output));
        });
    let (input_error, output_error) = match &thread {
        Ok(_) => opened.recv().unwrap_or_else(|_| (Some("audio thread died".into()), Some("audio thread died".into()))),
        Err(e) => (Some(format!("no audio thread: {e}")), Some(format!("no audio thread: {e}"))),
    };
    let io = AudioIo { capture, playback, stop: Some(stop), thread: thread.ok() };
    (io, input_error, output_error)
}

/// (input names, output names) for the Settings device pickers.
pub fn device_names() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    (names(host.input_devices()), names(host.output_devices()))
}

fn names<E>(devices: Result<impl Iterator<Item = Device>, E>) -> Vec<String> {
    devices.map(|devices| devices.filter_map(|d| d.name().ok()).collect()).unwrap_or_default()
}

fn find<E>(devices: Result<impl Iterator<Item = Device>, E>, name: &str) -> Option<Device> {
    devices.ok()?.find(|d| d.name().is_ok_and(|n| n == name))
}

fn open_input(host: &cpal::Host, name: Option<&str>, capture: Buffer) -> Result<Stream, String> {
    let device = match name {
        Some(name) => find(host.input_devices(), name).ok_or_else(|| format!("microphone \"{name}\" not found"))?,
        None => host.default_input_device().ok_or("no microphone")?,
    };
    let supported = device.default_input_config().map_err(|e| format!("microphone: {e}"))?;
    let format = supported.sample_format();
    let config = supported.config();
    let stream = match format {
        SampleFormat::F32 => build_input::<f32>(&device, &config, capture),
        SampleFormat::I16 => build_input::<i16>(&device, &config, capture),
        SampleFormat::U16 => build_input::<u16>(&device, &config, capture),
        other => return Err(format!("unsupported microphone format {other}")),
    }?;
    stream.play().map_err(|e| format!("microphone: {e}"))?;
    Ok(stream)
}

fn build_input<T>(device: &Device, config: &StreamConfig, capture: Buffer) -> Result<Stream, String>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels).max(1);
    let mut resampler = Resampler::new(config.sample_rate.0, SAMPLE_RATE, 1);
    let (mut mono, mut resampled) = (Vec::new(), Vec::new());
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                mono.clear();
                mono.extend(data.chunks(channels).map(|frame| {
                    frame.iter().map(|&s| f32::from_sample(s)).sum::<f32>() / channels as f32
                }));
                resampled.clear();
                resampler.process(&mono, &mut resampled);
                if let Ok(mut buffer) = capture.lock() {
                    buffer.extend(&resampled);
                    let excess = buffer.len().saturating_sub(MAX_CAPTURE);
                    buffer.drain(..excess);
                }
            },
            |_| {}, // device errors surface as silence; the call keeps going
            None,
        )
        .map_err(|e| format!("microphone: {e}"))
}

fn open_output(host: &cpal::Host, name: Option<&str>, playback: Buffer) -> Result<Stream, String> {
    let device = match name {
        Some(name) => find(host.output_devices(), name).ok_or_else(|| format!("speakers \"{name}\" not found"))?,
        None => host.default_output_device().ok_or("no speakers")?,
    };
    let supported = device.default_output_config().map_err(|e| format!("speakers: {e}"))?;
    let format = supported.sample_format();
    let config = supported.config();
    let stream = match format {
        SampleFormat::F32 => build_output::<f32>(&device, &config, playback),
        SampleFormat::I16 => build_output::<i16>(&device, &config, playback),
        SampleFormat::U16 => build_output::<u16>(&device, &config, playback),
        other => return Err(format!("unsupported speaker format {other}")),
    }?;
    stream.play().map_err(|e| format!("speakers: {e}"))?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens the real default devices for half a second. Plays only silence.
    #[test]
    #[ignore = "opens the real microphone and speakers"]
    fn opens_default_devices_and_captures() {
        let started = std::time::Instant::now();
        let (io, input_error, output_error) = start(None, None, true);
        println!("opened in {:?}; input error {input_error:?}; output error {output_error:?}", started.elapsed());
        println!("devices: {:?}", device_names());
        io.playback.lock().unwrap().extend(std::iter::repeat_n(0.0, FRAME_SAMPLES * CHANNELS * 10));
        std::thread::sleep(std::time::Duration::from_millis(500));
        let captured = io.capture.lock().unwrap().len();
        let played = MAX_PLAYBACK.min(FRAME_SAMPLES * CHANNELS * 10) - io.playback.lock().unwrap().len();
        println!("captured {captured} samples (bounded to {MAX_CAPTURE}), speakers consumed {played}");
        assert!(input_error.is_none() && output_error.is_none());
        assert!(captured > 0, "microphone delivered audio");
        assert!(played > 0, "speakers pulled audio");
        drop(io); // stops and joins the audio thread
    }
}

fn build_output<T>(device: &Device, config: &StreamConfig, playback: Buffer) -> Result<Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = usize::from(config.channels).max(1);
    let mut resampler = Resampler::new(SAMPLE_RATE, config.sample_rate.0, CHANNELS);
    // Stereo at the device rate, ready to write.
    let mut ready: VecDeque<f32> = VecDeque::new();
    let (mut chunk, mut resampled) = (Vec::new(), Vec::new());
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let frames = data.len() / channels;
                while ready.len() < frames * CHANNELS {
                    // 10 ms of 48 kHz stereo; silence when nothing is queued.
                    chunk.clear();
                    if let Ok(mut buffer) = playback.lock() {
                        let take = buffer.len().min(FRAME_SAMPLES);
                        chunk.extend(buffer.drain(..take));
                    }
                    chunk.resize(FRAME_SAMPLES, 0.0);
                    resampled.clear();
                    resampler.process(&chunk, &mut resampled);
                    ready.extend(&resampled);
                }
                for frame in data.chunks_mut(channels) {
                    let (left, right) = (ready.pop_front().unwrap_or(0.0), ready.pop_front().unwrap_or(0.0));
                    if channels == 1 {
                        frame[0] = T::from_sample((left + right) / 2.0);
                        continue;
                    }
                    for (index, sample) in frame.iter_mut().enumerate() {
                        *sample = T::from_sample(match index {
                            0 => left,
                            1 => right,
                            _ => 0.0,
                        });
                    }
                }
            },
            |_| {},
            None,
        )
        .map_err(|e| format!("speakers: {e}"))
}
