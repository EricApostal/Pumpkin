use bytes::Bytes;
use pumpkin_macros::{Event, cancellable};
use uuid::Uuid;

/// Default for the `hold_timeout_seconds` of the events that can hold a connection.
pub const DEFAULT_HOLD_TIMEOUT_SECONDS: u32 = 30;

/// A Java client entered the configuration phase, right before the server sends its brand.
///
/// Handlers may set [`hold`](Self::hold) to speak first: the brand and everything after it
/// (including [`ConfigurationStartEvent`]) wait until the connection is released. Mod loaders
/// that classify the connection when they see the brand (NeoForge) need their negotiation to
/// arrive before it. Only handlers that block can set it, the changes of others are ignored.
/// Fired after multiversion packet translation, so older clients behave like current ones.
#[derive(Event, Clone)]
pub struct ConfigurationPreBrandEvent {
    /// Identifies the connection, no player exists yet.
    pub connection_id: u64,
    /// The authenticated profile of the connecting client.
    pub uuid: Uuid,
    pub username: String,
    /// The protocol version the client connected with.
    pub protocol_version: i32,
    /// Hold the brand and the remaining configuration until released or `hold_timeout_seconds`
    /// pass.
    pub hold: bool,
    pub hold_timeout_seconds: u32,
}

impl ConfigurationPreBrandEvent {
    #[must_use]
    pub const fn new(
        connection_id: u64,
        uuid: Uuid,
        username: String,
        protocol_version: i32,
    ) -> Self {
        Self {
            connection_id,
            uuid,
            username,
            protocol_version,
            hold: false,
            hold_timeout_seconds: DEFAULT_HOLD_TIMEOUT_SECONDS,
        }
    }
}

/// A Java client entered the configuration phase, after login and before the world is sent.
///
/// Handlers may set [`hold`](Self::hold) to pause the configuration after the server brand until
/// the connection is released, to run a custom handshake over [`ConfigurationPayloadEvent`]s.
/// Only handlers that block can set it, the changes of others are ignored.
/// Fired after multiversion packet translation, so older clients behave like current ones.
#[derive(Event, Clone)]
pub struct ConfigurationStartEvent {
    /// Identifies the connection, no player exists yet.
    pub connection_id: u64,
    /// The authenticated profile of the connecting client.
    pub uuid: Uuid,
    pub username: String,
    /// The protocol version the client connected with.
    pub protocol_version: i32,
    /// Hold the remaining configuration until released or `hold_timeout_seconds` pass.
    pub hold: bool,
    pub hold_timeout_seconds: u32,
}

impl ConfigurationStartEvent {
    #[must_use]
    pub const fn new(
        connection_id: u64,
        uuid: Uuid,
        username: String,
        protocol_version: i32,
    ) -> Self {
        Self {
            connection_id,
            uuid,
            username,
            protocol_version,
            hold: false,
            hold_timeout_seconds: DEFAULT_HOLD_TIMEOUT_SECONDS,
        }
    }
}

/// A custom payload from a client during the configuration phase.
///
/// The `minecraft:brand` channel is handled by the server and not reported.
#[derive(Event, Clone)]
pub struct ConfigurationPayloadEvent {
    pub connection_id: u64,
    pub uuid: Uuid,
    pub username: String,
    /// The client brand, if it has been received.
    pub brand: Option<String>,
    pub channel: String,
    pub data: Vec<u8>,
}

impl ConfigurationPayloadEvent {
    #[must_use]
    pub const fn new(
        connection_id: u64,
        uuid: Uuid,
        username: String,
        brand: Option<String>,
        channel: String,
        data: Vec<u8>,
    ) -> Self {
        Self {
            connection_id,
            uuid,
            username,
            brand,
            channel,
            data,
        }
    }
}

/// The configuration phase of a client ended.
#[derive(Event, Clone)]
pub struct ConfigurationEndEvent {
    pub connection_id: u64,
    pub uuid: Uuid,
    pub username: String,
    /// `true` when the client finished configuration, `false` when the connection closed first.
    pub completed: bool,
}

impl ConfigurationEndEvent {
    #[must_use]
    pub const fn new(connection_id: u64, uuid: Uuid, username: String, completed: bool) -> Self {
        Self {
            connection_id,
            uuid,
            username,
            completed,
        }
    }
}

/// A packet from a client during the configuration phase, before the server handles it.
///
/// Fired after multiversion packet translation, so `packet_id` and `payload` are in the 26.3
/// format. Cancelling makes the server ignore the packet, so a plugin can answer it on its own.
#[cancellable]
#[derive(Event, Clone)]
pub struct ConfigurationPacketReceivedEvent {
    pub connection_id: u64,
    pub uuid: Uuid,
    pub username: String,
    pub packet_id: i32,
    /// The packet body without its id.
    pub payload: Bytes,
}

impl ConfigurationPacketReceivedEvent {
    #[must_use]
    pub const fn new(
        connection_id: u64,
        uuid: Uuid,
        username: String,
        packet_id: i32,
        payload: Bytes,
    ) -> Self {
        Self {
            connection_id,
            uuid,
            username,
            packet_id,
            payload,
            cancelled: false,
        }
    }
}

/// The server is about to send "finish configuration", after the registries and tags.
///
/// Handlers may set [`hold`](Self::hold) to run more steps first, like the configuration tasks of
/// mod loaders. Only handlers that block can set it, the changes of others are ignored.
#[derive(Event, Clone)]
pub struct ConfigurationFinishEvent {
    pub connection_id: u64,
    pub uuid: Uuid,
    pub username: String,
    /// Hold "finish configuration" until released or `hold_timeout_seconds` pass.
    pub hold: bool,
    pub hold_timeout_seconds: u32,
}

impl ConfigurationFinishEvent {
    #[must_use]
    pub const fn new(connection_id: u64, uuid: Uuid, username: String) -> Self {
        Self {
            connection_id,
            uuid,
            username,
            hold: false,
            hold_timeout_seconds: DEFAULT_HOLD_TIMEOUT_SECONDS,
        }
    }
}
