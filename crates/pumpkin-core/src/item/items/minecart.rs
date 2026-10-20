use std::sync::Arc;

use crate::block::registry::BlockActionResult;
use crate::entity::Entity;
use crate::entity::player::Player;
use crate::entity::vehicle::minecart::{MinecartEntity, rail_shape};
use crate::item::{ItemBehaviour, ItemMetadata};
use crate::server::Server;
use pumpkin_data::BlockDirection;
use pumpkin_data::block_properties::RailShape;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::tag::Taggable;
use pumpkin_data::{Block, tag};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

/// Vanilla `MinecartItem.useOn` spawn position
/// (`MinecartItem.java:41-45`): `new Vec3(pos.getX() + 0.5, pos.getY() + 0.0625 + offset, pos.getZ() + 0.5)`,
/// where `offset` is `0.5` for a sloped rail and `0.0` otherwise.
#[must_use]
pub fn minecart_spawn_position(pos: &BlockPos, ascending: bool) -> Vector3<f64> {
    let height = if ascending { 0.5 } else { 0.0 };
    Vector3::new(
        f64::from(pos.0.x) + 0.5,
        f64::from(pos.0.y) + 0.0625 + height,
        f64::from(pos.0.z) + 0.5,
    )
}

pub struct MinecartItem;

impl MinecartItem {
    pub(crate) fn item_to_entity(item: &Item) -> &'static EntityType {
        match item.id {
            val if val == Item::MINECART.id => &EntityType::MINECART,
            val if val == Item::TNT_MINECART.id => &EntityType::TNT_MINECART,
            val if val == Item::CHEST_MINECART.id => &EntityType::CHEST_MINECART,
            val if val == Item::HOPPER_MINECART.id => &EntityType::HOPPER_MINECART,
            val if val == Item::FURNACE_MINECART.id => &EntityType::FURNACE_MINECART,
            val if val == Item::COMMAND_BLOCK_MINECART.id => &EntityType::COMMAND_BLOCK_MINECART,
            _ => {
                tracing::error!("Unknown minecart item ID: {}", item.id);
                &EntityType::MINECART
            }
        }
    }
}

impl ItemMetadata for MinecartItem {
    fn ids() -> Box<[u16]> {
        [
            Item::MINECART.id,
            Item::TNT_MINECART.id,
            Item::CHEST_MINECART.id,
            Item::HOPPER_MINECART.id,
            Item::FURNACE_MINECART.id,
            Item::COMMAND_BLOCK_MINECART.id,
        ]
        .into()
    }
}

impl ItemBehaviour for MinecartItem {
    fn use_on_block(
        &self,
        item: &mut ItemStack,
        player: &Player,
        location: BlockPos,
        _face: BlockDirection,
        _cursor_pos: Vector3<f32>,
        block: &Block,
        _server: &Server,
    ) -> BlockActionResult {
        let world = player.world();

        if !block.has_tag(&tag::Block::MINECRAFT_RAILS) {
            return BlockActionResult::Fail;
        }
        let state_id = world.get_block_state_id(&location);
        let shape = rail_shape(block, state_id).unwrap_or(RailShape::NorthSouth);
        let entity_type = Self::item_to_entity(item.item);
        let spawn_pos = minecart_spawn_position(&location, shape.is_ascending());
        let entity = Entity::new(world.clone(), spawn_pos, entity_type);
        let minecart_entity = Arc::new(MinecartEntity::new(entity));
        world.spawn_entity(minecart_entity);
        item.decrement_unless_creative(player.gamemode.load(), 1);
        BlockActionResult::Success
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_position_is_centred_on_the_rail_block() {
        let pos = BlockPos::new(10, 64, -3);

        let flat = minecart_spawn_position(&pos, false);
        assert_eq!(flat, Vector3::new(10.5, 64.0625, -2.5));

        let slope = minecart_spawn_position(&pos, true);
        assert_eq!(slope, Vector3::new(10.5, 64.5625, -2.5));
    }
}
