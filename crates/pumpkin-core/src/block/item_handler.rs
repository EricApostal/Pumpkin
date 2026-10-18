//! A view of the slots of a container block through one side, for plugins and other code that moves
//! items without a hopper.
//!
//! The sided rules are the ones hoppers read from [`Inventory`]: `slots_for_face`,
//! `can_place_item_through_face` and `can_take_item_through_face`. Double chests are merged.

use std::sync::Arc;

use pumpkin_data::block_properties::{ChestLikeProperties, ChestType};
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::{Block, BlockDirection};
use pumpkin_inventory::Inventory;
use pumpkin_inventory::double::DoubleInventory;
use pumpkin_util::math::position::BlockPos;

use crate::block::entities::BlockEntity;
use crate::world::World;

/// The slots of one container as seen from a side. Handler slot `n` is the `n`-th slot of
/// `slots_for_face(side)`, so a furnace seen from below has the output first.
pub struct ItemHandler {
    inventory: Arc<dyn Inventory>,
    /// `None` is the inside view: every slot, only the validity of the slot checked.
    side: Option<BlockDirection>,
    slots: Vec<usize>,
}

impl ItemHandler {
    #[must_use]
    pub fn new(inventory: Arc<dyn Inventory>, side: Option<BlockDirection>) -> Self {
        let slots = side.map_or_else(
            || (0..inventory.size()).collect(),
            |face| inventory.slots_for_face(face),
        );
        Self {
            inventory,
            side,
            slots,
        }
    }

    /// The container at a position, a double chest as one container. `None` if the chunk is not
    /// loaded or the block entity has no slots.
    #[must_use]
    pub fn at(world: &World, position: &BlockPos, side: Option<BlockDirection>) -> Option<Self> {
        container_at(world, position).map(|inventory| Self::new(inventory, side))
    }

    #[must_use]
    pub const fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Copy of the stack in a handler slot, empty if out of range.
    #[must_use]
    pub fn get_stack(&self, slot: usize) -> ItemStack {
        self.slots
            .get(slot)
            .map_or_else(|| ItemStack::EMPTY.clone(), |&s| self.inventory.get_stack(s))
    }

    /// Most items the slot takes, before the max stack size of the item.
    #[must_use]
    pub fn get_slot_limit(&self, slot: usize) -> u8 {
        if slot < self.slots.len() {
            self.inventory.get_max_count_per_stack()
        } else {
            0
        }
    }

    /// Whether the stack may be put into the slot through the side, ignoring what it holds.
    #[must_use]
    pub fn is_item_valid(&self, slot: usize, stack: &ItemStack) -> bool {
        self.slots.get(slot).is_some_and(|&s| self.can_place(s, stack))
    }

    fn can_place(&self, slot: usize, stack: &ItemStack) -> bool {
        match self.side {
            Some(face) => self.inventory.can_place_item_through_face(slot, stack, face),
            None => self.inventory.is_valid_slot_for(slot, stack),
        }
    }

    /// Puts as much of `stack` as fits into one slot, or into the slots in order when `slot` is
    /// `None` (stacks it merges with first, then empty slots). Returns what did not fit. With
    /// `simulate` nothing changes.
    #[must_use]
    pub fn insert(&self, slot: Option<usize>, stack: &ItemStack, simulate: bool) -> ItemStack {
        let mut remainder = stack.clone();
        if remainder.is_empty() {
            return remainder;
        }
        match slot {
            Some(slot) => {
                if let Some(&inner) = self.slots.get(slot) {
                    self.insert_into(inner, &mut remainder, simulate, true, true);
                }
            }
            None => {
                for allow_empty in [false, true] {
                    for &inner in &self.slots {
                        if remainder.is_empty() {
                            return remainder;
                        }
                        self.insert_into(inner, &mut remainder, simulate, !allow_empty, allow_empty);
                    }
                }
            }
        }
        remainder
    }

    fn insert_into(
        &self,
        inner: usize,
        remainder: &mut ItemStack,
        simulate: bool,
        merge: bool,
        fill_empty: bool,
    ) {
        let current = self.inventory.get_stack(inner);
        let limit = self
            .inventory
            .get_max_count_per_stack()
            .min(remainder.get_max_stack_size());
        let room = if current.is_empty() {
            if !fill_empty {
                return;
            }
            limit
        } else {
            if !merge
                || !current.are_items_and_components_equal(remainder)
                || current.item_count >= limit
            {
                return;
            }
            limit - current.item_count
        };
        if room == 0 || !self.can_place(inner, remainder) {
            return;
        }
        let moved = room.min(remainder.item_count);
        if !simulate {
            let mut target = if current.is_empty() {
                remainder.copy_with_count(0)
            } else {
                current
            };
            target.item_count += moved;
            self.inventory.set_stack(inner, target);
            self.inventory.mark_dirty();
        }
        remainder.item_count -= moved;
        if remainder.item_count == 0 {
            *remainder = ItemStack::EMPTY.clone();
        }
    }

    /// Takes up to `amount` items out of a slot, if the side may take them. With `simulate`
    /// nothing changes.
    #[must_use]
    pub fn extract(&self, slot: usize, amount: u8, simulate: bool) -> ItemStack {
        let Some(&inner) = self.slots.get(slot) else {
            return ItemStack::EMPTY.clone();
        };
        let current = self.inventory.get_stack(inner);
        if current.is_empty() || amount == 0 {
            return ItemStack::EMPTY.clone();
        }
        if let Some(face) = self.side
            && !self
                .inventory
                .can_take_item_through_face(inner, &current, face)
        {
            return ItemStack::EMPTY.clone();
        }
        let taken = amount.min(current.item_count);
        if simulate {
            return current.copy_with_count(taken);
        }
        let stack = self.inventory.remove_stack_specific(inner, taken);
        if !stack.is_empty() {
            self.inventory.mark_dirty();
        }
        stack
    }
}

/// The inventory of the block entity at a position, with the other half of a double chest.
#[must_use]
pub fn container_at(world: &World, position: &BlockPos) -> Option<Arc<dyn Inventory>> {
    let (block, state) = world.get_block_and_state_id(position);
    let inventory = world.get_block_entity(position).and_then(BlockEntity::get_inventory)?;
    if block != &Block::CHEST && block != &Block::TRAPPED_CHEST {
        return Some(inventory);
    }
    let props = ChestLikeProperties::from_state_id(state);
    let toward = match props.r#type {
        ChestType::Single => return Some(inventory),
        ChestType::Left => props.facing.rotate_clockwise(),
        ChestType::Right => props.facing.rotate_counter_clockwise(),
    };
    let other_pos = position.offset(toward.to_offset());
    let (other_block, other_state) = world.get_block_and_state_id(&other_pos);
    let other_props = ChestLikeProperties::from_state_id(other_state);
    let paired = other_block == block
        && other_props.facing == props.facing
        && matches!(
            (props.r#type, other_props.r#type),
            (ChestType::Left, ChestType::Right) | (ChestType::Right, ChestType::Left)
        );
    let other = paired
        .then(|| world.get_block_entity(&other_pos).and_then(BlockEntity::get_inventory))
        .flatten();
    Some(match other {
        // The right half comes first, like the screen of the chest.
        Some(other) if matches!(props.r#type, ChestType::Right) => {
            DoubleInventory::new(inventory, other)
        }
        Some(other) => DoubleInventory::new(other, inventory),
        None => inventory,
    })
}
