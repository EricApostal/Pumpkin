//! `neoforge:auxiliary_light_data` (`AuxiliaryLightDataPayload`): the light a chunk's blocks emit
//! *on top of* their block state, kept per chunk by `LevelChunkAuxiliaryLightManager`.
//!
//! A mod uses it where a block's light is not a property of its state: `CopycatBlock` adopts the
//! emission of the block a copycat copies (`CopycatBlock.getLightEmission` reads it back through
//! `Level.getAuxLightManager`), and `CopycatBlockEntity.updateLight` is what writes it.
//!
//! `NeoForge` sends one payload to a client together with every `LEVEL_CHUNK_WITH_LIGHT` it sends
//! (`PlayerChunkSender.sendChunk` bundles the two), empty map and all, and the client *replaces*
//! the whole map of that chunk with it (`ClientPayloadHandler.handle` →
//! `LevelChunkAuxiliaryLightManager.handleLightDataSync`). It is therefore not an optional extra: a
//! client that is given a chunk without it keeps whatever it had for that chunk — nothing, on join
//! — and a chunk whose blocks do carry auxiliary light would never be lit there.
//!
//! The host keeps no auxiliary light: no block a plugin registers can emit light dynamically (in
//! `pumpkin-plugin-wit` a block's emission is a property of its registered state) and no chunk
//! stores a per-position light map, so there is nothing to read. Every payload this module encodes
//! is the empty map, which is the exact truth about the world the host serves.

use bytes::Bytes;
use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::CCustomPayload;
use pumpkin_protocol::ser::NetworkWriteExt;
use pumpkin_protocol::{ClientPacket, MultiVersionJavaPacket};

use super::channels;

/// The body of the payload for the chunk at `(chunk_x, chunk_z)`: the packed `ChunkPos` and the map
/// of light entries.
///
/// `FriendlyByteBuf.writeChunkPos` writes the `long` of `ChunkPos.pack` (`x` in the low 32 bits,
/// `z` in the high ones) big-endian, and `ByteBufCodecs.map` writes a `VarInt` count — zero here,
/// see the module docs.
#[must_use]
pub const fn empty_payload(chunk_x: i32, chunk_z: i32) -> [u8; 9] {
    let packed = ((chunk_x as u32 as u64) | ((chunk_z as u32 as u64) << 32)).to_be_bytes();
    [
        packed[0], packed[1], packed[2], packed[3], packed[4], packed[5], packed[6], packed[7], 0,
    ]
}

/// The serialized `CUSTOM_PAYLOAD` packet (packet id, channel, payload) that tells a `NeoForge`
/// client the auxiliary light of the chunk at `(chunk_x, chunk_z)`.
///
/// `None` only if framing the packet fails, which the channel and the nine fixed bytes cannot.
#[must_use]
pub fn packet_bytes(chunk_x: i32, chunk_z: i32) -> Option<Bytes> {
    let mut buf = Vec::with_capacity(16 + channels::AUXILIARY_LIGHT_DATA.len());
    buf.write_var_int(&VarInt(CCustomPayload::to_id(CURRENT_MC_VERSION)))
        .ok()?;
    CCustomPayload::new(
        channels::AUXILIARY_LIGHT_DATA,
        &empty_payload(chunk_x, chunk_z),
    )
    .write_packet_data(&mut buf, &CURRENT_MC_VERSION)
    .ok()?;
    Some(Bytes::from(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumpkin_protocol::ser::NetworkReadExt;

    /// The bytes of a payload, as the `AuxiliaryLightDataPayload` codec reads them: the chunk pos it
    /// names, then the entries.
    #[test]
    fn payload_names_its_chunk_and_carries_no_entries() {
        for (x, z) in [(0, -1), (1, -2), (-1, -1), (0, 0), (-2, 3)] {
            let payload = empty_payload(x, z);
            let mut read = &payload[..];
            let packed = read.get_u64().unwrap();
            assert_eq!(packed as u32 as i32, x, "chunk x of {payload:?}");
            assert_eq!((packed >> 32) as u32 as i32, z, "chunk z of {payload:?}");
            assert_eq!(read.get_var_int().unwrap().0, 0, "no entries");
            assert!(read.is_empty());
        }
    }

    /// **The bytes of the reference**, as mctrace recorded them for the nine chunks of the
    /// differential's contraption run (`client-reference-java.jsonl`, `neoforge:auxiliary_light_data`
    /// ×9, in the order the server sent them).
    ///
    /// Every one of them is the empty map of one chunk of the client's 3×3 neighbourhood, which is
    /// what says the channel carries no light in this rig: the reference is byte-identical to what
    /// this host now sends for the same chunk.
    #[test]
    fn bytes_are_what_the_reference_sent() {
        let reference: [(i32, i32, [u8; 9]); 9] = [
            (0, -1, [0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0]),
            (1, -1, [0xff, 0xff, 0xff, 0xff, 0, 0, 0, 1, 0]),
            (-1, -1, [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0]),
            (0, 0, [0, 0, 0, 0, 0, 0, 0, 0, 0]),
            (0, -2, [0xff, 0xff, 0xff, 0xfe, 0, 0, 0, 0, 0]),
            (1, 0, [0, 0, 0, 0, 0, 0, 0, 1, 0]),
            (-1, -2, [0xff, 0xff, 0xff, 0xfe, 0xff, 0xff, 0xff, 0xff, 0]),
            (-1, 0, [0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0]),
            (1, -2, [0xff, 0xff, 0xff, 0xfe, 0, 0, 0, 1, 0]),
        ];
        for (x, z, expected) in reference {
            assert_eq!(empty_payload(x, z), expected, "payload of chunk ({x}, {z})");
        }
    }

    /// The packet is a `CUSTOM_PAYLOAD` on the channel of the payload, so the client routes it to
    /// the auxiliary light handler.
    #[test]
    fn packet_is_the_auxiliary_light_channel() {
        let bytes = packet_bytes(3, -4).expect("encodes");
        let mut read = &bytes[..];
        assert_eq!(
            read.get_var_int().unwrap().0,
            CCustomPayload::to_id(CURRENT_MC_VERSION)
        );
        assert_eq!(&*read.get_str().unwrap(), channels::AUXILIARY_LIGHT_DATA);
        assert_eq!(read.get_u64().unwrap() as u32 as i32, 3);
        assert_eq!(read.get_var_int().unwrap().0, 0);
        assert!(read.is_empty());
    }
}
