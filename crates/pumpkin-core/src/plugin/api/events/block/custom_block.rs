//! Events of the hooks that a custom block opted in to, see `BlockCallbacks`. They are fired for
//! the blocks that plugins register, only when the block asked for the hook.

use std::sync::Arc;

use pumpkin_data::entity::EntityType;
use pumpkin_data::{Block, BlockDirection, BlockStateId};
use pumpkin_macros::Event;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use uuid::Uuid;

use crate::world::World;

/// A neighbour of a custom block changed.
#[derive(Event, Clone)]
pub struct CustomBlockNeighborUpdateEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    /// The index of the state of the block, as in the registration.
    pub state_index: u16,
    /// Where the change happened.
    pub source_position: BlockPos,
    /// The block that changed.
    pub source_block: &'static Block,
}

/// A custom block was set in the world, by a player (`player` is set) or in any other way.
#[derive(Event, Clone)]
pub struct CustomBlockPlacedEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub player: Option<Uuid>,
    /// The state that was there before, unknown when a player placed the block.
    pub old_state_id: Option<BlockStateId>,
    /// The block was moved there, for example by a piston.
    pub moved: bool,
}

/// A custom block was replaced by another block or moved away.
#[derive(Event, Clone)]
pub struct CustomBlockStateReplacedEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    /// The index of the state that the block had.
    pub old_state_index: u16,
    pub moved: bool,
}

/// A player broke a custom block.
#[derive(Event, Clone)]
pub struct CustomBlockBrokenEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub player: Uuid,
}

/// A tick scheduled for a custom block is due.
#[derive(Event, Clone)]
pub struct CustomBlockScheduledTickEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
}

/// A custom block got a random tick.
#[derive(Event, Clone)]
pub struct CustomBlockRandomTickEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
}

/// An entity is inside a custom block.
#[derive(Event, Clone)]
pub struct CustomBlockEntityCollisionEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub entity_id: i32,
    pub entity_uuid: Uuid,
    pub entity_type: &'static EntityType,
    pub entity_position: Vector3<f64>,
}

/// An entity stands on or walks over a custom block.
#[derive(Event, Clone)]
pub struct CustomBlockEntityStepEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub entity_id: i32,
    pub entity_uuid: Uuid,
    pub entity_type: &'static EntityType,
    pub entity_position: Vector3<f64>,
    pub below_supporting_block: bool,
}

/// A custom block is about to be placed. A handler sets `can_place` to false to refuse it.
#[derive(Event, Clone)]
pub struct CustomBlockCanPlaceEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    /// The index of the state that is about to be placed.
    pub state_index: u16,
    /// The face of the clicked block, when a player places.
    pub face: Option<BlockDirection>,
    pub player: Option<Uuid>,
    /// Whether the block can be placed, true to start with.
    pub can_place: bool,
}

/// A neighbour of a custom block changed and the block updates its shape. A handler sets
/// `new_state_index` to change the state the block ends up with.
#[derive(Event, Clone)]
pub struct CustomBlockUpdateShapeEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    /// The index of the current state of the block.
    pub state_index: u16,
    /// From the block to the changed neighbour.
    pub direction: BlockDirection,
    pub neighbor_position: BlockPos,
    pub neighbor_block: &'static Block,
    pub neighbor_state_id: BlockStateId,
    /// The state index the block gets, the one its connect rules give to start with.
    pub new_state_index: u16,
}

/// A redstone reader asks a custom block for its power, for the `weak_power` (`strong` is false)
/// or `strong_power` (`strong` is true) callback. A handler sets `power`, 0 to 15. Fired only when
/// the plugin did not store an output for the block, see `World::set_plugin_redstone_output`.
#[derive(Event, Clone)]
pub struct CustomBlockRedstonePowerEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    /// The side of the block that the power is read from, the one facing the reader.
    pub face: BlockDirection,
    /// Whether the strong power is asked for, the weak power otherwise.
    pub strong: bool,
    /// The power of the block on `face`, 0 to start with.
    pub power: u8,
}

/// A comparator reads a custom block that set the `comparator-output` callback. A handler sets
/// `output`, `None` means the block has no analog output. Fired only when the plugin did not
/// store an output for the block, see `World::set_plugin_comparator_output`.
#[derive(Event, Clone)]
pub struct CustomBlockComparatorOutputEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    /// The side of the block the comparator sits against.
    pub face: BlockDirection,
    /// The analog output, 0 to 15, `None` to start with.
    pub output: Option<u8>,
}
