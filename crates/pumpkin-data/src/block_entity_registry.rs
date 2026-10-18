//! Block entity types registered by plugins on top of the vanilla ones.
//!
//! A type registered as the n-th gets the network id `vanilla count + n`, the id a modded client's
//! registry sync announces for its block entity type. The registry is append only and closes before
//! players can connect, see [`crate::plugin_registry`].

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, PoisonError, RwLock};

use crate::block_properties::BLOCK_ENTITY_TYPES;
use crate::plugin_registry::{PluginRegistry, RegistryError, split_plugin_key};

/// Block entity type ids are a `VarInt`, this is far more than a plugin needs.
const MAX_PLUGIN_BLOCK_ENTITY_TYPES: usize = 4096;
/// A block entity type is valid for a handful of blocks, this bounds what a plugin can send.
const MAX_VALID_BLOCKS: usize = 4096;

/// A block entity type that a plugin registered.
pub struct PluginBlockEntityType {
    /// The full resource location, `namespace:path`. Also the `id` saved in the chunk.
    pub key: &'static str,
    /// The network id: the vanilla count plus the registration index.
    pub id: u32,
    /// The keys of the blocks this type belongs to.
    pub valid_blocks: &'static [&'static str],
    /// The tick interval in ticks a plugin asked for, 0 when it does not want the type ticked.
    /// Use [`PluginBlockEntityType::tick_interval`] and [`set_plugin_block_entity_tick_interval`].
    tick_interval: AtomicU32,
}

impl PluginBlockEntityType {
    /// A type that is not ticked.
    #[must_use]
    pub const fn new(key: &'static str, id: u32, valid_blocks: &'static [&'static str]) -> Self {
        Self {
            key,
            id,
            valid_blocks,
            tick_interval: AtomicU32::new(0),
        }
    }

    /// How often the type is ticked, in ticks. 0 when it is not.
    #[must_use]
    pub fn tick_interval(&self) -> u32 {
        self.tick_interval.load(Ordering::Relaxed)
    }
}

/// How many types have a tick interval, so a world can tell with one load that nothing ticks.
static TICKING_TYPES: AtomicU32 = AtomicU32::new(0);

/// Sets how often the entities of a type are ticked, 0 stops it. Returns the previous interval.
pub fn set_plugin_block_entity_tick_interval(
    entity_type: &'static PluginBlockEntityType,
    interval: u32,
) -> u32 {
    let previous = entity_type.tick_interval.swap(interval, Ordering::AcqRel);
    match (previous == 0, interval == 0) {
        (true, false) => {
            TICKING_TYPES.fetch_add(1, Ordering::AcqRel);
        }
        (false, true) => {
            TICKING_TYPES.fetch_sub(1, Ordering::AcqRel);
        }
        _ => {}
    }
    previous
}

/// Whether any plugin block entity type is ticked. Cheap, meant for the tick loop.
#[must_use]
pub fn any_plugin_block_entity_ticking() -> bool {
    TICKING_TYPES.load(Ordering::Acquire) != 0
}

static REGISTRY: LazyLock<RwLock<PluginRegistry<PluginBlockEntityType>>> =
    LazyLock::new(|| RwLock::new(PluginRegistry::new(MAX_PLUGIN_BLOCK_ENTITY_TYPES)));

fn read_registry() -> std::sync::RwLockReadGuard<'static, PluginRegistry<PluginBlockEntityType>> {
    REGISTRY.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_registry() -> std::sync::RwLockWriteGuard<'static, PluginRegistry<PluginBlockEntityType>> {
    REGISTRY.write().unwrap_or_else(PoisonError::into_inner)
}

/// Number of vanilla block entity types. Plugin types get the ids from here on.
#[must_use]
pub const fn vanilla_block_entity_type_count() -> u32 {
    BLOCK_ENTITY_TYPES.len() as u32
}

/// Registers a block entity type. Registering the same definition again returns the existing type,
/// a different one for a known key is [`RegistryError::Duplicate`].
pub fn register_plugin_block_entity_type(
    key: &str,
    valid_blocks: Vec<String>,
) -> Result<&'static PluginBlockEntityType, RegistryError> {
    if valid_blocks.len() > MAX_VALID_BLOCKS {
        return Err(RegistryError::InvalidDefinition(format!(
            "a block entity type has at most {MAX_VALID_BLOCKS} valid blocks"
        )));
    }
    for block in &valid_blocks {
        // Vanilla blocks may be written with or without the namespace, plugin blocks need it.
        let path = block.strip_prefix("minecraft:").unwrap_or(block);
        if path.is_empty() || (path.contains(':') && split_plugin_key(path).is_err()) {
            return Err(RegistryError::InvalidDefinition(format!(
                "'{block}' is not a block key"
            )));
        }
    }
    let same = |existing: &'static PluginBlockEntityType| {
        existing.valid_blocks.len() == valid_blocks.len()
            && existing
                .valid_blocks
                .iter()
                .zip(&valid_blocks)
                .all(|(known, new)| *known == new.as_str())
    };
    let vanilla_count = vanilla_block_entity_type_count();
    let leaked_blocks: Vec<&'static str> = valid_blocks
        .iter()
        .map(|block| &*Box::leak(block.clone().into_boxed_str()))
        .collect();
    write_registry().register(key, same, move |index, key| {
        PluginBlockEntityType::new(
            key,
            vanilla_count + index as u32,
            Box::leak(leaked_blocks.into_boxed_slice()),
        )
    })
}

/// Closes the registry. Later registrations fail with [`RegistryError::Frozen`].
pub fn freeze_plugin_block_entity_types() {
    write_registry().freeze();
}

#[must_use]
pub fn is_plugin_block_entity_registry_frozen() -> bool {
    read_registry().is_frozen()
}

/// The registered types in id order.
#[must_use]
pub fn plugin_block_entity_types() -> Vec<&'static PluginBlockEntityType> {
    read_registry().entries().to_vec()
}

#[must_use]
pub fn plugin_block_entity_type_by_key(key: &str) -> Option<&'static PluginBlockEntityType> {
    read_registry().get_by_key(key)
}

#[must_use]
pub fn plugin_block_entity_type_by_id(id: u32) -> Option<&'static PluginBlockEntityType> {
    let index = id.checked_sub(vanilla_block_entity_type_count())?;
    read_registry().get_by_index(index as usize)
}
