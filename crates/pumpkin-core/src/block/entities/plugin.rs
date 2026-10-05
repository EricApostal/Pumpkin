use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pumpkin_data::block_entity_registry::{PluginBlockEntityType, plugin_block_entity_type_by_key};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use tracing::warn;

use super::BlockEntity;

/// Key of the compound with the data of the plugin in the saved block entity.
pub const SAVE_DATA_KEY: &str = "PluginData";
/// Key of the compound with the data that is sent to clients in the saved block entity.
pub const CLIENT_DATA_KEY: &str = "PluginClientData";

/// Stand in for an entity whose type is gone, see [`PluginBlockEntity::from_nbt`].
static UNREGISTERED: PluginBlockEntityType = PluginBlockEntityType {
    key: "pumpkin:unregistered",
    id: u32::MAX,
    valid_blocks: &[],
};

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
        }
    }

    /// Reads the saved form of an entity. `None` if its type is not registered.
    #[must_use]
    pub fn try_from_nbt(nbt: &NbtCompound, position: BlockPos) -> Option<Self> {
        let entity_type = plugin_block_entity_type_by_key(nbt.get_string("id")?)?;
        Some(Self::new(
            position,
            entity_type,
            nbt.get_compound(SAVE_DATA_KEY).cloned().unwrap_or_default(),
            nbt.get_compound(CLIENT_DATA_KEY).cloned(),
        ))
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

    /// Marks the entity as announced to the plugin and returns whether it was before.
    pub fn mark_announced(&self) -> bool {
        self.announced.swap(true, Ordering::AcqRel)
    }
}

impl BlockEntity for PluginBlockEntity {
    fn write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_compound(SAVE_DATA_KEY, self.data());
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

    fn as_any(&self) -> &dyn Any {
        self
    }
}
