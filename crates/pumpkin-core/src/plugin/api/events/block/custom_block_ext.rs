//! Events of the extension hooks of custom blocks (shapes, interaction, physics and queries), see
//! `BlockCallbacks`. They are fired only for blocks that opted in to the hook.

use std::sync::Arc;

use pumpkin_data::block_registry::ShapeKind;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::{Block, BlockDirection};
use pumpkin_macros::{Event, cancellable};
use pumpkin_util::Hand;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use uuid::Uuid;

use crate::entity::player::Player;
use crate::world::World;

/// The shape of a custom block with the `dynamic_shape` callback is asked for.
///
/// A handler changes
/// `shapes`, boxes in block units (`0..1` is the block). It starts as the shape that is registered
/// for the state. The answer is cached per position until `World::invalidate_block_shape`.
#[derive(Event, Clone)]
pub struct CustomBlockShapeEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub kind: ShapeKind,
    pub shapes: Vec<BoundingBox>,
}

/// The collision shape of a custom block with the `entity_collision_shape` callback is asked for
/// while `entity` moves.
///
/// A handler changes `shapes`, boxes in block units (`0..1` is the block). It starts as the
/// collision shape that is registered for the state, and an empty list lets the entity pass. Unlike
/// [`CustomBlockShapeEvent`] the answer is never cached: it depends on the entity and is asked
/// again for every move.
#[derive(Event, Clone)]
pub struct CustomBlockCollisionShapeEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub entity_id: i32,
    pub entity_uuid: Uuid,
    pub entity_type: &'static EntityType,
    pub entity_position: Vector3<f64>,
    pub shapes: Vec<BoundingBox>,
}

/// What a use of a block gives back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustomBlockUseResult {
    /// Handled, the hand swings.
    Success,
    /// Handled, nothing else runs.
    Consume,
    /// Not handled, the item may still be used.
    Pass,
    /// Not handled, other actions still run.
    Fail,
    /// Only for a use with an item: not handled, try the use without an item.
    TryEmptyHand,
}

/// A player right clicked a custom block with the `use_without_item` or `use_with_item` callback.
/// A handler sets `result`.
#[derive(Event, Clone)]
pub struct CustomBlockUseEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub player: Arc<Player>,
    pub hand: Hand,
    /// Whether this is the use with an item.
    pub with_item: bool,
    /// A copy of the item in hand.
    pub item: ItemStack,
    pub face: BlockDirection,
    /// Where on the block the click hit, `0..1` on each axis.
    pub cursor: Vector3<f32>,
    pub sneaking: bool,
    /// `Pass` for a use without item and `TryEmptyHand` with one to start with.
    pub result: CustomBlockUseResult,
}

/// A player left clicked (started digging) a custom block with the `attack` callback.
#[derive(Event, Clone)]
pub struct CustomBlockAttackEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub player: Arc<Player>,
}

/// A player picks a custom block with the `get_clone_item` callback. A handler sets `item`, none
/// picks nothing. It starts as the item linked with the block.
#[derive(Event, Clone)]
pub struct CustomBlockCloneItemEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub player: Arc<Player>,
    pub item: Option<ItemStack>,
}

/// A player is about to break a custom block with the `player_will_destroy` callback.
#[cancellable]
#[derive(Event, Clone)]
pub struct CustomBlockPlayerWillDestroyEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub player: Arc<Player>,
}

/// The physics value of a custom block that is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CustomBlockPhysicsKind {
    Friction,
    SpeedFactor,
    JumpFactor,
}

/// The friction, speed factor or jump factor of a custom block with the matching callback is asked
/// for. A handler sets `value`, which starts as the default of the block. Cached per position.
#[derive(Event, Clone)]
pub struct CustomBlockPhysicsEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub kind: CustomBlockPhysicsKind,
    pub value: f32,
}

/// An entity landed on a custom block with the `on_landed_upon` callback. A handler sets
/// `damage_multiplier`, 0 cancels the fall damage.
#[derive(Event, Clone)]
pub struct CustomBlockLandedEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub entity_id: i32,
    pub fall_distance: f32,
    pub damage_multiplier: f32,
}

/// The yes or no question that is asked about a custom block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CustomBlockCondition {
    /// Whether an entity (explosion, wither) can destroy the block.
    EntityDestroy,
    PathfindableLand,
    PathfindableWater,
    PathfindableAir,
    /// Whether redstone wire connects to the block.
    RedstoneConnect,
}

/// A custom block with the `can_entity_destroy`, `is_pathfindable` or `can_connect_redstone`
/// callback is asked a question. A handler sets `value`, which starts as the default answer.
#[derive(Event, Clone)]
pub struct CustomBlockConditionEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub condition: CustomBlockCondition,
    /// The type of the entity for `EntityDestroy`, none for an explosion.
    pub entity_type: Option<&'static EntityType>,
    /// The direction from the wire to the block for `RedstoneConnect`.
    pub direction: Option<BlockDirection>,
    pub value: bool,
}

/// The sound type of a custom block with the `get_sound_type` callback is asked for. A handler
/// sets `sound_type`, a vanilla sound type name like `stone`.
#[derive(Event, Clone)]
pub struct CustomBlockSoundTypeEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    pub sound_type: String,
}

/// How fast a custom block with the `get_fire_spread` callback catches fire and burns. A handler
/// sets the chances, 0 to 255 like vanilla `setFlammable`. Both 0 means the block does not burn.
#[derive(Event, Clone)]
pub struct CustomBlockFireSpreadEvent {
    pub world: Arc<World>,
    pub position: BlockPos,
    pub block: &'static Block,
    pub state_index: u16,
    /// The chance fire next to the block spreads to it.
    pub spread_chance: u8,
    /// The chance the block burns away.
    pub burn_chance: u8,
}
