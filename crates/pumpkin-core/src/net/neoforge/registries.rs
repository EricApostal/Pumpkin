//! The registries the handshake synchronises, built from the host's registry data.
//!
//! A real `NeoForge` server sends the registries that contain modded entries, with every entry of
//! the registry: the vanilla ones in the ids the host puts on the wire, then the ones plugins
//! registered, in registration order (the host numbers them `vanilla count + index`, which is
//! the order of these lists). A registry without plugin entries is not sent, the client keeps
//! the vanilla ids for it.
//!
//! A registry is only sent when the declarations [vouch](Vouched) for its plugin entries. The
//! client applies the snapshot by *name* and disconnects on a key it does not have ("The server
//! sent registries with unknown keys"), so a plugin whose port of a mod is partial — a name it
//! has not confirmed against the client, an entry the client does not have yet — must not be
//! advertised until its content matches. Nothing in the host knows what the client has; the
//! declaration is the only place that can say it, and it is opt-in *per registry*: a declaration
//! names the registry it vouches for, and a plugin that registers entries without naming it keeps
//! them server-side. Declaring a mod for its payload channels therefore cannot advertise content.
//!
//! The block order also numbers the block states of the chunk data on the client.

use std::collections::{BTreeMap, BTreeSet};

use pumpkin_data::{
    Block, BlockId,
    block_entity_registry::{plugin_block_entity_types, vanilla_block_entity_type_count},
    block_properties::BLOCK_ENTITY_TYPES,
    data_component::DataComponent,
    data_component_registry::plugin_component_types,
    entity::EntityType,
    entity_data_serializer_registry::plugin_entity_data_serializers,
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

/// The registries the declarations vouch for: for each registry key, the namespaces whose
/// registered entries in it are complete and match the client's.
///
/// **Declaring a mod does not vouch for its content.** A declaration vouches only for the
/// registries it *names* in its `synced-registry` list, and then for the mod's own namespace in
/// them. Channels and content are separate claims: a port that is still incomplete declares its
/// payload channels — so a client that has the mod can negotiate them — and names no registry, and
/// the content it registered stays server-side instead of disconnecting every client.
///
/// Entries whose namespace no declaration claims are never put in the snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Vouched {
    by_registry: BTreeMap<String, BTreeSet<String>>,
}

/// The registries whose ids the host numbers from its own dynamic registries (vanilla entries
/// first, then the plugin entries in registration order).
///
/// A declaration names one of these to vouch for the mod's entries in it; the entries themselves
/// come from the host (the plugin registered them through the registry interfaces), so a named
/// `synced-registry` for one of these carries no entries.
pub const HOST_NUMBERED: [&str; 15] = [
    BLOCK,
    ITEM,
    FLUID,
    ENTITY_TYPE,
    BLOCK_ENTITY_TYPE,
    MENU,
    DATA_COMPONENT_TYPE,
    ENTITY_DATA_SERIALIZERS,
    super::extra_registries::SOUND_EVENT,
    super::extra_registries::PARTICLE_TYPE,
    super::extra_registries::MOB_EFFECT,
    super::extra_registries::MAP_DECORATION_TYPE,
    super::extra_registries::ENCHANTMENT_EFFECT_COMPONENT_TYPE,
    super::recipe_sync::RECIPE_TYPE_REGISTRY,
    super::recipe_sync::RECIPE_SERIALIZER_REGISTRY,
];

impl Vouched {
    /// The vouchers of `declarations`, in declaration order.
    ///
    /// Only the registries a declaration names are vouched for; declaring the mod itself vouches
    /// for nothing (see [`Vouched`]).
    #[must_use]
    pub fn from_declarations(declarations: &[super::declaration::Declaration]) -> Self {
        let mut vouched = Self::default();
        for declaration in declarations {
            for registry in &declaration.registries {
                vouched.vouch(&registry.key, &declaration.mod_info.id);
            }
        }
        vouched
    }

    /// Records that `namespace` vouches for its entries in `registry`.
    pub fn vouch(&mut self, registry: &str, namespace: &str) {
        self.by_registry
            .entry(registry.to_string())
            .or_default()
            .insert(namespace.to_string());
    }

    /// Whether the host numbers `key` from its own dynamic registries (see [`HOST_NUMBERED`]).
    #[must_use]
    pub fn is_host_numbered(key: &str) -> bool {
        HOST_NUMBERED.contains(&key)
    }

    /// Whether nothing is vouched for, which is the case without a declaration.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_registry.is_empty()
    }

    /// Whether the entry `key` of `registry` may be sent to a client.
    ///
    /// For a registry the host numbers, the entry's namespace has to be vouched for in it: the
    /// declaration says "the blocks/items of this mod are the client's", not "every entry of this
    /// registry is". For a registry the host does not number (a mod's own, like
    /// `create:fan_processing_type`), naming the key at all is the claim, and the host's entries
    /// are the whole registry.
    #[must_use]
    fn covers(&self, registry: &str, key: &str) -> bool {
        let Some(namespaces) = self.by_registry.get(registry) else {
            return false;
        };
        if !Self::is_host_numbered(registry) {
            return true;
        }
        let namespace = key
            .split_once(':')
            .map_or("minecraft", |(namespace, _)| namespace);
        namespaces.contains(namespace)
    }
}

/// The plugin entries of `registry` that may go into the snapshot, `None` when the registry must
/// not be synchronised at all.
///
/// The ids of a snapshot are the positions of its entries, and the host numbers the plugin entries
/// of a registry in registration order, so the entries sent have to be a *prefix* of them: leaving
/// out an entry that comes before one that is sent would give every later entry a different id on
/// the client than the one the host puts in chunk data.
pub(super) fn syncable(
    registry: &str,
    mut plugin: Vec<String>,
    vouched: &Vouched,
) -> Option<Vec<String>> {
    if plugin.is_empty() {
        return None;
    }
    let sent = plugin
        .iter()
        .take_while(|entry| vouched.covers(registry, entry))
        .count();
    if sent == 0 {
        tracing::debug!(
            "not synchronising {registry}: no declaration vouches for its {} plugin entries \
             (the plugin has to name the registry in its declaration)",
            plugin.len()
        );
        return None;
    }
    if plugin[sent..]
        .iter()
        .any(|entry| vouched.covers(registry, entry))
    {
        tracing::warn!(
            "not synchronising {registry}: entries of a mod that did not vouch for it are \
             registered before entries that did, which would renumber the later ones on the client"
        );
        return None;
    }
    if sent < plugin.len() {
        tracing::debug!(
            "not synchronising {} of the {} plugin entries of {registry}: their mod did not \
             vouch for the registry",
            plugin.len() - sent,
            plugin.len()
        );
        plugin.truncate(sent);
    }
    Some(plugin)
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
    vouched: &Vouched,
    out: &mut Vec<SyncedRegistry>,
) {
    let Some(plugin) = syncable(key, plugin, vouched) else {
        return;
    };
    let mut entries = vanilla;
    entries.extend(plugin);
    out.push(SyncedRegistry {
        key: key.to_string(),
        entries,
    });
}

/// The registries that contain plugin registered entries, with all their entries. Needs the
/// dynamic registries to be frozen to be final.
///
/// Only the entries a declaration vouched for are included; a registry whose plugin entries are
/// not vouched for (or only partly, see [`syncable`]) is left out entirely, so the client keeps
/// its own registry for it.
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn host_registries(vouched: &Vouched) -> Vec<SyncedRegistry> {
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
            vouched,
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
            vouched,
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
            vouched,
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
            entity_types
                .iter()
                .map(|entity| entity.key.to_string())
                .collect(),
            vouched,
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
            vouched,
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
            vouched,
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
            vouched,
            &mut out,
        );
    }

    // `neoforge:entity_data_serializers` holds only the serializers mods register: the vanilla
    // ones live in the vanilla `EntityDataSerializers` map (ids 0..44) and are never in this
    // registry, and modded ids start at `CommonHooks.VANILLA_SERIALIZER_LIMIT` (256), see
    // `NeoForgeRegistries.ENTITY_DATA_SERIALIZERS` and `CommonHooks.getSerializer`. Sending the
    // vanilla names here makes the client refuse the sync with "unknown keys".
    let serializers = plugin_entity_data_serializers();
    if let Some(entries) = syncable(
        ENTITY_DATA_SERIALIZERS,
        serializers
            .iter()
            .map(|serializer| serializer.key.to_string())
            .collect(),
        vouched,
    ) {
        out.push(SyncedRegistry {
            key: ENTITY_DATA_SERIALIZERS.to_string(),
            entries,
        });
    }

    // Sounds, particles, mob effects, map decorations and plugin owned registries.
    out.extend(super::extra_registries::registries(vouched));

    // The recipe types and serializers the plugins sync recipes for.
    for (key, vanilla, plugin) in super::recipe_sync::registry_extras() {
        with_plugin_entries(key, vanilla, plugin, vouched, &mut out);
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

    /// A voucher for every namespace in `entries`, so a test does not depend on which entries
    /// other tests of the crate registered before it.
    fn vouch_all(registry: &str, entries: &[String]) -> Vouched {
        let mut vouched = Vouched::default();
        for entry in entries {
            let namespace = entry
                .split_once(':')
                .map_or("minecraft", |(namespace, _)| namespace);
            vouched.vouch(registry, namespace);
        }
        vouched
    }

    fn serializer_keys() -> Vec<String> {
        plugin_entity_data_serializers()
            .iter()
            .map(|serializer| serializer.key.to_string())
            .collect()
    }

    fn block_keys() -> Vec<String> {
        Block::dynamic_blocks()
            .iter()
            .map(|block| block.resource_location().into_owned())
            .collect()
    }

    fn plain_block(key: &str) -> pumpkin_data::block_registry::BlockRegistration {
        pumpkin_data::block_registry::BlockRegistration {
            key: key.to_string(),
            properties: Vec::new(),
            default_state: Vec::new(),
            hardness: 1.5,
            blast_resistance: 6.0,
            requires_correct_tool: false,
            sound_type: "stone".to_string(),
            luminance: 0,
            can_occlude: true,
            replaceable: false,
            map_color: 0,
            collision_shape: pumpkin_data::block_registry::ShapeDefinition::FullCube,
            selection_shape: pumpkin_data::block_registry::ShapeDefinition::FullCube,
            shape_rules: Vec::new(),
            piston_behavior: pumpkin_data::block_state::PistonBehavior::Normal,
            connect_rules: Vec::new(),
            drops: pumpkin_data::block_registry::BlockDrops::SelfItem,
            tags: Vec::new(),
        }
    }

    #[test]
    fn entity_data_serializers_sync_only_modded_entries() {
        // `neoforge:entity_data_serializers` is a modded registry: the vanilla serializers are
        // not in it, and a client refuses the sync when it is sent keys it does not have.
        let key = "test_sync_serializers:thing";
        let serializer =
            pumpkin_data::entity_data_serializer_registry::register_plugin_entity_data_serializer(
                key,
            )
            .unwrap();
        assert!(
            serializer.id
                >= pumpkin_data::entity_data_serializer_registry::VANILLA_SERIALIZER_LIMIT
        );
        let vouched = vouch_all(ENTITY_DATA_SERIALIZERS, &serializer_keys());
        let registry = host_registries(&vouched)
            .into_iter()
            .find(|registry| registry.key == ENTITY_DATA_SERIALIZERS)
            .expect("the registry is synced once a serializer is registered");
        assert!(registry.entries.iter().any(|entry| entry == key));
        assert!(
            !registry
                .entries
                .iter()
                .any(|entry| entry.starts_with("minecraft:")),
            "vanilla serializers must not be sent: {:?}",
            registry.entries
        );
    }

    #[test]
    fn nothing_is_synced_without_a_voucher() {
        // The failure this guards against: a plugin registers content whose names the client does
        // not have (`aeronautics:hot_air_burner`, which Java registers as
        // `aeronautics:adjustable_burner`), the snapshot carries it, and the client disconnects
        // with "The server sent registries with unknown keys". Without a declaration that names a
        // registry, nothing a plugin registered reaches a client — not even a vanilla list.
        Block::register_dynamic(plain_block("test_unvouched:block")).unwrap();
        let unvouched = host_registries(&Vouched::default());
        assert!(
            unvouched.is_empty(),
            "nothing may be synchronised without a declaration that names a registry: {unvouched:?}"
        );
    }

    /// A declaration of [mod_id] with a payload channel and no registry: a port in progress.
    fn partial_port(mod_id: &str) -> crate::net::neoforge::declaration::Declaration {
        use crate::net::neoforge::declaration::{ChannelDecl, Declaration, ModInfo};
        use crate::net::neoforge::payloads::Phase;
        Declaration {
            mod_info: ModInfo {
                id: mod_id.into(),
                version: "0.1".into(),
                display_name: "Partial".into(),
            },
            channels: vec![ChannelDecl {
                id: format!("{mod_id}:sync"),
                version: "1".into(),
                phases: vec![Phase::Play],
                flow: None,
                optional: true,
            }],
            registries: Vec::new(),
            config_files: Vec::new(),
            datapack_entries: Vec::new(),
            data_maps: Vec::new(),
            feature_flags: Vec::new(),
            allow_non_neoforge: true,
            reject_message: None,
        }
    }

    #[test]
    fn declaring_a_mod_does_not_sync_the_content_it_registers() {
        // Declaring a mod is how a port announces its payload channels, so a client that has the
        // mod can negotiate them. It must not advertise the content the plugin registered: a port
        // that is not finished has names the client's release does not have yet.
        Block::register_dynamic(plain_block("test_partial:hot_air_burner")).unwrap();
        let vouched = Vouched::from_declarations(&[partial_port("test_partial")]);
        assert!(
            vouched.is_empty(),
            "a declaration that names no registry vouches for nothing"
        );
        for registry in host_registries(&vouched) {
            assert!(
                !registry
                    .entries
                    .iter()
                    .any(|entry| entry.starts_with("test_partial:")),
                "{} carries an entry of a port that did not vouch for it: {:?}",
                registry.key,
                registry.entries
            );
        }
    }

    #[test]
    fn naming_a_registry_is_what_syncs_it() {
        // The other half: naming `minecraft:block` in the declaration is the claim that the mod's
        // registered blocks are the client's — and only that registry, not every one the host
        // numbers.
        let mut declaration = partial_port("test_named");
        declaration
            .registries
            .push(crate::net::neoforge::declaration::RegistryDecl {
                key: BLOCK.into(),
                entries: Vec::new(),
            });
        let vouched = Vouched::from_declarations(&[declaration]);
        assert!(vouched.covers(BLOCK, "test_named:hot_air_burner"));
        assert!(!vouched.covers(ITEM, "test_named:hot_air_burner"));
        assert!(!vouched.covers(BLOCK, "other_mod:thing"));
        // A declared list for a registry the host numbers is not what is sent: the host's dynamic
        // registry is (`Aggregate::build` / `host_registries`).
        assert_eq!(vouched.by_registry[BLOCK], BTreeSet::from(["test_named".to_string()]));
    }

    #[test]
    fn a_voucher_syncs_the_entries_of_its_namespace() {
        Block::register_dynamic(plain_block("test_vouched:block")).unwrap();
        let vouched = vouch_all(BLOCK, &block_keys());
        let registry = host_registries(&vouched)
            .into_iter()
            .find(|registry| registry.key == BLOCK)
            .expect("the block registry is synced once a mod vouches for it");
        assert!(
            registry
                .entries
                .iter()
                .any(|entry| entry == "test_vouched:block")
        );
        assert!(registry.entries[0].starts_with("minecraft:"));
    }

    #[test]
    fn entries_after_an_unvouched_one_are_not_sent() {
        // Ids are positional, so a registry whose vouched entries come after unvouched ones
        // cannot be sent: the client would number the vouched entries differently.
        Block::register_dynamic(plain_block("test_prefix_first:block")).unwrap();
        Block::register_dynamic(plain_block("test_prefix_second:block")).unwrap();
        let mut vouched = Vouched::default();
        vouched.vouch(BLOCK, "test_prefix_second");
        assert!(
            !host_registries(&vouched)
                .iter()
                .any(|registry| registry.key == BLOCK),
            "a registry whose sent entries are a prefix must be left out"
        );
    }

    /// The id of an entry in a snapshot is its index, and the plugin entries keep the order the
    /// host registered them in — across plugins. Two plugins that each register blocks get
    /// contiguous ids in that order, which is what the client adopts
    /// (`RegistryManager.applySnapshot` -> `registerIdMapping`), so no plugin is "wrong" for
    /// loading second; the ids only have to be the same on every start.
    #[test]
    fn plugin_entries_keep_their_registration_order_and_index_as_id() {
        let first = "test_order_a:block";
        let second = "test_order_b:block";
        Block::register_dynamic(plain_block(first)).unwrap();
        Block::register_dynamic(plain_block(second)).unwrap();

        let vouched = vouch_all(BLOCK, &block_keys());
        let registry = host_registries(&vouched)
            .into_iter()
            .find(|registry| registry.key == BLOCK)
            .expect("the block registry is synced once a mod vouches for it");

        let vanilla = usize::from(BlockId::COUNT);
        assert_eq!(registry.entries.len(), vanilla + block_keys().len());
        let position = |key: &str| {
            registry
                .entries
                .iter()
                .position(|entry| entry == key)
                .unwrap_or_else(|| panic!("{key} is not in the snapshot"))
        };
        let (a, b) = (position(first), position(second));
        assert_eq!(a, vanilla + block_keys().iter().position(|k| k == first).unwrap());
        assert_eq!(b, vanilla + block_keys().iter().position(|k| k == second).unwrap());
        assert_eq!(b, a + 1, "the second plugin's block follows the first's");
        // The id the payload carries is the index (`encode_frozen_registry`).
        assert!(a >= vanilla && b >= vanilla);
    }
}
