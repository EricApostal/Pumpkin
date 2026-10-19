//! Events of the hooks that custom blocks opted in to with `block-callbacks`.

use pumpkin_data::BlockStateId;

use pumpkin_core::plugin::block::{
    CustomBlockBrokenEvent, CustomBlockCanPlaceEvent, CustomBlockEntityCollisionEvent,
    CustomBlockEntityStepEvent, CustomBlockNeighborUpdateEvent, CustomBlockPlacedEvent,
    CustomBlockRandomTickEvent, CustomBlockScheduledTickEvent, CustomBlockStateReplacedEvent,
    CustomBlockUpdateShapeEvent,
};
use pumpkin_core::plugin::block::{
    CustomBlockComparatorOutputEvent, CustomBlockRedstonePowerEvent,
};
use pumpkin_wasm_host_common::state::PluginHostState;

use crate::{
    events::{
        ToFromWasmEvent, cleanup_event, consume_world, from_wasm_block_name,
        from_wasm_block_position, from_wasm_entity_type, from_wasm_position, to_wasm_block_name,
        to_wasm_block_position, to_wasm_entity_type, to_wasm_position,
    },
    mob::{from_wit_block_direction, to_wit_block_direction},
    pumpkin::plugin::{
        event::{
            CustomBlockBrokenEventData, CustomBlockCanPlaceEventData,
            CustomBlockEntityCollisionEventData, CustomBlockEntityStepEventData,
            CustomBlockNeighborUpdateEventData, CustomBlockPlacedEventData,
            CustomBlockStateReplacedEventData, CustomBlockTickEventData,
            CustomBlockUpdateShapeEventData, Event,
        },
        event::{CustomBlockComparatorOutputEventData, CustomBlockRedstonePowerEventData},
        uuid::Uuid as WitUuid,
    },
    uuid::UuidExt,
};

impl ToFromWasmEvent for CustomBlockNeighborUpdateEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::CustomBlockNeighborUpdateEvent(CustomBlockNeighborUpdateEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            source_pos: to_wasm_block_position(self.source_position),
            source_block: to_wasm_block_name(self.source_block),
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::CustomBlockNeighborUpdateEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                block: from_wasm_block_name(&data.block),
                state_index: data.state_index,
                source_position: from_wasm_block_position(data.source_pos),
                source_block: from_wasm_block_name(&data.source_block),
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for CustomBlockPlacedEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::CustomBlockPlacedEvent(CustomBlockPlacedEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            player: self.player.as_ref().map(WitUuid::to_wit),
            old_state_id: self.old_state_id.map(BlockStateId::as_u32),
            moved: self.moved,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::CustomBlockPlacedEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                block: from_wasm_block_name(&data.block),
                state_index: data.state_index,
                player: data.player.map(|id| WitUuid::from_wit(&id)),
                old_state_id: data.old_state_id.map(BlockStateId::from_raw_or_air),
                moved: data.moved,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for CustomBlockStateReplacedEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::CustomBlockStateReplacedEvent(CustomBlockStateReplacedEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            old_state_index: self.old_state_index,
            moved: self.moved,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::CustomBlockStateReplacedEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                block: from_wasm_block_name(&data.block),
                old_state_index: data.old_state_index,
                moved: data.moved,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for CustomBlockBrokenEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::CustomBlockBrokenEvent(CustomBlockBrokenEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            player: WitUuid::to_wit(&self.player),
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::CustomBlockBrokenEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                block: from_wasm_block_name(&data.block),
                state_index: data.state_index,
                player: WitUuid::from_wit(&data.player),
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for CustomBlockScheduledTickEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::CustomBlockScheduledTickEvent(CustomBlockTickEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::CustomBlockScheduledTickEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                block: from_wasm_block_name(&data.block),
                state_index: data.state_index,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for CustomBlockRandomTickEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::CustomBlockRandomTickEvent(CustomBlockTickEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::CustomBlockRandomTickEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                block: from_wasm_block_name(&data.block),
                state_index: data.state_index,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for CustomBlockEntityCollisionEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::CustomBlockEntityCollisionEvent(CustomBlockEntityCollisionEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            entity_id: self.entity_id,
            entity_uuid: WitUuid::to_wit(&self.entity_uuid),
            entity_type: to_wasm_entity_type(self.entity_type),
            entity_pos: to_wasm_position(self.entity_position),
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::CustomBlockEntityCollisionEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                block: from_wasm_block_name(&data.block),
                state_index: data.state_index,
                entity_id: data.entity_id,
                entity_uuid: WitUuid::from_wit(&data.entity_uuid),
                entity_type: from_wasm_entity_type(&data.entity_type),
                entity_position: from_wasm_position(data.entity_pos),
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for CustomBlockEntityStepEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::CustomBlockEntityStepEvent(CustomBlockEntityStepEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            entity_id: self.entity_id,
            entity_uuid: WitUuid::to_wit(&self.entity_uuid),
            entity_type: to_wasm_entity_type(self.entity_type),
            entity_pos: to_wasm_position(self.entity_position),
            below_supporting_block: self.below_supporting_block,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::CustomBlockEntityStepEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                block: from_wasm_block_name(&data.block),
                state_index: data.state_index,
                entity_id: data.entity_id,
                entity_uuid: WitUuid::from_wit(&data.entity_uuid),
                entity_type: from_wasm_entity_type(&data.entity_type),
                entity_position: from_wasm_position(data.entity_pos),
                below_supporting_block: data.below_supporting_block,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for CustomBlockCanPlaceEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::CustomBlockCanPlaceEvent(CustomBlockCanPlaceEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            face: self.face.map(to_wit_block_direction),
            player: self.player.as_ref().map(WitUuid::to_wit),
            can_place: self.can_place,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::CustomBlockCanPlaceEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                block: from_wasm_block_name(&data.block),
                state_index: data.state_index,
                face: data.face.map(from_wit_block_direction),
                player: data.player.map(|id| WitUuid::from_wit(&id)),
                can_place: data.can_place,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// The block and where it goes are the host's, a handler can only refuse.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::CustomBlockCanPlaceEvent(data) = event {
            self.can_place = data.can_place;
        }
    }
}

impl ToFromWasmEvent for CustomBlockUpdateShapeEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::CustomBlockUpdateShapeEvent(CustomBlockUpdateShapeEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            direction: to_wit_block_direction(self.direction),
            neighbor_pos: to_wasm_block_position(self.neighbor_position),
            neighbor_block: to_wasm_block_name(self.neighbor_block),
            neighbor_state_id: self.neighbor_state_id.as_u32(),
            new_state_index: self.new_state_index,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::CustomBlockUpdateShapeEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                block: from_wasm_block_name(&data.block),
                state_index: data.state_index,
                direction: from_wit_block_direction(data.direction),
                neighbor_position: from_wasm_block_position(data.neighbor_pos),
                neighbor_block: from_wasm_block_name(&data.neighbor_block),
                neighbor_state_id: BlockStateId::from_raw_or_air(data.neighbor_state_id),
                new_state_index: data.new_state_index,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// The block and its neighbour are the host's, a handler can only choose the new state.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::CustomBlockUpdateShapeEvent(data) = event {
            self.new_state_index = data.new_state_index;
        }
    }
}

impl ToFromWasmEvent for CustomBlockRedstonePowerEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::CustomBlockRedstonePowerEvent(CustomBlockRedstonePowerEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            face: to_wit_block_direction(self.face),
            strong: self.strong,
            power: self.power,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::CustomBlockRedstonePowerEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                block: from_wasm_block_name(&data.block),
                state_index: data.state_index,
                face: from_wit_block_direction(data.face),
                strong: data.strong,
                power: data.power,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// The block and the side are the host's, a handler can only set the power.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::CustomBlockRedstonePowerEvent(data) = event {
            self.power = data.power;
        }
    }
}

impl ToFromWasmEvent for CustomBlockComparatorOutputEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::CustomBlockComparatorOutputEvent(CustomBlockComparatorOutputEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            face: to_wit_block_direction(self.face),
            output: self.output,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::CustomBlockComparatorOutputEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                block: from_wasm_block_name(&data.block),
                state_index: data.state_index,
                face: from_wit_block_direction(data.face),
                output: data.output,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// The block and the side are the host's, a handler can only set the output.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::CustomBlockComparatorOutputEvent(data) = event {
            self.output = data.output;
        }
    }
}
