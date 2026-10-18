//! An entity of a type that a plugin registered, see [`pumpkin_data::entity_registry`].
//!
//! The server knows nothing about what such an entity is. Position, velocity, rotation, passengers
//! and the custom data of plugins are the ones of every entity. On top of that it keeps two blobs
//! that only the plugin interprets and that clients receive:
//!
//! * the **synced data**: encoded entity metadata entries (index, serializer id, value; no end
//!   marker), sent with the spawn and whenever the plugin changes it.
//! * the **spawn data**: bytes sent in the custom payload of the type's `spawn_data_channel`, if it
//!   has one, to every client that starts to track the entity.
//!
//! Both are saved with the entity.

use std::sync::{Arc, Mutex, PoisonError};

use bytes::BufMut;
use pumpkin_data::entity_registry::PluginEntityType;
use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_nbt::{compound::NbtCompound, tag::NbtTag};
use pumpkin_protocol::{
    codec::var_int::VarInt,
    java::client::play::{CCustomPayload, CSetEntityMetadata, Metadata},
    ser::NetworkWriteExt,
};
use pumpkin_util::math::boundingbox::{BoundingBox, EntityDimensions};
use pumpkin_util::version::JavaMinecraftVersion;

use crate::{
    entity::{Entity, EntityBase, living::LivingEntity},
    net::java::JavaClient,
    world::World,
};

/// Key of the synced data in the saved entity.
const SYNCED_DATA_KEY: &str = "PluginSyncedData";
/// Key of the spawn data in the saved entity.
const SPAWN_DATA_KEY: &str = "PluginSpawnData";
/// The most a plugin can set as spawn data, it is written into one packet.
pub const MAX_SPAWN_DATA: usize = 16 * 1024;
/// The most a plugin can set as synced data, it is written into one packet.
pub const MAX_SYNCED_DATA: usize = 8 * 1024;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn to_nbt_bytes(data: &[u8]) -> NbtTag {
    NbtTag::ByteArray(data.iter().map(|byte| *byte as i8).collect())
}

fn from_nbt_bytes(nbt: &NbtCompound, key: &str) -> Option<Vec<u8>> {
    nbt.get_byte_array(key)
        .map(|bytes| bytes.iter().map(|byte| *byte as u8).collect())
}

pub struct PluginEntity {
    pub entity: Entity,
    plugin_type: &'static PluginEntityType,
    synced_data: Mutex<Vec<u8>>,
    spawn_data: Mutex<Vec<u8>>,
}

impl PluginEntity {
    #[must_use]
    pub fn new(entity: Entity, plugin_type: &'static PluginEntityType) -> Arc<Self> {
        Arc::new(Self {
            entity,
            plugin_type,
            synced_data: Mutex::new(plugin_type.definition.default_synced_data.clone()),
            spawn_data: Mutex::new(Vec::new()),
        })
    }

    #[must_use]
    pub const fn plugin_type(&self) -> &'static PluginEntityType {
        self.plugin_type
    }

    #[must_use]
    pub fn synced_data(&self) -> Vec<u8> {
        lock(&self.synced_data).clone()
    }

    /// Replaces the synced data without telling anybody, for an entity that is not spawned yet.
    pub fn set_initial_synced_data(&self, data: Vec<u8>) {
        *lock(&self.synced_data) = data;
    }

    /// Replaces the synced data and sends it to the Java clients that track the entity.
    pub fn set_synced_data(&self, data: Vec<u8>) {
        *lock(&self.synced_data) = data.clone();
        if data.is_empty() {
            return;
        }
        let mut buffer = data;
        buffer.put_u8(255);

        let entity = &self.entity;
        let world = entity.world.load();
        let players = world.players.load();
        let recipients = entity.java_metadata_recipients(&world, &players);
        let recipients_by_version =
            World::collect_java_recipients_by_version(recipients.into_iter());
        for (version, recipients) in recipients_by_version {
            let packet =
                CSetEntityMetadata::new(entity.entity_id.into(), buffer.clone().into_boxed_slice());
            if let Ok(packet_data) = JavaClient::serialize_packet_for_version(&packet, version) {
                for recipient in recipients {
                    recipient.try_enqueue_packet(packet_data.clone());
                }
            }
        }
    }

    #[must_use]
    pub fn spawn_data(&self) -> Vec<u8> {
        lock(&self.spawn_data).clone()
    }

    /// Replaces the spawn data. Clients that track the entity already keep what they got, the new
    /// bytes go to the ones that start to track it from now on.
    pub fn set_spawn_data(&self, data: Vec<u8>) {
        *lock(&self.spawn_data) = data;
    }

    /// Changes the size of the hitbox, the eye height keeps its distance to the top.
    pub fn set_size(&self, width: f32, height: f32) {
        let entity = &self.entity;
        let old = entity.entity_dimension.load();
        let dimensions = EntityDimensions {
            width,
            height,
            eye_height: (height - (old.height - old.eye_height)).max(0.0),
        };
        entity.entity_dimension.store(dimensions);
        let pos = entity.pos.load();
        entity
            .bounding_box
            .store(BoundingBox::new_from_pos(pos.x, pos.y, pos.z, &dimensions));
    }

    /// The payload of the type's `spawn_data_channel`: the entity id and the spawn data.
    fn spawn_payload(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        let _ = payload.write_var_int(&VarInt(self.entity.entity_id));
        payload.extend_from_slice(&lock(&self.spawn_data));
        payload
    }
}

impl EntityBase for PluginEntity {
    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }

    fn get_plugin_entity(&self) -> Option<&PluginEntity> {
        Some(self)
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }

    fn write_custom_nbt(&self, nbt: &mut NbtCompound) {
        let synced = lock(&self.synced_data);
        if !synced.is_empty() {
            nbt.put(SYNCED_DATA_KEY, to_nbt_bytes(&synced));
        }
        let spawn = lock(&self.spawn_data);
        if !spawn.is_empty() {
            nbt.put(SPAWN_DATA_KEY, to_nbt_bytes(&spawn));
        }
    }

    fn read_custom_nbt(&self, nbt: &NbtCompound) {
        if let Some(data) = from_nbt_bytes(nbt, SYNCED_DATA_KEY) {
            *lock(&self.synced_data) = data;
        }
        if let Some(data) = from_nbt_bytes(nbt, SPAWN_DATA_KEY) {
            *lock(&self.spawn_data) = data;
        }
    }

    fn is_collidable(&self, _entity: Option<Box<dyn EntityBase>>) -> bool {
        self.plugin_type.definition.collidable
    }

    fn can_hit(&self) -> bool {
        self.plugin_type.definition.pickable
    }

    fn java_spawn_metadata(&self, version: JavaMinecraftVersion) -> Option<Box<[u8]>> {
        if version < JavaMinecraftVersion::V_1_9 {
            return None;
        }
        let mut buffer = Vec::new();
        let flags = self.entity.flags.load(std::sync::atomic::Ordering::Relaxed);
        if flags != 0 {
            let _ = Metadata::new(
                pumpkin_data::tracked_data::entity::DATA_SHARED_FLAGS_ID,
                flags,
            )
            .write(&mut buffer, &version);
        }
        buffer.extend_from_slice(&lock(&self.synced_data));
        if buffer.is_empty() {
            return None;
        }
        buffer.put_u8(255);
        Some(buffer.into_boxed_slice())
    }

    fn send_java_spawn_packet(&self, client: &JavaClient) {
        let entity = &self.entity;
        let spawn_packet = entity.create_spawn_packet();
        if let Ok(data) = client.serialize_packet(&spawn_packet) {
            client.try_enqueue_packet(data);
        }
        if let Some(channel) = &self.plugin_type.definition.spawn_data_channel {
            let payload = self.spawn_payload();
            let packet = CCustomPayload::new(channel, &payload);
            if let Ok(data) = client.serialize_packet(&packet) {
                client.try_enqueue_packet(data);
            }
        }
        if let Some(metadata) = self.java_spawn_metadata(CURRENT_MC_VERSION) {
            let packet = CSetEntityMetadata::new(entity.entity_id.into(), metadata);
            if let Ok(data) = client.serialize_packet(&packet) {
                client.try_enqueue_packet(data);
            }
        }
    }

    /// Bedrock has no counterpart of a Java mod entity.
    fn send_bedrock_spawn_packet(&self, _client: &crate::net::bedrock::BedrockClient) {}
}
