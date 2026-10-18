use crate::pumpkin::plugin::damage_types::{
    DamageEffects as WitEffects, DamageScaling as WitScaling, DamageTypeDefinition,
    DamageTypeEntry, DeathMessageType as WitDeathMessage, Host,
};
use pumpkin_core::plugin::permissions;
use pumpkin_data::damage::{DamageEffects, DamageScaling, DeathMessageType};
use pumpkin_data::damage_type_registry::{
    DamageTypeRegistration, VANILLA_DAMAGE_TYPE_COUNT, damage_type_from_key, plugin_damage_types,
    register_plugin_damage_type,
};
use pumpkin_wasm_host_common::state::PluginHostState;
use tracing::info;

impl Host for PluginHostState {
    fn register_damage_type(
        &mut self,
        definition: DamageTypeDefinition,
    ) -> wasmtime::Result<Result<u32, String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::REGISTRY_DAMAGE_TYPES)
        {
            return Ok(Err(format!(
                "registering damage types needs the '{}' permission",
                permissions::REGISTRY_DAMAGE_TYPES
            )));
        }
        let key = definition.key;
        let registration = DamageTypeRegistration {
            message_id: definition.message_id,
            scaling: match definition.scaling {
                WitScaling::Never => DamageScaling::Never,
                WitScaling::WhenCausedByLivingNonPlayer => {
                    DamageScaling::WhenCausedByLivingNonPlayer
                }
                WitScaling::Always => DamageScaling::Always,
            },
            exhaustion: definition.exhaustion,
            effects: definition.effects.map(|effects| match effects {
                WitEffects::Hurt => DamageEffects::Hurt,
                WitEffects::Thorns => DamageEffects::Thorns,
                WitEffects::Drowning => DamageEffects::Drowning,
                WitEffects::Burning => DamageEffects::Burning,
                WitEffects::Poking => DamageEffects::Poking,
                WitEffects::Freezing => DamageEffects::Freezing,
            }),
            death_message_type: match definition.death_message_type {
                WitDeathMessage::Default => DeathMessageType::Default,
                WitDeathMessage::FallVariants => DeathMessageType::FallVariants,
                WitDeathMessage::IntentionalGameDesign => DeathMessageType::IntentionalGameDesign,
            },
        };
        Ok(match register_plugin_damage_type(&key, registration) {
            Ok(damage_type) => {
                info!(
                    "Plugin {} registered damage type {} with id {}",
                    self.name.as_deref().unwrap_or("<unknown>"),
                    damage_type.key,
                    damage_type.damage_type.id
                );
                Ok(u32::from(damage_type.damage_type.id))
            }
            Err(error) => Err(format!("damage type '{key}': {error}")),
        })
    }

    fn get_damage_type_id(&mut self, key: String) -> wasmtime::Result<Option<u32>> {
        Ok(damage_type_from_key(&key).map(|damage_type| u32::from(damage_type.id)))
    }

    fn get_vanilla_damage_type_count(&mut self) -> wasmtime::Result<u32> {
        Ok(u32::from(VANILLA_DAMAGE_TYPE_COUNT))
    }

    fn get_registered_damage_types(&mut self) -> wasmtime::Result<Vec<DamageTypeEntry>> {
        Ok(plugin_damage_types()
            .into_iter()
            .map(|plugin| DamageTypeEntry {
                key: plugin.key.to_string(),
                id: u32::from(plugin.damage_type.id),
            })
            .collect())
    }
}
