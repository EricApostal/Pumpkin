//! Enchantments registered by plugins on top of the vanilla ones (the `minecraft:enchantment`
//! registry), and enchantment effect component types.
//!
//! An enchantment registered as the n-th gets the id `vanilla count + n`, which is the position
//! the registry data puts it at. The generated [`Enchantment`] is what the data components store,
//! so every registered enchantment has one (leaked, like the entry). Its `name` is the full key.
//! Only the fields the server logic reads are filled, the client facing definition (description,
//! costs, effects) lives in the core enchantment manager. The registries are append only and close
//! before players can connect, see [`crate::plugin_registry`].

use std::sync::{LazyLock, PoisonError, RwLock};

use crate::enchantment::{AttributeModifierSlot, Cost, Enchantment, EnchantmentEffects};
use crate::plugin_registry::{PluginRegistry, RegistryError};
use crate::tag::Tag;

/// Enchantment ids are a `u8` in the data components.
const MAX_PLUGIN_ENCHANTMENTS: usize = 128;
const MAX_PLUGIN_EFFECT_TYPES: usize = 256;

/// The vanilla `minecraft:enchantment_effect_component_type` entries in registry order
/// (`EnchantmentEffectComponents` of 26.3).
///
/// The list is the client's id space, so it has to end where the client's does: the last entry,
/// `trident_spin_attack_strength`, was missing here, which left every probe of the registry one
/// short of the client (a plugin's effect type was numbered `30 + index` by the host and `31 +
/// index` by the client).
pub const VANILLA_ENCHANTMENT_EFFECT_TYPES: [&str; 31] = [
    "minecraft:damage_protection",
    "minecraft:damage_immunity",
    "minecraft:damage",
    "minecraft:smash_damage_per_fallen_block",
    "minecraft:knockback",
    "minecraft:armor_effectiveness",
    "minecraft:post_attack",
    "minecraft:post_piercing_attack",
    "minecraft:hit_block",
    "minecraft:item_damage",
    "minecraft:equipment_drops",
    "minecraft:location_changed",
    "minecraft:tick",
    "minecraft:ammo_use",
    "minecraft:projectile_piercing",
    "minecraft:projectile_spawned",
    "minecraft:projectile_spread",
    "minecraft:projectile_count",
    "minecraft:trident_return_acceleration",
    "minecraft:fishing_time_reduction",
    "minecraft:fishing_luck_bonus",
    "minecraft:block_experience",
    "minecraft:mob_experience",
    "minecraft:repair_with_xp",
    "minecraft:attributes",
    "minecraft:crossbow_charge_time",
    "minecraft:crossbow_charging_sounds",
    "minecraft:trident_sound",
    "minecraft:prevent_equipment_drop",
    "minecraft:prevent_armor_change",
    "minecraft:trident_spin_attack_strength",
];

/// What the server needs to know about a plugin enchantment.
#[derive(Clone, Debug, PartialEq)]
pub struct EnchantmentRegistration {
    pub max_level: i32,
    pub anvil_cost: u32,
    pub weight: i32,
    pub slots: Vec<AttributeModifierSlot>,
}

/// An enchantment that a plugin registered.
pub struct PluginEnchantment {
    /// The full resource location, `namespace:path`.
    pub key: &'static str,
    pub index: usize,
    pub registration: EnchantmentRegistration,
    /// The enchantment the server works with, its `id` is the vanilla count plus the index.
    pub enchantment: &'static Enchantment,
}

/// An enchantment effect component type that a plugin registered.
pub struct PluginEnchantmentEffectType {
    pub key: &'static str,
    pub index: usize,
}

static ENCHANTMENTS: LazyLock<RwLock<PluginRegistry<PluginEnchantment>>> =
    LazyLock::new(|| RwLock::new(PluginRegistry::new(MAX_PLUGIN_ENCHANTMENTS)));
static EFFECT_TYPES: LazyLock<RwLock<PluginRegistry<PluginEnchantmentEffectType>>> =
    LazyLock::new(|| RwLock::new(PluginRegistry::new(MAX_PLUGIN_EFFECT_TYPES)));

/// Number of vanilla enchantments, their ids are `0..count`.
#[must_use]
pub const fn vanilla_enchantment_count() -> usize {
    Enchantment::ALL.len()
}

/// Number of vanilla enchantment effect component types.
#[must_use]
pub const fn vanilla_enchantment_effect_type_count() -> usize {
    VANILLA_ENCHANTMENT_EFFECT_TYPES.len()
}

const EMPTY_TAG: Tag = (&[], &[], "");

const NO_EFFECTS: EnchantmentEffects = EnchantmentEffects {
    projectile_spawned: &[],
    post_attack: &[],
    projectile_count: &[],
    projectile_spread: &[],
    projectile_piercing: &[],
    ammo_use: &[],
    damage: &[],
    knockback: &[],
    armor_effectiveness: &[],
    damage_protection: &[],
    hit_block: &[],
    item_damage: &[],
    equipment_drops: &[],
    fishing_time_reduction: &[],
    fishing_luck_bonus: &[],
    block_experience: &[],
    mob_experience: &[],
    repair_with_xp: &[],
    smash_damage_per_fallen_block: &[],
    trident_return_acceleration: &[],
    trident_spin_attack_strength: None,
    crossbow_charge_time: None,
    location_changed: &[],
    prevent_armor_change: false,
    prevent_equipment_drop: false,
};

/// Registers an enchantment. The same definition again returns the existing one.
pub fn register_plugin_enchantment(
    key: &str,
    registration: EnchantmentRegistration,
) -> Result<&'static PluginEnchantment, RegistryError> {
    let vanilla = vanilla_enchantment_count();
    let reg = registration.clone();
    ENCHANTMENTS
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .register(
            key,
            |existing| existing.registration == registration,
            move |index, key| {
                let (namespace, path) = key.split_once(':').unwrap_or(("", key));
                let enchantment: &'static Enchantment = Box::leak(Box::new(Enchantment {
                    id: (vanilla + index) as u8,
                    name: key,
                    registry_key: Box::leak(path.to_string().into_boxed_str()),
                    description: Box::leak(
                        format!("enchantment.{namespace}.{path}")
                            .replace('/', ".")
                            .into_boxed_str(),
                    ),
                    anvil_cost: reg.anvil_cost,
                    supported_items: &EMPTY_TAG,
                    primary_items: None,
                    exclusive_set: None,
                    max_level: reg.max_level,
                    slots: Box::leak(reg.slots.clone().into_boxed_slice()),
                    weight: reg.weight,
                    min_cost: Cost {
                        base: 1,
                        per_level_above_first: 10,
                    },
                    max_cost: Cost {
                        base: 21,
                        per_level_above_first: 10,
                    },
                    effects: NO_EFFECTS,
                }));
                PluginEnchantment {
                    key,
                    index,
                    registration: reg,
                    enchantment,
                }
            },
        )
}

/// Closes the enchantment registry. Later registrations fail with [`RegistryError::Frozen`].
pub fn freeze_plugin_enchantments() {
    ENCHANTMENTS
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .freeze();
    EFFECT_TYPES
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .freeze();
}

/// The registered enchantments in id order.
#[must_use]
pub fn plugin_enchantments() -> Vec<&'static PluginEnchantment> {
    ENCHANTMENTS
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .entries()
        .to_vec()
}

#[must_use]
pub fn plugin_enchantment_by_key(key: &str) -> Option<&'static PluginEnchantment> {
    ENCHANTMENTS
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get_by_key(key)
}

/// Resolves a vanilla (`minecraft:` prefix optional) or plugin enchantment by key.
#[must_use]
pub fn enchantment_from_key(key: &str) -> Option<&'static Enchantment> {
    if let Some(plugin) = plugin_enchantment_by_key(key) {
        return Some(plugin.enchantment);
    }
    Enchantment::from_name(key).or_else(|| {
        key.strip_prefix("minecraft:")
            .and_then(Enchantment::from_name)
    })
}

/// Resolves a vanilla or plugin enchantment by registry id.
#[must_use]
pub fn enchantment_from_id(id: u8) -> Option<&'static Enchantment> {
    if let Some(vanilla) = Enchantment::from_id(id) {
        return Some(vanilla);
    }
    let index = usize::from(id).checked_sub(vanilla_enchantment_count())?;
    ENCHANTMENTS
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get_by_index(index)
        .map(|plugin| plugin.enchantment)
}

/// Registers an enchantment effect component type and returns its id (`vanilla count + index`).
/// The same key again returns the same id.
pub fn register_plugin_enchantment_effect_type(key: &str) -> Result<usize, RegistryError> {
    let entry = EFFECT_TYPES
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .register(
            key,
            |_| true,
            |index, key| PluginEnchantmentEffectType { key, index },
        )?;
    Ok(vanilla_enchantment_effect_type_count() + entry.index)
}

/// The registered effect component types in id order.
#[must_use]
pub fn plugin_enchantment_effect_types() -> Vec<&'static PluginEnchantmentEffectType> {
    EFFECT_TYPES
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .entries()
        .to_vec()
}

/// The id of an effect component type, vanilla (`minecraft:` prefix optional) or plugin.
#[must_use]
pub fn enchantment_effect_type_id(key: &str) -> Option<usize> {
    let full = if key.contains(':') {
        key.to_string()
    } else {
        format!("minecraft:{key}")
    };
    if let Some(id) = VANILLA_ENCHANTMENT_EFFECT_TYPES
        .iter()
        .position(|k| *k == full)
    {
        return Some(id);
    }
    EFFECT_TYPES
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get_by_key(&full)
        .map(|entry| vanilla_enchantment_effect_type_count() + entry.index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_enchantments_follow_the_vanilla_ids() {
        let registration = EnchantmentRegistration {
            max_level: 3,
            anvil_cost: 1,
            weight: 10,
            slots: vec![AttributeModifierSlot::MainHand],
        };
        let entry =
            register_plugin_enchantment("data_ench_test:lifesteal", registration.clone()).unwrap();
        assert_eq!(
            usize::from(entry.enchantment.id),
            vanilla_enchantment_count() + entry.index
        );
        assert_eq!(
            enchantment_from_id(entry.enchantment.id).map(|e| e.name),
            Some("data_ench_test:lifesteal")
        );
        assert!(enchantment_from_key("data_ench_test:lifesteal").is_some());
        assert!(enchantment_from_key("sharpness").is_some());
        assert!(register_plugin_enchantment("data_ench_test:lifesteal", registration).is_ok());
        let id = register_plugin_enchantment_effect_type("data_ench_test:heal").unwrap();
        assert!(id >= VANILLA_ENCHANTMENT_EFFECT_TYPES.len());
        assert_eq!(enchantment_effect_type_id("ammo_use"), Some(13));
    }
}
