//! The block entities of plugin registered types, see `PluginBlockEntity`.
//!
//! The live entity lives in `World::block_entities` like any other, and the chunk keeps the saved
//! form in `pending_block_entities`, which is also what the chunk packet is built from. The helpers
//! here keep the two in step: every change writes the saved form into the chunk, so a plugin entity
//! survives a restart and reaches clients with the chunk even if nothing unloads it cleanly.

use std::sync::Arc;

use pumpkin_data::block_entity_registry::{
    PluginBlockEntityType, any_plugin_block_entity_ticking, plugin_block_entity_type_by_key,
};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::CBlockEntityData;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector2::Vector2;
use pumpkin_world::chunk::io::Dirtiable;
use rustc_hash::FxHashMap;
use tracing::warn;

use crate::block::entities::BlockEntity;
use crate::block::entities::plugin::PluginBlockEntity;
use crate::plugin::block::{BlockEntityLoadEvent, BlockEntityTickEvent, BlockEntityUnloadEvent};
use crate::plugin::world::WorldTickEvent;
use crate::server::Server;

use super::World;

/// Spreads the entities of a type over the ticks of its interval, so that they do not all fall due
/// in the same tick. Stable for a position.
const fn tick_phase(position: &BlockPos) -> u32 {
    (position.0.x.wrapping_mul(73_856_093)
        ^ position.0.y.wrapping_mul(19_349_663)
        ^ position.0.z.wrapping_mul(83_492_791)) as u32
}

impl World {
    /// The `Arc` of this world, found through the server.
    fn arc_self(&self) -> Option<Arc<Self>> {
        let server = self.server.upgrade()?;
        server
            .worlds
            .load()
            .iter()
            .find(|world| world.uuid == self.uuid)
            .cloned()
    }

    /// Runs `f` on the plugin block entity at a position, loading it from the chunk if needed.
    /// `None` if there is none (or its chunk is not loaded).
    pub fn with_plugin_block_entity<R>(
        &self,
        position: &BlockPos,
        f: impl FnOnce(&PluginBlockEntity) -> R,
    ) -> Option<R> {
        let entity = self.get_block_entity(position)?;
        entity.as_any().downcast_ref::<PluginBlockEntity>().map(f)
    }

    /// Creates the plugin block entity at a position, or replaces the data of the one that is
    /// there, and sends the client data to the players watching the chunk.
    ///
    /// Fails if the chunk is not loaded or the position holds another kind of block entity.
    pub fn set_plugin_block_entity(
        &self,
        position: BlockPos,
        entity_type: &'static PluginBlockEntityType,
        data: NbtCompound,
        client_data: Option<NbtCompound>,
    ) -> Result<(), String> {
        let chunk_position = position.chunk_position();
        if !self.level.is_chunk_loaded(&chunk_position) {
            return Err(format!(
                "the chunk of ({}, {}, {}) is not loaded",
                position.0.x, position.0.y, position.0.z
            ));
        }

        if let Some(existing) = self.get_block_entity(&position) {
            let Some(plugin_entity) = existing.as_any().downcast_ref::<PluginBlockEntity>() else {
                return Err(format!(
                    "({}, {}, {}) holds a block entity that is not a plugin block entity",
                    position.0.x, position.0.y, position.0.z
                ));
            };
            if !std::ptr::eq(plugin_entity.entity_type(), entity_type) {
                return Err(format!(
                    "({}, {}, {}) holds a block entity of type '{}', not '{}'",
                    position.0.x,
                    position.0.y,
                    position.0.z,
                    plugin_entity.entity_type().key,
                    entity_type.key
                ));
            }
            plugin_entity.set(data, client_data);
            self.publish_plugin_block_entity(plugin_entity);
            return Ok(());
        }

        self.warn_if_block_not_valid(&position, entity_type);

        let entity = Arc::new(PluginBlockEntity::new(
            position,
            entity_type,
            data,
            client_data,
        ));
        // The plugin made it, it does not need to be told that it exists.
        entity.mark_announced();
        self.block_entities
            .entry(chunk_position)
            .or_default()
            .insert(position, entity.clone());
        self.publish_plugin_block_entity(&entity);
        Ok(())
    }

    /// Sends the client data of the plugin block entity at a position to the players watching the
    /// chunk again and marks the chunk as changed.
    pub fn sync_plugin_block_entity(&self, position: &BlockPos) -> Result<(), String> {
        self.with_plugin_block_entity(position, |entity| self.publish_plugin_block_entity(entity))
            .ok_or_else(|| {
                format!(
                    "there is no plugin block entity at ({}, {}, {})",
                    position.0.x, position.0.y, position.0.z
                )
            })
    }

    /// Removes the plugin block entity at a position with its saved data, firing the unload event
    /// for it. Returns whether there was one.
    pub fn remove_plugin_block_entity(&self, position: &BlockPos) -> bool {
        let Some(entity) = self.get_block_entity(position) else {
            return false;
        };
        let Some(plugin_entity) = entity.as_any().downcast_ref::<PluginBlockEntity>() else {
            return false;
        };
        self.fire_plugin_block_entity_unload(plugin_entity, true);

        // What is left in its inventory drops, like the contents of a chest.
        if let Some(inventory) = plugin_entity.plugin_inventory()
            && let Some(world) = self.arc_self()
        {
            let inventory: Arc<dyn pumpkin_inventory::Inventory> = inventory;
            world.scatter_inventory(position, &inventory);
        }

        self.remove_block_entity(position);
        // The saved copy as well, or the entity would come back when something asks for it.
        let removed = |chunk: &pumpkin_world::chunk::ChunkData| {
            chunk
                .pending_block_entities
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(position);
            chunk.mark_dirty(true);
        };
        self.level
            .read_chunk_sync(&position.chunk_position(), |chunk| removed(chunk));
        true
    }

    /// The hook of `set_block_state`: a block was replaced by another one, so the plugin block
    /// entity that belonged to it, if any, goes with it.
    pub(super) fn remove_plugin_block_entity_for_replaced_block(&self, position: &BlockPos) {
        let chunk_position = position.chunk_position();
        let live_is_plugin = self
            .block_entities
            .get(&chunk_position)
            .and_then(|entities| entities.get(position).cloned())
            .map(|entity| entity.as_any().is::<PluginBlockEntity>());
        let is_plugin = live_is_plugin.unwrap_or_else(|| {
            self.level
                .read_chunk_sync(&chunk_position, |chunk| {
                    chunk
                        .pending_block_entities
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get(position)
                        .and_then(|nbt| nbt.get_string("id"))
                        .is_some_and(|id| plugin_block_entity_type_by_key(id).is_some())
                })
                .unwrap_or(false)
        });
        if is_plugin {
            self.remove_plugin_block_entity(position);
        }
    }

    /// Tells the plugins about the plugin block entities of a chunk that they have not been told
    /// about, which is what happens when a chunk loads.
    pub(super) fn announce_plugin_block_entities(&self, chunk_position: Vector2<i32>) {
        let entities: Vec<Arc<dyn BlockEntity>> = self
            .block_entities
            .get(&chunk_position)
            .map(|entities| entities.values().cloned().collect())
            .unwrap_or_default();
        for entity in entities {
            if let Some(plugin_entity) = entity.as_any().downcast_ref::<PluginBlockEntity>()
                && !plugin_entity.mark_announced()
            {
                self.fire_plugin_block_entity_load(plugin_entity);
            }
        }
    }

    /// Tells the plugins that the plugin block entities of a chunk unload, before the chunk goes.
    pub(super) async fn announce_plugin_block_entity_unloads(&self, chunk_position: Vector2<i32>) {
        let entities: Vec<Arc<dyn BlockEntity>> = self
            .block_entities
            .get(&chunk_position)
            .map(|entities| entities.values().cloned().collect())
            .unwrap_or_default();
        let Some((world, server)) = self.arc_self().zip(self.server.upgrade()) else {
            return;
        };
        for entity in entities {
            let Some(plugin_entity) = entity.as_any().downcast_ref::<PluginBlockEntity>() else {
                continue;
            };
            let mut event = BlockEntityUnloadEvent::new(
                world.clone(),
                plugin_entity.get_position(),
                plugin_entity.entity_type().key.to_string(),
                false,
                plugin_entity.data(),
            );
            server.plugin_manager.fire(&server, &mut event).await;
        }
    }

    /// Ticks the plugin block entities of the types that have a tick interval: for each type one
    /// `BlockEntityTickEvent` with the positions that are due. Only entities in `active_chunks`
    /// (the chunks that tick) count, an entity that is not loaded is not in `block_entities`.
    ///
    /// Costs a load of one atomic and a check for handlers when nothing wants ticking.
    pub(super) fn tick_plugin_block_entities(self: &Arc<Self>, server: &Arc<Server>) {
        if !any_plugin_block_entity_ticking()
            || !server.plugin_manager.has_handlers::<BlockEntityTickEvent>()
        {
            return;
        }
        let tick = server.tick_count.load(std::sync::atomic::Ordering::Relaxed);
        let tick_bits = tick as u32;

        // Collected first: the plugin may change the entities while it handles an event, and
        // nothing of `block_entities` may be held across that.
        let mut due: FxHashMap<u32, (&'static PluginBlockEntityType, Vec<BlockPos>)> =
            FxHashMap::default();
        let mut collect = |entities: &FxHashMap<BlockPos, Arc<dyn BlockEntity>>| {
            for entity in entities.values() {
                let Some(plugin_entity) = entity.as_any().downcast_ref::<PluginBlockEntity>()
                else {
                    continue;
                };
                let entity_type = plugin_entity.entity_type();
                let interval = entity_type.tick_interval();
                if interval == 0 {
                    continue;
                }
                let position = plugin_entity.get_position();
                if interval == 1 || tick_bits.wrapping_add(tick_phase(&position)) % interval == 0 {
                    due.entry(entity_type.id)
                        .or_insert_with(|| (entity_type, Vec::new()))
                        .1
                        .push(position);
                }
            }
        };
        {
            // Not held while the plugin runs.
            let active_chunks = self
                .active_chunks
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.block_entities.len() < active_chunks.len() {
                for chunk in &self.block_entities {
                    if active_chunks.contains(chunk.key()) {
                        collect(chunk.value());
                    }
                }
            } else {
                for chunk_position in active_chunks.iter() {
                    if let Some(chunk) = self.block_entities.get(chunk_position) {
                        collect(chunk.value());
                    }
                }
            }
        }
        if due.is_empty() {
            return;
        }

        let mut due: Vec<_> = due.into_values().collect();
        due.sort_unstable_by_key(|(entity_type, _)| entity_type.id);
        for (index, (entity_type, mut positions)) in due.into_iter().enumerate() {
            if index > 0 {
                // An earlier event may have removed some of them.
                positions.retain(|position| {
                    self.block_entities
                        .get(&position.chunk_position())
                        .and_then(|chunk| chunk.get(position).cloned())
                        .is_some_and(|entity| {
                            entity
                                .as_any()
                                .downcast_ref::<PluginBlockEntity>()
                                .is_some_and(|entity| {
                                    std::ptr::eq(entity.entity_type(), entity_type)
                                })
                        })
                });
                if positions.is_empty() {
                    continue;
                }
            }
            let mut event = BlockEntityTickEvent::new(
                self.clone(),
                entity_type.key.to_string(),
                tick,
                positions,
            );
            server.plugin_manager.fire_blocking(server, &mut event);
        }
    }

    /// Fires `WorldTickEvent` for this world, nothing is allocated if nobody listens.
    pub(super) fn fire_world_tick_event(self: &Arc<Self>, server: &Arc<Server>) {
        if !server.plugin_manager.has_handlers::<WorldTickEvent>() {
            return;
        }
        let tick = server.tick_count.load(std::sync::atomic::Ordering::Relaxed);
        let mut event = WorldTickEvent::new(self.clone(), tick);
        server.plugin_manager.fire_blocking(server, &mut event);
    }

    fn fire_plugin_block_entity_load(&self, entity: &PluginBlockEntity) {
        let Some((world, server)) = self.arc_self().zip(self.server.upgrade()) else {
            return;
        };
        if !server.plugin_manager.has_handlers::<BlockEntityLoadEvent>() {
            return;
        }
        let mut event = BlockEntityLoadEvent::new(
            world,
            entity.get_position(),
            entity.entity_type().key.to_string(),
            entity.data(),
            entity.client_data(),
        );
        server.plugin_manager.fire_blocking(&server, &mut event);
    }

    fn fire_plugin_block_entity_unload(&self, entity: &PluginBlockEntity, removed: bool) {
        let Some((world, server)) = self.arc_self().zip(self.server.upgrade()) else {
            return;
        };
        if !server
            .plugin_manager
            .has_handlers::<BlockEntityUnloadEvent>()
        {
            return;
        }
        let mut event = BlockEntityUnloadEvent::new(
            world,
            entity.get_position(),
            entity.entity_type().key.to_string(),
            removed,
            entity.data(),
        );
        server.plugin_manager.fire_blocking(&server, &mut event);
    }

    /// Writes the saved form of the entity into its chunk and sends its client data.
    fn publish_plugin_block_entity(&self, entity: &PluginBlockEntity) {
        let position = entity.get_position();
        let mut nbt = NbtCompound::new();
        entity.write_internal(&mut nbt);
        self.add_block_entity_nbt(position, &nbt);

        if let Some(client_data) = entity.client_data() {
            let bytes = pumpkin_nbt::Nbt::from(client_data).write_unnamed();
            self.broadcast_to_chunk(
                position.chunk_position(),
                &CBlockEntityData::new(
                    position,
                    VarInt(entity.get_id() as i32),
                    bytes.as_ref().into(),
                ),
            );
        }
    }

    fn warn_if_block_not_valid(&self, position: &BlockPos, entity_type: &PluginBlockEntityType) {
        if entity_type.valid_blocks.is_empty() {
            return;
        }
        let block = self.get_block(position);
        let key = block.resource_location();
        let normalized: &str = key.strip_prefix("minecraft:").unwrap_or(&key);
        let valid = entity_type
            .valid_blocks
            .iter()
            .any(|valid| valid.strip_prefix("minecraft:").unwrap_or(valid) == normalized);
        if !valid {
            warn!(
                "Block entity '{}' was set at ({}, {}, {}) where the block is '{key}', not one of its valid blocks",
                entity_type.key, position.0.x, position.0.y, position.0.z
            );
        }
    }
}
