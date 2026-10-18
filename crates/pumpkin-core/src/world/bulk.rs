//! Bulk block changes: many writes, then one pass of neighbour, shape and light updates.

use std::collections::HashSet;
use std::sync::Arc;

use pumpkin_data::block_entity_registry::PluginBlockEntityType;
use pumpkin_data::{Block, BlockDirection, BlockStateId};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use pumpkin_world::world::BlockFlags;
use uuid::Uuid;

use super::World;
use crate::entity::EntityBase;
use crate::entity::player::Player;

/// The data of a plugin block entity to create with a block.
pub struct PluginBlockEntityChange {
    pub entity_type: &'static PluginBlockEntityType,
    pub data: NbtCompound,
    pub client_data: Option<NbtCompound>,
}

/// One block of a bulk change.
pub struct BlockChange {
    pub pos: BlockPos,
    pub state: BlockStateId,
    /// Vanilla block entity data, with an `id`. The position is set from `pos`.
    pub block_entity: Option<NbtCompound>,
    pub plugin_block_entity: Option<PluginBlockEntityChange>,
}

impl BlockChange {
    #[must_use]
    pub const fn new(pos: BlockPos, state: BlockStateId) -> Self {
        Self {
            pos,
            state,
            block_entity: None,
            plugin_block_entity: None,
        }
    }
}

/// What a bulk change did.
#[derive(Debug, Default)]
pub struct BulkOutcome {
    /// Blocks whose state was written and differed from before (or were forced).
    pub changed: u32,
    /// Block entities that could not be created, and why.
    pub errors: Vec<String>,
}

impl World {
    /// Writes all `changes` first, then sends the neighbour, shape and light updates once for
    /// the whole batch, the way vanilla `fill` and structure placement do. Positions outside the
    /// build height and in unloaded chunks are skipped. `SKIP_DROPS` and `MOVED` also skip the
    /// replaced callback of old block entities, so a container that was moved does not spill.
    pub fn set_blocks_bulk(
        self: &Arc<Self>,
        changes: Vec<BlockChange>,
        flags: BlockFlags,
    ) -> BulkOutcome {
        let mut flags = flags;
        flags.remove(BlockFlags::DEFER_NEIGHBOR_UPDATES);
        if flags.intersects(BlockFlags::SKIP_DROPS | BlockFlags::MOVED) {
            flags |= BlockFlags::SKIP_BLOCK_ENTITY_REPLACED_CALLBACK;
        }
        let write_flags = flags | BlockFlags::DEFER_NEIGHBOR_UPDATES;

        let mut outcome = BulkOutcome::default();
        let mut done: Vec<(BlockPos, BlockStateId, BlockStateId)> = Vec::with_capacity(changes.len());

        for change in changes {
            let pos = change.pos;
            let Some(old) = self.set_block_state_if(&pos, change.state, write_flags, |_| true)
            else {
                continue;
            };
            if old != change.state || flags.contains(BlockFlags::FORCE_STATE) {
                outcome.changed += 1;
                done.push((pos, old, change.state));
            }

            if let Some(mut nbt) = change.block_entity {
                if nbt.get_string("id").is_none() {
                    outcome
                        .errors
                        .push(format!("block entity at {pos:?} has no id"));
                } else {
                    // Replaces a block entity that stayed because the block did not change.
                    self.remove_block_entity(&pos);
                    nbt.put_int("x", pos.0.x);
                    nbt.put_int("y", pos.0.y);
                    nbt.put_int("z", pos.0.z);
                    self.add_block_entity_nbt(pos, &nbt);
                }
            }
            if let Some(plugin) = change.plugin_block_entity {
                self.remove_plugin_block_entity(&pos);
                if let Err(error) = self.set_plugin_block_entity(
                    pos,
                    plugin.entity_type,
                    plugin.data,
                    plugin.client_data,
                ) {
                    outcome.errors.push(error);
                }
            }
        }

        self.finish_block_changes(&done, flags);
        outcome
    }

    /// Replaces every position with air (water for waterlogged blocks), like `set_blocks_bulk`.
    pub fn remove_blocks_bulk(
        self: &Arc<Self>,
        positions: &[BlockPos],
        flags: BlockFlags,
    ) -> BulkOutcome {
        let changes = positions
            .iter()
            .map(|pos| {
                let (block, state_id) = self.get_block_and_state_id(pos);
                let replacement = if block.is_waterlogged(state_id) {
                    Block::WATER.default_state.id
                } else {
                    Block::AIR.default_state.id
                };
                BlockChange::new(*pos, replacement)
            })
            .collect();
        self.set_blocks_bulk(changes, flags)
    }

    /// The updates that follow a set of writes made with `DEFER_NEIGHBOR_UPDATES`: light once
    /// for the batch, then the neighbour, shape and indirect updates of every position.
    /// Each entry is a position with the state it had and the state it got.
    pub fn finish_block_changes(
        self: &Arc<Self>,
        changes: &[(BlockPos, BlockStateId, BlockStateId)],
        flags: BlockFlags,
    ) {
        let engine = &self.level.light_engine;
        let mut light_dirty = false;
        for (pos, old, new) in changes {
            if pumpkin_world::lighting::LightEngine::has_different_light_properties(
                old.to_state(),
                new.to_state(),
            ) {
                engine.check_block_light_updates(&self.level, *pos);
                engine.check_sky_light_updates(&self.level, *pos);
                light_dirty = true;
            }
        }
        if light_dirty {
            engine.perform_block_light_updates(&self.level);
            engine.perform_sky_light_updates(&self.level);
        }

        let mut indirect: HashSet<BlockPos> = HashSet::new();
        for (pos, old, new) in changes {
            // A later write or a block update may have changed the position again.
            if self.get_block_state_id(pos) != *new {
                continue;
            }
            let old_block = Block::from_state_id(*old);
            let new_block = Block::from_state_id(*new);

            if flags.contains(BlockFlags::NOTIFY_NEIGHBORS) {
                self.update_neighbors_at(pos, old_block, None);
                if crate::block::blocks::dynamic::has_analog_output(new_block, new.to_state()) {
                    self.update_neighbour_for_output_signal(pos, new_block);
                }
            }

            if !flags.intersects(BlockFlags::MOVED | BlockFlags::UPDATE_KNOWN_SHAPE) {
                let mut neighbour_flags = flags;
                neighbour_flags.remove(BlockFlags::NOTIFY_NEIGHBORS);
                neighbour_flags.remove(BlockFlags::SKIP_DROPS);
                self.block_registry
                    .prepare(self, pos, old_block, *old, neighbour_flags);
                self.block_registry
                    .update_neighbors(self, pos, neighbour_flags);
                self.block_registry
                    .prepare(self, pos, new_block, *new, neighbour_flags);
            }

            if flags.contains(BlockFlags::UPDATE_INDIRECT_NEIGHBORS) {
                for direction in BlockDirection::all() {
                    indirect.insert(pos.offset(direction.to_offset()));
                }
            }
        }

        // The neighbours of the changed blocks tell their own neighbours, which is what
        // strong redstone power needs.
        for pos in indirect {
            let block = self.get_block(&pos);
            self.update_neighbors_at(&pos, block, None);
        }
    }

    /// Sends the shape updates of the six neighbours of `pos`, as if the block there changed.
    pub fn update_shape_around(self: &Arc<Self>, pos: &BlockPos) {
        self.block_registry
            .update_neighbors(self, pos, BlockFlags::NOTIFY_ALL);
    }

    /// Vanilla `markAndNotifyBlock`: tells clients and neighbours that the state at `pos` went
    /// from `old` to `new` without writing anything.
    pub fn mark_and_notify(
        self: &Arc<Self>,
        pos: &BlockPos,
        old: BlockStateId,
        new: BlockStateId,
        flags: BlockFlags,
    ) {
        if flags.contains(BlockFlags::NOTIFY_LISTENERS) {
            let current = self.get_block_state_id(pos);
            self.unsent_block_changes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(*pos, current);
        }
        self.finish_block_changes(&[(*pos, old, new)], flags);
    }

    /// Breaks the block at `pos` like a player does: the `BlockBreakEvent`, loot (only when
    /// `drop`), the break particles and sound (only when `with_particles`), and the neighbour
    /// updates. Returns whether a block was broken.
    pub fn destroy_block(
        self: &Arc<Self>,
        pos: &BlockPos,
        drop: bool,
        player: Option<&Arc<Player>>,
        with_particles: bool,
    ) -> bool {
        let mut flags = BlockFlags::NOTIFY_ALL;
        if !drop {
            flags |= BlockFlags::SKIP_DROPS;
        }
        self.break_block_with_effects(pos, player, flags, with_particles)
            .is_some()
    }

    /// Creates an entity from its saved NBT (`id` and the rest) and spawns it. The position is
    /// `pos` if given, else the `Pos` of the NBT. The entity gets a new uuid. Returns `None`
    /// for an unknown entity type or when a plugin cancelled the spawn.
    pub fn spawn_entity_from_nbt(
        self: &Arc<Self>,
        nbt: &NbtCompound,
        pos: Option<Vector3<f64>>,
    ) -> Option<Arc<dyn EntityBase>> {
        let id = nbt.get_string("id")?;
        let entity_type = pumpkin_data::entity_registry::entity_type_by_key(id)?;
        let mut nbt = nbt.clone();
        nbt.child_tags.remove("UUID");
        if let Some(pos) = pos {
            nbt.put(
                "Pos",
                NbtTag::List(vec![pos.x.into(), pos.y.into(), pos.z.into()]),
            );
        }
        let spawn_pos = nbt
            .get_list("Pos")
            .filter(|list| list.len() == 3)
            .map_or_else(Vector3::default, |list| {
                Vector3::new(
                    list[0].extract_double().unwrap_or_default(),
                    list[1].extract_double().unwrap_or_default(),
                    list[2].extract_double().unwrap_or_default(),
                )
            });
        let entity = crate::entity::r#type::from_type(entity_type, spawn_pos, self, Uuid::new_v4());
        entity.read_nbt_non_mut(&nbt);
        self.spawn_entity(entity.clone()).then_some(entity)
    }
}
