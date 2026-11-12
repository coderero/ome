use std::io::{self, Read, Write};

pub const SOFH_LEN: usize = 6;
pub const SBE_LITTLE_ENDIAN: u16 = 0x5BE0;
pub const MAX_FRAME_LEN: usize = 1 << 20;

pub fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(SOFH_LEN + payload.len());
    out.extend_from_slice(&((SOFH_LEN + payload.len()) as u32).to_be_bytes());
    out.extend_from_slice(&SBE_LITTLE_ENDIAN.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn write_frame(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    writer.write_all(&frame(payload))
}

pub fn read_frame(reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; SOFH_LEN];
    match reader.read_exact(&mut header[..1]) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    reader.read_exact(&mut header[1..])?;

    let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let encoding = u16::from_be_bytes([header[4], header[5]]);
    if encoding != SBE_LITTLE_ENDIAN {
        return Err(invalid(format!(
            "unsupported encoding type {encoding:#06x}"
        )));
    }
    if !(SOFH_LEN..=MAX_FRAME_LEN).contains(&len) {
        return Err(invalid(format!("invalid frame length {len}")));
    }

    let mut payload = vec![0u8; len - SOFH_LEN];
    reader.read_exact(&mut payload)?;
    Ok(Some(payload))
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frames_round_trip_back_to_back() {
        let mut wire = Vec::new();
        write_frame(&mut wire, b"first").unwrap();
        write_frame(&mut wire, b"").unwrap();
        write_frame(&mut wire, b"third").unwrap();

        let mut reader = Cursor::new(wire);
        assert_eq!(read_frame(&mut reader).unwrap().unwrap(), b"first");
        assert_eq!(read_frame(&mut reader).unwrap().unwrap(), b"");
        assert_eq!(read_frame(&mut reader).unwrap().unwrap(), b"third");
        assert!(read_frame(&mut reader).unwrap().is_none());
    }

    #[test]
    fn header_is_big_endian_length_then_encoding() {
        let bytes = frame(b"ab");
        assert_eq!(&bytes[..6], &[0, 0, 0, 8, 0x5B, 0xE0]);
    }

    #[test]
    fn rejects_bad_encoding_oversize_and_partial_frames() {
        let mut bad = frame(b"x");
        bad[4] = 0xEB;
        assert!(read_frame(&mut Cursor::new(bad)).is_err());

        let mut huge = frame(b"x");
        huge[..4].copy_from_slice(&(MAX_FRAME_LEN as u32 + 1).to_be_bytes());
        assert!(read_frame(&mut Cursor::new(huge)).is_err());

        let partial = frame(b"hello")[..8].to_vec();
        assert!(read_frame(&mut Cursor::new(partial)).is_err());
    }
}
