use std::sync::Arc;

use crate::entity::player::Player;
use pumpkin_data::BlockDirection;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_macros::{Event, cancellable};
use pumpkin_util::Hand;
use pumpkin_util::math::position::BlockPos;

use super::PlayerEvent;

/// A player used an item: a right click with an item in hand, in the air or on a block.
///
/// Fired before the server uses the item, so cancelling it stops the use, and a block from being
/// placed. A click on a block fires [`PlayerUseBlockEvent`](super::PlayerUseBlockEvent) first and
/// then this event with the clicked position, if that was not cancelled and the block did not
/// handle the click. A client also sends a use in the air after a click on a block that did not
/// handle it, which fires this event again without a position.
#[cancellable]
#[derive(Event, Clone)]
pub struct PlayerUseItemEvent {
    /// The player who used the item.
    pub player: Arc<Player>,

    /// The hand the item is in.
    pub hand: Hand,

    /// A copy of the item that is used.
    pub item: ItemStack,

    /// The block that was clicked, `None` for a use in the air.
    pub clicked_pos: Option<BlockPos>,

    /// The face of the block that was clicked, `None` for a use in the air.
    pub face: Option<BlockDirection>,
}

impl PlayerUseItemEvent {
    #[must_use]
    pub fn new(
        player: &Arc<Player>,
        hand: Hand,
        item: ItemStack,
        clicked_pos: Option<BlockPos>,
        face: Option<BlockDirection>,
    ) -> Self {
        Self {
            player: Arc::clone(player),
            hand,
            item,
            clicked_pos,
            face,
            cancelled: false,
        }
    }
}

impl PlayerEvent for PlayerUseItemEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}
