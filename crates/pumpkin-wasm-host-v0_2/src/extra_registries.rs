use crate::item_stack::from_wit_item_operation;
use crate::pumpkin::plugin::extra_registries::{
    Host, MapDecorationDefinition, MobEffectCategory as WitCategory, MobEffectDefinition,
    ParticleDefinition, SoundDefinition,
};
use pumpkin_core::plugin::permissions;
use pumpkin_data::extra_registry::{
    self, MapDecorationRegistration, plugin_map_decoration_by_key, plugin_particle_by_key,
    plugin_sound_by_key, vanilla_map_decoration_count, vanilla_particle_count, vanilla_sound_count,
};
use pumpkin_data::map_decoration::MapDecorationType;
use pumpkin_data::mob_effect_registry::{
    EffectModifier, MobEffectCategoryKind, MobEffectRegistration, mob_effect_from_key,
    register_plugin_mob_effect,
};
use pumpkin_data::particle::Particle;
use pumpkin_data::sound::Sound;
use pumpkin_protocol::VarInt;
use pumpkin_protocol::java::client::play::CParticle;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_wasm_host_common::state::PluginHostState;
use tracing::info;

fn strip(key: &str) -> &str {
    key.strip_prefix("minecraft:").unwrap_or(key)
}

fn check_permission(state: &PluginHostState, what: &str) -> Result<(), String> {
    if state
        .permissions
        .iter()
        .any(|p| p == permissions::REGISTRY_EXTRA)
    {
        Ok(())
    } else {
        Err(format!(
            "registering {what} needs the '{}' permission",
            permissions::REGISTRY_EXTRA
        ))
    }
}

fn plugin_name(state: &PluginHostState) -> &str {
    state.name.as_deref().unwrap_or("<unknown>")
}

/// Sends a plugin particle to the `NeoForge` players of `world`, the others do not know the id.
pub fn spawn_plugin_particle(
    world: &pumpkin_core::world::World,
    key: &str,
    pos: Vector3<f64>,
    delta: Vector3<f32>,
    speed: f32,
    count: i32,
    options: &[u8],
) -> Result<(), String> {
    let particle = plugin_particle_by_key(key)
        .ok_or_else(|| format!("'{key}' is not a registered particle type"))?;
    let valid = match particle.options_format {
        "none" => options.is_empty(),
        "color" => options.len() == 4,
        "block-state" => (1..=5).contains(&options.len()),
        _ => true,
    };
    if !valid {
        return Err(format!(
            "the options do not fit the '{}' format of '{key}'",
            particle.options_format
        ));
    }
    let id = i32::try_from(vanilla_particle_count() + particle.index)
        .map_err(|_| "the particle id is too large".to_string())?;
    let packet = CParticle::new(
        particle.override_limiter,
        false,
        pos,
        delta,
        speed,
        count,
        VarInt(id),
        options,
    );
    for player in world.players.load().iter() {
        let neoforge = player.client.java().is_some_and(|client| {
            client.flavour.load() == pumpkin_protocol::ConnectionFlavour::NeoForge
        });
        if neoforge {
            player.try_send_client_packet(&packet);
        }
    }
    Ok(())
}

impl Host for PluginHostState {
    fn register_sound(
        &mut self,
        definition: SoundDefinition,
    ) -> wasmtime::Result<Result<u32, String>> {
        if let Err(error) = check_permission(self, "sounds") {
            return Ok(Err(error));
        }
        Ok(
            match extra_registry::register_plugin_sound(&definition.key, definition.fixed_range) {
                Ok(sound) => {
                    info!(
                        "Plugin {} registered sound {}",
                        plugin_name(self),
                        sound.key
                    );
                    Ok((vanilla_sound_count() + sound.index) as u32)
                }
                Err(error) => Err(format!("sound '{}': {error}", definition.key)),
            },
        )
    }

    fn get_sound_id(&mut self, key: String) -> wasmtime::Result<Option<u32>> {
        if let Some(sound) = plugin_sound_by_key(&key) {
            return Ok(Some((vanilla_sound_count() + sound.index) as u32));
        }
        Ok(Sound::from_name(strip(&key)).map(|sound| sound as u32))
    }

    fn register_particle(
        &mut self,
        definition: ParticleDefinition,
    ) -> wasmtime::Result<Result<u32, String>> {
        if let Err(error) = check_permission(self, "particles") {
            return Ok(Err(error));
        }
        Ok(
            match extra_registry::register_plugin_particle(
                &definition.key,
                definition.override_limiter,
                &definition.options_format,
            ) {
                Ok(particle) => {
                    info!(
                        "Plugin {} registered particle {}",
                        plugin_name(self),
                        particle.key
                    );
                    Ok((vanilla_particle_count() + particle.index) as u32)
                }
                Err(error) => Err(format!("particle '{}': {error}", definition.key)),
            },
        )
    }

    fn get_particle_id(&mut self, key: String) -> wasmtime::Result<Option<u32>> {
        if let Some(particle) = plugin_particle_by_key(&key) {
            return Ok(Some((vanilla_particle_count() + particle.index) as u32));
        }
        Ok(Particle::from_name(strip(&key)).map(|particle| u32::from(particle.to_id())))
    }

    fn register_mob_effect(
        &mut self,
        definition: MobEffectDefinition,
    ) -> wasmtime::Result<Result<u32, String>> {
        if let Err(error) = check_permission(self, "mob effects") {
            return Ok(Err(error));
        }
        let registration = MobEffectRegistration {
            category: match definition.category {
                WitCategory::Beneficial => MobEffectCategoryKind::Beneficial,
                WitCategory::Harmful => MobEffectCategoryKind::Harmful,
                WitCategory::Neutral => MobEffectCategoryKind::Neutral,
            },
            color: definition.color as i32,
            modifiers: definition
                .modifiers
                .into_iter()
                .map(|modifier| EffectModifier {
                    attribute: modifier.attribute,
                    id: modifier.id,
                    amount: modifier.amount,
                    operation: from_wit_item_operation(modifier.operation),
                })
                .collect(),
        };
        Ok(
            match register_plugin_mob_effect(&definition.key, registration) {
                Ok(effect) => {
                    info!(
                        "Plugin {} registered mob effect {} with id {}",
                        plugin_name(self),
                        effect.key,
                        effect.effect.id
                    );
                    Ok(u32::from(effect.effect.id))
                }
                Err(error) => Err(format!("mob effect '{}': {error}", definition.key)),
            },
        )
    }

    fn get_mob_effect_id(&mut self, key: String) -> wasmtime::Result<Option<u32>> {
        Ok(mob_effect_from_key(&key).map(|effect| u32::from(effect.id)))
    }

    fn register_map_decoration_type(
        &mut self,
        definition: MapDecorationDefinition,
    ) -> wasmtime::Result<Result<u32, String>> {
        if let Err(error) = check_permission(self, "map decoration types") {
            return Ok(Err(error));
        }
        let registration = MapDecorationRegistration {
            asset_name: definition.asset_name,
            show_on_item_frame: definition.show_on_item_frame,
            map_color: definition.map_color,
            exploration_map_element: definition.exploration_map_element,
            track_count: definition.track_count,
        };
        Ok(
            match extra_registry::register_plugin_map_decoration(&definition.key, registration) {
                Ok(decoration) => Ok((vanilla_map_decoration_count() + decoration.index) as u32),
                Err(error) => Err(format!("map decoration '{}': {error}", definition.key)),
            },
        )
    }

    fn get_map_decoration_id(&mut self, key: String) -> wasmtime::Result<Option<u32>> {
        if let Some(decoration) = plugin_map_decoration_by_key(&key) {
            return Ok(Some(
                (vanilla_map_decoration_count() + decoration.index) as u32,
            ));
        }
        Ok(MapDecorationType::from_name(strip(&key)).map(|decoration| decoration.id))
    }

    fn register_datapack_registry(
        &mut self,
        key: String,
        kind: String,
    ) -> wasmtime::Result<Result<(), String>> {
        if let Err(error) = check_permission(self, "datapack registries") {
            return Ok(Err(error));
        }
        Ok(extra_registry::register_datapack_registry(&key, &kind)
            .map_err(|error| format!("datapack registry '{key}': {error}")))
    }

    fn add_datapack_entry(
        &mut self,
        registry: String,
        id: String,
        nbt: Option<Vec<u8>>,
    ) -> wasmtime::Result<Result<(), String>> {
        if let Err(error) = check_permission(self, "datapack entries") {
            return Ok(Err(error));
        }
        Ok(extra_registry::add_datapack_entry(&registry, &id, nbt)
            .map_err(|error| format!("datapack entry '{id}' of '{registry}': {error}")))
    }

    fn register_synced_registry(&mut self, key: String) -> wasmtime::Result<Result<(), String>> {
        if let Err(error) = check_permission(self, "registries") {
            return Ok(Err(error));
        }
        Ok(extra_registry::register_owned_registry(&key)
            .map_err(|error| format!("registry '{key}': {error}")))
    }

    fn add_synced_registry_entry(
        &mut self,
        registry: String,
        entry: String,
    ) -> wasmtime::Result<Result<u32, String>> {
        if let Err(error) = check_permission(self, "registry entries") {
            return Ok(Err(error));
        }
        Ok(extra_registry::add_owned_registry_entry(&registry, &entry)
            .map(|id| id as u32)
            .map_err(|error| format!("entry '{entry}' of '{registry}': {error}")))
    }

    fn get_synced_registry_entry_id(
        &mut self,
        registry: String,
        entry: String,
    ) -> wasmtime::Result<Option<u32>> {
        Ok(extra_registry::owned_registry_entry_id(&registry, &entry).map(|id| id as u32))
    }
}
