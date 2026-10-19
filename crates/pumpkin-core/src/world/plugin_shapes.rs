//! Answers of plugins for custom blocks that depend on the position, not only on the state.
//!
//! These are the
//! shapes of blocks with the `dynamic_shape` callback and the physics values of blocks with the
//! friction, speed and jump factor callbacks. They are asked once per position and kept until
//! [`World::invalidate_block_shape`] or until the block at the position changes.

use std::sync::Arc;

use dashmap::DashMap;
use pumpkin_data::block_registry::{BlockCallbacks, ShapeKind};
use pumpkin_data::{Block, BlockState, BlockStateId};
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;

use crate::block::blocks::dynamic;
use crate::plugin::block::CustomBlockPhysicsKind;

use super::World;

/// The callbacks whose answers are cached here.
pub const CACHED_CALLBACKS: BlockCallbacks = BlockCallbacks::DYNAMIC_SHAPE
    .union(BlockCallbacks::GET_FRICTION)
    .union(BlockCallbacks::GET_SPEED_FACTOR)
    .union(BlockCallbacks::GET_JUMP_FACTOR);

const fn shape_slot(kind: ShapeKind) -> usize {
    match kind {
        ShapeKind::Collision => 0,
        ShapeKind::Selection => 1,
        ShapeKind::Visual => 2,
    }
}

const fn physics_slot(kind: CustomBlockPhysicsKind) -> usize {
    match kind {
        CustomBlockPhysicsKind::Friction => 0,
        CustomBlockPhysicsKind::SpeedFactor => 1,
        CustomBlockPhysicsKind::JumpFactor => 2,
    }
}

/// What was asked for the block at one position.
#[derive(Default)]
struct Entry {
    /// The state the answers are for, a different state at the position makes them stale.
    state: Option<BlockStateId>,
    shapes: [Option<Arc<[BoundingBox]>>; 3],
    physics: [Option<f32>; 3],
}

/// The cached answers of plugins, by position.
#[derive(Default)]
pub struct PluginBlockCache(DashMap<BlockPos, Entry>);

impl PluginBlockCache {
    /// The cached shape of the block at `position` in `state`.
    #[must_use]
    pub fn shape(
        &self,
        position: &BlockPos,
        state: BlockStateId,
        kind: ShapeKind,
    ) -> Option<Arc<[BoundingBox]>> {
        let entry = self.0.get(position)?;
        if entry.state != Some(state) {
            return None;
        }
        entry.shapes[shape_slot(kind)].clone()
    }

    pub fn store_shape(
        &self,
        position: &BlockPos,
        state: BlockStateId,
        kind: ShapeKind,
        shapes: Arc<[BoundingBox]>,
    ) {
        let mut entry = self.0.entry(*position).or_default();
        if entry.state != Some(state) {
            *entry = Entry {
                state: Some(state),
                ..Entry::default()
            };
        }
        entry.shapes[shape_slot(kind)] = Some(shapes);
    }

    /// The cached physics value of the block at `position` in `state`.
    #[must_use]
    pub fn physics(
        &self,
        position: &BlockPos,
        state: BlockStateId,
        kind: CustomBlockPhysicsKind,
    ) -> Option<f32> {
        let entry = self.0.get(position)?;
        if entry.state != Some(state) {
            return None;
        }
        entry.physics[physics_slot(kind)]
    }

    pub fn store_physics(
        &self,
        position: &BlockPos,
        state: BlockStateId,
        kind: CustomBlockPhysicsKind,
        value: f32,
    ) {
        let mut entry = self.0.entry(*position).or_default();
        if entry.state != Some(state) {
            *entry = Entry {
                state: Some(state),
                ..Entry::default()
            };
        }
        entry.physics[physics_slot(kind)] = Some(value);
    }

    /// Forgets everything cached for `position`.
    pub fn invalidate(&self, position: &BlockPos) {
        self.0.remove(position);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.len()
    }
}

impl World {
    /// Forgets the shapes and physics values that plugins gave for the block at `position`, so
    /// they are asked for again. Call it when the answer of a `dynamic_shape` block changes.
    pub fn invalidate_block_shape(&self, position: &BlockPos) {
        self.plugin_block_cache.invalidate(position);
    }

    /// The shape that a plugin gives for a custom block with the `dynamic_shape` callback, in
    /// block units. `None` for every other block (and when no plugin listens), then the shapes of
    /// the state apply.
    #[must_use]
    pub fn dynamic_block_shapes(
        &self,
        position: &BlockPos,
        state: &BlockState,
        kind: ShapeKind,
    ) -> Option<Arc<[BoundingBox]>> {
        // Vanilla blocks are the bulk of the lookups, they have no hooks.
        if state.id.as_u32() < u32::from(Block::vanilla_state_count()) {
            return None;
        }
        dynamic::dynamic_shapes(self, position, state, kind)
    }

    /// Like [`Self::dynamic_block_shapes`], but only what is already cached: it never asks a
    /// plugin, so it is safe to call from inside a plugin call.
    #[must_use]
    pub fn cached_block_shapes(
        &self,
        position: &BlockPos,
        state: &BlockState,
        kind: ShapeKind,
    ) -> Option<Arc<[BoundingBox]>> {
        if state.id.as_u32() < u32::from(Block::vanilla_state_count()) {
            return None;
        }
        self.plugin_block_cache.shape(position, state.id, kind)
    }

    /// The friction, speed factor or jump factor of the block at `position`: the answer of the
    /// plugin for a custom block with the matching callback, else the value of `block`.
    #[must_use]
    pub fn block_physics(
        &self,
        position: &BlockPos,
        block: &Block,
        state: &BlockState,
        kind: CustomBlockPhysicsKind,
    ) -> f32 {
        let default = match kind {
            CustomBlockPhysicsKind::Friction => block.slipperiness,
            CustomBlockPhysicsKind::SpeedFactor => block.velocity_multiplier,
            CustomBlockPhysicsKind::JumpFactor => block.jump_velocity_multiplier,
        };
        if state.id.as_u32() < u32::from(Block::vanilla_state_count()) {
            return default;
        }
        dynamic::physics_value(self, position, block, state, kind).unwrap_or(default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shapes() -> Arc<[BoundingBox]> {
        Arc::from(vec![BoundingBox::full_block()])
    }

    #[test]
    fn answers_are_cached_per_position_and_state() {
        let cache = PluginBlockCache::default();
        let pos = BlockPos::new(1, 2, 3);
        let state = BlockStateId::from_raw_or_air(1);
        let other = BlockStateId::from_raw_or_air(2);

        assert!(cache.shape(&pos, state, ShapeKind::Collision).is_none());
        cache.store_shape(&pos, state, ShapeKind::Collision, shapes());
        cache.store_physics(&pos, state, CustomBlockPhysicsKind::Friction, 0.8);
        assert!(cache.shape(&pos, state, ShapeKind::Collision).is_some());
        // Each kind is its own answer.
        assert!(cache.shape(&pos, state, ShapeKind::Selection).is_none());
        assert_eq!(
            cache.physics(&pos, state, CustomBlockPhysicsKind::Friction),
            Some(0.8)
        );
        assert_eq!(
            cache.physics(&pos, state, CustomBlockPhysicsKind::SpeedFactor),
            None
        );
        // Another state at the position never reads the old answers.
        assert!(cache.shape(&pos, other, ShapeKind::Collision).is_none());
        cache.store_shape(&pos, other, ShapeKind::Collision, shapes());
        assert!(cache.shape(&pos, state, ShapeKind::Collision).is_none());
        assert_eq!(
            cache.physics(&pos, other, CustomBlockPhysicsKind::Friction),
            None
        );

        cache.invalidate(&pos);
        assert!(cache.shape(&pos, other, ShapeKind::Collision).is_none());
        assert_eq!(cache.len(), 0);
    }
}
