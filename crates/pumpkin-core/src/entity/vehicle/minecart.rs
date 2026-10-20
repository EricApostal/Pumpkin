mod chest;
mod container;
mod furnace;
mod hopper;
mod rideable;
mod tnt;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_protocol::java::server::play::SPlayerInput;
use rand::RngExt;

use crate::{
    entity::{Entity, EntityBase, living::LivingEntity, player::Player},
    server::Server,
};
use pumpkin_data::Block;
use pumpkin_data::block_properties::{BlockProperties, PoweredRailLikeProperties, RailShape};
use pumpkin_data::damage::DamageType;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_inventory::Inventory;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::GameMode;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

use crate::entity::vehicle::vehicle::VehicleEntity;
use chest::ChestMinecart;
use container::MinecartInventory;
use furnace::FurnaceMinecart;
use hopper::HopperMinecart;
use rideable::RideableMinecart;
use tnt::TntMinecart;

const fn get_exits(
    shape: pumpkin_data::block_properties::RailShape,
) -> (Vector3<f64>, Vector3<f64>) {
    match shape {
        RailShape::NorthSouth => (Vector3::new(0.0, 0.0, -1.0), Vector3::new(0.0, 0.0, 1.0)),
        RailShape::EastWest => (Vector3::new(-1.0, 0.0, 0.0), Vector3::new(1.0, 0.0, 0.0)),
        // Vanilla `AbstractMinecart.EXITS` uses the *lower* exit below the rail block for
        // ascending shapes (`AbstractMinecart.java:80-87`); the y component is -1, not +1.
        RailShape::AscendingEast => (Vector3::new(-1.0, -1.0, 0.0), Vector3::new(1.0, 0.0, 0.0)),
        RailShape::AscendingWest => (Vector3::new(-1.0, 0.0, 0.0), Vector3::new(1.0, -1.0, 0.0)),
        RailShape::AscendingNorth => (Vector3::new(0.0, 0.0, -1.0), Vector3::new(0.0, -1.0, 1.0)),
        RailShape::AscendingSouth => (Vector3::new(0.0, -1.0, -1.0), Vector3::new(0.0, 0.0, 1.0)),
        RailShape::SouthEast => (Vector3::new(0.0, 0.0, 1.0), Vector3::new(1.0, 0.0, 0.0)),
        RailShape::SouthWest => (Vector3::new(0.0, 0.0, 1.0), Vector3::new(-1.0, 0.0, 0.0)),
        RailShape::NorthWest => (Vector3::new(0.0, 0.0, -1.0), Vector3::new(-1.0, 0.0, 0.0)),
        RailShape::NorthEast => (Vector3::new(0.0, 0.0, -1.0), Vector3::new(1.0, 0.0, 0.0)),
    }
}

/// The [`RailShape`] of a rail block, including custom rails in the `minecraft:rails` tag.
fn shape_of(block: &Block, state_id: pumpkin_data::BlockStateId) -> pumpkin_data::block_properties::RailShape {
    use pumpkin_data::block_properties::{RailLikeProperties, RailShape, RailShapeStraight};

    if block.id == Block::RAIL.id {
        RailLikeProperties::from_state_id(state_id).shape
    } else if PoweredRailLikeProperties::handles_block_id(block.id) {
        match PoweredRailLikeProperties::from_state_id(state_id).shape {
            RailShapeStraight::NorthSouth => RailShape::NorthSouth,
            RailShapeStraight::EastWest => RailShape::EastWest,
            RailShapeStraight::AscendingEast => RailShape::AscendingEast,
            RailShapeStraight::AscendingWest => RailShape::AscendingWest,
            RailShapeStraight::AscendingNorth => RailShape::AscendingNorth,
            RailShapeStraight::AscendingSouth => RailShape::AscendingSouth,
        }
    } else {
        // A custom rail from the `minecraft:rails` tag, like Create's cart assembler.
        rail_shape(block, state_id).unwrap_or(RailShape::NorthSouth)
    }
}

/// The position of a cart snapped onto the centre line of `rail_pos`'s rail, exactly as
/// vanilla `OldMinecartBehavior.getPos` computes it (`OldMinecartBehavior.java:305-353`).
///
/// The cart is projected onto the line between the rail's two exits; `y` follows the
/// slope of the rail, including vanilla's `+0.5`/`+1.0` slope adjustment.
fn rail_snap_position(
    rail_pos: BlockPos,
    shape: pumpkin_data::block_properties::RailShape,
    pos: Vector3<f64>,
) -> Vector3<f64> {
    let (exit0, exit1) = get_exits(shape);

    let x0 = f64::from(rail_pos.0.x) + 0.5 + exit0.x * 0.5;
    let y0 = f64::from(rail_pos.0.y) + RAIL_HEIGHT_OFFSET + exit0.y * 0.5;
    let z0 = f64::from(rail_pos.0.z) + 0.5 + exit0.z * 0.5;
    let x1 = f64::from(rail_pos.0.x) + 0.5 + exit1.x * 0.5;
    let y1 = f64::from(rail_pos.0.y) + RAIL_HEIGHT_OFFSET + exit1.y * 0.5;
    let z1 = f64::from(rail_pos.0.z) + 0.5 + exit1.z * 0.5;

    let xd = x1 - x0;
    let yd = (y1 - y0) * 2.0;
    let zd = z1 - z0;

    let progress = if xd == 0.0 {
        pos.z - f64::from(rail_pos.0.z)
    } else if zd == 0.0 {
        pos.x - f64::from(rail_pos.0.x)
    } else {
        ((pos.x - x0) * xd + (pos.z - z0) * zd) * 2.0
    };

    let mut y = y0 + yd * progress;
    if yd < 0.0 {
        y += 1.0;
    } else if yd > 0.0 {
        y += 0.5;
    }

    Vector3::new(x0 + xd * progress, y, z0 + zd * progress)
}

/// Vanilla `AbstractMinecart.push` (`AbstractMinecart.java:498-530`): the `(xa, za)` impulse
/// applied to the *other* entity. The cart itself receives `(-xa, -za)`.
///
/// Returns `None` when the horizontal squared distance is below vanilla's `1.0E-4` threshold.
fn minecart_push_impulse(
    self_pos: Vector3<f64>,
    other_pos: Vector3<f64>,
) -> Option<(f64, f64)> {
    let mut xa = other_pos.x - self_pos.x;
    let mut za = other_pos.z - self_pos.z;
    let mut dd = xa * xa + za * za;
    if dd < 1.0E-4 {
        return None;
    }
    dd = dd.sqrt();
    xa /= dd;
    za /= dd;
    let mut pow = 1.0 / dd;
    if pow > 1.0 {
        pow = 1.0;
    }
    xa *= pow;
    za *= pow;
    xa *= 0.1;
    za *= 0.1;
    xa *= 0.5;
    za *= 0.5;
    Some((xa, za))
}

/// Vanilla `Entity.getInputVector` (`Entity.java:1791-1801`).
fn get_input_vector(input: Vector3<f64>, speed: f64, yaw: f32) -> Vector3<f64> {
    let length = input.length_squared();
    if length < 1.0E-7 {
        return Vector3::new(0.0, 0.0, 0.0);
    }
    let movement = if length > 1.0 {
        input.normalize().multiply(speed, speed, speed)
    } else {
        input.multiply(speed, speed, speed)
    };
    let (sin, cos) = f64::from(yaw).to_radians().sin_cos();
    Vector3::new(
        movement.x.mul_add(cos, -(movement.z * sin)),
        movement.y,
        movement.z.mul_add(cos, movement.x * sin),
    )
}

const GRAVITY: f64 = 0.04;
const RAIL_HEIGHT_OFFSET: f64 = 0.0625;

/// Whether `block` in `state` is a rail for minecart movement: a vanilla rail, or a custom block
/// in the `minecraft:rails` tag with a `shape` property. Create's cart assembler and controller
/// rail extend vanilla's `BaseRailBlock`, so a plugin block that joins the tag behaves the same.
fn is_rail(block: &Block, state_id: pumpkin_data::BlockStateId) -> bool {
    block.id == Block::RAIL.id
        || block.id == Block::POWERED_RAIL.id
        || block.id == Block::DETECTOR_RAIL.id
        || block.id == Block::ACTIVATOR_RAIL.id
        || (block.is_dynamic()
            && block.has_tag(&tag::Block::MINECRAFT_RAILS)
            && rail_shape(block, state_id).is_some())
}

/// The shape of a custom rail from its `shape` property. `None` when the block has no such
/// property or the value is not a rail shape.
pub(crate) fn rail_shape(
    block: &Block,
    state_id: pumpkin_data::BlockStateId,
) -> Option<pumpkin_data::block_properties::RailShape> {
    let shape = block
        .properties(state_id)?
        .to_props()
        .into_iter()
        .find(|(name, _)| *name == "shape")?
        .1;
    Some(match shape {
        "north_south" => RailShape::NorthSouth,
        "east_west" => RailShape::EastWest,
        "ascending_east" => RailShape::AscendingEast,
        "ascending_west" => RailShape::AscendingWest,
        "ascending_north" => RailShape::AscendingNorth,
        "ascending_south" => RailShape::AscendingSouth,
        "south_east" => RailShape::SouthEast,
        "south_west" => RailShape::SouthWest,
        "north_west" => RailShape::NorthWest,
        "north_east" => RailShape::NorthEast,
        _ => return None,
    })
}

pub struct MinecartEntity {
    pub vehicle: VehicleEntity,
    kind: MinecartKind,
}

enum MinecartKind {
    Rideable(RideableMinecart),
    Chest(ChestMinecart),
    Furnace(FurnaceMinecart),
    Hopper(HopperMinecart),
    Tnt(TntMinecart),
    Other,
}

impl MinecartEntity {
    pub fn new(entity: Entity) -> Self {
        let kind = match entity.entity_type.id {
            id if id == EntityType::MINECART.id => MinecartKind::Rideable(RideableMinecart),
            id if id == EntityType::CHEST_MINECART.id => MinecartKind::Chest(ChestMinecart::new()),
            id if id == EntityType::FURNACE_MINECART.id => {
                MinecartKind::Furnace(FurnaceMinecart::new())
            }
            id if id == EntityType::HOPPER_MINECART.id => {
                MinecartKind::Hopper(HopperMinecart::new())
            }
            id if id == EntityType::TNT_MINECART.id => MinecartKind::Tnt(TntMinecart::new()),
            _ => MinecartKind::Other,
        };
        Self {
            vehicle: VehicleEntity::new(entity),
            kind,
        }
    }

    const fn container(&self) -> Option<&Arc<MinecartInventory>> {
        match &self.kind {
            MinecartKind::Chest(minecart) => Some(minecart.inventory()),
            MinecartKind::Hopper(minecart) => Some(minecart.inventory()),
            _ => None,
        }
    }

    const fn drop_item(&self) -> Option<&'static Item> {
        match &self.kind {
            MinecartKind::Chest(_) => Some(&Item::CHEST_MINECART),
            MinecartKind::Furnace(_) => Some(&Item::FURNACE_MINECART),
            MinecartKind::Hopper(_) => Some(&Item::HOPPER_MINECART),
            MinecartKind::Tnt(_) => Some(&Item::TNT_MINECART),
            _ => None,
        }
    }
}

impl EntityBase for MinecartEntity {
    fn write_custom_nbt(&self, nbt: &mut NbtCompound) {
        match &self.kind {
            MinecartKind::Chest(minecart) => minecart.write_nbt(nbt),
            MinecartKind::Furnace(minecart) => minecart.write_nbt(nbt),
            MinecartKind::Hopper(minecart) => minecart.write_nbt(nbt),
            MinecartKind::Tnt(minecart) => minecart.write_nbt(nbt),
            MinecartKind::Rideable(_) | MinecartKind::Other => {}
        }
    }

    fn read_custom_nbt(&self, nbt: &NbtCompound) {
        match &self.kind {
            MinecartKind::Chest(minecart) => minecart.read_nbt(nbt),
            MinecartKind::Furnace(minecart) => minecart.read_nbt(nbt),
            MinecartKind::Hopper(minecart) => minecart.read_nbt(nbt),
            MinecartKind::Tnt(minecart) => minecart.read_nbt(nbt),
            MinecartKind::Rideable(_) | MinecartKind::Other => {}
        }
    }

    #[allow(clippy::too_many_lines)]
    fn tick(&self, caller: &dyn EntityBase, _server: &Server) {
        self.vehicle.tick();
        if let MinecartKind::Furnace(minecart) = &self.kind {
            minecart.tick(&self.vehicle.entity);
        }

        // Vanilla `Entity.baseTick` records `xo`/`yo`/`zo` before the cart moves; the old
        // behavior derives its yaw from that delta (`OldMinecartBehavior.java:68-71`).
        self.vehicle.entity.update_last_pos();

        let world = self.vehicle.entity.world.load();
        let pos = self.vehicle.entity.pos.load();
        let mut block_pos = BlockPos(Vector3::new(
            pos.x.floor() as i32,
            pos.y.floor() as i32,
            pos.z.floor() as i32,
        ));

        let (mut block, mut state_id) = world.get_block_and_state_id(&block_pos);

        let mut is_powered_rail = block.id == Block::POWERED_RAIL.id;
        let mut is_activator_rail = block.id == Block::ACTIVATOR_RAIL.id;
        let mut is_on_rails = is_rail(block, state_id);

        // If not on rails at current Y level, check the block directly below
        if !is_on_rails {
            let below_block_pos = BlockPos(Vector3::new(
                block_pos.0.x,
                block_pos.0.y - 1,
                block_pos.0.z,
            ));
            let (below_block, below_state_id) = world.get_block_and_state_id(&below_block_pos);
            if is_rail(below_block, below_state_id) {
                block_pos = below_block_pos;
                block = below_block;
                state_id = below_state_id;
                is_powered_rail = block.id == Block::POWERED_RAIL.id;
                is_activator_rail = block.id == Block::ACTIVATOR_RAIL.id;
                is_on_rails = true;
            }
        }

        let shape = shape_of(block, state_id);

        if is_powered_rail || is_activator_rail {
            let props = PoweredRailLikeProperties::from_state_id(state_id);
            let powered = props.powered;

            if is_activator_rail && let MinecartKind::Hopper(minecart) = &self.kind {
                minecart.set_enabled(!powered);
            }
        } else if block.id == Block::DETECTOR_RAIL.id
            && let Some(server) = world.server.upgrade()
        {
            world.block_registry.on_entity_collision(
                block,
                &world,
                self,
                &block_pos,
                world.get_block_state(&block_pos),
                &server,
            );
        }

        if is_powered_rail || is_activator_rail {
            let props = PoweredRailLikeProperties::from_state_id(state_id);
            let powered = props.powered;

            if powered {
                if is_activator_rail {
                    match &self.kind {
                        MinecartKind::Tnt(minecart) => {
                            minecart.prime(&self.vehicle.entity, 80);
                        }
                        MinecartKind::Rideable(_) => {
                            if let Ok(passengers) = self.vehicle.entity.passengers.try_lock() {
                                let p_ids: Vec<i32> = passengers
                                    .iter()
                                    .map(|p| p.get_entity().entity_id)
                                    .collect();
                                if !p_ids.is_empty() {
                                    let world = self.vehicle.entity.world.load();
                                    let vid = self.vehicle.entity.entity_id;
                                    if let Some(v) = world.get_entity_by_id(vid) {
                                        for pid in p_ids {
                                            v.get_entity().remove_passenger_sync(pid);
                                        }
                                    }
                                }
                            }
                            if self.vehicle.get_hurt_time() == 0 {
                                self.vehicle.set_hurt_dir(-self.vehicle.get_hurt_dir());
                                self.vehicle.set_hurt_time(10);
                                self.vehicle.set_damage(50.0);
                                self.vehicle.send_wobble_metadata();
                            }
                        }
                        _ => {}
                    }
                }
            } else if is_powered_rail {
                // `OldMinecartBehavior.moveAlongTrack` (`OldMinecartBehavior.java:170-177`).
                let mut velocity = self.vehicle.entity.velocity.load();
                if velocity.x.hypot(velocity.z) < 0.03 {
                    velocity = Vector3::new(0.0, 0.0, 0.0);
                } else {
                    velocity = velocity.multiply(0.5, 0.0, 0.5);
                }
                self.vehicle.entity.velocity.store(velocity);
                self.vehicle.entity.send_velocity();
            }
        }

        if let MinecartKind::Tnt(minecart) = &self.kind
            && minecart.tick(&self.vehicle.entity)
        {
            return;
        }

        let mut velocity = self.vehicle.entity.velocity.load();

        let mut has_driver = false;
        let mut driver_input = 0;
        let mut driver_yaw = 0.0f32;

        if let Ok(passengers) = self.vehicle.entity.passengers.try_lock()
            && let Some(passenger) = passengers.first()
            && let Some(player) = passenger.get_player()
        {
            driver_input = player.last_input.load(Ordering::Relaxed);
            driver_yaw = player.get_entity().yaw.load();
            has_driver = true;
        }

        if has_driver && is_on_rails {
            let left = driver_input & SPlayerInput::LEFT != 0;
            let right = driver_input & SPlayerInput::RIGHT != 0;
            let forward = driver_input & SPlayerInput::FORWARD != 0;
            let backward = driver_input & SPlayerInput::BACKWARD != 0;

            // Vanilla `ServerPlayer.getLastClientMoveIntent` (`ServerPlayer.java:2439-2443`).
            let left_intent = if left == right {
                0.0
            } else if left {
                1.0
            } else {
                -1.0
            };
            let forward_intent = if forward == backward {
                0.0
            } else if forward {
                1.0
            } else {
                -1.0
            };
            let intent = get_input_vector(
                Vector3::new(left_intent, 0.0, forward_intent),
                1.0,
                driver_yaw,
            );

            // Vanilla `OldMinecartBehavior.moveAlongTrack` (`OldMinecartBehavior.java:161-168`):
            // a rider only nudges a nearly stopped cart, by `0.001` per tick.
            let own_dist = velocity.x.mul_add(velocity.x, velocity.z * velocity.z);
            if intent.length_squared() > 0.0 && own_dist < 0.01 {
                velocity.x += intent.x * 0.001;
                velocity.z += intent.z * 0.001;
                self.vehicle.entity.velocity.store(velocity);
                self.vehicle.entity.send_velocity();
            }
        }

        let mut velocity = self.vehicle.entity.velocity.load();

        if is_on_rails {
            let pos = self.vehicle.entity.pos.load();
            let block_center_bottom = Vector3::new(
                f64::from(block_pos.0.x) + 0.5,
                f64::from(block_pos.0.y),
                f64::from(block_pos.0.z) + 0.5,
            );

            // Snap onto the rail centre line, including the slope height, exactly like
            // vanilla `OldMinecartBehavior.getPos`.
            let target_position = rail_snap_position(block_pos, shape, pos);
            self.vehicle.entity.pos.store(target_position);

            let (exit0, exit1) = get_exits(shape);
            let exit0 = exit0.multiply(0.5, 0.5, 0.5);
            let exit1 = exit1.multiply(0.5, 0.5, 0.5);

            let horizontal_in_direction = Vector3::new(exit1.x, 0.0, exit1.z);
            let mut horizontal_out_direction = Vector3::new(exit0.x, 0.0, exit0.z);

            if velocity.dot(&horizontal_out_direction) < velocity.dot(&horizontal_in_direction) {
                horizontal_out_direction = horizontal_in_direction;
            }

            let out_position = block_center_bottom.add(&horizontal_out_direction).add(
                &horizontal_out_direction
                    .normalize()
                    .multiply(1e-5, 1e-5, 1e-5),
            );

            let mut towards_out = out_position - target_position;
            towards_out.y = 0.0;
            let towards_length = towards_out.length();
            if towards_length > 1e-5 {
                towards_out = towards_out.normalize();
                let speed = velocity.length();
                velocity = towards_out.multiply(speed, speed, speed);
            }

            velocity.y = 0.0;
            self.vehicle.entity.velocity.store(velocity);
        } else if !self.vehicle.entity.on_ground.load(Ordering::Relaxed) {
            // `AbstractMinecart.getDefaultGravity` (`AbstractMinecart.java:260-263`).
            let gravity = if self.vehicle.entity.touching_water.load(Ordering::Relaxed) {
                0.005
            } else {
                GRAVITY
            };
            velocity.y -= gravity;
            self.vehicle.entity.velocity.store(velocity);
        }

        if velocity.length_squared() > 0.0 {
            // Vanilla `OldMinecartBehavior.moveAlongTrack` moves with the horizontal velocity
            // clamped to `getMaxSpeed` and scaled by 0.75 for a ridden cart
            // (`OldMinecartBehavior.java:199-203`).
            if is_on_rails {
                let scale = if self.vehicle.entity.has_passengers() {
                    0.75
                } else {
                    1.0
                };
                let max_speed = if self.vehicle.entity.touching_water.load(Ordering::Relaxed) {
                    0.2
                } else {
                    0.4
                };
                velocity.x = (scale * velocity.x).clamp(-max_speed, max_speed);
                velocity.z = (scale * velocity.z).clamp(-max_speed, max_speed);
                velocity.y = 0.0;
                self.vehicle.entity.velocity.store(velocity);
            } else {
                // `AbstractMinecart.comeOffTrack` clamps the horizontal movement to
                // `getMaxSpeed` too (`AbstractMinecart.java:366-378`).
                let max_speed = if self.vehicle.entity.touching_water.load(Ordering::Relaxed) {
                    0.2
                } else {
                    0.4
                };
                velocity.x = velocity.x.clamp(-max_speed, max_speed);
                velocity.z = velocity.z.clamp(-max_speed, max_speed);
                self.vehicle.entity.velocity.store(velocity);
            }
            self.move_entity(caller, velocity);

            if let MinecartKind::Tnt(minecart) = &self.kind
                && self
                    .vehicle
                    .entity
                    .horizontal_collision
                    .load(Ordering::Relaxed)
                && velocity.x.mul_add(velocity.x, velocity.z * velocity.z) >= 0.01
            {
                minecart.explode(
                    &self.vehicle.entity,
                    velocity.x.mul_add(velocity.x, velocity.z * velocity.z),
                );
                return;
            }

            let new_pos = self.vehicle.entity.pos.load();

            if is_on_rails {
                // `OldMinecartBehavior.moveAlongTrack` re-derives the cart's y from the rail
                // it now occupies after moving (`OldMinecartBehavior.java:213-223`).
                let rail_block_pos = BlockPos(Vector3::new(
                    new_pos.x.floor() as i32,
                    new_pos.y.floor() as i32,
                    new_pos.z.floor() as i32,
                ));
                let (mut rail_block, mut rail_state) =
                    world.get_block_and_state_id(&rail_block_pos);
                let mut rail_pos = rail_block_pos;
                if !is_rail(rail_block, rail_state) {
                    let below = BlockPos(Vector3::new(
                        rail_block_pos.0.x,
                        rail_block_pos.0.y - 1,
                        rail_block_pos.0.z,
                    ));
                    let (below_block, below_state) = world.get_block_and_state_id(&below);
                    if is_rail(below_block, below_state) {
                        rail_pos = below;
                        rail_block = below_block;
                        rail_state = below_state;
                    }
                }
                if is_rail(rail_block, rail_state) {
                    let snapped = rail_snap_position(rail_pos, shape_of(rail_block, rail_state), new_pos);
                    self.vehicle.entity.pos.store(Vector3::new(
                        new_pos.x,
                        snapped.y,
                        new_pos.z,
                    ));
                }
            }

            if let Ok(passengers) = self.vehicle.entity.passengers.try_lock() {
                let passenger_pos = self.vehicle.entity.pos.load();
                for passenger in passengers.iter() {
                    passenger.get_entity().set_pos(passenger_pos);
                }
            }

            // Vanilla applies the slowdown to the delta movement left after `move()`, so use
            // the velocity the movement produced rather than the pre-move one.
            let velocity = self.vehicle.entity.velocity.load();

            #[allow(clippy::useless_let_if_seq)]
            let mut friction = 0.95; // Vanilla minecart air drag

            if is_on_rails {
                let has_passengers = self
                    .vehicle
                    .entity
                    .passengers
                    .try_lock()
                    .is_ok_and(|p| !p.is_empty());
                // Vanilla `OldMinecartBehavior.getSlowdownFactor` (`OldMinecartBehavior.java:406-409`).
                friction = if has_passengers { 0.997 } else { 0.96 };
                // `AbstractMinecart.applyNaturalSlowdown` scales by 0.95 in water
                // (`AbstractMinecart.java:467-475`).
                if self.vehicle.entity.touching_water.load(Ordering::Relaxed) {
                    friction *= 0.95;
                }
            } else {
                let below_block_pos = BlockPos(Vector3::new(
                    block_pos.0.x,
                    block_pos.0.y - 1,
                    block_pos.0.z,
                ));
                let below_block = world.get_block(&below_block_pos);

                let is_on_ground = self.vehicle.entity.on_ground.load(Ordering::Relaxed)
                    || (below_block.id != Block::AIR.id
                        && below_block.id != Block::WATER.id
                        && below_block.id != Block::LAVA.id);
                let is_in_water = self.vehicle.entity.touching_water.load(Ordering::Relaxed)
                    || below_block.id == Block::WATER.id;

                if is_on_ground {
                    friction = 0.5;
                } else if is_in_water {
                    friction = 0.95;
                }
            }

            let next_vel = if is_on_rails && let MinecartKind::Furnace(minecart) = &self.kind {
                minecart.velocity(&self.vehicle.entity, velocity)
            } else if is_on_rails && let Some(inventory) = self.container() {
                container::velocity(&self.vehicle.entity, inventory, velocity)
            } else if is_on_rails {
                // `AbstractMinecart.applyNaturalSlowdown` multiplies by
                // `(slowdown, 0.0, slowdown)` (`AbstractMinecart.java:467-475`).
                velocity.multiply(friction, 0.0, friction)
            } else {
                velocity.multiply(friction, friction, friction)
            };
            self.vehicle.entity.velocity.store(next_vel);
            if next_vel.length_squared() == 0.0 {
                self.vehicle.entity.send_velocity();
            }
        }

        // `OldMinecartBehavior.moveAlongTrack` (`OldMinecartBehavior.java:233-263`): the boost
        // is applied after the move and slowdown, so it survives into the next tick.
        if is_powered_rail && PoweredRailLikeProperties::from_state_id(state_id).powered {
            let mut velocity = self.vehicle.entity.velocity.load();
            let speed = velocity.x.hypot(velocity.z);
            if speed > 0.01 {
                velocity.x += velocity.x / speed * 0.06;
                velocity.z += velocity.z / speed * 0.06;
                self.vehicle.entity.velocity.store(velocity);
            } else {
                // A stopped cart is launched away from the adjacent redstone conductor.
                let mut dx = velocity.x;
                let mut dz = velocity.z;
                if shape == RailShape::EastWest {
                    if world
                        .get_block_state(&block_pos.offset(Vector3::new(-1, 0, 0)))
                        .is_solid_block()
                    {
                        dx = 0.02;
                    } else if world
                        .get_block_state(&block_pos.offset(Vector3::new(1, 0, 0)))
                        .is_solid_block()
                    {
                        dx = -0.02;
                    }
                } else if shape == RailShape::NorthSouth {
                    if world
                        .get_block_state(&block_pos.offset(Vector3::new(0, 0, -1)))
                        .is_solid_block()
                    {
                        dz = 0.02;
                    } else if world
                        .get_block_state(&block_pos.offset(Vector3::new(0, 0, 1)))
                        .is_solid_block()
                    {
                        dz = -0.02;
                    }
                }
                self.vehicle
                    .entity
                    .velocity
                    .store(Vector3::new(dx, velocity.y, dz));
            }
            self.vehicle.entity.send_velocity();
        }

        // Vanilla `AbstractMinecart.move` applies the effects of the blocks the cart is inside
        // after moving, which is what fires `entityInside` (the cart assembler needs it).
        self.vehicle.entity.tick_block_collisions(caller);

        // `OldMinecartBehavior.tick` (`OldMinecartBehavior.java:67-84`): xRot is always 0 and
        // yRot follows the direction the cart travelled this tick.
        let old_pos = self.vehicle.entity.last_pos.load();
        let new_pos = self.vehicle.entity.pos.load();
        let x_diff = old_pos.x - new_pos.x;
        let z_diff = old_pos.z - new_pos.z;
        let mut yaw = self.vehicle.entity.yaw.load();
        if x_diff.mul_add(x_diff, z_diff * z_diff) > 0.001 {
            yaw = (z_diff.atan2(x_diff).to_degrees() as f32) % 360.0;
        }
        self.vehicle.entity.set_rotation(yaw, 0.0);

        // `OldMinecartBehavior.tick` pushes and picks up entities at the end of the tick
        // (`OldMinecartBehavior.java:85`).
        caller.push_entities(caller);

        if let MinecartKind::Hopper(minecart) = &self.kind {
            minecart.tick(&self.vehicle.entity);
        }
    }

    fn get_entity(&self) -> &Entity {
        &self.vehicle.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }

    fn is_pushable(&self) -> bool {
        true
    }

    fn get_block_speed_factor(&self) -> f32 {
        // Vanilla `AbstractMinecart.getBlockSpeedFactor` (`AbstractMinecart.java:233-237`):
        // a cart standing on a rail always uses 1.0, ignoring the block below it.
        let world = self.vehicle.entity.world.load();
        let block_pos = self.vehicle.entity.block_pos.load();
        let (block, _) = world.get_block_and_state(&block_pos);
        if block.has_tag(&tag::Block::MINECRAFT_RAILS) {
            1.0
        } else {
            self.vehicle.entity.get_block_speed_factor()
        }
    }

    #[allow(clippy::too_many_lines)]
    fn push(&self, entity: &dyn EntityBase) {
        let self_entity = self.get_entity();
        let other_entity = entity.get_entity();

        if self_entity.no_physics.load(Ordering::Relaxed)
            || other_entity.no_physics.load(Ordering::Relaxed)
        {
            return;
        }

        {
            let passengers = self_entity
                .passengers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if passengers
                .iter()
                .any(|p| p.get_entity().entity_id == other_entity.entity_id)
            {
                return;
            }
        }
        {
            let passengers = other_entity
                .passengers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if passengers
                .iter()
                .any(|p| p.get_entity().entity_id == self_entity.entity_id)
            {
                return;
            }
        }

        if let Some((xa, za)) = minecart_push_impulse(
            self_entity.pos.load(),
            other_entity.pos.load(),
        ) {
            let is_other_minecart = other_entity.entity_type.id == EntityType::MINECART.id
                || other_entity.entity_type.id == EntityType::CHEST_MINECART.id
                || other_entity.entity_type.id == EntityType::COMMAND_BLOCK_MINECART.id
                || other_entity.entity_type.id == EntityType::FURNACE_MINECART.id
                || other_entity.entity_type.id == EntityType::HOPPER_MINECART.id
                || other_entity.entity_type.id == EntityType::SPAWNER_MINECART.id
                || other_entity.entity_type.id == EntityType::TNT_MINECART.id;

            if is_other_minecart {
                // `AbstractMinecart.pushOtherMinecart` (`AbstractMinecart.java:532-545`):
                // without the experimental movement flag the direction comes from the other
                // cart's position, not from the cart's velocity.
                let xo = other_entity.pos.load().x - self_entity.pos.load().x;
                let zo = other_entity.pos.load().z - self_entity.pos.load().z;

                let dir = Vector3::new(xo, 0.0, zo).normalize();
                let facing = Vector3::new(
                    f64::from(self_entity.yaw.load().to_radians().cos()),
                    0.0,
                    f64::from(self_entity.yaw.load().to_radians().sin()),
                )
                .normalize();

                let dot = dir.dot(&facing).abs();
                if dot >= 0.8 {
                    let vel = self_entity.velocity.load();
                    let ovel = other_entity.velocity.load();

                    let is_self_furnace =
                        self_entity.entity_type.id == EntityType::FURNACE_MINECART.id;
                    let is_other_furnace =
                        other_entity.entity_type.id == EntityType::FURNACE_MINECART.id;

                    if is_other_furnace && !is_self_furnace {
                        self_entity.velocity.store(vel.multiply(0.2, 1.0, 0.2));
                        let mut new_self_vel = self_entity.velocity.load();
                        new_self_vel.x += ovel.x - xa;
                        new_self_vel.z += ovel.z - za;
                        self_entity.velocity.store(new_self_vel);
                        self_entity.send_velocity();

                        other_entity.velocity.store(ovel.multiply(0.95, 1.0, 0.95));
                        other_entity.send_velocity();
                    } else if !is_other_furnace && is_self_furnace {
                        other_entity.velocity.store(ovel.multiply(0.2, 1.0, 0.2));
                        let mut new_other_vel = other_entity.velocity.load();
                        new_other_vel.x += vel.x + xa;
                        new_other_vel.z += vel.z + za;
                        other_entity.velocity.store(new_other_vel);
                        other_entity.send_velocity();

                        self_entity.velocity.store(vel.multiply(0.95, 1.0, 0.95));
                        self_entity.send_velocity();
                    } else {
                        #[allow(clippy::manual_midpoint)]
                        let xdd = (ovel.x + vel.x) / 2.0;
                        #[allow(clippy::manual_midpoint)]
                        let zdd = (ovel.z + vel.z) / 2.0;

                        self_entity.velocity.store(vel.multiply(0.2, 1.0, 0.2));
                        let mut new_self_vel = self_entity.velocity.load();
                        new_self_vel.x += xdd - xa;
                        new_self_vel.z += zdd - za;
                        self_entity.velocity.store(new_self_vel);
                        self_entity.send_velocity();

                        other_entity.velocity.store(ovel.multiply(0.2, 1.0, 0.2));
                        let mut new_other_vel = other_entity.velocity.load();
                        new_other_vel.x += xdd + xa;
                        new_other_vel.z += zdd + za;
                        other_entity.velocity.store(new_other_vel);
                        other_entity.send_velocity();
                    }
                }
            } else {
                if !self_entity.has_passengers() && self.is_pushable() {
                    let mut vel = self_entity.velocity.load();
                    vel.x -= xa;
                    vel.z -= za;
                    self_entity.velocity.store(vel);
                    self_entity.send_velocity();
                }

                if !other_entity.has_passengers() && entity.is_pushable() {
                    let mut vel = other_entity.velocity.load();
                    vel.x += xa / 4.0;
                    vel.z += za / 4.0;
                    other_entity.velocity.store(vel);
                    other_entity.send_velocity();
                }
            }
        }
    }

    fn is_collidable(&self, _entity: Option<Box<dyn EntityBase>>) -> bool {
        true
    }

    fn init_data_tracker(&self) {
        self.vehicle.send_wobble_metadata();
        if let MinecartKind::Furnace(minecart) = &self.kind {
            minecart.init_data_tracker(&self.vehicle.entity);
        }
    }

    fn can_hit(&self) -> bool {
        self.vehicle.entity.is_alive()
    }

    fn damage_with_context(
        &self,
        _caller: &dyn EntityBase,
        amount: f32,
        damage_type: DamageType,
        _position: Option<Vector3<f64>>,
        source: Option<&dyn EntityBase>,
        cause: Option<&dyn EntityBase>,
    ) -> bool {
        let creative = source
            .and_then(EntityBase::get_player)
            .is_some_and(|player| player.gamemode.load() == GameMode::Creative);

        if let MinecartKind::Tnt(minecart) = &self.kind
            && damage_type == DamageType::ARROW
            && self.vehicle.entity.fire_ticks.load(Ordering::Relaxed) > 0
        {
            let projectile_speed_squared = cause
                .map(|entity| entity.get_entity().velocity.load().length_squared())
                .unwrap_or_default();
            minecart.explode(&self.vehicle.entity, projectile_speed_squared);
            if self.vehicle.entity.is_removed() {
                return true;
            }
        }

        let will_break = self.vehicle.entity.is_alive()
            && (creative || self.vehicle.get_damage() + amount * 10.0 > 40.0);

        if let MinecartKind::Tnt(minecart) = &self.kind
            && will_break
            && !creative
        {
            let velocity = self.vehicle.entity.velocity.load();
            let speed_squared = velocity.x.mul_add(velocity.x, velocity.z * velocity.z);
            let ignites = damage_type.has_tag(&tag::DamageType::MINECRAFT_IS_FIRE)
                || damage_type.has_tag(&tag::DamageType::MINECRAFT_IS_EXPLOSION)
                || self.vehicle.entity.fire_ticks.load(Ordering::Relaxed) > 0;
            if ignites || speed_squared >= 0.01 {
                self.vehicle.apply_damage_wobble(amount);
                let fuse = rand::rng().random_range(0..20) + rand::rng().random_range(0..20);
                if self
                    .vehicle
                    .entity
                    .world
                    .load()
                    .level_info
                    .load()
                    .game_rules
                    .tnt_explodes
                {
                    minecart.prime(&self.vehicle.entity, fuse);
                } else {
                    minecart.set_fuse(fuse);
                }
                return true;
            }
        }

        let damaged = self.vehicle.damage_with_context(amount, source);

        if will_break && !creative && self.vehicle.entity.is_removed() {
            let world = self.vehicle.entity.world.load();
            if world.level_info.load().game_rules.entity_drops {
                let position = self.vehicle.entity.block_pos.load();
                if let Some(container) = self.container()
                    && container.claim_drops()
                {
                    container.unpack_loot();
                    let inventory: Arc<dyn Inventory> = container.clone();
                    world.scatter_inventory(&position, &inventory);
                }
                if let Some(item) = self.drop_item() {
                    world.drop_stack(&position, ItemStack::new(1, item));
                }
            }
        }

        damaged
    }

    fn interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        match &self.kind {
            MinecartKind::Chest(minecart) => {
                let custom_name = self.vehicle.entity.custom_name.load().as_ref().clone();
                minecart.interact(custom_name, player);
                true
            }
            MinecartKind::Furnace(minecart) => {
                minecart.interact(&self.vehicle.entity, player, item_stack)
            }
            MinecartKind::Hopper(minecart) => {
                let custom_name = self.vehicle.entity.custom_name.load().as_ref().clone();
                minecart.interact(custom_name, player);
                true
            }
            MinecartKind::Rideable(_) => RideableMinecart::interact(&self.vehicle.entity, player),
            MinecartKind::Tnt(_) | MinecartKind::Other => false,
        }
    }

    fn move_entity(&self, caller: &dyn EntityBase, motion: Vector3<f64>) {
        // `AbstractMinecart.move` for the old behavior is a plain move; entity
        // push/pickup happens at the end of the tick (`OldMinecartBehavior.java:85`).
        self.vehicle.entity.move_entity(caller, motion);
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_rails_count_as_rails() {
        for block in [
            &Block::RAIL,
            &Block::POWERED_RAIL,
            &Block::DETECTOR_RAIL,
            &Block::ACTIVATOR_RAIL,
        ] {
            assert!(is_rail(block, block.default_state.id), "{}", block.name);
        }
        assert!(!is_rail(&Block::STONE, Block::STONE.default_state.id));
        assert!(!is_rail(&Block::AIR, Block::AIR.default_state.id));
    }

    #[test]
    fn rail_shape_reads_the_shape_property() {
        let shape = rail_shape(&Block::RAIL, Block::RAIL.default_state.id);
        assert!(shape.is_some());
        assert!(matches!(
            shape,
            Some(
                RailShape::NorthSouth
                    | RailShape::EastWest
                    | RailShape::AscendingEast
                    | RailShape::AscendingWest
                    | RailShape::AscendingNorth
                    | RailShape::AscendingSouth
            )
        ));
        // A block without a `shape` property is not a rail shape.
        assert!(rail_shape(&Block::STONE, Block::STONE.default_state.id).is_none());
    }

    /// Vanilla `OldMinecartBehavior.getPos`: straight rails put the cart exactly on the
    /// block centre line, preserving the coordinate along the rail.
    #[test]
    fn rail_snap_centres_straight_rails() {
        let block = BlockPos::new(3, 64, -7);

        let snapped = rail_snap_position(
            block,
            RailShape::NorthSouth,
            Vector3::new(3.9, 64.0625, -6.4),
        );
        assert_eq!(snapped.x, 3.5);
        assert_eq!(snapped.z, -6.4);
        assert_eq!(snapped.y, 64.0625);

        let snapped = rail_snap_position(
            block,
            RailShape::EastWest,
            Vector3::new(3.9, 64.0625, -6.4),
        );
        assert_eq!(snapped.x, 3.9);
        assert_eq!(snapped.z, -6.5);
        assert_eq!(snapped.y, 64.0625);
    }

    /// A curve snaps onto the diagonal between its two exits.
    #[test]
    fn rail_snap_projects_onto_curves() {
        let block = BlockPos::new(0, 0, 0);
        let snapped = rail_snap_position(
            block,
            RailShape::SouthEast,
            Vector3::new(0.75, 0.0625, 0.75),
        );
        assert!((snapped.x - 0.75).abs() < 1e-9);
        assert!((snapped.z - 0.75).abs() < 1e-9);
        assert_eq!(snapped.y, 0.0625);
    }

    /// Vanilla's slope adjustment (`getPos`): the low end of an ascending east rail sits at
    /// `blockY + 0.0625`, the high end at `blockY + 1.0625`.
    #[test]
    fn rail_snap_follows_slopes() {
        let block = BlockPos::new(0, 0, 0);

        let low = rail_snap_position(
            block,
            RailShape::AscendingEast,
            Vector3::new(0.0, 0.0625, 0.5),
        );
        assert!((low.y - 0.0625).abs() < 1e-9);

        let high = rail_snap_position(
            block,
            RailShape::AscendingEast,
            Vector3::new(1.0, 0.5625, 0.5),
        );
        assert!((high.y - 1.0625).abs() < 1e-9);
    }

    /// Vanilla `AbstractMinecart.push` caps the impulse at `0.05` per axis
    /// (`0.1 * 0.5` after normalisation), independent of how close the entities are.
    #[test]
    fn push_impulse_matches_vanilla() {
        let origin = Vector3::new(0.0, 0.0, 0.0);

        let (xa, za) = minecart_push_impulse(origin, Vector3::new(1.0, 0.0, 0.0)).unwrap();
        assert!((xa - 0.05).abs() < 1e-12);
        assert_eq!(za, 0.0);

        let (xa, za) = minecart_push_impulse(origin, Vector3::new(0.1, 0.0, 0.0)).unwrap();
        assert!((xa - 0.05).abs() < 1e-12);
        assert_eq!(za, 0.0);

        // Below vanilla's 1.0E-4 squared-distance threshold there is no push.
        assert!(minecart_push_impulse(origin, Vector3::new(0.005, 0.0, 0.0)).is_none());
    }
}
