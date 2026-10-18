//! The part of network NBT that the text components of the negotiation need.
//!
//! `neoforge:modded_network_setup_failed` carries components, which vanilla writes as network NBT:
//! the tag type byte, then the payload, with no root name. A plain text is a string tag, a
//! translatable one a compound `{translate, with}`.

use super::buf::{CodecError, Reader, Writer};

const TAG_END: u8 = 0;
const TAG_STRING: u8 = 8;
const TAG_LIST: u8 = 9;
const TAG_COMPOUND: u8 = 10;

/// A text component: literal text, or a translation key with arguments the client translates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NbtText {
    Literal(String),
    Translatable { key: String, args: Vec<NbtText> },
}

impl NbtText {
    pub fn literal(text: impl Into<String>) -> Self {
        Self::Literal(text.into())
    }

    pub fn translatable(key: impl Into<String>, args: Vec<Self>) -> Self {
        Self::Translatable {
            key: key.into(),
            args,
        }
    }

    /// Writes the component as network NBT.
    pub fn write(&self, writer: &mut Writer) -> Result<(), CodecError> {
        match self {
            Self::Literal(text) => {
                // A component without style or children is a bare string tag.
                writer.byte(TAG_STRING);
                write_nbt_string(writer, text)
            }
            Self::Translatable { .. } => {
                writer.byte(TAG_COMPOUND);
                self.write_compound_payload(writer)
            }
        }
    }

    /// The payload of a compound tag holding this component, as a list element.
    fn write_compound_payload(&self, writer: &mut Writer) -> Result<(), CodecError> {
        match self {
            Self::Literal(text) => {
                write_named(writer, TAG_STRING, "text")?;
                write_nbt_string(writer, text)?;
            }
            Self::Translatable { key, args } => {
                write_named(writer, TAG_STRING, "translate")?;
                write_nbt_string(writer, key)?;
                if !args.is_empty() {
                    write_named(writer, TAG_LIST, "with")?;
                    // Lists are homogeneous: every argument is a compound.
                    writer.byte(TAG_COMPOUND);
                    writer.int(i32::try_from(args.len()).map_err(|_| CodecError::new("too many"))?);
                    for arg in args {
                        arg.write_compound_payload(writer)?;
                    }
                }
            }
        }
        writer.byte(TAG_END);
        Ok(())
    }

    /// Reads a component written by [`Self::write`], or a plain string tag or a
    /// `{text}` / `{translate, with}` compound.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        let tag = reader.byte()?;
        Self::read_payload(reader, tag)
    }

    fn read_payload(reader: &mut Reader<'_>, tag: u8) -> Result<Self, CodecError> {
        if tag == TAG_STRING {
            return Ok(Self::Literal(read_nbt_string(reader)?));
        }
        if tag != TAG_COMPOUND {
            return Err(CodecError::new(format!(
                "unsupported NBT tag type {tag} in a text component"
            )));
        }
        let mut text = None;
        let mut key = None;
        let mut args = Vec::new();
        loop {
            let entry = reader.byte()?;
            if entry == TAG_END {
                break;
            }
            let name = read_nbt_string(reader)?;
            match (name.as_str(), entry) {
                ("text", TAG_STRING) => text = Some(read_nbt_string(reader)?),
                ("translate", TAG_STRING) => key = Some(read_nbt_string(reader)?),
                ("with", TAG_LIST) => {
                    let element = reader.byte()?;
                    let count = reader.int()?;
                    if !(0..=256).contains(&count) {
                        return Err(CodecError::new("too many text arguments"));
                    }
                    for _ in 0..count {
                        args.push(Self::read_payload(reader, element)?);
                    }
                }
                _ => {
                    return Err(CodecError::new(format!(
                        "unsupported text component field '{name}'"
                    )));
                }
            }
        }
        match (text, key) {
            (Some(text), _) => Ok(Self::Literal(text)),
            (None, Some(key)) => Ok(Self::Translatable { key, args }),
            (None, None) => Err(CodecError::new("text component without text or translate")),
        }
    }
}

fn write_named(writer: &mut Writer, tag: u8, name: &str) -> Result<(), CodecError> {
    writer.byte(tag);
    write_nbt_string(writer, name)
}

/// Java's modified UTF-8: a 16 bit byte length, NUL as two bytes, supplementary characters as
/// two 3 byte surrogates.
#[must_use]
pub fn encode_modified_utf8(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for unit in text.encode_utf16() {
        match unit {
            1..=0x7f => out.push(unit as u8),
            0 | 0x80..=0x7ff => {
                out.push(0xc0 | (unit >> 6) as u8);
                out.push(0x80 | (unit & 0x3f) as u8);
            }
            _ => {
                out.push(0xe0 | (unit >> 12) as u8);
                out.push(0x80 | ((unit >> 6) & 0x3f) as u8);
                out.push(0x80 | (unit & 0x3f) as u8);
            }
        }
    }
    out
}

fn decode_modified_utf8(bytes: &[u8]) -> Result<String, CodecError> {
    let bad = || CodecError::new("malformed modified UTF-8 in an NBT string");
    let mut units = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            units.push(u16::from(b));
            i += 1;
        } else if b & 0xe0 == 0xc0 && i + 1 < bytes.len() {
            units.push((u16::from(b & 0x1f) << 6) | u16::from(bytes[i + 1] & 0x3f));
            i += 2;
        } else if b & 0xf0 == 0xe0 && i + 2 < bytes.len() {
            units.push(
                (u16::from(b & 0x0f) << 12)
                    | (u16::from(bytes[i + 1] & 0x3f) << 6)
                    | u16::from(bytes[i + 2] & 0x3f),
            );
            i += 3;
        } else {
            return Err(bad());
        }
    }
    String::from_utf16(&units).map_err(|_| bad())
}

fn write_nbt_string(writer: &mut Writer, text: &str) -> Result<(), CodecError> {
    let bytes = encode_modified_utf8(text);
    let len = u16::try_from(bytes.len())
        .map_err(|_| CodecError::new(format!("NBT string of {} bytes is too long", bytes.len())))?;
    writer.short(len);
    writer.bytes(&bytes);
    Ok(())
}

fn read_nbt_string(reader: &mut Reader<'_>) -> Result<String, CodecError> {
    let len = usize::from(reader.short()?);
    decode_modified_utf8(reader.bytes(len)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(text: &NbtText) -> Vec<u8> {
        let mut writer = Writer::new();
        text.write(&mut writer).unwrap();
        writer.into_bytes()
    }

    #[test]
    fn literal_is_a_string_tag() {
        assert_eq!(encode(&NbtText::literal("hi")), [8, 0, 2, b'h', b'i']);
    }

    #[test]
    fn translatable_round_trips() {
        let text = NbtText::translatable(
            "neoforge.network.negotiation.failure.mod",
            vec![
                NbtText::literal("Mod"),
                NbtText::translatable("neoforge.network.negotiation.failure.version.mismatch", vec![
                    NbtText::literal("1"),
                    NbtText::literal("2"),
                ]),
            ],
        );
        let bytes = encode(&text);
        assert_eq!(bytes[0], TAG_COMPOUND);
        let mut reader = Reader::new(&bytes);
        assert_eq!(NbtText::read(&mut reader).unwrap(), text);
        reader.finish().unwrap();
    }

    #[test]
    fn translatable_without_arguments_has_no_with_list() {
        let bytes = encode(&NbtText::translatable("k", vec![]));
        assert_eq!(
            bytes,
            [10, 8, 0, 9, b't', b'r', b'a', b'n', b's', b'l', b'a', b't', b'e', 0, 1, b'k', 0]
        );
    }

    #[test]
    fn modified_utf8_matches_java() {
        assert_eq!(encode_modified_utf8("a\0"), [b'a', 0xc0, 0x80]);
        // U+1F600 is the surrogate pair D83D DE00, each as 3 bytes.
        assert_eq!(
            encode_modified_utf8("\u{1F600}"),
            [0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80]
        );
        let text = "caf\u{e9} \u{1F600} \0";
        assert_eq!(
            decode_modified_utf8(&encode_modified_utf8(text)).unwrap(),
            text
        );
    }
}
