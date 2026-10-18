//! Hooks that replace the small behaviour mixins of gameplay mods. Each one is only built and
//! fired while a plugin listens, and the ones that are per entity or item need an opt-in.

use std::{
    collections::HashMap,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU32, Ordering},
    },
};

use pumpkin_data::{BlockDirection, item_stack::ItemStack};
use pumpkin_macros::{Event, cancellable};
use pumpkin_util::{
    Hand,
    math::{position::BlockPos, vector3::Vector3},
};

use crate::entity::player::Player;
use crate::plugin::api::events::player::PlayerEvent;
use crate::world::World;

/// Fired after an entity with the `fluid-movement-events` flag moved in a fluid. A handler can
/// change `velocity`, which is stored as the velocity of the entity.
#[derive(Event, Clone)]
pub struct EntityFluidMovementEvent {
    /// The ID of the entity.
    pub entity_id: i32,
    /// The fluid the entity is in, `minecraft:water` or `minecraft:lava`.
    pub fluid: String,
    /// The velocity after the movement.
    pub velocity: Vector3<f64>,
}

impl EntityFluidMovementEvent {
    #[must_use]
    pub const fn new(entity_id: i32, fluid: String, velocity: Vector3<f64>) -> Self {
        Self {
            entity_id,
            fluid,
            velocity,
        }
    }
}

/// Fired every tick a player keeps using an item (eating, drawing a bow), for custom use effects.
/// Informational.
#[derive(Event, Clone)]
pub struct ItemUseTickEvent {
    /// The player.
    pub player: Arc<Player>,
    /// The stack in use.
    pub stack: ItemStack,
    /// Ticks left until the use is done.
    pub remaining_ticks: i32,
}

impl ItemUseTickEvent {
    #[must_use]
    pub const fn new(player: Arc<Player>, stack: ItemStack, remaining_ticks: i32) -> Self {
        Self {
            player,
            stack,
            remaining_ticks,
        }
    }
}

impl PlayerEvent for ItemUseTickEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}

/// Fired when a player swings an arm. Cancelling it stops the swing and what follows from it
/// (the click on a block or the air).
#[cancellable]
#[derive(Event, Clone)]
pub struct ItemSwingEvent {
    /// The player.
    pub player: Arc<Player>,
    /// The hand that swings.
    pub hand: Hand,
    /// A copy of the item in that hand.
    pub stack: ItemStack,
}

impl ItemSwingEvent {
    #[must_use]
    pub const fn new(player: Arc<Player>, hand: Hand, stack: ItemStack) -> Self {
        Self {
            player,
            hand,
            stack,
            cancelled: false,
        }
    }
}

impl PlayerEvent for ItemSwingEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}

/// What a handler of `ItemUseFirstEvent` did with the click.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ItemUseFirstResult {
    /// The click goes on: the block handles it, then the item.
    #[default]
    Pass,
    /// The click is done, the player does not swing.
    Consume,
    /// The click is done and the player swings.
    Success,
}

/// Fired when a player right clicks a block, before the block handles the click. A handler sets
/// `result` to take the click away from the block and the item.
#[derive(Event, Clone)]
pub struct ItemUseFirstEvent {
    /// The player.
    pub player: Arc<Player>,
    /// The hand of the click.
    pub hand: Hand,
    /// A copy of the item in that hand.
    pub stack: ItemStack,
    /// The clicked block.
    pub position: BlockPos,
    /// The face that was clicked.
    pub face: BlockDirection,
    /// What happens with the click.
    pub result: ItemUseFirstResult,
}

impl ItemUseFirstEvent {
    #[must_use]
    pub const fn new(
        player: Arc<Player>,
        hand: Hand,
        stack: ItemStack,
        position: BlockPos,
        face: BlockDirection,
    ) -> Self {
        Self {
            player,
            hand,
            stack,
            position,
            face,
            result: ItemUseFirstResult::Pass,
        }
    }
}

impl PlayerEvent for ItemUseFirstEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}

/// Fired when a player starts to break a block, before the block gets damaged. Cancelling it
/// keeps the player from attacking the block (the block is sent to the client again).
#[cancellable]
#[derive(Event, Clone)]
pub struct CanAttackBlockEvent {
    /// The player.
    pub player: Arc<Player>,
    /// A copy of the item in the main hand.
    pub stack: ItemStack,
    /// The block.
    pub position: BlockPos,
}

impl CanAttackBlockEvent {
    #[must_use]
    pub const fn new(player: Arc<Player>, stack: ItemStack, position: BlockPos) -> Self {
        Self {
            player,
            stack,
            position,
            cancelled: false,
        }
    }
}

impl PlayerEvent for CanAttackBlockEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}

/// Fired after an item entity ticked, for the item keys set with `set-item-entity-ticking`.
/// Informational.
#[derive(Event, Clone)]
pub struct EntityItemUpdateEvent {
    /// The ID of the item entity.
    pub entity_id: i32,
    /// A copy of the stack.
    pub stack: ItemStack,
    /// Where the item entity is.
    pub position: Vector3<f64>,
    /// How many ticks it lives.
    pub age: i32,
}

impl EntityItemUpdateEvent {
    #[must_use]
    pub const fn new(entity_id: i32, stack: ItemStack, position: Vector3<f64>, age: i32) -> Self {
        Self {
            entity_id,
            stack,
            position,
            age,
        }
    }
}

/// Fired when a fake player is about to place a block item on a clicked block. Cancelling it
/// stops the placement and lets the click pass (a deployer must not build next to a block it
/// cannot replace).
#[cancellable]
#[derive(Event, Clone)]
pub struct FakePlayerPlaceCheckEvent {
    /// The fake player.
    pub player: Arc<Player>,
    /// A copy of the item to place.
    pub stack: ItemStack,
    /// The clicked block.
    pub position: BlockPos,
    /// The face that was clicked.
    pub face: BlockDirection,
    /// Whether the clicked block can be replaced by the placed block.
    pub clicked_replaceable: bool,
}

impl FakePlayerPlaceCheckEvent {
    #[must_use]
    pub const fn new(
        player: Arc<Player>,
        stack: ItemStack,
        position: BlockPos,
        face: BlockDirection,
        clicked_replaceable: bool,
    ) -> Self {
        Self {
            player,
            stack,
            position,
            face,
            clicked_replaceable,
            cancelled: false,
        }
    }
}

impl PlayerEvent for FakePlayerPlaceCheckEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}

/// Fired when flowing fluid wants to spread sideways into a block. Cancelling it keeps the
/// fluid from passing. Only fired while a plugin listens.
#[cancellable]
#[derive(Event, Clone)]
pub struct FluidCanPassThroughEvent {
    /// The world.
    pub world: Arc<World>,
    /// The block the fluid flows from.
    pub position: BlockPos,
    /// The direction it flows to.
    pub direction: BlockDirection,
    /// The fluid, like `minecraft:water`.
    pub fluid: String,
}

impl FluidCanPassThroughEvent {
    #[must_use]
    pub const fn new(
        world: Arc<World>,
        position: BlockPos,
        direction: BlockDirection,
        fluid: String,
    ) -> Self {
        Self {
            world,
            position,
            direction,
            fluid,
            cancelled: false,
        }
    }
}

/// The tick interval of the item entities of each item id, 0 or missing when they do not tick
/// for plugins.
static ITEM_TICK_INTERVALS: RwLock<Option<HashMap<u16, u32>>> = RwLock::new(None);

/// How many items have an interval, so the item tick can tell with one load that nothing ticks.
static TICKING_ITEMS: AtomicU32 = AtomicU32::new(0);

/// Sets how often the item entities of an item fire `EntityItemUpdateEvent`, in ticks, 0 stops it.
pub fn set_item_entity_tick_interval(item_id: u16, interval: u32) {
    let mut map = ITEM_TICK_INTERVALS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let map = map.get_or_insert_with(HashMap::new);
    if interval == 0 {
        map.remove(&item_id);
    } else {
        map.insert(item_id, interval);
    }
    TICKING_ITEMS.store(map.len() as u32, Ordering::Release);
}

/// Whether any item ticks for plugins. Cheap, meant for the item tick.
#[must_use]
pub fn any_item_entity_ticking() -> bool {
    TICKING_ITEMS.load(Ordering::Acquire) != 0
}

/// The tick interval of an item, 0 when its entities do not tick for plugins.
#[must_use]
pub fn item_entity_tick_interval(item_id: u16) -> u32 {
    if TICKING_ITEMS.load(Ordering::Acquire) == 0 {
        return 0;
    }
    ITEM_TICK_INTERVALS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|map| map.get(&item_id).copied())
        .unwrap_or(0)
}
