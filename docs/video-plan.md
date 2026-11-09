# Video and screen share: scope

Status: **watching is built, untested live.** Receiving cameras and Go Live
streams (H.264 only, Media Foundation decoding, small tiles) is in
`src/voice/h264.rs`, `decoder.rs` and `video.rs`, behind Settings → Performance
→ Video. Offline tests cover depacketizing, DAVE video decryption and a full
encode → RTP → decode round trip. Not yet confirmed on a live call: the op 15
media sink wants, the PLI (RTCP) encryption format, the Go Live DAVE group id,
and whether Discord sends video to a client that only decodes.

**Screen share sending is built, untested live** (`src/voice/screen.rs`): op 18
STREAM_CREATE, op 22 unpause, a second voice connection with `streams: [screen]`,
op 12 announcing our SSRCs, GDI capture scaled to ≤720p, Windows' H.264 encoder
(15 fps, 2.5 Mbit/s CBR, baseline, GOP 2 s, low latency), DAVE video encryption,
FU-A packetizing (`h264::packetize`), and PLI → forced keyframe. Unconfirmed:
whether Discord wants RTP header extensions or a simulcast `rid` on our packets,
and the stream's DAVE group id. Sending a camera is not started.

The rest of this file is the plan as written before the code, the same way
voice was scoped first (`docs/voice-plan.md`). Protocol details come from what
Discord's clients and public reverse-engineering projects describe.

## What Discord does

- **Camera video** rides on the **same voice connection** as audio. Identify
  says the client can do video, Ready lists the video SSRCs we may send on,
  `Select Protocol` advertises the codecs we support with their RTP payload
  types, and op 12 announces when our camera starts or stops. The main gateway's
  op 4 carries `self_video`.
- **Go Live (screen share)** is a **separate connection**. The main gateway
  starts a stream (op 18) or watches one (op 20), then sends a stream server and
  token (`STREAM_CREATE` / `STREAM_SERVER_UPDATE`). We open a second voice-style
  connection to it, with its own DAVE session, and send or receive video there.
- **Codecs:** H.264, VP8, VP9 and AV1. Every sender picks from what the
  receivers support, so the codecs we can **decode** decide what others send us.
- **DAVE** (end-to-end encryption, required since March 2026) also covers video.
  Frames are encrypted with codec-aware unencrypted ranges, so the SFU can still
  read headers (H.264 NAL headers, VP8 payload descriptors). `davey` takes the
  codec when encrypting, so it should already handle this; that's unchecked.
- **Transport:** RTP packetisation per codec (H.264 FU-A, VP8 descriptors),
  RTX retransmission, RTCP NACKs and keyframe requests (PLI), and bitrate
  adaptation when the network struggles.

## What has to be built

| Part | What | Likely way on Windows |
|------|------|------------------------|
| Signalling | Video in identify/Ready/select protocol, op 12, `self_video`, Go Live ops 18/20/22 and the second connection | Extends `src/voice/` |
| Packetising | H.264 and VP8 RTP payloads both ways, frame reassembly, jitter, RTX, NACK, PLI | New `src/video/rtp.rs` |
| Decode | H.264 (all hardware has it); VP8 is common from browsers | **Media Foundation** H.264 decoder (built into Windows, often hardware); VP8 needs libvpx |
| Encode | H.264 for our camera or screen | Media Foundation H.264 encoder (hardware where available) |
| Capture | Screens and windows; webcams | Windows.Graphics.Capture; Media Foundation source reader |
| Show | Video tiles in the call view, full-screen stream | Frames converted to RGBA for iced |
| Settings | Camera picker, stream quality, "video off" in Lightweight mode | `settings.rs`, Performance card |

## The costs that matter here

These are estimates, not measurements:

- **Drawing frames with the software renderer is expensive.** A 720p30 stream
  is about 110 MB/s of RGBA to copy and scale on the CPU, which would undo
  Ferrochat's point. Realistic choices:
  - Show small tiles (e.g. 320×180) and only enlarge one stream at a time.
  - Make video use the GPU renderer, or switch to it while a video is open.
    That's about 170 MB more, as measured earlier for wgpu.
- **Decoding** with Media Foundation is mostly hardware on any GPU from the last
  ~10 years, so it's cheap in CPU. A software fallback costs much more.
- **Dependencies:** Media Foundation and Windows.Graphics.Capture are part of
  Windows. Using them from Rust means the `windows` crate with several API
  features: a larger build, but no C toolchain like CMake or nasm. VP8 via
  libvpx would need a C build. AV1 and VP9 we would leave out.
- **Effort:** similar to voice again. It's the biggest remaining feature by far.

## Decisions needed before starting

1. **Which first:** watching others (camera and Go Live) is less work and more
   useful than sending. Sending needs capture, encoding and bitrate control too.
2. **Codecs:** H.264 only through Media Foundation (no C toolchain), or also VP8
   through libvpx (needs a C build, covers more senders).
3. **Rendering:** small tiles on the software renderer, or the GPU renderer for
   video.

## Suggested order

1. Receive camera video in a voice call (H.264, small tiles). This proves
   signalling, DAVE for video, packetisation and decoding.
2. Watch Go Live streams: the second connection, one large view.
3. Send camera video: capture, encode and bitrate control.
4. Share a screen or window (Go Live).
5. Optionally VP8 decoding, and simulcast or quality picking.

Each step needs a live call to verify. Like voice, it would get a "built but
untested live" status until you try it.
