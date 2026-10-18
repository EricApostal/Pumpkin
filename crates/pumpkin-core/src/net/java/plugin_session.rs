//! Plugin access to the login and configuration phases of a Java connection.
//!
//! A [`ConnectionSession`] lives on the [`PendingConnection`] of a client that is logging in or
//! configuring. Plugins reach it through a [`ConnectionHandle`] registered on the [`Server`] and
//! drive it with [`ConnectionCommand`]s, which the connection task picks up between packets.
//! Plugins can hold the connection at four points ([`HoldStage`]) to exchange their own packets
//! with the client, and release or disconnect it afterwards.

use std::{
    collections::HashSet,
    fmt,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use crossbeam::atomic::AtomicCell;
use pumpkin_protocol::{
    ConnectionFlavour, ConnectionState, MAX_PACKET_SIZE, RawPacket,
    codec::var_int::VarInt,
    java::client::{
        config::{CFinishConfig, CPluginMessage},
        login::{CLoginPluginRequest, CLoginSuccess},
    },
};
use pumpkin_util::text::TextComponent;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    plugin::server::{
        configuration::{
            ConfigurationEndEvent, ConfigurationFinishEvent, ConfigurationPacketReceivedEvent,
            ConfigurationPayloadEvent, ConfigurationPreBrandEvent, ConfigurationStartEvent,
        },
        login::{LoginQueryAnswerEvent, LoginStartEvent},
    },
    server::Server,
};

use super::pending::PendingConnection;

/// How many commands may wait for the connection task before further ones are rejected.
const COMMAND_QUEUE_CAPACITY: usize = 256;

/// Vanilla `ClientboundCustomPayloadPacket.MAX_PAYLOAD_SIZE`, also the limit of login queries.
const MAX_CUSTOM_PAYLOAD_SIZE: usize = 1_048_576;

/// Room for the packet id in front of a raw packet body.
const PACKET_ID_RESERVE: usize = 5;

/// Which part of the connection a plugin may currently drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionPhase {
    Login,
    Configuration,
    /// Past configuration or closed.
    Done,
}

impl fmt::Display for ConnectionPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Login => "login",
            Self::Configuration => "configuration",
            Self::Done => "finished",
        })
    }
}

/// Where a plugin holds a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldStage {
    /// Before "login finished".
    LoginStart,
    /// Right after the configuration began, before the server brand.
    ConfigurationPreBrand,
    /// After the server brand, before the rest of the configuration.
    ConfigurationStart,
    /// After the registries and tags, before "finish configuration".
    ConfigurationFinish,
}

impl fmt::Display for HoldStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LoginStart => "login",
            Self::ConfigurationPreBrand => "configuration before the brand",
            Self::ConfigurationStart => "configuration start",
            Self::ConfigurationFinish => "configuration finish",
        })
    }
}

/// Something a plugin asks of a connection.
#[derive(Debug, PartialEq, Eq)]
pub enum ConnectionCommand {
    /// Send a custom payload to a client in the configuration phase.
    SendPayload { channel: String, data: Vec<u8> },
    /// Send a clientbound configuration packet (26.3 id, body without the id).
    SendPacket { packet_id: i32, payload: Vec<u8> },
    /// Send a login query to a client in the login phase.
    SendLoginQuery {
        query_id: i32,
        channel: String,
        data: Vec<u8>,
    },
    /// Continue the connection from wherever it is held.
    Release,
    /// Disconnect the client with a reason.
    Disconnect(String),
}

impl ConnectionCommand {
    /// A [`SendPayload`](Self::SendPayload) with the channel and size checked, because the client
    /// fails to decode an invalid packet.
    pub fn send_payload(channel: String, data: Vec<u8>) -> Result<Self, String> {
        check_custom_payload(&channel, &data)?;
        Ok(Self::SendPayload { channel, data })
    }

    /// A [`SendPacket`](Self::SendPacket) with the id and size checked.
    pub fn send_packet(packet_id: i32, payload: Vec<u8>) -> Result<Self, String> {
        if packet_id < 0 {
            return Err(format!("invalid packet id {packet_id}"));
        }
        if payload.len() + PACKET_ID_RESERVE > MAX_PACKET_SIZE as usize {
            return Err(format!("packet of {} bytes is too large", payload.len()));
        }
        Ok(Self::SendPacket { packet_id, payload })
    }
}

fn check_custom_payload(channel: &str, data: &[u8]) -> Result<(), String> {
    if !is_valid_channel(channel) {
        return Err(format!("invalid channel '{channel}'"));
    }
    if channel.len() + data.len() > MAX_CUSTOM_PAYLOAD_SIZE {
        return Err(format!(
            "payload of {} bytes exceeds the limit of {MAX_CUSTOM_PAYLOAD_SIZE} bytes",
            data.len()
        ));
    }
    Ok(())
}

/// A channel is an identifier, `namespace:path` with the namespace optional.
fn is_valid_channel(channel: &str) -> bool {
    let valid = |part: &str, extra: &[char]| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-' | '.') || extra.contains(&c))
    };
    match channel.split_once(':') {
        Some((namespace, path)) => valid(namespace, &[]) && valid(path, &['/']),
        None => valid(channel, &['/']),
    }
}

/// State both the plugin facing handle and the connection need to see right away.
struct Shared {
    phase: AtomicCell<ConnectionPhase>,
    /// The play phase encoding a plugin chose, read by the connection when the session ends.
    flavour: AtomicCell<ConnectionFlavour>,
    /// Ids of login queries that wait for an answer.
    queries: Mutex<HashSet<i32>>,
}

impl Shared {
    fn queries(&self) -> MutexGuard<'_, HashSet<i32>> {
        self.queries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The sending side of a connection's command queue, registered on the [`Server`].
#[derive(Clone)]
pub struct ConnectionHandle {
    sender: mpsc::Sender<ConnectionCommand>,
    shared: Arc<Shared>,
}

impl ConnectionHandle {
    fn check_phase(&self, phase: ConnectionPhase) -> Result<(), String> {
        if self.shared.phase.load() == phase {
            Ok(())
        } else {
            Err(format!("the connection is not in the {phase} phase"))
        }
    }

    /// Never waits, plugins call this from event handlers.
    fn enqueue(&self, command: ConnectionCommand) -> Result<(), String> {
        self.sender.try_send(command).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => "too many pending commands".to_string(),
            mpsc::error::TrySendError::Closed(_) => "the connection is closed".into(),
        })
    }

    /// Queues a command if the connection is in `phase`.
    pub fn send(&self, phase: ConnectionPhase, command: ConnectionCommand) -> Result<(), String> {
        self.check_phase(phase)?;
        self.enqueue(command)
    }

    /// Chooses how the play phase of the connection is encoded, right away. Only while the
    /// connection is logging in or configuring, afterwards the client has taken the value over.
    pub fn set_flavour(&self, flavour: ConnectionFlavour) -> Result<(), String> {
        if self.shared.phase.load() == ConnectionPhase::Done {
            return Err("the connection is not logging in or configuring".to_string());
        }
        self.shared.flavour.store(flavour);
        Ok(())
    }

    /// Queues a login query and reserves its id until the client answers.
    pub fn send_login_query(
        &self,
        query_id: i32,
        channel: String,
        data: Vec<u8>,
    ) -> Result<(), String> {
        self.check_phase(ConnectionPhase::Login)?;
        check_custom_payload(&channel, &data)?;
        if !self.shared.queries().insert(query_id) {
            return Err(format!("login query id {query_id} is already in use"));
        }
        self.enqueue(ConnectionCommand::SendLoginQuery {
            query_id,
            channel,
            data,
        })
        .inspect_err(|_| {
            self.shared.queries().remove(&query_id);
        })
    }
}

/// Whether the connection is paused for a plugin, and for how long.
#[derive(Debug, Default)]
pub struct ConnectionHold {
    held: Option<(HoldStage, Instant)>,
}

impl ConnectionHold {
    /// Pauses at `stage` until [`release`](Self::release), or until `timeout` has passed since `now`.
    pub fn hold(&mut self, stage: HoldStage, now: Instant, timeout: Duration) {
        // A timeout too large to represent never expires.
        let deadline = now
            .checked_add(timeout)
            .unwrap_or_else(|| now + Duration::from_hours(24 * 365));
        self.held = Some((stage, deadline));
    }

    /// Ends the hold. The stage when there was one, so the caller continues from there.
    pub const fn release(&mut self) -> Option<HoldStage> {
        match self.held.take() {
            Some((stage, _)) => Some(stage),
            None => None,
        }
    }

    #[must_use]
    pub const fn is_held(&self) -> bool {
        self.held.is_some()
    }

    #[must_use]
    pub const fn stage(&self) -> Option<HoldStage> {
        match self.held {
            Some((stage, _)) => Some(stage),
            None => None,
        }
    }

    #[must_use]
    pub const fn deadline(&self) -> Option<Instant> {
        match self.held {
            Some((_, deadline)) => Some(deadline),
            None => None,
        }
    }

    #[must_use]
    pub fn is_expired(&self, now: Instant) -> bool {
        self.deadline().is_some_and(|deadline| now >= deadline)
    }
}

/// Resolves at `deadline`, never without one.
pub(super) async fn sleep_until_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

/// The plugin facing state of one connection.
pub struct ConnectionSession {
    commands: mpsc::Receiver<ConnectionCommand>,
    shared: Arc<Shared>,
    hold: ConnectionHold,
    /// The configuration phase began, so `ConfigurationEndEvent` is due.
    configuration_started: bool,
    /// `CFinishConfig` went out, the client leaves the configuration state with its next packet.
    finish_sent: bool,
    /// The client acknowledged the end of the configuration.
    client_finished: bool,
}

impl ConnectionSession {
    fn new(phase: ConnectionPhase) -> (Self, ConnectionHandle) {
        let (sender, commands) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
        let shared = Arc::new(Shared {
            phase: AtomicCell::new(phase),
            flavour: AtomicCell::new(ConnectionFlavour::Vanilla),
            queries: Mutex::new(HashSet::new()),
        });
        let session = Self {
            commands,
            shared: Arc::clone(&shared),
            hold: ConnectionHold::default(),
            configuration_started: false,
            finish_sent: false,
            client_finished: false,
        };
        (session, ConnectionHandle { sender, shared })
    }

    /// The client acknowledged "finish configuration".
    pub(super) const fn mark_client_finished(&mut self) {
        self.client_finished = true;
    }
}

impl PendingConnection {
    fn identity(&self) -> Option<(Uuid, String)> {
        self.gameprofile
            .as_ref()
            .map(|profile| (profile.id, profile.name.clone()))
    }

    /// Registers the connection for plugins, unless that happened already or no plugin cares.
    fn ensure_session(&mut self, server: &Arc<Server>, phase: ConnectionPhase) -> bool {
        if let Some(session) = &self.session {
            session.shared.phase.store(phase);
            return true;
        }
        let plugins = &server.plugin_manager;
        if !(plugins.has_handlers::<LoginStartEvent>()
            || plugins.has_handlers::<LoginQueryAnswerEvent>()
            || plugins.has_handlers::<ConfigurationPreBrandEvent>()
            || plugins.has_handlers::<ConfigurationStartEvent>()
            || plugins.has_handlers::<ConfigurationPayloadEvent>()
            || plugins.has_handlers::<ConfigurationPacketReceivedEvent>()
            || plugins.has_handlers::<ConfigurationFinishEvent>()
            || plugins.has_handlers::<ConfigurationEndEvent>())
        {
            return false;
        }
        let (session, handle) = ConnectionSession::new(phase);
        // A flavour chosen by the host before a plugin could (the NeoForge handshake).
        session.shared.flavour.store(self.flavour);
        server.plugin_connections.insert(self.id, handle);
        self.session = Some(session);
        true
    }

    /// Holds at `stage` when the event asked for it.
    fn hold_connection(&mut self, stage: HoldStage, hold: bool, timeout_seconds: u32) -> bool {
        if !hold {
            return false;
        }
        let Some(session) = &mut self.session else {
            return false;
        };
        session.hold.hold(
            stage,
            Instant::now(),
            Duration::from_secs(u64::from(timeout_seconds)),
        );
        info!(
            "Holding {stage} of client {} for up to {timeout_seconds}s",
            self.id
        );
        true
    }

    /// Fires `LoginStartEvent` right before "login finished".
    ///
    /// Returns `true` when a plugin holds the login, the caller then must not send "login finished"
    /// before [`ConnectionCommand::Release`] arrives.
    pub(super) async fn begin_login_hook(&mut self, server: &Arc<Server>) -> bool {
        if !self.ensure_session(server, ConnectionPhase::Login) {
            return false;
        }
        if !server.plugin_manager.has_handlers::<LoginStartEvent>() {
            return false;
        }
        let Some((uuid, username)) = self.identity() else {
            return false;
        };
        let mut event = LoginStartEvent::new(
            self.id,
            uuid,
            username,
            self.version.load().protocol_version(),
        );
        server.plugin_manager.fire(server, &mut event).await;
        self.hold_connection(
            HoldStage::LoginStart,
            event.hold,
            event.hold_timeout_seconds,
        )
    }

    /// Sends "login finished" for the profile of the connection.
    pub(super) async fn send_login_success(&mut self) {
        let Some(profile) = self.gameprofile.clone() else {
            return;
        };
        let props = profile.properties.load();
        let packet = CLoginSuccess::new(&profile.id, &profile.name, &props, false, Uuid::new_v4());
        self.send_packet_now(&packet).await;
        self.login_success_sent = true;
    }

    /// Routes a login query answer to the plugin that asked, `false` if it is not a plugin's.
    pub(super) async fn handle_plugin_query_answer(
        &self,
        server: &Arc<Server>,
        query_id: i32,
        data: Option<Vec<u8>>,
    ) -> bool {
        let known = self
            .session
            .as_ref()
            .is_some_and(|session| session.shared.queries().remove(&query_id));
        if !known {
            return false;
        }
        debug!(
            "Client {} answered login query {query_id} ({} bytes)",
            self.id,
            data.as_ref().map_or(0, Vec::len)
        );
        if server
            .plugin_manager
            .has_handlers::<LoginQueryAnswerEvent>()
            && let Some((uuid, username)) = self.identity()
        {
            let mut event = LoginQueryAnswerEvent::new(self.id, uuid, username, query_id, data);
            server.plugin_manager.fire(server, &mut event).await;
        }
        true
    }

    /// Registers the connection for plugins and fires `ConfigurationPreBrandEvent`, right before
    /// the server brand.
    ///
    /// Returns `true` when a plugin holds the connection, the caller then must not send the brand
    /// (nor anything after it) before [`ConnectionCommand::Release`] arrives. Plugin commands such
    /// as payloads and raw packets already work, the connection is in the configuration phase.
    pub(super) async fn begin_pre_brand_hook(&mut self, server: &Arc<Server>) -> bool {
        if !self.ensure_session(server, ConnectionPhase::Configuration) {
            return false;
        }
        if let Some(session) = &mut self.session {
            session.configuration_started = true;
        }
        if !server
            .plugin_manager
            .has_handlers::<ConfigurationPreBrandEvent>()
        {
            return false;
        }
        let Some((uuid, username)) = self.identity() else {
            return false;
        };
        // The session is registered before the event so handlers can queue commands right away.
        let mut event = ConfigurationPreBrandEvent::new(
            self.id,
            uuid,
            username,
            self.version.load().protocol_version(),
        );
        server.plugin_manager.fire(server, &mut event).await;
        self.hold_connection(
            HoldStage::ConfigurationPreBrand,
            event.hold,
            event.hold_timeout_seconds,
        )
    }

    /// Sends the server brand, then fires `ConfigurationStartEvent` and, unless a plugin holds the
    /// configuration there, sends the rest of the configuration.
    ///
    /// This is the part of the configuration phase that follows the pre-brand hook, run directly
    /// when no plugin held there, or once the plugin released the connection.
    pub(super) async fn send_brand_and_continue_configuration(&mut self, server: &Arc<Server>) {
        self.send_packet_now(&server.get_branding()).await;

        // The registries of the NeoForge handshake go out right after the brand and must be
        // acknowledged before the rest of the configuration (registry data and tags).
        if self.neoforge_after_brand().await {
            debug!(
                "Configuration of client {} waits for the NeoForge registry sync",
                self.id
            );
            return;
        }
        self.continue_after_neoforge_registries(server).await;
    }

    /// The pre-brand wait of the `NeoForge` handshake is over: plugins may still hold the
    /// connection before the brand, then the brand goes out.
    pub(super) async fn proceed_after_neoforge_pre_brand(&mut self, server: &Arc<Server>) {
        if self.begin_pre_brand_hook(server).await {
            debug!(
                "Configuration of client {} is held by a plugin before the brand",
                self.id
            );
            return;
        }
        self.send_brand_and_continue_configuration(server).await;
    }

    /// Chooses the encoding of the play phase for the host, ahead of the plugin session.
    pub(super) fn set_native_flavour(&mut self, flavour: ConnectionFlavour) {
        self.flavour = flavour;
        if let Some(session) = &self.session {
            session.shared.flavour.store(flavour);
        }
    }

    /// The part of the configuration after the `NeoForge` registry sync (or without one):
    /// `ConfigurationStartEvent` and the rest of the configuration unless a plugin holds there.
    pub(super) async fn continue_after_neoforge_registries(&mut self, server: &Arc<Server>) {
        if self.begin_configuration_hook(server).await {
            debug!("Configuration of client {} is held by a plugin", self.id);
            return;
        }
        self.continue_configuration(server).await;
    }

    /// Registers the connection for plugins and fires `ConfigurationStartEvent`.
    ///
    /// Returns `true` when a plugin holds the configuration, the caller then must not send the rest
    /// of it before [`ConnectionCommand::Release`] arrives.
    pub(super) async fn begin_configuration_hook(&mut self, server: &Arc<Server>) -> bool {
        if !self.ensure_session(server, ConnectionPhase::Configuration) {
            return false;
        }
        if let Some(session) = &mut self.session {
            session.configuration_started = true;
        }
        if !server
            .plugin_manager
            .has_handlers::<ConfigurationStartEvent>()
        {
            return false;
        }
        let Some((uuid, username)) = self.identity() else {
            return false;
        };
        // The session is registered before the event so handlers can queue commands right away.
        let mut event = ConfigurationStartEvent::new(
            self.id,
            uuid,
            username,
            self.version.load().protocol_version(),
        );
        server.plugin_manager.fire(server, &mut event).await;
        self.hold_connection(
            HoldStage::ConfigurationStart,
            event.hold,
            event.hold_timeout_seconds,
        )
    }

    /// Fires `ConfigurationFinishEvent` right before "finish configuration".
    ///
    /// Returns `true` when a plugin holds it, the caller then must not send "finish configuration"
    /// before [`ConnectionCommand::Release`] arrives.
    pub(super) async fn begin_configuration_finish_hook(&mut self, server: &Arc<Server>) -> bool {
        if !server
            .plugin_manager
            .has_handlers::<ConfigurationFinishEvent>()
            || !self.ensure_session(server, ConnectionPhase::Configuration)
        {
            return false;
        }
        let Some((uuid, username)) = self.identity() else {
            return false;
        };
        let mut event = ConfigurationFinishEvent::new(self.id, uuid, username);
        server.plugin_manager.fire(server, &mut event).await;
        self.hold_connection(
            HoldStage::ConfigurationFinish,
            event.hold,
            event.hold_timeout_seconds,
        )
    }

    /// Sends "finish configuration", the client leaves the configuration state once it is answered.
    pub(super) async fn send_finish_configuration(&mut self) {
        self.send_packet_now(&CFinishConfig).await;
        if let Some(session) = &mut self.session {
            session.finish_sent = true;
        }
    }

    /// The client acknowledged "finish configuration".
    pub(super) const fn mark_configuration_finished(&mut self) {
        if let Some(session) = &mut self.session {
            session.mark_client_finished();
        }
    }

    /// Fires `ConfigurationPacketReceivedEvent`. `None` when a plugin cancelled the packet.
    pub(super) async fn configuration_packet_received(
        &self,
        server: &Arc<Server>,
        packet: RawPacket,
    ) -> Option<RawPacket> {
        if !server
            .plugin_manager
            .has_handlers::<ConfigurationPacketReceivedEvent>()
        {
            return Some(packet);
        }
        let Some((uuid, username)) = self.identity() else {
            return Some(packet);
        };
        let mut event = ConfigurationPacketReceivedEvent::new(
            self.id,
            uuid,
            username,
            packet.id,
            packet.payload,
        );
        server.plugin_manager.fire(server, &mut event).await;
        if event.cancelled {
            debug!(
                "A plugin cancelled configuration packet {} of client {}",
                event.packet_id, self.id
            );
            return None;
        }
        Some(RawPacket {
            id: event.packet_id,
            payload: event.payload,
        })
    }

    /// Fires `ConfigurationPayloadEvent` for a payload the client sent.
    pub(super) async fn fire_configuration_payload(&self, channel: &str, data: &[u8]) {
        let Some(server) = self.server.upgrade() else {
            return;
        };
        if !server
            .plugin_manager
            .has_handlers::<ConfigurationPayloadEvent>()
        {
            return;
        }
        let Some((uuid, username)) = self.identity() else {
            return;
        };
        debug!(
            "Client {} sent {} bytes on configuration channel {channel}",
            self.id,
            data.len()
        );
        let mut event = ConfigurationPayloadEvent::new(
            self.id,
            uuid,
            username,
            self.brand.clone(),
            channel.to_string(),
            data.to_vec(),
        );
        server.plugin_manager.fire(&server, &mut event).await;
    }

    /// Unregisters the connection and fires `ConfigurationEndEvent` once, if the configuration began.
    pub(super) async fn end_plugin_session(&mut self, server: &Arc<Server>) {
        server.plugin_connections.remove(&self.id);
        let Some(session) = self.session.take() else {
            return;
        };
        // Done first, so a plugin that sets the flavour afterwards is told it is too late.
        session.shared.phase.store(ConnectionPhase::Done);
        self.flavour = session.shared.flavour.load();
        if !session.configuration_started {
            return;
        }
        let completed = session.client_finished;
        info!(
            "Configuration of client {} ended, completed: {completed}",
            self.id
        );
        if server
            .plugin_manager
            .has_handlers::<ConfigurationEndEvent>()
            && let Some((uuid, username)) = self.identity()
        {
            let mut event = ConfigurationEndEvent::new(self.id, uuid, username, completed);
            server.plugin_manager.fire(server, &mut event).await;
        }
    }

    pub(super) fn hold_deadline(&self) -> Option<Instant> {
        self.session
            .as_ref()
            .and_then(|session| session.hold.deadline())
    }

    pub(super) fn is_held(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.hold.is_held())
    }

    /// The next command of a plugin. Never resolves without a session or once all handles are gone.
    pub(super) async fn next_plugin_command(&mut self) -> Option<ConnectionCommand> {
        match &mut self.session {
            Some(session) => match session.commands.recv().await {
                Some(command) => Some(command),
                None => std::future::pending().await,
            },
            None => std::future::pending().await,
        }
    }

    /// Whether a configuration packet can still go out.
    fn can_send_configuration_packet(&self) -> bool {
        self.connection_state.load() == ConnectionState::Config
            && self
                .session
                .as_ref()
                .is_some_and(|session| !session.finish_sent)
    }

    /// Runs a plugin command. `false` when the connection is done.
    pub(super) async fn handle_plugin_command(&mut self, command: ConnectionCommand) -> bool {
        match command {
            ConnectionCommand::SendPayload { channel, data } => {
                if !self.can_send_configuration_packet() {
                    warn!(
                        "Dropping payload on {channel} for client {}, it is not configuring",
                        self.id
                    );
                    return true;
                }
                debug!(
                    "Sending {} bytes on configuration channel {channel} to client {}",
                    data.len(),
                    self.id
                );
                self.send_packet_now(&CPluginMessage::new(&channel, &data))
                    .await;
            }
            ConnectionCommand::SendPacket { packet_id, payload } => {
                if !self.can_send_configuration_packet() {
                    warn!(
                        "Dropping packet {packet_id} for client {}, it is not configuring",
                        self.id
                    );
                    return true;
                }
                debug!(
                    "Sending configuration packet {packet_id} ({} bytes) to client {}",
                    payload.len(),
                    self.id
                );
                self.send_raw_packet_now(packet_id, &payload).await;
            }
            ConnectionCommand::SendLoginQuery {
                query_id,
                channel,
                data,
            } => {
                if self.connection_state.load() != ConnectionState::Login {
                    warn!(
                        "Dropping login query {query_id} for client {}, it is not logging in",
                        self.id
                    );
                    if let Some(session) = &self.session {
                        session.shared.queries().remove(&query_id);
                    }
                    return true;
                }
                debug!(
                    "Sending login query {query_id} on {channel} to client {}",
                    self.id
                );
                self.send_packet_now(&CLoginPluginRequest::new(VarInt(query_id), &channel, &data))
                    .await;
            }
            ConnectionCommand::Release => return self.release_hold().await,
            ConnectionCommand::Disconnect(reason) => {
                info!("Disconnecting client {} for a plugin: {reason}", self.id);
                self.kick(TextComponent::text(reason)).await;
                return false;
            }
        }
        true
    }

    /// Continues from the hold. `false` when the connection is done.
    async fn release_hold(&mut self) -> bool {
        let Some(stage) = self
            .session
            .as_mut()
            .and_then(|session| session.hold.release())
        else {
            debug!("Client {} is not held, ignoring release", self.id);
            return true;
        };
        info!("Releasing {stage} of client {}", self.id);
        match stage {
            HoldStage::LoginStart => {
                if self
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.shared.queries().is_empty())
                {
                    warn!(
                        "Client {} still has unanswered login queries at login release",
                        self.id
                    );
                }
                self.send_login_success().await;
            }
            HoldStage::ConfigurationPreBrand => {
                let Some(server) = self.server.upgrade() else {
                    self.close();
                    return false;
                };
                self.send_brand_and_continue_configuration(&server).await;
            }
            HoldStage::ConfigurationStart => {
                let Some(server) = self.server.upgrade() else {
                    self.close();
                    return false;
                };
                self.continue_configuration(&server).await;
            }
            HoldStage::ConfigurationFinish => self.send_finish_configuration().await,
        }
        true
    }

    /// Kicks a client whose hold ran out.
    pub(super) async fn kick_hold_expired(&mut self) {
        let stage = self
            .session
            .as_ref()
            .and_then(|session| session.hold.stage());
        info!(
            "Hold of client {} ({stage:?}) timed out, disconnecting",
            self.id
        );
        self.kick(TextComponent::text(
            "Timed out while waiting for the server to continue the connection",
        ))
        .await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Weak;

    use super::*;
    use crate::net::{GameProfile, PacketRateLimiter};
    use arc_swap::ArcSwap;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    #[test]
    fn hold_is_released_once_with_its_stage() {
        let mut hold = ConnectionHold::default();
        assert!(!hold.is_held());
        assert_eq!(hold.release(), None, "nothing to release");

        hold.hold(
            HoldStage::ConfigurationStart,
            Instant::now(),
            Duration::from_secs(30),
        );
        assert!(hold.is_held());
        assert_eq!(hold.stage(), Some(HoldStage::ConfigurationStart));
        assert_eq!(hold.release(), Some(HoldStage::ConfigurationStart));
        assert!(!hold.is_held());
        assert_eq!(hold.release(), None, "a second release is a no-op");
    }

    #[test]
    fn a_connection_can_be_held_at_several_stages_in_turn() {
        let now = Instant::now();
        let mut hold = ConnectionHold::default();
        hold.hold(HoldStage::LoginStart, now, Duration::from_secs(5));
        assert_eq!(hold.release(), Some(HoldStage::LoginStart));
        hold.hold(HoldStage::ConfigurationStart, now, Duration::from_secs(10));
        assert_eq!(hold.release(), Some(HoldStage::ConfigurationStart));
        hold.hold(HoldStage::ConfigurationFinish, now, Duration::from_secs(20));
        assert_eq!(hold.stage(), Some(HoldStage::ConfigurationFinish));
        assert_eq!(hold.deadline(), Some(now + Duration::from_secs(20)));
    }

    #[test]
    fn the_pre_brand_hold_comes_before_the_start_hold() {
        let now = Instant::now();
        let mut hold = ConnectionHold::default();
        hold.hold(
            HoldStage::ConfigurationPreBrand,
            now,
            Duration::from_secs(5),
        );
        assert_eq!(hold.stage(), Some(HoldStage::ConfigurationPreBrand));
        assert_eq!(hold.release(), Some(HoldStage::ConfigurationPreBrand));
        hold.hold(HoldStage::ConfigurationStart, now, Duration::from_secs(10));
        assert_eq!(hold.release(), Some(HoldStage::ConfigurationStart));
    }

    #[test]
    fn flavour_can_be_chosen_until_the_connection_is_done() {
        let (session, handle) = ConnectionSession::new(ConnectionPhase::Configuration);
        assert_eq!(session.shared.flavour.load(), ConnectionFlavour::Vanilla);

        assert!(handle.set_flavour(ConnectionFlavour::NeoForge).is_ok());
        assert_eq!(session.shared.flavour.load(), ConnectionFlavour::NeoForge);

        session.shared.phase.store(ConnectionPhase::Done);
        assert!(handle.set_flavour(ConnectionFlavour::Vanilla).is_err());
        assert_eq!(session.shared.flavour.load(), ConnectionFlavour::NeoForge);
    }

    #[test]
    fn hold_expires_at_its_deadline() {
        let now = Instant::now();
        let mut hold = ConnectionHold::default();
        assert!(!hold.is_expired(now), "no hold never expires");

        hold.hold(HoldStage::LoginStart, now, Duration::from_secs(5));
        assert!(!hold.is_expired(now + Duration::from_secs(4)));
        assert!(hold.is_expired(now + Duration::from_secs(5)));

        hold.release();
        assert!(!hold.is_expired(now + Duration::from_mins(1)));
    }

    #[test]
    fn huge_hold_timeout_does_not_overflow() {
        let now = Instant::now();
        let mut hold = ConnectionHold::default();
        hold.hold(HoldStage::LoginStart, now, Duration::MAX);
        assert!(hold.is_held());
        assert!(!hold.is_expired(now + Duration::from_hours(1)));
    }

    #[test]
    fn channel_validation() {
        for valid in [
            "minecraft:register",
            "fml:handshake",
            "mymod:net/v1",
            "channel",
            "a-b_c.d:e",
        ] {
            assert!(is_valid_channel(valid), "{valid}");
        }
        for invalid in ["", ":", "a:", ":b", "A:b", "a:b:c", "a b:c", "a/b:c"] {
            assert!(!is_valid_channel(invalid), "{invalid}");
        }
    }

    #[test]
    fn commands_check_their_arguments() {
        assert!(ConnectionCommand::send_payload("a:b".into(), vec![0; 16]).is_ok());
        assert!(
            ConnectionCommand::send_payload("a:b".into(), vec![0; MAX_CUSTOM_PAYLOAD_SIZE])
                .is_err()
        );
        assert!(ConnectionCommand::send_payload("Bad".into(), vec![]).is_err());

        assert!(ConnectionCommand::send_packet(0x05, vec![1, 2, 3]).is_ok());
        assert!(ConnectionCommand::send_packet(-1, vec![]).is_err());
        assert!(ConnectionCommand::send_packet(1, vec![0; MAX_PACKET_SIZE as usize]).is_err());
    }

    #[test]
    fn handle_checks_the_phase_and_the_queue() {
        let (session, handle) = ConnectionSession::new(ConnectionPhase::Configuration);
        assert!(
            handle
                .send(ConnectionPhase::Login, ConnectionCommand::Release)
                .is_err(),
            "wrong phase"
        );
        for _ in 0..COMMAND_QUEUE_CAPACITY {
            assert!(
                handle
                    .send(ConnectionPhase::Configuration, ConnectionCommand::Release)
                    .is_ok()
            );
        }
        assert_eq!(
            handle.send(ConnectionPhase::Configuration, ConnectionCommand::Release),
            Err("too many pending commands".to_string())
        );

        drop(session);
        assert_eq!(
            handle.send(ConnectionPhase::Configuration, ConnectionCommand::Release),
            Err("the connection is closed".to_string())
        );
    }

    #[test]
    fn login_query_ids_are_reserved_until_answered() {
        let (session, handle) = ConnectionSession::new(ConnectionPhase::Login);
        assert!(handle.send_login_query(7, "a:b".into(), vec![1]).is_ok());
        assert_eq!(
            handle.send_login_query(7, "a:b".into(), vec![1]),
            Err("login query id 7 is already in use".to_string())
        );
        assert!(handle.send_login_query(8, "a:b".into(), vec![]).is_ok());

        assert!(session.shared.queries().remove(&7), "answered");
        assert!(handle.send_login_query(7, "a:b".into(), vec![]).is_ok());

        drop(session);
        assert!(
            handle.send_login_query(9, "a:b".into(), vec![]).is_err(),
            "closed connection"
        );
        assert!(
            !handle.shared.queries().contains(&9),
            "a failed send gives the id back"
        );

        handle.shared.phase.store(ConnectionPhase::Configuration);
        assert!(handle.send_login_query(10, "a:b".into(), vec![]).is_err());
    }

    /// A connection in `state` and the client's end of its socket.
    async fn connection(
        state: ConnectionState,
        phase: ConnectionPhase,
    ) -> (PendingConnection, TcpStream, ConnectionHandle) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let (client, accepted) = tokio::join!(TcpStream::connect(address), listener.accept());
        let (server_side, client_address) = accepted.expect("accept");
        let mut pending = PendingConnection::new(
            server_side,
            client_address,
            7,
            PacketRateLimiter::new(false, 0.0, 0.0),
            Weak::new(),
        );
        pending.connection_state.store(state);
        pending.gameprofile = Some(GameProfile {
            id: Uuid::nil(),
            name: "tester".to_string(),
            properties: ArcSwap::new(Arc::new(vec![])),
            profile_actions: None,
        });
        let (session, handle) = ConnectionSession::new(phase);
        pending.session = Some(session);
        (pending, client.expect("connect"), handle)
    }

    async fn configuring() -> (PendingConnection, TcpStream, ConnectionHandle) {
        connection(ConnectionState::Config, ConnectionPhase::Configuration).await
    }

    /// Reads one uncompressed frame, returning packet id and body.
    async fn read_frame(client: &mut TcpStream) -> (u8, Vec<u8>) {
        let len = client.read_u8().await.expect("frame length");
        let mut body = vec![0; usize::from(len)];
        client.read_exact(&mut body).await.expect("frame body");
        (body[0], body[1..].to_vec())
    }

    fn hold_connection(pending: &mut PendingConnection, stage: HoldStage) {
        pending.session.as_mut().expect("session").hold.hold(
            stage,
            Instant::now(),
            Duration::from_mins(5),
        );
    }

    #[tokio::test]
    async fn payload_command_is_sent_without_losing_a_half_read_packet() {
        let (mut pending, mut client, handle) = configuring().await;
        let reading = tokio::spawn(async move {
            let packet = pending.get_packet().await;
            (pending, packet)
        });

        // Length 6 (id and "hello"), the rest arrives after the plugin's command.
        client.write_all(&[6, 2, b'h']).await.expect("write");
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle
            .send(
                ConnectionPhase::Configuration,
                ConnectionCommand::send_payload("a:b".into(), vec![9, 9]).expect("valid payload"),
            )
            .expect("queued");

        let (_, body) = read_frame(&mut client).await;
        assert_eq!(body, [3, b'a', b':', b'b', 9, 9]);

        client.write_all(b"ello").await.expect("write");
        let (_, packet) = reading.await.expect("task");
        let packet = packet.expect("the packet survives the command");
        assert_eq!(packet.id, 2);
        assert_eq!(&packet.payload[..], b"hello");
    }

    #[tokio::test]
    async fn raw_packet_command_is_sent_as_is() {
        let (mut pending, mut client, handle) = configuring().await;
        handle
            .send(
                ConnectionPhase::Configuration,
                ConnectionCommand::send_packet(0x2a, vec![1, 2, 3]).expect("valid"),
            )
            .expect("queued");
        let reading = tokio::spawn(async move { pending.get_packet().await });

        let (id, body) = read_frame(&mut client).await;
        assert_eq!((id, body), (0x2a, vec![1, 2, 3]));
        drop(client);
        assert!(reading.await.expect("task").is_none());
    }

    #[tokio::test]
    async fn login_query_command_is_sent_in_the_login_state() {
        let (mut pending, mut client, handle) =
            connection(ConnectionState::Login, ConnectionPhase::Login).await;
        handle
            .send_login_query(300, "a:b".into(), vec![9])
            .expect("queued");
        let reading = tokio::spawn(async move { pending.get_packet().await });

        let (_, body) = read_frame(&mut client).await;
        // Query id 300 as a var int, the channel, then the data.
        assert_eq!(body, [0xac, 0x02, 3, b'a', b':', b'b', 9]);
        drop(client);
        assert!(reading.await.expect("task").is_none());
    }

    #[tokio::test]
    async fn disconnect_command_kicks_and_ends_the_connection() {
        let (mut pending, mut client, handle) = configuring().await;
        handle
            .send(
                ConnectionPhase::Configuration,
                ConnectionCommand::Disconnect("bye".into()),
            )
            .expect("queued");

        assert!(pending.get_packet().await.is_none());
        assert!(pending.is_closed());
        let (_, body) = read_frame(&mut client).await;
        assert!(body.ends_with(b"bye"));
    }

    #[tokio::test]
    async fn releasing_the_finish_hold_sends_finish_configuration() {
        let (mut pending, mut client, handle) = configuring().await;
        hold_connection(&mut pending, HoldStage::ConfigurationFinish);
        handle
            .send(ConnectionPhase::Configuration, ConnectionCommand::Release)
            .expect("queued");
        let reading = tokio::spawn(async move {
            let packet = pending.get_packet().await;
            (pending, packet)
        });

        let (_, body) = read_frame(&mut client).await;
        assert!(body.is_empty(), "finish configuration has no body");
        drop(client);
        let (pending, packet) = reading.await.expect("task");
        assert!(packet.is_none());
        assert!(!pending.is_held());
        assert!(pending.session.expect("session").finish_sent);
    }

    #[tokio::test]
    async fn releasing_the_pre_brand_hold_without_a_server_closes_the_connection() {
        // The brand needs the server, which this connection has none of.
        let (mut pending, _client, handle) = configuring().await;
        hold_connection(&mut pending, HoldStage::ConfigurationPreBrand);
        handle
            .send(ConnectionPhase::Configuration, ConnectionCommand::Release)
            .expect("queued");

        assert!(pending.get_packet().await.is_none());
        assert!(pending.is_closed());
        assert!(!pending.is_held());
    }

    #[tokio::test]
    async fn releasing_the_login_hold_sends_login_success() {
        let (mut pending, mut client, handle) =
            connection(ConnectionState::Login, ConnectionPhase::Login).await;
        hold_connection(&mut pending, HoldStage::LoginStart);
        handle
            .send(ConnectionPhase::Login, ConnectionCommand::Release)
            .expect("queued");
        let reading = tokio::spawn(async move { pending.get_packet().await });

        let (_, body) = read_frame(&mut client).await;
        assert_eq!(&body[..16], Uuid::nil().as_bytes(), "login success");
        drop(client);
        assert!(reading.await.expect("task").is_none());
    }

    #[tokio::test]
    async fn release_without_hold_is_ignored() {
        let (mut pending, mut client, handle) = configuring().await;
        handle
            .send(ConnectionPhase::Configuration, ConnectionCommand::Release)
            .expect("queued");
        let reading = tokio::spawn(async move { pending.get_packet().await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        client.write_all(&[1, 9]).await.expect("write");
        let packet = reading.await.expect("task").expect("packet");
        assert_eq!(packet.id, 9);
    }

    #[tokio::test(start_paused = true)]
    async fn held_connection_outlives_the_idle_timeout_but_not_the_hold() {
        let (mut pending, mut client, _handle) = configuring().await;
        let started = tokio::time::Instant::now();
        let hold_for = super::super::pending::HANDSHAKE_IDLE_TIMEOUT * 3;
        pending.session.as_mut().expect("session").hold.hold(
            HoldStage::ConfigurationFinish,
            Instant::now(),
            hold_for,
        );

        assert!(pending.get_packet().await.is_none());
        assert!(started.elapsed() >= hold_for, "dropped by the idle timer");
        assert!(pending.is_closed());
        let (_, body) = read_frame(&mut client).await;
        assert!(
            String::from_utf8_lossy(&body).contains("Timed out"),
            "the client is told why"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn silent_connection_without_hold_is_dropped_by_the_idle_timeout() {
        let (mut pending, _client, _handle) = configuring().await;
        let started = tokio::time::Instant::now();

        assert!(pending.get_packet().await.is_none());
        assert_eq!(
            started.elapsed(),
            super::super::pending::HANDSHAKE_IDLE_TIMEOUT
        );
    }
}
