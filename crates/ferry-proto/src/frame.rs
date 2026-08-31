use std::io::{Read, Write};

use crate::error::ProtoError;

pub const MAX_FRAME_BODY: usize = 64 * 1024 * 1024;

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

        let mut partial = Vec::new();
        partial.extend_from_slice(&100u32.to_be_bytes());
        partial.extend_from_slice(&[0u8; 40]);
        a.write_all(&partial).unwrap();
        a.close();

        let err = read_body(&mut b).unwrap_err();
        assert!(matches!(err, ProtoError::Io(_)), "{err}");
    }
}
