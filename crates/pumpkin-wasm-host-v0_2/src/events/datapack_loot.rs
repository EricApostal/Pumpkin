//! Events of datapack reloads, datapack syncs and loot modification.

use std::sync::Arc;

use pumpkin_data::item_stack::ItemStack;
use pumpkin_wasm_host_common::state::PluginHostState;
use tokio::sync::Mutex;
use wasmtime::component::Resource;

use pumpkin_core::plugin::{
    Context, EventPriority,
    api::events::world::{
        DatapackReloadEvent, DatapackSyncEvent, LootModifyEvent, LootSource, TagsUpdatedEvent,
    },
};

use crate::{
    events::{
        ToFromWasmEvent, WasmPluginEventHandler, cleanup_event, consume_player,
        to_wasm_block_position, to_wasm_position,
    },
    pumpkin::plugin::{
        event::{
            DatapackReloadEventData, DatapackSyncEventData, Event, EventType,
            LootModifyEventData, TagsUpdatedEventData,
        },
        item_stack::ItemStack as WitItemStack,
        uuid::Uuid as WitUuid,
    },
    uuid::UuidExt,
};

/// Registers one of the events of this file.
pub fn register_datapack_loot_event(
    ctx: &Context,
    handler: &Arc<WasmPluginEventHandler>,
    priority: EventPriority,
    blocking: bool,
    event_type: EventType,
) {
    let handler = Arc::clone(handler);
    match event_type {
        EventType::DatapackReloadEvent => {
            ctx.register_event::<DatapackReloadEvent, _>(handler, priority, blocking);
        }
        EventType::TagsUpdatedEvent => {
            ctx.register_event::<TagsUpdatedEvent, _>(handler, priority, blocking);
        }
        EventType::DatapackSyncEvent => {
            ctx.register_event::<DatapackSyncEvent, _>(handler, priority, blocking);
        }
        EventType::LootModifyEvent => {
            ctx.register_event::<LootModifyEvent, _>(handler, priority, blocking);
        }
        _ => {
            tracing::error!("unexpected event routed to register_datapack_loot_event");
        }
    }
}

fn add_item_stack(state: &mut PluginHostState, stack: &ItemStack) -> Resource<WitItemStack> {
    state
        .add(Arc::new(Mutex::new(stack.clone())))
        .expect("failed to add item stack resource")
}

/// Copies the stack out of its resource, the resource stays until `cleanup_event`.
fn read_item_stack(state: &PluginHostState, item: &Resource<WitItemStack>) -> ItemStack {
    state
        .get(item)
        .ok()
        .and_then(|stack| stack.try_lock().ok().map(|stack| stack.clone()))
        .unwrap_or_else(|| ItemStack::EMPTY.clone())
}

impl ToFromWasmEvent for DatapackReloadEvent {
    fn to_wasm_event(&self, _state: &mut PluginHostState) -> Event {
        Event::DatapackReloadEvent(DatapackReloadEventData {
            recipe_count: self.recipe_count,
            tag_count: self.tag_count,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct DatapackReloadEvent from WASM")
    }

    /// Informational.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for TagsUpdatedEvent {
    fn to_wasm_event(&self, _state: &mut PluginHostState) -> Event {
        Event::TagsUpdatedEvent(TagsUpdatedEventData {
            tag_count: self.tag_count,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct TagsUpdatedEvent from WASM")
    }

    /// Informational.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for DatapackSyncEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        Event::DatapackSyncEvent(DatapackSyncEventData {
            player,
            reload: self.reload,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::DatapackSyncEvent(data) => Self {
                player: consume_player(state, &data.player),
                reload: data.reload,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for LootModifyEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let tool = self
            .tool
            .as_ref()
            .map(|stack| add_item_stack(state, stack));
        let loot = self
            .loot
            .iter()
            .map(|stack| add_item_stack(state, stack))
            .collect();
        Event::LootModifyEvent(LootModifyEventData {
            source: match self.source {
                LootSource::Block => "block",
                LootSource::Entity => "entity",
            }
            .to_string(),
            loot_table: self.loot_table.clone(),
            damage_source: self.damage_source.clone(),
            killer_uuid: self.killer_uuid.as_ref().map(WitUuid::to_wit),
            killer_id: self.killer_id,
            tool,
            looting_level: self.looting_level,
            block_pos: self.block_pos.map(to_wasm_block_position),
            block_state: self.block_state.map(u32::from),
            entity_id: self.entity_id,
            entity_type: self.entity_type.clone(),
            position: to_wasm_position(self.position),
            loot,
            cancelled: self.cancelled,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct LootModifyEvent from WASM")
    }

    /// The handler may have changed the list and the stacks.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        if let Event::LootModifyEvent(data) = &event {
            self.cancelled = data.cancelled;
            self.loot = data
                .loot
                .iter()
                .map(|item| read_item_stack(state, item))
                .filter(|stack| !stack.is_empty())
                .collect();
        }
        cleanup_event(&event, state);
    }
}
