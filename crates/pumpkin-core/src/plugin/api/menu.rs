//! Menus of plugin registered types, see `pumpkin_data::menu_registry`.
//!
//! A menu is a window like a chest's: it has a container id from the player's counter and the
//! server tracks it in `Player::current_screen_handler`, so it closes like any other window. It has
//! no behaviour of its own. Clicks in it are routed to the plugin as `MenuClickEvent` (see
//! `Player::on_slot_click`) and are never applied, and closing it fires `MenuClosedEvent` (see
//! `Player::on_handled_screen_closed`). A menu with zero slots sends no slot data at all.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::menu_registry::PluginMenuType;
use pumpkin_inventory::Inventory;
use pumpkin_inventory::player::player_screen_handler::PlayerScreenHandler;
use pumpkin_inventory::screen_handler::{
    ClickType, InventoryPlayer, ScreenHandler, ScreenHandlerBehaviour,
};
use pumpkin_inventory::slot::NormalSlot;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::{CCustomPayload, COpenScreen, CSetContainerProperty};
use pumpkin_protocol::java::server::play::{SClickSlot, SlotActionType};
use pumpkin_protocol::ser::{NetworkWriteExt, WritingError};
use pumpkin_util::text::TextComponent;
use pumpkin_util::version::JavaMinecraftVersion;

use super::events::player::menu::{MenuChangedSlot, MenuClickEvent};
use super::gui::PluginInventory;
use crate::entity::player::Player;
use crate::net::ClientPlatform;
use crate::server::Server;

/// The most slots a plugin menu can have the server track.
pub const MAX_PLUGIN_MENU_SLOTS: usize = 256;

/// The channel of the payload NeoForge opens menus with when they carry extra data.
pub const ADVANCED_OPEN_SCREEN_CHANNEL: &str = "neoforge:advanced_open_screen";

/// The screen handler of a plugin menu.
pub struct PluginMenuScreenHandler {
    behaviour: ScreenHandlerBehaviour,
    inventory: Arc<PluginInventory>,
    menu_type: &'static PluginMenuType,
}

impl PluginMenuScreenHandler {
    #[must_use]
    pub fn new(sync_id: u8, menu_type: &'static PluginMenuType, slot_count: usize) -> Self {
        let inventory = Arc::new(PluginInventory::new(slot_count));
        // Not a vanilla window type: the type id is the one of the plugin's menu type.
        let mut behaviour = ScreenHandlerBehaviour::new(sync_id, None);
        behaviour.allow_grab_items = false;
        behaviour.allow_put_items = false;
        behaviour.container_slots = slot_count;

        let mut handler = Self {
            behaviour,
            inventory: inventory.clone(),
            menu_type,
        };
        for index in 0..slot_count {
            handler.add_slot(Arc::new(NormalSlot::new(inventory.clone(), index)));
        }
        handler
    }

    #[must_use]
    pub const fn menu_type(&self) -> &'static PluginMenuType {
        self.menu_type
    }

    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.inventory.size()
    }

    /// Puts a stack into a slot and sends it to the client. `false` if the slot does not exist.
    pub fn set_slot(&mut self, slot: usize, stack: ItemStack) -> bool {
        if slot >= self.inventory.size() {
            return false;
        }
        self.inventory.set_stack(slot, stack);
        self.send_content_updates();
        true
    }
}

impl ScreenHandler for PluginMenuScreenHandler {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn get_behaviour(&self) -> &ScreenHandlerBehaviour {
        &self.behaviour
    }

    fn get_behaviour_mut(&mut self) -> &mut ScreenHandlerBehaviour {
        &mut self.behaviour
    }

    /// The plugin gets every click, whatever slot it names.
    fn is_slot_valid(&self, _slot: i32) -> bool {
        true
    }

    fn quick_move(&mut self, _player: &dyn InventoryPlayer, _slot_index: i32) -> ItemStack {
        ItemStack::EMPTY.clone()
    }

    /// Clicks are never applied, so there is no cursor stack to give back.
    fn on_closed(&mut self, _player: &dyn InventoryPlayer) {}
}

/// The body of the `neoforge:advanced_open_screen` payload: container id, menu type id, title and
/// the extra data of the menu as a byte array.
fn advanced_open_screen_body(
    sync_id: u8,
    menu_type: &PluginMenuType,
    title: &TextComponent,
    extra_data: &[u8],
    version: &JavaMinecraftVersion,
) -> Result<Vec<u8>, WritingError> {
    let mut body = Vec::with_capacity(extra_data.len() + 64);
    body.write_var_int(&VarInt(i32::from(sync_id)))?;
    body.write_var_int(&VarInt(menu_type.id as i32))?;
    body.write_component(title, version)?;
    body.write_var_int(&VarInt(extra_data.len() as i32))?;
    body.write_slice(extra_data)?;
    Ok(body)
}

impl Player {
    /// Opens a menu of a plugin registered type for a Java player and returns its container id.
    ///
    /// The window the player has open closes first, like opening a chest does. With `extra_data`
    /// the menu opens with the NeoForge payload `neoforge:advanced_open_screen`, otherwise with the
    /// vanilla open screen packet. The `slot_count` slots start empty and are sent to the client.
    pub fn open_plugin_menu(
        &self,
        menu_type: &'static PluginMenuType,
        title: &TextComponent,
        extra_data: Option<&[u8]>,
        slot_count: usize,
    ) -> Result<u32, String> {
        let ClientPlatform::Java(client) = self.client.as_ref() else {
            return Err("menus of plugin types are only available to Java players".to_string());
        };
        if slot_count > MAX_PLUGIN_MENU_SLOTS {
            return Err(format!(
                "a plugin menu has at most {MAX_PLUGIN_MENU_SLOTS} slots"
            ));
        }
        let payload = extra_data.map(|extra_data| (extra_data.to_vec(), client.version.load()));

        if !self
            .current_screen_handler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_any()
            .is::<PlayerScreenHandler>()
        {
            self.close_handled_screen();
        }

        self.increment_screen_handler_sync_id();
        let sync_id = self.screen_handler_sync_id.load(Ordering::Relaxed);
        let handler: Arc<std::sync::Mutex<dyn ScreenHandler>> = Arc::new(std::sync::Mutex::new(
            PluginMenuScreenHandler::new(sync_id, menu_type, slot_count),
        ));

        if let Some((extra_data, version)) = payload {
            let body = advanced_open_screen_body(sync_id, menu_type, title, &extra_data, &version)
                .map_err(|error| format!("could not encode the open screen payload: {error}"))?;
            self.try_send_client_packet(&CCustomPayload::new(ADVANCED_OPEN_SCREEN_CHANNEL, &body));
        } else {
            self.try_send_client_packet(&COpenScreen::new(
                VarInt(i32::from(sync_id)),
                VarInt(menu_type.id as i32),
                title,
            ));
        }

        // A menu without slots has nothing to synchronise, it never gets a sync handler.
        if slot_count > 0 {
            self.on_screen_handler_opened(&handler);
        }
        *self
            .current_screen_handler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = handler;
        self.open_container_pos.store(None);
        Ok(u32::from(sync_id))
    }

    /// Runs `f` on the plugin menu the player has open, `None` if the open window is not one.
    fn with_plugin_menu<R>(&self, f: impl FnOnce(&mut PluginMenuScreenHandler) -> R) -> Option<R> {
        let handler = self
            .current_screen_handler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut handler = handler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        handler
            .as_any_mut()
            .downcast_mut::<PluginMenuScreenHandler>()
            .map(f)
    }

    /// The container id, type and slot count of the plugin menu the player has open.
    #[must_use]
    pub fn open_plugin_menu_info(&self) -> Option<(u8, &'static PluginMenuType, usize)> {
        self.with_plugin_menu(|menu| (menu.sync_id(), menu.menu_type(), menu.slot_count()))
    }

    /// Closes the plugin menu the player has open. Returns whether there was one.
    pub fn close_plugin_menu(&self) -> bool {
        if self.open_plugin_menu_info().is_none() {
            return false;
        }
        self.close_handled_screen();
        true
    }

    /// Puts a stack into a slot of the open plugin menu and sends it to the client.
    pub fn set_plugin_menu_slot(&self, slot: usize, stack: ItemStack) -> Result<(), String> {
        match self.with_plugin_menu(|menu| menu.set_slot(slot, stack)) {
            Some(true) => Ok(()),
            Some(false) => Err(format!("the menu has no slot {slot}")),
            None => Err("the player has no plugin menu open".to_string()),
        }
    }

    /// Sends a data slot of the open plugin menu to the client.
    pub fn set_plugin_menu_data(&self, id: i16, value: i16) -> Result<(), String> {
        let Some((sync_id, _, _)) = self.open_plugin_menu_info() else {
            return Err("the player has no plugin menu open".to_string());
        };
        self.try_send_client_packet(&CSetContainerProperty::new(
            VarInt(i32::from(sync_id)),
            id,
            value,
        ));
        Ok(())
    }
}

/// The type of a click packet, like `Player::on_slot_click` works it out.
fn click_type(packet: &SClickSlot) -> ClickType {
    match packet.mode {
        SlotActionType::Pickup => {
            if packet.button == SClickSlot::BUTTON_LEFT {
                ClickType::Left
            } else {
                ClickType::Right
            }
        }
        SlotActionType::QuickMove => {
            if packet.button == SClickSlot::BUTTON_LEFT {
                ClickType::ShiftLeft
            } else {
                ClickType::ShiftRight
            }
        }
        SlotActionType::Swap => ClickType::NumberKey(packet.button as u8),
        SlotActionType::Clone => ClickType::Middle,
        SlotActionType::Throw => {
            if packet.button == SClickSlot::BUTTON_DROP_SINGLE {
                ClickType::Drop
            } else {
                ClickType::ControlDrop
            }
        }
        SlotActionType::QuickCraft => {
            if [0, 4, 8].contains(&packet.button) {
                ClickType::Left
            } else if [1, 5, 9].contains(&packet.button) {
                ClickType::Right
            } else {
                ClickType::Middle
            }
        }
        SlotActionType::PickupAll => ClickType::DoubleClick,
    }
}

/// The item id and count of a stack a client reports in a click.
fn reported_stack(
    hash: &pumpkin_protocol::codec::item_stack_seralizer::OptionalItemStackHash,
) -> (Option<u32>, u32) {
    hash.item_and_count().map_or((None, 0), |(item_id, count)| {
        (
            u32::try_from(item_id).ok(),
            u32::try_from(count).unwrap_or(0),
        )
    })
}

impl Player {
    /// Routes a click packet to the plugin when the player's open window is a plugin menu. Returns
    /// whether it was, in which case the server does not apply the click.
    ///
    /// The plugin hears about the click as a `MenuClickEvent`, then the slots of the menu are sent
    /// to the client again, so what it predicted is reverted unless the plugin changed the slots.
    pub(crate) fn route_plugin_menu_click(
        self: &Arc<Self>,
        packet: &SClickSlot,
        server: &Arc<Server>,
    ) -> bool {
        let handler = self
            .current_screen_handler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let (container_id, menu_type) = {
            let guard = handler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(menu) = guard.as_any().downcast_ref::<PluginMenuScreenHandler>() else {
                return false;
            };
            (menu.sync_id(), menu.menu_type().key)
        };

        let (carried_item_id, carried_count) = reported_stack(&packet.carried_item);
        let changed_slots = packet
            .array_of_changed_slots
            .iter()
            .map(|(slot, hash)| {
                let (item_id, count) = reported_stack(hash);
                MenuChangedSlot {
                    slot: *slot,
                    item_id,
                    count,
                }
            })
            .collect();
        let mut event = MenuClickEvent {
            player: self.clone(),
            container_id: u32::from(container_id),
            menu_type: menu_type.to_string(),
            slot: packet.slot,
            button: packet.button,
            click_type: click_type(packet),
            changed_slots,
            carried_item_id,
            carried_count,
        };
        server.plugin_manager.fire_blocking(server, &mut event);

        // The plugin may have closed or replaced the menu while it handled the click.
        let still_open = Arc::ptr_eq(
            &handler,
            &self
                .current_screen_handler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        if still_open {
            handler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .sync_state();
        }
        true
    }
}
