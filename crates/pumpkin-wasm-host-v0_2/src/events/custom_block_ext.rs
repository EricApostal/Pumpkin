//! Events of the extension hooks of custom blocks (shapes, interaction, physics and queries).

use std::sync::Arc;

use pumpkin_core::plugin::block::{
    CustomBlockAttackEvent, CustomBlockCloneItemEvent, CustomBlockCondition,
    CustomBlockConditionEvent, CustomBlockFireSpreadEvent, CustomBlockLandedEvent,
    CustomBlockPhysicsEvent, CustomBlockPhysicsKind, CustomBlockPlayerWillDestroyEvent,
    CustomBlockShapeEvent, CustomBlockSoundTypeEvent, CustomBlockUseEvent, CustomBlockUseResult,
};
use pumpkin_data::block_registry::ShapeKind;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_wasm_host_common::state::PluginHostState;
use tokio::sync::Mutex;

use crate::{
    events::{
        ToFromWasmEvent, cleanup_event, to_wasm_block_name, to_wasm_block_position,
        to_wasm_entity_type, to_wasm_hand,
    },
    mob::to_wit_block_direction,
    pumpkin::plugin::{
        block_registry::BlockBox as WitBlockBox,
        event::{
            BlockCondition as WitBlockCondition, BlockPhysicsKind as WitPhysicsKind,
            BlockShapeKind as WitShapeKind, BlockUseResult as WitUseResult,
            CustomBlockAttackEventData, CustomBlockCloneItemEventData,
            CustomBlockConditionEventData, CustomBlockFireSpreadEventData,
            CustomBlockLandedEventData, CustomBlockPhysicsEventData,
            CustomBlockPlayerWillDestroyEventData, CustomBlockShapeEventData,
            CustomBlockSoundTypeEventData, CustomBlockUseEventData, Event,
        },
    },
};

const fn to_wit_shape_kind(kind: ShapeKind) -> WitShapeKind {
    match kind {
        ShapeKind::Collision => WitShapeKind::Collision,
        ShapeKind::Selection => WitShapeKind::Selection,
        ShapeKind::Visual => WitShapeKind::Visual,
    }
}

const fn to_wit_use_result(result: CustomBlockUseResult) -> WitUseResult {
    match result {
        CustomBlockUseResult::Success => WitUseResult::Success,
        CustomBlockUseResult::Consume => WitUseResult::Consume,
        CustomBlockUseResult::Pass => WitUseResult::Pass,
        CustomBlockUseResult::Fail => WitUseResult::Fail,
        CustomBlockUseResult::TryEmptyHand => WitUseResult::TryEmptyHand,
    }
}

const fn from_wit_use_result(result: WitUseResult) -> CustomBlockUseResult {
    match result {
        WitUseResult::Success => CustomBlockUseResult::Success,
        WitUseResult::Consume => CustomBlockUseResult::Consume,
        WitUseResult::Pass => CustomBlockUseResult::Pass,
        WitUseResult::Fail => CustomBlockUseResult::Fail,
        WitUseResult::TryEmptyHand => CustomBlockUseResult::TryEmptyHand,
    }
}

const fn to_wit_physics_kind(kind: CustomBlockPhysicsKind) -> WitPhysicsKind {
    match kind {
        CustomBlockPhysicsKind::Friction => WitPhysicsKind::Friction,
        CustomBlockPhysicsKind::SpeedFactor => WitPhysicsKind::SpeedFactor,
        CustomBlockPhysicsKind::JumpFactor => WitPhysicsKind::JumpFactor,
    }
}

const fn to_wit_condition(condition: CustomBlockCondition) -> WitBlockCondition {
    match condition {
        CustomBlockCondition::EntityDestroy => WitBlockCondition::EntityDestroy,
        CustomBlockCondition::PathfindableLand => WitBlockCondition::PathfindableLand,
        CustomBlockCondition::PathfindableWater => WitBlockCondition::PathfindableWater,
        CustomBlockCondition::PathfindableAir => WitBlockCondition::PathfindableAir,
        CustomBlockCondition::RedstoneConnect => WitBlockCondition::RedstoneConnect,
    }
}

const fn to_wit_box(shape: &BoundingBox) -> WitBlockBox {
    WitBlockBox {
        min_x: shape.min.x,
        min_y: shape.min.y,
        min_z: shape.min.z,
        max_x: shape.max.x,
        max_y: shape.max.y,
        max_z: shape.max.z,
    }
}

const fn from_wit_box(shape: &WitBlockBox) -> BoundingBox {
    BoundingBox {
        min: Vector3::new(shape.min_x, shape.min_y, shape.min_z),
        max: Vector3::new(shape.max_x, shape.max_y, shape.max_z),
    }
}

impl ToFromWasmEvent for CustomBlockShapeEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");
        Event::CustomBlockShapeEvent(CustomBlockShapeEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            kind: to_wit_shape_kind(self.kind),
            shapes: self.shapes.iter().map(to_wit_box).collect(),
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct CustomBlockShapeEvent from WASM")
    }

    /// Only the shapes are the plugin's to change.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::CustomBlockShapeEvent(data) = event {
            self.shapes = data.shapes.iter().map(from_wit_box).collect();
        }
    }
}

impl ToFromWasmEvent for CustomBlockUseEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        let item = state
            .add(Arc::new(Mutex::new(self.item.clone())))
            .expect("failed to add item stack resource");
        Event::CustomBlockUseEvent(CustomBlockUseEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            player,
            hand: to_wasm_hand(self.hand),
            with_item: self.with_item,
            item,
            face: to_wit_block_direction(self.face),
            cursor: (self.cursor.x, self.cursor.y, self.cursor.z),
            sneaking: self.sneaking,
            result: to_wit_use_result(self.result),
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct CustomBlockUseEvent from WASM")
    }

    /// Only the result is the plugin's to change.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::CustomBlockUseEvent(data) = event {
            self.result = from_wit_use_result(data.result);
        }
    }
}

impl ToFromWasmEvent for CustomBlockAttackEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        Event::CustomBlockAttackEvent(CustomBlockAttackEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            player,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct CustomBlockAttackEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for CustomBlockCloneItemEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        let item = self.item.as_ref().map(|item| {
            state
                .add(Arc::new(Mutex::new(item.clone())))
                .expect("failed to add item stack resource")
        });
        Event::CustomBlockCloneItemEvent(CustomBlockCloneItemEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            player,
            item,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct CustomBlockCloneItemEvent from WASM")
    }

    /// Reads the item before the resources are cleaned up.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        if let Event::CustomBlockCloneItemEvent(data) = &event {
            self.item = data.item.as_ref().and_then(|item| {
                state
                    .get(item)
                    .ok()
                    .and_then(|stack| stack.try_lock().ok().map(|stack| stack.clone()))
            });
        }
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for CustomBlockPlayerWillDestroyEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        Event::CustomBlockPlayerWillDestroyEvent(CustomBlockPlayerWillDestroyEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            player,
            cancelled: self.cancelled,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct CustomBlockPlayerWillDestroyEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::CustomBlockPlayerWillDestroyEvent(data) = event {
            self.cancelled = data.cancelled;
        }
    }
}

impl ToFromWasmEvent for CustomBlockPhysicsEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");
        Event::CustomBlockPhysicsEvent(CustomBlockPhysicsEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            kind: to_wit_physics_kind(self.kind),
            value: self.value,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct CustomBlockPhysicsEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::CustomBlockPhysicsEvent(data) = event {
            self.value = data.value;
        }
    }
}

impl ToFromWasmEvent for CustomBlockLandedEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");
        Event::CustomBlockLandedEvent(CustomBlockLandedEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            entity_id: self.entity_id,
            fall_distance: self.fall_distance,
            damage_multiplier: self.damage_multiplier,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct CustomBlockLandedEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::CustomBlockLandedEvent(data) = event {
            self.damage_multiplier = data.damage_multiplier;
        }
    }
}

impl ToFromWasmEvent for CustomBlockConditionEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");
        Event::CustomBlockConditionEvent(CustomBlockConditionEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            condition: to_wit_condition(self.condition),
            entity_type: self.entity_type.map(to_wasm_entity_type),
            direction: self.direction.map(to_wit_block_direction),
            value: self.value,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct CustomBlockConditionEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::CustomBlockConditionEvent(data) = event {
            self.value = data.value;
        }
    }
}

impl ToFromWasmEvent for CustomBlockSoundTypeEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");
        Event::CustomBlockSoundTypeEvent(CustomBlockSoundTypeEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            sound_type: self.sound_type.clone(),
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct CustomBlockSoundTypeEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::CustomBlockSoundTypeEvent(data) = event {
            self.sound_type = data.sound_type;
        }
    }
}

impl ToFromWasmEvent for CustomBlockFireSpreadEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");
        Event::CustomBlockFireSpreadEvent(CustomBlockFireSpreadEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            state_index: self.state_index,
            spread_chance: self.spread_chance,
            burn_chance: self.burn_chance,
        })
    }

    fn from_wasm_event(_event: Event, _state: &mut PluginHostState) -> Self {
        panic!("Cannot construct CustomBlockFireSpreadEvent from WASM")
    }

    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::CustomBlockFireSpreadEvent(data) = event {
            self.spread_chance = data.spread_chance;
            self.burn_chance = data.burn_chance;
        }
    }
}
