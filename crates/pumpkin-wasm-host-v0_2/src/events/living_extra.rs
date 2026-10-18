//! Events of living entities, ticks and chunk sections.

use std::sync::Arc;

use pumpkin_data::item_stack::ItemStack;
use pumpkin_wasm_host_common::state::PluginHostState;
use tokio::sync::Mutex;
use wasmtime::component::Resource;

use pumpkin_core::plugin::{
    Context, EventPriority,
    api::events::entity::{
        EntityEnteringSectionEvent, EntitySizeEvent, EntityStartTrackingEvent,
        EntityStopTrackingEvent, EntityTickEvent, LivingBreatheEvent,
        LivingDropsEvent, LivingEquipmentChangeEvent, LivingVisibilityEvent, PlayerTickEvent,
    },
};

use crate::{
    events::{
        ToFromWasmEvent, WasmPluginEventHandler, cleanup_event, consume_player,
        from_wasm_block_position, to_wasm_block_position, to_wasm_position,
    },
    pumpkin::plugin::{
        event::{
            EntityEnteringSectionEventData, EntitySizeEventData, EntityTickEventData,
            EntityTrackingEventData, Event,
            EventType, LivingBreatheEventData, LivingDropsEventData,
            LivingEquipmentChangeEventData, LivingVisibilityEventData, PlayerTickEventData,
        },
        item_stack::ItemStack as WitItemStack,
        uuid::Uuid as WitUuid,
    },
    uuid::UuidExt,
};

/// Registers one of the events of this file.
pub fn register_living_extra_event(
    ctx: &Context,
    handler: &Arc<WasmPluginEventHandler>,
    priority: EventPriority,
    blocking: bool,
    event_type: EventType,
) {
    let handler = Arc::clone(handler);
    match event_type {
        EventType::LivingDropsEvent => {
            ctx.register_event::<LivingDropsEvent, _>(handler, priority, blocking);
        }
        EventType::LivingEquipmentChangeEvent => {
            ctx.register_event::<LivingEquipmentChangeEvent, _>(handler, priority, blocking);
        }
        EventType::LivingBreatheEvent => {
            ctx.register_event::<LivingBreatheEvent, _>(handler, priority, blocking);
        }
        EventType::LivingVisibilityEvent => {
            ctx.register_event::<LivingVisibilityEvent, _>(handler, priority, blocking);
        }
        EventType::EntitySizeEvent => {
            ctx.register_event::<EntitySizeEvent, _>(handler, priority, blocking);
        }
        EventType::PlayerTickEvent => {
            ctx.register_event::<PlayerTickEvent, _>(handler, priority, blocking);
        }
        EventType::EntityTickEvent => {
            ctx.register_event::<EntityTickEvent, _>(handler, priority, blocking);
        }
        EventType::EntityEnteringSectionEvent => {
            ctx.register_event::<EntityEnteringSectionEvent, _>(handler, priority, blocking);
        }
        EventType::EntityStartTrackingEvent => {
            ctx.register_event::<EntityStartTrackingEvent, _>(handler, priority, blocking);
        }
        EventType::EntityStopTrackingEvent => {
            ctx.register_event::<EntityStopTrackingEvent, _>(handler, priority, blocking);
        }
        _ => {
            tracing::error!("unexpected event routed to register_living_extra_event");
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

impl ToFromWasmEvent for LivingDropsEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let drops = self
            .drops
            .iter()
            .map(|stack| add_item_stack(state, stack))
            .collect();
        Event::LivingDropsEvent(LivingDropsEventData {
            entity_id: self.entity_id,
            killer_id: self.killer_id,
            position: to_wasm_position(self.position),
            drops,
            cancelled: self.cancelled,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct LivingDropsEvent from WASM")
    }

    /// The handler may have changed the list and the stacks.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        if let Event::LivingDropsEvent(data) = &event {
            self.cancelled = data.cancelled;
            self.drops = data
                .drops
                .iter()
                .map(|item| read_item_stack(state, item))
                .filter(|stack| !stack.is_empty())
                .collect();
        }
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for LivingEquipmentChangeEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        Event::LivingEquipmentChangeEvent(LivingEquipmentChangeEventData {
            entity_id: self.entity_id,
            slot: self.slot.clone(),
            old_stack: add_item_stack(state, &self.old_stack),
            new_stack: add_item_stack(state, &self.new_stack),
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct LivingEquipmentChangeEvent from WASM")
    }

    /// Informational.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for LivingBreatheEvent {
    fn to_wasm_event(&self, _state: &mut PluginHostState) -> Event {
        Event::LivingBreatheEvent(LivingBreatheEventData {
            entity_id: self.entity_id,
            submerged: self.submerged,
            air: self.air,
            air_change: self.air_change,
            cancelled: self.cancelled,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct LivingBreatheEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::LivingBreatheEvent(data) = event {
            self.air_change = data.air_change;
            self.cancelled = data.cancelled;
        }
    }
}

impl ToFromWasmEvent for LivingVisibilityEvent {
    fn to_wasm_event(&self, _state: &mut PluginHostState) -> Event {
        Event::LivingVisibilityEvent(LivingVisibilityEventData {
            entity_id: self.entity_id,
            looking_entity_id: self.looking_entity_id,
            multiplier: self.multiplier,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct LivingVisibilityEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::LivingVisibilityEvent(data) = event {
            self.multiplier = data.multiplier;
        }
    }
}

impl ToFromWasmEvent for EntitySizeEvent {
    fn to_wasm_event(&self, _state: &mut PluginHostState) -> Event {
        Event::EntitySizeEvent(EntitySizeEventData {
            entity_id: self.entity_id,
            pose: self.pose.clone(),
            width: self.width,
            height: self.height,
            eye_height: self.eye_height,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct EntitySizeEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::EntitySizeEvent(data) = event {
            // A box that is not positive would break the collision code.
            if data.width > 0.0 && data.height > 0.0 {
                self.width = data.width;
                self.height = data.height;
                self.eye_height = data.eye_height;
            }
        }
    }
}

impl ToFromWasmEvent for PlayerTickEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        Event::PlayerTickEvent(PlayerTickEventData { player })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::PlayerTickEvent(data) => Self {
                player: consume_player(state, &data.player),
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for EntityTickEvent {
    fn to_wasm_event(&self, _state: &mut PluginHostState) -> Event {
        Event::EntityTickEvent(EntityTickEventData {
            entity_id: self.entity_id,
            entity_type: self.entity_type.clone(),
            tick: self.tick,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct EntityTickEvent from WASM")
    }

    /// Informational.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for EntityEnteringSectionEvent {
    fn to_wasm_event(&self, _state: &mut PluginHostState) -> Event {
        Event::EntityEnteringSectionEvent(EntityEnteringSectionEventData {
            entity_id: self.entity_id,
            old_section: to_wasm_block_position(self.old_section),
            new_section: to_wasm_block_position(self.new_section),
        })
    }

    fn from_wasm_event(event: Event, _state: &mut PluginHostState) -> Self {
        match event {
            Event::EntityEnteringSectionEvent(data) => Self {
                entity_id: data.entity_id,
                old_section: from_wasm_block_position(data.old_section),
                new_section: from_wasm_block_position(data.new_section),
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for EntityStartTrackingEvent {
    fn to_wasm_event(&self, _state: &mut PluginHostState) -> Event {
        Event::EntityStartTrackingEvent(EntityTrackingEventData {
            entity_id: self.entity_id,
            player: WitUuid::to_wit(&self.player),
        })
    }

    fn from_wasm_event(event: Event, _state: &mut PluginHostState) -> Self {
        match event {
            Event::EntityStartTrackingEvent(data) => Self {
                entity_id: data.entity_id,
                player: WitUuid::from_wit(&data.player),
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for EntityStopTrackingEvent {
    fn to_wasm_event(&self, _state: &mut PluginHostState) -> Event {
        Event::EntityStopTrackingEvent(EntityTrackingEventData {
            entity_id: self.entity_id,
            player: WitUuid::to_wit(&self.player),
        })
    }

    fn from_wasm_event(event: Event, _state: &mut PluginHostState) -> Self {
        match event {
            Event::EntityStopTrackingEvent(data) => Self {
                entity_id: data.entity_id,
                player: WitUuid::from_wit(&data.player),
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}
