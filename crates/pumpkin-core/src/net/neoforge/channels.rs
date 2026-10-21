//! The payload channels and registries of `NeoForge`'s own protocol (26.3.x).

use super::payloads::{Flow, Phase, QueryComponent};

// The two channel lists as the mod mechanism knows them (`pumpkin_protocol::mods` uses them to
// learn which mods a connection has), so there is one definition of each name.
pub const MINECRAFT_REGISTER: &str = pumpkin_protocol::mods::MINECRAFT_REGISTER;
pub const MINECRAFT_UNREGISTER: &str = pumpkin_protocol::mods::MINECRAFT_UNREGISTER;
/// The query (what the client registered), and the empty one the server sends first.
pub const MODDED_NETWORK_QUERY: &str = "neoforge:register";
/// The negotiated channels.
pub const MODDED_NETWORK: &str = "neoforge:network";
/// Why the negotiation failed, for the client's mismatch screen.
pub const MODDED_NETWORK_SETUP_FAILED: &str = "neoforge:modded_network_setup_failed";
pub const COMMON_VERSION: &str = "c:version";
pub const COMMON_REGISTER: &str = "c:register";

pub const FROZEN_REGISTRY_SYNC_START: &str = "neoforge:frozen_registry_sync_start";
pub const FROZEN_REGISTRY: &str = "neoforge:frozen_registry";
pub const FROZEN_REGISTRY_SYNC_COMPLETED: &str = "neoforge:frozen_registry_sync_completed";
pub const KNOWN_REGISTRY_DATA_MAPS: &str = "neoforge:known_registry_data_maps";
pub const KNOWN_REGISTRY_DATA_MAPS_REPLY: &str = "neoforge:known_registry_data_maps_reply";
pub const EXTENSIBLE_ENUM_DATA: &str = "neoforge:extensible_enum_data";
pub const EXTENSIBLE_ENUM_ACK: &str = "neoforge:extensible_enum_ack";
pub const FEATURE_FLAGS: &str = "neoforge:feature_flags";
pub const FEATURE_FLAGS_ACK: &str = "neoforge:feature_flags_ack";
pub const CONFIG_FILE: &str = "neoforge:config_file";
pub const SPLIT: &str = "neoforge:split";
/// Play phase, sent before the recipes.
pub const RECIPE_CONTENT: &str = "neoforge:recipe_content";
/// The auxiliary light of a chunk (`AuxiliaryLightDataPayload`), sent with the chunk itself.
pub const AUXILIARY_LIGHT_DATA: &str = "neoforge:auxiliary_light_data";

/// The channels usable before any negotiation (`BUILTIN_PAYLOADS`).
pub const BUILTIN: [&str; 7] = [
    MINECRAFT_REGISTER,
    MINECRAFT_UNREGISTER,
    MODDED_NETWORK_QUERY,
    MODDED_NETWORK,
    MODDED_NETWORK_SETUP_FAILED,
    COMMON_VERSION,
    COMMON_REGISTER,
];

/// What a `NeoForge` server announces it can receive in the configuration phase: the builtin
/// channels and every optional configuration payload that flows to the server.
pub const SERVER_CONFIGURATION_LISTENING: [&str; 12] = [
    MINECRAFT_REGISTER,
    MINECRAFT_UNREGISTER,
    MODDED_NETWORK_QUERY,
    MODDED_NETWORK,
    MODDED_NETWORK_SETUP_FAILED,
    COMMON_VERSION,
    COMMON_REGISTER,
    FROZEN_REGISTRY_SYNC_COMPLETED,
    KNOWN_REGISTRY_DATA_MAPS_REPLY,
    EXTENSIBLE_ENUM_ACK,
    FEATURE_FLAGS_ACK,
    SPLIT,
];

/// The payloads a client must announce for the registry synchronisation to work.
pub const REGISTRY_SYNC_REQUIRED: [&str; 3] = [
    FROZEN_REGISTRY_SYNC_START,
    FROZEN_REGISTRY,
    FROZEN_REGISTRY_SYNC_COMPLETED,
];

/// The `c:` common network version (the only one that exists).
pub const COMMON_NETWORK_VERSION: i32 = 1;

/// The synced config file `NeoForge` itself sends (`ConfigTracker.defaultConfigName`: `<modid>-<type>.toml`,
/// `SYNCED`).
///
/// The client looks the name up in its own config map (`ConfigSync.receiveSyncedConfig`) and silently
/// ignores a name it does not know; the config then stays unloaded and the first read of one of its
/// values throws "Cannot get config value before config is loaded" — which happens in
/// `Level.guardEntityTick` when an entity tick fails, turning a logged error into a crash.
pub const NEOFORGE_SERVER_CONFIG_FILE: &str = "neoforge-synced.toml";

/// The body of [`NEOFORGE_SERVER_CONFIG_FILE`]: the defaults of `NeoForgeSyncedConfig`, so the client
/// has a loaded config. A real server sends its file, which is these keys with comments.
pub const NEOFORGE_SERVER_CONFIG: &str = "\
removeErroringBlockEntities = false
removeErroringEntities = false
fullBoundingBoxLadders = false
permissionHandler = \"neoforge:default_handler\"
advertiseDedicatedServerToLan = false
";

/// An empty `neoforge:recipe_content`: no recipe types and no recipes. It makes the client fire
/// `RecipesReceivedEvent`.
pub const EMPTY_RECIPE_CONTENT: [u8; 2] = [0, 0];

fn own(id: &str, flow: Option<Flow>) -> QueryComponent {
    QueryComponent {
        id: id.to_string(),
        version: "1".to_string(),
        flow,
        optional: true,
    }
}

/// `NeoForge`'s own payload registrations of `phase`, as a `NeoForge` peer reports them in
/// `neoforge:register` (all optional, version "1").
#[must_use]
pub fn own_registrations(phase: Phase) -> Vec<QueryComponent> {
    use Flow::{Clientbound, Serverbound};
    match phase {
        Phase::Configuration => vec![
            own(CONFIG_FILE, Some(Clientbound)),
            own(FROZEN_REGISTRY_SYNC_START, Some(Clientbound)),
            own(FROZEN_REGISTRY, Some(Clientbound)),
            own(FROZEN_REGISTRY_SYNC_COMPLETED, None),
            own(KNOWN_REGISTRY_DATA_MAPS, Some(Clientbound)),
            own(EXTENSIBLE_ENUM_DATA, Some(Clientbound)),
            own(FEATURE_FLAGS, Some(Clientbound)),
            own(KNOWN_REGISTRY_DATA_MAPS_REPLY, Some(Serverbound)),
            own(EXTENSIBLE_ENUM_ACK, Some(Serverbound)),
            own(FEATURE_FLAGS_ACK, Some(Serverbound)),
            own(SPLIT, None),
        ],
        Phase::Play => vec![
            own(CONFIG_FILE, Some(Clientbound)),
            own("neoforge:advanced_add_entity", Some(Clientbound)),
            own("neoforge:advanced_open_screen", Some(Clientbound)),
            own(AUXILIARY_LIGHT_DATA, Some(Clientbound)),
            own("neoforge:registry_data_map_sync", Some(Clientbound)),
            own("neoforge:advanced_container_set_data", Some(Clientbound)),
            own(RECIPE_CONTENT, Some(Clientbound)),
            own("neoforge:sync_attachments", Some(Clientbound)),
            own(SPLIT, None),
        ],
        _ => Vec::new(),
    }
}
