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
//!
//! The synced data can also be set entry by entry ([`PluginEntity::set_synced_entry`]), the
//! entries are sent in their own packet and follow the blob when a client starts to track.
//! Spawn data goes to `NeoForge` clients in the `neoforge:advanced_add_entity` payload, to the
//! other clients in the `spawn_data_channel` payload of the type (a plugin must handle it).
//!
//! Plugin entities have no physics of their own: the plugin moves them with [`PluginEntity::move_by`]
//! and the like. Riders are placed at their offset from the entity every tick.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use bytes::BufMut;
use pumpkin_data::entity_registry::PluginEntityType;
use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_nbt::{compound::NbtCompound, tag::NbtTag};
use pumpkin_protocol::{
    ConnectionFlavour,
    codec::var_int::VarInt,
    java::client::play::{CCustomPayload, CSetEntityMetadata, Metadata},
    ser::NetworkWriteExt,
};
use pumpkin_util::math::boundingbox::{BoundingBox, EntityDimensions};
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::version::JavaMinecraftVersion;

use crate::{
    entity::{Entity, EntityBase, living::LivingEntity},
    net::java::JavaClient,
    server::Server,
    world::World,
};

/// The payload `NeoForge` clients take the spawn data of an entity from.
const ADVANCED_ADD_ENTITY: &str = "neoforge:advanced_add_entity";
/// Key of the entries of the synced data in the saved entity.
const SYNCED_ENTRIES_KEY: &str = "PluginSyncedEntries";

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
    /// Entries set one by one: index to serializer id and value. They follow the blob.
    synced_entries: Mutex<BTreeMap<u8, Vec<u8>>>,
    spawn_data: Mutex<Vec<u8>>,
    /// Where riders sit, relative to the entity, by passenger entity id.
    passenger_offsets: Mutex<HashMap<i32, Vector3<f64>>>,
    /// Overrides of the tracking of the type: update interval (0 = the type's) and track deltas.
    update_interval: AtomicU32,
    track_deltas: Mutex<Option<bool>>,
    /// Whether vehicle moves of the client driving the entity are applied.
    client_movement: AtomicBool,
}

impl PluginEntity {
    #[must_use]
    pub fn new(entity: Entity, plugin_type: &'static PluginEntityType) -> Arc<Self> {
        Arc::new(Self {
            entity,
            plugin_type,
            synced_data: Mutex::new(plugin_type.definition.default_synced_data.clone()),
            synced_entries: Mutex::new(BTreeMap::new()),
            spawn_data: Mutex::new(Vec::new()),
            passenger_offsets: Mutex::new(HashMap::new()),
            update_interval: AtomicU32::new(0),
            track_deltas: Mutex::new(None),
            client_movement: AtomicBool::new(false),
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

    /// Replaces the synced data (and drops the entries set one by one) and sends it to the Java
    /// clients that track the entity.
    pub fn set_synced_data(&self, data: Vec<u8>) {
        lock(&self.synced_data).clone_from(&data);
        lock(&self.synced_entries).clear();
        self.send_metadata(data);
    }

    /// Sets one entry of the synced data: `data` is the serializer id (`VarInt`) and the value.
    /// Empty data forgets the entry on the server (clients keep what they got). The entry is sent
    /// to the clients that track the entity.
    pub fn set_synced_entry(&self, index: u8, data: &[u8]) {
        if index == 255 {
            return;
        }
        if data.is_empty() {
            lock(&self.synced_entries).remove(&index);
            return;
        }
        lock(&self.synced_entries).insert(index, data.to_owned());
        let mut entry = vec![index];
        entry.extend_from_slice(data);
        self.send_metadata(entry);
    }

    /// The blob and the entries as one run of entries, without the end marker.
    fn full_synced_data(&self) -> Vec<u8> {
        let mut out = lock(&self.synced_data).clone();
        for (index, data) in lock(&self.synced_entries).iter() {
            out.push(*index);
            out.extend_from_slice(data);
        }
        out
    }

    fn send_metadata(&self, mut buffer: Vec<u8>) {
        if buffer.is_empty() {
            return;
        }
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

    /// Sets where a rider sits, relative to the entity. The server places the rider there every
    /// tick, clients get the offset from the plugin's synced data.
    pub fn set_passenger_offset(&self, passenger_id: i32, offset: Vector3<f64>) {
        lock(&self.passenger_offsets).insert(passenger_id, offset);
    }

    #[must_use]
    pub fn passenger_offset(&self, passenger_id: i32) -> Option<Vector3<f64>> {
        lock(&self.passenger_offsets).get(&passenger_id).copied()
    }

    /// Overrides the update interval (0 = the one of the type) and the delta tracking (`None` =
    /// the one of the type) the position updates of this entity use. A short interval makes
    /// clients interpolate in smaller steps.
    pub fn set_tracking(&self, update_interval: u32, track_deltas: Option<bool>) {
        self.update_interval
            .store(update_interval, Ordering::Relaxed);
        *lock(&self.track_deltas) = track_deltas;
    }

    /// The update interval and delta tracking in use, the arguments are the ones of the type.
    #[must_use]
    pub fn effective_tracking(&self, update_interval: u32, track_deltas: bool) -> (u32, bool) {
        let interval = self.update_interval.load(Ordering::Relaxed);
        (
            if interval == 0 {
                update_interval
            } else {
                interval
            },
            lock(&self.track_deltas).unwrap_or(track_deltas),
        )
    }

    pub fn set_accepts_client_movement(&self, accept: bool) {
        self.client_movement.store(accept, Ordering::Relaxed);
    }

    #[must_use]
    pub fn accepts_client_movement(&self) -> bool {
        self.client_movement.load(Ordering::Relaxed)
    }

    /// Moves the entity by `delta`, stopped by blocks when `collide` is set. Returns the movement
    /// that happened.
    pub fn move_by(&self, delta: Vector3<f64>, collide: bool) -> Vector3<f64> {
        let entity = &self.entity;
        if !collide {
            entity.move_pos(delta);
            return delta;
        }
        let before = entity.pos.load();
        entity.move_entity(self, delta);
        entity.pos.load().sub(&before)
    }

    /// Whether projectiles fly through `other`: a plugin entity that cannot be targeted, or a
    /// rider of one that ignores its riders' hits.
    #[must_use]
    pub fn projectiles_pass_through(other: &dyn EntityBase) -> bool {
        if let Some(plugin_entity) = other.get_plugin_entity() {
            return !plugin_entity.plugin_type.definition.pickable;
        }
        other.get_entity().get_vehicle().is_some_and(|vehicle| {
            vehicle
                .get_plugin_entity()
                .is_some_and(|plugin| plugin.plugin_type.definition.ignore_riders_hit)
        })
    }

    /// Puts the riders at their offsets from the entity.
    fn place_passengers(&self) {
        let entity = &self.entity;
        let passengers = entity
            .passengers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if passengers.is_empty() {
            return;
        }
        let pos = entity.pos.load();
        for passenger in passengers {
            let offset = self
                .passenger_offset(passenger.get_entity().entity_id)
                .unwrap_or_default();
            let target = pos + offset;
            let rider = passenger.get_entity();
            if rider.pos.load() == target {
                continue;
            }
            rider.set_pos(target);
            if passenger.get_player().is_some()
                && let Some(player) = entity.world.load().get_player_by_id(rider.entity_id)
            {
                crate::world::chunker::update_position(&player);
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
    ///
    /// NeoForge's `AdvancedAddEntityPayload` is `(VAR_INT entityId, UNBOUNDED_BYTE_ARRAY data)`,
    /// and `UNBOUNDED_BYTE_ARRAY` is `FriendlyByteBuf.readByteArray`/`writeByteArray`, which
    /// length-prefixes the bytes with a VarInt. Without that prefix the client reads the first
    /// spawn-data byte as the length and the packet overruns ("was larger than I expected").
    fn spawn_payload(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        let _ = payload.write_var_int(&VarInt(self.entity.entity_id));
        let data = lock(&self.spawn_data);
        let _ = payload.write_var_int(&VarInt(data.len() as i32));
        payload.extend_from_slice(&data);
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
        drop(synced);
        let entries = lock(&self.synced_entries);
        if !entries.is_empty() {
            let mut compound = NbtCompound::new();
            for (index, data) in entries.iter() {
                compound.put(&index.to_string(), to_nbt_bytes(data));
            }
            nbt.put(SYNCED_ENTRIES_KEY, NbtTag::Compound(compound));
        }
        drop(entries);
        let spawn = lock(&self.spawn_data);
        if !spawn.is_empty() {
            nbt.put(SPAWN_DATA_KEY, to_nbt_bytes(&spawn));
        }
        drop(spawn);
    }

    fn read_custom_nbt(&self, nbt: &NbtCompound) {
        if let Some(data) = from_nbt_bytes(nbt, SYNCED_DATA_KEY) {
            *lock(&self.synced_data) = data;
        }
        if let Some(compound) = nbt.get_compound(SYNCED_ENTRIES_KEY) {
            let mut entries = lock(&self.synced_entries);
            for key in compound.child_tags.keys() {
                if let (Ok(index), Some(data)) = (key.parse::<u8>(), from_nbt_bytes(compound, key))
                {
                    entries.insert(index, data);
                }
            }
        }
        if let Some(data) = from_nbt_bytes(nbt, SPAWN_DATA_KEY) {
            *lock(&self.spawn_data) = data;
        }
    }

    fn tick(&self, caller: &dyn EntityBase, server: &Server) {
        self.entity.tick(caller, server);
        self.place_passengers();
    }

    fn is_pushable(&self) -> bool {
        self.plugin_type.definition.pushable
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
        buffer.extend_from_slice(&self.full_synced_data());
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
        // NeoForge clients read the spawn data from their own payload, others from the channel
        // of the type, if it has one.
        let channel = if client.flavour.load() == ConnectionFlavour::NeoForge
            && (self.plugin_type.definition.spawn_data_channel.is_some()
                || !lock(&self.spawn_data).is_empty())
        {
            Some(ADVANCED_ADD_ENTITY)
        } else {
            self.plugin_type.definition.spawn_data_channel.as_deref()
        };
        if let Some(channel) = channel {
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
