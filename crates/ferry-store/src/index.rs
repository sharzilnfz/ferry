#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{PassthroughCipher, TAG_LEN};
    use crate::format::HEADER_LEN;
    use crate::pack::{seal_pack_bytes, FooterEntry};

    fn salt() -> [u8; SALT_LEN] {
        core::array::from_fn(|i| 0x20 + i as u8)
    }
    fn fmk() -> [u8; KEY_LEN] {
        core::array::from_fn(|i| i as u8)
    }

    fn entry(kind: BlobKind, id_byte: u8, pack_byte: u8, off: u64, len: u64) -> IndexEntry {
        IndexEntry {
            kind,
            id: core::array::from_fn(|i| id_byte.wrapping_add(i as u8)),
            pack: [pack_byte; 32],
            plain_off: off,
            plain_len: len,
        }
    }

    #[test]
    fn table_serialization_fixture_hand_computed() {
        let e = IndexEntry {
            kind: BlobKind::DataChunk,
            id: [0xaa; 32],
            pack: [0xcc; 32],
            plain_off: 5,
            plain_len: 7,
        };
        let mut expect: Vec<u8> = Vec::new();
        expect.extend_from_slice(&1u32.to_le_bytes());
        expect.push(0x01);
        expect.extend_from_slice(&[0xaa; 32]);
        expect.extend_from_slice(&[0xcc; 32]);
        expect.extend_from_slice(&5u64.to_le_bytes());
        expect.extend_from_slice(&7u64.to_le_bytes());
        assert_eq!(table_plain(&[e]), expect);
        assert_eq!(expect.len(), 4 + 81);
    }

    #[test]
    fn table_is_sorted_by_kind_then_id_regardless_of_input_order() {
        let tree = entry(BlobKind::TreeNode, 0x02, 9, 0, 0);
        let data_low = entry(BlobKind::DataChunk, 0x10, 9, 0, 0);
        let data_high = entry(BlobKind::DataChunk, 0x80, 9, 0, 0);
        let manifest = entry(BlobKind::Manifest, 0x01, 9, 0, 0);

        let out = table_plain(&[
            data_high.clone(),
            tree.clone(),
            manifest.clone(),
            data_low.clone(),
        ]);
        let parsed = table_parse(&out).unwrap();
        assert_eq!(
            parsed,
            vec![
                data_low.clone(),
                data_high.clone(),
                tree.clone(),
                manifest.clone()
            ]
        );

        let again = table_plain(&[manifest, data_low, tree, data_high]);
        assert_eq!(out, again);
    }

    #[test]
    fn table_parse_rejects_unsorted_tables() {
        let a = entry(BlobKind::DataChunk, 0x10, 9, 0, 0);
        let b = entry(BlobKind::DataChunk, 0x20, 9, 0, 0);
        let good = table_plain(&[a.clone(), b.clone()]);

        let mut bad = Vec::new();
        put_u32(&mut bad, 2);
        for e in [b, a] {
            bad.push(e.kind.to_u8());
            put_bytes(&mut bad, &e.id);
            put_bytes(&mut bad, &e.pack);
            put_u64(&mut bad, e.plain_off);
            put_u64(&mut bad, e.plain_len);
        }
        assert!(matches!(table_parse(&bad), Err(IndexError::Unsorted)));

        assert!(matches!(
            table_parse(&good[..good.len() - 3]),
            Err(IndexError::Corrupt(_))
        ));
    }

    #[test]
    fn index_container_layout_and_round_trip() {
        let cipher = PassthroughCipher;
        let entries = vec![
            entry(BlobKind::DataChunk, 0x10, 1, 0, 100),
            entry(BlobKind::TreeNode, 0x40, 2, 200, 50),
        ];
        let file = seal_index_container(&fmk(), &salt(), &entries, &cipher).unwrap();

        assert_eq!(&file[..5], b"FERRY");
        assert_eq!(file[5], ContainerKind::Index.to_u8());
        assert_eq!(&file[6..10], &1u32.to_le_bytes());
        assert_eq!(&file[10..26], &salt());
        let tlen = u32::from_le_bytes(file[file.len() - 4..].try_into().unwrap()) as usize;
        let table_pt_len = 4 + entries.len() * 81;
        assert_eq!(tlen, table_pt_len + TAG_LEN);
        assert_eq!(
            file.len(),
            HEADER_LEN + SALT_LEN + tlen + 4,
            "empty body: prologue + table ciphertext + trailer only"
        );

        let got = open_index_container(&file, &fmk(), &cipher).unwrap();
        assert_eq!(got, entries);
    }

    #[test]
    fn index_container_rejects_wrong_magic_and_kind() {
        let cipher = PassthroughCipher;
        let file = seal_index_container(&fmk(), &salt(), &[], &cipher).unwrap();
        let mut evil = file.clone();
        evil[..5].copy_from_slice(b"FERXY");
        let err = open_index_container(&evil, &fmk(), &cipher).unwrap_err();
        assert!(matches!(err, IndexError::Format(FormatError::BadMagic)));

        let mut not_index = file.clone();
        not_index[5] = ContainerKind::PackData.to_u8();
        let err = open_index_container(&not_index, &fmk(), &cipher).unwrap_err();
        assert!(matches!(err, IndexError::NotAnIndex));
    }

    #[test]
    fn corrupt_trailer_length_is_a_typed_error_not_a_panic() {
        let cipher = PassthroughCipher;
        let file = seal_index_container(&fmk(), &salt(), &[], &cipher).unwrap();

        let mut evil = file.clone();
        let n = evil.len();
        evil[n - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            open_index_container(&evil, &fmk(), &cipher),
            Err(IndexError::Corrupt("negative table extent"))
        ));

        let mut small = file;
        let n = small.len();
        let tlen = (n - HEADER_LEN) as u32;
        small[n - 4..].copy_from_slice(&tlen.to_le_bytes());
        assert!(matches!(
            open_index_container(&small, &fmk(), &cipher),
            Err(IndexError::Corrupt("negative table extent"))
        ));
    }

    #[test]
    fn union_of_indexes_resolves_duplicates_preferring_existing_packs() {
        let mut table = LocationTable::default();
        let a = entry(BlobKind::DataChunk, 0x10, 0xAA, 0, 10);
        let b = entry(BlobKind::DataChunk, 0x10, 0xBB, 20, 10);

        table.merge(std::iter::once(a.clone()));
        table.merge(std::iter::once(b.clone()));

        let exists_aa = |p: &PackId| p[0] == 0xAA;
        let exists_bb = |p: &PackId| p[0] == 0xBB;
        let exists_none = |_: &PackId| false;

        let got = table
            .resolve(BlobKind::DataChunk, &a.id, exists_aa)
            .unwrap();
        assert_eq!(got.pack, a.pack);
        let got = table
            .resolve(BlobKind::DataChunk, &a.id, exists_bb)
            .unwrap();
        assert_eq!(got.pack, b.pack);

        assert!(table
            .resolve(BlobKind::DataChunk, &a.id, exists_none)
            .is_none());

        let mut duped = LocationTable::default();
        duped.merge([a.clone(), a.clone(), b.clone()]);
        assert_eq!(duped.candidates(BlobKind::DataChunk, &a.id).len(), 2);
    }

    #[test]
    fn packs_dedup_and_sort_regardless_of_merge_order() {
        let mk = |pack_byte: u8| entry(BlobKind::DataChunk, pack_byte, pack_byte, 0, 0);
        let mut t1 = LocationTable::default();
        t1.merge([mk(0x02), mk(0x01)]);
        let mut t2 = LocationTable::default();
        t2.merge([mk(0x03)]);
        t1.merge(t2.entries.clone());
        assert_eq!(t1.packs(), vec![[0x01; 32], [0x02; 32], [0x03; 32]]);

        assert_eq!(t1.len(), 3);
    }

    #[test]
    fn rebuild_recovers_every_entry_from_packs_and_skips_liars() {
        let cipher = PassthroughCipher;
        let dir = tempfile::tempdir().unwrap();
        let packs_dir = dir.path().join("packs");
        std::fs::create_dir(&packs_dir).unwrap();

        let mk_pack = |kind: ContainerKind, payloads: &[(BlobKind, u8, usize)]| {
            let mut body = Vec::new();
            let mut entries = Vec::new();
            for (bk, seed, len) in payloads {
                let bytes: Vec<u8> = (0..*len).map(|i| (*seed as usize + i) as u8).collect();
                let id: BlobId = *blake3::hash(&bytes).as_bytes();
                entries.push(FooterEntry {
                    kind: *bk,
                    id,
                    plain_off: body.len() as u64,
                    plain_len: bytes.len() as u64,
                });
                body.extend_from_slice(&bytes);
            }
            let salt: [u8; SALT_LEN] = core::array::from_fn(|i| 0x50 + i as u8);
            let file = seal_pack_bytes(kind, &fmk(), &salt, &body, &entries, &cipher).unwrap();
            let name = crate::pack::pack_name_of(&file);
            std::fs::write(
                packs_dir.join(format!("{}.pack", crate::format::hex(&name))),
                file,
            )
            .unwrap();
            entries
                .into_iter()
                .map(|e| IndexEntry {
                    kind: e.kind,
                    id: e.id,
                    pack: name,
                    plain_off: e.plain_off,
                    plain_len: e.plain_len,
                })
                .collect::<Vec<_>>()
        };

        let want_data = mk_pack(
            ContainerKind::PackData,
            &[(BlobKind::DataChunk, 1, 500), (BlobKind::DataChunk, 2, 700)],
        );
        let want_meta = mk_pack(ContainerKind::PackMeta, &[(BlobKind::TreeNode, 3, 64)]);

        let (got, skipped) = rebuild_entries(&packs_dir, &fmk(), &cipher).unwrap();
        let mut got = got;
        got.sort_by_key(|e| (e.kind, e.id));
        let mut want = [want_data, want_meta].concat();
        want.sort_by_key(|e| (e.kind, e.id));
        assert_eq!(got, want);
        assert!(skipped.is_empty());

        let liar_path = packs_dir.join(format!("{}.pack", "0".repeat(64)));
        std::fs::write(&liar_path, b"not really a pack").unwrap();
        let (_, skipped) = rebuild_entries(&packs_dir, &fmk(), &cipher).unwrap();
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("0000"));
    }

    #[test]
    fn atomic_index_write_leaves_no_temp_files() {
        let tmp = tempfile::tempdir().unwrap();
        let idx_dir = tmp.path().join("index");
        let staging = tmp.path().join("tmp");
        std::fs::create_dir(&idx_dir).unwrap();
        std::fs::create_dir(&staging).unwrap();
        let bytes = b"an index worth keeping".to_vec();
        write_named_atomically(&staging, &idx_dir, "7.ferryindex", &bytes).unwrap();
        assert_eq!(std::fs::read(idx_dir.join("7.ferryindex")).unwrap(), bytes);
        assert!(std::fs::read_dir(staging).unwrap().next().is_none());
    }
}

use thiserror::Error;

use crate::crypto::{
    derive_index_key, footer_aad, CryptoError, PackCipher, FOOTER_NONCE, KEY_LEN, SALT_LEN,
};
use crate::format::{
    hex, parse_header, put_bytes, put_u32, put_u64, write_header, BlobId, BlobKind, ContainerKind,
    FormatError, PackId, Reader, HEADER_LEN,
};
use rand::RngCore;

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("not an INDEX container")]
    NotAnIndex,
    #[error("index table corrupt: {0}")]
    Corrupt(&'static str),
    #[error("index table not sorted by (blob_kind, id)")]
    Unsorted,
    #[error("reserved field must be zero")]
    ReservedNonzero,
    #[error("{0}")]
    Format(#[from] FormatError),
    #[error("{0}")]
    Crypto(#[from] CryptoError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IndexEntry {
    pub kind: BlobKind,
    pub id: BlobId,
    pub pack: PackId,
    pub plain_off: u64,
    pub plain_len: u64,
}

fn entry_key(e: &IndexEntry) -> (u8, &[u8]) {
    (e.kind.to_u8(), &e.id[..])
}

pub fn table_plain(entries: &[IndexEntry]) -> Vec<u8> {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|a, b| entry_key(a).cmp(&entry_key(b)));
    sorted.dedup_by(|a, b| a.kind == b.kind && a.id == b.id);
    let mut out = Vec::with_capacity(4 + sorted.len() * 81);
    put_u32(&mut out, sorted.len() as u32);
    for e in &sorted {
        out.push(e.kind.to_u8());
        put_bytes(&mut out, &e.id);
        put_bytes(&mut out, &e.pack);
        put_u64(&mut out, e.plain_off);
        put_u64(&mut out, e.plain_len);
    }
    out
}

pub fn table_parse(bytes: &[u8]) -> Result<Vec<IndexEntry>, IndexError> {
    let mut r = Reader::new(bytes);
    let count = r.u32().map_err(|_| IndexError::Corrupt("truncated"))?;
    let mut out = Vec::new();
    let mut prev: Option<(u8, [u8; 32])> = None;
    for _ in 0..count {
        let kb = r.u8().map_err(|_| IndexError::Corrupt("truncated"))?;
        let kind = BlobKind::from_u8(kb).ok_or(IndexError::Corrupt("unknown blob kind"))?;
        let id = r.array().map_err(|_| IndexError::Corrupt("truncated"))?;
        let pack = r.array().map_err(|_| IndexError::Corrupt("truncated"))?;
        let plain_off = r.u64().map_err(|_| IndexError::Corrupt("truncated"))?;
        let plain_len = r.u64().map_err(|_| IndexError::Corrupt("truncated"))?;
        let cur = (kb, id);
        if let Some(p) = prev {
            if p >= cur {
                return Err(IndexError::Unsorted);
            }
        }
        prev = Some(cur);
        out.push(IndexEntry {
            kind,
            id,
            pack,
            plain_off,
            plain_len,
        });
    }
    r.expect_end()
        .map_err(|_| IndexError::Corrupt("trailing bytes"))?;
    Ok(out)
}

pub fn seal_index_container(
    fmk: &[u8; KEY_LEN],
    salt: &[u8; SALT_LEN],
    entries: &[IndexEntry],
    cipher: &dyn PackCipher,
) -> Result<Vec<u8>, IndexError> {
    let kind = ContainerKind::Index;
    let header = write_header(kind);
    let key = derive_index_key(fmk, salt);
    let table_ct = cipher.seal(
        &key,
        &FOOTER_NONCE,
        &footer_aad(&header, kind),
        &table_plain(entries),
    )?;
    let mut file = Vec::with_capacity(HEADER_LEN + SALT_LEN + table_ct.len() + 4);
    file.extend_from_slice(&header);
    file.extend_from_slice(salt);
    file.extend_from_slice(&table_ct);
    file.extend_from_slice(&(table_ct.len() as u32).to_le_bytes());
    Ok(file)
}

pub fn open_index_container(
    file_bytes: &[u8],
    fmk: &[u8; KEY_LEN],
    cipher: &dyn PackCipher,
) -> Result<Vec<IndexEntry>, IndexError> {
    if file_bytes.len() < HEADER_LEN + SALT_LEN + 4 {
        return Err(IndexError::Corrupt("too short"));
    }
    match parse_header(file_bytes)? {
        ContainerKind::Index => {}
        _ => return Err(IndexError::NotAnIndex),
    }
    let salt: [u8; SALT_LEN] = file_bytes[HEADER_LEN..HEADER_LEN + SALT_LEN]
        .try_into()
        .unwrap();
    let tlen_pos = file_bytes.len() - 4;
    let tlen = u32::from_le_bytes(file_bytes[tlen_pos..].try_into().unwrap()) as usize;

    let ct_start = tlen_pos
        .checked_sub(tlen)
        .ok_or(IndexError::Corrupt("negative table extent"))?;
    if ct_start < HEADER_LEN + SALT_LEN {
        return Err(IndexError::Corrupt("negative table extent"));
    }
    let key = derive_index_key(fmk, &salt);
    let header_arr: [u8; HEADER_LEN] = file_bytes[..HEADER_LEN].try_into().unwrap();
    let pt = cipher.open(
        &key,
        &FOOTER_NONCE,
        &footer_aad(&header_arr, ContainerKind::Index),
        &file_bytes[ct_start..tlen_pos],
    )?;
    table_parse(&pt)
}

#[derive(Debug, Default)]
pub struct LocationTable {
    entries: Vec<IndexEntry>,

    seen: std::collections::HashSet<IndexEntry>,

    by_blob: std::collections::HashMap<(BlobKind, BlobId), Vec<usize>>,
}

impl LocationTable {
    pub fn merge<I: IntoIterator<Item = IndexEntry>>(&mut self, incoming: I) {
        for e in incoming {
            if self.seen.insert(e.clone()) {
                self.by_blob
                    .entry((e.kind, e.id))
                    .or_default()
                    .push(self.entries.len());
                self.entries.push(e);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn candidates(&self, kind: BlobKind, id: &BlobId) -> Vec<IndexEntry> {
        match self.by_blob.get(&(kind, *id)) {
            Some(rows) => rows
                .iter()
                .filter_map(|i| self.entries.get(*i))
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn resolve(
        &self,
        kind: BlobKind,
        id: &BlobId,
        pack_exists: impl Fn(&PackId) -> bool,
    ) -> Option<IndexEntry> {
        self.candidates(kind, id)
            .into_iter()
            .find(|e| pack_exists(&e.pack))
    }

    pub fn packs(&self) -> Vec<PackId> {
        let seen: std::collections::BTreeSet<PackId> =
            self.entries.iter().map(|e| e.pack).collect();
        seen.into_iter().collect()
    }

    pub fn iter_sorted(&self) -> Vec<IndexEntry> {
        let mut v = self.entries.clone();
        v.sort_by(|a, b| Ord::cmp(&entry_key(a), &entry_key(b)));
        v.dedup();
        v
    }
}

pub fn rebuild_entries(
    packs_dir: &std::path::Path,
    fmk: &[u8; KEY_LEN],
    cipher: &dyn PackCipher,
) -> Result<(Vec<IndexEntry>, Vec<String>), IndexError> {
    let mut names: Vec<std::path::PathBuf> = std::fs::read_dir(packs_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "pack"))
        .collect();
    names.sort();

    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for path in names {
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let claimed: PackId = match crate::format::unhex(&stem) {
            Some(id) => id,
            None => {
                skipped.push(stem);
                continue;
            }
        };
        let bytes = std::fs::read(&path)?;
        match crate::pack::read_footer(&bytes, &claimed, fmk, cipher) {
            Ok((_, entries)) => {
                for e in entries {
                    out.push(IndexEntry {
                        kind: e.kind,
                        id: e.id,
                        pack: claimed,
                        plain_off: e.plain_off,
                        plain_len: e.plain_len,
                    });
                }
            }
            Err(_) => skipped.push(stem),
        }
    }
    Ok((out, skipped))
}

pub fn write_named_atomically(
    tmp_dir: &std::path::Path,
    final_dir: &std::path::Path,
    file_name: &str,
    bytes: &[u8],
) -> Result<std::path::PathBuf, IndexError> {
    use rand::rngs::OsRng;

    let mut rnd = [0u8; 16];
    OsRng.fill_bytes(&mut rnd);
    let tmp_path = tmp_dir.join(format!("{}.{}", hex(&rnd), file_name));
    {
        let mut f = std::fs::File::create(&tmp_path)?;
        std::io::Write::write_all(&mut f, bytes)?;
        f.sync_all()?;
    }
    let final_path = final_dir.join(file_name);
    std::fs::rename(&tmp_path, &final_path)?;
    crate::pack::sync_dir(final_dir)?;
    Ok(final_path)
}
