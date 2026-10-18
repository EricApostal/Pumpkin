//! Events of living entities, ticks and chunk sections, for plugins that port gameplay mods.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use pumpkin_data::item_stack::ItemStack;
use pumpkin_macros::{Event, cancellable};
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use uuid::Uuid;

use crate::entity::player::Player;
use crate::plugin::api::events::player::PlayerEvent;

/// Fired when a living entity dies, with the stacks it is about to drop (loot and equipment).
/// A handler can change the list or cancel the event to drop nothing.
#[cancellable]
#[derive(Event, Clone)]
pub struct LivingDropsEvent {
    /// The ID of the entity that died.
    pub entity_id: i32,
    /// The ID of the entity that killed it, if any.
    pub killer_id: Option<i32>,
    /// Where the entity died.
    pub position: Vector3<f64>,
    /// The stacks that drop.
    pub drops: Vec<ItemStack>,
}

impl LivingDropsEvent {
    #[must_use]
    pub const fn new(
        entity_id: i32,
        killer_id: Option<i32>,
        position: Vector3<f64>,
        drops: Vec<ItemStack>,
    ) -> Self {
        Self {
            entity_id,
            killer_id,
            position,
            drops,
            cancelled: false,
        }
    }
}

/// Fired when the stack in an equipment slot of a living entity changed and clients are told.
///
/// Informational. `old_stack` is what the previous event reported for the slot, it is empty for
/// the first event of a slot, and the event is only built while a handler listens.
#[derive(Event, Clone)]
pub struct LivingEquipmentChangeEvent {
    /// The ID of the entity.
    pub entity_id: i32,
    /// The slot, like `mainhand`, `offhand`, `feet`, `legs`, `chest`, `head`.
    pub slot: String,
    /// The stack that was in the slot.
    pub old_stack: ItemStack,
    /// The stack that is in the slot.
    pub new_stack: ItemStack,
}

impl LivingEquipmentChangeEvent {
    #[must_use]
    pub const fn new(
        entity_id: i32,
        slot: String,
        old_stack: ItemStack,
        new_stack: ItemStack,
    ) -> Self {
        Self {
            entity_id,
            slot,
            old_stack,
            new_stack,
        }
    }
}

/// Fired every tick a player would lose or regain air. A handler sets `air_change`, which is
/// added to the air supply (clamped), or cancels the event to keep the air as it is.
#[cancellable]
#[derive(Event, Clone)]
pub struct LivingBreatheEvent {
    /// The ID of the entity.
    pub entity_id: i32,
    /// Whether the head is in water.
    pub submerged: bool,
    /// The air supply now.
    pub air: i32,
    /// The change of the air supply this tick, negative while submerged.
    pub air_change: i32,
}

impl LivingBreatheEvent {
    #[must_use]
    pub const fn new(entity_id: i32, submerged: bool, air: i32, air_change: i32) -> Self {
        Self {
            entity_id,
            submerged,
            air,
            air_change,
            cancelled: false,
        }
    }
}

/// Fired when a mob checks how far it sees a target. A handler sets `multiplier`, which scales
/// the range of the check (1.0 is unchanged, 0.0 makes the target seen only from the minimum).
#[derive(Event, Clone)]
pub struct LivingVisibilityEvent {
    /// The ID of the entity that is looked at.
    pub entity_id: i32,
    /// The ID of the entity that looks.
    pub looking_entity_id: i32,
    /// The factor of the range.
    pub multiplier: f64,
}

impl LivingVisibilityEvent {
    #[must_use]
    pub const fn new(entity_id: i32, looking_entity_id: i32) -> Self {
        Self {
            entity_id,
            looking_entity_id,
            multiplier: 1.0,
        }
    }
}

/// Fired when the pose of an entity changed and its dimensions were computed for it. A handler
/// can change the dimensions the entity gets.
#[derive(Event, Clone)]
pub struct EntitySizeEvent {
    /// The ID of the entity.
    pub entity_id: i32,
    /// The new pose.
    pub pose: String,
    /// The width of the bounding box.
    pub width: f32,
    /// The height of the bounding box.
    pub height: f32,
    /// The eye height above the bottom of the box.
    pub eye_height: f32,
}

impl EntitySizeEvent {
    #[must_use]
    pub const fn new(
        entity_id: i32,
        pose: String,
        width: f32,
        height: f32,
        eye_height: f32,
    ) -> Self {
        Self {
            entity_id,
            pose,
            width,
            height,
            eye_height,
        }
    }
}

/// Fired every tick for every player, after the player ticked.
#[derive(Event, Clone)]
pub struct PlayerTickEvent {
    /// The player.
    pub player: Arc<Player>,
}

impl PlayerTickEvent {
    #[must_use]
    pub const fn new(player: Arc<Player>) -> Self {
        Self { player }
    }
}

impl PlayerEvent for PlayerTickEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}

/// Fired for the entities of the types a plugin opted in with `set-entity-ticking`, after they
/// ticked. Players have their own `PlayerTickEvent`.
#[derive(Event, Clone)]
pub struct EntityTickEvent {
    /// The ID of the entity.
    pub entity_id: i32,
    /// The key of the type of the entity.
    pub entity_type: String,
    /// The tick of the server.
    pub tick: i32,
}

impl EntityTickEvent {
    #[must_use]
    pub const fn new(entity_id: i32, entity_type: String, tick: i32) -> Self {
        Self {
            entity_id,
            entity_type,
            tick,
        }
    }
}

/// Fired when an entity moved into another 16x16x16 chunk section. Informational. The sections
/// are `floor(coordinate / 16)` of the old and new block position.
#[derive(Event, Clone)]
pub struct EntityEnteringSectionEvent {
    /// The ID of the entity.
    pub entity_id: i32,
    /// The section the entity left.
    pub old_section: BlockPos,
    /// The section the entity entered.
    pub new_section: BlockPos,
}

impl EntityEnteringSectionEvent {
    #[must_use]
    pub const fn new(entity_id: i32, old_section: BlockPos, new_section: BlockPos) -> Self {
        Self {
            entity_id,
            old_section,
            new_section,
        }
    }
}

/// Fired when a player starts tracking an entity, the server is about to send it to the player.
/// Informational. Fired for plugin and vanilla entities.
#[derive(Event, Clone)]
pub struct EntityStartTrackingEvent {
    /// The ID of the entity.
    pub entity_id: i32,
    /// The player that starts tracking it.
    pub player: Uuid,
}

/// Fired when a player stops tracking an entity, the server removes it from the player.
/// Informational.
#[derive(Event, Clone)]
pub struct EntityStopTrackingEvent {
    /// The ID of the entity.
    pub entity_id: i32,
    /// The player that stops tracking it.
    pub player: Uuid,
}

/// Highest entity type id that can tick for plugins.
const MAX_TICKING_TYPES: usize = 2048;

/// The tick interval of each entity type, 0 when it does not tick for plugins.
static TICK_INTERVALS: [AtomicU32; MAX_TICKING_TYPES] =
    [const { AtomicU32::new(0) }; MAX_TICKING_TYPES];

/// How many types have an interval, so the tick loop can tell with one load that nothing ticks.
static TICKING_TYPES: AtomicU32 = AtomicU32::new(0);

/// Sets how often the entities of a type fire `EntityTickEvent`, in ticks, 0 stops it. Returns
/// false for an id that cannot be ticked.
pub fn set_entity_tick_interval(entity_type_id: u16, interval: u32) -> bool {
    let Some(slot) = TICK_INTERVALS.get(usize::from(entity_type_id)) else {
        return false;
    };
    let previous = slot.swap(interval, Ordering::AcqRel);
    match (previous == 0, interval == 0) {
        (true, false) => {
            TICKING_TYPES.fetch_add(1, Ordering::AcqRel);
        }
        (false, true) => {
            TICKING_TYPES.fetch_sub(1, Ordering::AcqRel);
        }
        _ => {}
    }
    true
}

/// The tick interval of an entity type, 0 when it does not tick for plugins.
#[must_use]
pub fn entity_tick_interval(entity_type_id: u16) -> u32 {
    TICK_INTERVALS
        .get(usize::from(entity_type_id))
        .map_or(0, |slot| slot.load(Ordering::Relaxed))
}

/// Whether any entity type ticks for plugins. Cheap, meant for the tick loop.
#[must_use]
pub fn any_entity_ticking() -> bool {
    TICKING_TYPES.load(Ordering::Acquire) != 0
}
