//! Entity types registered by plugins on top of the vanilla ones (the `minecraft:entity_type`
//! registry).
//!
//! A type registered as the n-th gets the network id `vanilla count + n`, the id a modded client's
//! registry sync announces for its entity type, and the same id in `ClientboundAddEntity`. The
//! registry is append only and closes before players can connect, see [`crate::plugin_registry`].
//!
//! The generated [`EntityType`] is what the rest of the server works with, so every registered type
//! has one (leaked, like the entry itself). Its `resource_name` is the full key
//! (`namespace:path`) instead of the path of a vanilla type, use [`EntityType::resource_location`]
//! and [`EntityType::translation_key`] where the name is written out.

use std::borrow::Cow;
use std::sync::{LazyLock, PoisonError, RwLock};

use pumpkin_util::HeightMap;

use crate::entity::{EntityType, MobCategory, SpawnLocation, SpawnRestriction};
use crate::plugin_registry::{PluginRegistry, RegistryError};

/// Entity type ids are a `VarInt`, `EntityType::id` a `u16`.
const MAX_PLUGIN_ENTITY_TYPES: usize = 1024;
/// A bound for the size of an entity, in blocks.
const MAX_ENTITY_SIZE: f32 = 64.0;
/// A bound for the tracking range, in chunks.
const MAX_TRACKING_RANGE: u32 = 256;
/// A bound for the blobs a type keeps, the entity metadata of a packet has to stay small.
const MAX_SYNCED_DATA: usize = 8192;
/// Vanilla's `EntityType.getDefaultEyeHeight` is 85% of the height.
const DEFAULT_EYE_HEIGHT_FACTOR: f32 = 0.85;

/// The spawn category of a type (vanilla `MobCategory`). It is only used for the category counts
/// of natural spawning, which never spawns plugin types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginEntityCategory {
    Misc,
    Creature,
    Monster,
    Ambient,
    Axolotls,
    UndergroundWaterCreature,
    WaterCreature,
    WaterAmbient,
}

impl PluginEntityCategory {
    const fn mob_category(self) -> &'static MobCategory {
        match self {
            Self::Misc => &MobCategory::MISC,
            Self::Creature => &MobCategory::CREATURE,
            Self::Monster => &MobCategory::MONSTER,
            Self::Ambient => &MobCategory::AMBIENT,
            Self::Axolotls => &MobCategory::AXOLOTLS,
            Self::UndergroundWaterCreature => &MobCategory::UNDERGROUND_WATER_CREATURE,
            Self::WaterCreature => &MobCategory::WATER_CREATURE,
            Self::WaterAmbient => &MobCategory::WATER_AMBIENT,
        }
    }
}

/// What a plugin gives when it registers an entity type.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginEntityDefinition {
    pub category: PluginEntityCategory,
    /// The width of the hitbox, in blocks.
    pub width: f32,
    /// The height of the hitbox, in blocks.
    pub height: f32,
    /// The height of the eyes above the feet. `None` is 85% of the height.
    pub eye_height: Option<f32>,
    pub fire_immune: bool,
    /// How far clients see the entity, in chunks (vanilla `clientTrackingRange`). 0 means the
    /// entity is never sent to clients.
    pub tracking_range: u32,
    /// Ticks between the position updates sent to clients (vanilla `updateInterval`).
    pub update_interval: u32,
    /// Whether movement is sent as deltas (vanilla `trackDeltas`).
    pub track_deltas: bool,
    /// Whether `/summon` may create it.
    pub summonable: bool,
    /// Whether the entity is written to the chunk.
    pub saveable: bool,
    /// Whether players can target the entity (attack, interact), vanilla `isPickable`.
    pub pickable: bool,
    /// Whether other entities collide with it, like with a boat, vanilla `canBeCollidedWith`.
    pub collidable: bool,
    /// The synced data (the entity metadata entries, without the 0xFF end marker) of a new entity.
    pub default_synced_data: Vec<u8>,
    /// The channel of the custom payload the server sends right after the add entity packet to
    /// every client that starts to track an entity of the type, if any. The payload is the entity's
    /// `VarInt` id followed by its spawn data. This is what NeoForge's `advanced_add_entity`
    /// payload carries for an entity with complex spawn data.
    pub spawn_data_channel: Option<String>,
}

/// An entity type that a plugin registered.
pub struct PluginEntityType {
    /// The full resource location, `namespace:path`. Also the `id` saved in the chunk.
    pub key: &'static str,
    /// The network id: the vanilla count plus the registration index.
    pub id: u32,
    pub definition: PluginEntityDefinition,
    /// The type as the rest of the server sees it, its `id` is [`Self::id`].
    pub entity_type: &'static EntityType,
}

static REGISTRY: LazyLock<RwLock<PluginRegistry<PluginEntityType>>> =
    LazyLock::new(|| RwLock::new(PluginRegistry::new(MAX_PLUGIN_ENTITY_TYPES)));

fn read_registry() -> std::sync::RwLockReadGuard<'static, PluginRegistry<PluginEntityType>> {
    REGISTRY.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_registry() -> std::sync::RwLockWriteGuard<'static, PluginRegistry<PluginEntityType>> {
    REGISTRY.write().unwrap_or_else(PoisonError::into_inner)
}

/// Number of vanilla entity types. Plugin types get the ids from here on.
#[must_use]
pub const fn vanilla_entity_type_count() -> u32 {
    EntityType::ALL.len() as u32
}

fn validate(definition: &PluginEntityDefinition) -> Result<(), RegistryError> {
    let invalid = |message: &str| Err(RegistryError::InvalidDefinition(message.to_string()));
    let size_ok = |size: f32| size.is_finite() && (0.0..=MAX_ENTITY_SIZE).contains(&size);
    if !size_ok(definition.width) || !size_ok(definition.height) {
        return invalid("width and height have to be between 0 and 64 blocks");
    }
    if let Some(eye_height) = definition.eye_height
        && !size_ok(eye_height)
    {
        return invalid("the eye height has to be between 0 and 64 blocks");
    }
    if definition.tracking_range > MAX_TRACKING_RANGE {
        return invalid("the tracking range is at most 256 chunks");
    }
    if definition.update_interval == 0 {
        return invalid("the update interval is at least 1 tick");
    }
    if definition.default_synced_data.len() > MAX_SYNCED_DATA {
        return invalid("the default synced data is at most 8192 bytes");
    }
    if let Some(channel) = &definition.spawn_data_channel
        && channel.split_once(':').is_none_or(|(namespace, path)| {
            namespace.is_empty() || path.is_empty() || channel.len() > 256
        })
    {
        return invalid("the spawn data channel has to be a namespace:path resource location");
    }
    Ok(())
}

fn build_entity_type(
    id: u32,
    key: &'static str,
    definition: &PluginEntityDefinition,
) -> &'static EntityType {
    let eye_height = definition
        .eye_height
        .unwrap_or(definition.height * DEFAULT_EYE_HEIGHT_FACTOR);
    Box::leak(Box::new(EntityType {
        id: id as u16,
        attributes: &[],
        experience_reward: 0,
        hurt_sound: None,
        death_sound: None,
        attackable: Some(definition.pickable),
        mob: false,
        is_enemy: false,
        saveable: definition.saveable,
        limit_per_chunk: 0,
        summonable: definition.summonable,
        fire_immune: definition.fire_immune,
        category: definition.category.mob_category(),
        can_spawn_far_from_player: true,
        client_tracking_range: definition.tracking_range,
        update_interval: definition.update_interval,
        track_deltas: definition.track_deltas,
        dimension: [definition.width, definition.height],
        eye_height,
        spawn_dimensions_scale: 1.0,
        spawn_restriction: SpawnRestriction {
            location: SpawnLocation::Unrestricted,
            heightmap: HeightMap::MotionBlockingNoLeaves,
        },
        resource_name: key,
    }))
}

/// Registers an entity type. Registering the same definition again returns the existing type, a
/// different one for a known key is [`RegistryError::Duplicate`].
pub fn register_plugin_entity_type(
    key: &str,
    definition: PluginEntityDefinition,
) -> Result<&'static PluginEntityType, RegistryError> {
    validate(&definition)?;
    let vanilla_count = vanilla_entity_type_count();
    let known = definition.clone();
    write_registry().register(
        key,
        |existing| existing.definition == known,
        move |index, key| {
            let id = vanilla_count + index as u32;
            PluginEntityType {
                key,
                id,
                entity_type: build_entity_type(id, key, &definition),
                definition,
            }
        },
    )
}

/// Closes the registry. Later registrations fail with [`RegistryError::Frozen`].
pub fn freeze_plugin_entity_types() {
    write_registry().freeze();
}

#[must_use]
pub fn is_plugin_entity_registry_frozen() -> bool {
    read_registry().is_frozen()
}

/// The registered types in id order.
#[must_use]
pub fn plugin_entity_types() -> Vec<&'static PluginEntityType> {
    read_registry().entries().to_vec()
}

#[must_use]
pub fn plugin_entity_type_by_key(key: &str) -> Option<&'static PluginEntityType> {
    read_registry().get_by_key(key)
}

#[must_use]
pub fn plugin_entity_type_by_id(id: u32) -> Option<&'static PluginEntityType> {
    let index = id.checked_sub(vanilla_entity_type_count())?;
    read_registry().get_by_index(index as usize)
}

/// Looks an entity type up by network id, vanilla or plugin.
#[must_use]
pub fn entity_type_by_id(id: u32) -> Option<&'static EntityType> {
    u16::try_from(id)
        .ok()
        .and_then(EntityType::from_raw)
        .or_else(|| plugin_entity_type_by_id(id).map(|plugin| plugin.entity_type))
}

/// Looks an entity type up by resource location: a vanilla type (the `minecraft:` prefix is
/// optional) or a plugin one. This is also how the `id` saved with an entity resolves.
#[must_use]
pub fn entity_type_by_key(key: &str) -> Option<&'static EntityType> {
    match key.strip_prefix("minecraft:") {
        Some(path) => EntityType::from_name(path),
        None if key.contains(':') => plugin_entity_type_by_key(key).map(|plugin| plugin.entity_type),
        None => EntityType::from_name(key),
    }
}

impl EntityType {
    /// Whether a plugin registered this type.
    #[must_use]
    pub const fn is_plugin(&self) -> bool {
        self.id as u32 >= vanilla_entity_type_count()
    }

    /// The plugin registration of this type, `None` for a vanilla type.
    #[must_use]
    pub fn plugin_type(&self) -> Option<&'static PluginEntityType> {
        if self.is_plugin() {
            plugin_entity_type_by_id(u32::from(self.id))
        } else {
            None
        }
    }

    /// The full resource location: `minecraft:` prefixed for vanilla types.
    #[must_use]
    pub fn resource_location(&self) -> Cow<'static, str> {
        if self.is_plugin() {
            Cow::Borrowed(self.resource_name)
        } else {
            Cow::Owned(format!("minecraft:{}", self.resource_name))
        }
    }

    /// The translation key of the name: `entity.<namespace>.<path>` with `/` as `.`.
    #[must_use]
    pub fn translation_key(&self) -> String {
        match self.resource_name.split_once(':') {
            Some((namespace, path)) if self.is_plugin() => {
                format!("entity.{namespace}.{}", path.replace('/', "."))
            }
            _ => format!("entity.minecraft.{}", self.resource_name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> PluginEntityDefinition {
        PluginEntityDefinition {
            category: PluginEntityCategory::Misc,
            width: 1.0,
            height: 2.0,
            eye_height: None,
            fire_immune: true,
            tracking_range: 10,
            update_interval: 3,
            track_deltas: true,
            summonable: false,
            saveable: true,
            pickable: true,
            collidable: false,
            default_synced_data: vec![],
            spawn_data_channel: None,
        }
    }

    #[test]
    fn registers_after_the_vanilla_types() {
        let first = register_plugin_entity_type("test_entities:seat", definition()).unwrap();
        assert_eq!(first.id, vanilla_entity_type_count());
        assert_eq!(u32::from(first.entity_type.id), first.id);
        assert!(first.entity_type.is_plugin());
        assert_eq!(first.entity_type.resource_location(), "test_entities:seat");
        assert_eq!(
            first.entity_type.translation_key(),
            "entity.test_entities.seat"
        );
        assert_eq!(first.entity_type.dimension, [1.0, 2.0]);
        assert!((first.entity_type.eye_height - 1.7).abs() < 1e-6);

        let again = register_plugin_entity_type("test_entities:seat", definition()).unwrap();
        assert!(std::ptr::eq(first, again));
        let mut other = definition();
        other.width = 2.0;
        assert!(matches!(
            register_plugin_entity_type("test_entities:seat", other),
            Err(RegistryError::Duplicate(_))
        ));

        let found = entity_type_by_key("test_entities:seat").unwrap();
        assert!(std::ptr::eq(found, first.entity_type));
        assert!(std::ptr::eq(
            entity_type_by_id(first.id).unwrap(),
            first.entity_type
        ));
        assert_eq!(
            entity_type_by_key("minecraft:marker").map(|t| t.id),
            Some(EntityType::MARKER.id)
        );
        assert!(!EntityType::MARKER.is_plugin());
        assert_eq!(EntityType::MARKER.resource_location(), "minecraft:marker");
    }

    #[test]
    fn rejects_bad_definitions() {
        let mut bad = definition();
        bad.height = -1.0;
        assert!(matches!(
            register_plugin_entity_type("test_entities:bad", bad),
            Err(RegistryError::InvalidDefinition(_))
        ));
        let mut bad = definition();
        bad.spawn_data_channel = Some("nocolon".to_string());
        assert!(matches!(
            register_plugin_entity_type("test_entities:bad", bad),
            Err(RegistryError::InvalidDefinition(_))
        ));
    }
}
