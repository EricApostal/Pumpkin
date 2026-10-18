//! Events of the small hooks that replace gameplay mod mixins: fluid movement, item use ticks,
//! swings, first use, block attacks, item entity updates, fake player placement and fluid spread.

use std::sync::Arc;

use pumpkin_data::item_stack::ItemStack;
use pumpkin_wasm_host_common::state::PluginHostState;
use tokio::sync::Mutex;
use wasmtime::component::Resource;

use pumpkin_core::plugin::{
    Context, EventPriority,
    api::events::entity::{
        CanAttackBlockEvent, EntityFluidMovementEvent, EntityItemUpdateEvent,
        FakePlayerPlaceCheckEvent, FluidCanPassThroughEvent, ItemSwingEvent, ItemUseFirstEvent,
        ItemUseFirstResult, ItemUseTickEvent,
    },
};

use crate::{
    events::{
        ToFromWasmEvent, WasmPluginEventHandler, cleanup_event, from_wasm_position,
        to_wasm_block_position, to_wasm_hand, to_wasm_position,
    },
    mob::to_wit_block_direction,
    pumpkin::plugin::{
        event::{
            CanAttackBlockEventData, EntityFluidMovementEventData, EntityItemUpdateEventData,
            Event, EventType, FakePlayerPlaceCheckEventData, FluidCanPassThroughEventData,
            ItemSwingEventData, ItemUseFirstEventData, ItemUseFirstResult as WitUseFirstResult,
            ItemUseTickEventData,
        },
        item_stack::ItemStack as WitItemStack,
    },
};

/// Registers one of the events of this file.
pub fn register_mixin_event(
    ctx: &Context,
    handler: &Arc<WasmPluginEventHandler>,
    priority: EventPriority,
    blocking: bool,
    event_type: EventType,
) {
    let handler = Arc::clone(handler);
    match event_type {
        EventType::EntityFluidMovementEvent => {
            ctx.register_event::<EntityFluidMovementEvent, _>(handler, priority, blocking);
        }
        EventType::ItemUseTickEvent => {
            ctx.register_event::<ItemUseTickEvent, _>(handler, priority, blocking);
        }
        EventType::ItemSwingEvent => {
            ctx.register_event::<ItemSwingEvent, _>(handler, priority, blocking);
        }
        EventType::ItemUseFirstEvent => {
            ctx.register_event::<ItemUseFirstEvent, _>(handler, priority, blocking);
        }
        EventType::CanAttackBlockEvent => {
            ctx.register_event::<CanAttackBlockEvent, _>(handler, priority, blocking);
        }
        EventType::EntityItemUpdateEvent => {
            ctx.register_event::<EntityItemUpdateEvent, _>(handler, priority, blocking);
        }
        EventType::FakePlayerPlaceCheckEvent => {
            ctx.register_event::<FakePlayerPlaceCheckEvent, _>(handler, priority, blocking);
        }
        EventType::FluidCanPassThroughEvent => {
            ctx.register_event::<FluidCanPassThroughEvent, _>(handler, priority, blocking);
        }
        _ => {
            tracing::error!("unexpected event routed to register_mixin_event");
        }
    }
}

fn add_stack(state: &mut PluginHostState, stack: &ItemStack) -> Resource<WitItemStack> {
    state
        .add(Arc::new(Mutex::new(stack.clone())))
        .expect("failed to add item stack resource")
}

impl ToFromWasmEvent for EntityFluidMovementEvent {
    fn to_wasm_event(&self, _state: &mut PluginHostState) -> Event {
        Event::EntityFluidMovementEvent(EntityFluidMovementEventData {
            entity_id: self.entity_id,
            fluid: self.fluid.clone(),
            velocity: to_wasm_position(self.velocity),
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct EntityFluidMovementEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::EntityFluidMovementEvent(data) = event {
            let velocity = from_wasm_position(data.velocity);
            // A velocity that is not a number would break the movement.
            if velocity.x.is_finite() && velocity.y.is_finite() && velocity.z.is_finite() {
                self.velocity = velocity;
            }
        }
    }
}

impl ToFromWasmEvent for ItemUseTickEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        Event::ItemUseTickEvent(ItemUseTickEventData {
            player,
            stack: add_stack(state, &self.stack),
            remaining_ticks: self.remaining_ticks,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct ItemUseTickEvent from WASM")
    }

    /// Informational.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for ItemSwingEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        Event::ItemSwingEvent(ItemSwingEventData {
            player,
            hand: to_wasm_hand(self.hand),
            stack: add_stack(state, &self.stack),
            cancelled: self.cancelled,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct ItemSwingEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::ItemSwingEvent(data) = event {
            self.cancelled = data.cancelled;
        }
    }
}

impl ToFromWasmEvent for ItemUseFirstEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        Event::ItemUseFirstEvent(ItemUseFirstEventData {
            player,
            hand: to_wasm_hand(self.hand),
            stack: add_stack(state, &self.stack),
            block_pos: to_wasm_block_position(self.position),
            face: to_wit_block_direction(self.face),
            use_result: match self.result {
                ItemUseFirstResult::Pass => WitUseFirstResult::Pass,
                ItemUseFirstResult::Consume => WitUseFirstResult::Consume,
                ItemUseFirstResult::Success => WitUseFirstResult::Success,
            },
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct ItemUseFirstEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::ItemUseFirstEvent(data) = event {
            self.result = match data.use_result {
                WitUseFirstResult::Pass => ItemUseFirstResult::Pass,
                WitUseFirstResult::Consume => ItemUseFirstResult::Consume,
                WitUseFirstResult::Success => ItemUseFirstResult::Success,
            };
        }
    }
}

impl ToFromWasmEvent for CanAttackBlockEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        Event::CanAttackBlockEvent(CanAttackBlockEventData {
            player,
            stack: add_stack(state, &self.stack),
            block_pos: to_wasm_block_position(self.position),
            cancelled: self.cancelled,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct CanAttackBlockEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::CanAttackBlockEvent(data) = event {
            self.cancelled = data.cancelled;
        }
    }
}

impl ToFromWasmEvent for EntityItemUpdateEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        Event::EntityItemUpdateEvent(EntityItemUpdateEventData {
            entity_id: self.entity_id,
            stack: add_stack(state, &self.stack),
            position: to_wasm_position(self.position),
            age: self.age,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct EntityItemUpdateEvent from WASM")
    }

    /// Informational.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for FakePlayerPlaceCheckEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        Event::FakePlayerPlaceCheckEvent(FakePlayerPlaceCheckEventData {
            player,
            stack: add_stack(state, &self.stack),
            block_pos: to_wasm_block_position(self.position),
            face: to_wit_block_direction(self.face),
            clicked_replaceable: self.clicked_replaceable,
            cancelled: self.cancelled,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct FakePlayerPlaceCheckEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::FakePlayerPlaceCheckEvent(data) = event {
            self.cancelled = data.cancelled;
        }
    }
}

impl ToFromWasmEvent for FluidCanPassThroughEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");
        Event::FluidCanPassThroughEvent(FluidCanPassThroughEventData {
            target_world,
            block_pos: to_wasm_block_position(self.position),
            direction: to_wit_block_direction(self.direction),
            fluid: self.fluid.clone(),
            cancelled: self.cancelled,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct FluidCanPassThroughEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::FluidCanPassThroughEvent(data) = event {
            self.cancelled = data.cancelled;
        }
    }
}
