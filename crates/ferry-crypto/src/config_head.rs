



















use ferry_store::format::{parse_header, put_u32, write_header, FormatError, Reader, HEADER_LEN};
use thiserror::Error;

use crate::folder_key::WRAPPED_LEN;
use crate::identity::DeviceId;


pub const BODY_PREAMBLE_LEN: usize = 16 + 4 + 4;

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


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedKeyEntry {
    pub device_pub: DeviceId,
    
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


pub fn write_config_head(folder_id: &[u8; 16], entries: &[WrappedKeyEntry]) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(HEADER_LEN + BODY_PREAMBLE_LEN + entries.len() * ENTRY_FIXED_LEN);
    out.extend_from_slice(&write_header(
        ferry_store::format::ContainerKind::ConfigHead,
    ));
    out.extend_from_slice(folder_id);
    put_u32(&mut out, 0); 
    put_u32(&mut out, entries.len() as u32);
    for e in entries {
        out.extend_from_slice(&e.device_pub);
        put_u32(&mut out, WRAPPED_LEN as u32);
        out.extend_from_slice(&e.wrapped);
    }
    out
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigHead {
    pub folder_id: [u8; 16],
    pub entries: Vec<WrappedKeyEntry>,
}


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
        
        let fmk_env = unhex::<80>(ENVELOPE_A_HEX).unwrap();
        let head = write_config_head(
            &unhex::<16>(FOLDER_ID_HEX).unwrap(),
            &[WrappedKeyEntry::new(unhex(ALICE_PK_HEX).unwrap(), fmk_env)],
        );
        assert_eq!(
            hex(&head),
            String::from(concat!(
                "46455252",                                                         
                "59",                               
                "04",                               
                "01000000",                         
                "00112233445566778899aabbccddeeff", 
                "00000000",                         
                "01000000",                         
                "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a", 
                "50000000",                         
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

        
        let mut evil = good.clone();
        evil[0] = b'X';
        assert!(matches!(
            parse_config_head(&evil),
            Err(ConfigHeadError::Format(FormatError::BadMagic))
        ));
        
        let mut evil = good.clone();
        evil[5] = 0x01;
        assert!(matches!(
            parse_config_head(&evil),
            Err(ConfigHeadError::NotConfigHead(0x01))
        ));
        
        let mut evil = good.clone();
        evil[6] = 2;
        assert!(matches!(
            parse_config_head(&evil),
            Err(ConfigHeadError::Format(FormatError::BadVersion(2, 1)))
        ));
        
        let mut evil = good.clone();
        evil[26] = 1; 
        assert!(matches!(
            parse_config_head(&evil),
            Err(ConfigHeadError::Format(FormatError::ReservedNonzero))
        ));
        
        let evil = good[..good.len() - 1].to_vec();
        assert!(parse_config_head(&evil).is_err());
    }

    #[test]
    fn wrapped_len_must_be_80() {
        
        let mut b = Vec::new();
        b.extend_from_slice(&write_header(
            ferry_store::format::ContainerKind::ConfigHead,
        ));
        b.extend_from_slice(&[0u8; 16]);
        b.extend_from_slice(&0u32.to_le_bytes()); 
        b.extend_from_slice(&1u32.to_le_bytes()); 
        b.extend_from_slice(&[9u8; 32]); 
        b.extend_from_slice(&79u32.to_le_bytes()); 
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
