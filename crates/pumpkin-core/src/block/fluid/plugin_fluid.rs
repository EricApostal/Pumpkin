//! Placing and picking up fluids for plugins, on top of the bucket logic.

use std::sync::Arc;

use pumpkin_data::{Block, fluid::Fluid};
use pumpkin_util::math::position::BlockPos;
use pumpkin_world::{tick::TickPriority, world::BlockFlags};

use crate::item::items::bucket::try_pickup_fluid_at;
use crate::world::World;

/// Places a source of `fluid` at `pos`, waterlogging a block that can hold water. Fails when the
/// spot is taken or the fluid has no block (virtual fluids and flowing variants).
pub fn try_place_fluid(world: &Arc<World>, pos: BlockPos, fluid: &Fluid) -> bool {
    let fluid = fluid.to_still();
    let (block, state) = world.get_block_and_state(&pos);

    let source = if fluid.matches_type(&Fluid::WATER) {
        if block.is_waterloggable() && !block.is_waterlogged(state.id) {
            let state_id = block.set_waterlogged(state.id, true).unwrap_or(state.id);
            world.set_block_state(&pos, state_id, BlockFlags::NOTIFY_ALL);
            world.schedule_fluid_tick(&Fluid::WATER, pos, 5, TickPriority::Normal);
            return true;
        }
        Block::WATER.default_state.id
    } else if fluid.matches_type(&Fluid::LAVA) {
        Block::LAVA.default_state.id
    } else {
        let Some(info) = fluid.dynamic_info() else {
            return false;
        };
        let Some(block) = info.block.filter(|_| !info.holds_flowing()) else {
            return false;
        };
        block.default_state.id
    };

    if !(state.is_air() || state.is_liquid() || state.replaceable()) {
        return false;
    }
    if world.get_block_state_id(&pos) == source {
        return false;
    }
    world.set_block_state(&pos, source, BlockFlags::NOTIFY_ALL);
    true
}

/// Removes the source of a fluid at `pos` and returns the still fluid. Handles waterlogged
/// blocks, water, lava and sources of plugin fluids.
pub fn try_pickup_fluid(world: &Arc<World>, pos: BlockPos) -> Option<&'static Fluid> {
    let state_id = world.get_block_state_id(&pos);
    if let Some(fluid) = Fluid::from_state_id(state_id).filter(|fluid| fluid.is_dynamic()) {
        let (_, state) = Fluid::dynamic_fluid_state(state_id)?;
        if !state.is_source {
            return None;
        }
        world.set_block_state(
            &pos,
            Block::AIR.default_state.id,
            BlockFlags::NOTIFY_ALL,
        );
        return Some(fluid.to_still());
    }
    // Powder snow has a bucket but no fluid.
    if world.get_block(&pos) == &Block::POWDER_SNOW {
        return None;
    }
    let item = try_pickup_fluid_at(world, pos)?;
    Fluid::from_bucket_item(item)
}
