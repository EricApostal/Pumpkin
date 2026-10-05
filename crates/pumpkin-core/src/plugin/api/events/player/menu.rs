use std::sync::Arc;

use crate::entity::player::Player;
use pumpkin_inventory::screen_handler::ClickType;
use pumpkin_macros::Event;

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

/// A player clicked in a menu of a plugin registered type.
///
/// The server does not apply the click, the menu belongs to the plugin. After the event it resends
/// the slots of the menu, so the client's prediction is reverted unless the plugin changed them.
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

    /// The type of the click.
    pub click_type: ClickType,

    /// The slots the client says it changed.
    pub changed_slots: Vec<MenuChangedSlot>,

    /// The item id of the stack the client has on its cursor, `None` if it is empty.
    pub carried_item_id: Option<u32>,

    /// The count of the stack the client has on its cursor.
    pub carried_count: u32,
}

impl PlayerEvent for MenuClickEvent {
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
