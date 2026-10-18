/// Allows the plugin to perform DNS resolution (resolving hostnames to IP addresses).
///
/// Corresponds to the `wasi:sockets/ip-name-lookup` interface.
pub const NETWORK_DNS: &str = "network.dns";

/// Allows the plugin to use TCP sockets.
pub const NETWORK_TCP: &str = "network.tcp";

/// Allows the plugin to use UDP sockets.
pub const NETWORK_UDP: &str = "network.udp";

/// Allows the plugin to initiate TCP connections.
pub const NETWORK_TCP_CONNECT: &str = "network.tcp.connect";

/// Allows the plugin to bind TCP listeners (accept inbound connections).
pub const NETWORK_TCP_BIND: &str = "network.tcp.bind";

/// Allows the plugin to send and receive UDP packets to specific destinations.
pub const NETWORK_UDP_CONNECT: &str = "network.udp.connect";

/// Allows the plugin to bind UDP sockets to local ports.
pub const NETWORK_UDP_BIND: &str = "network.udp.bind";

/// Allows the plugin to send datagram on non-connected UDP socket.
pub const NETWORK_UDP_OUTGOING_DATAGRAM: &str = "network.udp.outgoingdatagram";

/// Restricts all networking permissions to loopback addresses (localhost) only.
pub const NETWORK_LOOPBACK: &str = "network.loopback";

/// Allows the plugin to make outbound TCP/UDP connections.
///
/// This gives the plugin full access to the host's network interfaces.
/// **Warning:** This is a powerful permission and should only be granted to trusted plugins.
pub const NETWORK_OUTBOUND: &str = "network.outbound";

/// Allows the plugin to make outbound HTTP connections.
///
/// This is separate from `network.outbound`. This allows the use of `wasi:http`; the other allows the more powerful `wasi:sockets`.
pub const HTTP_OUTBOUND: &str = "http.outbound";

/// Allows the plugin to read files within its own data folder (`plugins/data/<name>`).
pub const FS_READ_DATA: &str = "fs.read.data";

/// Allows the plugin to write (and read) files within its own data folder (`plugins/data/<name>`).
///
/// Note that this permission also implies `FS_READ_DATA`
pub const FS_WRITE_DATA: &str = "fs.write.data";

/// Allows the plugin to read all environment variables.
pub const SYS_ENV: &str = "sys.env";

/// Allows the plugin to read specific environment variables.
/// Used with a prefix like "sys.env.PATH".
pub const SYS_ENV_PREFIX: &str = "sys.env.";

/// Allows the plugin to read system information (CPU, Memory, OS).
pub const SYS_INFO: &str = "sys.info";

/// Allows the plugin to read CPU information.
pub const SYS_INFO_CPU: &str = "sys.info.cpu";

/// Allows the plugin to read RAM information.
pub const SYS_INFO_RAM: &str = "sys.info.ram";

/// Allows the plugin to read OS information.
pub const SYS_INFO_OS: &str = "sys.info.os";

/// Allows the plugin to register custom items (and item tags) while the server starts.
pub const REGISTRY_ITEMS: &str = "registry.items";

/// Allows the plugin to register custom blocks (and block tags) while the server starts.
pub const REGISTRY_BLOCKS: &str = "registry.blocks";

/// Allows the plugin to register custom fluids while the server starts.
pub const REGISTRY_FLUIDS: &str = "registry.fluids";

/// Allows the plugin to register custom block entity types while the server starts.
///
/// Block entity types are a registry that clients synchronise, their ids are handed to clients.
pub const REGISTRY_BLOCK_ENTITIES: &str = "registry.block-entities";

/// Allows the plugin to register custom menu types while the server starts.
///
/// Menu types are a registry that clients synchronise, their ids are handed to clients.
pub const REGISTRY_MENUS: &str = "registry.menus";

/// Allows the plugin to register custom data component types while the server starts.
///
/// Component types are a registry that clients synchronise, their ids are written into every item
/// stack that carries one.
pub const REGISTRY_COMPONENTS: &str = "registry.components";

/// Allows the plugin to register custom damage types while the server starts.
///
/// Damage types are a registry that clients synchronise, their ids are handed to clients.
pub const REGISTRY_DAMAGE_TYPES: &str = "registry.damage-types";

/// Allows the plugin to register custom entity types while the server starts.
///
/// Entity types are a registry that clients synchronise, their ids are handed to clients.
pub const REGISTRY_ENTITIES: &str = "registry.entities";

/// Allows the plugin to register custom recipe types and add recipes of those types.
///
/// Recipe types are plugin side data, they are not sent to clients and can be added at any time.
pub const REGISTRY_RECIPES: &str = "registry.recipes";

/// Allows the plugin to create fake players: server side actors that behave like players
/// (break blocks, use items, attack) without a connection.
pub const FAKE_PLAYERS: &str = "fake-players";

/// Allows the plugin to move items in and out of the containers of blocks with item handlers, and
/// to give plugin block entities inventories.
pub const CONTAINERS: &str = "containers";

/// Allows the plugin to declare mods, channels and registries for the NeoForge client handshake
/// with `neoforge`. Declarations are only possible while the server starts.
pub const NEOFORGE: &str = "neoforge";

#[must_use]
pub fn get_permission_description(permission: &str) -> Option<&'static str> {
    match permission {
        NETWORK_DNS => Some(
            "Allows the plugin to perform DNS resolution (resolving hostnames to IP addresses).",
        ),
        NETWORK_TCP => Some("Allows the plugin to use TCP sockets."),
        NETWORK_UDP => Some("Allows the plugin to use UDP sockets."),
        NETWORK_TCP_CONNECT => Some("Allows the plugin to initiate TCP connections."),
        NETWORK_TCP_BIND => {
            Some("Allows the plugin to bind TCP listeners (accept inbound connections).")
        }
        NETWORK_UDP_CONNECT => {
            Some("Allows the plugin to send and receive UDP packets to specific destinations.")
        }
        NETWORK_UDP_BIND => Some("Allows the plugin to bind UDP sockets to local ports."),
        NETWORK_UDP_OUTGOING_DATAGRAM => {
            Some("Allows the plugin to send datagram on non-connected UDP socket.")
        }
        NETWORK_LOOPBACK => {
            Some("Restricts all networking permissions to loopback addresses (localhost) only.")
        }
        NETWORK_OUTBOUND => {
            Some("Allows the plugin to make outbound TCP/UDP connections. (POWERFUL)")
        }
        HTTP_OUTBOUND => {
            Some("Allows the plugin to make outbound HTTP requests (through `wasi:http`)")
        }
        FS_READ_DATA => Some("Allows the plugin to read files within its own data folder."),
        FS_WRITE_DATA => Some(
            "Allows the plugin to write (and read) files within its own data folder. Implies `fs.read.data`.",
        ),
        SYS_ENV => Some("Allows the plugin to read all environment variables."),
        SYS_INFO => Some("Allows the plugin to read system information (CPU, Memory, OS)."),
        SYS_INFO_CPU => Some("Allows the plugin to read CPU information."),
        SYS_INFO_RAM => Some("Allows the plugin to read RAM information."),
        SYS_INFO_OS => Some("Allows the plugin to read OS information."),
        REGISTRY_ITEMS => Some("Allows the plugin to register custom items and item tags."),
        REGISTRY_BLOCKS => Some("Allows the plugin to register custom blocks and block tags."),
        REGISTRY_FLUIDS => Some("Allows the plugin to register custom fluids."),
        REGISTRY_BLOCK_ENTITIES => Some("Allows the plugin to register custom block entity types."),
        REGISTRY_MENUS => Some("Allows the plugin to register custom menu types."),
        REGISTRY_COMPONENTS => Some("Allows the plugin to register custom data component types."),
        REGISTRY_DAMAGE_TYPES => Some("Allows the plugin to register custom damage types."),
        REGISTRY_ENTITIES => Some("Allows the plugin to register custom entity types."),
        REGISTRY_RECIPES => Some("Allows the plugin to register custom recipe types and recipes."),
        FAKE_PLAYERS => Some(
            "Allows the plugin to create fake players that break blocks, use items and attack like players.",
        ),
        NEOFORGE => Some(
            "Allows the plugin to declare mods, channels and registries for the NeoForge handshake.",
        ),
        CONTAINERS => Some(
            "Allows the plugin to move items in and out of block containers and give plugin block entities inventories.",
        ),
        p if p.starts_with(SYS_ENV_PREFIX) => {
            Some("Allows the plugin to read specific environment variables.")
        }
        _ => None,
    }
}
