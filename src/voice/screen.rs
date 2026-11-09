//! Go Live: captures the primary screen with GDI, scales it to the stream size,
//! and encodes H.264 with Windows' built-in encoder. Runs on its own thread
//! only while you share; frames go to the stream connection, which encrypts,
//! packetizes and sends them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use iced::futures::channel::mpsc;

/// Bits per pixel per frame for the bitrate (screen content compresses well).
const BITS_PER_PIXEL: f64 = 0.12;
const MIN_BITRATE: u32 = 1_500_000;
const MAX_BITRATE: u32 = 8_000_000;

/// What to send: the largest picture (the screen is scaled down to fit) and fps.
#[derive(Debug, Clone, Copy)]
pub struct Quality {
    pub max: (u32, u32),
    pub fps: u32,
}

/// The encoder's target for a picture size and frame rate.
pub fn bitrate(size: (u32, u32), fps: u32) -> u32 {
    let bits = f64::from(size.0) * f64::from(size.1) * f64::from(fps) * BITS_PER_PIXEL;
    (bits as u32).clamp(MIN_BITRATE, MAX_BITRATE)
}

/// A keyframe at least this often, so new viewers see the picture quickly.
const KEYFRAME_SECONDS: u32 = 2;
/// RTP video clock.
const CLOCK_RATE: u64 = 90_000;

/// Your own share, shown back to you on the stage: smaller and slower than
/// what's sent, and only while the stage is on screen.
pub struct Preview {
    pub out: mpsc::Sender<super::VoiceEvent>,
    pub controls: Arc<super::Controls>,
}

/// The preview's largest size and rate.
const PREVIEW_SIZE: (u32, u32) = (960, 540);
const PREVIEW_FPS: u32 = 15;
/// The user id preview pictures are sent under.
pub const OWN_PREVIEW: &str = "own-screen";

/// One encoded frame (Annex B) and its RTP timestamp.
pub struct Encoded {
    pub data: Vec<u8>,
    pub timestamp: u32,
}

/// The capture thread. Dropping it stops capturing.
pub struct Share {
    stop: Arc<AtomicBool>,
    keyframe: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub size: (u32, u32),
    pub fps: u32,
    pub bitrate: u32,
}

impl Share {
    /// A viewer lost the picture (RTCP PLI): send a keyframe next.
    pub fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts capturing. Frames that the connection can't take in time are dropped.
pub fn start(frames: mpsc::Sender<Encoded>, quality: Quality, preview: Option<Preview>) -> Result<Share, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let keyframe = Arc::new(AtomicBool::new(false));
    let (ready, opened) = std::sync::mpsc::channel();
    let (stop_flag, keyframe_flag) = (stop.clone(), keyframe.clone());
    let fps = quality.fps.clamp(1, 60);
    let thread = std::thread::Builder::new()
        .name("ferrochat-screen".to_owned())
        .spawn(move || platform::run(frames, quality.max, fps, preview, &stop_flag, &keyframe_flag, ready))
        .map_err(|e| format!("no screen thread: {e}"))?;
    let size = opened.recv().map_err(|_| "the screen thread stopped".to_owned())??;
    Ok(Share { stop, keyframe, thread: Some(thread), size, fps, bitrate: bitrate(size, fps) })
}

/// Scales `screen` down to fit `max`, keeping its shape, with even sides (H.264 needs them).
pub fn fit(screen: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let scale = (max.0 as f32 / screen.0.max(1) as f32).min(max.1 as f32 / screen.1.max(1) as f32).min(1.0);
    let even = |v: f32| ((v as u32) & !1).max(2);
    (even(screen.0 as f32 * scale), even(screen.1 as f32 * scale))
}

/// Sends a small RGBA copy of the shared picture to the UI, when it's wanted and due.
fn send_preview(preview: &mut Preview, bgra: &[u8], size: (u32, u32), last: &mut Option<Instant>, scratch: &mut Vec<u8>) {
    let due = last.is_none_or(|at| at.elapsed() >= Duration::from_secs(1) / PREVIEW_FPS);
    if !due || !preview.controls.own_preview() {
        return;
    }
    *last = Some(Instant::now());
    let small = fit(size, PREVIEW_SIZE);
    scale_bgra(bgra, (size.0 as usize, size.1 as usize), size.0 as usize * 4, (small.0 as usize, small.1 as usize), scratch);
    let mut rgba = std::mem::take(scratch);
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let frame = Arc::new(super::video::VideoFrame { width: small.0, height: small.1, rgba });
    // A busy UI just misses this one.
    let _ = preview.out.try_send(super::VoiceEvent::Video { user_id: OWN_PREVIEW.to_owned(), frame });
}

/// BGRA (top row first) to NV12 in BT.601 limited range, what decoders expect.
pub fn bgra_to_nv12(bgra: &[u8], width: usize, height: usize, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(width * height * 3 / 2);
    for pixel in bgra.chunks_exact(4).take(width * height) {
        let (b, g, r) = (i32::from(pixel[0]), i32::from(pixel[1]), i32::from(pixel[2]));
        out.push((((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8);
    }
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            // The average of each 2×2 block.
            let (mut r, mut g, mut b) = (0, 0, 0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let i = ((y + dy).min(height - 1) * width + (x + dx).min(width - 1)) * 4;
                b += i32::from(bgra[i]);
                g += i32::from(bgra[i + 1]);
                r += i32::from(bgra[i + 2]);
            }
            let (r, g, b) = (r / 4, g / 4, b / 4);
            out.push((((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8);
            out.push((((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8);
        }
    }
}

/// Scales BGRA (rows `pitch` bytes apart) into `out`, averaging each output
/// pixel's whole source area, so text stays readable when shrunk.
pub fn scale_bgra(source: &[u8], (width, height): (usize, usize), pitch: usize, (out_w, out_h): (usize, usize), out: &mut Vec<u8>) {
    out.clear();
    out.reserve(out_w * out_h * 4);
    if (out_w, out_h) == (width, height) {
        // Same size: copy the rows.
        for row in 0..height {
            out.extend_from_slice(&source[row * pitch..row * pitch + width * 4]);
        }
        return;
    }
    let span = |o: usize, out_len: usize, len: usize| (o * len / out_len, ((o + 1) * len / out_len).max(o * len / out_len + 1).min(len));
    let columns: Vec<(usize, usize)> = (0..out_w).map(|x| span(x, out_w, width)).collect();
    for y in 0..out_h {
        let (top, bottom) = span(y, out_h, height);
        for &(left, right) in &columns {
            let mut sum = [0u32; 3];
            for row in top..bottom {
                let line = &source[row * pitch + left * 4..row * pitch + right * 4];
                for pixel in line.chunks_exact(4) {
                    sum[0] += u32::from(pixel[0]);
                    sum[1] += u32::from(pixel[1]);
                    sum[2] += u32::from(pixel[2]);
                }
            }
            let count = ((bottom - top) * (right - left)) as u32;
            out.extend_from_slice(&[(sum[0] / count) as u8, (sum[1] / count) as u8, (sum[2] / count) as u8, 255]);
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::mem::ManuallyDrop;

    use windows::core::Interface;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    use super::*;

    pub fn run(
        mut frames: mpsc::Sender<Encoded>,
        max: (u32, u32),
        fps: u32,
        mut preview: Option<Preview>,
        stop: &AtomicBool,
        keyframe: &AtomicBool,
        ready: std::sync::mpsc::Sender<Result<(u32, u32), String>>,
    ) {
        let (mut last_preview, mut preview_scratch) = (None, Vec::new());
        let setup = (|| -> Result<(Grabber, Encoder), String> {
            crate::voice::decoder::startup()?;
            let capture = Grabber::new(max)?;
            let (width, height) = capture.size();
            let encoder = Encoder::new(width, height, fps)?;
            Ok((capture, encoder))
        })();
        let (mut capture, mut encoder) = match setup {
            Ok(parts) => parts,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        let size = capture.size();
        let (width, height) = (size.0 as usize, size.1 as usize);
        let _ = ready.send(Ok(size));
        let interval = Duration::from_secs(1) / fps;
        let started = Instant::now();
        let mut next = Instant::now();
        let mut nv12 = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            if now < next {
                std::thread::sleep(next - now);
                continue;
            }
            next += interval;
            // Fell far behind (the machine was busy): don't try to catch up.
            if next < now {
                next = now + interval;
            }
            let Some(bgra) = capture.grab() else { continue };
            if let Some(preview) = preview.as_mut() {
                send_preview(preview, bgra, size, &mut last_preview, &mut preview_scratch);
            }
            bgra_to_nv12(bgra, width, height, &mut nv12);
            let elapsed = started.elapsed();
            let force = keyframe.swap(false, Ordering::Relaxed);
            // A failed frame is skipped; the next keyframe recovers viewers.
            let Ok(outputs) = encoder.encode(&nv12, elapsed, force) else { continue };
            let timestamp = (elapsed.as_micros() as u64 * CLOCK_RATE / 1_000_000) as u32;
            for data in outputs {
                // Full: the connection is behind, so this frame is skipped.
                let _ = frames.try_send(Encoded { data, timestamp });
            }
        }
    }

    /// Milliseconds per grab at each size, and which capture ran (for `cargo test -- --ignored --nocapture`).
    #[cfg(test)]
    pub fn time_grabs(max: (u32, u32)) -> (u32, u32, f64, &'static str) {
        crate::voice::decoder::startup().unwrap();
        let mut capture = Grabber::new(max).unwrap();
        let (width, height) = capture.size();
        let mut busy = Duration::ZERO;
        for _ in 0..20 {
            let started = Instant::now();
            capture.grab().unwrap();
            busy += started.elapsed();
            // Let the desktop present new frames between grabs.
            std::thread::sleep(Duration::from_millis(30));
        }
        let ms = busy.as_secs_f64() * 1000.0 / 20.0;
        let kind = if matches!(capture, Grabber::Duplication(_)) { "DXGI" } else { "GDI" };
        (width, height, ms, kind)
    }

    /// DXGI Desktop Duplication when the system allows it (fast: the GPU hands
    /// over the finished desktop), GDI otherwise.
    enum Grabber {
        Duplication(Duplication),
        Gdi(Capture),
    }

    impl Grabber {
        fn new(max: (u32, u32)) -> Result<Self, String> {
            match Duplication::new(max) {
                Ok(duplication) => Ok(Grabber::Duplication(duplication)),
                Err(_) => Capture::new(max).map(Grabber::Gdi),
            }
        }

        fn size(&self) -> (u32, u32) {
            match self {
                Grabber::Duplication(d) => d.size,
                Grabber::Gdi(c) => (c.width, c.height),
            }
        }

        fn grab(&mut self) -> Option<&[u8]> {
            match self {
                Grabber::Duplication(d) => d.grab(),
                Grabber::Gdi(c) => c.grab(),
            }
        }
    }

    /// The primary output through Desktop Duplication, scaled on the CPU.
    struct Duplication {
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        output: IDXGIOutput1,
        duplication: IDXGIOutputDuplication,
        staging: ID3D11Texture2D,
        source: (u32, u32),
        size: (u32, u32),
        /// The last picture, scaled; reused while the desktop doesn't change.
        frame: Vec<u8>,
    }

    impl Duplication {
        fn new(max: (u32, u32)) -> windows::core::Result<Self> {
            unsafe {
                let (mut device, mut context) = (None, None);
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_HARDWARE,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )?;
                let (device, context): (ID3D11Device, ID3D11DeviceContext) =
                    (device.ok_or_else(windows::core::Error::empty)?, context.ok_or_else(windows::core::Error::empty)?);
                let adapter = device.cast::<IDXGIDevice>()?.GetAdapter()?;
                let output: IDXGIOutput1 = adapter.EnumOutputs(0)?.cast()?;
                let duplication = output.DuplicateOutput(&device)?;
                let mode = duplication.GetDesc().ModeDesc;
                let source = (mode.Width, mode.Height);
                let staging = Self::staging(&device, source)?;
                let size = fit(source, max);
                Ok(Self { device, context, output, duplication, staging, source, size, frame: Vec::new() })
            }
        }

        unsafe fn staging(device: &ID3D11Device, (width, height): (u32, u32)) -> windows::core::Result<ID3D11Texture2D> {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut texture = None;
            device.CreateTexture2D(&desc, None, Some(&mut texture))?;
            texture.ok_or_else(windows::core::Error::empty)
        }

        fn grab(&mut self) -> Option<&[u8]> {
            unsafe {
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut resource = None;
                match self.duplication.AcquireNextFrame(0, &mut info, &mut resource) {
                    Ok(()) => {}
                    // Nothing changed on screen: send the last picture again.
                    Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                        return (!self.frame.is_empty()).then_some(self.frame.as_slice());
                    }
                    // A full-screen app or a mode change took the desktop: start over.
                    Err(error) if error.code() == DXGI_ERROR_ACCESS_LOST => {
                        if let Ok(duplication) = self.output.DuplicateOutput(&self.device) {
                            self.duplication = duplication;
                            // A new mode can bring a new desktop size, and a copy between
                            // textures of different sizes does nothing (a frozen stream).
                            // The stream keeps its size; the new desktop is scaled into it.
                            let mode = self.duplication.GetDesc().ModeDesc;
                            let source = (mode.Width, mode.Height);
                            if source != self.source {
                                if let Ok(staging) = Self::staging(&self.device, source) {
                                    (self.staging, self.source) = (staging, source);
                                }
                            }
                        }
                        return (!self.frame.is_empty()).then_some(self.frame.as_slice());
                    }
                    Err(_) => return None,
                }
                let copied = resource.and_then(|r| r.cast::<ID3D11Texture2D>().ok()).map(|texture| {
                    self.context.CopyResource(&self.staging, &texture);
                });
                let _ = self.duplication.ReleaseFrame();
                copied?;
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).ok()?;
                let pitch = mapped.RowPitch as usize;
                let source = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * self.source.1 as usize);
                let (width, height) = (self.source.0 as usize, self.source.1 as usize);
                scale_bgra(source, (width, height), pitch, (self.size.0 as usize, self.size.1 as usize), &mut self.frame);
                self.context.Unmap(&self.staging, 0);
                Some(&self.frame)
            }
        }
    }

    /// The primary screen, scaled into a bitmap we can read.
    struct Capture {
        screen: HDC,
        memory: HDC,
        bitmap: HBITMAP,
        previous: HGDIOBJ,
        bits: *mut u8,
        source: (i32, i32),
        width: u32,
        height: u32,
    }

    impl Capture {
        fn new(max: (u32, u32)) -> Result<Self, String> {
            unsafe {
                let source = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
                if source.0 <= 0 || source.1 <= 0 {
                    return Err("no screen to capture".to_owned());
                }
                let (width, height) = fit((source.0 as u32, source.1 as u32), max);
                let screen = GetDC(None);
                let memory = CreateCompatibleDC(Some(screen));
                let mut info = BITMAPINFO::default();
                info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                info.bmiHeader.biWidth = width as i32;
                info.bmiHeader.biHeight = -(height as i32); // top row first
                info.bmiHeader.biPlanes = 1;
                info.bmiHeader.biBitCount = 32;
                let mut bits = std::ptr::null_mut();
                let bitmap = match CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                    Ok(bitmap) if !bits.is_null() => bitmap,
                    _ => {
                        let _ = DeleteDC(memory);
                        ReleaseDC(None, screen);
                        return Err("could not set up screen capture".to_owned());
                    }
                };
                let previous = SelectObject(memory, bitmap.into());
                // Smooth scaling (text stays readable at 720p).
                SetStretchBltMode(memory, HALFTONE);
                let _ = SetBrushOrgEx(memory, 0, 0, None);
                Ok(Self { screen, memory, bitmap, previous, bits: bits.cast(), source, width, height })
            }
        }

        /// The current screen as BGRA.
        fn grab(&mut self) -> Option<&[u8]> {
            unsafe {
                let (w, h) = (self.width as i32, self.height as i32);
                StretchBlt(self.memory, 0, 0, w, h, Some(self.screen), 0, 0, self.source.0, self.source.1, SRCCOPY).ok().ok()?;
                let _ = GdiFlush();
                Some(std::slice::from_raw_parts(self.bits, (self.width * self.height * 4) as usize))
            }
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.memory, self.previous);
                let _ = DeleteObject(self.bitmap.into());
                let _ = DeleteDC(self.memory);
                ReleaseDC(None, self.screen);
            }
        }
    }

    struct Encoder {
        transform: IMFTransform,
        codec: Option<ICodecAPI>,
        provides_samples: bool,
        output_size: u32,
        frame_bytes: usize,
        fps: u32,
    }

    impl Encoder {
        fn new(width: u32, height: u32, fps: u32) -> Result<Self, String> {
            let failed = |what: &'static str| move |e: windows::core::Error| format!("H.264 encoder ({what}): {}", e.message());
            unsafe {
                let transform: IMFTransform =
                    CoCreateInstance(&CMSH264EncoderMFT, None, CLSCTX_INPROC_SERVER).map_err(failed("create"))?;
                // Without this the encoder holds several frames back.
                if let Ok(attributes) = transform.GetAttributes() {
                    let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
                }
                let media_type = |subtype: &windows::core::GUID| -> windows::core::Result<IMFMediaType> {
                    let t = MFCreateMediaType()?;
                    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                    t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
                    t.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(width) << 32) | u64::from(height))?;
                    t.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)?;
                    t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                    Ok(t)
                };
                let output = media_type(&MFVideoFormat_H264).map_err(failed("output type"))?;
                output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate((width, height), fps)).map_err(failed("output type"))?;
                // Constrained baseline: what every WebRTC H.264 decoder takes.
                output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32).map_err(failed("output type"))?;
                transform.SetOutputType(0, &output, 0).map_err(failed("output type"))?;
                transform.SetInputType(0, &media_type(&MFVideoFormat_NV12).map_err(failed("input type"))?, 0).map_err(failed("input type"))?;
                // Best effort: low latency, steady bitrate, regular keyframes.
                let codec: Option<ICodecAPI> = windows::core::Interface::cast(&transform).ok();
                if let Some(codec) = &codec {
                    let _ = codec.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
                    let _ = codec.SetValue(&CODECAPI_AVEncCommonRateControlMode, &VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32));
                    let _ = codec.SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(fps * KEYFRAME_SECONDS));
                }
                transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(failed("start"))?;
                transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(failed("start"))?;
                let info = transform.GetOutputStreamInfo(0).map_err(failed("start"))?;
                let provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
                let frame_bytes = (width * height * 3 / 2) as usize;
                Ok(Self { transform, codec, provides_samples, output_size: info.cbSize.max(1 << 18), frame_bytes, fps })
            }
        }

        /// Encodes one NV12 picture; returns the frames the encoder had ready.
        fn encode(&mut self, nv12: &[u8], time: Duration, keyframe: bool) -> windows::core::Result<Vec<Vec<u8>>> {
            unsafe {
                if keyframe {
                    if let Some(codec) = &self.codec {
                        let _ = codec.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32));
                    }
                }
                let length = nv12.len().min(self.frame_bytes);
                let buffer = MFCreateMemoryBuffer(length as u32)?;
                let mut data = std::ptr::null_mut();
                buffer.Lock(&mut data, None, None)?;
                std::ptr::copy_nonoverlapping(nv12.as_ptr(), data, length);
                buffer.Unlock()?;
                buffer.SetCurrentLength(length as u32)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                // Media Foundation counts time in 100 ns units.
                sample.SetSampleTime((time.as_nanos() / 100) as i64)?;
                sample.SetSampleDuration(i64::from(10_000_000 / self.fps))?;
                let mut frames = Vec::new();
                if let Err(error) = self.transform.ProcessInput(0, &sample, 0) {
                    if error.code() != MF_E_NOTACCEPTING {
                        return Err(error);
                    }
                    frames.extend(self.drain()?);
                    self.transform.ProcessInput(0, &sample, 0)?;
                }
                frames.extend(self.drain()?);
                Ok(frames)
            }
        }

        unsafe fn drain(&mut self) -> windows::core::Result<Vec<Vec<u8>>> {
            let mut frames = Vec::new();
            loop {
                let ours = if self.provides_samples {
                    None
                } else {
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(self.output_size)?)?;
                    Some(sample)
                };
                let mut output =
                    [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: ManuallyDrop::new(ours), dwStatus: 0, pEvents: ManuallyDrop::new(None) }];
                let mut status = 0;
                let result = self.transform.ProcessOutput(0, &mut output, &mut status);
                let sample = ManuallyDrop::take(&mut output[0].pSample);
                drop(ManuallyDrop::take(&mut output[0].pEvents));
                match result {
                    Ok(()) => {
                        let Some(sample) = sample else { continue };
                        let buffer = sample.ConvertToContiguousBuffer()?;
                        let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
                        buffer.Lock(&mut data, None, Some(&mut length))?;
                        frames.push(std::slice::from_raw_parts(data, length as usize).to_vec());
                        buffer.Unlock()?;
                    }
                    Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(frames),
                    Err(error) => return Err(error),
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub fn run(
        _frames: mpsc::Sender<Encoded>,
        _max: (u32, u32),
        _fps: u32,
        _preview: Option<Preview>,
        _stop: &AtomicBool,
        _keyframe: &AtomicBool,
        ready: std::sync::mpsc::Sender<Result<(u32, u32), String>>,
    ) {
        let _ = ready.send(Err("screen sharing is only available on Windows".to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_the_screen_into_720p() {
        const P720: (u32, u32) = (1280, 720);
        assert_eq!(fit((1920, 1080), P720), (1280, 720));
        assert_eq!(fit((2560, 1440), P720), (1280, 720));
        assert_eq!(fit((1280, 1024), P720), (900, 720), "taller screens keep their shape");
        assert_eq!(fit((1024, 768), P720), (960, 720));
        assert_eq!(fit((801, 601), P720), (800, 600), "never scaled up; sides even");
    }

    #[test]
    fn bitrate_follows_size_and_frame_rate() {
        assert_eq!(bitrate((1280, 720), 15), 1_658_880);
        assert_eq!(bitrate((854, 480), 5), MIN_BITRATE, "tiny streams get a floor");
        assert_eq!(bitrate((1920, 1080), 60), MAX_BITRATE, "and big ones a ceiling");
    }

    #[test]
    fn scaling_averages_areas() {
        // 4×2: a white 2×2 block, then a black one; rows padded to 20 bytes.
        let mut source = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                let v = if x < 2 { 255 } else { 0 };
                source.extend_from_slice(&[v, v, v, 255]);
            }
            source.extend_from_slice(&[9; 4]); // padding, never read
        }
        let mut out = Vec::new();
        scale_bgra(&source, (4, 2), 20, (2, 1), &mut out);
        assert_eq!(out, vec![255, 255, 255, 255, 0, 0, 0, 255]);
        scale_bgra(&source, (4, 2), 20, (1, 1), &mut out);
        assert_eq!(out, vec![127, 127, 127, 255], "the halves average to grey");
        scale_bgra(&source, (4, 2), 20, (4, 2), &mut out);
        assert_eq!(&out[..8], &[255, 255, 255, 255, 255, 255, 255, 255], "same size copies");
    }

    #[test]
    fn converts_colours_to_nv12() {
        // 2×2 white, then 2×2 black, side by side (4×2).
        let mut bgra = Vec::new();
        for x in 0..8 {
            let v = if x % 4 < 2 { 255 } else { 0 };
            bgra.extend_from_slice(&[v, v, v, 255]);
        }
        let mut nv12 = Vec::new();
        bgra_to_nv12(&bgra, 4, 2, &mut nv12);
        assert_eq!(nv12.len(), 4 * 2 * 3 / 2);
        assert_eq!(&nv12[..4], &[235, 235, 16, 16], "luma: white then black");
        assert_eq!(&nv12[8..], &[128, 128, 128, 128], "grey has neutral chroma");
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "captures the real screen"]
    fn grab_timings() {
        for max in [(1280, 720), (1920, 1080), (7680, 4320)] {
            let (w, h, ms, kind) = platform::time_grabs(max);
            println!("grab {kind} {w}x{h}: {ms:.1} ms");
        }
    }

    /// Captures and encodes the real screen for a moment.
    #[cfg(windows)]
    #[test]
    #[ignore = "captures the real screen"]
    fn captures_and_encodes_the_screen() {
        let (sender, mut receiver) = mpsc::channel(8);
        let share = start(sender, Quality { max: (1280, 720), fps: 15 }, None).unwrap();
        std::thread::sleep(Duration::from_secs(3));
        drop(share);
        let mut frames = Vec::new();
        while let Ok(frame) = receiver.try_recv() {
            frames.push(frame);
        }
        assert!(!frames.is_empty(), "frames were encoded");
        assert!(crate::voice::h264::is_keyframe(&frames[0].data), "the stream starts with a keyframe");
    }
}
