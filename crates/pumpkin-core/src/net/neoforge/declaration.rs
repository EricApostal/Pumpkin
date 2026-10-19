//! What plugins declare for the `NeoForge` handshake, and the one handshake all declarations are
//! merged into.

use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        Arc, LazyLock, Mutex, PoisonError, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use tracing::warn;

use super::{
    channels::{self, NEOFORGE_SERVER_CONFIG_FILE},
    ident::{is_valid_namespace, normalize},
    payloads::{
        Flow, Phase, QueryComponent, encode_feature_flags, encode_frozen_registry,
        encode_known_data_maps, encode_sync_start,
    },
    registries::{self, SyncedRegistry},
};

/// The most entries one registry of a declaration may add.
const MAX_REGISTRY_ENTRIES: usize = 1 << 20;
/// The most channels, files and the like one declaration may have.
const MAX_ITEMS: usize = 4096;
/// A config file is a custom payload, and those are limited to 1 MiB.
const MAX_CONFIG_FILE_BYTES: usize = 900 * 1024;

pub const DEFAULT_REJECT_MESSAGE: &str = "This server requires NeoForge with the mod(s): {mods}. \
Install NeoForge and the mod(s) to join.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModInfo {
    pub id: String,
    pub version: String,
    pub display_name: String,
}

/// A custom payload channel of a mod.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelDecl {
    pub id: String,
    pub version: String,
    pub phases: Vec<Phase>,
    /// None for both directions.
    pub flow: Option<Flow>,
    pub optional: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryDecl {
    pub key: String,
    pub entries: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigFileDecl {
    pub name: String,
    pub content: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatapackEntryDecl {
    pub registry: String,
    pub id: String,
    pub data: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataMapDecl {
    pub registry: String,
    pub id: String,
    pub mandatory: bool,
}

/// The declaration of one plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub mod_info: ModInfo,
    pub channels: Vec<ChannelDecl>,
    pub registries: Vec<RegistryDecl>,
    pub config_files: Vec<ConfigFileDecl>,
    pub datapack_entries: Vec<DatapackEntryDecl>,
    pub data_maps: Vec<DataMapDecl>,
    pub feature_flags: Vec<String>,
    /// Whether clients without `NeoForge` may join.
    pub allow_non_neoforge: bool,
    pub reject_message: Option<String>,
}

fn id(text: &str, what: &str) -> Result<String, String> {
    normalize(text).map_err(|error| format!("{what}: {error}"))
}

impl Declaration {
    /// Checks the declaration and brings every identifier into the `namespace:path` form.
    pub fn validated(mut self) -> Result<Self, String> {
        if !is_valid_namespace(&self.mod_info.id) {
            return Err(format!("'{}' is not a valid mod id", self.mod_info.id));
        }
        if self.mod_info.display_name.is_empty() {
            self.mod_info.display_name.clone_from(&self.mod_info.id);
        }
        if [
            self.channels.len(),
            self.registries.len(),
            self.config_files.len(),
            self.datapack_entries.len(),
            self.data_maps.len(),
            self.feature_flags.len(),
        ]
        .iter()
        .any(|&len| len > MAX_ITEMS)
        {
            return Err(format!(
                "a declaration has at most {MAX_ITEMS} of each kind"
            ));
        }

        let mut seen = HashSet::new();
        for channel in &mut self.channels {
            channel.id = id(&channel.id, "channel")?;
            if channel.phases.is_empty() {
                return Err(format!("channel {} needs at least one phase", channel.id));
            }
            channel
                .phases
                .retain(|phase| matches!(phase, Phase::Configuration | Phase::Play));
            channel.phases.sort();
            channel.phases.dedup();
            if channel.phases.is_empty() {
                return Err(format!(
                    "channel {} exists in the configuration and play phases only",
                    channel.id
                ));
            }
            if channel.id.starts_with("minecraft:")
                || channel.id.starts_with("neoforge:")
                || channel.id.starts_with("c:")
            {
                return Err(format!("channel {} uses a reserved namespace", channel.id));
            }
            if !seen.insert(channel.id.clone()) {
                return Err(format!("channel {} is declared twice", channel.id));
            }
        }

        let mut seen = HashSet::new();
        for registry in &mut self.registries {
            registry.key = id(&registry.key, "registry")?;
            if !seen.insert(registry.key.clone()) {
                return Err(format!("registry {} is declared twice", registry.key));
            }
            if registry.entries.len() > MAX_REGISTRY_ENTRIES {
                return Err(format!("registry {} has too many entries", registry.key));
            }
            let mut entries = HashSet::new();
            for entry in &mut registry.entries {
                *entry = id(entry, "registry entry")?;
                if !entries.insert(entry.clone()) {
                    return Err(format!("{entry} is listed twice in {}", registry.key));
                }
            }
        }

        let mut seen = HashSet::new();
        for file in &self.config_files {
            if file.name.is_empty() || file.name.contains(['/', '\\']) || !seen.insert(&file.name) {
                return Err(format!("'{}' is not a unique config file name", file.name));
            }
            if file.content.len() > MAX_CONFIG_FILE_BYTES {
                return Err(format!("config file {} is too large", file.name));
            }
        }

        for entry in &mut self.datapack_entries {
            entry.registry = id(&entry.registry, "datapack registry")?;
            entry.id = id(&entry.id, "datapack entry")?;
        }
        for map in &mut self.data_maps {
            map.registry = id(&map.registry, "data map registry")?;
            map.id = id(&map.id, "data map")?;
        }
        for flag in &mut self.feature_flags {
            *flag = id(flag, "feature flag")?;
        }
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

struct Store {
    /// Declarations in the order the plugins made them, by plugin name.
    declarations: Vec<(String, Declaration)>,
    generation: u64,
}

static STORE: LazyLock<RwLock<Store>> = LazyLock::new(|| {
    RwLock::new(Store {
        declarations: Vec::new(),
        generation: 0,
    })
});

static CACHE: LazyLock<Mutex<Option<CachedAggregate>>> = LazyLock::new(|| Mutex::new(None));

/// Counts how often the cache was rebuilt, for tests.
static REBUILDS: AtomicU64 = AtomicU64::new(0);

struct CachedAggregate {
    generation: u64,
    plugin_counts: [usize; 8],
    aggregate: Arc<Aggregate>,
}

fn same_channel_elsewhere(owner: &str, new: &ChannelDecl, store: &Store) -> Option<String> {
    store
        .declarations
        .iter()
        .filter(|(other, _)| other != owner)
        .flat_map(|(other, declaration)| declaration.channels.iter().map(move |c| (other, c)))
        .find(|(_, channel)| channel.id == new.id)
        .map(|(other, _)| other.clone())
}

/// Sets the declaration of `owner` (a plugin name), replacing its previous one.
pub fn declare(owner: &str, declaration: Declaration) -> Result<(), String> {
    let declaration = declaration.validated()?;
    let mut store = STORE.write().unwrap_or_else(PoisonError::into_inner);
    let unchanged = store
        .declarations
        .iter()
        .any(|(other, previous)| other == owner && *previous == declaration);
    if unchanged {
        return Ok(());
    }
    if pumpkin_data::item::Item::is_dynamic_registry_frozen() {
        return Err("the NeoForge handshake can only be declared before players connect".into());
    }
    for channel in &declaration.channels {
        if let Some(other) = same_channel_elsewhere(owner, channel, &store) {
            return Err(format!(
                "channel {} is already declared by plugin {other}",
                channel.id
            ));
        }
    }
    if store
        .declarations
        .iter()
        .filter(|(other, _)| other != owner)
        .any(|(_, other)| other.mod_info.id == declaration.mod_info.id)
    {
        return Err(format!(
            "mod id {} is already declared by another plugin",
            declaration.mod_info.id
        ));
    }
    match store
        .declarations
        .iter_mut()
        .find(|(other, _)| other == owner)
    {
        Some((_, previous)) => *previous = declaration,
        None => store.declarations.push((owner.to_string(), declaration)),
    }
    store.generation += 1;
    Ok(())
}

/// Removes the declaration of `owner`.
pub fn withdraw(owner: &str) {
    let mut store = STORE.write().unwrap_or_else(PoisonError::into_inner);
    let before = store.declarations.len();
    store.declarations.retain(|(other, _)| other != owner);
    if store.declarations.len() != before {
        store.generation += 1;
    }
}

/// Replaces (or adds) a config file of the mod `owner` declared, also after players connected.
///
/// Clients that join later get the new content with the handshake. Fails for a plugin without
/// declaration, another mod id, a bad file name or a file that is too large.
pub fn update_config_file(
    owner: &str,
    mod_id: &str,
    name: &str,
    content: Vec<u8>,
) -> Result<(), String> {
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err(format!("'{name}' is not a valid config file name"));
    }
    if content.len() > MAX_CONFIG_FILE_BYTES {
        return Err(format!("config file {name} is too large"));
    }
    let mut store = STORE.write().unwrap_or_else(PoisonError::into_inner);
    let Some((_, declaration)) = store
        .declarations
        .iter_mut()
        .find(|(other, _)| other == owner)
    else {
        return Err("the plugin has no NeoForge declaration".to_string());
    };
    if declaration.mod_info.id != mod_id {
        return Err(format!(
            "'{mod_id}' is not the mod id of the plugin's declaration"
        ));
    }
    if let Some(file) = declaration
        .config_files
        .iter_mut()
        .find(|file| file.name == name)
    {
        file.content = content;
    } else {
        if declaration.config_files.len() >= MAX_ITEMS {
            return Err(format!(
                "a declaration has at most {MAX_ITEMS} config files"
            ));
        }
        declaration.config_files.push(ConfigFileDecl {
            name: name.to_string(),
            content,
        });
    }
    store.generation += 1;
    Ok(())
}

/// The handshake all declarations make up, none when no plugin declared anything. Cheap, built
/// once and rebuilt only when a declaration or the plugin registered entries change.
#[must_use]
pub fn current() -> Option<Arc<Aggregate>> {
    let (generation, declarations) = {
        let store = STORE.read().unwrap_or_else(PoisonError::into_inner);
        if store.declarations.is_empty() {
            return None;
        }
        (
            store.generation,
            store
                .declarations
                .iter()
                .map(|(_, declaration)| declaration.clone())
                .collect::<Vec<_>>(),
        )
    };
    let plugin_counts = registries::plugin_entry_counts();
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(cached) = cache.as_ref()
        && cached.generation == generation
        && cached.plugin_counts == plugin_counts
    {
        return Some(Arc::clone(&cached.aggregate));
    }
    let mut aggregate = Aggregate::build(&declarations, registries::host_registries());
    // The datapack entries plugins registered through the host need no declaration.
    aggregate
        .datapack_entries
        .extend(super::extra_registries::datapack_entries());
    let aggregate = Arc::new(aggregate);
    REBUILDS.fetch_add(1, Ordering::Relaxed);
    *cache = Some(CachedAggregate {
        generation,
        plugin_counts,
        aggregate: Arc::clone(&aggregate),
    });
    Some(aggregate)
}

// ---------------------------------------------------------------------------
// The aggregate
// ---------------------------------------------------------------------------

/// The merged declarations, with the payloads that are the same for every client encoded once.
#[derive(Debug)]
pub struct Aggregate {
    pub mods: Vec<ModInfo>,
    /// The payload registrations of the server per protocol: `NeoForge`'s own and the plugins'.
    pub channels: BTreeMap<Phase, Vec<QueryComponent>>,
    /// The configuration channels the server can receive, announced with `minecraft:register`.
    pub configuration_channels_to_server: Vec<String>,
    /// The registries to synchronise, in the order they are sent.
    pub registries: Vec<SyncedRegistry>,
    pub sync_start_payload: Vec<u8>,
    /// One `neoforge:frozen_registry` payload per registry.
    pub registry_payloads: Vec<Vec<u8>>,
    pub config_files: Vec<(String, Vec<u8>)>,
    pub datapack_entries: Vec<DatapackEntryDecl>,
    pub known_data_maps_payload: Option<Vec<u8>>,
    pub feature_flags_payload: Option<Vec<u8>>,
    pub allow_non_neoforge: bool,
    pub reject_message: String,
}

impl Aggregate {
    /// Merges `declarations` (in plugin order) with the registries of the host. A declared
    /// registry that the host also synchronises adds its entries after the host's.
    #[must_use]
    pub fn build(declarations: &[Declaration], host_registries: Vec<SyncedRegistry>) -> Self {
        let mut mods = Vec::new();
        let mut channels: BTreeMap<Phase, Vec<QueryComponent>> = BTreeMap::new();
        for phase in [Phase::Configuration, Phase::Play] {
            channels.insert(phase, channels::own_registrations(phase));
        }
        let mut to_server = Vec::new();
        let mut registries = host_registries;
        let mut config_files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut datapack_entries = Vec::new();
        let mut data_maps: BTreeMap<String, Vec<(String, bool)>> = BTreeMap::new();
        let mut feature_flags: Vec<String> = Vec::new();
        let mut allow = true;
        let mut reject_message = None;

        for declaration in declarations {
            mods.push(declaration.mod_info.clone());
            for channel in &declaration.channels {
                for phase in &channel.phases {
                    channels.entry(*phase).or_default().push(QueryComponent {
                        id: channel.id.clone(),
                        version: channel.version.clone(),
                        flow: channel.flow,
                        optional: channel.optional,
                    });
                    if *phase == Phase::Configuration
                        && channel.flow != Some(Flow::Clientbound)
                        && !to_server.contains(&channel.id)
                    {
                        to_server.push(channel.id.clone());
                    }
                }
            }
            for extra in &declaration.registries {
                if let Some(existing) = registries.iter_mut().find(|r| r.key == extra.key) {
                    for entry in &extra.entries {
                        if !existing.entries.contains(entry) {
                            existing.entries.push(entry.clone());
                        }
                    }
                } else {
                    // The client rebuilds a registry from the snapshot, so the entries sent must
                    // be all of its entries, in the client's registration order. That only holds
                    // for a registry the host numbers; for one it does not (a mod's own, like
                    // `computercraft:recipe_function`) the declaration has to list every entry
                    // the client's mod registers, or the client loses the ones it left out.
                    warn!(
                        "Registry {} of the NeoForge declaration of {} is not one the host \
                         numbers: the client replaces its own entries with the declared ones",
                        extra.key, declaration.mod_info.id
                    );
                    registries.push(SyncedRegistry {
                        key: extra.key.clone(),
                        entries: extra.entries.clone(),
                    });
                }
            }
            for file in &declaration.config_files {
                match config_files.iter_mut().find(|(name, _)| *name == file.name) {
                    Some((_, content)) => content.clone_from(&file.content),
                    None => config_files.push((file.name.clone(), file.content.clone())),
                }
            }
            datapack_entries.extend(declaration.datapack_entries.iter().cloned());
            for map in &declaration.data_maps {
                let list = data_maps.entry(map.registry.clone()).or_default();
                if !list.iter().any(|(id, _)| *id == map.id) {
                    list.push((map.id.clone(), map.mandatory));
                }
            }
            for flag in &declaration.feature_flags {
                if !feature_flags.contains(flag) {
                    feature_flags.push(flag.clone());
                }
            }
            if !declaration.allow_non_neoforge {
                allow = false;
                if reject_message.is_none() {
                    reject_message.clone_from(&declaration.reject_message);
                }
            }
        }

        registries.retain(|registry| !registry.entries.is_empty());
        let keys: Vec<&str> = registries.iter().map(|r| r.key.as_str()).collect();
        let sync_start_payload = encode_sync_start(&keys);
        let registry_payloads = registries
            .iter()
            .map(|r| encode_frozen_registry(&r.key, &r.entries))
            .collect();

        Self {
            mods,
            channels,
            configuration_channels_to_server: to_server,
            registries,
            sync_start_payload,
            registry_payloads,
            config_files,
            datapack_entries,
            known_data_maps_payload: (!data_maps.is_empty())
                .then(|| encode_known_data_maps(&data_maps)),
            feature_flags_payload: (!feature_flags.is_empty())
                .then(|| encode_feature_flags(&feature_flags)),
            allow_non_neoforge: allow,
            reject_message: reject_message.unwrap_or_else(|| DEFAULT_REJECT_MESSAGE.to_string()),
        }
    }

    /// The mod list for messages: `Name version, Other 1.0`.
    #[must_use]
    pub fn mod_list(&self) -> String {
        if self.mods.is_empty() {
            return "none".to_string();
        }
        self.mods
            .iter()
            .map(|m| format!("{} {}", m.display_name, m.version))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `template` with `{mods}` and `{detail}` replaced.
    #[must_use]
    pub fn text(&self, template: &str, detail: &str) -> String {
        template
            .replace("{mods}", &self.mod_list())
            .replace("{detail}", detail)
    }

    /// The display name of the mod with this id, which is the namespace of its channels.
    #[must_use]
    pub fn mod_name(&self, namespace: &str) -> Option<String> {
        self.mods
            .iter()
            .find(|m| m.id == namespace)
            .map(|m| m.display_name.clone())
    }

    /// The config files to send to a `NeoForge` client: `NeoForge`'s own server config, empty, unless
    /// a plugin declared a file of that name.
    #[must_use]
    pub fn config_files_to_send(&self) -> Vec<(String, Vec<u8>)> {
        let mut files = Vec::with_capacity(self.config_files.len() + 1);
        if !self
            .config_files
            .iter()
            .any(|(name, _)| name == NEOFORGE_SERVER_CONFIG_FILE)
        {
            files.push((NEOFORGE_SERVER_CONFIG_FILE.to_string(), Vec::new()));
        }
        files.extend(self.config_files.iter().cloned());
        files
    }

    /// The declared datapack entries of `registry` (compared without the `minecraft:` prefix).
    pub fn datapack_entries_of<'a>(
        &'a self,
        registry: &'a str,
    ) -> impl Iterator<Item = &'a DatapackEntryDecl> {
        let wanted = registry.strip_prefix("minecraft:").unwrap_or(registry);
        self.datapack_entries.iter().filter(move |entry| {
            entry
                .registry
                .strip_prefix("minecraft:")
                .unwrap_or(&entry.registry)
                == wanted
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declaration(mod_id: &str) -> Declaration {
        Declaration {
            mod_info: ModInfo {
                id: mod_id.into(),
                version: "1.0".into(),
                display_name: String::new(),
            },
            channels: Vec::new(),
            registries: Vec::new(),
            config_files: Vec::new(),
            datapack_entries: Vec::new(),
            data_maps: Vec::new(),
            feature_flags: Vec::new(),
            allow_non_neoforge: true,
            reject_message: None,
        }
    }

    fn channel(id: &str, phases: &[Phase], flow: Option<Flow>) -> ChannelDecl {
        ChannelDecl {
            id: id.into(),
            version: "1".into(),
            phases: phases.to_vec(),
            flow,
            optional: false,
        }
    }

    #[test]
    fn validation_normalizes_and_rejects() {
        let mut d = declaration("mymod");
        d.channels
            .push(channel("mymod:net", &[Phase::Play, Phase::Play], None));
        let d = d.validated().unwrap();
        assert_eq!(d.mod_info.display_name, "mymod");
        assert_eq!(d.channels[0].phases, [Phase::Play]);

        let mut d = declaration("mymod");
        d.channels.push(channel("neoforge:x", &[Phase::Play], None));
        assert!(d.validated().is_err());

        let mut d = declaration("mymod");
        d.channels.push(channel("mymod:x", &[], None));
        assert!(d.validated().is_err());

        let mut d = declaration("mymod");
        d.channels.push(channel("mymod:x", &[Phase::Login], None));
        assert!(d.validated().is_err());

        let mut d = declaration("mymod");
        d.registries.push(RegistryDecl {
            key: "minecraft:item".into(),
            entries: vec!["mymod:a".into(), "mymod:a".into()],
        });
        assert!(d.validated().is_err());

        assert!(declaration("Bad Id").validated().is_err());
    }

    #[test]
    fn aggregation_merges_all_plugins() {
        let mut a = declaration("moda");
        a.channels.push(channel(
            "moda:net",
            &[Phase::Configuration, Phase::Play],
            None,
        ));
        a.channels.push(channel(
            "moda:down",
            &[Phase::Configuration],
            Some(Flow::Clientbound),
        ));
        a.registries.push(RegistryDecl {
            key: "minecraft:item".into(),
            entries: vec!["moda:gem".into(), "moda:ore".into()],
        });
        a.config_files.push(ConfigFileDecl {
            name: "moda-server.toml".into(),
            content: b"x=1".to_vec(),
        });
        a.allow_non_neoforge = false;
        a.reject_message = Some("Need {mods}".into());
        let a = a.validated().unwrap();

        let mut b = declaration("modb");
        b.mod_info.display_name = "Mod B".into();
        b.channels
            .push(channel("modb:net", &[Phase::Play], Some(Flow::Serverbound)));
        b.registries.push(RegistryDecl {
            key: "minecraft:item".into(),
            entries: vec!["modb:tool".into(), "moda:gem".into()],
        });
        b.registries.push(RegistryDecl {
            key: "modb:thing".into(),
            entries: vec!["modb:one".into()],
        });
        let b = b.validated().unwrap();

        let host = vec![SyncedRegistry {
            key: "minecraft:item".into(),
            entries: vec!["minecraft:air".into(), "minecraft:stone".into()],
        }];
        let aggregate = Aggregate::build(&[a, b], host);

        assert_eq!(aggregate.mods.len(), 2);
        assert_eq!(aggregate.mod_name("modb").as_deref(), Some("Mod B"));
        assert_eq!(aggregate.configuration_channels_to_server, ["moda:net"]);
        let play: Vec<&str> = aggregate.channels[&Phase::Play]
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert!(play.contains(&"moda:net") && play.contains(&"modb:net"));
        assert!(play.contains(&"neoforge:recipe_content"));

        assert_eq!(aggregate.registries.len(), 2);
        assert_eq!(
            aggregate.registries[0].entries,
            [
                "minecraft:air",
                "minecraft:stone",
                "moda:gem",
                "moda:ore",
                "modb:tool"
            ]
        );
        assert_eq!(aggregate.registries[1].key, "modb:thing");
        assert_eq!(aggregate.registry_payloads.len(), 2);

        assert!(!aggregate.allow_non_neoforge);
        assert_eq!(
            aggregate.text(&aggregate.reject_message, ""),
            "Need moda 1.0, Mod B 1.0"
        );
        let files = aggregate.config_files_to_send();
        assert_eq!(files[0].0, "neoforge-server.toml");
        assert_eq!(files[1].0, "moda-server.toml");
    }

    #[test]
    fn everything_allows_only_when_every_plugin_allows() {
        let allow = declaration("moda").validated().unwrap();
        assert!(Aggregate::build(std::slice::from_ref(&allow), Vec::new()).allow_non_neoforge);
        let mut reject = declaration("modb");
        reject.allow_non_neoforge = false;
        let reject = reject.validated().unwrap();
        let aggregate = Aggregate::build(&[allow, reject], Vec::new());
        assert!(!aggregate.allow_non_neoforge);
        assert_eq!(aggregate.reject_message, DEFAULT_REJECT_MESSAGE);
    }

    #[test]
    fn declaring_replaces_and_conflicts_are_rejected() {
        // The store is global, use names no other test uses.
        let mut a = declaration("storea");
        a.channels.push(channel("storea:net", &[Phase::Play], None));
        declare("store-plugin-a", a.clone()).unwrap();
        // Declaring the same again is accepted.
        declare("store-plugin-a", a).unwrap();

        let mut other = declaration("storeb");
        other
            .channels
            .push(channel("storea:net", &[Phase::Play], None));
        assert!(declare("store-plugin-b", other).is_err());
        assert!(declare("store-plugin-c", declaration("storea")).is_err());

        let mut replaced = declaration("storea");
        replaced.feature_flags.push("storea:flag".into());
        declare("store-plugin-a", replaced).unwrap();
        let current = current().expect("a declaration exists");
        assert!(current.feature_flags_payload.is_some());

        withdraw("store-plugin-a");
        withdraw("store-plugin-b");
        withdraw("store-plugin-c");
    }
}
