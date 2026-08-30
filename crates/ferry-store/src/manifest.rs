








#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_node_serialization_fixture_hand_computed() {
        
        
        let tn = TreeNode {
            entries: vec![file_entry(
                "b.txt",
                true,
                0x10000000,
                999_999_999,
                vec![([0xaa; 32], 5)],
            )],
        };
        let mut expect: Vec<u8> = Vec::new();
        expect.extend_from_slice(&1u32.to_le_bytes()); 
        expect.push(0x00); 
        expect.extend_from_slice(&5u32.to_le_bytes()); 
        expect.extend_from_slice(b"b.txt"); 
        expect.push(0x01); 
        expect.extend_from_slice(&0x10000000i64.to_le_bytes()); 
        expect.extend_from_slice(&999_999_999u32.to_le_bytes()); 
        expect.extend_from_slice(&5u64.to_le_bytes()); 
        expect.extend_from_slice(&1u32.to_le_bytes()); 
        expect.extend_from_slice(&[0xaa; 32]); 
        expect.extend_from_slice(&5u64.to_le_bytes()); 
        assert_eq!(serialize_tree_node(&tn), expect);
        
        assert_eq!(parse_tree_node(&expect).unwrap(), tn);
    }

    #[test]
    fn manifest_serialization_fixture_hand_computed() {
        let m = RootManifest {
            folder_id: [0x11; 16],
            device_id: [0x22; 32],
            created_sec: 0x10000000,
            created_nsec: 999_999_999,
            root_tree_id: [0x33; 32],
            parent_manifest_id: [0; 32],
        };
        let mut expect: Vec<u8> = Vec::new();
        expect.extend_from_slice(&[0x11; 16]);
        expect.extend_from_slice(&[0x22; 32]);
        expect.extend_from_slice(&0x10000000i64.to_le_bytes());
        expect.extend_from_slice(&999_999_999u32.to_le_bytes());
        expect.extend_from_slice(&[0x33; 32]);
        expect.extend_from_slice(&[0; 32]); 
        expect.extend_from_slice(&[0; 32]); 
        assert_eq!(serialize_manifest(&m), expect);
        assert_eq!(parse_manifest(&expect).unwrap(), m);
    }

    #[test]
    fn all_three_entry_types_round_trip() {
        let tn = TreeNode {
            entries: vec![
                dir_entry("sub", 1700000000, 123, [7; 32]),
                file_entry("a.bin", false, -5, 0, vec![([1; 32], 512), ([2; 32], 300)]),
                symlink_entry("link", 42, 999_999_999, "../elsewhere"),
            ],
        };
        let bytes = serialize_tree_node(&tn);
        let parsed = parse_tree_node(&bytes).unwrap();
        
        let mut expect_entries = tn.entries.clone();
        expect_entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        assert_eq!(parsed.entries, expect_entries);

        
        let a = &parsed.entries.iter().find(|e| e.name == "a.bin").unwrap();
        assert_eq!(a.mtime_sec, -5);
        assert_eq!(a.mtime_nsec, 0);
    }

    #[test]
    fn entries_serialize_sorted_by_name_bytes() {
        let tn = TreeNode {
            entries: vec![
                file_entry("zeta", false, 0, 0, vec![]),
                dir_entry("Alpha", 0, 0, [3; 32]),
                file_entry("beta", false, 0, 0, vec![]),
            ],
        };
        let parsed = parse_tree_node(&serialize_tree_node(&tn)).unwrap();
        let names: Vec<&str> = parsed.entries.iter().map(|e| e.name.as_str()).collect();
        
        assert_eq!(names, ["Alpha", "beta", "zeta"]);
        
        let reordered = TreeNode {
            entries: vec![
                dir_entry("Alpha", 0, 0, [3; 32]),
                file_entry("zeta", false, 0, 0, vec![]),
                file_entry("beta", false, 0, 0, vec![]),
            ],
        };
        assert_eq!(serialize_tree_node(&reordered), serialize_tree_node(&tn));
    }

    #[test]
    fn duplicate_names_rejected() {
        let tn = TreeNode {
            entries: vec![
                file_entry("same", false, 0, 0, vec![]),
                file_entry("same", false, 0, 0, vec![]),
            ],
        };
        assert!(matches!(
            validate_entries(&tn.entries),
            Err(ManifestError::DuplicateName(_))
        ));
    }

    #[test]
    fn invalid_names_rejected() {
        for bad in ["dir/x", "a\0b", ".", ".."] {
            let e = TreeEntry {
                name: bad.to_string(),
                exec: false,
                mtime_sec: 0,
                mtime_nsec: 0,
                payload: EntryPayload::File {
                    size: 0,
                    chunks: vec![],
                },
            };
            assert!(validate_name(bad).is_err(), "name {bad:?} must be refused");
            assert!(validate_entry(&e).is_err(), "entry {bad:?} must be refused");
        }
    }

    #[test]
    fn colon_or_prefixed_names_rejected() {
        
        
        
        
        
        
        for bad in ["C:x", "C:\\x", "a:b", "C:", "/abs"].map(str::to_string) {
            let e = TreeEntry {
                name: bad.clone(),
                exec: false,
                mtime_sec: 0,
                mtime_nsec: 0,
                payload: EntryPayload::File {
                    size: 0,
                    chunks: vec![],
                },
            };
            assert!(validate_name(&bad).is_err(), "name {bad:?} must be refused");
            assert!(validate_entry(&e).is_err(), "entry {bad:?} must be refused");
            assert!(
                matches!(validate_name(&bad), Err(ManifestError::InvalidName(_))),
                "name {bad:?} must be refused as InvalidName"
            );
        }
    }

    #[test]
    fn names_are_nfc_normalized_on_write_and_validated_on_read() {
        
        let decomposed = "cafe\u{301}";
        let composed = "caf\u{e9}";
        assert_ne!(decomposed, composed);
        let tn = TreeNode {
            entries: vec![file_entry(decomposed, false, 0, 0, vec![])],
        };
        let bytes = serialize_tree_node(&tn);
        let stored_name = parse_tree_node(&bytes).unwrap().entries[0].name.clone();
        assert_eq!(stored_name, composed, "stored form must be NFC");

        
        
        
        let mut evil: Vec<u8> = Vec::new();
        put_u32(&mut evil, 1); 
        evil.push(0x00); 
        put_u32(&mut evil, decomposed.len() as u32);
        put_bytes(&mut evil, decomposed.as_bytes());
        evil.push(0x00); 
        put_i64(&mut evil, 0); 
        put_u32(&mut evil, 0); 
        put_u64(&mut evil, 0); 
        put_u32(&mut evil, 0); 
        assert!(matches!(
            parse_tree_node(&evil),
            Err(ManifestError::NotNfc(_))
        ));
    }

    #[test]
    fn reserved_flag_bits_and_non_file_exec_rejected_on_parse() {
        
        
        let tn = TreeNode {
            entries: vec![file_entry("f", false, 0, 0, vec![])],
        };
        let mut b = serialize_tree_node(&tn);
        
        assert_eq!(10, 4 + 1 + 4 + 1);
        b[10] = 0b10;
        assert!(matches!(
            parse_tree_node(&b),
            Err(ManifestError::ReservedBitsSet)
        ));

        
        let d = TreeNode {
            entries: vec![dir_entry("d", 0, 0, [1; 32])],
        };
        let mut db = serialize_tree_node(&d);
        db[10] = 0x01;
        assert!(matches!(
            parse_tree_node(&db),
            Err(ManifestError::ExecFlagOnNonFile)
        ));
    }

    #[test]
    fn nanoseconds_out_of_range_rejected() {
        let e = TreeEntry {
            name: "f".to_string(),
            exec: false,
            mtime_sec: 0,
            mtime_nsec: 1_000_000_000,
            payload: EntryPayload::File {
                size: 0,
                chunks: vec![],
            },
        };
        assert!(matches!(
            validate_entry(&e),
            Err(ManifestError::NsecOutOfRange)
        ));
    }

    #[test]
    fn size_must_equal_chunk_sum() {
        let e = file_entry("f", false, 0, 0, vec![([1; 32], 10), ([2; 32], 20)]);
        let mut wrong = e.clone();
        if let EntryPayload::File { size, .. } = &mut wrong.payload {
            *size = 31;
        }
        assert!(matches!(
            validate_entry(&wrong),
            Err(ManifestError::SizeMismatch { .. })
        ));
    }

    
    
    
    
    #[test]
    fn lying_chunk_count_is_a_typed_error_without_huge_allocation() {
        
        
        let mut evil: Vec<u8> = Vec::new();
        put_u32(&mut evil, 1); 
        evil.push(0x00); 
        put_u32(&mut evil, 1);
        put_bytes(&mut evil, b"f");
        evil.push(0x00); 
        put_i64(&mut evil, 0); 
        put_u32(&mut evil, 0); 
        put_u64(&mut evil, 0); 
        put_u32(&mut evil, u32::MAX); 
        let err = parse_tree_node(&evil).unwrap_err();
        assert!(
            matches!(err, ManifestError::Corrupt("truncated")),
            "got {err:?}"
        );

        
        
        let mut half = evil.clone();
        half.extend_from_slice(&[0xaa; 32]); 
        put_u64(&mut half, 5); 
        assert!(matches!(
            parse_tree_node(&half),
            Err(ManifestError::Corrupt("truncated"))
        ));
    }

    #[test]
    fn manifest_reserved_field_must_be_zero() {
        let m = RootManifest {
            folder_id: [1; 16],
            device_id: [2; 32],
            created_sec: 0,
            created_nsec: 0,
            root_tree_id: [3; 32],
            parent_manifest_id: [0; 32],
        };
        let mut b = serialize_manifest(&m);
        let n = b.len();
        b[n - 1] = 1; 
        assert!(matches!(
            parse_manifest(&b),
            Err(ManifestError::ReservedNonzero)
        ));
    }

    #[test]
    fn truncated_objects_refused() {
        let tn = TreeNode {
            entries: vec![file_entry("f", false, 0, 0, vec![([1; 32], 4)])],
        };
        let full = serialize_tree_node(&tn);
        for cut in [0, 3, full.len() - 5] {
            assert!(parse_tree_node(&full[..cut]).is_err(), "cut at {cut}");
        }
        assert!(parse_tree_node(&full[..full.len() - 1]).is_err());
    }

    #[test]
    fn reference_collection_walks_chunks_children_and_roots() {
        let leaf = TreeNode {
            entries: vec![file_entry("f", false, 0, 0, vec![([0xA0; 32], 1)])],
        };
        let parent = TreeNode {
            entries: vec![
                dir_entry(
                    "sub",
                    0,
                    0,
                    *blake3::hash(&serialize_tree_node(&leaf)).as_bytes(),
                ),
                file_entry("g", false, 0, 0, vec![([0xB0; 32], 2), ([0xB1; 32], 3)]),
            ],
        };
        let refs = parent.referenced_blob_ids();
        assert!(refs.contains(&[0xB0; 32]));
        assert!(refs.contains(&[0xB1; 32]));
        assert!(refs.contains(blake3::hash(&serialize_tree_node(&leaf)).as_bytes()));
        assert_eq!(refs.len(), 3);

        let m = RootManifest {
            folder_id: [0; 16],
            device_id: [0; 32],
            created_sec: 0,
            created_nsec: 0,
            root_tree_id: [0x77; 32],
            parent_manifest_id: [0xEE; 32],
        };
        assert_eq!(m.root_tree_id(), &[0x77; 32]);
    }
}

use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

use crate::format::{put_bytes, put_i64, put_u32, put_u64, BlobId, FormatError, Reader};

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("duplicate name {0:?} within one tree node")]
    DuplicateName(String),
    #[error("invalid name {0:?}: single component, no '/', NUL, '.', '..'")]
    InvalidName(String),
    #[error("name is not valid UTF-8")]
    NotUtf8,
    #[error("name {0:?} is not Unicode NFC")]
    NotNfc(String),
    #[error("reserved bits set in flags byte")]
    ReservedBitsSet,
    #[error("exec flag is only valid on files")]
    ExecFlagOnNonFile,
    #[error("mtime_nsec out of range 0..999_999_999")]
    NsecOutOfRange,
    #[error("file size {declared} != sum of chunk lengths {actual}")]
    SizeMismatch { declared: u64, actual: u64 },
    #[error("reserved field must be zero")]
    ReservedNonzero,
    #[error("entry type {0:#04x} unknown")]
    UnknownEntryType(u8),
    #[error("symlink target is not valid UTF-8")]
    BadSymlinkTarget,
    #[error("{0}")]
    Corrupt(&'static str),
}

impl From<FormatError> for ManifestError {
    fn from(e: FormatError) -> Self {
        match e {
            FormatError::ReservedNonzero => ManifestError::ReservedNonzero,
            _ => ManifestError::Corrupt("truncated"),
        }
    }
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryPayload {
    File {
        
        size: u64,
        
        chunks: Vec<(BlobId, u64)>,
    },
    Dir {
        child_tree_id: BlobId,
    },
    Symlink {
        target: String,
    },
}

impl EntryPayload {
    pub fn type_byte(&self) -> u8 {
        match self {
            EntryPayload::File { .. } => 0x00,
            EntryPayload::Dir { .. } => 0x01,
            EntryPayload::Symlink { .. } => 0x02,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    
    pub name: String,
    
    pub exec: bool,
    
    pub mtime_sec: i64,
    
    pub mtime_nsec: u32,
    pub payload: EntryPayload,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TreeNode {
    pub entries: Vec<TreeEntry>,
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootManifest {
    pub folder_id: [u8; 16],
    pub device_id: [u8; 32],
    pub created_sec: i64,
    pub created_nsec: u32,
    pub root_tree_id: BlobId,
    pub parent_manifest_id: BlobId,
}

impl RootManifest {
    pub fn root_tree_id(&self) -> &BlobId {
        &self.root_tree_id
    }
}


fn to_nfc(s: &str) -> String {
    s.nfc().collect()
}




pub fn file_entry(
    name: &str,
    exec: bool,
    mtime_sec: i64,
    mtime_nsec: u32,
    chunks: Vec<(BlobId, u64)>,
) -> TreeEntry {
    let size = chunks.iter().map(|c| c.1).sum();
    let e = TreeEntry {
        name: to_nfc(name),
        exec,
        mtime_sec,
        mtime_nsec,
        payload: EntryPayload::File { size, chunks },
    };
    expect_valid(&e);
    e
}

pub fn dir_entry(name: &str, mtime_sec: i64, mtime_nsec: u32, child_tree_id: BlobId) -> TreeEntry {
    let e = TreeEntry {
        name: to_nfc(name),
        exec: false,
        mtime_sec,
        mtime_nsec,
        payload: EntryPayload::Dir { child_tree_id },
    };
    expect_valid(&e);
    e
}

pub fn symlink_entry(name: &str, mtime_sec: i64, mtime_nsec: u32, target: &str) -> TreeEntry {
    
    let e = TreeEntry {
        name: to_nfc(name),
        exec: false,
        mtime_sec,
        mtime_nsec,
        payload: EntryPayload::Symlink {
            target: to_nfc(target),
        },
    };
    expect_valid(&e);
    e
}

fn expect_valid(e: &TreeEntry) {
    if let Err(err) = validate_entry(e) {
        panic!("invalid entry construction: {err}");
    }
}






pub fn validate_name(name: &str) -> Result<(), ManifestError> {
    if name.contains('/')
        || name.contains('\0')
        || name == "."
        || name == ".."
        || name.contains(':')
        
        
        
        || !matches!(
            std::path::Path::new(name).components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(ManifestError::InvalidName(name.to_string()));
    }
    let nfc: String = name.nfc().collect();
    if nfc != name {
        return Err(ManifestError::NotNfc(name.to_string()));
    }
    Ok(())
}

fn validate_target(target: &str) -> Result<(), ManifestError> {
    
    
    let nfc: String = target.nfc().collect();
    if nfc != target {
        return Err(ManifestError::NotNfc(target.to_string()));
    }
    Ok(())
}


pub fn validate_entry(e: &TreeEntry) -> Result<(), ManifestError> {
    validate_name(&e.name)?;
    if e.mtime_nsec > 999_999_999 {
        return Err(ManifestError::NsecOutOfRange);
    }
    match &e.payload {
        EntryPayload::File { size, chunks } => {
            let sum = chunks.iter().map(|c| c.1).sum();
            if *size != sum {
                return Err(ManifestError::SizeMismatch {
                    declared: *size,
                    actual: sum,
                });
            }
        }
        EntryPayload::Dir { .. } => {
            if e.exec {
                return Err(ManifestError::ExecFlagOnNonFile);
            }
        }
        EntryPayload::Symlink { target } => validate_target(target)?,
    }
    Ok(())
}



pub fn validate_entries(entries: &[TreeEntry]) -> Result<(), ManifestError> {
    for pair in entries.windows(2) {
        if pair[0].name.as_bytes() >= pair[1].name.as_bytes() {
            if pair[0].name == pair[1].name {
                return Err(ManifestError::DuplicateName(pair[0].name.clone()));
            }
            return Err(ManifestError::Corrupt("entries not sorted by name"));
        }
    }
    for e in entries {
        validate_entry(e)?;
    }
    Ok(())
}

fn flags_byte(e: &TreeEntry) -> Result<u8, ManifestError> {
    match &e.payload {
        EntryPayload::File { .. } => Ok(u8::from(e.exec)),
        _ => {
            if e.exec {
                return Err(ManifestError::ExecFlagOnNonFile);
            }
            Ok(0x00)
        }
    }
}




pub fn serialize_tree_node(node: &TreeNode) -> Vec<u8> {
    let mut sorted = node.entries.clone();
    for e in &mut sorted {
        e.name = to_nfc(&e.name);
        if let EntryPayload::Symlink { target } = &mut e.payload {
            *target = to_nfc(target);
        }
    }
    sorted.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    validate_entries(&sorted).expect("tree node violates determinism rules");

    let mut out = Vec::new();
    put_u32(&mut out, sorted.len() as u32);
    for e in &sorted {
        out.push(e.payload.type_byte());
        let name_bytes = e.name.as_bytes();
        put_u32(&mut out, name_bytes.len() as u32);
        put_bytes(&mut out, name_bytes);
        out.push(flags_byte(e).expect("flags validated above"));
        put_i64(&mut out, e.mtime_sec);
        put_u32(&mut out, e.mtime_nsec);
        match &e.payload {
            EntryPayload::File { size, chunks } => {
                put_u64(&mut out, *size);
                put_u32(&mut out, chunks.len() as u32);
                for (id, len) in chunks {
                    put_bytes(&mut out, id);
                    put_u64(&mut out, *len);
                }
            }
            EntryPayload::Dir { child_tree_id } => {
                put_bytes(&mut out, child_tree_id);
            }
            EntryPayload::Symlink { target } => {
                put_u32(&mut out, target.len() as u32);
                put_bytes(&mut out, target.as_bytes());
            }
        }
    }
    out
}



pub fn parse_tree_node(bytes: &[u8]) -> Result<TreeNode, ManifestError> {
    let mut r = Reader::new(bytes);
    let count = r.u32()?;
    let mut entries = Vec::new();
    for _ in 0..count {
        let type_byte = r.u8()?;
        let name_len = r.u32()? as usize;
        let name_bytes = r.take(name_len)?;
        let name = std::str::from_utf8(name_bytes)
            .map_err(|_| ManifestError::NotUtf8)?
            .to_string();
        let flags = r.u8()?;
        if flags & 0b1111_1110 != 0 {
            return Err(ManifestError::ReservedBitsSet);
        }
        let exec = flags & 0x01 != 0;
        let mtime_sec = r.i64()?;
        let mtime_nsec = r.u32()?;
        let payload = match type_byte {
            0x00 => {
                let size = r.u64()?;
                let chunk_count = r.u32()?;
                
                
                
                
                
                let mut chunks = Vec::new();
                for _ in 0..chunk_count {
                    let id = r.array()?;
                    let len = r.u64()?;
                    chunks.push((id, len));
                }
                EntryPayload::File { size, chunks }
            }
            0x01 => {
                let child_tree_id = r.array()?;
                EntryPayload::Dir { child_tree_id }
            }
            0x02 => {
                let target_len = r.u32()? as usize;
                let t = r.take(target_len)?;
                let target = std::str::from_utf8(t).map_err(|_| ManifestError::BadSymlinkTarget)?;
                EntryPayload::Symlink {
                    target: target.to_string(),
                }
            }
            other => return Err(ManifestError::UnknownEntryType(other)),
        };
        entries.push(TreeEntry {
            name,
            exec,
            mtime_sec,
            mtime_nsec,
            payload,
        });
    }
    r.expect_end()?;
    validate_entries(&entries)?;
    Ok(TreeNode { entries })
}


pub fn serialize_manifest(m: &RootManifest) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + 32 + 8 + 4 + 32 + 32 + 32);
    put_bytes(&mut out, &m.folder_id);
    put_bytes(&mut out, &m.device_id);
    put_i64(&mut out, m.created_sec);
    put_u32(&mut out, m.created_nsec);
    put_bytes(&mut out, &m.root_tree_id);
    put_bytes(&mut out, &m.parent_manifest_id);
    put_bytes(&mut out, &[0u8; 32]); 
    out
}


pub fn parse_manifest(bytes: &[u8]) -> Result<RootManifest, ManifestError> {
    let mut r = Reader::new(bytes);
    let folder_id = r.array()?;
    let device_id = r.array()?;
    let created_sec = r.i64()?;
    let created_nsec = r.u32()?;
    if created_nsec > 999_999_999 {
        return Err(ManifestError::NsecOutOfRange);
    }
    let root_tree_id = r.array()?;
    let parent_manifest_id = r.array()?;
    if r.array::<32>()? != [0u8; 32] {
        return Err(ManifestError::ReservedNonzero);
    }
    r.expect_end()?;
    Ok(RootManifest {
        folder_id,
        device_id,
        created_sec,
        created_nsec,
        root_tree_id,
        parent_manifest_id,
    })
}

impl TreeNode {
    
    
    
    pub fn referenced_blobs(&self) -> Vec<(crate::format::BlobKind, BlobId)> {
        use crate::format::BlobKind;
        let mut out = Vec::new();
        for e in &self.entries {
            match &e.payload {
                EntryPayload::File { chunks, .. } => {
                    out.extend(chunks.iter().map(|c| (BlobKind::DataChunk, c.0)));
                }
                EntryPayload::Dir { child_tree_id } => {
                    out.push((BlobKind::TreeNode, *child_tree_id));
                }
                EntryPayload::Symlink { .. } => {}
            }
        }
        out
    }

    
    pub fn referenced_blob_ids(&self) -> Vec<BlobId> {
        self.referenced_blobs()
            .into_iter()
            .map(|(_, id)| id)
            .collect()
    }
}
