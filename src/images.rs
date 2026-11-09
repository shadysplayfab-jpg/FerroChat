//! Bounded cache for remote images: avatars, custom emoji, attachment and embed
//! previews. Downloads are limited in number and size, only come from Discord's
//! media hosts, and are evicted least-recently-wanted when over budget.

use std::collections::{HashMap, VecDeque};
use std::io::Cursor;

use iced::widget::image::Handle;

/// Simultaneous downloads.
const MAX_IN_FLIGHT: usize = 6;
/// Largest image accepted.
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
/// Failed/queued slots kept before old ones are dropped.
const MAX_SLOTS: usize = 2048;
/// Attachment and embed previews are scaled down to fit this box.
const PREVIEW_BOX: (f32, f32) = (400.0, 300.0);

/// Round images (avatars, server icons) are masked once on download, so drawing
/// them costs nothing extra.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    Square,
    Round,
    /// Played if it turns out to be a GIF (only requested with animation on);
    /// anything else is kept as a still, like `Square`/`Round`.
    Animated { round: bool },
}

/// Round images are decoded for masking; refuse anything larger than this.
const MAX_ROUND_SIDE: u32 = 512;
/// Animations larger than these keep only their first frame.
const MAX_FRAMES: usize = 150;
const MAX_ANIMATION_BYTES: usize = 24 * 1024 * 1024;
const MAX_ANIMATION_SIDE: u32 = 640;

#[derive(Clone)]
enum State {
    Queued,
    Loading,
    Ready(Handle),
    /// Frames with how long each shows (ms), and their total.
    Animated(Vec<(Handle, u32)>, u32),
    Failed,
}

/// A downloaded image, decoded off the UI thread by [`prepare`] (avatars are
/// masked round and GIFs split into frames, which takes a while for many at once).
#[derive(Clone)]
pub struct Decoded(Option<(State, usize)>);

impl std::fmt::Debug for Decoded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "Decoded" } else { "Undecodable" })
    }
}

/// Decodes a download for `shape`; run it on a worker thread.
pub fn prepare(bytes: Vec<u8>, shape: Shape) -> Decoded {
    Decoded(decode(bytes, shape))
}

struct Slot {
    state: State,
    shape: Shape,
    /// Compressed bytes plus decoded RGBA size.
    cost: usize,
    /// `generation` of the last `want` that included this image.
    last_wanted: u64,
}

pub struct ImageCache {
    slots: HashMap<String, Slot>,
    queue: VecDeque<String>,
    in_flight: usize,
    used: usize,
    budget: usize,
    generation: u64,
    /// Animation time (ms); animated images show the frame for it.
    clock: u64,
}

impl ImageCache {
    pub fn new(budget: usize) -> Self {
        Self { slots: HashMap::new(), queue: VecDeque::new(), in_flight: 0, used: 0, budget, generation: 0, clock: 0 }
    }

    /// The image, or an animation's frame for the current clock.
    pub fn get(&self, url: &str) -> Option<&Handle> {
        match &self.slots.get(url)?.state {
            State::Ready(handle) => Some(handle),
            State::Animated(frames, total) => {
                let mut at = (self.clock % u64::from((*total).max(1))) as u32;
                for (handle, delay) in frames {
                    if at < *delay {
                        return Some(handle);
                    }
                    at -= delay;
                }
                frames.first().map(|(handle, _)| handle)
            }
            _ => None,
        }
    }

    pub fn set_clock(&mut self, ms: u64) {
        self.clock = ms;
    }

    /// The shortest frame (ms) among animations wanted right now; `None` when
    /// nothing animated is wanted, so frames needn't advance.
    pub fn shortest_frame(&self) -> Option<u32> {
        self.slots
            .values()
            .filter(|s| s.last_wanted == self.generation)
            .filter_map(|s| match &s.state {
                State::Animated(frames, _) => frames.iter().map(|(_, delay)| *delay).min(),
                _ => None,
            })
            .min()
    }

    /// Marks `urls` as currently needed and queues unknown ones. Returns the
    /// downloads to start now. Images from the latest call are never evicted.
    pub fn want(&mut self, urls: impl IntoIterator<Item = (String, Shape)>) -> Vec<String> {
        self.generation += 1;
        for (url, shape) in urls {
            match self.slots.get_mut(&url) {
                Some(slot) => slot.last_wanted = self.generation,
                None => {
                    let slot = Slot { state: State::Queued, shape, cost: 0, last_wanted: self.generation };
                    self.slots.insert(url.clone(), slot);
                    self.queue.push_back(url);
                }
            }
        }
        self.start()
    }

    /// How a queued download should be decoded.
    pub fn shape_of(&self, url: &str) -> Shape {
        self.slots.get(url).map_or(Shape::Square, |slot| slot.shape)
    }

    /// Records a finished (and decoded) download; returns the next downloads to start.
    pub fn loaded(&mut self, url: &str, result: Result<Decoded, String>) -> Vec<String> {
        self.in_flight = self.in_flight.saturating_sub(1);
        if let Some(slot) = self.slots.get_mut(url) {
            // Unreadable images are marked failed; the view falls back to text.
            let ready = result.ok().and_then(|decoded| decoded.0);
            match ready {
                Some((state, cost)) => {
                    slot.cost = cost;
                    slot.state = state;
                    self.used += cost;
                }
                None => slot.state = State::Failed,
            }
        }
        self.evict();
        self.start()
    }

    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        self.evict();
    }

    /// Drops everything; downloads still in flight are discarded when they finish.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.queue.clear();
        self.used = 0;
    }

    /// (cached images, bytes counted against the budget)
    pub fn stats(&self) -> (usize, usize) {
        let ready = self.slots.values().filter(|s| matches!(s.state, State::Ready(_) | State::Animated(..))).count();
        (ready, self.used)
    }

    fn start(&mut self) -> Vec<String> {
        let mut started = Vec::new();
        while self.in_flight < MAX_IN_FLIGHT {
            let Some(url) = self.queue.pop_front() else { break };
            let Some(slot) = self.slots.get_mut(&url) else { continue }; // evicted while queued
            slot.state = State::Loading;
            self.in_flight += 1;
            started.push(url);
        }
        started
    }

    fn evict(&mut self) {
        while self.used > self.budget {
            let oldest = self
                .slots
                .iter()
                .filter(|(_, s)| matches!(s.state, State::Ready(_) | State::Animated(..)) && s.last_wanted < self.generation)
                .min_by_key(|(_, s)| s.last_wanted)
                .map(|(url, _)| url.clone());
            let Some(url) = oldest else { break };
            if let Some(slot) = self.slots.remove(&url) {
                self.used -= slot.cost;
            }
        }
        if self.slots.len() > MAX_SLOTS {
            let generation = self.generation;
            self.slots.retain(|_, s| !matches!(s.state, State::Failed) || s.last_wanted == generation);
        }
    }
}

fn decode(bytes: Vec<u8>, shape: Shape) -> Option<(State, usize)> {
    let still = |bytes: Vec<u8>, round_it: bool| -> Option<(State, usize)> {
        if round_it {
            round(&bytes).map(|(handle, cost)| (State::Ready(handle), cost))
        } else {
            decoded_cost(&bytes).map(|cost| (State::Ready(Handle::from_bytes(bytes)), cost))
        }
    };
    match shape {
        Shape::Square => still(bytes, false),
        Shape::Round => still(bytes, true),
        Shape::Animated { round } => match animation(&bytes, round) {
            Some(decoded) => Some(decoded),
            None => still(bytes, round),
        },
    }
}

/// GIF frames as RGBA (masked round if asked). `None` for non-GIFs, single
/// frames and animations over the limits, which are then shown as stills.
fn animation(bytes: &[u8], round: bool) -> Option<(State, usize)> {
    use image::AnimationDecoder;
    if bytes.len() > MAX_IMAGE_BYTES || image::guess_format(bytes).ok()? != image::ImageFormat::Gif {
        return None;
    }
    let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).ok()?;
    let mut frames = Vec::new();
    let mut cost = bytes.len();
    for frame in decoder.into_frames().take(MAX_FRAMES + 1) {
        let frame = frame.ok()?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        // Like browsers: delays under 20 ms mean "unset" and play at 100 ms.
        let delay = match numerator / denominator.max(1) {
            ms if ms < 20 => 100,
            ms => ms,
        };
        let buffer = frame.into_buffer();
        let (width, height) = buffer.dimensions();
        if width.max(height) > MAX_ANIMATION_SIDE {
            return None;
        }
        let mut pixels = buffer.into_raw();
        if round {
            round_mask(&mut pixels, width, height);
        }
        cost += pixels.len();
        if cost > MAX_ANIMATION_BYTES || frames.len() == MAX_FRAMES {
            return None;
        }
        frames.push((Handle::from_rgba(width, height, pixels), delay));
    }
    if frames.len() < 2 {
        return None;
    }
    let total = frames.iter().map(|(_, delay)| delay).sum();
    Some((State::Animated(frames, total), cost))
}

/// Bytes the image occupies once decoded, or `None` if it can't be read.
fn decoded_cost(bytes: &[u8]) -> Option<usize> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let (width, height) = image::io::Reader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    Some(bytes.len() + width as usize * height as usize * 4)
}

/// Decodes and applies an anti-aliased circular alpha mask. Only the RGBA is
/// kept, so the cost is its size.
fn round(bytes: &[u8]) -> Option<(Handle, usize)> {
    let (width, height) =
        image::io::Reader::new(Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
    if bytes.len() > MAX_IMAGE_BYTES || width.max(height) > MAX_ROUND_SIDE {
        return None;
    }
    let mut pixels = image::load_from_memory(bytes).ok()?.to_rgba8().into_raw();
    round_mask(&mut pixels, width, height);
    let cost = pixels.len();
    Some((Handle::from_rgba(width, height, pixels), cost))
}

fn round_mask(rgba: &mut [u8], width: u32, height: u32) {
    let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
    let radius = width.min(height) as f32 / 2.0;
    for y in 0..height {
        for x in 0..width {
            let distance = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let coverage = (radius - distance + 0.5).clamp(0.0, 1.0);
            let alpha = &mut rgba[(y * width + x) as usize * 4 + 3];
            *alpha = (f32::from(*alpha) * coverage) as u8;
        }
    }
}

pub fn guild_icon_url(guild_id: &str, hash: &str) -> Option<String> {
    // Hashes are hex (optionally "a_" for animated); a static frame is enough.
    hash.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
        .then(|| format!("https://cdn.discordapp.com/icons/{guild_id}/{hash}.webp?size=96"))
}

/// Only Discord's own CDN and media proxy, so viewing a channel never contacts
/// third-party servers.
pub fn is_discord_media(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    host == "cdn.discordapp.com" || host == "media.discordapp.net" || host.ends_with(".discordapp.net")
}

/// `animated` asks for the GIF (only when animation is on).
pub fn emoji_url(id: &str, animated: bool) -> String {
    let format = if animated { "gif" } else { "png" };
    format!("https://cdn.discordapp.com/emojis/{id}.{format}?size=48")
}

/// Tenor's own media host, used only for GIFs (the GIF picker, and GIF embeds
/// when animation is on).
pub fn is_tenor_media(url: &str) -> bool {
    url.strip_prefix("https://media.tenor.com/").is_some_and(|rest| !rest.contains("..") && !rest.is_empty())
}

/// The small animated GIF ("tinygif") for a Tenor media URL such as an embed
/// thumbnail (`…AAAAe/name.png`) or a full GIF (`…AAAAC/name.gif`). Tenor's
/// format code is the last 5 characters of the first path segment.
pub fn tenor_tinygif(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://media.tenor.com/")?;
    let (id, file) = rest.split_once('/')?;
    let name = file.split(['?', '#']).next()?.rsplit_once('.')?.0;
    let valid = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if id.len() <= 5 || !valid(id) || !valid(name) {
        return None;
    }
    Some(format!("https://media.tenor.com/{}AAAAM/{name}.gif", &id[..id.len() - 5]))
}

pub struct Preview {
    pub url: String,
    pub width: f32,
    pub height: f32,
}

/// A scaled-down, WebP-encoded preview from Discord's media proxy.
pub fn preview(proxy_url: &str, width: u32, height: u32) -> Option<Preview> {
    preview_with(proxy_url, width, height, false)
}

/// `animated` keeps the original format (a GIF stays a GIF) instead of WebP.
pub fn preview_with(proxy_url: &str, width: u32, height: u32, animated: bool) -> Option<Preview> {
    if width == 0 || height == 0 || !is_discord_media(proxy_url) {
        return None;
    }
    let (w, h) = fit(width, height);
    let separator = if proxy_url.contains('?') { '&' } else { '?' };
    let format = if animated { "" } else { "&format=webp" };
    let url = format!("{proxy_url}{separator}width={}&height={}{format}", w as u32, h as u32);
    Some(Preview { url, width: w, height: h })
}

/// A Tenor GIF at its own size, scaled into the preview box.
pub fn tenor_preview(url: String, width: u32, height: u32) -> Option<Preview> {
    if width == 0 || height == 0 || !is_tenor_media(&url) {
        return None;
    }
    let (w, h) = fit(width, height);
    Some(Preview { url, width: w, height: h })
}

fn fit(width: u32, height: u32) -> (f32, f32) {
    let scale = (PREVIEW_BOX.0 / width as f32).min(PREVIEW_BOX.1 / height as f32).min(1.0);
    ((width as f32 * scale).round().max(1.0), (height as f32 * scale).round().max(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_discord_hosts() {
        assert!(is_discord_media("https://cdn.discordapp.com/emojis/1.png"));
        assert!(is_discord_media("https://images-ext-1.discordapp.net/external/x"));
        assert!(!is_discord_media("http://cdn.discordapp.com/x"));
        assert!(!is_discord_media("https://evil.com/cdn.discordapp.com"));
        assert!(!is_discord_media("https://discordapp.net.evil.com/x"));
    }

    #[test]
    fn previews_fit_and_keep_signed_query() {
        let p = preview("https://media.discordapp.net/a.png?ex=1&hm=2", 1600, 900).unwrap();
        assert_eq!((p.width, p.height), (400.0, 225.0));
        assert!(p.url.ends_with("?ex=1&hm=2&width=400&height=225&format=webp"));
        assert_eq!(preview("https://media.discordapp.net/a.png", 100, 50).unwrap().width, 100.0);
    }

    #[test]
    fn evicts_least_recently_wanted() {
        let png = |w: u32| {
            let mut bytes = Vec::new();
            image::DynamicImage::new_rgba8(w, 1)
                .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
                .unwrap();
            bytes
        };
        let square = |url: &str| [(url.to_owned(), Shape::Square)];
        let mut cache = ImageCache::new(1000);
        assert_eq!(cache.want(square("a")), ["a"]);
        cache.loaded("a", Ok(prepare(png(100), Shape::Square)));
        assert_eq!(cache.want(square("b")), ["b"]);
        cache.loaded("b", Ok(prepare(png(100), Shape::Square)));
        // a (~400 B decoded) and b together exceed nothing yet; c pushes over budget.
        cache.want(square("c"));
        cache.loaded("c", Ok(prepare(png(100), Shape::Square)));
        assert!(cache.get("a").is_none(), "oldest evicted first");
        assert!(cache.get("c").is_some(), "currently wanted kept");
        assert!(cache.stats().1 <= 1000);
    }

    fn gif(frames: &[(u8, u32)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for &(shade, ms) in frames {
                let buffer = image::RgbaImage::from_pixel(8, 8, image::Rgba([shade, 0, 0, 255]));
                let delay = image::Delay::from_numer_denom_ms(ms, 1);
                encoder.encode_frame(image::Frame::from_parts(buffer, 0, 0, delay)).unwrap();
            }
        }
        bytes
    }

    #[test]
    fn animated_gifs_follow_the_clock() {
        let mut cache = ImageCache::new(1 << 20);
        let shape = Shape::Animated { round: false };
        cache.want([("a".to_owned(), shape)]);
        cache.loaded("a", Ok(prepare(gif(&[(10, 100), (200, 300)]), shape)));
        assert_eq!(cache.shortest_frame(), Some(100));
        let red = |cache: &ImageCache| match cache.get("a") {
            Some(Handle::Rgba { pixels, .. }) => pixels[0],
            other => panic!("expected RGBA frame, got {other:?}"),
        };
        assert!(red(&cache) < 100, "first frame at 0 ms");
        cache.set_clock(150);
        assert!(red(&cache) > 100, "second frame from 100 ms");
        cache.set_clock(420);
        assert!(red(&cache) < 100, "loops after 400 ms");
        // One frame, or animation not asked for: a still.
        cache.want([("b".to_owned(), shape), ("c".to_owned(), Shape::Square)]);
        cache.loaded("b", Ok(prepare(gif(&[(10, 100)]), shape)));
        cache.loaded("c", Ok(prepare(gif(&[(10, 100), (200, 100)]), Shape::Square)));
        assert_eq!(cache.shortest_frame(), None, "neither b nor c animates");
    }

    #[test]
    fn tenor_urls() {
        assert!(is_tenor_media("https://media.tenor.com/abcdefghijkAAAAC/cat.gif"));
        assert!(!is_tenor_media("https://media.tenor.com.evil.example/x.gif"));
        assert!(!is_tenor_media("http://media.tenor.com/x.gif"));
        assert_eq!(
            tenor_tinygif("https://media.tenor.com/abcdefghijkAAAAe/funny-cat.png?x=1").as_deref(),
            Some("https://media.tenor.com/abcdefghijkAAAAM/funny-cat.gif")
        );
        assert_eq!(tenor_tinygif("https://media.tenor.com/AAAAe/x.png"), None);
        assert_eq!(tenor_tinygif("https://example.com/abcdefghijkAAAAe/x.png"), None);
        assert_eq!(tenor_tinygif("https://media.tenor.com/abc..defAAAAe/x.png"), None);
    }

    #[test]
    fn round_mask_clears_corners_keeps_centre() {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(32, 32, image::Rgba([200, 100, 50, 255]))
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        let (_, cost) = round(&bytes).unwrap();
        assert_eq!(cost, 32 * 32 * 4);
        let mut cache = ImageCache::new(1 << 20);
        cache.want([("icon".to_owned(), Shape::Round)]);
        cache.loaded("icon", Ok(prepare(bytes, Shape::Round)));
        assert!(cache.get("icon").is_some());

        let mut rgba = vec![255u8; 32 * 32 * 4];
        round_mask(&mut rgba, 32, 32);
        let alpha = |x: usize, y: usize| rgba[(y * 32 + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "corner transparent");
        assert_eq!(alpha(16, 16), 255, "centre opaque");
        assert!(alpha(16, 0) > 0, "edge midpoint still visible");
        assert_eq!(rgba[0], 255, "colour untouched");
    }
}
