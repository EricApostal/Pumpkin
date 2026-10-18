use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pumpkin_data::block_entity_registry::{PluginBlockEntityType, plugin_block_entity_type_by_key};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use tracing::warn;

use super::BlockEntity;
use super::plugin_inventory::{INVENTORY_KEY, MAX_SLOTS, PluginInventory};
use super::plugin_tank::{FluidContent, MAX_TANKS, PluginTanks, TANKS_KEY};
use pumpkin_inventory::Inventory;

/// Key of the compound with the data of the plugin in the saved block entity.
pub const SAVE_DATA_KEY: &str = "PluginData";
/// Key of the compound with the data that is sent to clients in the saved block entity.
pub const CLIENT_DATA_KEY: &str = "PluginClientData";

/// Stand in for an entity whose type is gone, see [`PluginBlockEntity::from_nbt`].
static UNREGISTERED: PluginBlockEntityType =
    PluginBlockEntityType::new("pumpkin:unregistered", u32::MAX, &[]);

/// A block entity of a type that a plugin registered. The server only stores its data: the plugin
/// owns the behaviour. It holds two compound tags, the save data (written to the chunk) and the
/// optional client data (the update tag that is sent to the players watching the chunk).
///
/// Saved as the usual block entity compound, `id` (the full key of the type), `x`, `y`, `z`, plus
/// [`SAVE_DATA_KEY`] and, if there is any, [`CLIENT_DATA_KEY`].
pub struct PluginBlockEntity {
    position: BlockPos,
    entity_type: &'static PluginBlockEntityType,
    data: Mutex<NbtCompound>,
    client_data: Mutex<Option<NbtCompound>>,
    /// Whether the plugin was told that the entity is loaded, see `World::announce_plugin_block_entities`.
    announced: AtomicBool,
    /// The slots a plugin registered for this entity, see [`Self::register_inventory`].
    inventory: Mutex<Option<Arc<PluginInventory>>>,
    /// The fluid tanks a plugin registered, see [`Self::register_tanks`].
    tanks: Mutex<Option<Arc<PluginTanks>>>,
}

impl PluginBlockEntity {
    #[must_use]
    pub const fn new(
        position: BlockPos,
        entity_type: &'static PluginBlockEntityType,
        data: NbtCompound,
        client_data: Option<NbtCompound>,
    ) -> Self {
        Self {
            position,
            entity_type,
            data: Mutex::new(data),
            client_data: Mutex::new(client_data),
            announced: AtomicBool::new(false),
            inventory: Mutex::new(None),
            tanks: Mutex::new(None),
        }
    }

    /// Reads the saved form of an entity. `None` if its type is not registered.
    #[must_use]
    pub fn try_from_nbt(nbt: &NbtCompound, position: BlockPos) -> Option<Self> {
        let entity_type = plugin_block_entity_type_by_key(nbt.get_string("id")?)?;
        let entity = Self::new(
            position,
            entity_type,
            nbt.get_compound(SAVE_DATA_KEY).cloned().unwrap_or_default(),
            nbt.get_compound(CLIENT_DATA_KEY).cloned(),
        );
        if let Some(inventory) = nbt.get_compound(INVENTORY_KEY).and_then(PluginInventory::from_nbt)
        {
            *entity
                .inventory
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(inventory));
        }
        if let Some(tanks) = nbt.get_compound(TANKS_KEY).and_then(PluginTanks::from_nbt) {
            *entity.tanks.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(tanks));
        }
        Some(entity)
    }

    /// Reads a saved block entity for the loader of the world. Entities of vanilla or unknown ids
    /// give `None` quietly, one of a plugin type that is not registered (the plugin is gone) gives
    /// `None` with a warning. Its data stays in the chunk.
    #[must_use]
    pub fn load(nbt: &NbtCompound, position: BlockPos) -> Option<Arc<dyn BlockEntity>> {
        let id = nbt.get_string("id")?;
        if !id.contains(':') || id.starts_with("minecraft:") {
            return None;
        }
        if let Some(entity) = Self::try_from_nbt(nbt, position) {
            Some(Arc::new(entity))
        } else {
            warn!(
                "Skipping block entity '{id}' at ({}, {}, {}): its type is not registered",
                position.0.x, position.0.y, position.0.z
            );
            None
        }
    }

    #[must_use]
    pub const fn entity_type(&self) -> &'static PluginBlockEntityType {
        self.entity_type
    }

    /// A copy of the save data.
    #[must_use]
    pub fn data(&self) -> NbtCompound {
        self.data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A copy of the client data.
    #[must_use]
    pub fn client_data(&self) -> Option<NbtCompound> {
        self.client_data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Replaces both tags.
    pub fn set(&self, data: NbtCompound, client_data: Option<NbtCompound>) {
        *self.data.lock().unwrap_or_else(PoisonError::into_inner) = data;
        *self
            .client_data
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = client_data;
    }

    /// The inventory a plugin registered, if any.
    #[must_use]
    pub fn plugin_inventory(&self) -> Option<Arc<PluginInventory>> {
        self.inventory
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Gives the entity an inventory of `slot_count` slots that take `slot_limit` items at most,
    /// or resizes the one it has (see [`PluginInventory::resize`]). It is saved with the entity.
    pub fn register_inventory(
        &self,
        slot_count: usize,
        slot_limit: u8,
    ) -> Result<Arc<PluginInventory>, String> {
        if slot_count > MAX_SLOTS {
            return Err(format!("an inventory has at most {MAX_SLOTS} slots"));
        }
        let mut guard = self
            .inventory
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = guard.as_ref() {
            existing.resize(slot_count, slot_limit)?;
            return Ok(existing.clone());
        }
        let inventory = Arc::new(PluginInventory::new(slot_count, slot_limit));
        *guard = Some(inventory.clone());
        Ok(inventory)
    }

    /// Takes the inventory away and returns the stacks that were in it.
    pub fn unregister_inventory(&self) -> Vec<pumpkin_data::item_stack::ItemStack> {
        self.inventory
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .map(|inventory| inventory.take_all())
            .unwrap_or_default()
    }

    /// The tanks a plugin registered, if any.
    #[must_use]
    pub fn plugin_tanks(&self) -> Option<Arc<PluginTanks>> {
        self.tanks.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Gives the entity `tank_count` fluid tanks of `capacity` millibuckets each, or resizes the
    /// ones it has (see [`PluginTanks::resize`]). They are saved with the entity.
    pub fn register_tanks(
        &self,
        tank_count: usize,
        capacity: u32,
    ) -> Result<Arc<PluginTanks>, String> {
        if tank_count > MAX_TANKS {
            return Err(format!("a block entity has at most {MAX_TANKS} tanks"));
        }
        let mut guard = self.tanks.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = guard.as_ref() {
            existing.resize(tank_count, capacity)?;
            return Ok(existing.clone());
        }
        let tanks = Arc::new(PluginTanks::new(tank_count, capacity));
        *guard = Some(tanks.clone());
        Ok(tanks)
    }

    /// Takes the tanks away and returns the fluids that were in them.
    pub fn unregister_tanks(&self) -> Vec<FluidContent> {
        self.tanks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .map(|tanks| tanks.take_all())
            .unwrap_or_default()
    }

    /// Marks the entity as announced to the plugin and returns whether it was before.
    pub fn mark_announced(&self) -> bool {
        self.announced.swap(true, Ordering::AcqRel)
    }
}

impl BlockEntity for PluginBlockEntity {
    fn write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_compound(SAVE_DATA_KEY, self.data());
        if let Some(inventory) = self.plugin_inventory() {
            nbt.put_compound(INVENTORY_KEY, inventory.write_nbt());
        }
        if let Some(tanks) = self.plugin_tanks() {
            nbt.put_compound(TANKS_KEY, tanks.write_nbt());
        }
        if let Some(client_data) = self.client_data() {
            nbt.put_compound(CLIENT_DATA_KEY, client_data);
        }
    }

    fn from_nbt(nbt: &NbtCompound, position: BlockPos) -> Self
    where
        Self: Sized,
    {
        Self::try_from_nbt(nbt, position)
            .unwrap_or_else(|| Self::new(position, &UNREGISTERED, NbtCompound::new(), None))
    }

    /// Writes the slots back to the chunk when hoppers or item handlers changed them.
    fn tick(&self, world: &Arc<crate::world::World>) {
        let inventory_dirty = self
            .plugin_inventory()
            .is_some_and(|inventory| inventory.take_dirty());
        let tanks_dirty = self.plugin_tanks().is_some_and(|tanks| tanks.take_dirty());
        if inventory_dirty || tanks_dirty {
            let mut nbt = NbtCompound::new();
            self.write_internal(&mut nbt);
            world.add_block_entity_nbt(self.position, &nbt);
        }
    }

    fn resource_location(&self) -> &'static str {
        self.entity_type.key
    }

    fn get_position(&self) -> BlockPos {
        self.position
    }

    /// The id of the type in the registry clients synchronise.
    fn get_id(&self) -> u32 {
        self.entity_type.id
    }

    /// The slots of the plugin, if it registered some. Hoppers go through this.
    fn get_inventory(self: Arc<Self>) -> Option<Arc<dyn Inventory>> {
        self.plugin_inventory()
            .map(|inventory| inventory as Arc<dyn Inventory>)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
