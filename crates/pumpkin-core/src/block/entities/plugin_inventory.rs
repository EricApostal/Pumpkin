//! The slots of a plugin block entity, see `PluginBlockEntity::register_inventory`.

use std::any::Any;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use pumpkin_data::item_stack::ItemStack;
use pumpkin_inventory::{Clearable, Inventory};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;

/// Most slots a plugin inventory can have.
pub const MAX_SLOTS: usize = 4096;
/// Key of the compound with the slots in the saved plugin block entity.
pub const INVENTORY_KEY: &str = "PluginInventory";

/// A list of slots that lives in a plugin block entity. Vanilla hoppers and other containers reach
/// it through [`Inventory`] like any container, every slot from every face.
pub struct PluginInventory {
    slots: RwLock<Vec<ItemStack>>,
    /// Most items a slot takes, before the max stack size of the item.
    slot_limit: AtomicU8,
    dirty: AtomicBool,
}

impl PluginInventory {
    #[must_use]
    pub fn new(slot_count: usize, slot_limit: u8) -> Self {
        Self {
            slots: RwLock::new(vec![ItemStack::EMPTY.clone(); slot_count]),
            slot_limit: AtomicU8::new(slot_limit.max(1)),
            dirty: AtomicBool::new(false),
        }
    }

    /// Reads the form written by [`Self::write_nbt`].
    #[must_use]
    pub fn from_nbt(nbt: &NbtCompound) -> Option<Self> {
        let size = usize::try_from(nbt.get_int("Size")?).ok()?.min(MAX_SLOTS);
        let limit = nbt
            .get_int("Limit")
            .map_or(99, |limit| limit.clamp(1, 99) as u8);
        let inventory = Self::new(size, limit);
        {
            let mut slots = inventory
                .slots
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for tag in nbt.get_list("Items").unwrap_or_default() {
                let Some(compound) = tag.extract_compound() else {
                    continue;
                };
                let Some(slot) = compound.get_int("Slot") else {
                    continue;
                };
                if let Some(target) = usize::try_from(slot).ok().and_then(|s| slots.get_mut(s))
                    && let Some(stack) = ItemStack::read_item_stack(compound)
                {
                    *target = stack;
                }
            }
        }
        Some(inventory)
    }

    /// The saved form: `Size`, `Limit` and the non empty slots as `Items` with an int `Slot`.
    #[must_use]
    pub fn write_nbt(&self) -> NbtCompound {
        let slots = self
            .slots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut nbt = NbtCompound::new();
        nbt.put_int("Size", slots.len() as i32);
        nbt.put_int("Limit", i32::from(self.slot_limit.load(Ordering::Relaxed)));
        let items: Vec<NbtTag> = slots
            .iter()
            .enumerate()
            .filter(|(_, stack)| !stack.is_empty())
            .map(|(index, stack)| {
                let mut item = NbtCompound::new();
                item.put_int("Slot", index as i32);
                stack.write_item_stack(&mut item);
                NbtTag::Compound(item)
            })
            .collect();
        nbt.put_list("Items", items);
        nbt
    }

    /// Changes the slot count and limit. Growing keeps the items. Shrinking fails, and changes
    /// nothing, if a slot that would go is not empty.
    pub fn resize(&self, slot_count: usize, slot_limit: u8) -> Result<(), String> {
        let mut slots = self
            .slots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot_count < slots.len() && slots[slot_count..].iter().any(|s| !s.is_empty()) {
            return Err(format!(
                "cannot shrink the inventory to {slot_count} slots, a slot that would go is not empty"
            ));
        }
        slots.resize(slot_count, ItemStack::EMPTY.clone());
        self.slot_limit.store(slot_limit.max(1), Ordering::Relaxed);
        self.dirty.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Takes every stack out, in slot order, keeping the slot count.
    pub fn take_all(&self) -> Vec<ItemStack> {
        let mut slots = self
            .slots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.dirty.store(true, Ordering::Relaxed);
        slots
            .iter_mut()
            .map(|slot| std::mem::replace(slot, ItemStack::EMPTY.clone()))
            .filter(|stack| !stack.is_empty())
            .collect()
    }

    /// Whether the slots changed since the last call.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }
}

impl Inventory for PluginInventory {
    fn size(&self) -> usize {
        self.slots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    fn is_empty(&self) -> bool {
        self.slots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .all(ItemStack::is_empty)
    }

    fn get_stack(&self, slot: usize) -> ItemStack {
        self.slots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(slot)
            .cloned()
            .unwrap_or_else(|| ItemStack::EMPTY.clone())
    }

    fn remove_stack(&self, slot: usize) -> ItemStack {
        let mut slots = self
            .slots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slots.get_mut(slot).map_or_else(
            || ItemStack::EMPTY.clone(),
            |stack| {
                self.dirty.store(true, Ordering::Relaxed);
                std::mem::replace(stack, ItemStack::EMPTY.clone())
            },
        )
    }

    fn remove_stack_specific(&self, slot: usize, amount: u8) -> ItemStack {
        let mut slots = self
            .slots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match slots.get_mut(slot) {
            Some(stack) if !stack.is_empty() && amount > 0 => {
                self.dirty.store(true, Ordering::Relaxed);
                stack.split(amount)
            }
            _ => ItemStack::EMPTY.clone(),
        }
    }

    fn set_stack(&self, slot: usize, stack: ItemStack) {
        let mut slots = self
            .slots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(target) = slots.get_mut(slot) {
            *target = stack;
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    fn get_max_count_per_stack(&self) -> u8 {
        self.slot_limit.load(Ordering::Relaxed)
    }

    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Clearable for PluginInventory {
    fn clear(&self) {
        let mut slots = self
            .slots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slots.fill_with(|| ItemStack::EMPTY.clone());
        self.dirty.store(true, Ordering::Relaxed);
    }
}
