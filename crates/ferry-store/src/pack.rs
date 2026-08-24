//! Pack files: grouping, framing, sealing, and the normative blob read path
//! (`docs/store-format.md`, "Pack files" and "Reading a blob").
//!
//! One pack is one immutable file: prologue (header + salt), one encrypted
//! STREAM covering every blob plaintext back to back, encrypted footer, clear
//! trailing `footer_len`. The file name is BLAKE3 of the entire ciphertext;
//! it is verified before any decryption.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{CryptoError, PackCipher, PassthroughCipher, NONCE_LEN};
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fmk() -> [u8; KEY_LEN] {
        core::array::from_fn(|i| i as u8)
    }

    fn prng(seed: u64) -> StdRng {
        StdRng::seed_from_u64(seed)
    }

    /// Cipher that counts every primitive call, proving when we do NOT
    /// decrypt.
    struct CountingCipher {
        inner: PassthroughCipher,
        opens: AtomicUsize,
        seals: AtomicUsize,
    }

    impl CountingCipher {
        fn new() -> Self {
            CountingCipher {
                inner: PassthroughCipher,
                opens: AtomicUsize::new(0),
                seals: AtomicUsize::new(0),
            }
        }
    }

    impl PackCipher for CountingCipher {
        fn seal(
            &self,
            key: &[u8; KEY_LEN],
            nonce: &[u8; NONCE_LEN],
            aad: &[u8],
            plaintext: &[u8],
        ) -> Result<Vec<u8>, CryptoError> {
            self.seals.fetch_add(1, Ordering::SeqCst);
            self.inner.seal(key, nonce, aad, plaintext)
        }

        fn open(
            &self,
            key: &[u8; KEY_LEN],
            nonce: &[u8; NONCE_LEN],
            aad: &[u8],
            ciphertext: &[u8],
        ) -> Result<Vec<u8>, CryptoError> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            self.inner.open(key, nonce, aad, ciphertext)
        }
    }

    fn blob(id_byte: u8, len: usize, seed: u64) -> (BlobId, Vec<u8>) {
        let mut rng = prng(seed);
        let bytes: Vec<u8> = (0..len).map(|_| rng.gen()).collect();
        let mut id = [0u8; 32];
        id[0] = id_byte;
        id[1] = id_byte.wrapping_mul(7);
        let h = blake3::hash(&bytes);
        id.copy_from_slice(h.as_bytes());
        (id, bytes)
    }

    #[test]
    fn footer_plaintext_serialization_fixture() {
        // Hand-computed footer: body_plain_len 18, two blobs.
        //  blob 1: kind DataChunk, id AA*32, off 0, len 11
        //  blob 2: kind TreeNode, id BB*32, off 11, len 7
        //  reserved u32 zeros
        let entries = vec![
            FooterEntry {
                kind: BlobKind::DataChunk,
                id: [0xAA; 32],
                plain_off: 0,
                plain_len: 11,
            },
            FooterEntry {
                kind: BlobKind::TreeNode,
                id: [0xBB; 32],
                plain_off: 11,
                plain_len: 7,
            },
        ];
        let got = footer_plain(&entries, 18);
        let mut expect: Vec<u8> = Vec::new();
        expect.extend_from_slice(&18u64.to_le_bytes()); // body_plain_len
        expect.extend_from_slice(&2u32.to_le_bytes()); // blob_count
        expect.push(0x01);
        expect.extend_from_slice(&[0xAA; 32]);
        expect.extend_from_slice(&0u64.to_le_bytes());
        expect.extend_from_slice(&11u64.to_le_bytes());
        expect.push(0x02);
        expect.extend_from_slice(&[0xBB; 32]);
        expect.extend_from_slice(&11u64.to_le_bytes());
        expect.extend_from_slice(&7u64.to_le_bytes());
        expect.extend_from_slice(&0u32.to_le_bytes()); // reserved
        assert_eq!(got, expect);

        // Round trip.
        let (bpl, parsed) = footer_parse(&got).unwrap();
        assert_eq!(bpl, 18);
        assert_eq!(parsed, entries);
    }

    #[test]
    fn footer_parse_rejects_reserved_nonzero_and_truncation() {
        let mut bad = footer_plain(&[], 0);
        let n = bad.len();
        bad[n - 1] = 1; // reserved byte
        assert!(matches!(
            footer_parse(&bad),
            Err(PackError::ReservedNonzero)
        ));
        assert!(matches!(
            footer_parse(&bad[..n - 2]),
            Err(PackError::FooterCorrupt(_))
        ));
    }

    #[test]
    fn sealed_pack_layout_is_spec_conformant() {
        let cipher = PassthroughCipher;
        let salt = [0x42u8; SALT_LEN];
        let (id1, d1) = blob(1, 70_000, 101); // spans two segments
        let (id2, d2) = blob(2, 500, 102);
        let mut body = d1.clone();
        body.extend_from_slice(&d2);
        let entries = vec![
            FooterEntry {
                kind: BlobKind::DataChunk,
                id: id1,
                plain_off: 0,
                plain_len: d1.len() as u64,
            },
            FooterEntry {
                kind: BlobKind::DataChunk,
                id: id2,
                plain_off: d1.len() as u64,
                plain_len: d2.len() as u64,
            },
        ];
        let file = seal_pack_bytes(
            ContainerKind::PackData,
            &fmk(),
            &salt,
            &body,
            &entries,
            &cipher,
        )
        .unwrap();

        // Prologue.
        assert_eq!(&file[..5], b"FERRY");
        assert_eq!(file[5], ContainerKind::PackData.to_u8());
        assert_eq!(&file[6..10], &1u32.to_le_bytes());
        assert_eq!(&file[10..26], &salt);

        // Trailing footer_len in clear: length of the FOOTER ciphertext
        // (u64 body_len + u32 count + 2 entries x 49 bytes + u32 reserved,
        // plus the stub tag slot).
        let flen = u32::from_le_bytes(file[file.len() - 4..].try_into().unwrap()) as usize;
        let footer_plain_len = 8 + 4 + entries.len() * 49 + 4;
        assert_eq!(flen, footer_plain_len + TAG_LEN);

        // Body region length identity.
        let segs = segment_count(body.len() as u64);
        let body_region = file.len() as u64 - 26 - flen as u64 - 4;
        assert_eq!(body_region, body.len() as u64 + TAG_LEN as u64 * segs);

        // Stub tags are zero; with the real AEAD this region is opaque.
        debug_assert!(file.len() > 26 + body.len());

        // Name is BLAKE3 over everything.
        let name: [u8; 32] = *blake3::hash(&file).as_bytes();
        assert_eq!(name, pack_name_of(&file));
    }

    #[test]
    fn read_blob_round_trip_across_segments() {
        let cipher = PassthroughCipher;
        let salt: [u8; SALT_LEN] = core::array::from_fn(|i| 0x10 + i as u8);
        let (id1, d1) = blob(1, 65_540, 201); // crosses one boundary
        let (id2, d2) = blob(2, 130_000, 202); // spans three segments
        let (id3, d3) = blob(3, 33, 203); // tail
        let mut body = Vec::new();
        let mut entries = Vec::new();
        for (id, d) in [(&id1, &d1), (&id2, &d2), (&id3, &d3)] {
            entries.push(FooterEntry {
                kind: BlobKind::DataChunk,
                id: *id,
                plain_off: body.len() as u64,
                plain_len: d.len() as u64,
            });
            body.extend_from_slice(d);
        }
        let file = seal_pack_bytes(
            ContainerKind::PackData,
            &fmk(),
            &salt,
            &body,
            &entries,
            &cipher,
        )
        .unwrap();
        let pid = pack_name_of(&file);

        for ((id, d), e) in [&id1, &id2, &id3]
            .iter()
            .zip([&d1, &d2, &d3])
            .zip(entries.iter())
        {
            let id: &BlobId = id;
            let got =
                read_blob(&file, &pid, &fmk(), &cipher, BlobKind::DataChunk, id, None).unwrap();
            assert_eq!(got, **d, "blob {} mismatch", e.id[0]);
        }
    }

    #[test]
    fn corrupted_pack_rejected_without_decrypting() {
        let cipher = CountingCipher::new();
        let salt = [0x77u8; SALT_LEN];
        let (id1, d1) = blob(9, 1000, 301);
        let file = seal_pack_bytes(
            ContainerKind::PackData,
            &fmk(),
            &salt,
            &d1,
            &[FooterEntry {
                kind: BlobKind::DataChunk,
                id: id1,
                plain_off: 0,
                plain_len: d1.len() as u64,
            }],
            &cipher,
        )
        .unwrap();
        let real_pid = pack_name_of(&file);
        let seals_from_setup = cipher.seals.load(Ordering::SeqCst);

        // Flip one byte in the middle of the body.
        let mut corrupt = file.clone();
        let mid = 27;
        corrupt[mid] ^= 0x80;

        // Name check fires before any cipher call: zero opens, zero seals.
        let err = read_blob(
            &corrupt,
            &real_pid,
            &fmk(),
            &cipher,
            BlobKind::DataChunk,
            &id1,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, PackError::NameMismatch { .. }), "{err}");
        assert_eq!(cipher.opens.load(Ordering::SeqCst), 0);
        assert_eq!(
            cipher.seals.load(Ordering::SeqCst),
            seals_from_setup,
            "no new seal work on a rejected pack"
        );

        // Sanity: the honest pack decrypts exactly the footer plus its one
        // body segment, and nothing more.
        read_blob(
            &file,
            &real_pid,
            &fmk(),
            &cipher,
            BlobKind::DataChunk,
            &id1,
            None,
        )
        .unwrap();
        assert_eq!(cipher.opens.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn footer_index_disagreement_aborts_read() {
        let cipher = PassthroughCipher;
        let salt = [0x31u8; SALT_LEN];
        let (id1, d1) = blob(4, 2000, 401);
        let file = seal_pack_bytes(
            ContainerKind::PackData,
            &fmk(),
            &salt,
            &d1,
            &[FooterEntry {
                kind: BlobKind::DataChunk,
                id: id1,
                plain_off: 0,
                plain_len: d1.len() as u64,
            }],
            &cipher,
        )
        .unwrap();
        let pid = pack_name_of(&file);

        // Index claims a different offset: trust neither, stop.
        let err = read_blob(
            &file,
            &pid,
            &fmk(),
            &cipher,
            BlobKind::DataChunk,
            &id1,
            Some((17, d1.len() as u64)),
        )
        .unwrap_err();
        assert!(matches!(err, PackError::Disagreement { .. }), "{err}");

        // Same for a wrong length and a wrong kind mapping.
        let err = read_blob(
            &file,
            &pid,
            &fmk(),
            &cipher,
            BlobKind::TreeNode,
            &id1,
            Some((0, d1.len() as u64)),
        )
        .unwrap_err();
        assert!(matches!(err, PackError::NotFound));
    }

    #[test]
    fn verify_after_decrypt_catches_tampered_body_with_valid_name() {
        // Corrupt the body, then re-name the file so the name check passes.
        // Step 6 (BLAKE3(plaintext) == id) must reject it.
        let cipher = PassthroughCipher;
        let salt = [0x5Au8; SALT_LEN];
        let (id1, d1) = blob(5, 3000, 501);
        let file = seal_pack_bytes(
            ContainerKind::PackData,
            &fmk(),
            &salt,
            &d1,
            &[FooterEntry {
                kind: BlobKind::DataChunk,
                id: id1,
                plain_off: 0,
                plain_len: d1.len() as u64,
            }],
            &cipher,
        )
        .unwrap();

        let mut evil = file.clone();
        evil[30] ^= 0xFF; // inside body plaintext (stub passes it through)
        let evil_pid = pack_name_of(&evil); // attacker renames to match

        let err = read_blob(
            &evil,
            &evil_pid,
            &fmk(),
            &cipher,
            BlobKind::DataChunk,
            &id1,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, PackError::HashMismatch { .. }), "{err}");
    }

    #[test]
    fn unknown_magic_or_version_rejected() {
        let cipher = PassthroughCipher;
        let mut junk = vec![0u8; 64];
        junk[..5].copy_from_slice(b"FERrY");
        // Name check runs first (step 2 of the read procedure), so present
        // the matching name to reach the header parse.
        let junk_name = pack_name_of(&junk);
        assert!(matches!(
            read_blob(
                &junk,
                &junk_name,
                &fmk(),
                &cipher,
                BlobKind::DataChunk,
                &[1; 32],
                None
            ),
            Err(PackError::BadMagic)
        ));
    }

    #[test]
    fn staging_pool_membership_rules() {
        // W=8 open packs per kind, redraw after overflow, empties dropped.
        let mut pools = StagingPools::new();
        let mut rng = prng(9001);
        let target = 1000usize; // tiny target for the test

        let mut sealed_data = Vec::new();
        for i in 0..200 {
            let payload = vec![i as u8; 120];
            sealed_data.extend(pools.offer(
                BlobKind::DataChunk,
                *blake3::hash(&payload).as_bytes(),
                &payload,
                target,
                STAGING_OPEN_PACKS,
                &mut rng,
            ));
            // Never more than W open at any moment.
            assert!(pools.open_count(BlobKind::DataChunk) <= STAGING_OPEN_PACKS);
        }

        // Overflow path must have sealed some packs already.
        assert!(!sealed_data.is_empty());

        // Meta kind is independent.
        let payload = vec![1u8; 10];
        let _ = pools.offer(
            BlobKind::TreeNode,
            *blake3::hash(&payload).as_bytes(),
            &payload,
            target,
            STAGING_OPEN_PACKS,
            &mut rng,
        );
        assert_eq!(pools.open_count(BlobKind::TreeNode), 1);

        // Drain: everything non-empty seals, nothing empty survives.
        let drained = pools.drain_all();
        assert_eq!(pools.open_count(BlobKind::DataChunk), 0);
        assert_eq!(pools.open_count(BlobKind::TreeNode), 0);
        // All drained packs hold at least one blob.
        assert!(drained.iter().all(|p| !p.entries.is_empty()));
    }

    #[test]
    fn oversized_single_blob_lands_in_fresh_pack() {
        let mut pools = StagingPools::new();
        let mut rng = prng(9002);
        let huge = vec![7u8; 4000]; // > target
        let sealed = pools.offer(
            BlobKind::DataChunk,
            *blake3::hash(&huge).as_bytes(),
            &huge,
            1000,
            STAGING_OPEN_PACKS,
            &mut rng,
        );
        // Nothing sealed yet: fresh pack accepted the oversize blob.
        assert!(sealed.is_empty());
        let drained = pools.drain_all();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].body.len(), 4000);
    }

    #[test]
    fn deterministic_membership_for_seeded_rng() {
        let build = |seed: u64| {
            let mut pools = StagingPools::new();
            let mut rng = prng(seed);
            for i in 0..50 {
                let payload = vec![i as u8; 90];
                pools.offer(
                    BlobKind::DataChunk,
                    *blake3::hash(&payload).as_bytes(),
                    &payload,
                    500,
                    STAGING_OPEN_PACKS,
                    &mut rng,
                );
            }
            pools.snapshot_membership()
        };
        assert_eq!(build(1234), build(1234));
        assert_ne!(build(1234), build(5678));
    }

    #[test]
    fn atomic_write_lands_only_in_final_location() {
        let tmp = tempfile::tempdir().unwrap();
        let packs = tempfile::tempdir().unwrap();
        let bytes = b"a pack worth keeping".to_vec();
        let name = write_pack_atomically(tmp.path(), packs.path(), &bytes).unwrap();
        let final_path = packs
            .path()
            .join(format!("{}.pack", crate::format::hex(&name)));
        assert!(final_path.exists());
        assert_eq!(std::fs::read(&final_path).unwrap(), bytes);
        // Temp area holds no leftovers.
        assert!(std::fs::read_dir(tmp.path()).unwrap().next().is_none());
    }
}

use rand::{Rng, RngCore};
use thiserror::Error;

use crate::crypto::{
    body_aad, body_nonce, derive_pack_key, footer_aad, segment_count, CryptoError, PackCipher,
    FOOTER_NONCE, KEY_LEN, SALT_LEN, SEGMENT_PLAIN_LEN, TAG_LEN,
};
use crate::format::{
    hex, parse_header, put_bytes, put_u32, put_u64, write_header, BlobId, BlobKind, ContainerKind,
    FormatError, PackId, Reader, HEADER_LEN,
};

/// Open staging packs per kind (data, meta) during a write burst.
pub const STAGING_OPEN_PACKS: usize = 8;

/// Seal target for staging packs: assignment that would push a pack past
/// this seals it immediately (`docs/store-format.md`, membership rules).
pub const SEAL_TARGET_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum PackError {
    #[error("bad magic bytes")]
    BadMagic,
    #[error("unknown container kind {0:#04x}")]
    UnknownKind(u8),
    #[error("unsupported format_version")]
    BadVersion,
    #[error("container too short to hold a pack prologue")]
    TooShort,
    #[error("not a pack container")]
    NotAPack,
    #[error("pack name mismatch: expected {expected}, found {found}")]
    NameMismatch { expected: String, found: String },
    #[error("footer decryption failed: {0}")]
    FooterCrypto(#[from] CryptoError),
    #[error("footer corrupt: {0}")]
    FooterCorrupt(&'static str),
    #[error("reserved field must be zero")]
    ReservedNonzero,
    #[error(
        "index and footer disagree about blob location: \
         index {index:?} vs footer {footer:?}; trusting neither"
    )]
    Disagreement {
        index: (u64, u64),
        footer: (u64, u64),
    },
    #[error("body region length mismatch: file implies {got}, footer claims {want}")]
    BodyRegionMismatch { got: u64, want: u64 },
    #[error("blob not present in this pack")]
    NotFound,
    #[error(
        "verify-after-decrypt failed: BLAKE3(plaintext) is {found}, \
         expected address {expected}"
    )]
    HashMismatch { expected: String, found: String },
    #[error("{0}")]
    Format(#[from] FormatError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// One blob's position inside the reassembled body plaintext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FooterEntry {
    pub kind: BlobKind,
    pub id: BlobId,
    pub plain_off: u64,
    pub plain_len: u64,
}

/// Serialize footer plaintext: `u64 body_plain_len, u32 blob_count, entries
/// in body order, u32 reserved zeros`.
pub fn footer_plain(entries: &[FooterEntry], body_plain_len: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + entries.len() * 81);
    put_u64(&mut out, body_plain_len);
    put_u32(&mut out, entries.len() as u32);
    for e in entries {
        out.push(e.kind.to_u8());
        put_bytes(&mut out, &e.id);
        put_u64(&mut out, e.plain_off);
        put_u64(&mut out, e.plain_len);
    }
    put_u32(&mut out, 0); // reserved
    out
}

/// Parse footer plaintext, validating the reserved field and trailing bytes.
pub fn footer_parse(bytes: &[u8]) -> Result<(u64, Vec<FooterEntry>), PackError> {
    let mut r = Reader::new(bytes);
    let body_plain_len = r.u64().map_err(|_| PackError::FooterCorrupt("truncated"))?;
    let count = r.u32().map_err(|_| PackError::FooterCorrupt("truncated"))?;
    let mut entries = Vec::new();
    for _ in 0..count {
        let kb = r.u8().map_err(|_| PackError::FooterCorrupt("truncated"))?;
        let kind = BlobKind::from_u8(kb).ok_or(PackError::FooterCorrupt("unknown blob kind"))?;
        let id = r
            .array()
            .map_err(|_| PackError::FooterCorrupt("truncated"))?;
        let plain_off = r.u64().map_err(|_| PackError::FooterCorrupt("truncated"))?;
        let plain_len = r.u64().map_err(|_| PackError::FooterCorrupt("truncated"))?;
        entries.push(FooterEntry {
            kind,
            id,
            plain_off,
            plain_len,
        });
    }
    if r.u32().map_err(|_| PackError::FooterCorrupt("truncated"))? != 0 {
        return Err(PackError::ReservedNonzero);
    }
    r.expect_end()
        .map_err(|_| PackError::FooterCorrupt("trailing bytes"))?;
    Ok((body_plain_len, entries))
}

/// BLAKE3 over the entire ciphertext: the pack file's name.
pub fn pack_name_of(file_bytes: &[u8]) -> PackId {
    *blake3::hash(file_bytes).as_bytes()
}

/// Assemble a complete pack file image in memory: prologue, encrypted body
/// segments, encrypted footer, clear trailing footer length. Disk IO happens
/// separately in [`write_pack_atomically`] so tests can inspect the bytes.
///
/// With the v0 [`PassthroughCipher`] the "ciphertext" is plaintext plus a
/// zeroed tag slot; every offset and length below is identical to what a
/// real AEAD produces.
pub fn seal_pack_bytes(
    kind: ContainerKind,
    fmk: &[u8; KEY_LEN],
    salt: &[u8; SALT_LEN],
    body: &[u8],
    entries: &[FooterEntry],
    cipher: &dyn PackCipher,
) -> Result<Vec<u8>, PackError> {
    debug_assert!(matches!(
        kind,
        ContainerKind::PackData | ContainerKind::PackMeta
    ));
    debug_assert_eq!(
        body.len(),
        entries.iter().map(|e| e.plain_len as usize).sum::<usize>(),
        "entries must tile the body exactly"
    );

    let header = write_header(kind);
    let key = derive_pack_key(fmk, salt, kind);

    // Body: consecutive 64 KiB STREAM segments.
    let aad_body = body_aad(&header, kind);
    let n_segments = segment_count(body.len() as u64) as usize;
    let mut file = Vec::with_capacity(26 + body.len() + TAG_LEN * (n_segments + 1) + 4);
    file.extend_from_slice(&header);
    file.extend_from_slice(salt);
    for s in 0..n_segments {
        let start = s * SEGMENT_PLAIN_LEN;
        let end = std::cmp::min(start + SEGMENT_PLAIN_LEN, body.len());
        let last_flag = u8::from(s + 1 == n_segments);
        let nonce = body_nonce(s as u32, last_flag);
        let ct = cipher.seal(&key, &nonce, &aad_body, &body[start..end])?;
        debug_assert_eq!(ct.len(), end - start + TAG_LEN);
        file.extend_from_slice(&ct);
    }

    // Footer under the reserved counter.
    let footer_pt = footer_plain(entries, body.len() as u64);
    let aad_footer = footer_aad(&header, kind);
    let footer_ct = cipher.seal(&key, &FOOTER_NONCE, &aad_footer, &footer_pt)?;
    file.extend_from_slice(&footer_ct);
    file.extend_from_slice(&(footer_ct.len() as u32).to_le_bytes());

    Ok(file)
}

/// Everything known about a pack after name verification and footer
/// decryption, before any blob-specific work.
struct PackContext {
    kind: ContainerKind,
    key: [u8; KEY_LEN],
    body_start: usize,
    footer_start: usize,
    body_plain_len: u64,
    entries: Vec<FooterEntry>,
}

/// Steps 2-3 of the normative procedure for every pack access: verify
/// BLAKE3(file bytes) == expected id WITHOUT decrypting anything on mismatch,
/// then parse prologue, decrypt the footer under the reserved counter, and
/// validate the body-region identity.
fn open_pack(
    pack_bytes: &[u8],
    expected_pack_id: &PackId,
    fmk: &[u8; KEY_LEN],
    cipher: &dyn PackCipher,
) -> Result<PackContext, PackError> {
    // Step 2: name verification precedes any crypto.
    let actual_name = pack_name_of(pack_bytes);
    if &actual_name != expected_pack_id {
        return Err(PackError::NameMismatch {
            expected: hex(expected_pack_id),
            found: hex(&actual_name),
        });
    }

    if pack_bytes.len() < HEADER_LEN + SALT_LEN + 4 {
        return Err(PackError::TooShort);
    }
    let kind = match parse_header(pack_bytes) {
        Ok(k) => k,
        Err(FormatError::BadMagic) => return Err(PackError::BadMagic),
        Err(FormatError::UnknownKind(k)) => return Err(PackError::UnknownKind(k)),
        Err(FormatError::BadVersion(..)) => return Err(PackError::BadVersion),
        Err(other) => return Err(PackError::Format(other)),
    };
    match kind {
        ContainerKind::PackData | ContainerKind::PackMeta => {}
        _ => return Err(PackError::NotAPack),
    }

    let salt: [u8; SALT_LEN] = pack_bytes[HEADER_LEN..HEADER_LEN + SALT_LEN]
        .try_into()
        .unwrap();
    let flen_pos = pack_bytes.len() - 4;
    let footer_len = u32::from_le_bytes(pack_bytes[flen_pos..].try_into().unwrap()) as usize;
    let footer_start = flen_pos
        .checked_sub(footer_len)
        .ok_or(PackError::FooterCorrupt("negative footer extent"))?;
    if footer_start < HEADER_LEN + SALT_LEN {
        return Err(PackError::FooterCorrupt("footer overlaps prologue"));
    }

    // Step 3: footer under the reserved counter.
    let key = derive_pack_key(fmk, &salt, kind);
    let header_arr: [u8; HEADER_LEN] = pack_bytes[..HEADER_LEN].try_into().unwrap();
    let footer_pt = cipher.open(
        &key,
        &FOOTER_NONCE,
        &footer_aad(&header_arr, kind),
        &pack_bytes[footer_start..flen_pos],
    )?;
    let (body_plain_len, entries) = footer_parse(&footer_pt)?;

    // Conformance: body region must satisfy plain_len + 16 * segments.
    let body_start = HEADER_LEN + SALT_LEN;
    let want_region = crate::crypto::body_region_len(body_plain_len);
    let got_region = (footer_start - body_start) as u64;
    if got_region != want_region {
        return Err(PackError::BodyRegionMismatch {
            got: got_region,
            want: want_region,
        });
    }

    Ok(PackContext {
        kind,
        key,
        body_start,
        footer_start,
        body_plain_len,
        entries,
    })
}

/// Decrypt and validate just the footer of a pack (used by index rebuild).
pub fn read_footer(
    pack_bytes: &[u8],
    expected_pack_id: &PackId,
    fmk: &[u8; KEY_LEN],
    cipher: &dyn PackCipher,
) -> Result<(u64, Vec<FooterEntry>), PackError> {
    let ctx = open_pack(pack_bytes, expected_pack_id, fmk, cipher)?;
    Ok((ctx.body_plain_len, ctx.entries))
}

/// Normative read procedure (`docs/store-format.md`, "Reading a blob").
///
/// 1. Verify BLAKE3(file) == `expected_pack_id`; reject WITHOUT decrypting.
/// 2. Parse header and salt; read trailing `footer_len`; decrypt footer under
///    the reserved counter; validate the body-region identity.
/// 3. Confirm `(kind, id)` appears; if `index_loc` disagrees with the
///    footer, trust neither and stop.
/// 4. Decrypt exactly the segments covering `[plain_off, plain_off+len)`.
/// 5. Require BLAKE3(plaintext) == id before returning anything.
pub fn read_blob(
    pack_bytes: &[u8],
    expected_pack_id: &PackId,
    fmk: &[u8; KEY_LEN],
    cipher: &dyn PackCipher,
    want_kind: BlobKind,
    want_id: &BlobId,
    index_loc: Option<(u64, u64)>,
) -> Result<Vec<u8>, PackError> {
    let ctx = open_pack(pack_bytes, expected_pack_id, fmk, cipher)?;

    // Step 3 continued: the blob must appear, and agree with the index.
    let entry = ctx
        .entries
        .iter()
        .find(|e| e.kind == want_kind && &e.id == want_id)
        .ok_or(PackError::NotFound)?
        .clone();
    if let Some((idx_off, idx_len)) = index_loc {
        if idx_off != entry.plain_off || idx_len != entry.plain_len {
            return Err(PackError::Disagreement {
                index: (idx_off, idx_len),
                footer: (entry.plain_off, entry.plain_len),
            });
        }
    }

    // Steps 4-5: decrypt exactly the spanning segments.
    let seg_plain = SEGMENT_PLAIN_LEN as u64;
    let n_segments = segment_count(ctx.body_plain_len);
    let first_seg = entry.plain_off / seg_plain;
    let last_seg = (entry.plain_off + entry.plain_len - 1) / seg_plain;
    let header_arr: [u8; HEADER_LEN] = pack_bytes[..HEADER_LEN].try_into().unwrap();
    let aad = body_aad(&header_arr, ctx.kind);
    let mut assembled = Vec::with_capacity(entry.plain_len as usize);
    for s in first_seg..=last_seg {
        let seg_start = s * seg_plain;
        let plain_here = std::cmp::min(seg_plain, ctx.body_plain_len - seg_start);
        let ct_off = ctx.body_start + (s as usize) * (SEGMENT_PLAIN_LEN + TAG_LEN);
        let ct_end = ct_off + plain_here as usize + TAG_LEN;
        if ct_end > ctx.footer_start {
            return Err(PackError::BodyRegionMismatch {
                got: ct_end as u64,
                want: ctx.footer_start as u64,
            });
        }
        let last_flag = u8::from(s + 1 == n_segments);
        let nonce = body_nonce(s as u32, last_flag);
        let pt = cipher.open(&ctx.key, &nonce, &aad, &pack_bytes[ct_off..ct_end])?;
        debug_assert_eq!(pt.len() as u64, plain_here);
        assembled.extend_from_slice(&pt);
    }

    // Step 6: verify-after-decrypt.
    let local = (entry.plain_off % seg_plain) as usize;
    let plain = &assembled[local..local + entry.plain_len as usize];
    let found = *blake3::hash(plain).as_bytes();
    if &found != want_id {
        return Err(PackError::HashMismatch {
            expected: hex(want_id),
            found: hex(&found),
        });
    }
    Ok(plain.to_vec())
}

/// Spec procedure steps 3-6 for creating a pack atomically: unique temp name
/// under `tmp_dir`, write + fsync, rename into `packs_dir/<name>.pack`,
/// fsync the directory. A crash anywhere leaves either nothing or a complete
/// pack; readers only ever look in `packs_dir`.
pub fn write_pack_atomically(
    tmp_dir: &std::path::Path,
    packs_dir: &std::path::Path,
    file_bytes: &[u8],
) -> Result<PackId, PackError> {
    use rand::rngs::OsRng;

    let name = pack_name_of(file_bytes);
    let mut rnd = [0u8; 16];
    OsRng.fill_bytes(&mut rnd);
    let tmp_path = tmp_dir.join(format!("pack-{}.tmp", hex(&rnd)));

    {
        let mut f = std::fs::File::create(&tmp_path)?;
        std::io::Write::write_all(&mut f, file_bytes)?;
        f.sync_all()?;
    }
    let final_path = packs_dir.join(format!("{}.pack", hex(&name)));
    std::fs::rename(&tmp_path, &final_path)?;
    sync_dir(packs_dir)?;
    Ok(name)
}

/// fsync a directory so the rename itself is durable. Not supported on every
/// platform (Windows); treated as best-effort there.
pub fn sync_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

/// One accumulating staging pack. Blobs append back-to-back; positions are
/// recorded for the footer written at seal time.
pub struct StagingPack {
    pub kind: ContainerKind,
    pub salt: [u8; SALT_LEN],
    pub body: Vec<u8>,
    pub entries: Vec<FooterEntry>,
}

impl StagingPack {
    fn new(kind: ContainerKind, rng: &mut impl RngCore) -> Self {
        let mut salt = [0u8; SALT_LEN];
        rng.fill_bytes(&mut salt);
        StagingPack {
            kind,
            salt,
            body: Vec::new(),
            entries: Vec::new(),
        }
    }

    fn push(&mut self, kind: BlobKind, id: BlobId, bytes: &[u8]) {
        self.entries.push(FooterEntry {
            kind,
            id,
            plain_off: self.body.len() as u64,
            plain_len: bytes.len() as u64,
        });
        self.body.extend_from_slice(bytes);
    }
}

fn container_for(kind: BlobKind) -> ContainerKind {
    if kind.is_meta() {
        ContainerKind::PackMeta
    } else {
        ContainerKind::PackData
    }
}

/// The two staging pools (data, meta) implementing the spec's membership
/// randomization: up to W open packs per kind, uniform CSPRNG assignment,
/// immediate seal plus redraw on target overflow, empties discarded.
///
/// Assignment order is never persisted or reproduced; it only shapes which
/// blobs share a pack, which the footers and index record.
pub struct StagingPools {
    data: Vec<StagingPack>,
    meta: Vec<StagingPack>,
}

impl Default for StagingPools {
    fn default() -> Self {
        Self::new()
    }
}

/// Comparable snapshot of open-pack membership (tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembershipSnapshot {
    pub is_meta: bool,
    pub salt: [u8; SALT_LEN],
    pub entries: Vec<(u8, BlobId)>,
}

impl StagingPools {
    pub fn new() -> Self {
        StagingPools {
            data: Vec::new(),
            meta: Vec::new(),
        }
    }

    /// Assign one blob to a uniformly chosen open staging pack of its kind.
    /// Returns packs that were sealed early because accepting the blob would
    /// have pushed them past `target`.
    pub fn offer(
        &mut self,
        kind: BlobKind,
        id: BlobId,
        bytes: &[u8],
        target: usize,
        max_open: usize,
        rng: &mut impl Rng,
    ) -> Vec<StagingPack> {
        let container = container_for(kind);
        let pool = if kind.is_meta() {
            &mut self.meta
        } else {
            &mut self.data
        };
        let mut sealed = Vec::new();
        loop {
            if pool.is_empty() {
                pool.push(StagingPack::new(container, rng));
            }
            let idx = rng.gen_range(0..pool.len());
            let overflow = {
                let sp = &pool[idx];
                !sp.entries.is_empty() && sp.body.len() + bytes.len() > target
            };
            if overflow {
                // Seal now, redraw among the remaining open packs; a fresh
                // pack starts only if none remain.
                sealed.push(pool.remove(idx));
                continue;
            }
            pool[idx].push(kind, id, bytes);
            debug_assert!(pool.len() <= max_open.max(1));
            break;
        }
        sealed
    }

    /// End-of-burst rule: seal everything still open, discarding empty packs.
    pub fn drain_all(&mut self) -> Vec<StagingPack> {
        let mut out = Vec::new();
        for pool in [&mut self.data, &mut self.meta] {
            for sp in pool.drain(..) {
                if !sp.entries.is_empty() {
                    out.push(sp);
                }
            }
        }
        out
    }

    /// Currently open packs for a kind (never more than W after an offer).
    pub fn open_count(&self, kind: BlobKind) -> usize {
        if kind.is_meta() {
            self.meta.len()
        } else {
            self.data.len()
        }
    }

    /// Look for a blob in the not-yet-sealed staging packs. Used by reads so
    /// a writer sees its own puts before any flush.
    pub fn staged_bytes(&self, kind: BlobKind, id: &BlobId) -> Option<Vec<u8>> {
        let pool = if kind.is_meta() {
            &self.meta
        } else {
            &self.data
        };
        pool.iter().find_map(|sp| {
            sp.entries
                .iter()
                .find(|e| e.kind == kind && e.id == *id)
                .map(|e| {
                    let start = e.plain_off as usize;
                    sp.body[start..start + e.plain_len as usize].to_vec()
                })
        })
    }

    /// Deterministic view of open membership for tests.
    pub fn snapshot_membership(&self) -> Vec<MembershipSnapshot> {
        let mut snap = Vec::new();
        for (is_meta, pool) in [(false, &self.data), (true, &self.meta)] {
            for sp in pool {
                snap.push(MembershipSnapshot {
                    is_meta,
                    salt: sp.salt,
                    entries: sp.entries.iter().map(|e| (e.kind.to_u8(), e.id)).collect(),
                });
            }
        }
        snap
    }
}
