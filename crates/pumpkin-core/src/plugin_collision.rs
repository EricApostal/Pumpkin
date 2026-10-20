//! Collision boxes that plugins add to the world.
//!
//! Host entities collide with the blocks of the world and with nothing else. A plugin that owns
//! geometry which is not a block of the world — a moving structure whose blocks live at a distant
//! position, a hull, a platform — supplies its boxes in world space every tick with the
//! `entity-collision` interface of the plugin API. This module keeps them and hands them to the
//! movement code.
//!
//! The boxes are axis aligned: a plugin that rotates its geometry transforms the corners itself and
//! gives the host the world aligned bounds, so a rotated shape is approximated by its bounding box.
//!
//! # HOST GAP
//! The boxes are used by entity movement: [`correct_player_position`] for players and the block
//! collision adjustment of `Entity` for server moved entities. They are not blocks: they
//! do not appear in `World::get_block_state`, in `is_space_empty` (block placement, entity spawn
//! checks), in the block hits of projectiles, in explosions, in `tick_block_collisions` (suffocation
//! and block effects) or in the collision of vehicles and their passengers. A plugin that needs
//! those to see its geometry has to place real blocks for it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};

use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::vector3::{Axis, Vector3};
use uuid::Uuid;

use crate::entity::Entity;
use crate::world::World;

/// How far a box is pushed out of another one beyond the overlap, in blocks. Without it a box that
/// is pushed exactly onto the surface still counts as touching after rounding.
const PUSH_EPSILON: f64 = 1.0e-3;

/// How often a box may be pushed in one call. Two boxes that push in opposite directions need more
/// than one pass; after this many the remaining overlap is accepted instead of looping.
const RESOLVE_PASSES: usize = 4;

/// The boxes of one plugin, in one world.
struct WorldBoxes {
    /// The world the entry belongs to, to drop the entry when the world unloads.
    world: Weak<World>,
    by_owner: HashMap<String, Vec<BoundingBox>>,
}

/// The collision boxes of every plugin, by world.
pub struct CollisionBoxRegistry {
    worlds: HashMap<Uuid, WorldBoxes>,
}

impl Default for CollisionBoxRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CollisionBoxRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            worlds: HashMap::new(),
        }
    }

    /// Replaces the boxes of `owner` in the world `id`. An empty list removes them.
    pub fn set(&mut self, id: Uuid, world: Weak<World>, owner: &str, boxes: Vec<BoundingBox>) {
        if boxes.is_empty() {
            self.clear(id, owner);
            return;
        }
        let entry = self.worlds.entry(id).or_insert_with(|| WorldBoxes {
            world: world.clone(),
            by_owner: HashMap::new(),
        });
        entry.world = world;
        entry.by_owner.insert(owner.to_string(), boxes);
    }

    /// Removes the boxes of `owner` in the world `id`.
    pub fn clear(&mut self, id: Uuid, owner: &str) {
        let Some(entry) = self.worlds.get_mut(&id) else {
            return;
        };
        entry.by_owner.remove(owner);
        if entry.by_owner.is_empty() {
            self.worlds.remove(&id);
        }
    }

    /// Removes the boxes of `owner` in every world.
    pub fn remove_owner(&mut self, owner: &str) {
        self.worlds.retain(|_, entry| {
            entry.by_owner.remove(owner);
            !entry.by_owner.is_empty()
        });
    }

    /// Removes every box of the world `id`.
    pub fn remove_world(&mut self, id: Uuid) {
        self.worlds.remove(&id);
    }

    /// Collects the boxes in the world `id` that touch `area`.
    #[must_use]
    pub fn boxes(&self, id: Uuid, area: &BoundingBox) -> Vec<BoundingBox> {
        let Some(entry) = self.worlds.get(&id) else {
            return Vec::new();
        };
        let mut boxes = Vec::new();
        for owned in entry.by_owner.values() {
            boxes.extend(owned.iter().filter(|b| b.intersects(area)).copied());
        }
        boxes
    }

    #[must_use]
    pub fn is_empty(&self, id: Uuid) -> bool {
        self.worlds.get(&id).is_none_or(|entry| entry.by_owner.is_empty())
    }

    #[must_use]
    pub fn total_boxes(&self) -> usize {
        self.worlds
            .values()
            .flat_map(|entry| entry.by_owner.values())
            .map(Vec::len)
            .sum()
    }

    /// Drops the entries of worlds that no longer exist.
    pub fn prune(&mut self) {
        self.worlds.retain(|_, entry| entry.world.strong_count() > 0);
    }
}

static REGISTRY: LazyLock<Mutex<CollisionBoxRegistry>> =
    LazyLock::new(|| Mutex::new(CollisionBoxRegistry::new()));

/// Number of registered boxes, so entity movement skips the registry lock when no plugin uses it.
static BOX_COUNT: AtomicUsize = AtomicUsize::new(0);

fn registry() -> std::sync::MutexGuard<'static, CollisionBoxRegistry> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

fn recount(registry: &CollisionBoxRegistry) {
    BOX_COUNT.store(registry.total_boxes(), Ordering::Relaxed);
}

/// Replaces the collision boxes of `owner` in `world`.
pub fn set_boxes(world: &Arc<World>, owner: &str, boxes: Vec<BoundingBox>) {
    let mut registry = registry();
    registry.prune();
    registry.set(world.uuid, Arc::downgrade(world), owner, boxes);
    recount(&registry);
}

/// Removes the collision boxes of `owner` in `world`.
pub fn clear_boxes(world: &World, owner: &str) {
    let mut registry = registry();
    registry.clear(world.uuid, owner);
    recount(&registry);
}

/// Removes the collision boxes of `owner` in every world, for example when the plugin unloads.
pub fn release_owner(owner: &str) {
    let mut registry = registry();
    registry.remove_owner(owner);
    recount(&registry);
}

/// Removes every collision box of `world`, for example when the world unloads.
pub fn release_world(world: &World) {
    let mut registry = registry();
    registry.remove_world(world.uuid);
    recount(&registry);
}

/// Returns whether no plugin added a collision box to `world`.
#[must_use]
pub fn is_empty_in(world: &World) -> bool {
    BOX_COUNT.load(Ordering::Relaxed) == 0 || registry().is_empty(world.uuid)
}

/// Collects the plugin collision boxes in `world` that touch `area`.
#[must_use]
pub fn collision_boxes(world: &World, area: BoundingBox) -> Vec<BoundingBox> {
    if BOX_COUNT.load(Ordering::Relaxed) == 0 {
        return Vec::new();
    }
    registry().boxes(world.uuid, &area)
}

/// Pushes `entity` out of the boxes it overlaps and returns the correction, if any.
///
/// Every overlapping box pushes along the axis on which the overlap is smallest, by the overlap plus
/// [`PUSH_EPSILON`]. Boxes that push in opposite directions are resolved over several passes.
#[must_use]
pub fn resolve(entity: BoundingBox, boxes: &[BoundingBox]) -> Option<Vector3<f64>> {
    let mut total = Vector3::<f64>::new(0.0, 0.0, 0.0);
    let mut current = entity;
    let mut moved = false;
    for _ in 0..RESOLVE_PASSES {
        let Some(push) = push_out(current, boxes) else {
            break;
        };
        moved = true;
        current = current.shift(push);
        total = total.add_raw(push.x, push.y, push.z);
    }
    moved.then_some(total)
}

/// The correction that moves `entity` out of every box it overlaps, in one pass.
fn push_out(entity: BoundingBox, boxes: &[BoundingBox]) -> Option<Vector3<f64>> {
    let mut push = Vector3::<f64>::new(0.0, 0.0, 0.0);
    let mut collided = false;
    for other in boxes {
        if !entity.intersects(other) {
            continue;
        }
        collided = true;

        // How far the entity has to move on each of the six directions to leave the box.
        let negative_x = entity.max.x - other.min.x;
        let positive_x = other.max.x - entity.min.x;
        let negative_y = entity.max.y - other.min.y;
        let positive_y = other.max.y - entity.min.y;
        let negative_z = entity.max.z - other.min.z;
        let positive_z = other.max.z - entity.min.z;

        let mut smallest: f64 = negative_x;
        let mut axis = Axis::X;
        let mut positive = false;
        for (overlap, candidate_axis, candidate_positive) in [
            (positive_x, Axis::X, true),
            (negative_y, Axis::Y, false),
            (positive_y, Axis::Y, true),
            (negative_z, Axis::Z, false),
            (positive_z, Axis::Z, true),
        ] {
            if overlap < smallest {
                smallest = overlap;
                axis = candidate_axis;
                positive = candidate_positive;
            }
        }

        let delta = if positive {
            smallest + PUSH_EPSILON
        } else {
            -(smallest + PUSH_EPSILON)
        };
        if delta.abs() > push.get_axis(axis).abs() {
            push.set_axis(axis, delta);
        }
    }
    collided.then_some(push)
}

/// Corrects a player position that ends inside a plugin collision box.
///
/// The client owns player movement: the host accepts the position from the movement packet and only
/// sends corrections. A position that ends inside a box is pushed out of it here, and the caller
/// teleports the player to the result so the client follows. Returns `pos` unchanged when no box is
/// in the way.
#[must_use]
pub fn correct_player_position(world: &World, entity: &Entity, pos: Vector3<f64>) -> Vector3<f64> {
    if is_empty_in(world) {
        return pos;
    }
    let previous = entity.pos.load();
    let moved = entity.bounding_box.load().shift(Vector3::new(
        pos.x - previous.x,
        pos.y - previous.y,
        pos.z - previous.z,
    ));
    // Only a box that touches the moved box can push it; the margin also catches the box the
    // player walked into with the step of this packet.
    let boxes = collision_boxes(world, moved.expand(1.0, 1.0, 1.0));
    if boxes.is_empty() {
        return pos;
    }
    resolve(moved, &boxes).map_or(pos, |push| pos.add_raw(push.x, push.y, push.z))
}

#[cfg(test)]
mod tests {
    use super::{CollisionBoxRegistry, resolve};
    use pumpkin_util::math::boundingbox::BoundingBox;
    use pumpkin_util::math::vector3::Vector3;
    use std::sync::Weak;
    use uuid::Uuid;

    fn boxed(min: [f64; 3], max: [f64; 3]) -> BoundingBox {
        BoundingBox::new(Vector3::new(min[0], min[1], min[2]), Vector3::new(max[0], max[1], max[2]))
    }

    #[test]
    fn a_box_pushes_out_on_the_smallest_overlap() {
        let entity = boxed([0.0, 0.0, 0.0], [0.6, 1.8, 0.6]);
        // The wall overlaps the entity by 0.2 on -X and much more on every other direction.
        let wall = boxed([0.4, 0.0, 0.0], [1.4, 2.0, 1.0]);
        let push = resolve(entity, &[wall]).expect("the entity is inside the wall");
        assert!(push.x < 0.0);
        assert!(push.x > -0.25);
        assert!(push.y.abs() < f64::EPSILON);
        assert!(push.z.abs() < f64::EPSILON);

        let moved = entity.shift(push);
        assert!(!moved.intersects(&wall));
    }

    #[test]
    fn a_box_the_entity_only_touches_does_not_push() {
        let entity = boxed([0.0, 1.0, 0.0], [0.6, 1.8, 0.6]);
        let floor = boxed([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        assert!(resolve(entity, &[floor]).is_none());
    }

    #[test]
    fn a_box_that_contains_the_entity_is_left_completely() {
        let entity = boxed([0.2, 0.2, 0.2], [0.4, 0.4, 0.4]);
        let block = boxed([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let push = resolve(entity, &[block]).expect("the entity is inside the block");
        assert!(!entity.shift(push).intersects(&block));
    }

    #[test]
    fn boxes_on_different_axes_are_both_resolved() {
        let entity = boxed([0.0, 0.0, 0.0], [0.6, 1.8, 0.6]);
        // A floor the feet are slightly inside of, and a wall on the right.
        let floor = boxed([0.0, -1.0, 0.0], [1.0, 0.1, 1.0]);
        let wall = boxed([0.4, 0.0, 0.0], [1.4, 2.0, 1.0]);
        let push = resolve(entity, &[floor, wall]).expect("the entity is inside both boxes");
        assert!(push.x < 0.0);
        assert!(push.y > 0.0);
        let moved = entity.shift(push);
        assert!(!moved.intersects(&floor));
        assert!(!moved.intersects(&wall));
    }

    #[test]
    fn the_registry_replaces_boxes_per_plugin() {
        let mut registry = CollisionBoxRegistry::new();
        let world = Uuid::new_v4();
        let area = boxed([-10.0, -10.0, -10.0], [10.0, 10.0, 10.0]);
        let far = boxed([100.0, 0.0, 100.0], [101.0, 1.0, 101.0]);

        registry.set(world, Weak::new(), "a", vec![boxed([0.0; 3], [1.0; 3])]);
        assert_eq!(registry.boxes(world, &area).len(), 1);

        registry.set(world, Weak::new(), "a", vec![far]);
        assert!(registry.boxes(world, &area).is_empty());

        registry.set(world, Weak::new(), "b", vec![boxed([0.0; 3], [1.0; 3])]);
        assert_eq!(registry.boxes(world, &area).len(), 1);
        assert_eq!(registry.total_boxes(), 2);
    }

    #[test]
    fn the_registry_keeps_worlds_and_plugins_apart() {
        let mut registry = CollisionBoxRegistry::new();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let area = boxed([-10.0, -10.0, -10.0], [10.0, 10.0, 10.0]);

        registry.set(first, Weak::new(), "a", vec![boxed([0.0; 3], [1.0; 3])]);
        registry.set(second, Weak::new(), "a", vec![boxed([0.0; 3], [1.0; 3])]);
        assert_eq!(registry.boxes(first, &area).len(), 1);
        assert_eq!(registry.boxes(second, &area).len(), 1);

        registry.clear(first, "a");
        assert!(registry.is_empty(first));
        assert!(!registry.is_empty(second));

        registry.remove_owner("a");
        assert!(registry.is_empty(second));
        assert_eq!(registry.total_boxes(), 0);
    }

    #[test]
    fn an_empty_list_removes_the_boxes() {
        let mut registry = CollisionBoxRegistry::new();
        let world = Uuid::new_v4();
        registry.set(world, Weak::new(), "a", vec![boxed([0.0; 3], [1.0; 3])]);
        registry.set(world, Weak::new(), "a", Vec::new());
        assert!(registry.is_empty(world));
    }

    #[test]
    fn a_world_without_entries_has_no_boxes() {
        // The fast path: no registry entry for this world at all.
        let world = Uuid::new_v4();
        let registry = CollisionBoxRegistry::new();
        assert!(registry.boxes(world, &boxed([0.0; 3], [1.0; 3])).is_empty());
    }
}
