//! Events of the host features for client mods: plugin block entities, plugin menus and the use
//! events of items and blocks.

use std::sync::Arc;

use pumpkin_data::item_stack::ItemStack;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::vector3::Vector3;
use tokio::sync::Mutex;

use pumpkin_core::plugin::{
    block::{BlockEntityLoadEvent, BlockEntityTickEvent, BlockEntityUnloadEvent},
    player::{MenuClickEvent, MenuClosedEvent, PlayerUseBlockEvent, PlayerUseItemEvent},
    world::WorldTickEvent,
};
use pumpkin_wasm_host_common::state::PluginHostState;

use crate::{
    common::to_wit_nbt_tree,
    events::{
        ToFromWasmEvent, cleanup_event, consume_player, consume_world, from_wasm_block_name,
        from_wasm_block_position, from_wasm_hand, to_wasm_block_name, to_wasm_block_position,
        to_wasm_click_type, to_wasm_hand,
    },
    mob::{from_wit_block_direction, to_wit_block_direction},
    pumpkin::plugin::event::{
        BlockEntityLoadEventData, BlockEntityTickEventData, BlockEntityUnloadEventData, Event,
        MenuChangedSlot, MenuClickEventData, MenuClosedEventData, PlayerUseBlockEventData,
        PlayerUseItemEventData, WorldTickEventData,
    },
};

impl ToFromWasmEvent for PlayerUseItemEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        let item = state
            .add(Arc::new(Mutex::new(self.item.clone())))
            .expect("failed to add item stack resource");

        Event::PlayerUseItemEvent(PlayerUseItemEventData {
            player,
            hand: to_wasm_hand(self.hand),
            item,
            clicked_pos: self.clicked_pos.map(to_wasm_block_position),
            face: self.face.map(to_wit_block_direction),
            cancelled: self.cancelled,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::PlayerUseItemEvent(data) => {
                state.discard_to_be_removed(&data.item);
                Self {
                    player: consume_player(state, &data.player),
                    hand: from_wasm_hand(data.hand),
                    item: ItemStack::EMPTY.clone(),
                    clicked_pos: data.clicked_pos.map(from_wasm_block_position),
                    face: data.face.map(from_wit_block_direction),
                    cancelled: data.cancelled,
                }
            }
            _ => panic!("unexpected event type"),
        }
    }

    /// The use is the host's, a handler can only cancel it.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::PlayerUseItemEvent(data) = event {
            self.cancelled = data.cancelled;
        }
    }
}

impl ToFromWasmEvent for PlayerUseBlockEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");
        let item = state
            .add(Arc::new(Mutex::new(self.item.clone())))
            .expect("failed to add item stack resource");

        Event::PlayerUseBlockEvent(PlayerUseBlockEventData {
            player,
            hand: to_wasm_hand(self.hand),
            item,
            block_pos: to_wasm_block_position(self.position),
            block: to_wasm_block_name(self.block),
            face: to_wit_block_direction(self.face),
            cursor: (self.cursor.x, self.cursor.y, self.cursor.z),
            sneaking: self.sneaking,
            cancelled: self.cancelled,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::PlayerUseBlockEvent(data) => {
                state.discard_to_be_removed(&data.item);
                Self {
                    player: consume_player(state, &data.player),
                    hand: from_wasm_hand(data.hand),
                    item: ItemStack::EMPTY.clone(),
                    position: from_wasm_block_position(data.block_pos),
                    block: from_wasm_block_name(&data.block),
                    face: from_wit_block_direction(data.face),
                    cursor: Vector3::new(data.cursor.0, data.cursor.1, data.cursor.2),
                    sneaking: data.sneaking,
                    cancelled: data.cancelled,
                }
            }
            _ => panic!("unexpected event type"),
        }
    }

    /// The click is the host's, a handler can only cancel it.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
        if let Event::PlayerUseBlockEvent(data) = event {
            self.cancelled = data.cancelled;
        }
    }
}

impl ToFromWasmEvent for MenuClickEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");

        Event::MenuClickEvent(MenuClickEventData {
            player,
            container_id: self.container_id,
            menu_type: self.menu_type.clone(),
            slot: self.slot,
            button: self.button,
            click_type: to_wasm_click_type(self.click_type),
            changed_slots: self
                .changed_slots
                .iter()
                .map(|changed| MenuChangedSlot {
                    slot: changed.slot,
                    item_id: changed.item_id,
                    count: changed.count,
                })
                .collect(),
            carried_item_id: self.carried_item_id,
            carried_count: self.carried_count,
        })
    }

    /// The event only reports what the client did, the host never reads it back.
    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::MenuClickEvent(data) => Self {
                player: consume_player(state, &data.player),
                container_id: data.container_id,
                menu_type: data.menu_type,
                slot: data.slot,
                button: data.button,
                click_type: pumpkin_inventory::screen_handler::ClickType::Unknown,
                changed_slots: Vec::new(),
                carried_item_id: data.carried_item_id,
                carried_count: data.carried_count,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for MenuClosedEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let player = state
            .add(self.player.clone())
            .expect("failed to add player resource");

        Event::MenuClosedEvent(MenuClosedEventData {
            player,
            container_id: self.container_id,
            menu_type: self.menu_type.clone(),
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::MenuClosedEvent(data) => Self {
                player: consume_player(state, &data.player),
                container_id: data.container_id,
                menu_type: data.menu_type,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for BlockEntityLoadEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::BlockEntityLoadEvent(BlockEntityLoadEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            type_key: self.type_key.clone(),
            save_data: to_wit_nbt_tree(NbtTag::Compound(self.save_data.clone())),
            client_data: self
                .client_data
                .clone()
                .map(|data| to_wit_nbt_tree(NbtTag::Compound(data))),
        })
    }

    /// The event only reports what loaded, the host never reads it back.
    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::BlockEntityLoadEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                type_key: data.type_key,
                save_data: pumpkin_nbt::compound::NbtCompound::new(),
                client_data: None,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for BlockEntityUnloadEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::BlockEntityUnloadEvent(BlockEntityUnloadEventData {
            target_world,
            pos: to_wasm_block_position(self.position),
            type_key: self.type_key.clone(),
            removed: self.removed,
            save_data: to_wit_nbt_tree(NbtTag::Compound(self.save_data.clone())),
        })
    }

    /// The event only reports what unloaded, the host never reads it back.
    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::BlockEntityUnloadEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                position: from_wasm_block_position(data.pos),
                type_key: data.type_key,
                removed: data.removed,
                save_data: pumpkin_nbt::compound::NbtCompound::new(),
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for BlockEntityTickEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::BlockEntityTickEvent(BlockEntityTickEventData {
            target_world,
            type_key: self.type_key.clone(),
            tick: self.tick,
            positions: self
                .positions
                .iter()
                .map(|position| to_wasm_block_position(*position))
                .collect(),
        })
    }

    /// The event only reports what is due, the host never reads it back.
    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::BlockEntityTickEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                type_key: data.type_key,
                tick: data.tick,
                positions: data
                    .positions
                    .into_iter()
                    .map(from_wasm_block_position)
                    .collect(),
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}

impl ToFromWasmEvent for WorldTickEvent {
    fn to_wasm_event(&self, state: &mut PluginHostState) -> Event {
        let target_world = state
            .add(self.world.clone())
            .expect("failed to add world resource");

        Event::WorldTickEvent(WorldTickEventData {
            target_world,
            tick: self.tick,
        })
    }

    fn from_wasm_event(event: Event, state: &mut PluginHostState) -> Self {
        match event {
            Event::WorldTickEvent(data) => Self {
                world: consume_world(state, &data.target_world),
                tick: data.tick,
            },
            _ => panic!("unexpected event type"),
        }
    }

    /// Informational only.
    fn apply_wasm_event(&mut self, event: Event, state: &mut PluginHostState) {
        cleanup_event(&event, state);
    }
}
