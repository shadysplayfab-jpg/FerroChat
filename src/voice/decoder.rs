//! H.264 decoding with Windows' built-in Media Foundation decoder (hardware
//! where the GPU offers it), and NV12 to RGBA scaled down to tile size.

/// A decoded picture, already scaled for display.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// NV12 (a Y plane, then interleaved U/V at half size) to RGBA, scaled to fit
/// `max` with nearest-neighbour sampling. `rows` is how many rows the Y plane
/// occupies in the buffer (decoders pad it, e.g. 1088 for 1080).
pub fn nv12_to_rgba(data: &[u8], stride: usize, rows: usize, width: u32, height: u32, max: (u32, u32)) -> Option<Picture> {
    if width == 0 || height == 0 || stride < width as usize || rows < height as usize {
        return None;
    }
    let chroma_start = stride * rows;
    if data.len() < chroma_start + stride * (height as usize).div_ceil(2) {
        return None;
    }
    let scale = (max.0 as f32 / width as f32).min(max.1 as f32 / height as f32).min(1.0);
    let out_w = ((width as f32 * scale) as u32).max(1);
    let out_h = ((height as f32 * scale) as u32).max(1);
    let mut rgba = vec![0u8; (out_w * out_h * 4) as usize];
    // Source column per output column, worked out once rather than per pixel.
    let columns: Vec<usize> = (0..out_w).map(|ox| (u64::from(ox) * u64::from(width) / u64::from(out_w)) as usize).collect();
    let row_bytes = out_w as usize * 4;
    let convert_rows = |first_row: usize, out: &mut [u8]| {
        for (index, line) in out.chunks_exact_mut(row_bytes).enumerate() {
            let oy = (first_row + index) as u64;
            let sy = (oy * u64::from(height) / u64::from(out_h)) as usize;
            let luma_row = &data[sy * stride..];
            let chroma_row = &data[chroma_start + (sy / 2) * stride..];
            for (pixel, &sx) in line.chunks_exact_mut(4).zip(&columns) {
                // BT.601, limited range: what camera and screen encoders send.
                let c = 298 * (i32::from(luma_row[sx]) - 16) + 128;
                let d = i32::from(chroma_row[sx & !1]) - 128;
                let e = i32::from(chroma_row[(sx & !1) + 1]) - 128;
                let clamp = |v: i32| (v >> 8).clamp(0, 255) as u8;
                pixel[0] = clamp(c + 409 * e);
                pixel[1] = clamp(c - 100 * d - 208 * e);
                pixel[2] = clamp(c + 516 * d);
                pixel[3] = 255;
            }
        }
    };
    // Large pictures are split across cores by rows (a 1080p picture takes
    // ~10 ms on one core, which a 60 fps stream can't afford).
    let threads = if out_w * out_h >= 640 * 360 { CONVERT_THREADS.min(out_h as usize) } else { 1 };
    let rows_per_thread = (out_h as usize).div_ceil(threads);
    std::thread::scope(|scope| {
        for (part, chunk) in rgba.chunks_mut(rows_per_thread * row_bytes).enumerate() {
            let convert_rows = &convert_rows;
            if part + 1 == threads || threads == 1 {
                convert_rows(part * rows_per_thread, chunk);
            } else {
                scope.spawn(move || convert_rows(part * rows_per_thread, chunk));
            }
        }
    });
    Some(Picture { width: out_w, height: out_h, rgba })
}

/// Threads a large picture's conversion is split across.
const CONVERT_THREADS: usize = 4;

#[cfg(windows)]
pub use platform::{startup, Decoder};

#[cfg(windows)]
mod platform {
    use std::mem::ManuallyDrop;

    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

    use windows::Win32::Graphics::Direct3D11::ID3D11Device;

    use super::{nv12_to_rgba, Picture};

    /// One frame's duration in 100 ns units (decoders want timestamps; 30 fps).
    const FRAME_TIME: i64 = 333_333;

    /// COM and Media Foundation for the calling (video) thread.
    pub fn startup() -> Result<(), String> {
        use windows::Win32::System::LibraryLoader::LoadLibraryW;
        unsafe {
            // Media Foundation is delay-loaded (build.rs); on Windows "N" it's
            // missing, and calling into it would crash instead of failing.
            for dll in [windows::core::w!("mfplat.dll"), windows::core::w!("mfreadwrite.dll")] {
                if LoadLibraryW(dll).is_err() {
                    return Err("Windows Media Foundation is missing (Windows N: install the Media Feature Pack)".to_owned());
                }
            }
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().map_err(|e| format!("COM: {e}"))?;
            MFStartup(MF_VERSION, MFSTARTUP_LITE).map_err(|e| format!("Media Foundation: {e}"))
        }
    }

    /// One sender's decoder. Not `Send`: it stays on the video thread.
    pub struct Decoder {
        transform: IMFTransform,
        width: u32,
        height: u32,
        stride: usize,
        /// Output sample size when we must allocate samples ourselves.
        sample_size: u32,
        provides_samples: bool,
        time: i64,
        /// The GPU decoding (DXVA) for this decoder, kept alive while it's used.
        gpu: Option<(ID3D11Device, IMFDXGIDeviceManager)>,
    }

    /// Hands the decoder a Direct3D 11 device so the GPU's video engine does
    /// the work. On one core, 1440p60 took ~14.5 ms a frame in software (too
    /// slow for 60 fps); low-latency mode is what keeps it on one core, and
    /// turning that off held back 17 frames (~0.3 s).
    unsafe fn attach_gpu(transform: &IMFTransform) -> windows::core::Result<(ID3D11Device, IMFDXGIDeviceManager)> {
        use windows::core::{Error, Interface};
        use windows::Win32::Foundation::HMODULE;
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
        use windows::Win32::Graphics::Direct3D11::*;

        if transform.GetAttributes()?.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
            return Err(Error::empty());
        }
        let mut device = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
        let device = device.ok_or_else(Error::empty)?;
        // Media Foundation uses the device from its own threads too.
        if let Ok(multithread) = device.cast::<ID3D11Multithread>() {
            let _ = multithread.SetMultithreadProtected(true);
        }
        let (mut token, mut manager) = (0u32, None);
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.ok_or_else(Error::empty)?;
        manager.ResetDevice(&device, token)?;
        transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
        Ok((device, manager))
    }

    impl Decoder {
        pub fn new() -> Result<Self, String> {
            let failed = |what: &'static str| move |e: windows::core::Error| format!("H.264 decoder ({what}): {e}");
            unsafe {
                let transform: IMFTransform =
                    CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER).map_err(failed("create"))?;
                // Without this the decoder holds several frames back.
                if let Ok(attributes) = transform.GetAttributes() {
                    let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
                }
                // The GPU when it can (FERROCHAT_SOFTWARE_DECODE forces the CPU, for comparing).
                let gpu = match std::env::var_os("FERROCHAT_SOFTWARE_DECODE") {
                    Some(_) => None,
                    None => attach_gpu(&transform).ok(),
                };
                let input = MFCreateMediaType().map_err(failed("input type"))?;
                input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(failed("input type"))?;
                input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264).map_err(failed("input type"))?;
                transform.SetInputType(0, &input, 0).map_err(failed("input type"))?;
                let mut decoder =
                    Self { transform, width: 0, height: 0, stride: 0, sample_size: 0, provides_samples: false, time: 0, gpu };
                // The real size arrives with the first frame (a stream change).
                let _ = decoder.choose_output();
                decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(failed("start"))?;
                decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(failed("start"))?;
                Ok(decoder)
            }
        }

        /// Picks NV12 output and reads the picture size.
        unsafe fn choose_output(&mut self) -> windows::core::Result<()> {
            let mut index = 0;
            loop {
                let candidate = self.transform.GetOutputAvailableType(0, index)?;
                index += 1;
                if candidate.GetGUID(&MF_MT_SUBTYPE)? != MFVideoFormat_NV12 {
                    continue;
                }
                self.transform.SetOutputType(0, &candidate, 0)?;
                let size = candidate.GetUINT64(&MF_MT_FRAME_SIZE)?;
                (self.width, self.height) = ((size >> 32) as u32, size as u32);
                self.stride = candidate.GetUINT32(&MF_MT_DEFAULT_STRIDE).map_or(self.width as usize, |s| s as i32 as usize);
                // The visible area, without the decoder's padding rows.
                let mut area = [0u8; 16];
                if candidate.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut area, None).is_ok() {
                    let cx = i32::from_le_bytes(area[8..12].try_into().unwrap_or_default());
                    let cy = i32::from_le_bytes(area[12..16].try_into().unwrap_or_default());
                    if cx > 0 && cy > 0 && cx as u32 <= self.width && cy as u32 <= self.height {
                        (self.width, self.height) = (cx as u32, cy as u32);
                    }
                }
                let info = self.transform.GetOutputStreamInfo(0)?;
                self.sample_size = info.cbSize;
                let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
                self.provides_samples = info.dwFlags & provides != 0;
                return Ok(());
            }
        }

        /// Decodes one Annex B frame. `show` converts the output (frames that
        /// won't be displayed are still decoded, since later frames need them).
        pub fn decode(&mut self, frame: &[u8], show: bool, max: (u32, u32)) -> Result<Vec<Picture>, String> {
            let failed = |e: windows::core::Error| format!("H.264 decode: {e}");
            unsafe {
                let sample = self.input_sample(frame).map_err(failed)?;
                let mut pictures = Vec::new();
                if let Err(error) = self.transform.ProcessInput(0, &sample, 0) {
                    if error.code() != MF_E_NOTACCEPTING {
                        return Err(failed(error));
                    }
                    // Full: take what's ready, then it accepts the frame.
                    pictures.extend(self.drain(show, max).map_err(failed)?);
                    self.transform.ProcessInput(0, &sample, 0).map_err(failed)?;
                }
                pictures.extend(self.drain(show, max).map_err(failed)?);
                Ok(pictures)
            }
        }

        unsafe fn input_sample(&mut self, frame: &[u8]) -> windows::core::Result<IMFSample> {
            let buffer = MFCreateMemoryBuffer(frame.len() as u32)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(frame.as_ptr(), data, frame.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(frame.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(self.time)?;
            sample.SetSampleDuration(FRAME_TIME)?;
            self.time += FRAME_TIME;
            Ok(sample)
        }

        unsafe fn drain(&mut self, show: bool, max: (u32, u32)) -> windows::core::Result<Vec<Picture>> {
            let mut pictures = Vec::new();
            loop {
                let ours = if self.provides_samples {
                    None
                } else {
                    let size = self.sample_size.max(self.width * self.height * 3 / 2).max(1);
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(size)?)?;
                    Some(sample)
                };
                let mut output = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(ours),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0;
                let result = self.transform.ProcessOutput(0, &mut output, &mut status);
                let sample = ManuallyDrop::take(&mut output[0].pSample);
                drop(ManuallyDrop::take(&mut output[0].pEvents));
                match result {
                    Ok(()) => {
                        if let (true, Some(sample)) = (show, sample) {
                            pictures.extend(self.picture(&sample, max)?);
                        }
                    }
                    Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(pictures),
                    Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.choose_output()?,
                    Err(error) => return Err(error),
                }
            }
        }

        /// Whether the GPU is decoding.
        pub fn is_hardware(&self) -> bool {
            self.gpu.is_some()
        }

        unsafe fn picture(&self, sample: &IMFSample, max: (u32, u32)) -> windows::core::Result<Option<Picture>> {
            use windows::core::Interface;
            use windows::Win32::Graphics::Direct3D11::{ID3D11Texture2D, D3D11_TEXTURE2D_DESC};
            // A GPU picture: read it through its 2D view, which copies it to memory
            // with the texture's own row pitch and height (its padding included).
            if self.gpu.is_some() {
                let buffer = sample.GetBufferByIndex(0)?;
                if let (Ok(surface), Ok(two_d)) = (buffer.cast::<IMFDXGIBuffer>(), buffer.cast::<IMF2DBuffer>()) {
                    let mut texture: Option<ID3D11Texture2D> = None;
                    surface.GetResource(&ID3D11Texture2D::IID, &mut texture as *mut _ as *mut _)?;
                    let mut desc = D3D11_TEXTURE2D_DESC::default();
                    texture.ok_or_else(windows::core::Error::empty)?.GetDesc(&mut desc);
                    let (mut scanline, mut pitch) = (std::ptr::null_mut(), 0i32);
                    two_d.Lock2D(&mut scanline, &mut pitch)?;
                    let picture = (pitch > 0).then(|| {
                        let rows = desc.Height as usize;
                        let bytes = std::slice::from_raw_parts(scanline, pitch as usize * rows * 3 / 2);
                        nv12_to_rgba(bytes, pitch as usize, rows, self.width, self.height, max)
                    });
                    two_d.Unlock2D()?;
                    return Ok(picture.flatten());
                }
            }
            let buffer = sample.ConvertToContiguousBuffer()?;
            let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut data, None, Some(&mut length))?;
            let bytes = std::slice::from_raw_parts(data, length as usize);
            // NV12 is 1.5 bytes per pixel, so the padded row count follows from the length.
            let stride = self.stride.max(self.width as usize).max(1);
            let rows = (length as usize / stride) * 2 / 3;
            let picture = nv12_to_rgba(bytes, stride, rows, self.width, self.height, max);
            buffer.Unlock()?;
            Ok(picture)
        }
    }

    impl Drop for Decoder {
        fn drop(&mut self) {
            unsafe {
                let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Picture;

    pub fn startup() -> Result<(), String> {
        Err("video decoding is only available on Windows".to_owned())
    }

    pub struct Decoder;

    impl Decoder {
        pub fn new() -> Result<Self, String> {
            Err("video decoding is only available on Windows".to_owned())
        }

        pub fn decode(&mut self, _frame: &[u8], _show: bool, _max: (u32, u32)) -> Result<Vec<Picture>, String> {
            Ok(Vec::new())
        }

        pub fn is_hardware(&self) -> bool {
            false
        }
    }
}

#[cfg(not(windows))]
pub use platform::{startup, Decoder};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_colours_and_scaling() {
        // 4×2 picture, stride 4, Y plane 2 rows: left half white, right half black.
        let mut data = vec![235, 235, 16, 16, 235, 235, 16, 16];
        data.extend_from_slice(&[128, 128, 128, 128]); // neutral chroma (one row for two)
        let picture = nv12_to_rgba(&data, 4, 2, 4, 2, (4, 2)).unwrap();
        assert_eq!((picture.width, picture.height), (4, 2));
        assert_eq!(&picture.rgba[..4], &[255, 255, 255, 255]);
        assert_eq!(&picture.rgba[12..16], &[0, 0, 0, 255]);
        let small = nv12_to_rgba(&data, 4, 2, 4, 2, (2, 2)).unwrap();
        assert_eq!((small.width, small.height), (2, 1), "keeps the aspect ratio");
        assert!(nv12_to_rgba(&data[..9], 4, 2, 4, 2, (4, 2)).is_none(), "short buffer");
    }

    /// Needs Windows' H.264 decoder; garbage input must fail cleanly, not crash.
    #[cfg(windows)]
    #[test]
    fn media_foundation_decoder_starts() {
        startup().unwrap();
        let mut decoder = Decoder::new().unwrap();
        let _ = decoder.decode(&[0, 0, 0, 1, 0x65, 0xFF, 0x00, 0x12], true, (320, 180));
    }

    /// Encodes `count` NV12 pictures from `picture(index)` with Windows' H.264 encoder.
    #[cfg(windows)]
    fn windows_encode(width: u32, height: u32, bitrate: u32, count: i64, picture: impl Fn(i64) -> Vec<u8>) -> Vec<Vec<u8>> {
        use std::mem::ManuallyDrop;
        use windows::Win32::Media::MediaFoundation::*;
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

        startup().unwrap();
        let mut stream = Vec::new();
        unsafe {
            let encoder: IMFTransform = CoCreateInstance(&CMSH264EncoderMFT, None, CLSCTX_INPROC_SERVER).unwrap();
            let media_type = |subtype: &windows::core::GUID| {
                let t = MFCreateMediaType().unwrap();
                t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).unwrap();
                t.SetGUID(&MF_MT_SUBTYPE, subtype).unwrap();
                t.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(width) << 32) | u64::from(height)).unwrap();
                t.SetUINT64(&MF_MT_FRAME_RATE, (30 << 32) | 1).unwrap();
                t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).unwrap();
                t
            };
            let output = media_type(&MFVideoFormat_H264);
            output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).unwrap();
            encoder.SetOutputType(0, &output, 0).unwrap();
            encoder.SetInputType(0, &media_type(&MFVideoFormat_NV12), 0).unwrap();
            encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).unwrap();
            encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).unwrap();
            let info = encoder.GetOutputStreamInfo(0).unwrap();
            let provides = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
            for frame in 0..count {
                let nv12 = picture(frame);
                let buffer = MFCreateMemoryBuffer(nv12.len() as u32).unwrap();
                let mut data = std::ptr::null_mut();
                buffer.Lock(&mut data, None, None).unwrap();
                std::ptr::copy_nonoverlapping(nv12.as_ptr(), data, nv12.len());
                buffer.Unlock().unwrap();
                buffer.SetCurrentLength(nv12.len() as u32).unwrap();
                let sample = MFCreateSample().unwrap();
                sample.AddBuffer(&buffer).unwrap();
                sample.SetSampleTime(frame * 333_333).unwrap();
                sample.SetSampleDuration(333_333).unwrap();
                encoder.ProcessInput(0, &sample, 0).unwrap();
                if frame == count - 1 {
                    encoder.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0).unwrap();
                }
                loop {
                    let ours = (!provides).then(|| {
                        let s = MFCreateSample().unwrap();
                        s.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(1 << 16)).unwrap()).unwrap();
                        s
                    });
                    let mut out = [MFT_OUTPUT_DATA_BUFFER {
                        dwStreamID: 0,
                        pSample: ManuallyDrop::new(ours),
                        dwStatus: 0,
                        pEvents: ManuallyDrop::new(None),
                    }];
                    let mut status = 0;
                    let result = encoder.ProcessOutput(0, &mut out, &mut status);
                    let sample = ManuallyDrop::take(&mut out[0].pSample);
                    drop(ManuallyDrop::take(&mut out[0].pEvents));
                    if result.is_err() {
                        break; // needs more input
                    }
                    let buffer = sample.unwrap().ConvertToContiguousBuffer().unwrap();
                    let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
                    buffer.Lock(&mut data, None, Some(&mut length)).unwrap();
                    stream.push(std::slice::from_raw_parts(data, length as usize).to_vec());
                    buffer.Unlock().unwrap();
                }
            }
        }
        stream
    }

    /// How long decoding and converting take per frame for a 1440p stream
    /// (`cargo test --release -- --ignored --nocapture decode_speed`).
    #[cfg(windows)]
    #[test]
    #[ignore = "slow benchmark"]
    fn decode_speed() {
        const W: u32 = 2560;
        const H: u32 = 1440;
        // Moving stripes, so every frame has real changes to decode.
        let stream = windows_encode(W, H, 8_000_000, 60, |frame| {
            let mut nv12: Vec<u8> = (0..W * H).map(|i| (((i % W) as i64 + frame * 8) % 220 + 16) as u8).collect();
            nv12.extend((0..W * H / 2).map(|i| (128 + (i % 64) as i64 - 32) as u8));
            nv12
        });
        let mut decoder = Decoder::new().unwrap();
        println!("GPU decoding: {}", decoder.is_hardware());
        let started = std::time::Instant::now();
        let mut shown = 0;
        let mut first_out = None;
        for (index, frame) in stream.iter().enumerate() {
            shown += decoder.decode(frame, true, (1920, 1080)).unwrap().len();
            if shown > 0 && first_out.is_none() {
                first_out = Some(index);
            }
        }
        // Frames held back: how many went in before the first came out.
        println!("held back: {first_out:?} frames");
        let per_frame = started.elapsed().as_secs_f64() * 1000.0 / stream.len() as f64;
        println!("decode+convert 2560x1440 -> 1920x1080: {per_frame:.1} ms per frame ({shown} pictures of {})", stream.len());
        let mut decoder = Decoder::new().unwrap();
        let started = std::time::Instant::now();
        for frame in &stream {
            decoder.decode(frame, false, (1920, 1080)).unwrap();
        }
        let per_frame = started.elapsed().as_secs_f64() * 1000.0 / stream.len() as f64;
        println!("decode only: {per_frame:.1} ms per frame");
    }

    /// Encodes a solid orange picture with Windows' H.264 encoder, then decodes
    /// it with ours: the picture must come back at its size and colour.
    #[cfg(windows)]
    #[test]
    fn decodes_what_windows_encodes() {
        const W: u32 = 128;
        const H: u32 = 96;
        // Orange in BT.601 limited range: Y 151, U 44, V 194 (RGB ≈ 255, 128, 0).
        let mut nv12 = vec![151u8; (W * H) as usize];
        for _ in 0..(W * H / 4) {
            nv12.extend_from_slice(&[44, 194]);
        }
        let stream = windows_encode(W, H, 500_000, 10, |_| nv12.clone());
        assert!(!stream.is_empty(), "the encoder produced frames");
        assert!(crate::voice::h264::is_keyframe(&stream[0]), "the first frame is a keyframe");

        // Through RTP the way a WebRTC sender packetizes: small NAL units whole,
        // big ones as FU-A fragments; the marker ends each frame.
        let mut depacketizer = crate::voice::h264::Depacketizer::default();
        let mut sequence = 0u16;
        let mut received = Vec::new();
        for (index, frame) in stream.iter().enumerate() {
            let packets = crate::voice::h264::packetize(frame, 1000);
            for (p, packet) in packets.iter().enumerate() {
                sequence = sequence.wrapping_add(1);
                let last = p == packets.len() - 1;
                if let Some(frame) = depacketizer.push(sequence, index as u32 * 3000, last, packet) {
                    received.push(frame);
                }
            }
        }
        assert_eq!(received.len(), stream.len(), "every frame survives RTP");
        assert_eq!(depacketizer.losses, 0);

        let mut decoder = Decoder::new().unwrap();
        let mut pictures = Vec::new();
        for frame in &received {
            pictures.extend(decoder.decode(frame, true, (640, 360)).unwrap());
        }
        let picture = pictures.last().expect("the decoder produced pictures");
        assert_eq!((picture.width, picture.height), (W, H));
        let centre = ((H / 2 * W + W / 2) * 4) as usize;
        let [r, g, b, _] = picture.rgba[centre..centre + 4] else { unreachable!() };
        assert!(r > 220 && (100..160).contains(&g) && b < 40, "orange came back as {r},{g},{b}");
    }
}
