//! Mob effects registered by plugins on top of the vanilla ones (the `minecraft:mob_effect`
//! registry).
//!
//! A mob effect registered as the n-th gets the id `vanilla count + n`. The generated
//! [`StatusEffect`] is what the rest of the server works with, so every registered effect has one
//! (leaked, like the entry). Its `minecraft_name` is the full key. The registry is append only and
//! closes before players can connect, see [`crate::plugin_registry`].

use std::sync::{LazyLock, PoisonError, RwLock};

use crate::attributes::Attributes;
use crate::data_component_impl::{IDSetContent, Operation};
use crate::effect::{MobEffectCategory, Modifiers, StatusEffect};
use crate::plugin_registry::{PluginRegistry, RegistryError};

/// Effect ids are sent as a `VarInt` and stored as a `u8`.
const MAX_PLUGIN_EFFECTS: usize = 128;

/// An attribute modifier of a plugin effect, applied per amplifier level.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectModifier {
    /// The attribute, for example `minecraft:movement_speed`.
    pub attribute: String,
    /// The modifier id, `namespace:path`.
    pub id: String,
    pub amount: f64,
    pub operation: Operation,
}

/// What a plugin gives to define a mob effect.
#[derive(Clone, Debug, PartialEq)]
pub struct MobEffectRegistration {
    pub category: MobEffectCategoryKind,
    /// The particle colour, RGB.
    pub color: i32,
    pub modifiers: Vec<EffectModifier>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MobEffectCategoryKind {
    Beneficial,
    Harmful,
    Neutral,
}

impl MobEffectCategoryKind {
    fn category(self) -> MobEffectCategory {
        match self {
            Self::Beneficial => MobEffectCategory::Beneficial,
            Self::Harmful => MobEffectCategory::Harmful,
            Self::Neutral => MobEffectCategory::Neutral,
        }
    }

    const fn of(category: &MobEffectCategory) -> Self {
        match category {
            MobEffectCategory::Beneficial => Self::Beneficial,
            MobEffectCategory::Harmful => Self::Harmful,
            MobEffectCategory::Neutral => Self::Neutral,
        }
    }
}

/// A mob effect that a plugin registered.
pub struct PluginMobEffect {
    /// The full resource location, `namespace:path`.
    pub key: &'static str,
    pub registration: MobEffectRegistration,
    /// The effect the server works with, its `id` is the vanilla count plus the index.
    pub effect: &'static StatusEffect,
}

static REGISTRY: LazyLock<RwLock<PluginRegistry<PluginMobEffect>>> =
    LazyLock::new(|| RwLock::new(PluginRegistry::new(MAX_PLUGIN_EFFECTS)));

fn read_registry() -> std::sync::RwLockReadGuard<'static, PluginRegistry<PluginMobEffect>> {
    REGISTRY.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_registry() -> std::sync::RwLockWriteGuard<'static, PluginRegistry<PluginMobEffect>> {
    REGISTRY.write().unwrap_or_else(PoisonError::into_inner)
}

/// Number of vanilla mob effects, their ids are `0..count`.
#[must_use]
pub fn vanilla_mob_effect_count() -> usize {
    (0..=u16::from(u8::MAX))
        .take_while(|id| StatusEffect::from_id(*id).is_some())
        .count()
}

/// The vanilla mob effect keys (`minecraft:speed`) in id order.
#[must_use]
pub fn vanilla_mob_effect_keys() -> Vec<String> {
    (0..vanilla_mob_effect_count() as u16)
        .filter_map(StatusEffect::from_id)
        .map(|effect| effect.minecraft_name.to_string())
        .collect()
}

fn attribute_by_name(name: &str) -> Option<&'static Attributes> {
    let name = if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    };
    Attributes::ALL.iter().find(|attribute| attribute.name == name)
}

/// Registers a mob effect. The same definition again returns the existing effect.
pub fn register_plugin_mob_effect(
    key: &str,
    registration: MobEffectRegistration,
) -> Result<&'static PluginMobEffect, RegistryError> {
    let mut modifiers = Vec::with_capacity(registration.modifiers.len());
    for modifier in &registration.modifiers {
        let attribute = attribute_by_name(&modifier.attribute).ok_or_else(|| {
            RegistryError::InvalidDefinition(format!("unknown attribute '{}'", modifier.attribute))
        })?;
        crate::plugin_registry::split_plugin_key(&modifier.id)?;
        if !modifier.amount.is_finite() {
            return Err(RegistryError::InvalidDefinition(
                "a modifier amount is not a number".to_string(),
            ));
        }
        modifiers.push(Modifiers {
            attribute,
            id: Box::leak(modifier.id.clone().into_boxed_str()),
            base_value: modifier.amount,
            operation: modifier.operation,
        });
    }
    let vanilla = vanilla_mob_effect_count();
    let reg = registration.clone();
    write_registry().register(
        key,
        |existing| existing.registration == registration,
        move |index, key| {
            let (namespace, path) = key.split_once(':').unwrap_or(("", key));
            let effect: &'static StatusEffect = Box::leak(Box::new(StatusEffect {
                minecraft_name: key,
                id: (vanilla + index) as u8,
                category: reg.category.category(),
                color: reg.color,
                translation_key: Box::leak(
                    format!("effect.{namespace}.{path}").replace('/', ".").into_boxed_str(),
                ),
                attribute_modifiers: Box::leak(modifiers.into_boxed_slice()),
            }));
            PluginMobEffect {
                key,
                registration: reg,
                effect,
            }
        },
    )
}

/// Closes the registry. Later registrations fail with [`RegistryError::Frozen`].
pub fn freeze_plugin_mob_effects() {
    write_registry().freeze();
}

/// The registered effects in id order.
#[must_use]
pub fn plugin_mob_effects() -> Vec<&'static PluginMobEffect> {
    read_registry().entries().to_vec()
}

#[must_use]
pub fn plugin_mob_effect_by_key(key: &str) -> Option<&'static PluginMobEffect> {
    read_registry().get_by_key(key)
}

#[must_use]
pub fn plugin_mob_effect_by_id(id: u8) -> Option<&'static PluginMobEffect> {
    let index = usize::from(id).checked_sub(vanilla_mob_effect_count())?;
    read_registry().get_by_index(index)
}

/// Resolves a vanilla (`minecraft:` prefix optional) or plugin effect by key.
#[must_use]
pub fn mob_effect_from_key(key: &str) -> Option<&'static StatusEffect> {
    if let Some(plugin) = plugin_mob_effect_by_key(key) {
        return Some(plugin.effect);
    }
    StatusEffect::from_name(key.strip_prefix("minecraft:").unwrap_or(key))
}

/// The kind of an effect category.
#[must_use]
pub const fn category_kind(category: &MobEffectCategory) -> MobEffectCategoryKind {
    MobEffectCategoryKind::of(category)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_effects_are_dense() {
        let count = vanilla_mob_effect_count();
        assert!(count >= 39);
        let keys = vanilla_mob_effect_keys();
        assert_eq!(keys.len(), count);
        assert_eq!(keys[0], "minecraft:speed");
    }

    #[test]
    fn registers_an_effect_with_modifiers() {
        let registration = MobEffectRegistration {
            category: MobEffectCategoryKind::Beneficial,
            color: 0x00ff00,
            modifiers: vec![EffectModifier {
                attribute: "minecraft:movement_speed".to_string(),
                id: "mob_effect_test:speed".to_string(),
                amount: 0.2,
                operation: Operation::AddMultipliedTotal,
            }],
        };
        let effect = register_plugin_mob_effect("mob_effect_test:fast", registration.clone())
            .unwrap();
        assert!(usize::from(effect.effect.id) >= vanilla_mob_effect_count());
        assert_eq!(effect.effect.attribute_modifiers.len(), 1);
        assert_eq!(effect.effect.translation_key, "effect.mob_effect_test.fast");
        let again = register_plugin_mob_effect("mob_effect_test:fast", registration).unwrap();
        assert!(std::ptr::eq(effect, again));
        assert!(
            register_plugin_mob_effect(
                "mob_effect_test:bad",
                MobEffectRegistration {
                    category: MobEffectCategoryKind::Harmful,
                    color: 0,
                    modifiers: vec![EffectModifier {
                        attribute: "nope".to_string(),
                        id: "mob_effect_test:x".to_string(),
                        amount: 1.0,
                        operation: Operation::AddValue,
                    }],
                }
            )
            .is_err()
        );
    }
}
