use crate::pumpkin::plugin::{
    common::EntityPose, entity_flags::Host, player::Player as WitPlayer, world::Entity as WitEntity,
};
use pumpkin_core::entity::{Entity, plugin_flags::flag_from_key};
use pumpkin_core::plugin::api::events::entity::set_item_entity_tick_interval;
use pumpkin_core::plugin::permissions;
use pumpkin_data::entity::EntityPose as InternalEntityPose;
use pumpkin_data::item::Item;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_wasm_host_common::state::PluginHostState;
use wasmtime::component::Resource;

fn unknown_key(key: &str) -> String {
    format!("unknown entity flag '{key}'")
}

fn permission_error() -> String {
    format!(
        "entity flags need the '{}' permission",
        permissions::ENTITY_FLAGS
    )
}

const fn internal_pose(pose: EntityPose) -> InternalEntityPose {
    match pose {
        EntityPose::Standing => InternalEntityPose::Standing,
        EntityPose::FallFlying => InternalEntityPose::FallFlying,
        EntityPose::Sleeping => InternalEntityPose::Sleeping,
        EntityPose::Swimming => InternalEntityPose::Swimming,
        EntityPose::SpinAttack => InternalEntityPose::SpinAttack,
        EntityPose::Crouching => InternalEntityPose::Crouching,
        EntityPose::LongJumping => InternalEntityPose::LongJumping,
        EntityPose::Dying => InternalEntityPose::Dying,
        EntityPose::Croaking => InternalEntityPose::Croaking,
        EntityPose::UsingTongue => InternalEntityPose::UsingTongue,
        EntityPose::Sitting => InternalEntityPose::Sitting,
        EntityPose::Roaring => InternalEntityPose::Roaring,
        EntityPose::Sniffing => InternalEntityPose::Sniffing,
        EntityPose::Emerging => InternalEntityPose::Emerging,
        EntityPose::Digging => InternalEntityPose::Digging,
        EntityPose::Sliding => InternalEntityPose::Sliding,
        EntityPose::Shooting => InternalEntityPose::Shooting,
        EntityPose::Inhaling => InternalEntityPose::Inhaling,
    }
}

fn set_flag(entity: &Entity, key: &str, value: bool) -> Result<(), String> {
    let flag = flag_from_key(key).ok_or_else(|| unknown_key(key))?;
    entity.plugin_flags.set(flag, value);
    Ok(())
}

fn get_flag(entity: &Entity, key: &str) -> Result<bool, String> {
    let flag = flag_from_key(key).ok_or_else(|| unknown_key(key))?;
    Ok(entity.plugin_flags.has(flag))
}

impl Host for PluginHostState {
    fn set_flag(
        &mut self,
        entity: Resource<WitEntity>,
        key: String,
        value: bool,
    ) -> wasmtime::Result<Result<(), String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::ENTITY_FLAGS)
        {
            return Ok(Err(permission_error()));
        }
        let entity = self.get(&entity)?;
        Ok(set_flag(entity.get_entity(), &key, value))
    }

    fn get_flag(
        &mut self,
        entity: Resource<WitEntity>,
        key: String,
    ) -> wasmtime::Result<Result<bool, String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::ENTITY_FLAGS)
        {
            return Ok(Err(permission_error()));
        }
        let entity = self.get(&entity)?;
        Ok(get_flag(entity.get_entity(), &key))
    }

    fn set_player_flag(
        &mut self,
        player: Resource<WitPlayer>,
        key: String,
        value: bool,
    ) -> wasmtime::Result<Result<(), String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::ENTITY_FLAGS)
        {
            return Ok(Err(permission_error()));
        }
        let player = self.get(&player)?;
        Ok(set_flag(&player.living_entity.entity, &key, value))
    }

    fn get_player_flag(
        &mut self,
        player: Resource<WitPlayer>,
        key: String,
    ) -> wasmtime::Result<Result<bool, String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::ENTITY_FLAGS)
        {
            return Ok(Err(permission_error()));
        }
        let player = self.get(&player)?;
        Ok(get_flag(&player.living_entity.entity, &key))
    }

    fn set_size_override(
        &mut self,
        entity: Resource<WitEntity>,
        size: Option<(f32, f32)>,
    ) -> wasmtime::Result<Result<(), String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::ENTITY_FLAGS)
        {
            return Ok(Err(permission_error()));
        }
        if size.is_some_and(|(width, height)| !(width > 0.0 && height > 0.0)) {
            return Ok(Err("the size must be positive".to_string()));
        }
        let entity = self.get(&entity)?;
        let entity = entity.get_entity();
        entity.plugin_flags.set_size_override(size);
        entity.refresh_dimensions();
        Ok(Ok(()))
    }

    fn set_pickup_box_override(
        &mut self,
        player: Resource<WitPlayer>,
        reach: Option<(f64, f64, f64)>,
    ) -> wasmtime::Result<Result<(), String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::ENTITY_FLAGS)
        {
            return Ok(Err(permission_error()));
        }
        if reach.is_some_and(|(x, y, z)| !(x >= 0.0 && y >= 0.0 && z >= 0.0)) {
            return Ok(Err("the reach must not be negative".to_string()));
        }
        let player = self.get(&player)?;
        player
            .living_entity
            .entity
            .plugin_flags
            .set_pickup_box_override(reach.map(|(x, y, z)| Vector3::new(x, y, z)));
        Ok(Ok(()))
    }

    fn set_pose_override(
        &mut self,
        entity: Resource<WitEntity>,
        pose: Option<EntityPose>,
    ) -> wasmtime::Result<Result<(), String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::ENTITY_FLAGS)
        {
            return Ok(Err(permission_error()));
        }
        let entity = self.get(&entity)?;
        let entity = entity.get_entity();
        let pose = pose.map(internal_pose);
        entity.plugin_flags.set_pose_override(pose);
        entity.set_pose(pose.unwrap_or(InternalEntityPose::Standing));
        Ok(Ok(()))
    }

    fn set_item_entity_ticking(
        &mut self,
        item_key: String,
        interval: u32,
    ) -> wasmtime::Result<bool> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::ENTITY_FLAGS)
        {
            return Ok(false);
        }
        let key = item_key.strip_prefix("minecraft:").unwrap_or(&item_key);
        let Some(item) = Item::from_registry_key(key) else {
            return Ok(false);
        };
        set_item_entity_tick_interval(item.id, interval);
        Ok(true)
    }
}
