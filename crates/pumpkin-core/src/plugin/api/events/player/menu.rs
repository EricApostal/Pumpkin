use std::sync::Arc;

use crate::entity::player::Player;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_inventory::screen_handler::ClickType;
use pumpkin_macros::{Event, cancellable};

use super::PlayerEvent;

/// A slot the client changed in a click. The server only gets a hash of the stack the client has
/// there, so this is the item and the count.
#[derive(Clone, Debug)]
pub struct MenuChangedSlot {
    pub slot: i16,
    /// The item id, `None` for an empty slot.
    pub item_id: Option<u32>,
    pub count: u32,
}

/// The mode of a container click packet, the raw `ClickType` of vanilla.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuClickMode {
    Pickup,
    QuickMove,
    Swap,
    Clone,
    Throw,
    /// A drag, in three packets: start (button 0, 4 or 8), one per slot (1, 5, 9), end (2, 6, 10).
    QuickCraft,
    PickupAll,
}

/// A player clicked in a menu of a plugin registered type.
///
/// Fired before the click does anything, so cancelling it drops the click. In a menu that does not
/// apply vanilla clicks the server never applies it: after the event it resends the slots of the
/// menu, so the client's prediction is reverted unless the plugin changed them. In a menu that
/// does, the click is applied after the event (unless it is cancelled) and `MenuSlotChangedEvent`
/// reports what it changed.
#[cancellable]
#[derive(Event, Clone)]
pub struct MenuClickEvent {
    /// The player who clicked.
    pub player: Arc<Player>,

    /// The container id of the menu.
    pub container_id: u32,

    /// The full key of the menu type.
    pub menu_type: String,

    /// The slot that was clicked, `-1` for the cursor and `-999` for outside of the window.
    pub slot: i16,

    /// The button of the click.
    pub button: i8,

    /// The type of the click, derived from the mode and the button.
    pub click_type: ClickType,

    /// The mode of the click packet.
    pub mode: MenuClickMode,

    /// The slots the client says it changed.
    pub changed_slots: Vec<MenuChangedSlot>,

    /// The item id of the stack the client has on its cursor, `None` if it is empty.
    pub carried_item_id: Option<u32>,

    /// The count of the stack the client has on its cursor.
    pub carried_count: u32,

    /// The server's cursor stack of the menu before the click, with its components.
    pub cursor_stack: ItemStack,
}

impl PlayerEvent for MenuClickEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}

/// A slot of a menu a click changed.
#[derive(Clone, Debug)]
pub struct MenuSlotChange {
    pub slot: u32,
    /// The stack the slot held before the click.
    pub previous: ItemStack,
    /// The stack the slot holds now.
    pub stack: ItemStack,
}

/// A click changed slots of a menu that applies vanilla clicks, or its cursor.
///
/// Fired after the click, so the plugin can mirror the state. Only the menu's own slots are
/// reported, not the player inventory slots of a menu that has them. Never fired for changes the
/// plugin makes itself.
#[derive(Event, Clone)]
pub struct MenuSlotChangedEvent {
    /// The player who clicked.
    pub player: Arc<Player>,

    /// The container id of the menu.
    pub container_id: u32,

    /// The full key of the menu type.
    pub menu_type: String,

    /// The slots that changed.
    pub changes: Vec<MenuSlotChange>,

    /// The cursor stack after the click.
    pub cursor_stack: ItemStack,
}

impl PlayerEvent for MenuSlotChangedEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}

/// A player clicked a button of a menu of a plugin registered type (`ServerboundContainerButtonClick`,
/// what the buttons of an enchanting table send). The server does nothing with it.
#[derive(Event, Clone)]
pub struct MenuButtonClickEvent {
    /// The player who clicked.
    pub player: Arc<Player>,

    /// The container id of the menu.
    pub container_id: u32,

    /// The full key of the menu type.
    pub menu_type: String,

    /// The id of the button.
    pub button_id: i32,
}

impl PlayerEvent for MenuButtonClickEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}

/// A player's menu of a plugin registered type closed: the player closed it, the plugin closed it,
/// or another screen opened.
#[derive(Event, Clone)]
pub struct MenuClosedEvent {
    /// The player whose menu closed.
    pub player: Arc<Player>,

    /// The container id of the menu.
    pub container_id: u32,

    /// The full key of the menu type.
    pub menu_type: String,
}

impl MenuClosedEvent {
    #[must_use]
    pub fn new(player: &Arc<Player>, container_id: u32, menu_type: String) -> Self {
        Self {
            player: Arc::clone(player),
            container_id,
            menu_type,
        }
    }
}

impl PlayerEvent for MenuClosedEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}
