use std::sync::Arc;

use crate::entity::player::Player;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::{Block, BlockDirection};
use pumpkin_macros::{Event, cancellable};
use pumpkin_util::Hand;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

use super::PlayerEvent;

/// A player right clicked a block, with whatever is in the given hand (possibly nothing).
///
/// Fired before the block handles the click and before the held item is used or placed, so
/// cancelling it stops all of that. The server resends the clicked block and the block next to it,
/// so the client does not keep a block it predicted.
#[cancellable]
#[derive(Event, Clone)]
pub struct PlayerUseBlockEvent {
    /// The player who clicked.
    pub player: Arc<Player>,

    /// The hand of the click.
    pub hand: Hand,

    /// A copy of the item in that hand, an empty stack for an empty hand.
    pub item: ItemStack,

    /// The position of the clicked block.
    pub position: BlockPos,

    /// The clicked block.
    pub block: &'static Block,

    /// The face that was clicked.
    pub face: BlockDirection,

    /// Where on the block the click hit, `0..1` on each axis from the block's minimum corner.
    pub cursor: Vector3<f32>,

    /// Whether the player was sneaking.
    pub sneaking: bool,
}

impl PlayerUseBlockEvent {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        player: &Arc<Player>,
        hand: Hand,
        item: ItemStack,
        position: BlockPos,
        block: &'static Block,
        face: BlockDirection,
        cursor: Vector3<f32>,
        sneaking: bool,
    ) -> Self {
        Self {
            player: Arc::clone(player),
            hand,
            item,
            position,
            block,
            face,
            cursor,
            sneaking,
            cancelled: false,
        }
    }
}

impl PlayerEvent for PlayerUseBlockEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}
