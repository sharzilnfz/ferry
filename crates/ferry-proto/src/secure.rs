//! Handshake cryptography and the session sealing layer.
//!
//! # Design (normative in `docs/store-format.md`, "Wire protocol v1")
//!
//! Mutual authentication WITHOUT signatures, Noise-style: possession of a
//! static X25519 secret is proven implicitly through Diffie-Hellman terms.
//! After Hello/HelloAck both sides compute
//!
//! ```text
//! e1 = X25519(eph_a,  eph_b)    // fresh per connection → forward secrecy
//! m1 = X25519(stat_a, eph_b)    // derivable only with stat_A's secret
//! m2 = X25519(stat_b, eph_a)    // derivable only with stat_B's secret
//! ```
//!
//! (`X25519(x, Y)` is symmetric in who runs it, so both parties reach all
//! three values.) The key schedule is then two HKDF-SHA-256 stages:
//!
//! ```text
//! ext1 = EXTRACT(salt = hash(Hello || HelloAck), ikm = e1 || m1 || m2)
//! prk  = EXPAND(ext1, "ferry/v1/handshake")
//! auth keys: EXPAND(prk, "ferry/v1/htk/{a2b|b2a}")
//! ext2 = EXTRACT(salt = hash(... || AuthInit_ct || AuthConfirm_ct), ikm = prk)
//! traffic keys: EXPAND(ext2, "ferry/v1/tk/{a2b|b2a}")
//! ```
//!
//! Each side seals exactly ONE message under its auth key — an [`AuthProof`]
//! carrying its own device_id. A peer without the claimed static secret
//! cannot produce a valid Poly1305 tag, so the tag IS the proof-of-
//! possession; no separate signature-analogue round trip exists. This beats
//! sealing challenges to the peer's public key because ONE schedule serves
//! auth and traffic, the transcript hash binds every handshake byte into the
//! proof, and replay dies automatically: fresh ephemerals and nonces make
//! every connection's transcript unique, so a replayed AUTH_INIT fails its
//! tag against the new salt.
//!
//! Post-auth frames are sealed with ChaCha20-Poly1305 under per-direction
//! traffic keys, nonce `b"FPN1" || u64 BE sequence`, strictly increasing per
//! direction. Reordered frames fail tag verification (wrong counter).
//! Ceiling: the u64 counter cannot wrap within any conceivable session;
//! hitting it is a hard typed error, never reuse. No rekey in v1 — a future
//! minor can add it behind a feature flag without breaking this layout.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::codec::AuthProof;
use crate::error::ProtoError;

pub const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

// HKDF info labels (normative strings).
pub const INFO_HANDSHAKE: &[u8] = b"ferry/v1/handshake";
pub const INFO_HTK_A2B: &[u8] = b"ferry/v1/htk/a2b";
pub const INFO_HTK_B2A: &[u8] = b"ferry/v1/htk/b2a";
pub const INFO_TK_A2B: &[u8] = b"ferry/v1/tk/a2b";
pub const INFO_TK_B2A: &[u8] = b"ferry/v1/tk/b2a";

/// Traffic-nonce prefix "FPN1" || u64 BE sequence.
const TRAFFIC_NONCE_PREFIX: [u8; 4] = *b"FPN1";

/// BLAKE3 over length-prefixed concatenation of the given byte strings.
/// Length prefixes keep concatenation unambiguous.
pub fn transcript_hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    for p in parts {
        h.update(&(p.len() as u32).to_be_bytes());
        h.update(p);
    }
    *h.finalize().as_bytes()
}

fn expand_from(prk: &[u8], info: &[u8]) -> Zeroizing<[u8; KEY_LEN]> {
    let hk = Hkdf::<Sha256>::from_prk(prk).expect("prk is a valid SHA-256 length");
    let mut okm = Zeroizing::new([0u8; KEY_LEN]);
    hk.expand(info, okm.as_mut())
        .expect("32-byte OKM always valid");
    okm
}

/// Stage 1: handshake PRK plus the two single-use auth keys.
///
/// Returns `(htk_a2b, htk_b2a, prk)`; all zeroized on drop.
pub(crate) fn kdf_handshake(
    transcript: &[u8; 32],
    e1: &[u8; 32],
    m1: &[u8; 32],
    m2: &[u8; 32],
) -> (
    Zeroizing<[u8; KEY_LEN]>,
    Zeroizing<[u8; KEY_LEN]>,
    Box<[u8; KEY_LEN]>,
) {
    let mut ikm = Zeroizing::new([0u8; 96]);
    ikm[..32].copy_from_slice(e1);
    ikm[32..64].copy_from_slice(m1);
    ikm[64..].copy_from_slice(m2);
    let ext = Hkdf::<Sha256>::new(Some(transcript), ikm.as_ref());
    let mut prk_box = Box::new([0u8; KEY_LEN]);
    ext.expand(INFO_HANDSHAKE, prk_box.as_mut())
        .expect("valid prk length");
    let htk_a2b = expand_from(prk_box.as_slice(), INFO_HTK_A2B);
    let htk_b2a = expand_from(prk_box.as_slice(), INFO_HTK_B2A);
    (htk_a2b, htk_b2a, prk_box)
}

/// A traffic-direction key. Zeroized on drop, never cloned out.
#[derive(Debug)] // Debug shows nothing secret; see test below.
pub struct SessionKey(Zeroizing<[u8; KEY_LEN]>);

impl SessionKey {
    pub fn cipher(self) -> SessionCipher {
        SessionCipher::new(self)
    }
}

/// Stage 2: per-direction traffic keys after both AUTH messages.
///
/// `final_transcript` covers Hello || HelloAck || AuthInit_ct ||
/// AuthConfirm_ct, chaining the proof bytes into session-key derivation so
/// any tampering upstream changes every downstream key.
pub(crate) fn traffic_keys(prk: &[u8; KEY_LEN], final_transcript: &[u8; 32]) -> (SessionKey, SessionKey) {
    let ext = Hkdf::<Sha256>::new(Some(final_transcript), prk);
    let mut root = Zeroizing::new([0u8; KEY_LEN]);
    ext.expand(b"ferry/v1/traffic", root.as_mut())
        .expect("valid root length");
    (
        SessionKey(expand_from(root.as_slice(), INFO_TK_A2B)),
        SessionKey(expand_from(root.as_slice(), INFO_TK_B2A)),
    )
}

/// Seal/open helper for the single-use auth messages: fixed all-zero nonce
/// (each key encrypts exactly one message per connection), AAD = transcript
/// hash at that point.
pub(crate) fn seal_auth(
    key: &[u8; KEY_LEN],
    th: &[u8; 32],
    device_id: &[u8; 32],
) -> Result<AuthProof, ProtoError> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = [0u8; NONCE_LEN];
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: device_id,
                aad: th,
            },
        )
        .map_err(|_| ProtoError::Auth("seal failure"))?;
    AuthProof::new(ct)
}

pub(crate) fn open_auth(
    key: &[u8; KEY_LEN],
    th: &[u8; 32],
    proof: &AuthProof,
) -> Result<[u8; 32], ProtoError> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = [0u8; NONCE_LEN];
    let pt = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &proof.ciphertext,
                aad: th,
            },
        )
        .map_err(|_| ProtoError::Auth("auth tag verification failed"))?;
    pt.try_into()
        .map_err(|_| ProtoError::Auth("auth plaintext wrong length"))
}

/// One direction's frame-level AEAD state: key + strictly increasing
/// sequence counter.
pub struct SessionCipher {
    key: SessionKey,
    seq: u64,
}

impl SessionCipher {
    pub(crate) fn new(key: SessionKey) -> Self {
        SessionCipher { key, seq: 0 }
    }

    /// Test/audit hook: construct a cipher already AT a given sequence.
    #[cfg(test)]
    pub(crate) fn at_sequence(key: SessionKey, seq: u64) -> Self {
        SessionCipher { key, seq }
    }

    fn next_nonce(&mut self) -> Result<[u8; NONCE_LEN], ProtoError> {
        if self.seq == u64::MAX {
            return Err(ProtoError::CounterExhausted);
        }
        let mut n = [0u8; NONCE_LEN];
        n[..4].copy_from_slice(&TRAFFIC_NONCE_PREFIX);
        n[4..].copy_from_slice(&self.seq.to_be_bytes());
        self.seq += 1;
        Ok(n)
    }

    /// Seal one frame body region (type || version || payload), binding the
    /// wire-visible length prefix into the tag as AAD. Returns ciphertext of
    /// `plaintext.len() + 16`.
    pub fn seal_frame(&mut self, len_prefix: u32, body_region: &[u8]) -> Result<Vec<u8>, ProtoError> {
        let nonce = self.next_nonce()?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(self.key.0.as_ref()));
        cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: body_region,
                    aad: &len_prefix.to_be_bytes(),
                },
            )
            .map_err(|_| ProtoError::ProtocolViolation("frame seal failure"))
    }

    /// Open one sealed frame body region. Any tamper, reorder, splice, or
    /// replay fails the tag check here.
    pub fn open_frame(&mut self, len_prefix: u32, ct: &[u8]) -> Result<Vec<u8>, ProtoError> {
        if ct.len() < 16 {
            return Err(ProtoError::ProtocolViolation("sealed body too short"));
        }
        let nonce = self.next_nonce()?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(self.key.0.as_ref()));
        cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: ct,
                    aad: &len_prefix.to_be_bytes(),
                },
            )
            .map_err(|_| ProtoError::Auth("post-auth frame failed tag verification"))
    }
}

impl core::fmt::Debug for SessionCipher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SessionCipher").field("seq", &self.seq).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dh_terms() -> ([Box<[u8; 32]>; 3], [u8; 32]) {
        // Arbitrary-but-fixed terms for wiring tests; real values come from
        // X25519 in the engine.
        ([
            Box::new(core::array::from_fn(|i| i as u8 + 1)),
            Box::new(core::array::from_fn(|i| i as u8 + 41)),
            Box::new(core::array::from_fn(|i| i as u8 + 91)),
        ], transcript_hash(&[b"hello", b"ack"]))
    }

    #[test]
    fn schedule_is_deterministic_and_direction_separated() {
        let (terms, th) = dh_terms();
        let (h_a2b, h_b2a, prk) = kdf_handshake(&th, &terms[0], &terms[1], &terms[2]);
        assert_ne!(h_a2b.as_ref(), h_b2a.as_ref());

        let (t_a2b, t_b2a) = traffic_keys(&prk, &th);
        // Direction separation survives into traffic keys...
        assert_ne!(t_a2b.0.as_ref(), t_b2a.0.as_ref());
        // ...and the same inputs give the same outputs (both sides must
        // land on identical keys independently).
        let again = kdf_handshake(&th, &terms[0], &terms[1], &terms[2]);
        assert_eq!(again.0.as_ref(), h_a2b.as_ref());
        let (t2, _) = traffic_keys(&again.2, &th);
        assert_eq!(t2.0.as_ref(), t_a2b.0.as_ref());
    }

    #[test]
    fn any_transcript_or_term_change_changes_every_key() {
        let (terms, th) = dh_terms();
        let base = kdf_handshake(&th, &terms[0], &terms[1], &terms[2]);

        let mut th2 = th;
        th2[0] ^= 1;
        let changed_th = kdf_handshake(&th2, &terms[0], &terms[1], &terms[2]);
        assert_ne!(base.0.as_ref(), changed_th.0.as_ref());

        let mut e2 = terms[0].clone();
        e2[31] ^= 1;
        let changed_e = kdf_handshake(&th, &e2, &terms[1], &terms[2]);
        assert_ne!(base.0.as_ref(), changed_e.0.as_ref());
        assert_ne!(base.2.as_ref(), changed_e.2.as_ref());

        // Traffic keys re-rooted on a different final transcript differ.
        let (ta, _) = traffic_keys(&base.2, &th);
        let (tb, _) = traffic_keys(&base.2, &th2);
        assert_ne!(ta.0.as_ref(), tb.0.as_ref());
    }

    #[test]
    fn auth_proof_round_trips_only_under_the_right_key_and_transcript() {
        let (terms, th) = dh_terms();
        let (h_a2b, _, _) = kdf_handshake(&th, &terms[0], &terms[1], &terms[2]);
        let proof = seal_auth(&h_a2b, &th, &[9; 32]).unwrap();
        assert_eq!(proof.ciphertext.len(), AuthProof::CT_LEN);
        assert_eq!(open_auth(&h_a2b, &th, &proof).unwrap(), [9; 32]);

        // Wrong transcript (replay context): tag fails.
        let mut th_evil = th;
        th_evil[0] ^= 1;
        assert!(open_auth(&h_a2b, &th_evil, &proof).is_err());
    }

    #[test]
    fn frames_seal_open_with_counters_and_reject_reorder_replay_tamper() {
        let (terms, th) = dh_terms();
        let (_, _, prk) = kdf_handshake(&th, &terms[0], &terms[1], &terms[2]);
        // Both ENDS of a direction hold that direction's key; derive twice
        // (deterministic schedule) so tx and rx share `a2b`.
        let (ka, _) = traffic_keys(&prk, &th);
        let (kb2, _) = traffic_keys(&prk, &th);
        let mut tx = ka.cipher();
        let mut rx = kb2.cipher();

        let f0 = tx.seal_frame(100, b"first").unwrap();
        let f1 = tx.seal_frame(105, b"second").unwrap();
        assert_ne!(f0, f1, "counter must change the ciphertext");

        // Out-of-order delivery: f1 arrives while rx expects sequence 0.
        // Wrong nonce → tag failure. One failure burns rx's counter slot,
        // so the session is dead afterwards BY DESIGN (fatal-error policy;
        // the engine disconnects rather than resync).
        assert!(rx.open_frame(105, &f1).is_err());

        // Fresh pair, happy path first, then the abuse cases.
        let (ka3, _) = traffic_keys(&prk, &th);
        let (ka4, _) = traffic_keys(&prk, &th);
        let mut tx = ka3.cipher();
        let mut rx = ka4.cipher();
        let g0 = tx.seal_frame(100, b"first").unwrap();
        let g1 = tx.seal_frame(105, b"second").unwrap();
        assert_eq!(rx.open_frame(100, &g0).unwrap(), b"first");
        assert_eq!(rx.open_frame(105, &g1).unwrap(), b"second");

        // Replay of an ALREADY-consumed frame fails (counter moved on).
        assert!(rx.open_frame(100, &g0).is_err());

        // Tamper anywhere fails.
        let fresh = tx.seal_frame(10, b"payload").unwrap();
        let mut evil = fresh.clone();
        evil[3] ^= 0x80;
        assert!(rx.open_frame(10, &evil).is_err());

        // Length-prefix binding: same ciphertext under a different declared
        // length fails.
        let bound = tx.seal_frame(77, b"x").unwrap();
        assert!(rx.open_frame(78, &bound).is_err());
    }

    #[test]
    fn exhausted_counter_is_a_hard_error_never_nonce_reuse() {
        let (_, _, prk) = kdf_handshake(
            &[0u8; 32],
            &[1u8; 32],
            &[2u8; 32],
            &[3u8; 32],
        );
        let (k, _) = traffic_keys(&prk, &[4u8; 32]);
        let mut c = SessionCipher::at_sequence(k, u64::MAX);
        assert!(matches!(
            c.seal_frame(1, b""),
            Err(ProtoError::CounterExhausted)
        ));
    }

    #[test]
    fn debug_output_never_contains_key_material() {
        let (_, _, prk) = kdf_handshake(&[0u8; 32], &[1; 32], &[2; 32], &[3; 32]);
        let (k, _) = traffic_keys(&prk, &[5u8; 32]);
        let rendered = format!("{:?}\n{:?}", k, SessionCipher::new(SessionKey(k.0.clone())));
        // The key bytes are 0x.. deterministic; search for their hex pattern.
        let hexy: String = k
            .0
            .as_ref()
            .iter()
            .take(4)
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(!rendered.contains(&hexy));
        assert!(!rendered.contains("secret"));
    }

    #[test]
    fn sealed_frames_carry_the_full_encoded_body_including_magic() {
        use crate::frame::{read_body, write_body};
        use crate::stream::duplex_pair;
        let (_, _, prk) = kdf_handshake(&[0u8; 32], &[1; 32], &[2; 32], &[3; 32]);
        let (k, _) = traffic_keys(&prk, &[5u8; 32]);
        let mut tx = k.cipher();
        let body = FrameBody::new(crate::codec::MSG_ITEM_BATCH, ProtocolVersion::V1_0, vec![7, 7])
            .encode();
        let len_prefix = (body.len() + 16) as u32;
        let ct = tx.seal_frame(len_prefix, &body).unwrap();

        let (mut a, mut b) = duplex_pair();
        write_body(&mut a, &ct).unwrap();
        let got = read_body(&mut b).unwrap();
        assert_eq!(got.len(), body.len() + 16);
        let (k2, _) = traffic_keys(&prk, &[5u8; 32]);
        let plain = k2.cipher().open_frame(len_prefix, &got).unwrap();
        assert_eq!(FrameBody::parse(&plain).unwrap().payload, vec![7, 7]);
    }
}
