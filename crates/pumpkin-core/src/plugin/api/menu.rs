//! Menus of plugin registered types, see `pumpkin_data::menu_registry`.
//!
//! A menu is a window like a chest's: it has a container id from the player's counter and the
//! server tracks it in `Player::current_screen_handler`, so it closes like any other window. Clicks
//! in it are routed to the plugin as `MenuClickEvent` (see `Player::on_slot_click`), and closing it
//! fires `MenuClosedEvent` (see `Player::on_handled_screen_closed`). A menu with zero slots sends no
//! slot data at all.
//!
//! By default the server does not apply the clicks, the plugin decides what they do. A menu opened
//! with `PluginMenuOptions::apply_vanilla_clicks` runs the vanilla click logic of
//! `ScreenHandler::on_slot_click` instead, on the menu's slots (and optionally the player's
//! inventory slots behind them), with the per-slot rules the plugin gives (`MenuSlotKind`).

use std::any::Any;
use std::ops::Range;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::menu_registry::PluginMenuType;
use pumpkin_inventory::Inventory;
use pumpkin_inventory::player::player_screen_handler::PlayerScreenHandler;
use pumpkin_inventory::screen_handler::{
    ClickType, InventoryPlayer, ScreenHandler, ScreenHandlerBehaviour,
};
use pumpkin_inventory::slot::{NormalSlot, Slot};
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::{CCustomPayload, COpenScreen, CSetContainerProperty};
use pumpkin_protocol::java::server::play::{SClickSlot, SContainerButtonClick, SlotActionType};
use pumpkin_protocol::ser::{NetworkWriteExt, WritingError};
use pumpkin_util::text::TextComponent;
use pumpkin_util::version::JavaMinecraftVersion;

use super::events::player::menu::{
    MenuButtonClickEvent, MenuChangedSlot, MenuClickEvent, MenuClickMode, MenuSlotChange,
    MenuSlotChangedEvent,
};
use super::gui::PluginInventory;
use crate::entity::player::Player;
use crate::net::ClientPlatform;
use crate::server::Server;

/// The most slots a plugin menu can have the server track, player inventory slots included.
pub const MAX_PLUGIN_MENU_SLOTS: usize = 256;

/// The number of slots of the player inventory a menu can show behind its own slots.
pub const PLAYER_INVENTORY_SLOTS: usize = 36;

/// The channel of the payload NeoForge opens menus with when they carry extra data.
pub const ADVANCED_OPEN_SCREEN_CHANNEL: &str = "neoforge:advanced_open_screen";

/// How a slot of a plugin menu takes part in vanilla clicks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MenuSlotKind {
    /// Items go in and out like in a chest.
    #[default]
    Normal,
    /// Items can be taken (the whole stack at once) but not placed, like a result slot.
    OutputOnly,
    /// Nothing moves in or out.
    Locked,
    /// Clicks never move real items: with a stack on the cursor the slot becomes a copy of it and
    /// the cursor stays as it is, with an empty cursor the slot is emptied.
    Ghost,
    /// Like `Normal`, but only these items can be placed. Keys are item keys, the `minecraft:`
    /// prefix is optional.
    Filter(Vec<String>),
}

/// The rule of one slot of a plugin menu.
#[derive(Clone, Debug)]
pub struct MenuSlotRule {
    pub slot: usize,
    pub kind: MenuSlotKind,
    /// The most items the slot holds, `None` for the limit of the item.
    pub max_stack_size: Option<u8>,
}

/// The rules a menu that applies vanilla clicks follows.
#[derive(Clone, Debug, Default)]
pub struct MenuClickRules {
    /// Slots without a rule are `Normal`.
    pub slot_rules: Vec<MenuSlotRule>,
    /// The menu slots a shift-click moves items into, in order, from the player inventory slots
    /// (and from the menu's own slots when the menu has no player inventory slots).
    pub quick_move_targets: Vec<Range<usize>>,
}

/// How a plugin menu is opened.
#[derive(Clone, Debug, Default)]
pub struct PluginMenuOptions {
    /// Apply the vanilla click logic instead of leaving every click to the plugin.
    pub apply_vanilla_clicks: bool,
    /// Add the 36 slots of the player's inventory behind the menu's own slots (the main inventory
    /// first, then the hotbar, like every vanilla menu), backed by the real inventory.
    pub player_inventory: bool,
    pub rules: MenuClickRules,
}

fn item_matches(keys: &[String], stack: &ItemStack) -> bool {
    let key = stack.item.registry_key;
    keys.iter()
        .any(|k| k.strip_prefix("minecraft:").unwrap_or(k) == key)
}

/// A slot of a plugin menu, with the rule the plugin gave it.
struct PluginMenuSlot {
    inventory: Arc<PluginInventory>,
    index: usize,
    id: AtomicU8,
    rule: RwLock<(MenuSlotKind, Option<u8>)>,
}

impl PluginMenuSlot {
    fn kind(&self) -> MenuSlotKind {
        self.rule
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
            .clone()
    }

    fn set_rule(&self, kind: MenuSlotKind, max_stack_size: Option<u8>) {
        *self
            .rule
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = (kind, max_stack_size);
    }
}

impl Slot for PluginMenuSlot {
    fn get_inventory(&self) -> Arc<dyn Inventory> {
        self.inventory.clone()
    }

    fn get_index(&self) -> usize {
        self.index
    }

    fn set_id(&self, id: usize) {
        self.id.store(id as u8, Ordering::Relaxed);
    }

    fn mark_dirty(&self) {
        self.inventory.mark_dirty();
    }

    fn can_insert(&self, stack: &ItemStack) -> bool {
        match &self
            .rule
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
        {
            MenuSlotKind::Normal => true,
            MenuSlotKind::Filter(keys) => item_matches(keys, stack),
            MenuSlotKind::OutputOnly | MenuSlotKind::Locked | MenuSlotKind::Ghost => false,
        }
    }

    fn can_take_items(&self, _player: &dyn InventoryPlayer) -> bool {
        matches!(
            self.rule
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0,
            MenuSlotKind::Normal | MenuSlotKind::Filter(_) | MenuSlotKind::OutputOnly
        )
    }

    /// An output slot never gives a part of its stack.
    fn allow_modification(&self, _player: &dyn InventoryPlayer) -> bool {
        matches!(
            self.rule
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0,
            MenuSlotKind::Normal | MenuSlotKind::Filter(_)
        )
    }

    fn get_max_item_count(&self) -> u8 {
        self.rule
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
            .unwrap_or_else(|| self.inventory.get_max_count_per_stack())
    }

    /// Zero for a stack the slot does not take, which also keeps `insert_item` from stacking onto
    /// the copy a ghost slot or the contents of a locked or output slot.
    fn get_max_item_count_for_stack(&self, stack: &ItemStack) -> u8 {
        if self.can_insert(stack) {
            self.get_max_item_count().min(stack.get_max_stack_size())
        } else {
            0
        }
    }

    fn take_stack(&self, amount: u8) -> ItemStack {
        if self.kind() == MenuSlotKind::OutputOnly {
            self.inventory.remove_stack(self.index)
        } else {
            self.inventory.remove_stack_specific(self.index, amount)
        }
    }
}

/// The screen handler of a plugin menu.
pub struct PluginMenuScreenHandler {
    behaviour: ScreenHandlerBehaviour,
    inventory: Arc<PluginInventory>,
    menu_type: &'static PluginMenuType,
    /// The menu's own slots, which are the first slots of the handler.
    menu_slots: Vec<Arc<PluginMenuSlot>>,
    quick_move_targets: Vec<Range<usize>>,
    apply_vanilla_clicks: bool,
}

impl PluginMenuScreenHandler {
    /// `player_inventory` is the inventory behind the `PLAYER_INVENTORY_SLOTS` slots that follow
    /// the menu's, if the menu has them.
    pub fn new(
        sync_id: u8,
        menu_type: &'static PluginMenuType,
        slot_count: usize,
        options: &PluginMenuOptions,
        player_inventory: Option<&Arc<dyn Inventory>>,
    ) -> Result<Self, String> {
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
            menu_slots: Vec::with_capacity(slot_count),
            quick_move_targets: Vec::new(),
            apply_vanilla_clicks: options.apply_vanilla_clicks,
        };
        for index in 0..slot_count {
            let slot = Arc::new(PluginMenuSlot {
                inventory: inventory.clone(),
                index,
                id: AtomicU8::new(0),
                rule: RwLock::new((MenuSlotKind::Normal, None)),
            });
            handler.menu_slots.push(slot.clone());
            handler.add_slot(slot);
        }
        if let Some(player_inventory) = player_inventory {
            for slot in 0..PLAYER_INVENTORY_SLOTS {
                // The main inventory (9 to 35) comes first, then the hotbar (0 to 8).
                let index = (slot + 9) % PLAYER_INVENTORY_SLOTS;
                handler.add_slot(Arc::new(NormalSlot::new(player_inventory.clone(), index)));
            }
        }
        handler.set_rules(&options.rules)?;
        Ok(handler)
    }

    #[must_use]
    pub const fn menu_type(&self) -> &'static PluginMenuType {
        self.menu_type
    }

    /// The number of the menu's own slots, not counting the player inventory slots behind them.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.inventory.size()
    }

    #[must_use]
    pub const fn applies_vanilla_clicks(&self) -> bool {
        self.apply_vanilla_clicks
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

    /// Replaces the rules of the menu's slots: slots without a rule are `Normal` again.
    pub fn set_rules(&mut self, rules: &MenuClickRules) -> Result<(), String> {
        let slot_count = self.slot_count();
        for rule in &rules.slot_rules {
            if rule.slot >= slot_count {
                return Err(format!("the menu has no slot {}", rule.slot));
            }
            if rule.max_stack_size == Some(0) {
                return Err(format!("slot {} cannot have a stack size of 0", rule.slot));
            }
        }
        for range in &rules.quick_move_targets {
            if range.start >= range.end || range.end > slot_count {
                return Err(format!(
                    "{}..{} is not a range of the menu's {slot_count} slots",
                    range.start, range.end
                ));
            }
        }
        for slot in &self.menu_slots {
            slot.set_rule(MenuSlotKind::Normal, None);
        }
        for rule in &rules.slot_rules {
            self.menu_slots[rule.slot].set_rule(rule.kind.clone(), rule.max_stack_size);
        }
        self.quick_move_targets
            .clone_from(&rules.quick_move_targets);
        self.send_content_updates();
        Ok(())
    }

    /// The stack on the cursor.
    #[must_use]
    pub fn cursor(&self) -> ItemStack {
        self.behaviour
            .cursor_stack
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Replaces the stack on the cursor and sends it to the client. `false` if the menu has no
    /// sync handler (a menu without slots), which has no cursor to send.
    pub fn set_cursor(&mut self, stack: ItemStack) -> bool {
        if self.behaviour.sync_handler.is_none() {
            return false;
        }
        *self
            .behaviour
            .cursor_stack
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = stack;
        self.send_content_updates();
        true
    }

    fn menu_stacks(&self) -> Vec<ItemStack> {
        self.menu_slots
            .iter()
            .map(|slot| slot.get_stack())
            .collect()
    }

    /// Applies a click packet with the vanilla logic, the way `Player::on_slot_click` does for a
    /// window of its own, and returns the menu slots it changed.
    fn apply_click(
        &mut self,
        player: &Player,
        packet: &SClickSlot,
    ) -> (Vec<MenuSlotChange>, ItemStack) {
        let before = self.menu_stacks();
        let not_in_sync =
            packet.revision.0 != (self.behaviour.revision.load(Ordering::Relaxed) as i32);

        self.disable_sync();
        self.on_slot_click(
            i32::from(packet.slot),
            i32::from(packet.button),
            packet.mode.clone(),
            player,
        );
        for (slot, hash) in &packet.array_of_changed_slots {
            self.set_received_hash(*slot as usize, hash.clone());
        }
        self.set_received_cursor_hash(packet.carried_item.clone());
        self.enable_sync();

        if not_in_sync {
            self.update_to_client();
        } else {
            self.send_content_updates();
        }

        let changes = before
            .into_iter()
            .zip(self.menu_stacks())
            .enumerate()
            .filter(|(_, (previous, stack))| !previous.are_equal(stack))
            .map(|(slot, (previous, stack))| MenuSlotChange {
                slot: slot as u32,
                previous,
                stack,
            })
            .collect();
        (changes, self.cursor())
    }

    /// A click on a ghost slot, `false` if the slot is not one.
    fn click_ghost_slot(&mut self, slot_index: i32, button: i32, mode: &SlotActionType) -> bool {
        let Some(slot) = usize::try_from(slot_index)
            .ok()
            .and_then(|index| self.menu_slots.get(index))
        else {
            return false;
        };
        if slot.kind() != MenuSlotKind::Ghost {
            return false;
        }
        if *mode == SlotActionType::Pickup && (button == 0 || button == 1) {
            let cursor = self.cursor();
            let ghost = if cursor.is_empty() {
                ItemStack::EMPTY.clone()
            } else {
                let count = if button == 0 { cursor.item_count } else { 1 };
                cursor.copy_with_count(
                    count
                        .min(slot.get_max_item_count())
                        .min(cursor.get_max_stack_size()),
                )
            };
            slot.set_stack(ghost);
        }
        true
    }

    /// Moves `stack` into the quick move target ranges, skipping `skip`. Returns whether
    /// anything moved.
    fn insert_into_targets(&mut self, stack: &mut ItemStack, skip: Option<usize>) -> bool {
        let mut moved = false;
        for range in self.quick_move_targets.clone() {
            let parts = match skip {
                Some(skip) if range.contains(&skip) => [(range.start, skip), (skip + 1, range.end)],
                _ => [(range.start, range.end), (0, 0)],
            };
            for (start, end) in parts {
                if start < end && !stack.is_empty() {
                    moved |= self.insert_item(stack, start as i32, end as i32, false);
                }
            }
        }
        moved
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

    /// The vanilla logic, with the slots the client names that the server does not track (the
    /// player inventory of a menu that has none) ignored and ghost slots handled here.
    fn on_slot_click(
        &mut self,
        slot_index: i32,
        button: i32,
        action_type: SlotActionType,
        player: &dyn InventoryPlayer,
    ) {
        let in_range = slot_index == -1
            || slot_index == -999
            || (0..self.behaviour.slots.len() as i32).contains(&slot_index);
        if !in_range || self.click_ghost_slot(slot_index, button, &action_type) {
            return;
        }
        self.internal_on_slot_click(slot_index, button, action_type, player);
    }

    /// Vanilla shift-click: out of the menu into the player inventory slots (the end first), or
    /// into the quick move targets when there are none; from a player inventory slot into the
    /// quick move targets.
    fn quick_move(&mut self, _player: &dyn InventoryPlayer, slot_index: i32) -> ItemStack {
        let Some(slot) = usize::try_from(slot_index)
            .ok()
            .and_then(|index| self.behaviour.slots.get(index))
            .cloned()
        else {
            return ItemStack::EMPTY.clone();
        };
        if !slot.has_stack() {
            return ItemStack::EMPTY.clone();
        }
        let index = slot_index as usize;
        let menu_len = self.slot_count();
        let total = self.behaviour.slots.len();
        let mut slot_stack = slot.get_stack();
        let stack_left = slot_stack.clone();

        let moved = if index >= menu_len {
            self.insert_into_targets(&mut slot_stack, None)
        } else if total > menu_len {
            self.insert_item(&mut slot_stack, menu_len as i32, total as i32, true)
        } else {
            self.insert_into_targets(&mut slot_stack, Some(index))
        };
        if !moved {
            return ItemStack::EMPTY.clone();
        }
        if slot_stack.is_empty() {
            slot.set_stack(ItemStack::EMPTY.clone());
        } else {
            slot.set_stack(slot_stack);
        }
        stack_left
    }

    /// Gives the cursor stack back to the player, like every window does.
    fn on_closed(&mut self, player: &dyn InventoryPlayer) {
        self.default_on_closed(player);
    }
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
        self.open_plugin_menu_with_options(
            menu_type,
            title,
            extra_data,
            slot_count,
            &PluginMenuOptions::default(),
        )
    }

    /// Like `open_plugin_menu`, with the options of the menu.
    pub fn open_plugin_menu_with_options(
        &self,
        menu_type: &'static PluginMenuType,
        title: &TextComponent,
        extra_data: Option<&[u8]>,
        slot_count: usize,
        options: &PluginMenuOptions,
    ) -> Result<u32, String> {
        let ClientPlatform::Java(client) = self.client.as_ref() else {
            return Err("menus of plugin types are only available to Java players".to_string());
        };
        let total_slots = slot_count
            + if options.player_inventory {
                PLAYER_INVENTORY_SLOTS
            } else {
                0
            };
        if total_slots > MAX_PLUGIN_MENU_SLOTS {
            return Err(format!(
                "a plugin menu has at most {MAX_PLUGIN_MENU_SLOTS} slots, the player inventory included"
            ));
        }
        let payload = extra_data.map(|extra_data| (extra_data.to_vec(), client.version.load()));

        let player_inventory: Arc<dyn Inventory> = InventoryPlayer::get_inventory(self);
        // Built before the open window closes, so bad rules leave it alone. The id is set below.
        let mut menu = PluginMenuScreenHandler::new(
            0,
            menu_type,
            slot_count,
            options,
            options.player_inventory.then_some(&player_inventory),
        )?;

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
        menu.get_behaviour_mut().sync_id = sync_id;
        let handler: Arc<std::sync::Mutex<dyn ScreenHandler>> =
            Arc::new(std::sync::Mutex::new(menu));

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
        if total_slots > 0 {
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

    /// Replaces the rules of the open plugin menu's slots.
    pub fn set_plugin_menu_rules(&self, rules: &MenuClickRules) -> Result<(), String> {
        self.with_plugin_menu(|menu| menu.set_rules(rules))
            .unwrap_or_else(|| Err("the player has no plugin menu open".to_string()))
    }

    /// The stack on the cursor of the open plugin menu.
    #[must_use]
    pub fn plugin_menu_cursor(&self) -> Option<ItemStack> {
        self.with_plugin_menu(|menu| menu.cursor())
    }

    /// Replaces the stack on the cursor of the open plugin menu and sends it to the client.
    pub fn set_plugin_menu_cursor(&self, stack: ItemStack) -> Result<(), String> {
        match self.with_plugin_menu(|menu| menu.set_cursor(stack)) {
            Some(true) => Ok(()),
            Some(false) => Err("a menu without slots has no cursor".to_string()),
            None => Err("the player has no plugin menu open".to_string()),
        }
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

/// The mode of a click packet.
const fn click_mode(mode: &SlotActionType) -> MenuClickMode {
    match mode {
        SlotActionType::Pickup => MenuClickMode::Pickup,
        SlotActionType::QuickMove => MenuClickMode::QuickMove,
        SlotActionType::Swap => MenuClickMode::Swap,
        SlotActionType::Clone => MenuClickMode::Clone,
        SlotActionType::Throw => MenuClickMode::Throw,
        SlotActionType::QuickCraft => MenuClickMode::QuickCraft,
        SlotActionType::PickupAll => MenuClickMode::PickupAll,
    }
}

impl Player {
    /// Routes a click packet to the plugin when the player's open window is a plugin menu. Returns
    /// whether it was, in which case the generic click handling does not run.
    ///
    /// The plugin hears about the click as a `MenuClickEvent`. A menu that applies vanilla clicks
    /// then applies the click (unless the event was cancelled) and the plugin hears about the
    /// changes as a `MenuSlotChangedEvent`. Any other menu, or a cancelled click, gets the slots
    /// and the cursor sent to the client again, so what the client predicted is reverted unless
    /// the plugin changed them.
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
        let (container_id, menu_type, applies, cursor_stack) = {
            let guard = handler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(menu) = guard.as_any().downcast_ref::<PluginMenuScreenHandler>() else {
                return false;
            };
            (
                menu.sync_id(),
                menu.menu_type().key,
                menu.applies_vanilla_clicks(),
                menu.cursor(),
            )
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
            mode: click_mode(&packet.mode),
            changed_slots,
            carried_item_id,
            carried_count,
            cursor_stack,
            cancelled: false,
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
        if !still_open {
            return true;
        }

        let applied = if applies && !event.cancelled && !self.is_spectator() {
            let mut guard = handler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .as_any_mut()
                .downcast_mut::<PluginMenuScreenHandler>()
                .map(|menu| menu.apply_click(self, packet))
        } else {
            None
        };
        let Some((changes, cursor_stack)) = applied else {
            handler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .sync_state();
            return true;
        };

        // The handler is unlocked, so the plugin can use the menu in its handler.
        if !changes.is_empty() || !event.cursor_stack.are_equal(&cursor_stack) {
            let mut changed = MenuSlotChangedEvent {
                player: self.clone(),
                container_id: u32::from(container_id),
                menu_type: menu_type.to_string(),
                changes,
                cursor_stack,
            };
            server.plugin_manager.fire_blocking(server, &mut changed);
        }
        true
    }

    /// Tells the plugin about a button click when the player's open window is a plugin menu.
    /// Returns whether it was one.
    pub(crate) fn route_plugin_menu_button_click(&self, packet: &SContainerButtonClick) -> bool {
        let Some((container_id, menu_type, _)) = self.open_plugin_menu_info() else {
            return false;
        };
        if i32::from(container_id) != packet.window_id.0 {
            return true;
        }
        let world = self.living_entity.entity.world.load();
        let Some(server) = world.server.upgrade() else {
            return true;
        };
        let Some(player) = world.get_player_by_uuid(self.gameprofile.id) else {
            return true;
        };
        let mut event = MenuButtonClickEvent {
            player,
            container_id: u32::from(container_id),
            menu_type: menu_type.key.to_string(),
            button_id: packet.button_id.0,
        };
        server.plugin_manager.fire_blocking(&server, &mut event);
        true
    }
}
