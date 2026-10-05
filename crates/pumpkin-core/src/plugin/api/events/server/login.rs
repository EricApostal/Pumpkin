use pumpkin_macros::Event;
use uuid::Uuid;

use super::configuration::DEFAULT_HOLD_TIMEOUT_SECONDS;

/// A Java client finished authenticating, before the server sends "login finished".
///
/// Handlers may set [`hold`](Self::hold) to exchange login queries with the client first. Only
/// handlers that block can set it, the changes of others are ignored.
#[derive(Event, Clone)]
pub struct LoginStartEvent {
    /// Identifies the connection, no player exists yet.
    pub connection_id: u64,
    pub uuid: Uuid,
    pub username: String,
    /// The protocol version the client connected with.
    pub protocol_version: i32,
    /// Hold the login until released or `hold_timeout_seconds` pass.
    pub hold: bool,
    pub hold_timeout_seconds: u32,
}

impl LoginStartEvent {
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

/// The client answered a login query a plugin sent.
#[derive(Event, Clone)]
pub struct LoginQueryAnswerEvent {
    pub connection_id: u64,
    pub uuid: Uuid,
    pub username: String,
    pub query_id: i32,
    /// `None` when the client did not understand the query.
    pub data: Option<Vec<u8>>,
}

impl LoginQueryAnswerEvent {
    #[must_use]
    pub const fn new(
        connection_id: u64,
        uuid: Uuid,
        username: String,
        query_id: i32,
        data: Option<Vec<u8>>,
    ) -> Self {
        Self {
            connection_id,
            uuid,
            username,
            query_id,
            data,
        }
    }
}
