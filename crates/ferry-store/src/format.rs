





use thiserror::Error;


pub const MAGIC: [u8; 5] = *b"FERRY";


pub const FORMAT_VERSION: u32 = 1;

pub const HEADER_LEN: usize = 10;


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContainerKind {
    PackData = 0x01,
    PackMeta = 0x02,
    Index = 0x03,
    ConfigHead = 0x04,
}

impl ContainerKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(ContainerKind::PackData),
            0x02 => Some(ContainerKind::PackMeta),
            0x03 => Some(ContainerKind::Index),
            0x04 => Some(ContainerKind::ConfigHead),
            _ => None,
        }
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BlobKind {
    DataChunk = 0x01,
    TreeNode = 0x02,
    Manifest = 0x03,
    Polynomial = 0x04,
}

impl BlobKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(BlobKind::DataChunk),
            0x02 => Some(BlobKind::TreeNode),
            0x03 => Some(BlobKind::Manifest),
            0x04 => Some(BlobKind::Polynomial),
            _ => None,
        }
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }

    
    pub fn is_meta(self) -> bool {
        !matches!(self, BlobKind::DataChunk)
    }
}



pub type BlobId = [u8; 32];

pub type PackId = [u8; 32];

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("bad magic bytes")]
    BadMagic,
    #[error("unknown container kind {0:#04x}")]
    UnknownKind(u8),
    #[error("unsupported format_version {0} (expected {1})")]
    BadVersion(u32, u32),
    #[error("truncated container: need {need} bytes, have {have}")]
    Truncated { need: usize, have: usize },
    #[error("reserved field must be zero")]
    ReservedNonzero,
}


pub fn write_header(kind: ContainerKind) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..5].copy_from_slice(&MAGIC);
    h[5] = kind as u8;
    h[6..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    h
}


pub fn parse_header(bytes: &[u8]) -> Result<ContainerKind, FormatError> {
    if bytes.len() < HEADER_LEN {
        return Err(FormatError::Truncated {
            need: HEADER_LEN,
            have: bytes.len(),
        });
    }
    if bytes[..5] != MAGIC {
        return Err(FormatError::BadMagic);
    }
    let kind = match bytes[5] {
        0x01 => ContainerKind::PackData,
        0x02 => ContainerKind::PackMeta,
        0x03 => ContainerKind::Index,
        0x04 => ContainerKind::ConfigHead,
        other => return Err(FormatError::UnknownKind(other)),
    };
    let version = u32::from_le_bytes(bytes[6..10].try_into().unwrap());
    if version != FORMAT_VERSION {
        return Err(FormatError::BadVersion(version, FORMAT_VERSION));
    }
    Ok(kind)
}



pub fn put_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

pub fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn put_i64(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn put_bytes(out: &mut Vec<u8>, v: &[u8]) {
    out.extend_from_slice(v);
}


pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], FormatError> {
        let end = self.pos.checked_add(n).ok_or(FormatError::Truncated {
            need: usize::MAX,
            have: self.buf.len(),
        })?;
        if end > self.buf.len() {
            return Err(FormatError::Truncated {
                need: end,
                have: self.buf.len(),
            });
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn u8(&mut self) -> Result<u8, FormatError> {
        Ok(self.take(1)?[0])
    }

    pub fn u32(&mut self) -> Result<u32, FormatError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn u64(&mut self) -> Result<u64, FormatError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn i64(&mut self) -> Result<i64, FormatError> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], FormatError> {
        Ok(self.take(N)?.try_into().unwrap())
    }

    pub fn rest(&mut self) -> &'a [u8] {
        let s = &self.buf[self.pos..];
        self.pos = self.buf.len();
        s
    }

    
    
    pub fn expect_end(&mut self) -> Result<(), FormatError> {
        if self.pos != self.buf.len() {
            return Err(FormatError::Truncated {
                need: self.pos,
                have: self.buf.len(),
            });
        }
        Ok(())
    }
}


pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap());
        s.push(char::from_digit(u32::from(b & 0xf), 16).unwrap());
    }
    s
}


pub fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    let b = s.as_bytes();
    for i in 0..N {
        let hi = (b[2 * i] as char).to_digit(16)?;
        let lo = (b[2 * i + 1] as char).to_digit(16)?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Some(out)
}
