//! Decoded view of the packets a [`crate::bot::HeadlessBot`] received.
//!
//! Tests use this to assert on client-visible state: "the bot got `ADD_ENTITY` for entity 42",
//! "a `BLOCK_UPDATE` placed stone at p", "`CUSTOM_PAYLOAD` on channel X carried payload Y".
//! Everything here decodes from the raw bytes that crossed the socket, never from server state.

use std::collections::BTreeMap;

use bytes::Bytes;
use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_protocol::{
    RawPacket, ServerPacket,
    codec::bit_set::BitSet,
    java::client::play::{
        CBlockEntityData, CBlockUpdate, CBundleDelimiter, CChunkData, CCustomPayload,
        CMultiBlockUpdate, CSpawnEntity, CUnloadChunk,
    },
    packet::MultiVersionJavaPacket,
    ser::NetworkReadExt,
};
use pumpkin_util::math::position::BlockPos;

/// One packet exactly as it arrived from the server (id already var-int decoded by the codec).
#[derive(Clone, Debug)]
pub struct CapturedPacket {
    pub id: i32,
    pub payload: Bytes,
}

/// Decoded `CUSTOM_PAYLOAD` (play, id 24 in protocol 777).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomPayload {
    pub channel: String,
    pub data: Vec<u8>,
}

/// Decoded `SET_ENTITY_DATA` (play, id 101 in protocol 777).
///
/// `metadata` is the raw metadata stream (index/type/value entries terminated by `0xFF`),
/// left undecoded so tests can assert on exact bytes when needed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityMetadata {
    pub entity_id: i32,
    pub metadata: Vec<u8>,
}

/// Ordered log of every packet the bot read.
#[derive(Default)]
pub struct PacketCapture {
    packets: Vec<CapturedPacket>,
}

impl PacketCapture {
    pub fn push(&mut self, packet: RawPacket) {
        self.packets.push(CapturedPacket {
            id: packet.id,
            payload: packet.payload,
        });
    }

    pub fn clear(&mut self) {
        self.packets.clear();
    }

    pub fn all(&self) -> &[CapturedPacket] {
        &self.packets
    }

    pub fn len(&self) -> usize {
        self.packets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    pub fn packets(&self, id: i32) -> impl Iterator<Item = &CapturedPacket> {
        self.packets.iter().filter(move |packet| packet.id == id)
    }

    pub fn count(&self, id: i32) -> usize {
        self.packets(packet_id(id)).count()
    }

    pub fn first(&self, id: i32) -> Option<&CapturedPacket> {
        self.packets(packet_id(id)).next()
    }

    // -- Typed accessors ---------------------------------------------------------------

    /// `ADD_ENTITY` (id 1): every entity spawn the client was told about.
    pub fn add_entities(&self) -> Result<Vec<CSpawnEntity>, String> {
        self.packets(CSpawnEntity::to_id(CURRENT_MC_VERSION))
            .map(CapturedPacket::add_entity)
            .collect()
    }

    /// `CUSTOM_PAYLOAD` (id 24), optionally filtered by channel.
    pub fn custom_payloads(&self, channel: Option<&str>) -> Result<Vec<CustomPayload>, String> {
        let id = CCustomPayload::to_id(CURRENT_MC_VERSION);
        self.packets(id)
            .map(CapturedPacket::custom_payload)
            .collect::<Result<Vec<_>, String>>()
            .map(|payloads| {
                payloads
                    .into_iter()
                    .filter(|payload| channel.is_none_or(|wanted| payload.channel == wanted))
                    .collect()
            })
    }

    /// `SET_ENTITY_DATA` (id 101): metadata updates keyed by entity id.
    pub fn entity_metadata(&self) -> Result<Vec<EntityMetadata>, String> {
        let id =
            pumpkin_protocol::java::client::play::CSetEntityMetadata::to_id(CURRENT_MC_VERSION);
        self.packets(id)
            .map(|packet| {
                let mut slice = &packet.payload[..];
                let entity_id = slice
                    .get_var_int()
                    .map_err(|err| format!("failed to decode SET_ENTITY_DATA entity id: {err}"))?
                    .0;
                Ok(EntityMetadata {
                    entity_id,
                    metadata: slice.to_vec(),
                })
            })
            .collect()
    }

    /// `BLOCK_UPDATE` (id 8): single block changes.
    pub fn block_updates(&self) -> Result<Vec<CBlockUpdate>, String> {
        self.packets(CBlockUpdate::to_id(CURRENT_MC_VERSION))
            .map(CapturedPacket::block_update)
            .collect()
    }

    /// `SECTION_BLOCKS_UPDATE` (id 86): batched block changes.
    pub fn section_block_updates(&self) -> Result<Vec<CMultiBlockUpdate>, String> {
        self.packets(CMultiBlockUpdate::to_id(CURRENT_MC_VERSION))
            .map(CapturedPacket::section_block_update)
            .collect()
    }

    /// Every block change from either `BLOCK_UPDATE` or `SECTION_BLOCKS_UPDATE`,
    /// in arrival order.
    pub fn all_block_changes(&self) -> Result<Vec<(BlockPos, i32)>, String> {
        let mut changes: Vec<(BlockPos, i32)> = self
            .block_updates()?
            .into_iter()
            .map(|update| (update.location, update.state_id.0))
            .collect();
        for batch in self.section_block_updates()? {
            changes.extend(
                batch
                    .updates
                    .into_iter()
                    .map(|(pos, state)| (pos, state.as_u32() as i32)),
            );
        }
        Ok(changes)
    }

    /// `BLOCK_ENTITY_DATA`: the block-entity payloads the client was told about.
    ///
    /// This is what carries a plugin block entity's client-visible state — a belt's items, a
    /// track's `Connections`, a machine's working state.
    pub fn block_entity_data(&self) -> Result<Vec<CBlockEntityData>, String> {
        self.packets(CBlockEntityData::to_id(CURRENT_MC_VERSION))
            .map(CapturedPacket::block_entity_data)
            .collect()
    }

    /// The chunk columns this client was sent, from `LEVEL_CHUNK_WITH_LIGHT`.
    ///
    /// Only the coordinates are decoded — a client discards any entity outside a chunk it has
    /// loaded, so "was the chunk sent" is a delivery precondition that does not need the full
    /// section payload.
    pub fn sent_chunks(&self) -> Result<Vec<(i32, i32)>, String> {
        self.packets(CChunkData::to_id(CURRENT_MC_VERSION))
            .map(CapturedPacket::chunk)
            .collect()
    }

    /// Folds `BUNDLE_DELIMITER`-delimited runs into groups, in arrival order.
    ///
    /// A delimiter opens a bundle and the matching delimiter closes it. A NeoForge client defers
    /// every packet inside until the closing delimiter arrives, so "the payload is in the same
    /// bundle as the entity" is a precondition for the entity to exist when the payload is
    /// handled — and a payload that lands *outside* the delimiters is applied to an entity the
    /// client has already discarded.
    #[must_use]
    pub fn bundles(&self) -> Vec<Vec<CapturedPacket>> {
        let delimiter = CBundleDelimiter::to_id(CURRENT_MC_VERSION);
        let mut bundles = Vec::new();
        let mut open: Option<Vec<CapturedPacket>> = None;
        for packet in &self.packets {
            if packet.id == delimiter {
                match open.take() {
                    Some(bundle) => bundles.push(bundle),
                    None => open = Some(Vec::new()),
                }
            } else if let Some(bundle) = open.as_mut() {
                bundle.push(packet.clone());
            }
        }
        // An unterminated bundle is still reported, so a missing closing delimiter is visible
        // rather than silently dropping the packets.
        if let Some(bundle) = open {
            bundles.push(bundle);
        }
        bundles
    }

    /// The unbundled packets, in arrival order — everything no delimiter wrapped.
    #[must_use]
    pub fn unbundled(&self) -> Vec<CapturedPacket> {
        let delimiter = CBundleDelimiter::to_id(CURRENT_MC_VERSION);
        let mut in_bundle = false;
        let mut out = Vec::new();
        for packet in &self.packets {
            if packet.id == delimiter {
                in_bundle = !in_bundle;
            } else if !in_bundle {
                out.push(packet.clone());
            }
        }
        out
    }

    /// The client-visible view: what this client was shown, independent of how it arrived.
    ///
    /// Two clients shown the same world must produce equal views, which is what makes this the
    /// comparable unit for the relogin diff.
    pub fn client_view(&self) -> Result<ClientView, String> {
        let mut view = ClientView::default();
        for (pos, state) in self.all_block_changes()? {
            view.blocks.insert((pos.0.x, pos.0.y, pos.0.z), state);
        }
        for packet in self.block_entity_data()? {
            view.block_entities.insert(
                (
                    packet.location.0.x,
                    packet.location.0.y,
                    packet.location.0.z,
                ),
                (packet.r#type.0, packet.nbt_data.to_vec()),
            );
        }
        for entity in self.add_entities()? {
            view.entities
                .insert(entity.entity_id.0, entity.r#type.0);
        }
        Ok(view)
    }
}

/// What one client was shown: block states and block-entity payloads by position, spawned
/// entities by id. Ordered so two views compare structurally.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientView {
    pub blocks: BTreeMap<(i32, i32, i32), i32>,
    pub block_entities: BTreeMap<(i32, i32, i32), (i32, Vec<u8>)>,
    pub entities: BTreeMap<i32, i32>,
}

impl ClientView {
    /// Positions with a block-entity payload in `self` that `other` never saw.
    ///
    /// For the relogin diff: whatever a late joiner is told about that an early client was not
    /// is exactly state the server failed to push when it changed.
    #[must_use]
    pub fn block_entities_missing_from<'a>(
        &'a self,
        other: &'a Self,
    ) -> Vec<&'a (i32, i32, i32)> {
        self.block_entities
            .keys()
            .filter(|pos| !other.block_entities.contains_key(pos))
            .collect()
    }

    /// Block positions in `self` that `other` never saw.
    #[must_use]
    pub fn blocks_missing_from<'a>(&'a self, other: &'a Self) -> Vec<&'a (i32, i32, i32)> {
        self.blocks
            .keys()
            .filter(|pos| !other.blocks.contains_key(pos))
            .collect()
    }
}

/// A block entity a `LEVEL_CHUNK_WITH_LIGHT` packet carried.
///
/// A client gets a block entity's client data in two ways: `BLOCK_ENTITY_DATA` when it changes
/// while the client is there, and inside the chunk packet when the column is sent. A joining
/// client therefore learns a plugin block entity's state from the chunk, which is invisible in a
/// trace that only reports the chunk's coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkBlockEntity {
    pub pos: BlockPos,
    pub type_id: i32,
    /// The wire bytes of the NBT compound, as the client reads them.
    pub nbt: Vec<u8>,
}

/// One heightmap a `LEVEL_CHUNK_WITH_LIGHT` carried.
///
/// The client files it under [`ChunkHeightmap::index`] (`Heightmap.Types`: 1 world surface,
/// 4 motion blocking, 5 motion blocking no leaves), and reads the packed heights from the
/// world's minimum Y: 9 bits per column, 7 columns per long, low column first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkHeightmap {
    pub index: i32,
    pub data: Vec<i64>,
}

impl ChunkHeightmap {
    /// The height the map reports for the column `(x, z)` of the chunk, in **block** Y.
    ///
    /// `min_y` is the world's minimum Y section floor (overworld: -64) — the packet itself does
    /// not carry it, a client knows it from the dimension.
    #[must_use]
    pub fn height_at(&self, min_y: i32, local_x: usize, local_z: usize) -> i32 {
        let column = local_z * 16 + local_x;
        let packed = self.data.get(column / 7).copied().unwrap_or(0);
        let value = (packed >> ((column % 7) * 9)) & 0x1FF;
        min_y + value as i32
    }

    /// A stable digest of the packed heights, for "did this column's heightmap change".
    #[must_use]
    pub fn digest(&self) -> u64 {
        let mut digest = 0xcbf29ce484222325u64;
        for value in &self.data {
            for byte in value.to_be_bytes() {
                digest = fnv1a_byte(digest, byte);
            }
        }
        digest
    }
}

/// Which of the two light layers of a chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightLayer {
    Sky,
    Block,
}

/// How one section of one light layer is described by the payload.
///
/// This mirrors what a client does with the packet (`LightData` in
/// `pumpkin-protocol/src/java/client/play/light_update.rs`, read by the client's
/// `readSectionList`): a section in the light mask carries a 2048-byte array, one in the empty
/// mask is all zeros, and one in neither mask is not mentioned at all — a client keeps whatever
/// layer it already had (which is an all-15 default for a lit sky section it has no data for).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionLight {
    /// A 2048-byte array of 16³ nibbles (low nibble of the first byte is x=0,y=0,z=0).
    Array(Vec<u8>),
    /// The empty mask says the section is all zeros.
    Empty,
    /// Neither mask mentions the section.
    Absent,
}

/// The light of one `LEVEL_CHUNK_WITH_LIGHT`, decoded from the payload's tail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkLight {
    pub sky_light_mask: u64,
    pub block_light_mask: u64,
    pub empty_sky_light_mask: u64,
    pub empty_block_light_mask: u64,
    pub sky_light_arrays: Vec<Vec<u8>>,
    pub block_light_arrays: Vec<Vec<u8>>,
}

impl ChunkLight {
    fn masks(&self, layer: LightLayer) -> (u64, u64) {
        match layer {
            LightLayer::Sky => (self.sky_light_mask, self.empty_sky_light_mask),
            LightLayer::Block => (self.block_light_mask, self.empty_block_light_mask),
        }
    }

    fn arrays(&self, layer: LightLayer) -> &[Vec<u8>] {
        match layer {
            LightLayer::Sky => &self.sky_light_arrays,
            LightLayer::Block => &self.block_light_arrays,
        }
    }

    /// The section `section` of `layer`, counted from the world's lowest section (`bit = section + 1`).
    #[must_use]
    pub fn section(&self, layer: LightLayer, section: usize) -> SectionLight {
        let bit = section + 1;
        if bit >= 64 {
            return SectionLight::Absent;
        }
        let (light_mask, empty_mask) = self.masks(layer);
        if empty_mask & (1u64 << bit) != 0 {
            return SectionLight::Empty;
        }
        if light_mask & (1u64 << bit) == 0 {
            return SectionLight::Absent;
        }
        // The arrays are in increasing bit order among the bits the mask sets.
        let index = (0..bit).filter(|i| light_mask & (1u64 << i) != 0).count();
        match self.arrays(layer).get(index) {
            Some(array) => SectionLight::Array(array.clone()),
            None => SectionLight::Absent,
        }
    }

    /// The light nibble of `layer` at the world block `(x, y, z)`, as the payload states it.
    ///
    /// `None` when the section is [`SectionLight::Absent`]: the packet does not state a value for
    /// that cell, so a client keeps the light it already had.
    #[must_use]
    pub fn cell(&self, layer: LightLayer, min_y: i32, x: i32, y: i32, z: i32) -> Option<u8> {
        let section = (y - min_y).div_euclid(16);
        let section = usize::try_from(section).ok()?;
        let (lx, ly, lz) = (
            x.rem_euclid(16) as usize,
            y.rem_euclid(16) as usize,
            z.rem_euclid(16) as usize,
        );
        match self.section(layer, section) {
            SectionLight::Array(array) => {
                let index = ((ly << 4) | lz) << 4 | lx;
                let byte = *array.get(index >> 1)?;
                Some((byte >> ((index & 1) * 4)) & 0x0F)
            }
            SectionLight::Empty => Some(0),
            SectionLight::Absent => None,
        }
    }

    /// A digest of everything the payload says about one layer: the arrays, in order.
    ///
    /// Two sends of the same world state have the same digest; a difference is a real difference
    /// in what the client was told, not a formatting one.
    #[must_use]
    pub fn digest(&self, layer: LightLayer) -> u64 {
        let mut digest = 0xcbf29ce484222325u64;
        let (light_mask, empty_mask) = self.masks(layer);
        for byte in light_mask.to_be_bytes() {
            digest = fnv1a_byte(digest, byte);
        }
        for byte in empty_mask.to_be_bytes() {
            digest = fnv1a_byte(digest, byte);
        }
for array in self.arrays(layer) {
            for byte in array {
                digest = fnv1a_byte(digest, *byte);
            }
        }
        digest
    }
}

fn fnv1a_byte(digest: u64, byte: u8) -> u64 {
    (digest ^ u64::from(byte)).wrapping_mul(0x100000001b3)
}

/// Decoding of a single captured packet.
///
/// The accessors above collect packets of one kind; these decode one packet each, which is what
/// the streaming client trace needs (`crate::client_trace` writes a line per packet as it
/// arrives). Both go through the same decoders, so a test and a trace can never disagree.
impl CapturedPacket {
    /// Whether this packet is a `BUNDLE_DELIMITER` — it frames a bundle and carries no data.
    #[must_use]
    pub fn is_bundle_delimiter(&self) -> bool {
        self.id == CBundleDelimiter::to_id(CURRENT_MC_VERSION)
    }

    /// `ADD_ENTITY` (id 1).
    pub fn add_entity(&self) -> Result<CSpawnEntity, String> {
        CSpawnEntity::read_packet_data(&self.payload[..], &CURRENT_MC_VERSION)
            .map_err(|err| format!("failed to decode ADD_ENTITY: {err}"))
    }

    /// `CUSTOM_PAYLOAD` (id 24).
    pub fn custom_payload(&self) -> Result<CustomPayload, String> {
        let mut slice = &self.payload[..];
        let channel = slice
            .get_str()
            .map_err(|err| format!("failed to decode CUSTOM_PAYLOAD channel: {err}"))?
            .into_string();
        Ok(CustomPayload {
            channel,
            data: slice.to_vec(),
        })
    }

    /// `BLOCK_UPDATE` (id 8).
    pub fn block_update(&self) -> Result<CBlockUpdate, String> {
        let mut slice = &self.payload[..];
        let location = slice
            .get_block_pos(&CURRENT_MC_VERSION)
            .map_err(|err| format!("failed to decode BLOCK_UPDATE position: {err}"))?;
        let state_id = slice
            .get_var_int()
            .map_err(|err| format!("failed to decode BLOCK_UPDATE state: {err}"))?;
        Ok(CBlockUpdate {
            location,
            state_id,
        })
    }

    /// `SECTION_BLOCKS_UPDATE` (id 86).
    pub fn section_block_update(&self) -> Result<CMultiBlockUpdate, String> {
        let mut slice = &self.payload[..];
        CMultiBlockUpdate::read(&mut slice, &CURRENT_MC_VERSION)
            .map_err(|err| format!("failed to decode SECTION_BLOCKS_UPDATE: {err}"))
    }

    /// `BLOCK_ENTITY_DATA`.
    pub fn block_entity_data(&self) -> Result<CBlockEntityData, String> {
        let mut slice = &self.payload[..];
        CBlockEntityData::read(&mut slice, &CURRENT_MC_VERSION)
            .map_err(|err| format!("failed to decode BLOCK_ENTITY_DATA: {err}"))
    }

    /// `LEVEL_CHUNK_WITH_LIGHT`: the column coordinates only.
    pub fn chunk(&self) -> Result<(i32, i32), String> {
        let mut slice = &self.payload[..];
        let x = slice
            .get_i32()
            .map_err(|err| format!("failed to decode chunk x: {err}"))?;
        let z = slice
            .get_i32()
            .map_err(|err| format!("failed to decode chunk z: {err}"))?;
        Ok((x, z))
    }

    /// `FORGET_LEVEL_CHUNK`: the column the client is told to drop.
    pub fn unload_chunk(&self) -> Result<(i32, i32), String> {
        let mut slice = &self.payload[..];
        let chunk = CUnloadChunk::read(&mut slice, &CURRENT_MC_VERSION)
            .map_err(|err| format!("failed to decode FORGET_LEVEL_CHUNK: {err}"))?;
        Ok((chunk.x, chunk.z))
    }

    /// The block entities `LEVEL_CHUNK_WITH_LIGHT` carried for its column.
    ///
    /// The packet is `x, z`, the heightmaps, the section data, then the block entity list. Only
    /// the list is decoded; the sections are skipped by their length. The writers this mirrors
    /// are `CChunkData::write_packet_data` and `ChunkHeightmaps::write_to`
    /// (`pumpkin-protocol/src/java/client/play/chunk_data.rs`), in their `1.21.5`+ form, which is
    /// what a protocol-777 client reads.
    pub fn chunk_block_entities(&self) -> Result<Vec<ChunkBlockEntity>, String> {
        let mut slice = &self.payload[..];
        let chunk_x = slice
            .get_i32()
            .map_err(|err| format!("failed to decode chunk x: {err}"))?;
        let chunk_z = slice
            .get_i32()
            .map_err(|err| format!("failed to decode chunk z: {err}"))?;

        // Heightmaps: entry count, then per entry an index, the long count and the longs.
        let heightmaps = slice
            .get_var_int()
            .map_err(|err| format!("failed to decode the heightmap count: {err}"))?
            .0;
        for _ in 0..heightmaps {
            slice
                .get_var_int()
                .map_err(|err| format!("failed to decode a heightmap index: {err}"))?;
            let longs = slice
                .get_var_int()
                .map_err(|err| format!("failed to decode a heightmap length: {err}"))?
                .0;
            let bytes = usize::try_from(longs).map_err(|err| err.to_string())? * 8;
            take(&mut slice, bytes)?;
        }

        // The section data.
        let sections = slice
            .get_var_int()
            .map_err(|err| format!("failed to decode the section data length: {err}"))?
            .0;
        let sections = usize::try_from(sections).map_err(|err| err.to_string())?;
        take(&mut slice, sections)?;

        let count = slice
            .get_var_int()
            .map_err(|err| format!("failed to decode the block entity count: {err}"))?
            .0;
        let count = usize::try_from(count).map_err(|err| err.to_string())?;
        let mut block_entities = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let packed_xz = slice
                .get_u8()
                .map_err(|err| format!("failed to decode a block entity xz: {err}"))?;
            let y = slice
                .get_i16_be()
                .map_err(|err| format!("failed to decode a block entity y: {err}"))?;
            let type_id = slice
                .get_var_int()
                .map_err(|err| format!("failed to decode a block entity type: {err}"))?
                .0;
            let nbt_start = slice;
            skip_nbt(&mut slice)?;
            let consumed = nbt_start.len() - slice.len();
            block_entities.push(ChunkBlockEntity {
                pos: BlockPos::new(
                    chunk_x * 16 + i32::from(packed_xz & 0x0F),
                    i32::from(y),
                    chunk_z * 16 + i32::from(packed_xz >> 4),
                ),
                type_id,
                nbt: nbt_start[..consumed].to_vec(),
            });
        }
        Ok(block_entities)
    }

    /// The heightmaps `LEVEL_CHUNK_WITH_LIGHT` carried, read the way a client reads them.
    ///
    /// The order on the wire is the writer's (`ChunkHeightmaps::write_to`): a map size, then per
    /// map an index, the long count and the big-endian longs.
    pub fn chunk_heightmaps(&self) -> Result<Vec<ChunkHeightmap>, String> {
        let mut slice = &self.payload[..];
        slice
            .get_i32()
            .map_err(|err| format!("failed to decode chunk x: {err}"))?;
        slice
            .get_i32()
            .map_err(|err| format!("failed to decode chunk z: {err}"))?;
        read_heightmaps(&mut slice)
    }

    /// The light `LEVEL_CHUNK_WITH_LIGHT` carried, decoded from the payload's tail: the four
    /// masks and the section arrays of both layers, in the order a client reads them.
    ///
    /// The packet is `x, z`, the heightmaps, the section data, the block entity list, then
    /// `LightData` — the same walk [`CapturedPacket::chunk_block_entities`] makes, continued past
    /// the list.
    pub fn chunk_light(&self) -> Result<ChunkLight, String> {
        let mut slice = &self.payload[..];
        slice
            .get_i32()
            .map_err(|err| format!("failed to decode chunk x: {err}"))?;
        slice
            .get_i32()
            .map_err(|err| format!("failed to decode chunk z: {err}"))?;

        read_heightmaps(&mut slice)?;

        let sections = slice
            .get_var_int()
            .map_err(|err| format!("failed to decode the section data length: {err}"))?
            .0;
        let sections = usize::try_from(sections).map_err(|err| err.to_string())?;
        take(&mut slice, sections)?;

        let count = slice
            .get_var_int()
            .map_err(|err| format!("failed to decode the block entity count: {err}"))?
            .0;
        let count = usize::try_from(count).map_err(|err| err.to_string())?;
        for _ in 0..count {
            take(&mut slice, 1)?;
            take(&mut slice, 2)?;
            slice
                .get_var_int()
                .map_err(|err| format!("failed to decode a block entity type: {err}"))?;
            skip_nbt(&mut slice)?;
        }

        let read_bitset = |slice: &mut &[u8], what: &str| {
            BitSet::decode_with_version(slice, &CURRENT_MC_VERSION)
                .map(|set| set.as_u64())
                .map_err(|err| format!("failed to decode the {what} mask: {err}"))
        };
        let sky_light_mask = read_bitset(&mut slice, "sky light")?;
        let block_light_mask = read_bitset(&mut slice, "block light")?;
        let empty_sky_light_mask = read_bitset(&mut slice, "empty sky light")?;
        let empty_block_light_mask = read_bitset(&mut slice, "empty block light")?;

        let read_arrays = |slice: &mut &[u8], what: &str| -> Result<Vec<Vec<u8>>, String> {
            let count = slice
                .get_var_int()
                .map_err(|err| format!("failed to decode the {what} array count: {err}"))?
                .0;
            let count = usize::try_from(count).map_err(|err| err.to_string())?;
            let mut arrays = Vec::with_capacity(count.min(64));
            for _ in 0..count {
                let length = slice
                    .get_var_int()
                    .map_err(|err| format!("failed to decode a {what} array length: {err}"))?
                    .0;
                let length = usize::try_from(length).map_err(|err| err.to_string())?;
                arrays.push(take(slice, length)?.to_vec());
            }
            Ok(arrays)
        };
        let sky_light_arrays = read_arrays(&mut slice, "sky light")?;
        let block_light_arrays = read_arrays(&mut slice, "block light")?;

        Ok(ChunkLight {
            sky_light_mask,
            block_light_mask,
            empty_sky_light_mask,
            empty_block_light_mask,
            sky_light_arrays,
            block_light_arrays,
        })
    }
}

/// The heightmaps at the front of a chunk payload, leaving `slice` at the section data.
fn read_heightmaps(slice: &mut &[u8]) -> Result<Vec<ChunkHeightmap>, String> {
    let count = slice
        .get_var_int()
        .map_err(|err| format!("failed to decode the heightmap count: {err}"))?
        .0;
    let mut maps = Vec::with_capacity(usize::try_from(count).unwrap_or(0).min(8));
    for _ in 0..count {
        let index = slice
            .get_var_int()
            .map_err(|err| format!("failed to decode a heightmap index: {err}"))?
            .0;
        let longs = slice
            .get_var_int()
            .map_err(|err| format!("failed to decode a heightmap length: {err}"))?
            .0;
        let longs = usize::try_from(longs).map_err(|err| err.to_string())?;
        let mut data = Vec::with_capacity(longs);
        for _ in 0..longs {
            data.push(
                slice
                    .get_i64_be()
                    .map_err(|err| format!("failed to decode a heightmap long: {err}"))?,
            );
        }
        maps.push(ChunkHeightmap { index, data });
    }
    Ok(maps)
}

/// Takes `count` bytes, or fails without consuming anything.
fn take<'a>(slice: &mut &'a [u8], count: usize) -> Result<&'a [u8], String> {
    if slice.len() < count {
        return Err(format!(
            "packet ends {} byte(s) early",
            count - slice.len()
        ));
    }
    let (head, rest) = slice.split_at(count);
    *slice = rest;
    Ok(head)
}

/// Skips one NBT tag, which is what the chunk payload has to do to stay aligned with the next
/// block entity (and the light data after the list).
///
/// A protocol-777 client reads the nameless form (`write_nbt_with_version` writes the root's name
/// only below `1.20.2`, `pumpkin-protocol/src/ser/mod.rs`), and an absent payload is a single end
/// tag, so no name is read here.
fn skip_nbt(slice: &mut &[u8]) -> Result<(), String> {
    let tag_type = slice
        .get_u8()
        .map_err(|err| format!("failed to decode an NBT tag type: {err}"))?;
    if tag_type == 0 {
        return Ok(());
    }
    skip_nbt_payload(slice, tag_type)
}

/// The body of one NBT tag, without its header.
fn skip_nbt_payload(slice: &mut &[u8], tag_type: u8) -> Result<(), String> {
    match tag_type {
        1 => take(slice, 1).map(|_| ()),     // BYTE
        2 => take(slice, 2).map(|_| ()),     // SHORT
        3 | 5 => take(slice, 4).map(|_| ()), // INT | FLOAT
        4 | 6 => take(slice, 8).map(|_| ()), // LONG | DOUBLE
        7 | 11 | 12 => {
            // BYTE_ARRAY | INT_ARRAY | LONG_ARRAY: a length and its elements.
            let length = read_i32(slice, "array length")?;
            let element = match tag_type {
                7 => 1,
                11 => 4,
                _ => 8,
            };
            let length = usize::try_from(length).map_err(|err| err.to_string())?;
            take(slice, length * element).map(|_| ())
        }
        8 => {
            let length = read_u16(slice, "string length")?;
            take(slice, usize::from(length)).map(|_| ())
        }
        9 => {
            // A list: the element type, the count, then the elements.
            let element = slice
                .get_u8()
                .map_err(|err| format!("failed to decode a list element type: {err}"))?;
            let count = read_i32(slice, "list length")?;
            let count = usize::try_from(count).map_err(|err| err.to_string())?;
            for _ in 0..count {
                skip_nbt_payload(slice, element)?;
            }
            Ok(())
        }
        10 => {
            // A compound: per entry a tag type, the entry **name**, then its body, until an end
            // tag. The name is what the old reader forgot: without it the body is read at the
            // name's bytes, which is how a block entity list came to "end early" or hit a byte
            // that is not a tag type as soon as a chunk carried one.
            loop {
                let inner = slice
                    .get_u8()
                    .map_err(|err| format!("failed to decode a compound entry: {err}"))?;
                if inner == 0 {
                    return Ok(());
                }
                let name = read_u16(slice, "compound entry name length")?;
                take(slice, usize::from(name))?;
                skip_nbt_payload(slice, inner)?;
            }
        }
        other => Err(format!("unknown NBT tag type {other}")),
    }
}

fn read_i32(slice: &mut &[u8], what: &str) -> Result<i32, String> {
    take(slice, 4).map(|bytes| i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .map_err(|err| format!("failed to decode the NBT {what}: {err}"))
}

fn read_u16(slice: &mut &[u8], what: &str) -> Result<u16, String> {
    take(slice, 2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        .map_err(|err| format!("failed to decode the NBT {what}: {err}"))
}

fn packet_id(id: i32) -> i32 {
    id
}
