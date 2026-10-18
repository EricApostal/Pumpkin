//! Damage types registered by plugins on top of the vanilla ones (the `minecraft:damage_type`
//! registry).
//!
//! A type registered as the n-th gets the id `vanilla count + n`, which is also its index in the
//! registry data packet the server sends (see [`crate::plugin_registry`]). The registry is append
//! only and closes before players can connect. Damage type ids are a `u8`, datapack damage types
//! share the id space and follow the plugin types.

use std::sync::{LazyLock, PoisonError, RwLock};

use crate::damage::{DamageEffects, DamageScaling, DamageType, DeathMessageType};
use crate::plugin_registry::{PluginRegistry, RegistryError};

/// The keys (without namespace) of the vanilla damage types, in id order.
pub const VANILLA_DAMAGE_TYPE_KEYS: [&str; 51] = [
    "arrow",
    "bad_respawn_point",
    "cactus",
    "campfire",
    "cramming",
    "dragon_breath",
    "drown",
    "dry_out",
    "ender_pearl",
    "explosion",
    "fall",
    "falling_anvil",
    "falling_block",
    "falling_stalactite",
    "fireball",
    "fireworks",
    "fly_into_wall",
    "freeze",
    "generic",
    "generic_kill",
    "hot_floor",
    "in_fire",
    "in_wall",
    "indirect_magic",
    "lava",
    "lightning_bolt",
    "mace_smash",
    "magic",
    "mob_attack",
    "mob_attack_no_aggro",
    "mob_projectile",
    "on_fire",
    "out_of_world",
    "outside_border",
    "player_attack",
    "player_explosion",
    "sonic_boom",
    "spear",
    "spit",
    "stalagmite",
    "starve",
    "sting",
    "sulfur_cube_hot",
    "sweet_berry_bush",
    "thorns",
    "thrown",
    "trident",
    "unattributed_fireball",
    "wind_charge",
    "wither",
    "wither_skull",
];

/// Number of vanilla damage types. Plugin types get the ids from here on.
pub const VANILLA_DAMAGE_TYPE_COUNT: u8 = VANILLA_DAMAGE_TYPE_KEYS.len() as u8;

/// Plugin types are at most this many, the rest of the `u8` ids is left for datapack types.
const MAX_PLUGIN_DAMAGE_TYPES: usize = 128;

/// A damage type that a plugin registered.
pub struct PluginDamageType {
    /// The full resource location, `namespace:path`.
    pub key: &'static str,
    /// The damage type, its `id` is the vanilla count plus the registration index.
    pub damage_type: DamageType,
}

/// What a plugin gives to define a damage type.
pub struct DamageTypeRegistration {
    pub message_id: String,
    pub scaling: DamageScaling,
    pub exhaustion: f32,
    pub effects: Option<DamageEffects>,
    pub death_message_type: DeathMessageType,
}

static REGISTRY: LazyLock<RwLock<PluginRegistry<PluginDamageType>>> =
    LazyLock::new(|| RwLock::new(PluginRegistry::new(MAX_PLUGIN_DAMAGE_TYPES)));

fn read_registry() -> std::sync::RwLockReadGuard<'static, PluginRegistry<PluginDamageType>> {
    REGISTRY.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_registry() -> std::sync::RwLockWriteGuard<'static, PluginRegistry<PluginDamageType>> {
    REGISTRY.write().unwrap_or_else(PoisonError::into_inner)
}

/// Registers a damage type. Registering the same key with the same definition again returns the
/// existing type.
pub fn register_plugin_damage_type(
    key: &str,
    registration: DamageTypeRegistration,
) -> Result<&'static PluginDamageType, RegistryError> {
    if registration.message_id.is_empty() {
        return Err(RegistryError::InvalidDefinition(
            "the message id is empty".to_string(),
        ));
    }
    if !registration.exhaustion.is_finite() || registration.exhaustion < 0.0 {
        return Err(RegistryError::InvalidDefinition(
            "the exhaustion must be a number of 0 or more".to_string(),
        ));
    }
    let DamageTypeRegistration {
        message_id,
        scaling,
        exhaustion,
        effects,
        death_message_type,
    } = registration;
    write_registry().register(
        key,
        |existing| {
            let existing = &existing.damage_type;
            existing.message_id == message_id.as_str()
                && existing.scaling == scaling
                && existing.exhaustion == exhaustion
                && existing.effects == effects
                && existing.death_message_type == death_message_type
        },
        |index, key| PluginDamageType {
            key,
            damage_type: DamageType {
                death_message_type,
                exhaustion,
                effects,
                message_id: Box::leak(message_id.clone().into_boxed_str()),
                scaling,
                id: VANILLA_DAMAGE_TYPE_COUNT + index as u8,
            },
        },
    )
}

/// Closes the registry. Later registrations fail with [`RegistryError::Frozen`].
pub fn freeze_plugin_damage_types() {
    write_registry().freeze();
}

#[must_use]
pub fn is_plugin_damage_type_registry_frozen() -> bool {
    read_registry().is_frozen()
}

/// The registered types in id order.
#[must_use]
pub fn plugin_damage_types() -> Vec<&'static PluginDamageType> {
    read_registry().entries().to_vec()
}

/// The number of registered types, the datapack types start after them.
#[must_use]
pub fn plugin_damage_type_count() -> usize {
    read_registry().entries().len()
}

#[must_use]
pub fn plugin_damage_type_by_key(key: &str) -> Option<&'static PluginDamageType> {
    read_registry().get_by_key(key)
}

#[must_use]
pub fn plugin_damage_type_by_id(id: u8) -> Option<&'static PluginDamageType> {
    let index = id.checked_sub(VANILLA_DAMAGE_TYPE_COUNT)?;
    read_registry().get_by_index(usize::from(index))
}

/// Resolves a vanilla (`minecraft:` prefix optional) or plugin damage type by key.
#[must_use]
pub fn damage_type_from_key(key: &str) -> Option<DamageType> {
    if let Some(plugin) = plugin_damage_type_by_key(key) {
        return Some(plugin.damage_type);
    }
    DamageType::from_name(key.strip_prefix("minecraft:").unwrap_or(key))
}

/// The key of a vanilla or plugin damage type. Datapack types have none known here.
#[must_use]
pub fn damage_type_key(damage_type: &DamageType) -> Option<String> {
    if let Some(name) = VANILLA_DAMAGE_TYPE_KEYS.get(usize::from(damage_type.id)) {
        return Some(format!("minecraft:{name}"));
    }
    plugin_damage_type_by_id(damage_type.id).map(|plugin| plugin.key.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_keys_match_the_generated_ids() {
        for (index, key) in VANILLA_DAMAGE_TYPE_KEYS.iter().enumerate() {
            let damage_type = DamageType::from_name(key).expect("known key");
            assert_eq!(usize::from(damage_type.id), index, "{key}");
        }
        assert!(DamageType::from_id(VANILLA_DAMAGE_TYPE_COUNT).is_none());
    }
}
