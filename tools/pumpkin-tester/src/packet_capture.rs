//! Decoded view of the packets a [`crate::bot::HeadlessBot`] received.
//!
//! Tests use this to assert on client-visible state: "the bot got `ADD_ENTITY` for entity 42",
//! "a `BLOCK_UPDATE` placed stone at p", "`CUSTOM_PAYLOAD` on channel X carried payload Y".
//! Everything here decodes from the raw bytes that crossed the socket, never from server state.

use bytes::Bytes;
use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_protocol::{
    RawPacket, ServerPacket,
    java::client::play::{CBlockUpdate, CMultiBlockUpdate, CSpawnEntity},
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
        let id = CSpawnEntity::to_id(CURRENT_MC_VERSION);
        self.packets(id)
            .map(|packet| {
                CSpawnEntity::read_packet_data(&packet.payload[..], &CURRENT_MC_VERSION)
                    .map_err(|err| format!("failed to decode ADD_ENTITY: {err}"))
            })
            .collect()
    }

    /// `CUSTOM_PAYLOAD` (id 24), optionally filtered by channel.
    pub fn custom_payloads(&self, channel: Option<&str>) -> Result<Vec<CustomPayload>, String> {
        let id = pumpkin_protocol::java::client::play::CCustomPayload::to_id(CURRENT_MC_VERSION);
        self.packets(id)
            .map(|packet| {
                let mut slice = &packet.payload[..];
                let channel = slice
                    .get_str()
                    .map_err(|err| format!("failed to decode CUSTOM_PAYLOAD channel: {err}"))?
                    .into_string();
                Ok(CustomPayload {
                    channel,
                    data: slice.to_vec(),
                })
            })
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
        let id = CBlockUpdate::to_id(CURRENT_MC_VERSION);
        self.packets(id)
            .map(|packet| {
                let mut slice = &packet.payload[..];
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
            })
            .collect()
    }

    /// `SECTION_BLOCKS_UPDATE` (id 86): batched block changes.
    pub fn section_block_updates(&self) -> Result<Vec<CMultiBlockUpdate>, String> {
        let id = CMultiBlockUpdate::to_id(CURRENT_MC_VERSION);
        self.packets(id)
            .map(|packet| {
                let mut slice = &packet.payload[..];
                CMultiBlockUpdate::read(&mut slice, &CURRENT_MC_VERSION)
                    .map_err(|err| format!("failed to decode SECTION_BLOCKS_UPDATE: {err}"))
            })
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
}

fn packet_id(id: i32) -> i32 {
    id
}
