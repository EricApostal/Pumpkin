//! Per-entity flags and overrides that plugins set. They replace the small behaviour mixins of
//! gameplay mods: fire immunity, no landing bounce, size and pickup box overrides.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::entity::EntityPose;
use pumpkin_util::math::boundingbox::EntityDimensions;
use pumpkin_util::math::vector3::Vector3;

use super::Entity;

/// Takes no fire damage and does not burn, like a fire immune entity type.
pub const FLAG_FIRE_IMMUNE: u32 = 1;
/// Slime blocks and beds do not bounce the entity after a fall.
pub const FLAG_NO_FALL_BOUNCE: u32 = 1 << 1;
/// Projectiles pass through the entities riding this entity (the opposite of vanilla).
pub const FLAG_IGNORE_RIDERS_HIT: u32 = 1 << 2;
/// Fires `EntityFluidMovementEvent` after the entity moved in a fluid.
pub const FLAG_FLUID_MOVEMENT_EVENTS: u32 = 1 << 3;
/// Mobs never pick the entity as a target (fake players, so bees and guards ignore them).
pub const FLAG_IGNORED_BY_MOBS: u32 = 1 << 4;
/// The entities riding this entity use their own box for mob attacks, not the vehicle box.
pub const FLAG_IGNORE_VEHICLE_BOX: u32 = 1 << 5;
/// Projectiles fired by a rider of this entity can hit the other riders (vanilla skips them).
pub const FLAG_PROJECTILES_HIT_RIDERS: u32 = 1 << 6;

/// Whether any entity got a flag that mobs and projectiles check on vehicles. Lets the hot paths
/// skip the vehicle lookup when no plugin uses it.
static ANY_VEHICLE_FLAG: AtomicBool = AtomicBool::new(false);

/// The names plugins use for the flags.
#[must_use]
pub fn flag_from_key(key: &str) -> Option<u32> {
    Some(match key {
        "fire-immune" => FLAG_FIRE_IMMUNE,
        "no-fall-bounce" => FLAG_NO_FALL_BOUNCE,
        "ignore-riders-hit" => FLAG_IGNORE_RIDERS_HIT,
        "fluid-movement-events" => FLAG_FLUID_MOVEMENT_EVENTS,
        "ignored-by-mobs" => FLAG_IGNORED_BY_MOBS,
        "ignore-vehicle-box" => FLAG_IGNORE_VEHICLE_BOX,
        "projectiles-hit-riders" => FLAG_PROJECTILES_HIT_RIDERS,
        _ => return None,
    })
}

/// Whether any entity has a vehicle flag, so riders are worth checking.
#[must_use]
pub fn any_vehicle_flag() -> bool {
    ANY_VEHICLE_FLAG.load(Ordering::Relaxed)
}

/// The flags and overrides of one entity. All reads are one atomic load.
pub struct PluginFlags {
    bits: AtomicU32,
    /// Width and height that replace the dimensions of every pose.
    size: AtomicCell<Option<(f32, f32)>>,
    /// How far the box that picks up items reaches around the bounding box.
    pickup_box: AtomicCell<Option<Vector3<f64>>>,
    /// Pose that replaces every pose the entity would take.
    pose: AtomicCell<Option<EntityPose>>,
}

impl Default for PluginFlags {
    fn default() -> Self {
        Self {
            bits: AtomicU32::new(0),
            size: AtomicCell::new(None),
            pickup_box: AtomicCell::new(None),
            pose: AtomicCell::new(None),
        }
    }
}

impl PluginFlags {
    #[must_use]
    pub fn has(&self, flag: u32) -> bool {
        self.bits.load(Ordering::Relaxed) & flag != 0
    }

    pub fn set(&self, flag: u32, value: bool) {
        if value {
            if flag
                & (FLAG_IGNORE_RIDERS_HIT | FLAG_IGNORE_VEHICLE_BOX | FLAG_PROJECTILES_HIT_RIDERS)
                != 0
            {
                ANY_VEHICLE_FLAG.store(true, Ordering::Relaxed);
            }
            self.bits.fetch_or(flag, Ordering::Relaxed);
        } else {
            self.bits.fetch_and(!flag, Ordering::Relaxed);
        }
    }

    #[must_use]
    pub fn pose_override(&self) -> Option<EntityPose> {
        self.pose.load()
    }

    pub fn set_pose_override(&self, pose: Option<EntityPose>) {
        self.pose.store(pose);
    }

    #[must_use]
    pub fn size_override(&self) -> Option<(f32, f32)> {
        self.size.load()
    }

    pub fn set_size_override(&self, size: Option<(f32, f32)>) {
        self.size.store(size);
    }

    #[must_use]
    pub fn pickup_box_override(&self) -> Option<Vector3<f64>> {
        self.pickup_box.load()
    }

    pub fn set_pickup_box_override(&self, reach: Option<Vector3<f64>>) {
        self.pickup_box.store(reach);
    }
}

impl Entity {
    /// Applies the size override of a plugin to the dimensions of a pose.
    #[must_use]
    pub fn plugin_dimensions(&self, dimensions: EntityDimensions) -> EntityDimensions {
        match self.plugin_flags.size_override() {
            Some((width, height)) if dimensions.height > 0.0 => EntityDimensions::new(
                width,
                height,
                dimensions.eye_height * height / dimensions.height,
            ),
            Some((width, height)) => EntityDimensions::new(width, height, height * 0.85),
            None => dimensions,
        }
    }

    /// Computes the dimensions of the current pose again and resizes the bounding box. Call it
    /// after the size override changed.
    pub fn refresh_dimensions(&self) {
        let dimension = self.plugin_dimensions(self.get_dimensions(self.pose.load()));
        let position = self.pos.load();
        self.bounding_box
            .store(pumpkin_util::math::boundingbox::BoundingBox::new_from_pos(
                position.x, position.y, position.z, &dimension,
            ));
        self.entity_dimension.store(dimension);
    }
}
