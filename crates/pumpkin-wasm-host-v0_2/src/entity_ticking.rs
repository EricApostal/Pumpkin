use crate::pumpkin::plugin::entity_types::EntityType as WitEntityType;
use crate::{entity::from_wit_entity_type, pumpkin::plugin::entity_ticking::Host};
use pumpkin_core::plugin::api::events::entity::set_entity_tick_interval;
use pumpkin_wasm_host_common::state::PluginHostState;

impl Host for PluginHostState {
    fn set_entity_ticking(
        &mut self,
        entity_type: WitEntityType,
        interval: u32,
    ) -> wasmtime::Result<()> {
        let entity_type = from_wit_entity_type(entity_type)?;
        set_entity_tick_interval(entity_type.id, interval);
        Ok(())
    }

    fn set_plugin_entity_ticking(&mut self, key: String, interval: u32) -> wasmtime::Result<bool> {
        let Some(entity_type) = pumpkin_data::entity_registry::plugin_entity_type_by_key(&key)
        else {
            return Ok(false);
        };
        Ok(set_entity_tick_interval(
            entity_type.entity_type.id,
            interval,
        ))
    }
}
