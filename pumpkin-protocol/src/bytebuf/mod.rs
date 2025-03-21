use core::str;

use crate::{
    FixedBitSet,
    codec::{
        Codec, DecodeError, bit_set::BitSet, identifier::Identifier, var_int::VarInt,
        var_long::VarLong,
    },
};
use bytes::{Buf, BufMut};

mod deserializer;
use thiserror::Error;
pub mod packet;
pub mod serializer;

#[derive(Debug, Error)]
pub enum ReadingError {
    /// End-of-File
    #[error("EOF, Tried to read {0}, but there are no bytes left to consume")]
    EOF(String),
    #[error("{0} is incomplete")]
    Incomplete(String),
    #[error("{0} is too large")]
    TooLarge(String),
    #[error("{0}")]
    Message(String),
}

impl From<bytes::TryGetError> for ReadingError {
    fn from(error: bytes::TryGetError) -> Self {
        Self::EOF(error.requested.to_string())
    }
}

pub trait ByteBuf: Buf {
    fn try_get_bool(&mut self) -> Result<bool, ReadingError>;

    fn try_copy_to_bytes(&mut self, len: usize) -> Result<bytes::Bytes, ReadingError>;

    fn try_copy_to_bytes_len(
        &mut self,
        len: usize,
        max_length: usize,
    ) -> Result<bytes::Bytes, ReadingError>;

    fn try_get_var_int(&mut self) -> Result<VarInt, ReadingError>;

    fn try_get_var_long(&mut self) -> Result<VarLong, ReadingError>;

    fn try_get_identifier(&mut self) -> Result<Identifier, ReadingError>;

    fn try_get_string(&mut self) -> Result<String, ReadingError>;

    fn try_get_string_len(&mut self, max_size: usize) -> Result<String, ReadingError>;

    /// Reads a boolean. If true, the closure is called, and the returned value is
    /// wrapped in Some. Otherwise, this returns None.
    fn try_get_option<G>(
        &mut self,
        val: impl FnOnce(&mut Self) -> Result<G, ReadingError>,
    ) -> Result<Option<G>, ReadingError>;

    fn get_list<G>(
        &mut self,
        val: impl Fn(&mut Self) -> Result<G, ReadingError>,
    ) -> Result<Vec<G>, ReadingError>;

    fn try_get_uuid(&mut self) -> Result<uuid::Uuid, ReadingError>;

    fn try_get_fixed_bitset(&mut self, bits: usize) -> Result<FixedBitSet, ReadingError>;
}

impl<T: Buf> ByteBuf for T {
    fn try_get_bool(&mut self) -> Result<bool, ReadingError> {
        Ok(self.try_get_u8()? != 0)
    }

    fn try_copy_to_bytes(&mut self, len: usize) -> Result<bytes::Bytes, ReadingError> {
        if self.remaining() >= len {
            Ok(self.copy_to_bytes(len))
        } else {
            Err(ReadingError::Message("Unable to copy bytes".to_string()))
        }
    }

    fn try_copy_to_bytes_len(
        &mut self,
        len: usize,
        max_size: usize,
    ) -> Result<bytes::Bytes, ReadingError> {
        if len > max_size {
            return Err(ReadingError::Message(
                "Tried to copy bytes, but length exceeds maximum length".to_string(),
            ));
        }
        if self.remaining() >= len {
            Ok(self.copy_to_bytes(len))
        } else {
            Err(ReadingError::Message("Unable to copy bytes".to_string()))
        }
    }

    fn try_get_var_int(&mut self) -> Result<VarInt, ReadingError> {
        match VarInt::decode(self) {
            Ok(var_int) => Ok(var_int),
            Err(error) => match error {
                DecodeError::Incomplete => Err(ReadingError::Incomplete("varint".to_string())),
                DecodeError::TooLarge => Err(ReadingError::TooLarge("varint".to_string())),
            },
        }
    }
    fn try_get_var_long(&mut self) -> Result<VarLong, ReadingError> {
        match VarLong::decode(self) {
            Ok(var_long) => Ok(var_long),
            Err(error) => match error {
                DecodeError::Incomplete => Err(ReadingError::Incomplete("varint".to_string())),
                DecodeError::TooLarge => Err(ReadingError::TooLarge("varlong".to_string())),
            },
        }
    }

    fn try_get_string(&mut self) -> Result<String, ReadingError> {
        self.try_get_string_len(i16::MAX as usize)
    }

    fn try_get_string_len(&mut self, max_size: usize) -> Result<String, ReadingError> {
        let size = self.try_get_var_int()?.0;
        if size as usize > max_size {
            return Err(ReadingError::TooLarge("string".to_string()));
        }

        let data = self.try_copy_to_bytes(size as usize)?;
        if data.len() > max_size {
            return Err(ReadingError::TooLarge("string".to_string()));
        }
        String::from_utf8(data.to_vec()).map_err(|e| ReadingError::Message(e.to_string()))
    }

    fn try_get_option<G>(
        &mut self,
        val: impl FnOnce(&mut Self) -> Result<G, ReadingError>,
    ) -> Result<Option<G>, ReadingError> {
        if self.try_get_bool()? {
            Ok(Some(val(self)?))
        } else {
            Ok(None)
        }
    }

    fn get_list<G>(
        &mut self,
        val: impl Fn(&mut Self) -> Result<G, ReadingError>,
    ) -> Result<Vec<G>, ReadingError> {
        let len = self.try_get_var_int()?.0 as usize;
        let mut list = Vec::with_capacity(len);
        for _ in 0..len {
            list.push(val(self)?);
        }
        Ok(list)
    }

    fn try_get_uuid(&mut self) -> Result<uuid::Uuid, ReadingError> {
        let mut bytes = [0u8; 16];
        self.try_copy_to_slice(&mut bytes)?;
        Ok(uuid::Uuid::from_slice(&bytes).expect("Failed to parse UUID"))
    }

    fn try_get_fixed_bitset(&mut self, bits: usize) -> Result<FixedBitSet, ReadingError> {
        self.try_copy_to_bytes(bits.div_ceil(8))
    }

    fn try_get_identifier(&mut self) -> Result<Identifier, ReadingError> {
        match Identifier::decode(self) {
            Ok(identifier) => Ok(identifier),
            Err(error) => match error {
                DecodeError::Incomplete => Err(ReadingError::Incomplete("identifier".to_string())),
                DecodeError::TooLarge => Err(ReadingError::TooLarge("identifier".to_string())),
            },
        }
    }
}

pub trait ByteBufMut {
    fn put_bool(&mut self, v: bool);

    fn put_uuid(&mut self, v: &uuid::Uuid);

    fn put_string(&mut self, val: &str);

    fn put_string_len(&mut self, val: &str, max_size: usize);

    fn put_string_array(&mut self, array: &[&str]);

    fn put_bit_set(&mut self, set: &BitSet);

    /// Writes `true` if the option is Some, or `false` if None. If the option is
    /// some, then it also calls the `write` closure.
    fn put_option<G>(&mut self, val: &Option<G>, write: impl FnOnce(&mut Self, &G));

    fn put_list<G>(&mut self, list: &[G], write: impl Fn(&mut Self, &G));

    fn put_identifier(&mut self, val: &Identifier);

    fn put_var_int(&mut self, value: &VarInt);

    fn put_varint_arr(&mut self, v: &[i32]);
}

impl<T: BufMut> ByteBufMut for T {
    fn put_bool(&mut self, v: bool) {
        if v {
            self.put_u8(1);
        } else {
            self.put_u8(0);
        }
    }

    fn put_uuid(&mut self, v: &uuid::Uuid) {
        // thats the vanilla way
        let pair = v.as_u64_pair();
        self.put_u64(pair.0);
        self.put_u64(pair.1);
    }

    fn put_string(&mut self, val: &str) {
        self.put_string_len(val, i16::MAX as usize);
    }

    fn put_string_len(&mut self, val: &str, max_size: usize) {
        if val.len() > max_size {
            // Should be panic?, I mean its our fault
            panic!("String is too big");
        }
        self.put_var_int(&val.len().into());
        self.put(val.as_bytes());
    }

    fn put_string_array(&mut self, array: &[&str]) {
        for string in array {
            self.put_string(string)
        }
    }

    fn put_var_int(&mut self, var_int: &VarInt) {
        var_int.encode(self);
    }

    fn put_bit_set(&mut self, bit_set: &BitSet) {
        bit_set.encode(self);
    }

    fn put_option<G>(&mut self, val: &Option<G>, write: impl FnOnce(&mut Self, &G)) {
        self.put_bool(val.is_some());
        if let Some(v) = val {
            write(self, v)
        }
    }

    fn put_list<G>(&mut self, list: &[G], write: impl Fn(&mut Self, &G)) {
        self.put_var_int(&list.len().into());
        for v in list {
            write(self, v);
        }
    }

    fn put_varint_arr(&mut self, v: &[i32]) {
        self.put_list(v, |p, &v| p.put_var_int(&v.into()))
    }

    fn put_identifier(&mut self, val: &Identifier) {
        val.encode(self);
    }
}

#[cfg(test)]
mod test {
    use bytes::{Bytes, BytesMut};
    use serde::{Deserialize, Serialize};

    use crate::{
        VarInt,
        bytebuf::{deserializer, serializer},
    };

    #[test]
    fn test_i32_reserialize() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
        struct Foo {
            bar: i32,
        }
        let foo = Foo { bar: 69 };
        let mut bytes = BytesMut::new();
        let mut serializer = serializer::Serializer::new(&mut bytes);
        foo.serialize(&mut serializer).unwrap();

        let deserialized: Foo =
            Foo::deserialize(deserializer::Deserializer::new(&mut Bytes::from(bytes))).unwrap();

        assert_eq!(foo, deserialized);
    }

    #[test]
    fn test_varint_reserialize() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
        struct Foo {
            bar: VarInt,
        }
        let foo = Foo { bar: 69.into() };
        let mut bytes = BytesMut::new();
        let mut serializer = serializer::Serializer::new(&mut bytes);
        foo.serialize(&mut serializer).unwrap();

        let deserialized: Foo =
            Foo::deserialize(deserializer::Deserializer::new(&mut Bytes::from(bytes))).unwrap();

        assert_eq!(foo, deserialized);
    }
}
