use std::sync::Arc;

use pumpkin_macros::Event;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;

use crate::world::World;

/// A block entity of a type that a plugin registered became active in a loaded chunk.
///
/// Fired once per entity when its chunk loads, so the plugin can rebuild what it keeps in memory
/// for it. Not fired for entities a plugin creates itself.
#[derive(Event, Clone)]
pub struct BlockEntityLoadEvent {
    /// The world of the entity.
    pub world: Arc<World>,
    /// Where the entity is.
    pub position: BlockPos,
    /// The full key of the block entity type.
    pub type_key: String,
    /// The data saved with the chunk.
    pub save_data: NbtCompound,
    /// The data sent to clients.
    pub client_data: Option<NbtCompound>,
}

impl BlockEntityLoadEvent {
    #[must_use]
    pub const fn new(
        world: Arc<World>,
        position: BlockPos,
        type_key: String,
        save_data: NbtCompound,
        client_data: Option<NbtCompound>,
    ) -> Self {
        Self {
            world,
            position,
            type_key,
            save_data,
            client_data,
        }
    }
}

/// A block entity of a type that a plugin registered goes away.
///
/// Either its chunk unloads (`removed` is false, the data stays saved) or the entity is removed:
/// its block was broken or replaced, or the plugin removed it (`removed` is true, the data is gone).
#[derive(Event, Clone)]
pub struct BlockEntityUnloadEvent {
    /// The world of the entity.
    pub world: Arc<World>,
    /// Where the entity is.
    pub position: BlockPos,
    /// The full key of the block entity type.
    pub type_key: String,
    /// Whether the entity is removed for good, instead of its chunk unloading.
    pub removed: bool,
    /// The data of the entity.
    pub save_data: NbtCompound,
}

impl BlockEntityUnloadEvent {
    #[must_use]
    pub const fn new(
        world: Arc<World>,
        position: BlockPos,
        type_key: String,
        removed: bool,
        save_data: NbtCompound,
    ) -> Self {
        Self {
            world,
            position,
            type_key,
            removed,
            save_data,
        }
    }
}

/// The block entities of one plugin type that are due to tick in a world.
///
/// Fired at most once per world, tick and type, for the types that a plugin gave a tick interval
/// (`plugin-block-entity.set-block-entity-ticking`). One event carries all the positions that are
/// due, so ticking many entities costs the plugin one call.
#[derive(Event, Clone)]
pub struct BlockEntityTickEvent {
    /// The world of the entities.
    pub world: Arc<World>,
    /// The full key of the block entity type.
    pub type_key: String,
    /// The tick of the server, `server.tick_count`.
    pub tick: i32,
    /// The entities that are due. Always at least one.
    pub positions: Vec<BlockPos>,
}

impl BlockEntityTickEvent {
    #[must_use]
    pub const fn new(
        world: Arc<World>,
        type_key: String,
        tick: i32,
        positions: Vec<BlockPos>,
    ) -> Self {
        Self {
            world,
            type_key,
            tick,
            positions,
        }
    }
}
