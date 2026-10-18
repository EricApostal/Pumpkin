//! The event of plugin features placed during chunk generation.

use std::sync::Arc;

use pumpkin_core::plugin::world::PluginFeaturePlaceEvent;
use pumpkin_core::plugin::{Context, EventPriority};
use pumpkin_wasm_host_common::state::{ChunkBuffer, PluginHostState};

use crate::{
    events::{ToFromWasmEvent, WasmPluginEventHandler, cleanup_event, to_wasm_block_position},
    pumpkin::plugin::event::{Event, EventType, PluginFeaturePlaceEventData},
    worldgen::step_from_index,
};

/// Registers the events of this file.
pub fn register_worldgen_event(
    ctx: &Context,
    handler: &Arc<WasmPluginEventHandler>,
    priority: EventPriority,
    blocking: bool,
    event_type: EventType,
) {
    match event_type {
        EventType::PluginFeaturePlaceEvent => {
            ctx.register_event::<PluginFeaturePlaceEvent, _>(
                Arc::clone(handler),
                priority,
                blocking,
            );
        }
        _ => {
            tracing::error!("unexpected event routed to register_worldgen_event");
        }
    }
}

impl ToFromWasmEvent for PluginFeaturePlaceEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");
        let (min_y, height) = {
            let chunk = self
                .chunk
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (i32::from(chunk.bottom_y()), u32::from(chunk.height()))
        };
        let chunk = state
            .add(ChunkBuffer {
                x: self.chunk_pos.x,
                z: self.chunk_pos.y,
                min_y,
                height,
                proto_chunk: self.chunk.clone(),
            })
            .expect("failed to add chunk buffer resource");

        Event::PluginFeaturePlaceEvent(PluginFeaturePlaceEventData {
            target_world,
            chunk_x: self.chunk_pos.x,
            chunk_z: self.chunk_pos.y,
            feature_key: self.feature_key.clone(),
            seed: self.seed,
            step: step_from_index(self.step),
            positions: self
                .positions
                .iter()
                .copied()
                .map(to_wasm_block_position)
                .collect(),
            chunk,
        })
    }

    /// The event cannot be rebuilt from guest data: it owns the chunk under generation.
    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("plugin feature place events are informational");
    }

    /// Informational only, the handler writes through the chunk buffer.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}
