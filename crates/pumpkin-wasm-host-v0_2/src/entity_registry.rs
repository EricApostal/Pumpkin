use crate::pumpkin::plugin::entity_registry::{
    EntityCategory as WitEntityCategory, EntityTypeDefinition as WitEntityTypeDefinition, Host,
};
use pumpkin_core::entity::plugin_entity::{MAX_SPAWN_DATA, MAX_SYNCED_DATA, PluginEntity};
use pumpkin_core::entity::{EntityBase, r#type::from_type};
use pumpkin_core::plugin::permissions;
use pumpkin_core::world::World;
use pumpkin_data::entity_registry::{
    self, PluginEntityCategory, PluginEntityDefinition, plugin_entity_type_by_key,
};
use pumpkin_util::math::vector3::Vector3;
use pumpkin_wasm_host_common::state::PluginHostState;
use std::sync::Arc;
use tracing::info;

const fn to_category(category: WitEntityCategory) -> PluginEntityCategory {
    match category {
        WitEntityCategory::Misc => PluginEntityCategory::Misc,
        WitEntityCategory::Creature => PluginEntityCategory::Creature,
        WitEntityCategory::Monster => PluginEntityCategory::Monster,
        WitEntityCategory::Ambient => PluginEntityCategory::Ambient,
        WitEntityCategory::Axolotls => PluginEntityCategory::Axolotls,
        WitEntityCategory::UndergroundWaterCreature => {
            PluginEntityCategory::UndergroundWaterCreature
        }
        WitEntityCategory::WaterCreature => PluginEntityCategory::WaterCreature,
        WitEntityCategory::WaterAmbient => PluginEntityCategory::WaterAmbient,
    }
}

fn to_definition(definition: WitEntityTypeDefinition) -> PluginEntityDefinition {
    PluginEntityDefinition {
        category: to_category(definition.category),
        width: definition.width,
        height: definition.height,
        eye_height: definition.eye_height,
        fire_immune: definition.fire_immune,
        tracking_range: definition.tracking_range,
        update_interval: definition.update_interval,
        track_deltas: definition.track_deltas,
        summonable: definition.summonable,
        saveable: definition.saveable,
        pickable: definition.pickable,
        collidable: definition.collidable,
        pushable: definition.pushable,
        ignore_riders_hit: definition.ignore_riders_hit,
        default_synced_data: definition.default_synced_data,
        spawn_data_channel: definition.spawn_data_channel,
    }
}

impl Host for PluginHostState {
    fn register_entity_type(
        &mut self,
        definition: WitEntityTypeDefinition,
    ) -> wasmtime::Result<Result<u32, String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::REGISTRY_ENTITIES)
        {
            return Ok(Err(format!(
                "registering entity types needs the '{}' permission",
                permissions::REGISTRY_ENTITIES
            )));
        }
        let key = definition.key.clone();
        Ok(
            match entity_registry::register_plugin_entity_type(&key, to_definition(definition)) {
                Ok(entity_type) => {
                    info!(
                        "Plugin {} registered entity type {} with id {}",
                        self.name.as_deref().unwrap_or("<unknown>"),
                        entity_type.key,
                        entity_type.id
                    );
                    Ok(entity_type.id)
                }
                Err(error) => Err(error.to_string()),
            },
        )
    }

    fn register_entity_data_serializer(
        &mut self,
        key: String,
    ) -> wasmtime::Result<Result<u32, String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::REGISTRY_ENTITIES)
        {
            return Ok(Err(format!(
                "registering entity data serializers needs the '{}' permission",
                permissions::REGISTRY_ENTITIES
            )));
        }
        Ok(
            pumpkin_data::entity_data_serializer_registry::register_plugin_entity_data_serializer(
                &key,
            )
            .map(|serializer| serializer.id)
            .map_err(|error| error.to_string()),
        )
    }

    fn register_entity_type_tag(
        &mut self,
        tag: String,
        entries: Vec<String>,
    ) -> wasmtime::Result<Result<(), String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::REGISTRY_ENTITIES)
        {
            return Ok(Err(format!(
                "registering entity type tags needs the '{}' permission",
                permissions::REGISTRY_ENTITIES
            )));
        }
        if entity_registry::is_plugin_entity_registry_frozen() {
            return Ok(Err(
                "entity type tags can only be registered before players connect".to_string(),
            ));
        }
        pumpkin_data::dynamic_tag::register_tag(
            pumpkin_data::tag::RegistryKey::EntityType,
            &tag,
            entries,
        );
        Ok(Ok(()))
    }

    fn get_entity_type_id(&mut self, key: String) -> wasmtime::Result<Option<u32>> {
        Ok(entity_registry::entity_type_by_key(&key).map(|entity_type| u32::from(entity_type.id)))
    }

    fn get_vanilla_entity_type_count(&mut self) -> wasmtime::Result<u32> {
        Ok(entity_registry::vanilla_entity_type_count())
    }
}

/// The plugin entity behind an entity, or an error for any other entity.
pub(crate) fn plugin_entity_of(entity: &dyn EntityBase) -> Result<&PluginEntity, String> {
    entity
        .get_plugin_entity()
        .ok_or_else(|| "entity is not a plugin entity".to_string())
}

pub(crate) fn set_synced_data(entity: &dyn EntityBase, data: Vec<u8>) -> Result<(), String> {
    let plugin_entity = plugin_entity_of(entity)?;
    if data.len() > MAX_SYNCED_DATA {
        return Err(format!("synced data is at most {MAX_SYNCED_DATA} bytes"));
    }
    plugin_entity.set_synced_data(data);
    Ok(())
}

pub(crate) fn set_synced_entry(
    entity: &dyn EntityBase,
    index: u8,
    data: Vec<u8>,
) -> Result<(), String> {
    let plugin_entity = plugin_entity_of(entity)?;
    if index == 255 {
        return Err("the entry index is at most 254".to_string());
    }
    if data.len() > MAX_SYNCED_DATA {
        return Err(format!("a synced entry is at most {MAX_SYNCED_DATA} bytes"));
    }
    plugin_entity.set_synced_entry(index, data);
    Ok(())
}

pub(crate) fn set_spawn_data(entity: &dyn EntityBase, data: Vec<u8>) -> Result<(), String> {
    let plugin_entity = plugin_entity_of(entity)?;
    if data.len() > MAX_SPAWN_DATA {
        return Err(format!("spawn data is at most {MAX_SPAWN_DATA} bytes"));
    }
    plugin_entity.set_spawn_data(data);
    Ok(())
}

pub(crate) fn set_size(entity: &dyn EntityBase, width: f32, height: f32) -> Result<(), String> {
    let plugin_entity = plugin_entity_of(entity)?;
    let size_ok = |size: f32| size.is_finite() && (0.0..=64.0).contains(&size);
    if !size_ok(width) || !size_ok(height) {
        return Err("width and height have to be between 0 and 64 blocks".to_string());
    }
    plugin_entity.set_size(width, height);
    Ok(())
}

/// Spawns an entity of a plugin type.
pub(crate) fn spawn_plugin_entity(
    world: &Arc<World>,
    type_key: &str,
    pos: (f64, f64, f64),
    synced_data: Option<Vec<u8>>,
    spawn_data: Option<Vec<u8>>,
) -> Result<Arc<dyn EntityBase>, String> {
    let plugin_type = plugin_entity_type_by_key(type_key)
        .ok_or_else(|| format!("unknown plugin entity type '{type_key}'"))?;
    if synced_data
        .as_ref()
        .is_some_and(|d| d.len() > MAX_SYNCED_DATA)
    {
        return Err(format!("synced data is at most {MAX_SYNCED_DATA} bytes"));
    }
    if spawn_data
        .as_ref()
        .is_some_and(|d| d.len() > MAX_SPAWN_DATA)
    {
        return Err(format!("spawn data is at most {MAX_SPAWN_DATA} bytes"));
    }
    let entity = from_type(
        plugin_type.entity_type,
        Vector3::new(pos.0, pos.1, pos.2),
        world,
        uuid::Uuid::new_v4(),
    );
    if let Some(plugin_entity) = entity.get_plugin_entity() {
        if let Some(data) = synced_data {
            plugin_entity.set_initial_synced_data(data);
        }
        if let Some(data) = spawn_data {
            plugin_entity.set_spawn_data(data);
        }
    }
    if world.spawn_entity(entity.clone()) {
        Ok(entity)
    } else {
        Err("the spawn was cancelled".to_string())
    }
}
