pub mod light;
pub mod util;
pub mod v1_18;

pub use light::ChunkLightExt;

use pumpkin_data::packet::clientbound::play::LEVEL_CHUNK_WITH_LIGHT;
use pumpkin_protocol::ClientPacket;
use pumpkin_protocol::packet::MultiVersionJavaPacket;
use pumpkin_protocol::ser::WritingError;
use pumpkin_util::version::JavaMinecraftVersion;
use pumpkin_world::chunk::ChunkData;
use std::io::Write;

/// Sent by the server to provide the client with the full data for a chunk.
///
/// This includes heightmaps, the actual block and biome data (organized into sections),
/// block entities (like signs or chests), and the light level information for both
/// sky and block light.
pub struct CChunkData<'a>(pub &'a ChunkData);

impl MultiVersionJavaPacket for CChunkData<'_> {
    fn to_id(version: JavaMinecraftVersion) -> i32 {
        LEVEL_CHUNK_WITH_LIGHT.to_id(version)
    }
}

impl<'a> CChunkData<'a> {
    #[must_use]
    pub const fn new(chunk: &'a ChunkData) -> Self {
        Self(chunk)
    }
}

impl ClientPacket for CChunkData<'_> {
    fn write_packet_data(
        &self,
        write: impl Write,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        v1_18::write_chunk_data(self.0, write, version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumpkin_world::chunk::ChunkData;

    #[test]
    fn chunk_data_all_versions() {
        let chunk = ChunkData::empty(0, 0);
        let packet = CChunkData(&chunk);

        let versions = [JavaMinecraftVersion::V_26_3];

        for version in versions {
            let mut buf = Vec::new();
            let id = CChunkData::to_id(version);
            assert_ne!(id, -1, "Packet ID for version {version:?} must be valid");
            assert!(
                packet.write_packet_data(&mut buf, &version).is_ok(),
                "Failed to serialize chunk data for version {version:?}"
            );
            assert!(
                !buf.is_empty(),
                "Serialized buffer must not be empty for version {version:?}"
            );
        }
    }

    #[test]
    fn populated_chunk_data_all_versions() {
        let chunk = ChunkData::empty(0, 0);
        chunk
            .section
            .set_block_absolute_y(0, 64, 0, pumpkin_data::Block::STONE.default_state.id);
        chunk
            .section
            .set_block_absolute_y(1, 64, 1, pumpkin_data::Block::DIRT.default_state.id);

        let mut nbt = pumpkin_nbt::compound::NbtCompound::new();
        nbt.put_string("id", "minecraft:chest".to_string());
        chunk.pending_block_entities.lock().unwrap().insert(
            pumpkin_util::math::position::BlockPos(pumpkin_util::math::vector3::Vector3::new(
                0, 64, 0,
            )),
            nbt,
        );

        let packet = CChunkData(&chunk);

        let versions = [JavaMinecraftVersion::V_26_3];

        for version in versions {
            let mut buf = Vec::new();
            let id = CChunkData::to_id(version);
            assert_ne!(id, -1, "Packet ID for version {version:?} must be valid");
            assert!(
                packet.write_packet_data(&mut buf, &version).is_ok(),
                "Failed to serialize populated chunk data for version {version:?}"
            );
            assert!(
                !buf.is_empty(),
                "Serialized buffer must not be empty for version {version:?}"
            );
        }
    }

    /// **What the client is told about a section that is uniformly lit.**
    ///
    /// The empty mask means "all zeros" (`ClientPacketListener.readSectionList` stores a
    /// `new DataLayer()` for a section in it), so a section that is uniformly 15 has to travel as
    /// an array of 15. The light payload is the last thing in the packet, so the tail of the
    /// serialized packet is the assertion.
    #[test]
    fn a_uniform_section_is_sent_as_an_array_and_not_as_an_empty_one() {
        use pumpkin_world::chunk::format::LightContainer;

        let chunk = ChunkData::empty(0, 0);
        {
            let mut engine = chunk
                .light_engine
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut sky = vec![LightContainer::new_empty(0); 24];
            sky[0] = LightContainer::new_empty(15);
            engine.sky_light = sky.into_boxed_slice();
            engine.block_light = vec![LightContainer::new_empty(0); 24].into_boxed_slice();
        }

        let mut buf = Vec::new();
        CChunkData(&chunk)
            .write_packet_data(&mut buf, &JavaMinecraftVersion::V_26_3)
            .expect("chunk data serializes");

        // Sky light arrays: the fixed length of one array, then the array, then no block light
        // array at all. A section announced as empty would leave the count at zero instead.
        let mut expected = vec![0x01, 0x80, 0x10];
        expected.extend_from_slice(&[0xFF; 2048]);
        expected.push(0x00);
        assert!(
            buf.ends_with(&expected),
            "the section that is uniformly 15 is not in the sky light arrays: the packet ends \
             with {:?}",
            &buf[buf.len().saturating_sub(8)..],
        );
    }
}
