//! The registries the handshake synchronises, built from the host's registry data.
//!
//! A real NeoForge server sends the registries that contain modded entries, with every entry of
//! the registry: the vanilla ones in the ids the host puts on the wire, then the ones plugins
//! registered, in registration order (the host numbers them `vanilla count + index`, which is
//! the order of these lists). A registry without plugin entries is not sent, the client keeps
//! the vanilla ids for it.
//!
//! The block order also numbers the block states of the chunk data on the client.

use pumpkin_data::{
    Block, BlockId,
    block_entity_registry::{plugin_block_entity_types, vanilla_block_entity_type_count},
    block_properties::BLOCK_ENTITY_TYPES,
    data_component::DataComponent,
    data_component_registry::plugin_component_types,
    entity::EntityType,
    entity_data_serializer_registry::{
        VANILLA_ENTITY_DATA_SERIALIZERS, plugin_entity_data_serializers,
    },
    entity_registry::{plugin_entity_types, vanilla_entity_type_count},
    fluid::Fluid,
    item::Item,
    menu_registry::{plugin_menu_types, vanilla_menu_type_count},
};

pub const BLOCK: &str = "minecraft:block";
pub const ITEM: &str = "minecraft:item";
pub const FLUID: &str = "minecraft:fluid";
pub const ENTITY_TYPE: &str = "minecraft:entity_type";
pub const BLOCK_ENTITY_TYPE: &str = "minecraft:block_entity_type";
pub const MENU: &str = "minecraft:menu";
pub const DATA_COMPONENT_TYPE: &str = "minecraft:data_component_type";
pub const ENTITY_DATA_SERIALIZERS: &str = "neoforge:entity_data_serializers";

/// One registry to synchronise: the key and every entry in id order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncedRegistry {
    pub key: String,
    pub entries: Vec<String>,
}

/// The vanilla menu types in registry order (`WindowType`).
const VANILLA_MENUS: [&str; 25] = [
    "generic_9x1",
    "generic_9x2",
    "generic_9x3",
    "generic_9x4",
    "generic_9x5",
    "generic_9x6",
    "generic_3x3",
    "crafter_3x3",
    "anvil",
    "beacon",
    "blast_furnace",
    "brewing_stand",
    "crafting",
    "enchantment",
    "furnace",
    "grindstone",
    "hopper",
    "lectern",
    "loom",
    "merchant",
    "shulker_box",
    "smithing",
    "smoker",
    "cartography_table",
    "stonecutter",
];

/// How many entries plugins registered per synced registry, in the order of [`host_registries`].
/// A change means the cached handshake data is stale.
#[must_use]
pub fn plugin_entry_counts() -> [usize; 8] {
    [
        Block::dynamic_blocks().len(),
        Item::dynamic_items().len(),
        Fluid::dynamic_fluids().len(),
        plugin_entity_types().len(),
        plugin_block_entity_types().len(),
        plugin_menu_types().len(),
        plugin_component_types().len(),
        plugin_entity_data_serializers().len() + super::extra_registries::entry_count(),
    ]
}

fn prefixed(names: impl Iterator<Item = String>) -> Vec<String> {
    names.collect()
}

fn with_plugin_entries(
    key: &str,
    vanilla: Vec<String>,
    plugin: Vec<String>,
    out: &mut Vec<SyncedRegistry>,
) {
    if plugin.is_empty() {
        return;
    }
    let mut entries = vanilla;
    entries.extend(plugin);
    out.push(SyncedRegistry {
        key: key.to_string(),
        entries,
    });
}

/// The registries that contain plugin registered entries, with all their entries. Needs the
/// dynamic registries to be frozen to be final.
#[must_use]
pub fn host_registries() -> Vec<SyncedRegistry> {
    let mut out = Vec::new();

    let blocks = Block::dynamic_blocks();
    if !blocks.is_empty() {
        let vanilla = (0..BlockId::COUNT)
            .filter_map(BlockId::new)
            .map(|id| Block::from_id(id).resource_location().into_owned());
        with_plugin_entries(
            BLOCK,
            prefixed(vanilla),
            blocks
                .iter()
                .map(|block| block.resource_location().into_owned())
                .collect(),
            &mut out,
        );
    }

    let items = Item::dynamic_items();
    if !items.is_empty() {
        let vanilla = (0..Item::vanilla_count())
            .filter_map(Item::from_vanilla_id)
            .map(|item| format!("minecraft:{}", item.registry_key));
        with_plugin_entries(
            ITEM,
            prefixed(vanilla),
            items
                .iter()
                .map(|item| item.resource_location().into_owned())
                .collect(),
            &mut out,
        );
    }

    let fluids = Fluid::dynamic_fluids();
    if !fluids.is_empty() {
        let vanilla = (0..Fluid::VANILLA_COUNT)
            .filter_map(Fluid::from_id)
            .map(|fluid| format!("minecraft:{}", fluid.name));
        with_plugin_entries(
            FLUID,
            prefixed(vanilla),
            fluids
                .iter()
                .map(|fluid| fluid.resource_location().into_owned())
                .collect(),
            &mut out,
        );
    }

    let entity_types = plugin_entity_types();
    if !entity_types.is_empty() {
        let vanilla = (0..vanilla_entity_type_count())
            .filter_map(|id| u16::try_from(id).ok().and_then(EntityType::from_raw))
            .map(|entity| format!("minecraft:{}", entity.resource_name));
        with_plugin_entries(
            ENTITY_TYPE,
            prefixed(vanilla),
            entity_types.iter().map(|entity| entity.key.to_string()).collect(),
            &mut out,
        );
    }

    let block_entities = plugin_block_entity_types();
    if !block_entities.is_empty() {
        debug_assert_eq!(
            BLOCK_ENTITY_TYPES.len(),
            vanilla_block_entity_type_count() as usize
        );
        let vanilla = BLOCK_ENTITY_TYPES
            .iter()
            .map(|name| format!("minecraft:{name}"));
        with_plugin_entries(
            BLOCK_ENTITY_TYPE,
            prefixed(vanilla),
            block_entities
                .iter()
                .map(|entity| entity.key.to_string())
                .collect(),
            &mut out,
        );
    }

    let menus = plugin_menu_types();
    if !menus.is_empty() {
        debug_assert_eq!(VANILLA_MENUS.len(), vanilla_menu_type_count() as usize);
        let vanilla = VANILLA_MENUS.iter().map(|name| format!("minecraft:{name}"));
        with_plugin_entries(
            MENU,
            prefixed(vanilla),
            menus.iter().map(|menu| menu.key.to_string()).collect(),
            &mut out,
        );
    }

    let components = plugin_component_types();
    if !components.is_empty() {
        let vanilla = (0..DataComponent::VANILLA_COUNT)
            .filter_map(DataComponent::try_from_id)
            .map(|component| component.to_name().to_string());
        with_plugin_entries(
            DATA_COMPONENT_TYPE,
            prefixed(vanilla),
            components
                .iter()
                .map(|component| component.key.to_string())
                .collect(),
            &mut out,
        );
    }

    let serializers = plugin_entity_data_serializers();
    if !serializers.is_empty() {
        let vanilla = VANILLA_ENTITY_DATA_SERIALIZERS
            .iter()
            .map(|name| format!("minecraft:{name}"));
        with_plugin_entries(
            ENTITY_DATA_SERIALIZERS,
            prefixed(vanilla),
            serializers
                .iter()
                .map(|serializer| serializer.key.to_string())
                .collect(),
            &mut out,
        );
    }

    // Sounds, particles, mob effects, map decorations and plugin owned registries.
    out.extend(super::extra_registries::registries());

    // The recipe types and serializers the plugins sync recipes for.
    for (key, entries) in super::recipe_sync::registry_extras() {
        out.push(SyncedRegistry { key, entries });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_lists_have_the_vanilla_counts() {
        assert_eq!(VANILLA_MENUS.len(), vanilla_menu_type_count() as usize);
        assert_eq!(
            BLOCK_ENTITY_TYPES.len(),
            vanilla_block_entity_type_count() as usize
        );
        let items: Vec<_> = (0..Item::vanilla_count())
            .filter_map(Item::from_vanilla_id)
            .collect();
        assert_eq!(items.len(), usize::from(Item::vanilla_count()));
        assert_eq!(items[0].registry_key, "air");
        let blocks = (0..BlockId::COUNT).filter_map(BlockId::new).count();
        assert_eq!(blocks, usize::from(BlockId::COUNT));
        let entities = (0..vanilla_entity_type_count())
            .filter_map(|id| u16::try_from(id).ok().and_then(EntityType::from_raw))
            .count();
        assert_eq!(entities, vanilla_entity_type_count() as usize);
        let components = (0..DataComponent::VANILLA_COUNT)
            .filter_map(DataComponent::try_from_id)
            .count();
        assert_eq!(components, usize::from(DataComponent::VANILLA_COUNT));
    }

    #[test]
    fn without_plugin_entries_no_registry_is_synced() {
        // Other tests of this crate may register entries into the global registries, so only
        // check that every listed registry is complete.
        for registry in host_registries() {
            assert!(registry.entries.len() > 1, "{}", registry.key);
            assert!(registry.entries[0].starts_with("minecraft:"));
        }
    }
}
