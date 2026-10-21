use crate::generation::proto_chunk::GenerationCache;
use bitflags::bitflags;
use pumpkin_data::{Block, BlockState, BlockStateId, Mirror, Rotation, chunk::Biome};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use thiserror::Error;

bitflags! {
    /// Flags used to control the side effects of a block state change.
    /// These match the internal bitmask used by Minecraft's `setBlockState` method
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct BlockFlags: u32 {
        /// Causes a neighbor update to be sent to surrounding blocks.
        /// This is what makes observers detect changes or redstone components react
        const NOTIFY_NEIGHBORS                      = 0b000_0000_0001;
        /// Notifies listeners (like clients) that the block has changed.
        /// On the server, this triggers a packet to be sent to players in range
        const NOTIFY_LISTENERS                      = 0b000_0000_0010;
        /// Combines neighbor notification and listener notification.
        /// This is the "standard" update used for most block placements
        const NOTIFY_ALL                            = 0b000_0000_0011;
        /// Forces the block state to be set even if it matches the current state
        /// Used by items like the Debug Stick to bypass "virtual" change checks
        const FORCE_STATE                           = 0b000_0000_0100;
        /// Prevents the previous block from dropping items when it is replaced
        /// Commonly used when a block is "transformed" rather than destroyed
        const SKIP_DROPS                            = 0b000_0000_1000;
        /// Signals that the block is being moved (usually by a piston)
        /// This prevents certain "on-break" logic from firing until the move is complete
        const MOVED                                 = 0b000_0001_0000;
        /// Prevents redstone wire from re-calculating its shape/power immediately
        /// Used during massive redstone updates to reduce calculation lag
        const SKIP_REDSTONE_WIRE_STATE_REPLACEMENT  = 0b000_0010_0000;
        /// If set, the `on_replaced` callback for block entities (containers) is skipped
        /// Useful if you are moving a Block Entity and don't want it to drop its contents yet
        const SKIP_BLOCK_ENTITY_REPLACED_CALLBACK   = 0b000_0100_0000;
        /// Prevents the `on_added` logic from firing for the new block state
        /// Use this to avoid recursive placement loops or unnecessary initialization
        const SKIP_BLOCK_ADDED_CALLBACK             = 0b000_1000_0000;
        /// Vanilla `Block.UPDATE_KNOWN_SHAPE`: skips the neighbour shape updates for this
        /// change. Used when a multi-block structure such as a nether portal is written in
        /// one go, so the blocks are not removed again while the structure is incomplete.
        const UPDATE_KNOWN_SHAPE                    = 0b001_0000_0000;
        /// Internal to bulk placement: skips the neighbour, shape and light updates of this
        /// change, the caller runs them once after all writes.
        const DEFER_NEIGHBOR_UPDATES                = 0b010_0000_0000;
        /// Also updates the neighbours of the neighbours of the changed block, which strong
        /// redstone power needs. Only bulk block changes honour it.
        const UPDATE_INDIRECT_NEIGHBORS             = 0b100_0000_0000;
    }
}

#[derive(Debug, Error)]
pub enum GetBlockError {
    InvalidBlockId,
    BlockOutOfWorldBounds,
}

impl std::fmt::Display for GetBlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

pub trait WorldPortalExt: Send + Sync {
    fn can_place_at(
        &self,
        block: &Block,
        state: &BlockState,
        block_accessor: &dyn BlockAccessor,
        block_pos: &BlockPos,
    ) -> bool;

    fn mirror(&self, block: &Block, state_id: BlockStateId, mirror: Mirror) -> &'static BlockState;

    fn rotate(
        &self,
        block: &Block,
        state_id: BlockStateId,
        rotation: Rotation,
    ) -> &'static BlockState;

    fn spawn_mobs_for_chunk_generation(
        &self,
        cache: &mut dyn GenerationCache,
        biome: &'static Biome,
        chunk_x: i32,
        chunk_z: i32,
    );

    fn spawn_structure_entities(&self, _entities: Vec<NbtCompound>) {}

    /// Places a plugin feature (see `generation::feature::additive`) in the chunk being
    /// generated. `positions` are the origins the placement rule chose, `seed` is the decorator
    /// seed of the rule. Runs on a generation worker thread, writes only reach `chunk`. The
    /// default does nothing.
    ///
    /// The call returns once the plugin placed its blocks: the generator reads the chunk again
    /// afterwards, so an implementation may wait for the plugin but must not let that wait delay
    /// the server's own plugin work.
    fn place_plugin_feature(
        &self,
        _feature_key: &str,
        _chunk: &mut crate::ProtoChunk,
        _step: usize,
        _seed: u64,
        _positions: &[BlockPos],
    ) {
    }
}

pub trait BlockAccessor: Send + Sync {
    fn get_block(&self, position: &BlockPos) -> &'static Block;

    fn get_block_state(&self, position: &BlockPos) -> &'static BlockState;

    fn get_block_state_id(&self, position: &BlockPos) -> BlockStateId;

    fn get_block_and_state(&self, position: &BlockPos) -> (&'static Block, &'static BlockState);

    /// The shape that a plugin gives for the block at `position` when its shape depends on more
    /// than the state, in block units. `None` for every block without that, the shapes of the
    /// state apply then.
    fn dynamic_shapes(
        &self,
        _position: &BlockPos,
        _state: &BlockState,
        _kind: pumpkin_data::block_registry::ShapeKind,
    ) -> Option<std::sync::Arc<[pumpkin_util::math::boundingbox::BoundingBox]>> {
        None
    }

    /// Clips a ray segment against the block and fluid shapes in this accessor.
    fn clip(&self, context: &crate::clip::ClipContext) -> crate::clip::BlockHitResult {
        crate::clip::clip(self, context)
    }
}
