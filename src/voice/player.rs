//! Plays voice messages and audio attachments. Decoding runs on its own thread
//! a little ahead of the speakers (about 300 ms), so a long file never sits in
//! memory as raw samples. Voice messages are Ogg Opus, read here with libopus;
//! other formats (MP3, AAC, WAV, FLAC, WMA) go through Windows' Media Foundation.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use audiopus::coder::Decoder as OpusDecoder;
use audiopus::{Channels, SampleRate};

use super::audio;
use super::dsp::{Resampler, CHANNELS, SAMPLE_RATE};

/// Decoded audio queued ahead of the speakers (48 kHz stereo samples).
const AHEAD: usize = SAMPLE_RATE as usize * CHANNELS * 3 / 10;

/// Shared between the UI and the playback thread.
#[derive(Default)]
pub struct Progress {
    /// Milliseconds played.
    pub position: AtomicU32,
    pub finished: AtomicBool,
    pub error: Mutex<Option<String>>,
    stop: AtomicBool,
}

/// A file being played. Dropping it stops playback.
pub struct Playback {
    pub url: String,
    pub progress: Arc<Progress>,
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.progress.stop.store(true, Ordering::Relaxed);
    }
}

impl Playback {
    pub fn is_finished(&self) -> bool {
        self.progress.finished.load(Ordering::Relaxed)
    }

    pub fn position_ms(&self) -> u32 {
        self.progress.position.load(Ordering::Relaxed)
    }

    pub fn error(&self) -> Option<String> {
        self.progress.error.lock().ok().and_then(|e| e.clone())
    }
}

/// Starts playing `bytes` (a whole file named `name`) on `output`, or the default
/// speakers, at `volume` (1.0 is unchanged).
pub fn play(url: String, name: &str, bytes: Vec<u8>, output: Option<String>, volume: f32) -> Playback {
    let progress = Arc::new(Progress::default());
    let shared = progress.clone();
    let extension = name.rsplit('.').next().unwrap_or("bin").to_ascii_lowercase();
    let spawned = std::thread::Builder::new().name("ferrochat-player".to_owned()).spawn(move || {
        if let Err(error) = run(&bytes, &extension, output, volume, &shared) {
            if let Ok(mut slot) = shared.error.lock() {
                *slot = Some(error);
            }
        }
        shared.finished.store(true, Ordering::Relaxed);
    });
    if let Err(error) = spawned {
        *progress.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("no player thread: {error}"));
        progress.finished.store(true, Ordering::Relaxed);
    }
    Playback { url, progress }
}

/// Something that yields interleaved float samples at its own rate.
trait Source {
    fn rate(&self) -> u32;
    fn channels(&self) -> usize;
    /// Appends the next chunk; `false` at the end.
    fn next(&mut self, out: &mut Vec<f32>) -> Result<bool, String>;
}

fn run(bytes: &[u8], extension: &str, output: Option<String>, volume: f32, progress: &Progress) -> Result<(), String> {
    let mut source: Box<dyn Source> = if bytes.starts_with(b"OggS") && contains(bytes, b"OpusHead") {
        Box::new(OggOpus::new(bytes.to_vec())?)
    } else {
        open_media_foundation(bytes, extension)?
    };
    let (io, _, output_error) = audio::start(None, output, false);
    if let Some(error) = output_error {
        return Err(error);
    }
    let channels = source.channels().max(1);
    let mut resampler = Resampler::new(source.rate(), SAMPLE_RATE, CHANNELS);
    let (mut chunk, mut stereo, mut resampled) = (Vec::new(), Vec::new(), Vec::new());
    let mut queued: u64 = 0;
    let mut ended = false;
    loop {
        if progress.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let pending = io.playback.lock().map(|b| b.len()).unwrap_or(0);
        let played = queued.saturating_sub(pending as u64) / CHANNELS as u64;
        progress.position.store((played * 1000 / u64::from(SAMPLE_RATE)) as u32, Ordering::Relaxed);
        if ended {
            if pending == 0 {
                return Ok(());
            }
        } else if pending < AHEAD {
            chunk.clear();
            ended = !source.next(&mut chunk)?;
            // Mono and multichannel to stereo (extra channels are left out).
            stereo.clear();
            for frame in chunk.chunks(channels) {
                let left = frame[0];
                let right = if channels > 1 { frame[1] } else { left };
                stereo.extend([left * volume, right * volume]);
            }
            resampled.clear();
            resampler.process(&stereo, &mut resampled);
            queued += resampled.len() as u64;
            if let Ok(mut buffer) = io.playback.lock() {
                buffer.extend(&resampled);
            }
            continue;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).take(4096).any(|w| w == needle)
}

/// Splits an Ogg stream into packets (RFC 3533). Only the first logical stream is read.
pub struct OggPackets {
    data: Vec<u8>,
    offset: usize,
    serial: Option<u32>,
    /// Lacing values and body of the current page.
    segments: Vec<u8>,
    body: usize,
    segment: usize,
    partial: Vec<u8>,
}

impl OggPackets {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data, offset: 0, serial: None, segments: Vec::new(), body: 0, segment: 0, partial: Vec::new() }
    }

    fn next_page(&mut self) -> bool {
        loop {
            let header = match self.data.get(self.offset..self.offset + 27) {
                Some(header) if header.starts_with(b"OggS") => header,
                _ => return false,
            };
            let serial = u32::from_le_bytes(header[14..18].try_into().unwrap_or_default());
            let count = usize::from(header[26]);
            let table = self.offset + 27;
            let Some(lacing) = self.data.get(table..table + count) else { return false };
            let size: usize = lacing.iter().map(|&l| usize::from(l)).sum();
            let body = table + count;
            if body + size > self.data.len() {
                return false;
            }
            self.offset = body + size;
            if *self.serial.get_or_insert(serial) != serial {
                continue;
            }
            self.segments = lacing.to_vec();
            self.body = body;
            self.segment = 0;
            return true;
        }
    }

    pub fn next_packet(&mut self) -> Option<Vec<u8>> {
        loop {
            while self.segment < self.segments.len() {
                let length = usize::from(self.segments[self.segment]);
                self.partial.extend_from_slice(&self.data[self.body..self.body + length]);
                self.body += length;
                self.segment += 1;
                // A lacing value under 255 ends the packet.
                if length < 255 {
                    return Some(std::mem::take(&mut self.partial));
                }
            }
            if !self.next_page() {
                return None;
            }
        }
    }
}

struct OggOpus {
    packets: OggPackets,
    decoder: OpusDecoder,
    /// Samples (per channel) to drop at the start, from the header.
    pre_skip: usize,
    decoded: Vec<f32>,
}

impl OggOpus {
    fn new(data: Vec<u8>) -> Result<Self, String> {
        let mut packets = OggPackets::new(data);
        let head = packets.next_packet().filter(|p| p.starts_with(b"OpusHead") && p.len() >= 19).ok_or("not an Opus file")?;
        let pre_skip = usize::from(u16::from_le_bytes([head[10], head[11]]));
        let _tags = packets.next_packet();
        // Always decoded to stereo; libopus upmixes mono.
        let decoder = OpusDecoder::new(SampleRate::Hz48000, Channels::Stereo).map_err(|e| format!("Opus: {e}"))?;
        Ok(Self { packets, decoder, pre_skip, decoded: vec![0.0; 5760 * CHANNELS] })
    }
}

impl Source for OggOpus {
    fn rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn channels(&self) -> usize {
        CHANNELS
    }

    fn next(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
        let Some(packet) = self.packets.next_packet() else { return Ok(false) };
        if packet.is_empty() {
            return Ok(true);
        }
        let samples = self.decoder.decode_float(Some(packet.as_slice()), self.decoded.as_mut_slice(), false).map_err(|e| format!("Opus: {e}"))?;
        let skip = self.pre_skip.min(samples);
        self.pre_skip -= skip;
        out.extend_from_slice(&self.decoded[skip * CHANNELS..samples * CHANNELS]);
        Ok(true)
    }
}

#[cfg(windows)]
fn open_media_foundation(bytes: &[u8], extension: &str) -> Result<Box<dyn Source>, String> {
    Ok(Box::new(platform::Reader::open(bytes, extension)?))
}

#[cfg(not(windows))]
fn open_media_foundation(_bytes: &[u8], _extension: &str) -> Result<Box<dyn Source>, String> {
    Err("only voice messages can be played on this system".to_owned())
}

#[cfg(windows)]
mod platform {
    use std::path::PathBuf;

    use windows::core::HSTRING;
    use windows::Win32::Media::MediaFoundation::*;

    use super::Source;

    /// Media Foundation's source reader on a temporary copy of the file.
    pub struct Reader {
        reader: IMFSourceReader,
        rate: u32,
        channels: usize,
        path: PathBuf,
    }

    impl Drop for Reader {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    const AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

    impl Reader {
        pub fn open(bytes: &[u8], extension: &str) -> Result<Self, String> {
            crate::voice::decoder::startup()?;
            // The extension helps Media Foundation pick a parser.
            let extension: String = extension.chars().filter(char::is_ascii_alphanumeric).take(5).collect();
            let path = std::env::temp_dir().join(format!("ferrochat-play-{}.{extension}", std::process::id()));
            std::fs::write(&path, bytes).map_err(|e| format!("could not stage the file: {e}"))?;
            let failed = |e: windows::core::Error| format!("can't play this file: {}", e.message());
            unsafe {
                let reader = match MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None) {
                    Ok(reader) => reader,
                    Err(e) => {
                        let _ = std::fs::remove_file(&path);
                        return Err(failed(e));
                    }
                };
                let mut this = Self { reader, rate: 0, channels: 0, path };
                let wanted = MFCreateMediaType().map_err(failed)?;
                wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(failed)?;
                wanted.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float).map_err(failed)?;
                this.reader.SetCurrentMediaType(AUDIO, None, &wanted).map_err(failed)?;
                let actual = this.reader.GetCurrentMediaType(AUDIO).map_err(failed)?;
                this.rate = actual.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).map_err(failed)?;
                this.channels = actual.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).map_err(failed)? as usize;
                if this.rate == 0 || this.channels == 0 {
                    return Err("can't play this file: no audio".to_owned());
                }
                Ok(this)
            }
        }
    }

    impl Source for Reader {
        fn rate(&self) -> u32 {
            self.rate
        }

        fn channels(&self) -> usize {
            self.channels
        }

        fn next(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
            let failed = |e: windows::core::Error| format!("playback stopped: {}", e.message());
            unsafe {
                let mut flags = 0u32;
                let mut sample: Option<IMFSample> = None;
                self.reader.ReadSample(AUDIO, 0, None, Some(&mut flags), None, Some(&mut sample)).map_err(failed)?;
                if let Some(sample) = sample {
                    let buffer = sample.ConvertToContiguousBuffer().map_err(failed)?;
                    let mut data = std::ptr::null_mut();
                    let mut length = 0u32;
                    buffer.Lock(&mut data, None, Some(&mut length)).map_err(failed)?;
                    let floats = std::slice::from_raw_parts(data.cast::<f32>(), length as usize / 4);
                    out.extend_from_slice(floats);
                    let _ = buffer.Unlock();
                }
                Ok(flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 == 0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps packets in Ogg pages the way an encoder would (one packet per page).
    fn ogg(packets: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for (sequence, packet) in packets.iter().enumerate() {
            let mut lacing = vec![255u8; packet.len() / 255];
            lacing.push((packet.len() % 255) as u8);
            out.extend_from_slice(b"OggS");
            out.extend_from_slice(&[0, if sequence == 0 { 2 } else { 0 }]);
            out.extend_from_slice(&0u64.to_le_bytes());
            out.extend_from_slice(&7u32.to_le_bytes());
            out.extend_from_slice(&(sequence as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes()); // CRC isn't checked
            out.push(lacing.len() as u8);
            out.extend_from_slice(&lacing);
            out.extend_from_slice(packet);
        }
        out
    }

    #[test]
    fn splits_ogg_pages_into_packets() {
        let long = vec![7u8; 600]; // spans 255 + 255 + 90 lacing values
        let data = ogg(&[b"first".to_vec(), long.clone(), b"last".to_vec()]);
        let mut packets = OggPackets::new(data);
        assert_eq!(packets.next_packet().unwrap(), b"first");
        assert_eq!(packets.next_packet().unwrap(), long);
        assert_eq!(packets.next_packet().unwrap(), b"last");
        assert!(packets.next_packet().is_none());
    }

    #[test]
    fn decodes_an_ogg_opus_voice_message() {
        use audiopus::coder::Encoder;
        use audiopus::Application;
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 1]); // version, mono
        head.extend_from_slice(&312u16.to_le_bytes()); // pre-skip
        head.extend_from_slice(&48_000u32.to_le_bytes());
        head.extend_from_slice(&[0, 0, 0]);
        let mut packets = vec![head, b"OpusTags\0\0\0\0\0\0\0\0".to_vec()];
        let encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip).unwrap();
        let tone: Vec<f32> = (0..960).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        for _ in 0..10 {
            let mut packet = vec![0u8; 400];
            let length = encoder.encode_float(&tone, &mut packet).unwrap();
            packet.truncate(length);
            packets.push(packet);
        }
        let mut source = OggOpus::new(ogg(&packets)).unwrap();
        let mut samples = Vec::new();
        while source.next(&mut samples).unwrap() {}
        // 10 frames of 960, less the pre-skip, in stereo.
        assert_eq!(samples.len(), (10 * 960 - 312) * CHANNELS);
        assert!(samples.iter().any(|s| s.abs() > 0.1), "the tone came through");
    }
}
