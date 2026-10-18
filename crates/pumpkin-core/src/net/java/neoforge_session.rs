//! The connection driven part of the `NeoForge` handshake, see [`crate::net::neoforge`].
//!
//! It is a state machine driven by the packets of the connection and by one deadline, because
//! the packet reader of a connection is not cancellation safe and must keep running while the
//! handshake waits. The configuration code calls it at four points:
//!
//! * [`PendingConnection::neoforge_begin`] right after the login was acknowledged, before the
//!   brand. The connection waits until the client answered the query (or did not),
//! * [`PendingConnection::neoforge_after_brand`] right after the brand, starts the registry sync,
//! * [`PendingConnection::neoforge_begin_finish`] after the registry data and tags,
//! * [`PendingConnection::neoforge_payload`], [`PendingConnection::neoforge_pong`] and
//!   [`PendingConnection::neoforge_timeout`] for what the client sends and the deadline.
//!
//! Whenever a wait ends the configuration continues where it stopped.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use pumpkin_protocol::{
    ConnectionFlavour,
    java::client::config::{CConfigPing, CPluginMessage},
};
use pumpkin_util::text::TextComponent;
use tracing::{debug, info};

use crate::net::neoforge::{
    channels::{self, COMMON_NETWORK_VERSION},
    declaration::{self, Aggregate},
    handshake::{
        self, BAD_REPLY, Decision, PRE_BRAND_GRACE_MS, PRE_BRAND_TIMEOUT_MS, REGISTRY_SYNC_TIMEOUT,
        REGISTRY_SYNC_TIMEOUT_MS, TASK_TIMEOUT, TASK_TIMEOUT_MS, UNSUPPORTED_COMMON_VERSION,
    },
    payloads::{
        ModdedNetworkQuery, Phase, decode_common_register, decode_common_version,
        decode_known_data_maps_reply, decode_register_channels, encode_common_register,
        encode_common_version, encode_config_file, encode_register_channels,
    },
};

use super::pending::PendingConnection;

/// A step of the tasks after the registry data and tags that waits for the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FinishStep {
    CommonVersion,
    CommonRegister,
    DataMaps,
    FeatureFlags,
}

impl FinishStep {
    const fn name(self) -> &'static str {
        match self {
            Self::CommonVersion => "c:version",
            Self::CommonRegister => "c:register",
            Self::DataMaps => "data map negotiation",
            Self::FeatureFlags => "feature flag check",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// The query and ping are out, the client has not decided yet.
    PreBrand,
    /// The client is a `NeoForge` client and nothing is pending.
    Negotiated,
    /// The registries are out, waiting for the acknowledgement.
    AwaitingRegistries,
    /// Waiting for the reply of a step after the registry data.
    Finishing(FinishStep),
    /// Nothing more to do: the client is not `NeoForge` (and let through), or it completed.
    Done,
}

/// The handshake state of one connection.
pub struct NeoForgeState {
    aggregate: Arc<Aggregate>,
    stage: Stage,
    deadline: Option<Instant>,
    /// What the client can receive (its `minecraft:register`).
    announced: Vec<String>,
    query: Option<ModdedNetworkQuery>,
    pong: bool,
    /// The client answered the query and was accepted.
    neoforge: bool,
}

impl NeoForgeState {
    fn new(aggregate: Arc<Aggregate>) -> Self {
        Self {
            aggregate,
            stage: Stage::PreBrand,
            deadline: Some(Instant::now() + Duration::from_millis(PRE_BRAND_TIMEOUT_MS)),
            announced: Vec::new(),
            query: None,
            pong: false,
            neoforge: false,
        }
    }

    fn announced(&self, channel: &str) -> bool {
        self.announced.iter().any(|announced| announced == channel)
    }
}

impl PendingConnection {
    /// The deadline of the handshake, if it waits for something.
    pub(super) fn neoforge_deadline(&self) -> Option<Instant> {
        self.neoforge.as_ref().and_then(|state| state.deadline)
    }

    /// Whether the client completed the handshake as a `NeoForge` client.
    #[must_use]
    pub fn is_neoforge(&self) -> bool {
        self.neoforge.as_ref().is_some_and(|state| state.neoforge)
    }

    async fn neoforge_send(&mut self, channel: &str, data: &[u8]) {
        self.send_packet_now(&CPluginMessage::new(channel, data))
            .await;
    }

    async fn neoforge_kick(&mut self, message: String) {
        self.neoforge_log(&format!("refused: {message}"));
        if let Some(state) = &mut self.neoforge {
            state.deadline = None;
            state.stage = Stage::Done;
        }
        self.kick(TextComponent::text(message)).await;
    }

    fn neoforge_log(&self, message: &str) {
        let name = self
            .gameprofile
            .as_ref()
            .map_or("<unknown>", |profile| profile.name.as_str());
        info!("NeoForge handshake of {name} ({}): {message}", self.id);
    }

    /// Opens the handshake before the brand. `true` when it waits for the client, the brand must
    /// not be sent before [`Self::neoforge_pre_brand_done`] ran. `false` when no plugin declared
    /// anything.
    pub(crate) async fn neoforge_begin(&mut self) -> bool {
        let Some(aggregate) = declaration::current() else {
            return false;
        };
        // What a NeoForge server opens with: reset the registrations, announce what the server
        // receives, ask what the client registered (an empty query), and ping.
        self.neoforge_send(
            channels::MINECRAFT_UNREGISTER,
            &encode_register_channels(&[
                channels::MINECRAFT_REGISTER,
                channels::MINECRAFT_UNREGISTER,
            ]),
        )
        .await;
        let listening: Vec<&str> = channels::SERVER_CONFIGURATION_LISTENING
            .iter()
            .copied()
            .chain(
                aggregate
                    .configuration_channels_to_server
                    .iter()
                    .map(String::as_str),
            )
            .collect();
        self.neoforge_send(
            channels::MINECRAFT_REGISTER,
            &encode_register_channels(&listening),
        )
        .await;
        self.neoforge_send(
            channels::MODDED_NETWORK_QUERY,
            &ModdedNetworkQuery::default().encode(),
        )
        .await;
        self.send_packet_now(&CConfigPing::new(0)).await;
        self.neoforge = Some(NeoForgeState::new(aggregate));
        true
    }

    /// Chooses the encoding of the play phase for this connection.
    fn neoforge_mark_flavour(&mut self) {
        self.set_native_flavour(ConnectionFlavour::NeoForge);
    }

    /// The client answered, or the time is up: decides what the client is.
    async fn neoforge_resolve(&mut self, timed_out: bool) {
        let Some(state) = &mut self.neoforge else {
            return;
        };
        if state.stage != Stage::PreBrand {
            return;
        }
        let answered = state.query.is_some();
        if answered && (state.pong || timed_out) {
            self.neoforge_accept_or_reject().await;
        } else if timed_out {
            self.neoforge_not_neoforge().await;
        } else if state.pong {
            // The pong came without an answer: a vanilla client, but give a slow one a moment.
            let grace = Instant::now() + Duration::from_millis(PRE_BRAND_GRACE_MS);
            state.deadline = Some(state.deadline.map_or(grace, |deadline| deadline.min(grace)));
        }
    }

    async fn neoforge_not_neoforge(&mut self) {
        let Some(state) = &mut self.neoforge else {
            return;
        };
        let aggregate = Arc::clone(&state.aggregate);
        state.deadline = None;
        if aggregate.allow_non_neoforge {
            state.stage = Stage::Done;
            let brand = self.brand.clone().unwrap_or_else(|| "unknown".to_string());
            self.neoforge_log(&format!(
                "not a NeoForge client (brand {brand}), letting it join"
            ));
            self.neoforge_pre_brand_done().await;
        } else {
            let brand = self.brand.clone().unwrap_or_else(|| "unknown".to_string());
            self.neoforge_log(&format!("not a NeoForge client (brand {brand})"));
            let message = aggregate.text(&aggregate.reject_message, "");
            self.neoforge_kick(message).await;
        }
    }

    async fn neoforge_accept_or_reject(&mut self) {
        let Some(state) = &mut self.neoforge else {
            return;
        };
        let aggregate = Arc::clone(&state.aggregate);
        let query = state.query.take().unwrap_or_default();
        let announced = state.announced.clone();
        state.deadline = None;
        match handshake::decide(&aggregate, &query, &announced) {
            Decision::Reject(message) => self.neoforge_kick(message).await,
            Decision::SetupFailed { payload, message } => {
                // Let the client show its mismatch screen, then end the connection.
                self.neoforge_send(channels::MODDED_NETWORK_SETUP_FAILED, &payload)
                    .await;
                self.neoforge_kick(message).await;
            }
            Decision::Accept(setup) => {
                // The client treats the connection as NeoForge from now on, so the play phase
                // has to be written in the NeoForge encodings.
                self.neoforge_mark_flavour();
                self.neoforge_send(channels::MODDED_NETWORK, &setup.encode())
                    .await;
                let register: Vec<String> = channels::BUILTIN
                    .iter()
                    .map(ToString::to_string)
                    .chain(setup.configuration_channels())
                    .collect();
                self.neoforge_send(
                    channels::MINECRAFT_REGISTER,
                    &encode_register_channels(&register),
                )
                .await;
                if let Some(state) = &mut self.neoforge {
                    state.stage = Stage::Negotiated;
                    state.neoforge = true;
                }
                self.neoforge_log("negotiated, a NeoForge client");
                self.neoforge_pre_brand_done().await;
            }
        }
    }

    /// The pre-brand wait is over: the rest of the pre-brand configuration, then the brand.
    async fn neoforge_pre_brand_done(&mut self) {
        let Some(server) = self.server.upgrade() else {
            self.close();
            return;
        };
        self.proceed_after_neoforge_pre_brand(&server).await;
    }

    /// Starts the registry sync after the brand. `true` when it waits for the acknowledgement.
    pub(crate) async fn neoforge_after_brand(&mut self) -> bool {
        let Some(state) = &mut self.neoforge else {
            return false;
        };
        if !state.neoforge || state.aggregate.registries.is_empty() {
            return false;
        }
        let aggregate = Arc::clone(&state.aggregate);
        state.stage = Stage::AwaitingRegistries;
        state.deadline = Some(Instant::now() + Duration::from_millis(REGISTRY_SYNC_TIMEOUT_MS));
        self.neoforge_send(
            channels::FROZEN_REGISTRY_SYNC_START,
            &aggregate.sync_start_payload,
        )
        .await;
        for payload in &aggregate.registry_payloads {
            self.neoforge_send(channels::FROZEN_REGISTRY, payload).await;
        }
        self.neoforge_send(channels::FROZEN_REGISTRY_SYNC_COMPLETED, &[])
            .await;
        self.neoforge_log(&format!(
            "sent {} registry snapshot(s)",
            aggregate.registries.len()
        ));
        true
    }

    /// Starts the tasks after the registry data and tags. `true` when it waits for the client.
    pub(crate) async fn neoforge_begin_finish(&mut self) -> bool {
        let Some(state) = &mut self.neoforge else {
            return false;
        };
        if !state.neoforge {
            return false;
        }
        state.stage = Stage::Finishing(FinishStep::CommonVersion);
        state.deadline = Some(Instant::now() + Duration::from_millis(TASK_TIMEOUT_MS));
        self.neoforge_send(channels::COMMON_VERSION, &encode_common_version())
            .await;
        true
    }

    /// The pong of the configuration ping.
    pub(crate) async fn neoforge_pong(&mut self, id: i32) {
        let Some(state) = &mut self.neoforge else {
            return;
        };
        if id == 0 && state.stage == Stage::PreBrand {
            state.pong = true;
            self.neoforge_resolve(false).await;
        }
    }

    /// The deadline passed. `false` when the connection is done.
    pub(crate) async fn neoforge_timeout(&mut self) -> bool {
        let Some(state) = &mut self.neoforge else {
            return true;
        };
        state.deadline = None;
        let aggregate = Arc::clone(&state.aggregate);
        match state.stage {
            Stage::PreBrand => self.neoforge_resolve(true).await,
            Stage::AwaitingRegistries => {
                let message = aggregate.text(REGISTRY_SYNC_TIMEOUT, "");
                self.neoforge_kick(message).await;
            }
            Stage::Finishing(step) => {
                let message = aggregate.text(TASK_TIMEOUT, step.name());
                self.neoforge_kick(message).await;
            }
            Stage::Negotiated | Stage::Done => {}
        }
        !self.is_closed()
    }

    /// A custom payload of the client. `true` when the handshake consumed it.
    pub(crate) async fn neoforge_payload(&mut self, channel: &str, data: &[u8]) -> bool {
        let Some(state) = &mut self.neoforge else {
            return false;
        };
        if state.stage == Stage::Done {
            return false;
        }
        let aggregate = Arc::clone(&state.aggregate);
        let bad = |what: &str, error: &dyn std::fmt::Display| {
            aggregate.text(BAD_REPLY, &format!("{what}: {error}"))
        };
        match (channel, state.stage) {
            (channels::MINECRAFT_REGISTER, _) => {
                for name in decode_register_channels(data) {
                    if !state.announced.contains(&name) {
                        state.announced.push(name);
                    }
                }
                debug!("client announced {} channel(s)", state.announced.len());
            }
            (channels::MINECRAFT_UNREGISTER, _) => {
                let removed = decode_register_channels(data);
                state.announced.retain(|name| !removed.contains(name));
            }
            (channels::MODDED_NETWORK_QUERY, Stage::PreBrand) => {
                match ModdedNetworkQuery::decode(data) {
                    Ok(query) => {
                        state.query = Some(query);
                        self.neoforge_resolve(false).await;
                    }
                    Err(error) => {
                        let message = bad(channels::MODDED_NETWORK_QUERY, &error);
                        self.neoforge_kick(message).await;
                    }
                }
            }
            (channels::FROZEN_REGISTRY_SYNC_COMPLETED, Stage::AwaitingRegistries) => {
                state.stage = Stage::Negotiated;
                state.deadline = None;
                self.neoforge_log("registry sync acknowledged");
                let Some(server) = self.server.upgrade() else {
                    self.close();
                    return true;
                };
                self.continue_after_neoforge_registries(&server).await;
            }
            (channels::COMMON_VERSION, Stage::Finishing(FinishStep::CommonVersion)) => {
                match decode_common_version(data) {
                    Ok(versions) if versions.contains(&COMMON_NETWORK_VERSION) => {
                        state.stage = Stage::Finishing(FinishStep::CommonRegister);
                        state.deadline =
                            Some(Instant::now() + Duration::from_millis(TASK_TIMEOUT_MS));
                        // No payload of the plugins uses the `c:` mechanism.
                        let register = encode_common_register(Phase::Play, &[]);
                        self.neoforge_send(channels::COMMON_REGISTER, &register)
                            .await;
                    }
                    Ok(_) => {
                        self.neoforge_kick(UNSUPPORTED_COMMON_VERSION.to_string())
                            .await;
                    }
                    Err(error) => {
                        let message = bad(channels::COMMON_VERSION, &error);
                        self.neoforge_kick(message).await;
                    }
                }
            }
            (channels::COMMON_REGISTER, Stage::Finishing(FinishStep::CommonRegister)) => {
                match decode_common_register(data) {
                    Ok(_) => {
                        self.neoforge_send_config_files().await;
                        self.neoforge_run_from(FinishStep::DataMaps).await;
                    }
                    Err(error) => {
                        let message = bad(channels::COMMON_REGISTER, &error);
                        self.neoforge_kick(message).await;
                    }
                }
            }
            (channels::KNOWN_REGISTRY_DATA_MAPS_REPLY, Stage::Finishing(FinishStep::DataMaps)) => {
                match decode_known_data_maps_reply(data) {
                    Ok(_) => self.neoforge_run_from(FinishStep::FeatureFlags).await,
                    Err(error) => {
                        let message = bad(channels::KNOWN_REGISTRY_DATA_MAPS_REPLY, &error);
                        self.neoforge_kick(message).await;
                    }
                }
            }
            (channels::FEATURE_FLAGS_ACK, Stage::Finishing(FinishStep::FeatureFlags)) => {
                self.neoforge_finished().await;
            }
            _ => return false,
        }
        true
    }

    /// The config files, if the client can receive them.
    async fn neoforge_send_config_files(&mut self) {
        let Some(state) = &self.neoforge else {
            return;
        };
        if !state.announced(channels::CONFIG_FILE) {
            return;
        }
        let files = state.aggregate.config_files_to_send();
        for (name, content) in files {
            self.neoforge_send(channels::CONFIG_FILE, &encode_config_file(&name, &content))
                .await;
        }
    }

    /// Sends the next step from `step` on that applies, waits for its reply, or finishes.
    async fn neoforge_run_from(&mut self, step: FinishStep) {
        let Some(state) = &mut self.neoforge else {
            return;
        };
        let aggregate = Arc::clone(&state.aggregate);
        let mut next = step;
        loop {
            let (payload, channel, applies) = match next {
                FinishStep::DataMaps => (
                    aggregate.known_data_maps_payload.as_ref(),
                    channels::KNOWN_REGISTRY_DATA_MAPS,
                    state.announced(channels::KNOWN_REGISTRY_DATA_MAPS),
                ),
                FinishStep::FeatureFlags => (
                    aggregate.feature_flags_payload.as_ref(),
                    channels::FEATURE_FLAGS,
                    state.announced(channels::FEATURE_FLAGS),
                ),
                FinishStep::CommonVersion | FinishStep::CommonRegister => break,
            };
            if let (Some(payload), true) = (payload, applies) {
                state.stage = Stage::Finishing(next);
                state.deadline = Some(Instant::now() + Duration::from_millis(TASK_TIMEOUT_MS));
                let payload = payload.clone();
                self.neoforge_send(channel, &payload).await;
                return;
            }
            if next == FinishStep::FeatureFlags {
                break;
            }
            next = FinishStep::FeatureFlags;
        }
        self.neoforge_finished().await;
    }

    async fn neoforge_finished(&mut self) {
        if let Some(state) = &mut self.neoforge {
            state.stage = Stage::Done;
            state.deadline = None;
        }
        self.neoforge_log("complete");
        let Some(server) = self.server.upgrade() else {
            self.close();
            return;
        };
        self.complete_configuration(&server).await;
    }

    /// The datapack registry entries plugins declared for `registry`, as registry data entries.
    pub(crate) fn neoforge_datapack_entries(
        &self,
        registry: &str,
    ) -> Vec<pumpkin_data::registry::RegistryEntryData> {
        let Some(state) = &self.neoforge else {
            return Vec::new();
        };
        if !state.neoforge {
            return Vec::new();
        }
        state
            .aggregate
            .datapack_entries_of(registry)
            .map(|entry| pumpkin_data::registry::RegistryEntryData {
                entry_id: entry.id.clone(),
                data: entry.data.clone().map(Vec::into_boxed_slice),
            })
            .collect()
    }

    /// The registries only plugins declared entries for, which the vanilla data does not have.
    pub(crate) fn neoforge_extra_datapack_registries(
        &self,
        known: &[String],
    ) -> Vec<(String, Vec<pumpkin_data::registry::RegistryEntryData>)> {
        let Some(state) = &self.neoforge else {
            return Vec::new();
        };
        if !state.neoforge {
            return Vec::new();
        }
        let strip = |text: &str| text.strip_prefix("minecraft:").unwrap_or(text).to_string();
        let known: Vec<String> = known.iter().map(|k| strip(k)).collect();
        let mut registries: Vec<String> = Vec::new();
        for entry in &state.aggregate.datapack_entries {
            if !known.contains(&strip(&entry.registry)) && !registries.contains(&entry.registry) {
                registries.push(entry.registry.clone());
            }
        }
        registries
            .into_iter()
            .map(|registry| {
                let entries = self.neoforge_datapack_entries(&registry);
                (registry, entries)
            })
            .collect()
    }
}
