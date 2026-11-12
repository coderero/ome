use ome_core::symbol::{SYMBOL_LEN, Symbol};
use std::fmt;

pub const SCHEMA_ID: u16 = 7;
pub const SCHEMA_VERSION: u16 = 1;
pub const HEADER_LEN: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    UnknownTemplate(u16),
    WrongSchema(u16),
    Invalid(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated => write!(f, "message truncated"),
            DecodeError::UnknownTemplate(id) => write!(f, "unknown template id {id}"),
            DecodeError::WrongSchema(id) => write!(f, "unexpected schema id {id}"),
            DecodeError::Invalid(what) => write!(f, "invalid field: {what}"),
        }
    }
}

impl std::error::Error for DecodeError {}

pub struct Encoder {
    buf: Vec<u8>,
}

pub struct GroupWriter {
    header_at: usize,
    entry_start: usize,
    block_set: bool,
}

impl Encoder {
    pub fn new(template_id: u16) -> Self {
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&template_id.to_le_bytes());
        buf.extend_from_slice(&SCHEMA_ID.to_le_bytes());
        buf.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
        Self { buf }
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn symbol(&mut self, symbol: Option<Symbol>) -> &mut Self {
        let bytes = symbol.map_or([0u8; SYMBOL_LEN], |s| s.as_bytes());
        self.buf.extend_from_slice(&bytes);
        self
    }

    pub fn bytes32(&mut self, v: &[u8; 32]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }

    pub fn end_block(&mut self) -> &mut Self {
        let block_length = (self.buf.len() - HEADER_LEN) as u16;
        self.buf[0..2].copy_from_slice(&block_length.to_le_bytes());
        self
    }

    pub fn begin_group(&mut self, count: usize) -> GroupWriter {
        let header_at = self.buf.len();
        self.u16(0).u16(count.min(u16::MAX as usize) as u16);
        GroupWriter {
            header_at,
            entry_start: self.buf.len(),
            block_set: false,
        }
    }

    pub fn begin_entry(&mut self, group: &mut GroupWriter) {
        group.entry_start = self.buf.len();
    }

    pub fn end_entry_block(&mut self, group: &mut GroupWriter) {
        if !group.block_set {
            let block_length = (self.buf.len() - group.entry_start) as u16;
            self.buf[group.header_at..group.header_at + 2]
                .copy_from_slice(&block_length.to_le_bytes());
            group.block_set = true;
        }
    }

    pub fn var(&mut self, text: &str) -> &mut Self {
        let mut end = text.len().min(u16::MAX as usize);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.u16(end as u16);
        self.buf.extend_from_slice(&text.as_bytes()[..end]);
        self
    }

    pub fn var_bytes(&mut self, data: &[u8]) -> &mut Self {
        self.u32(data.len() as u32);
        self.buf.extend_from_slice(data);
        self
    }

    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }
}

pub struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
    block_end: usize,
    pub template_id: u16,
    pub version: u16,
}

pub struct GroupReader {
    pub block_length: usize,
    pub count: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(buf: &'a [u8]) -> Result<Self, DecodeError> {
        if buf.len() < HEADER_LEN {
            return Err(DecodeError::Truncated);
        }
        let read = |at: usize| u16::from_le_bytes([buf[at], buf[at + 1]]);
        let block_length = read(0) as usize;
        let template_id = read(2);
        let schema_id = read(4);
        let version = read(6);
        if schema_id != SCHEMA_ID {
            return Err(DecodeError::WrongSchema(schema_id));
        }
        Ok(Self {
            buf,
            pos: HEADER_LEN,
            block_end: HEADER_LEN + block_length,
            template_id,
            version,
        })
    }

    pub fn peek_template(buf: &[u8]) -> Result<u16, DecodeError> {
        Ok(Decoder::new(buf)?.template_id)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(DecodeError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes(
            b.try_into().map_err(|_| DecodeError::Truncated)?,
        ))
    }

    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes(
            b.try_into().map_err(|_| DecodeError::Truncated)?,
        ))
    }

    pub fn optional_symbol(&mut self) -> Result<Option<Symbol>, DecodeError> {
        let bytes: [u8; SYMBOL_LEN] = self
            .take(SYMBOL_LEN)?
            .try_into()
            .map_err(|_| DecodeError::Truncated)?;
        if bytes == [0; SYMBOL_LEN] {
            return Ok(None);
        }
        Symbol::from_bytes(bytes)
            .map(Some)
            .ok_or(DecodeError::Invalid("symbol"))
    }

    pub fn symbol(&mut self) -> Result<Symbol, DecodeError> {
        self.optional_symbol()?
            .ok_or(DecodeError::Invalid("symbol"))
    }

    pub fn bytes32(&mut self) -> Result<[u8; 32], DecodeError> {
        self.take(32)?
            .try_into()
            .map_err(|_| DecodeError::Truncated)
    }

    pub fn end_block(&mut self) -> Result<(), DecodeError> {
        if self.pos > self.block_end {
            return Err(DecodeError::Invalid("block length shorter than fields"));
        }
        if self.block_end > self.buf.len() {
            return Err(DecodeError::Truncated);
        }
        self.pos = self.block_end;
        Ok(())
    }

    pub fn group(&mut self) -> Result<GroupReader, DecodeError> {
        let block_length = self.u16()? as usize;
        let count = self.u16()? as usize;
        Ok(GroupReader {
            block_length,
            count,
        })
    }

    pub fn entry_start(&self) -> usize {
        self.pos
    }

    pub fn end_entry_block(
        &mut self,
        start: usize,
        group: &GroupReader,
    ) -> Result<(), DecodeError> {
        let end = start + group.block_length;
        if self.pos > end || end > self.buf.len() {
            return Err(DecodeError::Invalid("group block length"));
        }
        self.pos = end;
        Ok(())
    }

    pub fn var(&mut self) -> Result<String, DecodeError> {
        let len = self.u16()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::Invalid("utf-8 text"))
    }

    pub fn var_bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_carries_block_length_and_template() {
        let mut e = Encoder::new(42);
        e.u64(7).u8(1).end_block().var("hi");
        let bytes = e.finish();

        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 9);
        let mut d = Decoder::new(&bytes).unwrap();
        assert_eq!(d.template_id, 42);
        assert_eq!(d.u64().unwrap(), 7);
        assert_eq!(d.u8().unwrap(), 1);
        d.end_block().unwrap();
        assert_eq!(d.var().unwrap(), "hi");
    }

    #[test]
    fn decoder_skips_unknown_trailing_fields() {
        let mut e = Encoder::new(1);
        e.u64(5).u64(999).end_block().var("tail");
        let bytes = e.finish();

        let mut d = Decoder::new(&bytes).unwrap();
        assert_eq!(d.u64().unwrap(), 5);
        d.end_block().unwrap();
        assert_eq!(d.var().unwrap(), "tail");
    }

    #[test]
    fn truncated_input_is_an_error_not_a_panic() {
        let mut e = Encoder::new(1);
        e.u64(5).end_block().var("hello");
        let bytes = e.finish();
        for cut in 0..bytes.len() {
            let result = Decoder::new(&bytes[..cut]).and_then(|mut d| {
                d.u64()?;
                d.end_block()?;
                d.var()
            });
            assert!(result.is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn wrong_schema_is_rejected() {
        let mut bytes = Encoder::new(1).end_block().finish();
        bytes[4] = 99;
        assert_eq!(
            Decoder::new(&bytes).err(),
            Some(DecodeError::WrongSchema(99))
        );
    }

    #[test]
    fn var_truncates_on_char_boundary() {
        let long = "é".repeat(40_000);
        let bytes = Encoder::new(1).end_block().var(&long).finish();
        let mut d = Decoder::new(&bytes).unwrap();
        d.end_block().unwrap();
        let text = d.var().unwrap();
        assert!(text.len() <= u16::MAX as usize);
        assert!(text.chars().all(|c| c == 'é'));
    }
}
