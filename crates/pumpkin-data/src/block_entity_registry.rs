//! Block entity types registered by plugins on top of the vanilla ones.
//!
//! A type registered as the n-th gets the network id `vanilla count + n`, the id a modded client's
//! registry sync announces for its block entity type. The registry is append only and closes before
//! players can connect, see [`crate::plugin_registry`].

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
    write_registry().register(key, same, move |index, key| PluginBlockEntityType {
        key,
        id: vanilla_count + index as u32,
        valid_blocks: Box::leak(leaked_blocks.into_boxed_slice()),
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
