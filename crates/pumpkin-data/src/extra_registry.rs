//! Sound events, particle types, map decoration types, datapack registries
//! and plugin owned registries that plugins add at runtime.
//!
//! The sound event, particle type and map decoration type registries are synced
//! registries: an entry registered as the n-th gets the id `vanilla count + n` and the registry
//! closes before players can connect, see [`crate::plugin_registry`]. Datapack registries and
//! plugin owned registries are plain lists that are sent to NeoForge clients.

use std::sync::{LazyLock, Mutex, PoisonError, RwLock};

use crate::plugin_registry::{PluginRegistry, RegistryError};

const MAX_PLUGIN_ENTRIES: usize = 4096;
/// The most bytes of an entry of a datapack registry.
pub const MAX_DATAPACK_ENTRY_BYTES: usize = 1 << 20;
/// The particle option formats the host can read.
pub const PARTICLE_FORMATS: [&str; 4] = ["none", "bytes", "color", "block-state"];

/// A sound event a plugin registered.
pub struct PluginSound {
    /// The full resource location, `namespace:path`.
    pub key: &'static str,
    /// The distance the sound is heard at, none for the default of the volume.
    pub fixed_range: Option<f32>,
    /// The registration index, the id is the vanilla count plus this.
    pub index: usize,
}

/// A particle type a plugin registered.
pub struct PluginParticle {
    pub key: &'static str,
    /// Whether the particle ignores the particle limit of the client.
    pub override_limiter: bool,
    /// How the options are encoded, one of [`PARTICLE_FORMATS`].
    pub options_format: &'static str,
    pub index: usize,
}

/// A map decoration type a plugin registered.
pub struct PluginMapDecoration {
    pub key: &'static str,
    /// The texture asset name, a resource location path.
    pub asset_name: &'static str,
    pub show_on_item_frame: bool,
    pub map_color: i32,
    pub exploration_map_element: bool,
    pub track_count: bool,
    pub index: usize,
}

static SOUNDS: LazyLock<RwLock<PluginRegistry<PluginSound>>> =
    LazyLock::new(|| RwLock::new(PluginRegistry::new(MAX_PLUGIN_ENTRIES)));
static PARTICLES: LazyLock<RwLock<PluginRegistry<PluginParticle>>> =
    LazyLock::new(|| RwLock::new(PluginRegistry::new(MAX_PLUGIN_ENTRIES)));
static MAP_DECORATIONS: LazyLock<RwLock<PluginRegistry<PluginMapDecoration>>> =
    LazyLock::new(|| RwLock::new(PluginRegistry::new(MAX_PLUGIN_ENTRIES)));

macro_rules! guard {
    ($registry:expr, $read:ident, $write:ident, $ty:ty) => {
        fn $read() -> std::sync::RwLockReadGuard<'static, PluginRegistry<$ty>> {
            $registry.read().unwrap_or_else(PoisonError::into_inner)
        }
        fn $write() -> std::sync::RwLockWriteGuard<'static, PluginRegistry<$ty>> {
            $registry.write().unwrap_or_else(PoisonError::into_inner)
        }
    };
}
guard!(SOUNDS, read_sounds, write_sounds, PluginSound);
guard!(PARTICLES, read_particles, write_particles, PluginParticle);
guard!(
    MAP_DECORATIONS,
    read_decorations,
    write_decorations,
    PluginMapDecoration
);

// --- vanilla counts ---

#[cfg(feature = "sound")]
#[must_use]
pub fn vanilla_sound_count() -> usize {
    crate::sound::Sound::NAMES.len()
}

#[cfg(feature = "particle")]
#[must_use]
pub fn vanilla_particle_count() -> usize {
    (0..=u16::MAX)
        .take_while(|id| crate::particle::Particle::from_id(*id).is_some())
        .count()
}

#[cfg(feature = "map_decoration")]
#[must_use]
pub fn vanilla_map_decoration_count() -> usize {
    crate::map_decoration::MapDecorationType::ALL.len()
}

// --- sounds ---

/// Registers a sound event. The same key and range again returns the existing one.
pub fn register_plugin_sound(
    key: &str,
    fixed_range: Option<f32>,
) -> Result<&'static PluginSound, RegistryError> {
    if let Some(range) = fixed_range
        && (!range.is_finite() || range <= 0.0)
    {
        return Err(RegistryError::InvalidDefinition(
            "the fixed range must be a positive number".to_string(),
        ));
    }
    write_sounds().register(
        key,
        |existing| existing.fixed_range == fixed_range,
        |index, key| PluginSound {
            key,
            fixed_range,
            index,
        },
    )
}

#[must_use]
pub fn plugin_sounds() -> Vec<&'static PluginSound> {
    read_sounds().entries().to_vec()
}

#[must_use]
pub fn plugin_sound_by_key(key: &str) -> Option<&'static PluginSound> {
    read_sounds().get_by_key(key)
}

pub fn freeze_plugin_sounds() {
    write_sounds().freeze();
}

// --- particles ---

/// Registers a particle type. The same definition again returns the existing one.
pub fn register_plugin_particle(
    key: &str,
    override_limiter: bool,
    options_format: &str,
) -> Result<&'static PluginParticle, RegistryError> {
    let Some(format) = PARTICLE_FORMATS.iter().find(|f| **f == options_format) else {
        return Err(RegistryError::InvalidDefinition(format!(
            "unknown options format '{options_format}', expected one of {PARTICLE_FORMATS:?}"
        )));
    };
    write_particles().register(
        key,
        |existing| {
            existing.override_limiter == override_limiter && existing.options_format == *format
        },
        |index, key| PluginParticle {
            key,
            override_limiter,
            options_format: format,
            index,
        },
    )
}

#[must_use]
pub fn plugin_particles() -> Vec<&'static PluginParticle> {
    read_particles().entries().to_vec()
}

#[must_use]
pub fn plugin_particle_by_key(key: &str) -> Option<&'static PluginParticle> {
    read_particles().get_by_key(key)
}

pub fn freeze_plugin_particles() {
    write_particles().freeze();
}

// --- map decorations ---

/// What a plugin gives to define a map decoration type.
pub struct MapDecorationRegistration {
    pub asset_name: String,
    pub show_on_item_frame: bool,
    pub map_color: i32,
    pub exploration_map_element: bool,
    pub track_count: bool,
}

/// Registers a map decoration type. The same definition again returns the existing one.
pub fn register_plugin_map_decoration(
    key: &str,
    registration: MapDecorationRegistration,
) -> Result<&'static PluginMapDecoration, RegistryError> {
    if registration.asset_name.is_empty() {
        return Err(RegistryError::InvalidDefinition(
            "the asset name is empty".to_string(),
        ));
    }
    write_decorations().register(
        key,
        |existing| {
            existing.asset_name == registration.asset_name
                && existing.show_on_item_frame == registration.show_on_item_frame
                && existing.map_color == registration.map_color
                && existing.exploration_map_element == registration.exploration_map_element
                && existing.track_count == registration.track_count
        },
        |index, key| PluginMapDecoration {
            key,
            asset_name: Box::leak(registration.asset_name.clone().into_boxed_str()),
            show_on_item_frame: registration.show_on_item_frame,
            map_color: registration.map_color,
            exploration_map_element: registration.exploration_map_element,
            track_count: registration.track_count,
            index,
        },
    )
}

#[must_use]
pub fn plugin_map_decorations() -> Vec<&'static PluginMapDecoration> {
    read_decorations().entries().to_vec()
}

#[must_use]
pub fn plugin_map_decoration_by_key(key: &str) -> Option<&'static PluginMapDecoration> {
    read_decorations().get_by_key(key)
}

pub fn freeze_plugin_map_decorations() {
    write_decorations().freeze();
}

// --- datapack registries and plugin owned registries ---

/// A registry whose entries are datapack data (`register-datapack-registry`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginDatapackRegistry {
    pub key: String,
    /// What the entries are, for the plugin's own bookkeeping (`nbt` for network NBT entries).
    pub kind: String,
    /// The entries: key and network NBT, none for an entry whose data the client has.
    pub entries: Vec<(String, Option<Vec<u8>>)>,
}

/// A registry a plugin owns (`create:fan_processing_type` style) and its entries in id order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginOwnedRegistry {
    pub key: String,
    pub entries: Vec<String>,
}

#[derive(Default)]
struct Extra {
    frozen: bool,
    datapack: Vec<PluginDatapackRegistry>,
    owned: Vec<PluginOwnedRegistry>,
}

static EXTRA: LazyLock<Mutex<Extra>> = LazyLock::new(|| Mutex::new(Extra::default()));

fn extra() -> std::sync::MutexGuard<'static, Extra> {
    EXTRA.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Checks a `namespace:path` location (any namespace).
pub fn check_location(key: &str) -> Result<(), RegistryError> {
    match crate::plugin_registry::split_plugin_key(key) {
        Ok(_) => Ok(()),
        Err(RegistryError::ReservedNamespace(_)) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Declares a datapack registry. Again with the same kind it is a no-op.
pub fn register_datapack_registry(key: &str, kind: &str) -> Result<(), RegistryError> {
    check_location(key)?;
    let mut extra = extra();
    if let Some(existing) = extra.datapack.iter().find(|r| r.key == key) {
        return if existing.kind == kind {
            Ok(())
        } else {
            Err(RegistryError::Duplicate(key.to_string()))
        };
    }
    if extra.frozen {
        return Err(RegistryError::Frozen);
    }
    extra.datapack.push(PluginDatapackRegistry {
        key: key.to_string(),
        kind: kind.to_string(),
        entries: Vec::new(),
    });
    Ok(())
}

/// Adds an entry to a datapack registry declared with [`register_datapack_registry`].
pub fn add_datapack_entry(
    registry: &str,
    id: &str,
    data: Option<Vec<u8>>,
) -> Result<(), RegistryError> {
    check_location(id)?;
    if data.as_ref().is_some_and(|d| d.len() > MAX_DATAPACK_ENTRY_BYTES) {
        return Err(RegistryError::InvalidDefinition(
            "the entry data is too large".to_string(),
        ));
    }
    let mut extra = extra();
    let frozen = extra.frozen;
    let Some(target) = extra.datapack.iter_mut().find(|r| r.key == registry) else {
        return Err(RegistryError::InvalidDefinition(format!(
            "unknown datapack registry '{registry}'"
        )));
    };
    if let Some(existing) = target.entries.iter().find(|(key, _)| key == id) {
        return if existing.1 == data {
            Ok(())
        } else {
            Err(RegistryError::Duplicate(id.to_string()))
        };
    }
    if frozen {
        return Err(RegistryError::Frozen);
    }
    target.entries.push((id.to_string(), data));
    Ok(())
}

#[must_use]
pub fn plugin_datapack_registries() -> Vec<PluginDatapackRegistry> {
    extra().datapack.clone()
}

/// Declares a plugin owned synced registry. The `minecraft` namespace is reserved.
pub fn register_owned_registry(key: &str) -> Result<(), RegistryError> {
    crate::plugin_registry::split_plugin_key(key)?;
    let mut extra = extra();
    if extra.owned.iter().any(|r| r.key == key) {
        return Ok(());
    }
    if extra.frozen {
        return Err(RegistryError::Frozen);
    }
    extra.owned.push(PluginOwnedRegistry {
        key: key.to_string(),
        entries: Vec::new(),
    });
    Ok(())
}

/// Adds an entry to a plugin owned registry and returns its id.
pub fn add_owned_registry_entry(registry: &str, entry: &str) -> Result<usize, RegistryError> {
    check_location(entry)?;
    let mut extra = extra();
    let frozen = extra.frozen;
    let Some(target) = extra.owned.iter_mut().find(|r| r.key == registry) else {
        return Err(RegistryError::InvalidDefinition(format!(
            "unknown registry '{registry}'"
        )));
    };
    if let Some(index) = target.entries.iter().position(|e| e == entry) {
        return Ok(index);
    }
    if frozen {
        return Err(RegistryError::Frozen);
    }
    if target.entries.len() >= MAX_PLUGIN_ENTRIES {
        return Err(RegistryError::Full);
    }
    target.entries.push(entry.to_string());
    Ok(target.entries.len() - 1)
}

#[must_use]
pub fn plugin_owned_registries() -> Vec<PluginOwnedRegistry> {
    extra().owned.clone()
}

#[must_use]
pub fn owned_registry_entry_id(registry: &str, entry: &str) -> Option<usize> {
    extra()
        .owned
        .iter()
        .find(|r| r.key == registry)?
        .entries
        .iter()
        .position(|e| e == entry)
}

/// Closes the datapack and plugin owned registries.
pub fn freeze_extra_registries() {
    extra().frozen = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datapack_registry_entries() {
        register_datapack_registry("extra_test:things", "nbt").unwrap();
        assert!(register_datapack_registry("extra_test:things", "other").is_err());
        add_datapack_entry("extra_test:things", "extra_test:a", Some(vec![10, 0])).unwrap();
        add_datapack_entry("extra_test:things", "extra_test:a", Some(vec![10, 0])).unwrap();
        assert!(add_datapack_entry("extra_test:things", "extra_test:a", None).is_err());
        assert!(add_datapack_entry("extra_test:unknown", "extra_test:a", None).is_err());
    }

    #[test]
    fn owned_registry_ids() {
        assert!(register_owned_registry("minecraft:x").is_err());
        register_owned_registry("extra_test:fan").unwrap();
        assert_eq!(add_owned_registry_entry("extra_test:fan", "extra_test:a"), Ok(0));
        assert_eq!(add_owned_registry_entry("extra_test:fan", "extra_test:b"), Ok(1));
        assert_eq!(add_owned_registry_entry("extra_test:fan", "extra_test:a"), Ok(0));
        assert_eq!(owned_registry_entry_id("extra_test:fan", "extra_test:b"), Some(1));
    }

    #[test]
    fn particles_validate_the_format() {
        assert!(register_plugin_particle("extra_test:p", false, "weird").is_err());
        let p = register_plugin_particle("extra_test:p", true, "color").unwrap();
        assert_eq!(p.options_format, "color");
    }
}
