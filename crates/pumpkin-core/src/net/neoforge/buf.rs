//! A minimal reader and writer for the `VarInt` / string encodings of the NeoForge payloads.

use std::fmt;

/// A payload that cannot be decoded, or a value that cannot be encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecError(pub String);

impl CodecError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CodecError {}

/// Vanilla's `FriendlyByteBuf.MAX_STRING_LENGTH`, in characters.
const MAX_STRING_CHARS: usize = 32767;

/// The most elements a decoder accepts in one list or map that the client controls. Real
/// clients send far fewer.
pub const MAX_ELEMENTS: usize = 4096;

#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn byte(&mut self, value: u8) {
        self.buf.push(value);
    }

    pub fn bool(&mut self, value: bool) {
        self.buf.push(u8::from(value));
    }

    pub fn bytes(&mut self, value: &[u8]) {
        self.buf.extend_from_slice(value);
    }

    pub fn short(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    pub fn int(&mut self, value: i32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    #[allow(clippy::cast_sign_loss)]
    pub fn var_int(&mut self, value: i32) {
        let mut value = value as u32;
        loop {
            let low = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.buf.push(low);
                return;
            }
            self.buf.push(low | 0x80);
        }
    }

    /// A collection length or an ordinal.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn var_len(&mut self, value: usize) {
        self.var_int(value as i32);
    }

    pub fn string(&mut self, value: &str) {
        self.var_len(value.len());
        self.buf.extend_from_slice(value.as_bytes());
    }

    /// A `VarInt` length and the bytes.
    pub fn byte_array(&mut self, value: &[u8]) {
        self.var_len(value.len());
        self.buf.extend_from_slice(value);
    }
}

pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// Fails when bytes are left, vanilla rejects a payload with trailing bytes.
    pub fn finish(self) -> Result<(), CodecError> {
        if self.data.is_empty() {
            Ok(())
        } else {
            Err(CodecError::new(format!(
                "{} unexpected trailing byte(s)",
                self.data.len()
            )))
        }
    }

    pub fn byte(&mut self) -> Result<u8, CodecError> {
        let (&first, rest) = self
            .data
            .split_first()
            .ok_or_else(|| CodecError::new("unexpected end of data"))?;
        self.data = rest;
        Ok(first)
    }

    pub fn bool(&mut self) -> Result<bool, CodecError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(CodecError::new(format!("{other} is not a boolean"))),
        }
    }

    pub fn bytes(&mut self, len: usize) -> Result<&'a [u8], CodecError> {
        if self.data.len() < len {
            return Err(CodecError::new("unexpected end of data"));
        }
        let (head, rest) = self.data.split_at(len);
        self.data = rest;
        Ok(head)
    }

    pub fn short(&mut self) -> Result<u16, CodecError> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub fn int(&mut self) -> Result<i32, CodecError> {
        let bytes = self.bytes(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    #[allow(clippy::cast_possible_wrap)]
    pub fn var_int(&mut self) -> Result<i32, CodecError> {
        let mut value = 0u32;
        for shift in 0..5 {
            let byte = self.byte()?;
            value |= u32::from(byte & 0x7f) << (shift * 7);
            if byte & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        Err(CodecError::new("VarInt is too big"))
    }

    /// A non negative `VarInt`, at most `max`.
    pub fn var_len(&mut self, max: usize) -> Result<usize, CodecError> {
        let value = self.var_int()?;
        usize::try_from(value)
            .ok()
            .filter(|&len| len <= max)
            .ok_or_else(|| CodecError::new(format!("length {value} is out of range (max {max})")))
    }

    pub fn string(&mut self) -> Result<String, CodecError> {
        let len = self.var_len(MAX_STRING_CHARS * 3)?;
        let text = std::str::from_utf8(self.bytes(len)?)
            .map_err(|_| CodecError::new("string is not UTF-8"))?;
        if text.chars().count() > MAX_STRING_CHARS {
            return Err(CodecError::new("string is too long"));
        }
        Ok(text.to_string())
    }

    pub fn byte_array(&mut self) -> Result<Vec<u8>, CodecError> {
        let len = self.var_len(self.data.len())?;
        Ok(self.bytes(len)?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn var_int_round_trips() {
        for value in [0, 1, 127, 128, 300, 25565, i32::MAX, -1, i32::MIN] {
            let mut writer = Writer::new();
            writer.var_int(value);
            let bytes = writer.into_bytes();
            let mut reader = Reader::new(&bytes);
            assert_eq!(reader.var_int().unwrap(), value);
            reader.finish().unwrap();
        }
        let mut writer = Writer::new();
        writer.var_int(300);
        assert_eq!(writer.into_bytes(), [0xac, 0x02]);
    }

    #[test]
    fn strings_and_trailing_bytes() {
        let mut writer = Writer::new();
        writer.string("neoforge:network");
        writer.byte(9);
        let bytes = writer.into_bytes();
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.string().unwrap(), "neoforge:network");
        assert!(reader.finish().is_err());
        assert!(Reader::new(&[0x05, b'a']).string().is_err());
    }
}
