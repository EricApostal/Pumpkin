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
}
