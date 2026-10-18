//! The input of riders of plugin vehicles.

use pumpkin_core::plugin::api::events::entity::PluginVehicleInputEvent;
use pumpkin_wasm_host_common::state::PluginHostState;

use crate::{
    events::{ToFromWasmEvent, cleanup_event, consume_player},
    pumpkin::plugin::event::{Event, PluginVehicleInputEventData},
};

impl ToFromWasmEvent for PluginVehicleInputEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        Event::PluginVehicleInputEvent(PluginVehicleInputEventData {
            vehicle_id: self.vehicle_id,
            player,
            forward: self.forward,
            sideways: self.sideways,
            jump: self.jump,
            sneak: self.sneak,
            sprint: self.sprint,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::PluginVehicleInputEvent(data) => Self {
                vehicle_id: data.vehicle_id,
                player: consume_player(state, &data.player),
                forward: data.forward,
                sideways: data.sideways,
                jump: data.jump,
                sneak: data.sneak,
                sprint: data.sprint,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}
