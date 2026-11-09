//! DAVE end-to-end encryption for one call, on top of `davey`. A plain state
//! machine: feed it voice gateway messages, send back what it returns.
//!
//! Wire formats follow <https://daveprotocol.com>. Server binary frames are
//! `seq (u16 BE) | opcode | payload`; client binary frames are `opcode | payload`.
//! Commit/welcome (op 28) is the commit followed directly by the welcome, as in
//! discord.js, which runs the same `davey` code in production.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU16;

use davey::{DaveSession, ProposalsOperationType};
use serde_json::{json, Value};

pub const MAX_PROTOCOL_VERSION: u16 = davey::DAVE_PROTOCOL_VERSION;
/// Seconds to keep accepting unencrypted frames after a downgrade.
const PASSTHROUGH_EXPIRY: u32 = 10;

pub mod op {
    pub const PREPARE_TRANSITION: u8 = 21;
    pub const EXECUTE_TRANSITION: u8 = 22;
    pub const TRANSITION_READY: u8 = 23;
    pub const PREPARE_EPOCH: u8 = 24;
    pub const EXTERNAL_SENDER: u8 = 25;
    pub const KEY_PACKAGE: u8 = 26;
    pub const PROPOSALS: u8 = 27;
    pub const COMMIT_WELCOME: u8 = 28;
    pub const ANNOUNCE_COMMIT: u8 = 29;
    pub const WELCOME: u8 = 30;
    pub const INVALID_COMMIT_WELCOME: u8 = 31;
}

#[derive(Debug, PartialEq)]
pub enum Outgoing {
    Json(Value),
    Binary(Vec<u8>),
}

/// (sequence, opcode, payload) of a server binary frame.
pub fn parse_server_frame(frame: &[u8]) -> Option<(u16, u8, &[u8])> {
    let (header, payload) = frame.split_at_checked(3)?;
    Some((u16::from_be_bytes([header[0], header[1]]), header[2], payload))
}

fn client_frame(opcode: u8, parts: &[&[u8]]) -> Outgoing {
    let mut frame = Vec::with_capacity(1 + parts.iter().map(|p| p.len()).sum::<usize>());
    frame.push(opcode);
    for part in parts {
        frame.extend_from_slice(part);
    }
    Outgoing::Binary(frame)
}

/// `transition_id (u16 BE) | rest`.
fn split_transition(payload: &[u8]) -> Option<(u16, &[u8])> {
    let (id, rest) = payload.split_at_checked(2)?;
    Some((u16::from_be_bytes([id[0], id[1]]), rest))
}

pub struct Dave {
    user_id: u64,
    channel_id: u64,
    session: Option<DaveSession>,
    protocol_version: u16,
    /// Everyone in the call, us included, used to validate MLS membership.
    users: HashSet<u64>,
    /// Waiting for a fresh welcome after reporting an invalid one; avoids
    /// reporting the same failure repeatedly.
    recovering: bool,
    /// Prepared transitions: id → protocol version applied on execute.
    pending: HashMap<u16, u16>,
}

impl Dave {
    pub fn new(user_id: u64, channel_id: u64) -> Self {
        Self {
            user_id,
            channel_id,
            session: None,
            protocol_version: 0,
            users: HashSet::from([user_id]),
            recovering: false,
            pending: HashMap::new(),
        }
    }

    pub fn protocol_version(&self) -> u16 {
        self.protocol_version
    }

    /// Encryption is active and keys are in place.
    pub fn is_ready(&self) -> bool {
        self.protocol_version > 0 && self.session.as_ref().is_some_and(DaveSession::is_ready)
    }

    /// The code users compare to confirm the call is end-to-end encrypted.
    pub fn privacy_code(&self) -> Option<String> {
        self.session.as_ref()?.voice_privacy_code().map(str::to_owned)
    }

    pub fn users_connected(&mut self, ids: impl IntoIterator<Item = u64>) {
        self.users.extend(ids);
    }

    pub fn user_disconnected(&mut self, id: u64) {
        self.users.remove(&id);
    }

    /// Session Description (op 4) told us the starting protocol version.
    pub fn start(&mut self, protocol_version: u16) -> Result<Vec<Outgoing>, String> {
        self.protocol_version = protocol_version;
        self.reinit()
    }

    /// E2EE-encrypts an Opus frame when the call is encrypted. Before keys are
    /// ready on an encrypted call, frames are dropped rather than sent in clear.
    pub fn encrypt(&mut self, opus: &[u8]) -> Result<Option<Vec<u8>>, String> {
        if self.protocol_version == 0 {
            return Ok(Some(opus.to_vec()));
        }
        match self.session.as_mut() {
            Some(session) if session.is_ready() => {
                session.encrypt_opus(opus).map(|frame| Some(frame.into_owned())).map_err(|e| e.to_string())
            }
            _ => Ok(None),
        }
    }

    /// Like [`Dave::encrypt`], for a whole H.264 frame (Annex B) before packetizing.
    /// DAVE leaves the NAL unit headers readable, so the result still splits on start codes.
    pub fn encrypt_video(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>, String> {
        if self.protocol_version == 0 {
            return Ok(Some(frame.to_vec()));
        }
        match self.session.as_mut() {
            Some(session) if session.is_ready() => session
                .encrypt(davey::MediaType::VIDEO, davey::Codec::H264, frame)
                .map(|frame| Some(frame.into_owned()))
                .map_err(|e| e.to_string()),
            _ => Ok(None),
        }
    }

    /// Decrypts another member's E2EE frame. Unencrypted calls pass frames through.
    pub fn decrypt(&mut self, user_id: u64, frame: &[u8]) -> Result<Vec<u8>, String> {
        match self.session.as_mut() {
            Some(session) if self.protocol_version > 0 => {
                session.decrypt(user_id, davey::MediaType::AUDIO, frame).map_err(|e| e.to_string())
            }
            _ => Ok(frame.to_vec()),
        }
    }

    /// Decrypts a whole video frame (after depacketizing) from another member.
    pub fn decrypt_video(&mut self, user_id: u64, frame: &[u8]) -> Result<Vec<u8>, String> {
        match self.session.as_mut() {
            Some(session) if self.protocol_version > 0 => {
                session.decrypt(user_id, davey::MediaType::VIDEO, frame).map_err(|e| e.to_string())
            }
            _ => Ok(frame.to_vec()),
        }
    }

    pub fn on_json(&mut self, opcode: u8, data: &Value) -> Result<Vec<Outgoing>, String> {
        let field = |name: &str| data[name].as_u64().and_then(|n| u16::try_from(n).ok());
        match opcode {
            op::PREPARE_TRANSITION => {
                let (Some(transition), Some(version)) = (field("transition_id"), field("protocol_version")) else {
                    return Err("malformed prepare transition".to_owned());
                };
                if version == 0 {
                    if let Some(session) = &mut self.session {
                        session.set_passthrough_mode(true, Some(PASSTHROUGH_EXPIRY));
                    }
                }
                self.prepare(transition, version)
            }
            op::EXECUTE_TRANSITION => {
                let transition = field("transition_id").ok_or("malformed execute transition")?;
                if let Some(version) = self.pending.remove(&transition) {
                    self.protocol_version = version;
                }
                if let Some(session) = &mut self.session {
                    session.execute_transition(transition).map_err(|e| e.to_string())?;
                }
                Ok(Vec::new())
            }
            op::PREPARE_EPOCH => {
                // Epoch 1 means a new group: start over with a fresh key package.
                if data["epoch"].as_u64() == Some(1) {
                    self.protocol_version = field("protocol_version").ok_or("malformed prepare epoch")?;
                    return self.reinit();
                }
                Ok(Vec::new())
            }
            _ => Ok(Vec::new()),
        }
    }

    pub fn on_binary(&mut self, opcode: u8, payload: &[u8]) -> Result<Vec<Outgoing>, String> {
        match opcode {
            op::EXTERNAL_SENDER => {
                self.session_mut()?.set_external_sender(payload).map_err(|e| e.to_string())?;
                Ok(Vec::new())
            }
            op::PROPOSALS => {
                let (&kind, proposals) = payload.split_first().ok_or("empty proposals")?;
                let kind = match kind {
                    0 => ProposalsOperationType::APPEND,
                    1 => ProposalsOperationType::REVOKE,
                    other => return Err(format!("unknown proposals operation {other}")),
                };
                let users: Vec<u64> = self.users.iter().copied().collect();
                let result = self.session_mut()?.process_proposals(kind, proposals, Some(&users)).map_err(|e| e.to_string())?;
                Ok(match result {
                    Some(cw) => vec![client_frame(op::COMMIT_WELCOME, &[&cw.commit, cw.welcome.as_deref().unwrap_or(&[])])],
                    None => Vec::new(),
                })
            }
            op::ANNOUNCE_COMMIT => {
                let (transition, commit) = split_transition(payload).ok_or("malformed commit")?;
                match self.session_mut()?.process_commit(commit) {
                    Ok(true) => self.prepare(transition, self.protocol_version),
                    Ok(false) => Ok(Vec::new()),
                    Err(error) => self.recover(transition, &error.to_string()),
                }
            }
            op::WELCOME => {
                let (transition, welcome) = split_transition(payload).ok_or("malformed welcome")?;
                let users: Vec<u64> = self.users.iter().copied().collect();
                match self.session_mut()?.process_welcome(welcome, Some(&users)) {
                    Ok(()) => {
                        self.recovering = false;
                        self.prepare(transition, self.protocol_version)
                    }
                    Err(error) => self.recover(transition, &error.to_string()),
                }
            }
            _ => Ok(Vec::new()),
        }
    }

    fn session_mut(&mut self) -> Result<&mut DaveSession, String> {
        self.session.as_mut().ok_or_else(|| "DAVE message before the session started".to_owned())
    }

    /// Prepares a transition; id 0 applies immediately, others wait for op 22.
    /// (davey switches keys itself; we track the version to know when to encrypt.)
    fn prepare(&mut self, transition: u16, version: u16) -> Result<Vec<Outgoing>, String> {
        if let Some(session) = &mut self.session {
            session.prepare_transition(transition, version).map_err(|e| e.to_string())?;
        }
        if transition == 0 {
            self.protocol_version = version;
            return Ok(Vec::new());
        }
        self.pending.insert(transition, version);
        Ok(vec![Outgoing::Json(json!({ "op": op::TRANSITION_READY, "d": { "transition_id": transition } }))])
    }

    /// Reports an unusable commit/welcome and rejoins with a fresh key package.
    fn recover(&mut self, transition: u16, reason: &str) -> Result<Vec<Outgoing>, String> {
        if self.recovering {
            return Ok(Vec::new());
        }
        self.recovering = true;
        let mut out = vec![Outgoing::Json(json!({
            "op": op::INVALID_COMMIT_WELCOME,
            "d": { "transition_id": transition },
        }))];
        out.extend(self.reinit().map_err(|e| format!("{reason}; then {e}"))?);
        Ok(out)
    }

    fn reinit(&mut self) -> Result<Vec<Outgoing>, String> {
        let Some(version) = NonZeroU16::new(self.protocol_version) else {
            if let Some(session) = &mut self.session {
                session.reset().map_err(|e| e.to_string())?;
                session.set_passthrough_mode(true, Some(PASSTHROUGH_EXPIRY));
            }
            return Ok(Vec::new());
        };
        let session = match &mut self.session {
            Some(session) => {
                session.reinit(version, self.user_id, self.channel_id, None).map_err(|e| e.to_string())?;
                session
            }
            None => self.session.insert(
                DaveSession::new(version, self.user_id, self.channel_id, None).map_err(|e| e.to_string())?,
            ),
        };
        let key_package = session.create_key_package().map_err(|e| e.to_string())?;
        Ok(vec![client_frame(op::KEY_PACKAGE, &[&key_package])])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames() {
        assert_eq!(parse_server_frame(&[0, 7, 25, 1, 2]), Some((7, 25, &[1u8, 2][..])));
        assert_eq!(parse_server_frame(&[0, 7]), None);
        assert_eq!(split_transition(&[0x01, 0x02, 9]), Some((0x0102, &[9u8][..])));
        assert_eq!(client_frame(28, &[&[1, 2], &[3]]), Outgoing::Binary(vec![28, 1, 2, 3]));
    }

    #[test]
    fn starts_with_key_package_and_stays_plain_without_dave() {
        let mut plain = Dave::new(1, 2);
        assert!(plain.start(0).unwrap().is_empty());
        assert_eq!(plain.encrypt(b"opus").unwrap(), Some(b"opus".to_vec()));

        let mut dave = Dave::new(1, 2);
        let out = dave.start(MAX_PROTOCOL_VERSION).unwrap();
        let [Outgoing::Binary(frame)] = out.as_slice() else { panic!("expected a key package, got {out:?}") };
        assert_eq!(frame[0], op::KEY_PACKAGE);
        assert!(frame.len() > 100, "MLS key package");
        // Not in a group yet: audio is held back, never sent unencrypted.
        assert_eq!(dave.encrypt(b"opus").unwrap(), None);
        assert!(!dave.is_ready());
    }

    #[test]
    fn epoch_one_regenerates_key_package_and_transitions_ack() {
        let mut dave = Dave::new(1, 2);
        dave.start(1).unwrap();
        let out = dave.on_json(op::PREPARE_EPOCH, &json!({ "epoch": 1, "protocol_version": 1 })).unwrap();
        assert!(matches!(out.as_slice(), [Outgoing::Binary(f)] if f[0] == op::KEY_PACKAGE));
        let out = dave.on_json(op::PREPARE_TRANSITION, &json!({ "transition_id": 5, "protocol_version": 1 })).unwrap();
        assert_eq!(out, vec![Outgoing::Json(json!({ "op": 23, "d": { "transition_id": 5 } }))]);
        assert!(dave.on_json(op::EXECUTE_TRANSITION, &json!({ "transition_id": 5 })).unwrap().is_empty());
    }

    /// The voice gateway's role: MLS external sender proposing adds.
    /// Adapted from davey's own tests (ported from libdave).
    mod gateway {
        use openmls::prelude::tls_codec::{Deserialize, Serialize};
        use openmls::prelude::*;
        use openmls_basic_credential::SignatureKeyPair;
        use openmls_rust_crypto::OpenMlsRustCrypto;

        pub struct ExternalSenderSim {
            provider: OpenMlsRustCrypto,
            group_id: GroupId,
            signer: SignatureKeyPair,
            sender: ExternalSender,
        }

        impl ExternalSenderSim {
            pub fn new(channel_id: u64) -> Self {
                let suite = Ciphersuite::MLS_128_DHKEMP256_AES128GCM_SHA256_P256;
                let signer = SignatureKeyPair::new(suite.signature_algorithm()).unwrap();
                let sender = ExternalSender::new(signer.public().into(), BasicCredential::new(vec![0, 1, 1, 0]).into());
                Self {
                    provider: OpenMlsRustCrypto::default(),
                    group_id: GroupId::from_slice(&channel_id.to_be_bytes()),
                    signer,
                    sender,
                }
            }

            pub fn package(&self) -> Vec<u8> {
                self.sender.tls_serialize_detached().unwrap()
            }

            /// Op 27 payload (append) adding the owner of `key_package`.
            pub fn propose_add(&self, key_package: &[u8]) -> Vec<u8> {
                let key_package = KeyPackageIn::tls_deserialize_exact_bytes(key_package)
                    .unwrap()
                    .validate(self.provider.crypto(), ProtocolVersion::Mls10)
                    .unwrap();
                let message = ExternalProposal::new_add::<OpenMlsRustCrypto>(
                    key_package,
                    self.group_id.clone(),
                    GroupEpoch::from(0),
                    &self.signer,
                    SenderExtensionIndex::new(0),
                )
                .unwrap();
                let proposals = VLBytes::new(message.tls_serialize_detached().unwrap()).tls_serialize_detached().unwrap();
                [&[0u8][..], &proposals].concat()
            }

            /// Splits our op 28 payload back into commit and welcome, as the gateway must.
            pub fn split_commit_welcome(payload: &[u8]) -> (Vec<u8>, Vec<u8>) {
                let mut rest = payload;
                MlsMessageIn::tls_deserialize(&mut rest).expect("commit is a self-delimiting MLS message");
                let commit_len = payload.len() - rest.len();
                (payload[..commit_len].to_vec(), rest.to_vec())
            }
        }
    }

    fn binary(out: Vec<Outgoing>, opcode: u8) -> Vec<u8> {
        match out.as_slice() {
            [Outgoing::Binary(frame)] if frame[0] == opcode => frame[1..].to_vec(),
            other => panic!("expected one op {opcode} frame, got {other:?}"),
        }
    }

    fn server(transition: u16, body: &[u8]) -> Vec<u8> {
        [&transition.to_be_bytes()[..], body].concat()
    }

    /// Two clients through our state machine: key packages, proposal, commit and
    /// welcome, transition, then end-to-end encrypted audio between them.
    #[test]
    fn two_members_agree_on_keys_and_exchange_encrypted_audio() {
        const CHANNEL: u64 = 1234567890;
        const A: u64 = 1111111111111111;
        const B: u64 = 2222222222222222;
        let gateway = gateway::ExternalSenderSim::new(CHANNEL);
        let mut a = Dave::new(A, CHANNEL);
        let mut b = Dave::new(B, CHANNEL);
        a.users_connected([B]);
        b.users_connected([A]);

        binary(a.start(1).unwrap(), op::KEY_PACKAGE);
        let key_package_b = binary(b.start(1).unwrap(), op::KEY_PACKAGE);
        for member in [&mut a, &mut b] {
            assert!(member.on_binary(op::EXTERNAL_SENDER, &gateway.package()).unwrap().is_empty());
        }

        // The gateway proposes adding B; A commits and welcomes B.
        let commit_welcome = binary(a.on_binary(op::PROPOSALS, &gateway.propose_add(&key_package_b)).unwrap(), op::COMMIT_WELCOME);
        let (commit, welcome) = gateway::ExternalSenderSim::split_commit_welcome(&commit_welcome);
        assert!(!welcome.is_empty(), "adding a member needs a welcome");

        let ready = Outgoing::Json(json!({ "op": 23, "d": { "transition_id": 7 } }));
        assert_eq!(a.on_binary(op::ANNOUNCE_COMMIT, &server(7, &commit)).unwrap(), vec![ready]);
        let ready = Outgoing::Json(json!({ "op": 23, "d": { "transition_id": 7 } }));
        assert_eq!(b.on_binary(op::WELCOME, &server(7, &welcome)).unwrap(), vec![ready]);
        for member in [&mut a, &mut b] {
            member.on_json(op::EXECUTE_TRANSITION, &json!({ "transition_id": 7 })).unwrap();
            assert!(member.is_ready());
        }
        assert_eq!(a.privacy_code(), b.privacy_code(), "both see the same privacy code");
        assert!(a.privacy_code().is_some());

        let opus = b"\x78not really opus but any frame";
        let sealed = a.encrypt(opus).unwrap().expect("keys ready");
        assert_ne!(sealed.as_slice(), &opus[..], "frame is encrypted");
        assert_eq!(&sealed[sealed.len() - 2..], &[0xFA, 0xFA], "DAVE magic marker");
        assert_eq!(b.decrypt(A, &sealed).unwrap(), opus);
        // Silence passes through unencrypted, as the protocol specifies.
        assert_eq!(a.encrypt(&crate::voice::rtp::SILENCE_FRAME).unwrap().unwrap(), crate::voice::rtp::SILENCE_FRAME);

        // Video: an H.264 frame (4-byte start codes, as WebRTC rebuilds them).
        let h264 = [0, 0, 0, 1, 0x65, 0x88, 0x84, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let session = a.session.as_mut().unwrap();
        let sealed = session.encrypt(davey::MediaType::VIDEO, davey::Codec::H264, &h264).unwrap().into_owned();
        assert_ne!(sealed.as_slice(), &h264[..]);
        assert_eq!(b.decrypt_video(A, &sealed).unwrap(), h264);
    }

    #[test]
    fn invalid_welcome_is_reported_once_with_new_key_package() {
        let mut dave = Dave::new(1, 2);
        dave.start(1).unwrap();
        let out = dave.on_binary(op::WELCOME, &[0, 3, 0xde, 0xad]).unwrap();
        assert_eq!(out[0], Outgoing::Json(json!({ "op": 31, "d": { "transition_id": 3 } })));
        assert!(matches!(&out[1], Outgoing::Binary(f) if f[0] == op::KEY_PACKAGE));
        assert!(dave.on_binary(op::WELCOME, &[0, 4, 0xde, 0xad]).unwrap().is_empty(), "no repeat while recovering");
    }
}
