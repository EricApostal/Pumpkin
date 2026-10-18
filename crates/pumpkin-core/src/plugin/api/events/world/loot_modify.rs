use pumpkin_data::item_stack::ItemStack;
use pumpkin_macros::{Event, cancellable};
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use uuid::Uuid;

/// Where loot came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LootSource {
    Block,
    Entity,
}

/// Fired for the loot a block or an entity death generated, before it drops.
///
/// A handler can change
/// the stacks (like a global loot modifier of other loaders) or cancel the event to drop nothing
/// from the loot table. Equipment of a dying entity is not part of it, `LivingDropsEvent` has it.
#[cancellable]
#[derive(Event, Clone)]
pub struct LootModifyEvent {
    pub source: LootSource,
    /// The loot table, when the host knows its key.
    pub loot_table: Option<String>,
    /// The key of the damage type that killed the entity.
    pub damage_source: Option<String>,
    pub killer_uuid: Option<Uuid>,
    pub killer_id: Option<i32>,
    /// The tool the loot was rolled with.
    pub tool: Option<ItemStack>,
    pub looting_level: u32,
    pub block_pos: Option<BlockPos>,
    /// The state id of the broken block.
    pub block_state: Option<u16>,
    pub entity_id: Option<i32>,
    pub entity_type: Option<String>,
    pub position: Vector3<f64>,
    /// The generated stacks.
    pub loot: Vec<ItemStack>,
}

impl LootModifyEvent {
    #[must_use]
    pub const fn new(source: LootSource, position: Vector3<f64>, loot: Vec<ItemStack>) -> Self {
        Self {
            source,
            loot_table: None,
            damage_source: None,
            killer_uuid: None,
            killer_id: None,
            tool: None,
            looting_level: 0,
            block_pos: None,
            block_state: None,
            entity_id: None,
            entity_type: None,
            position,
            loot,
            cancelled: false,
        }
    }
}
