//! Entity data serializers registered by plugins on top of the vanilla ones (the
//! `neoforge:entity_data_serializers` registry).
//!
//! A serializer registered as the n-th gets the id `vanilla count + n`, the id an entry of the
//! synced data of a plugin entity names. The registry is append only and closes before players can
//! connect, see [`crate::plugin_registry`].

use std::sync::{LazyLock, PoisonError, RwLock};

use crate::plugin_registry::{PluginRegistry, RegistryError};

/// Serializer ids are a `VarInt`, this is far more than a plugin needs.
const MAX_PLUGIN_ENTITY_DATA_SERIALIZERS: usize = 1024;

/// The vanilla serializers in id order, the names the registry sync lists them with.
pub const VANILLA_ENTITY_DATA_SERIALIZERS: [&str; 44] = [
    "byte",
    "int",
    "long",
    "float",
    "string",
    "component",
    "optional_component",
    "item_stack",
    "boolean",
    "rotations",
    "block_pos",
    "optional_block_pos",
    "direction",
    "optional_living_entity_reference",
    "block_state",
    "optional_block_state",
    "particle",
    "particles",
    "villager_data",
    "optional_unsigned_int",
    "pose",
    "cat_variant",
    "cat_sound_variant",
    "cow_variant",
    "cow_sound_variant",
    "wolf_variant",
    "wolf_sound_variant",
    "frog_variant",
    "pig_variant",
    "pig_sound_variant",
    "chicken_variant",
    "chicken_sound_variant",
    "zombie_nautilus_variant",
    "optional_global_pos",
    "painting_variant",
    "sniffer_state",
    "armadillo_state",
    "copper_golem_state",
    "weathering_copper_state",
    "vector3",
    "quaternion",
    "resolvable_profile",
    "humanoid_arm",
    "dye_color",
];

/// An entity data serializer that a plugin registered.
pub struct PluginEntityDataSerializer {
    /// The full resource location, `namespace:path`.
    pub key: &'static str,
    /// The id entries of the synced data use: the vanilla count plus the registration index.
    pub id: u32,
}

static REGISTRY: LazyLock<RwLock<PluginRegistry<PluginEntityDataSerializer>>> =
    LazyLock::new(|| RwLock::new(PluginRegistry::new(MAX_PLUGIN_ENTITY_DATA_SERIALIZERS)));

fn read_registry() -> std::sync::RwLockReadGuard<'static, PluginRegistry<PluginEntityDataSerializer>>
{
    REGISTRY.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_registry()
-> std::sync::RwLockWriteGuard<'static, PluginRegistry<PluginEntityDataSerializer>> {
    REGISTRY.write().unwrap_or_else(PoisonError::into_inner)
}

/// Number of vanilla serializers. Plugin serializers get the ids from here on.
#[must_use]
pub const fn vanilla_entity_data_serializer_count() -> u32 {
    VANILLA_ENTITY_DATA_SERIALIZERS.len() as u32
}

/// Registers a serializer. Registering the same key again returns the existing one.
pub fn register_plugin_entity_data_serializer(
    key: &str,
) -> Result<&'static PluginEntityDataSerializer, RegistryError> {
    let vanilla_count = vanilla_entity_data_serializer_count();
    write_registry().register(
        key,
        |_| true,
        move |index, key| PluginEntityDataSerializer {
            key,
            id: vanilla_count + index as u32,
        },
    )
}

/// Closes the registry. Later registrations fail with [`RegistryError::Frozen`].
pub fn freeze_plugin_entity_data_serializers() {
    write_registry().freeze();
}

#[must_use]
pub fn is_plugin_entity_data_serializer_registry_frozen() -> bool {
    read_registry().is_frozen()
}

/// The registered serializers in id order.
#[must_use]
pub fn plugin_entity_data_serializers() -> Vec<&'static PluginEntityDataSerializer> {
    read_registry().entries().to_vec()
}

#[must_use]
pub fn plugin_entity_data_serializer_by_key(
    key: &str,
) -> Option<&'static PluginEntityDataSerializer> {
    read_registry().get_by_key(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_after_the_vanilla_serializers() {
        let first = register_plugin_entity_data_serializer("test_serializers:contraption").unwrap();
        assert_eq!(first.id, vanilla_entity_data_serializer_count());
        let again = register_plugin_entity_data_serializer("test_serializers:contraption").unwrap();
        assert!(std::ptr::eq(first, again));
        assert!(register_plugin_entity_data_serializer("minecraft:byte").is_err());
    }
}
