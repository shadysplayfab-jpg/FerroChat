# Voice chat: scope and plan

Status: **phases 1–4 built, not yet tested on a live call** (2026-11-05). Researched
against Discord's
[voice connections docs](https://docs.discord.com/developers/topics/voice-connections).

| Phase | State |
|---|---|
| 1. Signalling | Built. `src/voice/mod.rs` plus op 4, `VOICE_STATE_UPDATE` and `VOICE_SERVER_UPDATE` in the main gateway, with voice channels and participants in the UI. |
| 2. Transport | Built. `src/voice/rtp.rs` does IP discovery, RTP, and both AEAD rtpsize modes (unit-tested), and sends 5 silence frames after Session Description. |
| 3. DAVE | Built. `src/voice/dave.rs` is a state machine over `davey` 0.2.0 (pinned). We advertise v1 and handle op 25/27/29/30 (binary) and 21/22/24 (JSON). Replies are 23/26/28/31. Clients connect/disconnect (11/13) is used to validate the MLS roster. Audio is held back until keys are ready, never sent in the clear. A test plays the gateway as MLS external sender and takes two members through key package, proposal, commit and welcome, then transition to an encrypted frame that the other member decrypts. The voice privacy code shows in the call panel. |
| 4. Audio | Built. `audio.rs` runs WASAPI through cpal on its own thread for the call, and dropping it stops and joins that thread. `dsp.rs` has the streaming resampler, voice-activity gate with 300 ms hold, jitter buffer (40–200 ms, handles sequence wrap) and mixer. `media.rs` does Opus at 48 kHz stereo, 20 ms and 64 kbps, with a decoder per SSRC freed after 10 s idle. The voice task runs a 20 ms clock: capture, gate, encode, DAVE, AEAD, send; and receive, AEAD, DAVE (user from the op 5 SSRC), decode, jitter buffer, mix, play. Mute and deafen apply locally at once through shared atomics, plus op 4 to the server. Settings has the devices, volumes and threshold. `audiopus` 0.2 linked its bundled libopus, so CMake wasn't needed after all. Tests: an Opus round trip, the DSP units, and an opt-in real-device test that passed here (both devices opened in 52 ms, the mic captured, the speakers pulled audio). |
| 5. UI | Partly done: the call panel (Mute, Deafen, Leave), speaking highlights, and a Settings toggle. |

## The hard requirement: DAVE end-to-end encryption

Since **March 2026**, Discord rejects clients without DAVE support in DMs, group
DMs, voice channels and Go Live streams. Only stage channels are exempt.
Plain-transport voice is no longer an option, so DAVE is part of the first
working version, not a later add-on.

DAVE is an MLS group (via OpenMLS) where the voice server is an *external
sender*, plus AES-128-GCM per-frame encryption of Opus.
[`davey`](https://docs.rs/davey) (MIT, Rust, OpenMLS) already implements the
session side: `DaveSession` has `set_external_sender`, `create_key_package`,
`process_proposals`, `process_commit` / `process_welcome`,
`prepare_transition` / `execute_transition`, `encrypt_opus` and
`decrypt(user_id, …)`. We'd wire it to the voice gateway rather than
reimplement MLS. Risk: `davey` 0.2.0 was released 2026-11-02 and has little
documentation, so pin the version and expect to read its source.

## What has to be built

| # | Piece | Work | New dependencies |
|---|---|---|---|
| 1 | **Signalling** | Main gateway op 4 join/leave. Wait for both `VOICE_STATE_UPDATE` (session id) and `VOICE_SERVER_UPDATE` (token, endpoint). Voice WebSocket **v8**: Identify with `max_dave_protocol_version`, Hello, heartbeat with `seq_ack`, Ready, Resume (op 7/9). Show voice channels and who's in them (guild `voice_states`). | none |
| 2 | **Transport** | UDP IP discovery (74-byte request/response), Select Protocol with `aead_aes256_gcm_rtpsize` (preferred) or `aead_xchacha20_poly1305_rtpsize` (required fallback), RTP header, sequence/timestamp, Speaking (op 5) before sending, 5 silence frames when stopping. | `aes-gcm`, `chacha20poly1305` (pure Rust) |
| 3 | **DAVE** | Binary voice opcodes (2-byte seq + opcode + payload): 25 external sender, 26 key package, 27 proposals, 28 commit/welcome, 29 announce commit, 30 welcome, 31 invalid commit/welcome. Transitions 21–24 (prepare, execute, ready, prepare epoch). `encrypt_opus` before transport encryption, `decrypt` per sender (SSRC → user id from speaking/client-connect events). Show the voice privacy code. | `davey` |
| 4 | **Audio** | Microphone capture and speaker playback via WASAPI, resampling to 48 kHz stereo, 20 ms Opus frames, per-speaker jitter buffer and mixing, mute/deafen, voice-activity gate or push-to-talk, device pickers and volumes in Settings. Optional noise suppression. | `cpal`, `opus` (C libopus; **needs CMake** to build), `rubato`; optional `nnnoiseless` |
| 5 | **UI** | Voice channels in the list, a connected-call panel (channel, Mute, Deafen, Disconnect), speaking rings on members, privacy code, a Settings "Voice" section, and a toggle to hide voice entirely. | none |

Out of scope for the first version: video, screen share (Go Live), stage
channels, soundboard, Krisp-grade noise removal.

### What only a live call can show

- Discord accepting our key package and commit/welcome bytes. The test uses
  OpenMLS on both sides, the same as Discord's real servers in principle, but
  it isn't Discord.
- Joining a call that already has members, where we get a welcome instead of
  creating the group.
- Rejoin after an invalid commit/welcome (op 31). This is unit-tested but not
  exercised against the gateway.
- Our op 28 layout (commit, then welcome, no selector byte) follows discord.js,
  not the spec text, which mentions a selector. If Discord rejects commits, look
  here first.
- Audio quality, latency and clock drift over a long call. Capture is bounded
  to 100 ms and playback to 200 ms, so drift drops samples rather than adding
  delay.
- Hearing people who were already talking before we joined. We learn a
  speaker's SSRC from their next speaking event, so a speaker could be silent
  to us until then.
- No echo cancellation: use headphones. Noise suppression (RNNoise via
  `nnnoiseless`, before the voice gate) is optional in Settings and off by
  default; a unit test checks it quiets steady hiss, not how it sounds on a call.
- Push-to-talk polls the chosen key with `GetAsyncKeyState` on the 20 ms media
  clock, so it works while Ferrochat is in the background without a keyboard
  hook, and nothing intercepts the key. Untested on a live call.

## Build prerequisite

libopus is C, and every Rust Opus binding builds it with **CMake** on Windows.
CMake isn't installed here (it isn't in PATH or in the VS install). It's
installed via `winget install Kitware.CMake`, about 100 MB. Everything else is
pure Rust.

## Cost estimate (not measured)

- **Memory:** probably +10–20 MB while in a call (OpenMLS state, audio buffers,
  jitter buffers). Nothing extra when not in a call, because the voice
  connection and audio devices only exist during a call.
- **CPU:** Opus at 20 ms frames is typically about 1–3% of one core.
- **Code:** about 2,500–3,500 lines across 3–4 sessions of work.

## How it would be verified

- **Unit tests:** IP-discovery packets, RTP headers, transport encryption round
  trips against known vectors, binary opcode framing, the jitter buffer.
- **Live test (the only real proof):** your test account in a voice channel with
  a second person or account. DAVE interop, audio quality and transitions when
  people join and leave can only be shown against Discord itself. The offline
  demo can't cover voice.
- **Risk:** this runs over a user token like the rest of the client. Whether
  Discord treats third-party voice differently from text is unknown.

## Suggested order

1. **Phases 1 and 2:** join a channel and send silence. That proves signalling
   and transport, but expect Discord to drop the connection without DAVE.
2. **Phase 3:** DAVE. This is the first point where a call should actually stay
   connected.
3. **Phase 4:** real audio in and out.
4. **Phase 5:** UI polish and Settings.
