//! Ctrl+V for attachments: files copied in Explorer, or an image (a screenshot,
//! "Copy image" in a browser). Text pastes are left to the message box itself.

use std::path::PathBuf;

/// What the clipboard held, ready to attach.
pub enum Pasted {
    Files(Vec<PathBuf>),
    /// A PNG written to Ferrochat's temp folder.
    Image(PathBuf),
}

/// Pasted images live here until sent; older ones are cleared on the next paste.
fn paste_dir() -> PathBuf {
    std::env::temp_dir().join("ferrochat-paste")
}

/// Encodes 8-bit RGBA as a PNG file in the paste folder.
fn save_png(width: u32, height: u32, rgba: Vec<u8>) -> Result<PathBuf, String> {
    let dir = paste_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    // Pastes from earlier runs (or sent long ago) aren't needed any more.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let day = std::time::Duration::from_secs(24 * 60 * 60);
        for entry in entries.flatten() {
            let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age > day));
            if old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S%.3f");
    let path = dir.join(format!("image_{stamp}.png"));
    let image = image::RgbaImage::from_raw(width, height, rgba).ok_or("the image was cut short")?;
    image.save_with_format(&path, image::ImageFormat::Png).map_err(|e| format!("could not save the image: {e}"))?;
    Ok(path)
}

/// A device-independent bitmap (CF_DIB: header, optional masks or palette,
/// pixels) as RGBA. Handles 24- and 32-bit pictures, which is what screenshots
/// and browsers put on the clipboard.
pub fn dib_to_rgba(dib: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let u32_at = |at: usize| dib.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let u16_at = |at: usize| dib.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let header = u32_at(0).ok_or("empty bitmap")? as usize;
    let width = u32_at(4).ok_or("short bitmap")? as i32;
    let height = u32_at(8).ok_or("short bitmap")? as i32;
    let bits = u16_at(14).ok_or("short bitmap")?;
    let compression = u32_at(16).ok_or("short bitmap")?;
    let colours = u32_at(32).unwrap_or(0) as usize;
    if width <= 0 || height == 0 || width > 16_384 || height.unsigned_abs() > 16_384 {
        return Err("unusual image size".to_owned());
    }
    if !matches!((bits, compression), (24, 0) | (32, 0) | (32, 3)) {
        return Err(format!("unsupported {bits}-bit image"));
    }
    // BI_BITFIELDS with the small header puts three masks after it.
    let masks = if compression == 3 && header == 40 { 12 } else { 0 };
    let start = header + masks + colours * 4;
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    let bytes_per_pixel = usize::from(bits / 8);
    let stride = (w * bytes_per_pixel).div_ceil(4) * 4;
    let pixels = dib.get(start..start + stride * h).ok_or("the image was cut short")?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        // Positive heights are stored bottom row first.
        let source = if height > 0 { h - 1 - row } else { row };
        let line = &pixels[source * stride..source * stride + w * bytes_per_pixel];
        for pixel in line.chunks_exact(bytes_per_pixel) {
            let alpha = if bytes_per_pixel == 4 { pixel[3] } else { 255 };
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], alpha]);
        }
    }
    // Many apps leave the alpha byte at zero; such pictures are meant opaque.
    if bytes_per_pixel == 4 && rgba.chunks_exact(4).all(|p| p[3] == 0) {
        rgba.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    }
    Ok((width as u32, h as u32, rgba))
}

#[cfg(windows)]
pub fn paste() -> Result<Option<Pasted>, String> {
    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

    unsafe {
        // Another app may hold the clipboard for a moment.
        let mut opened = false;
        for _ in 0..5 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !opened {
            return Err("the clipboard is busy".to_owned());
        }
        let result = (|| -> Result<Option<Pasted>, String> {
            if IsClipboardFormatAvailable(u32::from(CF_HDROP.0)).is_ok() {
                let handle: HANDLE = GetClipboardData(u32::from(CF_HDROP.0)).map_err(|e| e.message())?;
                let drop = HDROP(handle.0);
                let count = DragQueryFileW(drop, u32::MAX, None);
                let mut files = Vec::new();
                for index in 0..count {
                    let length = DragQueryFileW(drop, index, None) as usize;
                    let mut name = vec![0u16; length + 1];
                    let written = DragQueryFileW(drop, index, Some(&mut name)) as usize;
                    files.push(PathBuf::from(String::from_utf16_lossy(&name[..written])));
                }
                return Ok((!files.is_empty()).then_some(Pasted::Files(files)));
            }
            // Text copied from Word or Excel comes with a picture of itself;
            // like Discord, text wins and the message box pastes it.
            if IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT.0)).is_ok() {
                return Ok(None);
            }
            if IsClipboardFormatAvailable(u32::from(CF_DIB.0)).is_ok() {
                let handle = GetClipboardData(u32::from(CF_DIB.0)).map_err(|e| e.message())?;
                let global = HGLOBAL(handle.0);
                let pointer = GlobalLock(global);
                if pointer.is_null() {
                    return Err("could not read the image".to_owned());
                }
                let bytes = std::slice::from_raw_parts(pointer.cast::<u8>(), GlobalSize(global)).to_vec();
                let _ = GlobalUnlock(global);
                let (width, height, rgba) = dib_to_rgba(&bytes)?;
                return save_png(width, height, rgba).map(|path| Some(Pasted::Image(path)));
            }
            Ok(None)
        })();
        let _ = CloseClipboard();
        result
    }
}

#[cfg(not(windows))]
pub fn paste() -> Result<Option<Pasted>, String> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(width: i32, height: i32, bits: u16) -> Vec<u8> {
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&width.to_le_bytes());
        dib.extend_from_slice(&height.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&bits.to_le_bytes());
        dib.extend_from_slice(&[0; 24]); // BI_RGB, sizes, resolution, colours
        dib
    }

    #[test]
    fn reads_a_bottom_up_24_bit_bitmap() {
        // 2x2: bottom row blue, red; top row green, white. Rows pad to 8 bytes.
        let mut dib = header(2, 2, 24);
        dib.extend_from_slice(&[255, 0, 0, 0, 0, 255, 0, 0]);
        dib.extend_from_slice(&[0, 255, 0, 255, 255, 255, 0, 0]);
        let (w, h, rgba) = dib_to_rgba(&dib).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(&rgba[0..4], &[0, 255, 0, 255], "top-left is green");
        assert_eq!(&rgba[12..16], &[255, 0, 0, 255], "bottom-right is red");
    }

    #[test]
    fn treats_zero_alpha_as_opaque() {
        let mut dib = header(1, -1, 32);
        dib.extend_from_slice(&[10, 20, 30, 0]);
        let (_, _, rgba) = dib_to_rgba(&dib).unwrap();
        assert_eq!(rgba, vec![30, 20, 10, 255]);
    }

    #[test]
    fn rejects_truncated_bitmaps() {
        let dib = header(100, 100, 32);
        assert!(dib_to_rgba(&dib).is_err());
    }
}
