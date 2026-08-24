//! Length-prefixed frames: the outermost wire unit.
//!
//! Normative layout (`docs/store-format.md`, "Wire protocol v1"):
//!
//! ```text
//! u32 BE body_len     # length of `body` below; frames are read whole
//! body                # pre-auth:  type || version || payload  (cleartext)
//!                     # post-auth: AEAD(type || version || payload)
//! ```
//!
//! The 4-byte length prefix is little work and big safety: a reader never
//! buffers more than [`MAX_FRAME_BODY`] bytes on a hostile peer's say-so,
//! and after auth the prefix is bound into the AEAD as AAD so truncation or
//! splicing between frames breaks authentication.
//!
//! Every frame carries its message type byte and protocol version. After
//! auth those fields live INSIDE the sealed region: observers see lengths
//! only, mirroring how pack encryption hides blob geometry at rest.

use std::io::{Read, Write};

use crate::error::ProtoError;

/// Hard ceiling on one frame's body. A conforming sender never exceeds it;
/// a receiver seeing more treats it as a resource-limit violation rather
/// than allocating (the `DoS` guard). Sized to carry a worst-case spec pack
/// (16 MiB target + 8 MiB max-chunk overshoot + footer/tag overhead) in one
/// [`PackItem`](crate::codec::PackItem).
pub const MAX_FRAME_BODY: usize = 64 * 1024 * 1024;

/// Read exactly one frame body given its already-read length prefix.
///
/// Internal to the module pair below; exposed pub(crate) for tests.
pub(crate) fn read_body(reader: &mut impl Read) -> Result<Vec<u8>, ProtoError> {
    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes)?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len > MAX_FRAME_BODY {
        return Err(ProtoError::FrameTooLarge {
            len,
            max: MAX_FRAME_BODY,
        });
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(body)
}

pub(crate) fn write_body(writer: &mut impl Write, body: &[u8]) -> Result<(), ProtoError> {
    if body.len() > MAX_FRAME_BODY {
        return Err(ProtoError::FrameTooLarge {
            len: body.len(),
            max: MAX_FRAME_BODY,
        });
    }
    // ONE write_all per frame keeps in-memory test transports record-aligned;
    // real streams do not care.
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
    buf.extend_from_slice(body);
    writer.write_all(&buf)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::duplex_pair;

    #[test]
    fn bodies_round_trip_with_length_prefix() {
        let (mut a, mut b) = duplex_pair();
        write_body(&mut a, b"hello").unwrap();
        write_body(&mut a, b"").unwrap();
        assert_eq!(read_body(&mut b).unwrap(), b"hello");
        assert_eq!(read_body(&mut b).unwrap(), b"");
    }

    #[test]
    fn oversize_outgoing_is_rejected_before_touching_the_wire() {
        let (mut a, _b) = duplex_pair();
        let err = write_body(&mut a, &vec![0u8; MAX_FRAME_BODY + 1]).unwrap_err();
        assert!(matches!(err, ProtoError::FrameTooLarge { .. }), "{err}");
    }

    #[test]
    fn oversize_incoming_never_allocates_and_errors_typed() {
        let (mut a, mut b) = duplex_pair();
        // Hand-craft a lying prefix claiming MAX+1 bytes.
        a.write_all(&((MAX_FRAME_BODY as u32) + 1).to_be_bytes())
            .unwrap();
        let err = read_body(&mut b).unwrap_err();
        assert!(
            matches!(
                err,
                ProtoError::FrameTooLarge { len, .. } if len == MAX_FRAME_BODY + 1
            ),
            "{err}"
        );
    }

    #[test]
    fn truncated_frame_surfaces_io_error_not_garbage() {
        let (mut a, mut b) = duplex_pair();
        // Announce 100 bytes, deliver only the prefix and 40 of them.
        let mut partial = Vec::new();
        partial.extend_from_slice(&100u32.to_be_bytes());
        partial.extend_from_slice(&[0u8; 40]);
        a.write_all(&partial).unwrap();
        a.close();

        let err = read_body(&mut b).unwrap_err();
        assert!(matches!(err, ProtoError::Io(_)), "{err}");
    }
}
