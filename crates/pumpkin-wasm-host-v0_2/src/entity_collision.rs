//! The `entity-collision` interface: boxes that host entities collide with.
//!
//! The registry and the collision math live in [`pumpkin_core::plugin_collision`]; the movement code
//! reads the boxes from there. This module only converts the interface types and keeps the boxes of
//! the calling plugin apart from those of other plugins.

use pumpkin_core::plugin_collision;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_wasm_host_common::state::PluginHostState;
use wasmtime::component::Resource;

use crate::chunk_tickets::plugin_owner;
use crate::pumpkin::plugin::entity_collision::Host;
use crate::pumpkin::plugin::world::{BoundingBox as WitBoundingBox, World};

/// Converts a box of the plugin API to a world box, with the corners in min/max order.
///
/// A plugin may give the corners in any order; the host sorts them so the collision test does not
/// have to.
fn to_internal_box(bounding_box: WitBoundingBox) -> BoundingBox {
    let (min, max) = (bounding_box.min, bounding_box.max);
    BoundingBox::new(
        Vector3::new(min.0.min(max.0), min.1.min(max.1), min.2.min(max.2)),
        Vector3::new(min.0.max(max.0), min.1.max(max.1), min.2.max(max.2)),
    )
}

impl Host for PluginHostState {
    fn set_entity_collision_boxes(
        &mut self,
        world: Resource<World>,
        boxes: Vec<WitBoundingBox>,
    ) -> wasmtime::Result<()> {
        let world = self.get(&world)?.clone();
        let owner = plugin_owner(self);
        plugin_collision::set_boxes(&world, &owner, boxes.into_iter().map(to_internal_box).collect());
        Ok(())
    }

    fn clear_entity_collision_boxes(&mut self, world: Resource<World>) -> wasmtime::Result<()> {
        let world = self.get(&world)?.clone();
        let owner = plugin_owner(self);
        plugin_collision::clear_boxes(&world, &owner);
        Ok(())
    }
}
