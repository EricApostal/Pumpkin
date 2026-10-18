use crate::entity::player::Player;
use pumpkin_macros::Event;
use std::sync::Arc;

/// An event that occurs when items are placed into a smithing table.
///
/// A plugin can replace the result by changing `result_item` (the count of the computed result is
/// kept for the same item, a different item gets a count of 1) or strip it by setting none.
#[derive(Event, Clone)]
pub struct PrepareSmithingEvent {
    /// The player using the smithing table.
    pub player: Arc<Player>,

    /// The resulting item ID prepared in the output slot.
    pub result_item: Option<String>,

    /// The template item ID, none when the slot is empty.
    pub template_item: Option<String>,

    /// The base item ID, none when the slot is empty.
    pub base_item: Option<String>,

    /// The addition item ID, none when the slot is empty.
    pub addition_item: Option<String>,
}

impl PrepareSmithingEvent {
    #[must_use]
    pub const fn new(
        player: Arc<Player>,
        result_item: Option<String>,
        template_item: Option<String>,
        base_item: Option<String>,
        addition_item: Option<String>,
    ) -> Self {
        Self {
            player,
            result_item,
            template_item,
            base_item,
            addition_item,
        }
    }
}
