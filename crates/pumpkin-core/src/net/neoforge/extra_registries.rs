//! The sound event, particle type, mob effect and map decoration type registries, the
//! registries plugins own and the datapack registry entries plugins added, for the handshake.
//!
//! The vanilla entries come first in the ids the client has, then the plugin ones in
//! registration order. Unlike the entries a plugin puts in its declaration these are the host's
//! own, so a plugin that registers a sound does not have to repeat it for `NeoForge` clients.

use pumpkin_data::extra_registry::{
    plugin_datapack_registries, plugin_map_decorations, plugin_owned_registries, plugin_particles,
    plugin_sounds,
};
use pumpkin_data::map_decoration::MapDecorationType;
use pumpkin_data::mob_effect_registry::{plugin_mob_effects, vanilla_mob_effect_keys};
use pumpkin_data::particle::Particle;
use pumpkin_data::sound::Sound;

use super::{declaration::DatapackEntryDecl, registries::SyncedRegistry};

pub const SOUND_EVENT: &str = "minecraft:sound_event";
pub const PARTICLE_TYPE: &str = "minecraft:particle_type";
pub const MOB_EFFECT: &str = "minecraft:mob_effect";
pub const MAP_DECORATION_TYPE: &str = "minecraft:map_decoration_type";
pub const ENCHANTMENT_EFFECT_COMPONENT_TYPE: &str = "minecraft:enchantment_effect_component_type";

/// The number of plugin entries over all registries of this module, for the handshake cache.
#[must_use]
pub fn entry_count() -> usize {
    let datapack: usize = plugin_datapack_registries()
        .iter()
        .map(|registry| registry.entries.len())
        .sum();
    let owned: usize = plugin_owned_registries()
        .iter()
        .map(|registry| registry.entries.len())
        .sum();
    plugin_sounds().len()
        + plugin_particles().len()
        + plugin_mob_effects().len()
        + plugin_map_decorations().len()
        + pumpkin_data::enchantment_registry::plugin_enchantment_effect_types().len()
        + datapack
        + owned
}

fn push(
    out: &mut Vec<SyncedRegistry>,
    key: &str,
    vanilla: impl FnOnce() -> Vec<String>,
    plugin: Vec<String>,
) {
    if plugin.is_empty() {
        return;
    }
    let mut entries = vanilla();
    entries.extend(plugin);
    out.push(SyncedRegistry {
        key: key.to_string(),
        entries,
    });
}

/// The synced registries of this module that contain plugin entries.
#[must_use]
pub fn registries() -> Vec<SyncedRegistry> {
    let mut out = Vec::new();
    push(
        &mut out,
        SOUND_EVENT,
        || {
            Sound::NAMES
                .iter()
                .map(|name| format!("minecraft:{name}"))
                .collect()
        },
        plugin_sounds().iter().map(|s| s.key.to_string()).collect(),
    );
    push(
        &mut out,
        PARTICLE_TYPE,
        || {
            (0..=u16::MAX)
                .map_while(Particle::from_id)
                .map(|particle| format!("minecraft:{}", particle.to_name()))
                .collect()
        },
        plugin_particles()
            .iter()
            .map(|p| p.key.to_string())
            .collect(),
    );
    push(
        &mut out,
        MOB_EFFECT,
        vanilla_mob_effect_keys,
        plugin_mob_effects()
            .iter()
            .map(|e| e.key.to_string())
            .collect(),
    );
    push(
        &mut out,
        MAP_DECORATION_TYPE,
        || {
            MapDecorationType::ALL
                .iter()
                .map(|decoration| format!("minecraft:{}", decoration.name))
                .collect()
        },
        plugin_map_decorations()
            .iter()
            .map(|d| d.key.to_string())
            .collect(),
    );
    push(
        &mut out,
        ENCHANTMENT_EFFECT_COMPONENT_TYPE,
        || {
            pumpkin_data::enchantment_registry::VANILLA_ENCHANTMENT_EFFECT_TYPES
                .iter()
                .map(|key| (*key).to_string())
                .collect()
        },
        pumpkin_data::enchantment_registry::plugin_enchantment_effect_types()
            .iter()
            .map(|t| t.key.to_string())
            .collect(),
    );
    // Plugin owned registries are sent whole.
    for registry in plugin_owned_registries() {
        if !registry.entries.is_empty() {
            out.push(SyncedRegistry {
                key: registry.key,
                entries: registry.entries,
            });
        }
    }
    out
}

/// The datapack registry entries plugins added through the host.
#[must_use]
pub fn datapack_entries() -> Vec<DatapackEntryDecl> {
    plugin_datapack_registries()
        .into_iter()
        .flat_map(|registry| {
            let key = registry.key;
            registry
                .entries
                .into_iter()
                .map(move |(id, data)| DatapackEntryDecl {
                    registry: key.clone(),
                    id,
                    data,
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumpkin_data::extra_registry::{
        MapDecorationRegistration, add_datapack_entry, add_owned_registry_entry,
        register_datapack_registry, register_owned_registry, register_plugin_map_decoration,
        register_plugin_particle, register_plugin_sound,
    };

    #[test]
    fn plugin_entries_follow_the_vanilla_ones() {
        register_plugin_sound("core_extra_test:ding", Some(16.0)).unwrap();
        register_plugin_particle("core_extra_test:spark", false, "none").unwrap();
        register_plugin_map_decoration(
            "core_extra_test:pin",
            MapDecorationRegistration {
                asset_name: "pin".to_string(),
                show_on_item_frame: true,
                map_color: -1,
                exploration_map_element: false,
                track_count: false,
            },
        )
        .unwrap();
        register_owned_registry("core_extra_test:fan").unwrap();
        add_owned_registry_entry("core_extra_test:fan", "core_extra_test:smoke").unwrap();
        register_datapack_registry("core_extra_test:things", "nbt").unwrap();
        add_datapack_entry(
            "core_extra_test:things",
            "core_extra_test:a",
            Some(vec![10, 0]),
        )
        .unwrap();

        let all = registries();
        let find = |key: &str| all.iter().find(|r| r.key == key).expect(key);
        let sounds = find(SOUND_EVENT);
        assert_eq!(
            sounds.entries.len(),
            Sound::NAMES.len() + plugin_sounds().len()
        );
        assert_eq!(
            sounds.entries[0],
            "minecraft:entity.allay.ambient_with_item"
        );
        assert!(sounds.entries.iter().any(|e| e == "core_extra_test:ding"));
        let particles = find(PARTICLE_TYPE);
        assert_eq!(particles.entries[0], "minecraft:angry_villager");
        assert_eq!(
            find("core_extra_test:fan").entries,
            ["core_extra_test:smoke"]
        );
        assert!(
            find(MAP_DECORATION_TYPE)
                .entries
                .iter()
                .any(|e| e == "minecraft:player")
        );
        assert!(
            datapack_entries()
                .iter()
                .any(|e| e.id == "core_extra_test:a")
        );
        assert!(entry_count() >= 5);
    }
}
