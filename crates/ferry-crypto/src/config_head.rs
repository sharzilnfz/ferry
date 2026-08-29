//! `CONFIG_HEAD` container: the plaintext folder bootstrap record.
//!
//! Byte layout from `docs/store-format.md` ("Folder layout"), byte-for-byte:
//!
//! ```text
//! file header (10B): "FERRY" | kind 0x04 | format_version 1 (u32 LE)
//! 16B folder_id
//! u32 LE reserved (zeros)
//! u32 LE wrapped_key_count
//! per wrapped key, in order given:
//!     32B device_x25519_pub
//!     u32 LE wrapped_len        # MUST be 80 in v1
//!     wrapped                   # the X25519 wrap envelope
//! ```
//!
//! This container is NOT encrypted and holds no secrets — only public ids
//! and key-wrapping ciphertexts. Readers reject unknown magic/kind/version,
//! nonzero reserved fields, and `wrapped_len != 80`; there is no best-effort
//! parsing anywhere near trust boundaries.

use ferry_store::format::{parse_header, put_u32, write_header, FormatError, Reader, HEADER_LEN};
use thiserror::Error;

use crate::folder_key::WRAPPED_LEN;
use crate::identity::DeviceId;

/// Body bytes after the fixed prologue before per-entry data.
pub const BODY_PREAMBLE_LEN: usize = 16 + 4 + 4;
/// One entry: pub(32) + len(4) + wrapped(80).
pub const ENTRY_FIXED_LEN: usize = 32 + 4 + WRAPPED_LEN;

#[derive(Debug, Error)]
pub enum ConfigHeadError {
    #[error(transparent)]
    Format(#[from] FormatError),
    #[error("container kind {0:#04x} is not CONFIG_HEAD (0x04)")]
    NotConfigHead(u8),
    #[error("wrapped_len MUST be 80 in v1, got {0}")]
    BadWrappedLen(u32),
}

/// One recipient entry of the `CONFIG_HEAD` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedKeyEntry {
    pub device_pub: DeviceId,
    /// The 80-byte X25519 wrap envelope ([`crate::folder_key`]).
    pub wrapped: [u8; WRAPPED_LEN],
}

impl WrappedKeyEntry {
    pub fn new(device_pub: DeviceId, wrapped: [u8; WRAPPED_LEN]) -> Self {
        WrappedKeyEntry {
            device_pub,
            wrapped,
        }
    }
}

/// Serialize a complete `CONFIG_HEAD` container (header + body).
pub fn write_config_head(folder_id: &[u8; 16], entries: &[WrappedKeyEntry]) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(HEADER_LEN + BODY_PREAMBLE_LEN + entries.len() * ENTRY_FIXED_LEN);
    out.extend_from_slice(&write_header(
        ferry_store::format::ContainerKind::ConfigHead,
    ));
    out.extend_from_slice(folder_id);
    put_u32(&mut out, 0); // reserved
    put_u32(&mut out, entries.len() as u32);
    for e in entries {
        out.extend_from_slice(&e.device_pub);
        put_u32(&mut out, WRAPPED_LEN as u32);
        out.extend_from_slice(&e.wrapped);
    }
    out
}

/// Parsed `CONFIG_HEAD` contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigHead {
    pub folder_id: [u8; 16],
    pub entries: Vec<WrappedKeyEntry>,
}

/// Parse a complete `CONFIG_HEAD` container, enforcing every v1 rule.
pub fn parse_config_head(bytes: &[u8]) -> Result<ConfigHead, ConfigHeadError> {
    let kind = parse_header(bytes)?;
    if kind != ferry_store::format::ContainerKind::ConfigHead {
        return Err(ConfigHeadError::NotConfigHead(kind.to_u8()));
    }
    let mut r = Reader::new(&bytes[HEADER_LEN..]);
    let folder_id = r.array::<16>()?;
    if r.u32()? != 0 {
        return Err(FormatError::ReservedNonzero.into());
    }
    let count = r.u32()?;
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let device_pub = r.array::<32>()?;
        let wrapped_len = r.u32()?;
        if wrapped_len != WRAPPED_LEN as u32 {
            return Err(ConfigHeadError::BadWrappedLen(wrapped_len));
        }
        let wrapped = r.array::<WRAPPED_LEN>()?;
        entries.push(WrappedKeyEntry {
            device_pub,
            wrapped,
        });
    }
    r.expect_end()?;
    Ok(ConfigHead { folder_id, entries })
}

// keep the trailing re-export section empty; helpers come from ferry-store

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_store::format::{hex, unhex};

    const FOLDER_ID_HEX: &str = "00112233445566778899aabbccddeeff";
    const ALICE_PK_HEX: &str = "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a";
    const BOB_PK_HEX: &str = "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f";
    const ENVELOPE_A_HEX: &str = "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f\
         f59b0b9d840ca51536831c1af980f10ca51ac5030c2a56bab74061b5f68749\
         e835e64c6ec363b6ff0f670500b7cb59be";

    #[test]
    fn hand_computed_bytes_pin_the_full_container() {
        // Every byte here is derivable by hand from the spec table:
        let fmk_env = unhex::<80>(ENVELOPE_A_HEX).unwrap();
        let head = write_config_head(
            &unhex::<16>(FOLDER_ID_HEX).unwrap(),
            &[WrappedKeyEntry::new(unhex(ALICE_PK_HEX).unwrap(), fmk_env)],
        );
        assert_eq!(
            hex(&head),
            String::from(concat!(
                "46455252",                                                         // "FERR"
                "59",                               // "Y"     magic complete
                "04",                               // kind = CONFIG_HEAD
                "01000000",                         // format_version 1 LE
                "00112233445566778899aabbccddeeff", // folder_id
                "00000000",                         // reserved zeros
                "01000000",                         // wrapped_key_count = 1 LE
                "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a", // device pub
                "50000000",                         // wrapped_len = 80 = 0x50 LE
            )) + ENVELOPE_A_HEX,
            "CONFIG_HEAD must match the spec byte-for-byte"
        );
        assert_eq!(head.len(), 10 + 16 + 4 + 4 + 32 + 4 + 80);
    }

    #[test]
    fn parse_round_trips_multiple_entries_in_order() {
        let fmk_env = unhex::<80>(ENVELOPE_A_HEX).unwrap();
        let mut other = fmk_env;
        other[79] ^= 1;
        let entries = vec![
            WrappedKeyEntry::new(unhex(ALICE_PK_HEX).unwrap(), fmk_env),
            WrappedKeyEntry::new(unhex(BOB_PK_HEX).unwrap(), other),
        ];
        let bytes = write_config_head(&unhex::<16>(FOLDER_ID_HEX).unwrap(), &entries);
        let parsed = parse_config_head(&bytes).unwrap();
        assert_eq!(parsed.folder_id, unhex::<16>(FOLDER_ID_HEX).unwrap());
        assert_eq!(parsed.entries, entries);
    }

    #[test]
    fn readers_reject_spec_violations_loudly() {
        let fmk_env = unhex::<80>(ENVELOPE_A_HEX).unwrap();
        let good = write_config_head(
            &unhex::<16>(FOLDER_ID_HEX).unwrap(),
            &[WrappedKeyEntry::new(unhex(ALICE_PK_HEX).unwrap(), fmk_env)],
        );

        // bad magic
        let mut evil = good.clone();
        evil[0] = b'X';
        assert!(matches!(
            parse_config_head(&evil),
            Err(ConfigHeadError::Format(FormatError::BadMagic))
        ));
        // wrong kind: a valid PACK_DATA header is not a CONFIG_HEAD
        let mut evil = good.clone();
        evil[5] = 0x01;
        assert!(matches!(
            parse_config_head(&evil),
            Err(ConfigHeadError::NotConfigHead(0x01))
        ));
        // unknown version
        let mut evil = good.clone();
        evil[6] = 2;
        assert!(matches!(
            parse_config_head(&evil),
            Err(ConfigHeadError::Format(FormatError::BadVersion(2, 1)))
        ));
        // nonzero reserved
        let mut evil = good.clone();
        evil[26] = 1; // first reserved byte after folder_id
        assert!(matches!(
            parse_config_head(&evil),
            Err(ConfigHeadError::Format(FormatError::ReservedNonzero))
        ));
        // truncated mid-entry
        let evil = good[..good.len() - 1].to_vec();
        assert!(parse_config_head(&evil).is_err());
    }

    #[test]
    fn wrapped_len_must_be_80() {
        // A well-formed header/body whose entry claims wrapped_len 79.
        let mut b = Vec::new();
        b.extend_from_slice(&write_header(
            ferry_store::format::ContainerKind::ConfigHead,
        ));
        b.extend_from_slice(&[0u8; 16]);
        b.extend_from_slice(&0u32.to_le_bytes()); // reserved
        b.extend_from_slice(&1u32.to_le_bytes()); // one entry
        b.extend_from_slice(&[9u8; 32]); // pub
        b.extend_from_slice(&79u32.to_le_bytes()); // BAD length
        b.extend_from_slice(&[0u8; 79]);
        match parse_config_head(&b) {
            Err(ConfigHeadError::BadWrappedLen(79)) => {}
            other => panic!("expected BadWrappedLen(79), got {other:?}"),
        }
    }

    #[test]
    fn empty_entry_list_is_valid() {
        let bytes = write_config_head(&[5u8; 16], &[]);
        let parsed = parse_config_head(&bytes).unwrap();
        assert_eq!(parsed.entries.len(), 0);
        assert_eq!(parsed.folder_id, [5u8; 16]);
        assert_eq!(bytes.len(), HEADER_LEN + BODY_PREAMBLE_LEN);
    }
}
