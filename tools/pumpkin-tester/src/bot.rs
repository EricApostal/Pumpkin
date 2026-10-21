use std::{collections::BTreeMap, net::SocketAddr, sync::Arc, time::Duration};

use bytes::Bytes;
use pumpkin_core::net::neoforge::{
    buf::Reader,
    channels as neoforge_channels,
    declaration::{self, Aggregate},
    nbt_text::NbtText,
    payloads::{
        Flow, ModdedNetworkQuery, Phase, QueryComponent, decode_register_channels,
        encode_common_register, encode_common_version, encode_register_channels,
    },
};
use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_protocol::{
    ClientPacket, CompressionLevel, ConnectionState, RawPacket, ServerPacket,
    codec::var_int::VarInt,
    java::{
        client::{
            config::{CConfigDisconnect, CConfigPing, CFinishConfig, CKnownPacks, CPluginMessage},
            login::{CLoginSuccess, CSetCompression},
            play::{CLogin, CPlayerPosition},
        },
        packet_decoder::TCPNetworkDecoder,
        packet_encoder::TCPNetworkEncoder,
        server::{
            config::{
                SAcknowledgeFinishConfig, SClientInformationConfig, SConfigPong, SKnownPacks,
                SPluginMessage,
            },
            handshake::SHandShake,
            login::{SLoginAcknowledged, SLoginStart},
            play::{
                Action, SChatCommand, SConfirmTeleport, SPlayerAction, SPlayerCommand,
                SPlayerLoaded, SPlayerPosition, SSetHeldItem, SUseItemOn, Status,
            },
        },
    },
    packet::MultiVersionJavaPacket,
    ser::{NetworkReadSliceExt, NetworkWriteExt},
};
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use tokio::{
    io::{BufReader, BufWriter},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    time::timeout,
};
use uuid::Uuid;

pub const PACKET_TIMEOUT: Duration = Duration::from_secs(10);

/// The channel the server sends its brand on; the client answers with its own brand there.
///
/// `pumpkin-core` reads the payload of a client brand as raw UTF-8
/// (`crates/pumpkin-core/src/net/java/pending.rs`), so the bot writes it that way.
const BRAND_CHANNEL: &str = "minecraft:brand";
/// What the bot reports as its brand, so the server logs it as a `NeoForge` client.
const CLIENT_BRAND: &str = "neoforge";

/// The brand payload: vanilla's `BrandPayload` is a `VarInt` length and then the UTF-8 bytes.
///
/// The length has to be the *byte* length: a server that reads it with that codec reports
/// `Packet ... was larger than I expected, found 1 bytes extra` when it is one too small, which is
/// what a bare `neoforge` with the wrong count turns into. Building the payload from the name
/// keeps the two from drifting.
fn client_brand_payload() -> Vec<u8> {
    let mut payload = Vec::with_capacity(CLIENT_BRAND.len() + 1);
    payload.push(u8::try_from(CLIENT_BRAND.len()).unwrap_or(0));
    payload.extend_from_slice(CLIENT_BRAND.as_bytes());
    payload
}

/// The ping id of the `NeoForge` query handshake (`neoforge_session.rs` pings with `0`).
const NEOFORGE_PING_ID: i32 = 0;
/// How many connections a mirroring bot may spend learning a server's channels before it gives up.
///
/// A refusal reveals one class of mismatch at a time, so a server with a mod set this process
/// cannot read takes a handful of them: the missing channels, then their flows, then their
/// versions.
pub const MIRRORING_ATTEMPTS: u32 = 6;
/// The most channels one refusal may name.
const MAX_MIRRORED_CHANNELS: usize = 4096;
/// What this client compresses its own frames with once a server turned compression on. The level
/// is the client's choice; the server only has to be able to decode the frames.
const CLIENT_COMPRESSION_LEVEL: CompressionLevel = 6;

/// What the `NeoForge` configuration handshake of a bot saw, so a test can assert on it without
/// decoding the stream itself. Filled by
/// [`HeadlessBot::connect_and_join_neoforge`].
#[derive(Clone, Debug, Default)]
pub struct NeoforgeHandshake {
    /// Every channel the server sent in the configuration phase, in order (its brand, its
    /// `minecraft:*` announcements, the registry sync, `c:version`/`c:register`, the config
    /// files).
    pub received_channels: Vec<String>,
    /// The raw `neoforge:frozen_registry` payloads, in order: the registry name, then its
    /// snapshot (`encode_frozen_registry`).
    pub frozen_registries: Vec<Vec<u8>>,
    /// Whether the bot answered `neoforge:frozen_registry_sync_completed`, which is what the
    /// server waits for before it continues the configuration.
    pub registry_sync_acknowledged: bool,
}

pub struct HeadlessBot {
    pub name: String,
    pub reader: TCPNetworkDecoder<BufReader<OwnedReadHalf>>,
    pub writer: TCPNetworkEncoder<BufWriter<OwnedWriteHalf>>,
    pub position: Vector3<f64>,
    pub yaw: f32,
    pub pitch: f32,
    /// Whether the server negotiated this connection as a `NeoForge` client, that is whether it
    /// sent `neoforge:network`: from then on the play phase is read in the `NeoForge` encodings.
    pub neoforge: bool,
    /// The `NeoForge` handshake this connection ran, if any.
    pub neoforge_handshake: NeoforgeHandshake,
}

impl HeadlessBot {
    pub async fn send_packet<P: ClientPacket + MultiVersionJavaPacket>(
        &mut self,
        packet: &P,
    ) -> Result<(), String> {
        send_framed_packet(&mut self.writer, packet).await
    }

    pub async fn next_packet(&mut self) -> Result<RawPacket, String> {
        match timeout(PACKET_TIMEOUT, self.reader.get_raw_packet()).await {
            Ok(Ok(packet)) => Ok(packet),
            Ok(Err(err)) => Err(format!("Bot '{}' packet decode error: {err}", self.name)),
            Err(_) => Err(format!(
                "Bot '{}' timed out waiting for packet from server",
                self.name
            )),
        }
    }

    /// Connects, logs in and acknowledges the login, leaving the connection in the configuration
    /// phase. The `NeoForge` handshake the server may open with is answered by the caller.
    async fn connect_and_login(addr: SocketAddr, name: &str) -> Result<Self, String> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|err| format!("Bot '{name}' failed to connect to {addr}: {err}"))?;
        let _ = stream.set_nodelay(true);
        let (read_half, write_half) = stream.into_split();
        let mut bot = Self {
            name: name.to_string(),
            reader: TCPNetworkDecoder::new(BufReader::new(read_half)),
            writer: TCPNetworkEncoder::new(BufWriter::new(write_half)),
            position: Vector3::new(0.0, 0.0, 0.0),
            yaw: 0.0,
            pitch: 0.0,
            neoforge: false,
            neoforge_handshake: NeoforgeHandshake::default(),
        };

        // 1. Handshake -> Login
        bot.send_packet(&SHandShake {
            protocol_version: VarInt(CURRENT_MC_VERSION.protocol_version()),
            server_address: "127.0.0.1".into(),
            server_port: addr.port(),
            next_state: ConnectionState::Login,
        })
        .await?;

        bot.send_packet(&SLoginStart {
            name: name.into(),
            uuid: Uuid::new_v4(),
        })
        .await?;

        let login_success_id = CLoginSuccess::to_id(CURRENT_MC_VERSION);
        let compression_id = CSetCompression::to_id(CURRENT_MC_VERSION);
        loop {
            let pkt = bot.next_packet().await?;
            if pkt.id == compression_id {
                // A server turns compression on before it logs the client in (a vanilla server
                // does), and from then on **both** directions use the smaller framing: a client
                // that keeps writing plain frames would be unreadable to the server, and one that
                // keeps reading them would never see the login success.
                let mut slice = &pkt.payload[..];
                let compression = CSetCompression::read(&mut slice, &CURRENT_MC_VERSION)
                    .map_err(|err| format!("Failed to read SET_COMPRESSION: {err}"))?;
                bot.set_compression(compression.threshold.0);
                continue;
            }
            if pkt.id == login_success_id {
                break;
            }
        }

        bot.send_packet(&SLoginAcknowledged).await?;
        Ok(bot)
    }

    /// Applies the compression the server asked for, in both directions.
    ///
    /// The level is the client's own choice; the server only has to be able to decode the frames.
    /// A negative threshold means "no compression", which is what a server with compression off
    /// sends (`CSetCompression`).
    fn set_compression(&mut self, threshold: i32) {
        if let Ok(threshold) = usize::try_from(threshold) {
            self.reader.set_compression(threshold);
            self.writer.set_compression((threshold, CLIENT_COMPRESSION_LEVEL));
        }
    }

    /// The client information the server expects at the start of the configuration phase.
    async fn send_client_information(&mut self) -> Result<(), String> {
        self.send_packet(&SClientInformationConfig {
            locale: "en_us",
            view_distance: 4,
            chat_mode: VarInt(0),
            chat_colors: true,
            skin_parts: 0x7F,
            main_hand: VarInt(1),
            text_filtering: false,
            server_listing: true,
            // What a `NeoForge` server of 26.3 reads (`ALL`); a plain vanilla server ignores it.
            particle_status: VarInt(0),
        })
        .await
    }

    /// The configuration of a client without `NeoForge`: the known packs and the finish.
    async fn finish_configuration(&mut self) -> Result<(), String> {
        let known_packs_id = CKnownPacks::to_id(CURRENT_MC_VERSION);
        let finish_config_id = CFinishConfig::to_id(CURRENT_MC_VERSION);
        loop {
            let pkt = self.next_packet().await?;
            if pkt.id == known_packs_id {
                self.send_packet(&SKnownPacks {
                    known_packs: Vec::new(),
                })
                .await?;
            } else if pkt.id == finish_config_id {
                self.send_packet(&SAcknowledgeFinishConfig).await?;
                break;
            }
        }
        Ok(())
    }

    /// The play phase: wait for `CLogin` and the initial position, then confirm the teleport.
    async fn await_play_start(&mut self) -> Result<(), String> {
        let play_login_id = CLogin::to_id(CURRENT_MC_VERSION);
        let player_pos_id = CPlayerPosition::to_id(CURRENT_MC_VERSION);
        let mut saw_play_login = false;
        let mut saw_initial_pos = false;

        while !(saw_play_login && saw_initial_pos) {
            let pkt = self.next_packet().await?;
            if pkt.id == play_login_id {
                saw_play_login = true;
            } else if pkt.id == player_pos_id {
                let mut slice = &pkt.payload[..];
                let pos_pkt = CPlayerPosition::read(&mut slice, &CURRENT_MC_VERSION)
                    .map_err(|err| format!("Failed to read CPlayerPosition: {err}"))?;
                self.position = pos_pkt.position;
                self.yaw = pos_pkt.yaw;
                self.pitch = pos_pkt.pitch;

                self.send_packet(&SConfirmTeleport {
                    teleport_id: pos_pkt.teleport_id,
                    position: pos_pkt.position,
                    yaw: pos_pkt.yaw,
                    pitch: pos_pkt.pitch,
                })
                .await?;
                self.send_packet(&SPlayerLoaded).await?;
                saw_initial_pos = true;
            }
        }
        Ok(())
    }

    pub async fn connect_and_join(addr: SocketAddr, name: &str) -> Result<Self, String> {
        let mut bot = Self::connect_and_login(addr, name).await?;
        bot.send_client_information().await?;
        bot.finish_configuration().await?;
        bot.await_play_start().await?;
        Ok(bot)
    }

    /// Connects and joins like [`Self::connect_and_join`], and answers the `NeoForge` handshake
    /// the server opens with, so it marks the connection `NeoForge` and sends the modded play
    /// payloads (`neoforge:advanced_add_entity` on a plugin entity spawn, see
    /// `crates/pumpkin-core/src/entity/plugin_entity.rs`).
    ///
    /// The server opens the configuration with `minecraft:unregister`, `minecraft:register`, the
    /// empty `neoforge:register` query and a ping (`crates/pumpkin-core/src/net/java/neoforge_session.rs`).
    /// The connection has to answer with what it registered before the query answer is decided on:
    ///
    /// 1. `minecraft:register`, what this client can receive — without `REGISTRY_SYNC_REQUIRED`
    ///    (`neoforge:frozen_registry_sync_start`, `neoforge:frozen_registry`,
    ///    `neoforge:frozen_registry_sync_completed`) the server refuses a plugin that declared
    ///    registries with `NO_REGISTRY_SYNC`,
    /// 2. `neoforge:register`, the query answer: `(VAR_INT protocol count) { VAR_INT protocol,
    ///    VAR_INT component count, { STRING id, STRING version, BOOL hasFlow, [VAR_INT flow],
    ///    BOOL optional } }`. A channel both sides have must agree on the flow and the version,
    ///    and a non-optional one only one side has fails the negotiation, so the answer carries
    ///    the server's own registrations when the bot can see its declaration,
    /// 3. a pong (`SConfigPong`, `INT id`, big endian) for the ping. The server accepts when it
    ///    has both the answer and the pong, then sends `neoforge:network` and marks the
    ///    connection `NeoForge`,
    /// 4. the brand (`minecraft:brand`, the raw UTF-8 the host reads) when the server's brand
    ///    arrives, then the registry sync: the server sends
    ///    `neoforge:frozen_registry_sync_start`, one `neoforge:frozen_registry` per registry and
    ///    `neoforge:frozen_registry_sync_completed`, and waits for the client's
    ///    `neoforge:frozen_registry_sync_completed` (empty payload) before it continues,
    /// 5. after the registry data and tags: `c:version` (`VAR_INT count, VAR_INT version`), the
    ///    answer is the client's versions (`[1]`); then `c:register` (`VAR_INT version,
    ///    STRING phase id, VAR_INT count, { STRING channel }`), the answer is the client's own
    ///    registration.
    ///
    /// Announcing `neoforge:known_registry_data_maps` and `neoforge:feature_flags` asks the
    /// server to run those steps too; the bot answers the first with an empty map
    /// (`VAR_INT 0`: it knows no data map) and the second by echoing the flags, which is what the
    /// server reads (`neoforge_session.rs` only decodes the data map reply).
    pub async fn connect_and_join_neoforge(addr: SocketAddr, name: &str) -> Result<Self, String> {
        let (_declaration, query, announced) = neoforge_negotiation();
        Self::connect_and_join_neoforge_with(addr, name, query, announced).await
    }

    /// [`Self::connect_and_join_neoforge`] with the negotiation data given instead of derived.
    ///
    /// A `NeoForge` server only accepts a client whose answer reports every channel the server
    /// declared, with the same flow and version (and every channel of `REGISTRY_SYNC_REQUIRED` in
    /// what it announced). When the server's plugins run in another process, the caller has to
    /// supply them: `query` is the answer to the server's `neoforge:register`, `announced` what
    /// went into `minecraft:register`.
    pub async fn connect_and_join_neoforge_with(
        addr: SocketAddr,
        name: &str,
        query: ModdedNetworkQuery,
        announced: Vec<String>,
    ) -> Result<Self, String> {
        let mut bot = Self::connect_and_login(addr, name).await?;
        bot.send_client_information().await?;
        bot.finish_configuration_neoforge(&query, &announced).await?;
        bot.await_play_start().await?;
        Ok(bot)
    }

    /// Joins like [`Self::connect_and_join_neoforge`], but derives the handshake from **what the
    /// server says on the connection** instead of from the plugins of this process, and learns the
    /// rest from the refusals of the server itself (see `MirroredChannels`).
    ///
    /// This is what makes a server whose mods run in another process automatable: this process has
    /// no declaration to read, and a `NeoForge` server announces only the channels it listens on
    /// and asks an empty query, so the set it will accept has to come from the server one attempt
    /// at a time. `MIRRORING_ATTEMPTS` bounds that, so a refusal that is not about channels (a real
    /// mod mismatch, a policy) fails with the server's own message instead of looping.
    pub async fn connect_and_join_neoforge_mirroring(
        addr: SocketAddr,
        name: &str,
    ) -> Result<Self, String> {
        let attempts = MIRRORING_ATTEMPTS;
        let mut mirror = MirroredChannels::default();
        let mut last = String::new();
        for _ in 0..attempts {
            let mut bot = Self::connect_and_login(addr, name).await?;
            bot.send_client_information().await?;
            match bot.finish_configuration_mirroring(&mut mirror).await? {
                MirrorOutcome::Accepted => {
                    bot.await_play_start().await?;
                    return Ok(bot);
                }
                MirrorOutcome::Refused { message } => last = message,
            }
        }
        Err(format!(
            "the server refused every one of the {attempts} mirroring attempt(s); its last refusal \
             was: {last}"
        ))
    }

    /// The configuration of a mirroring client: answer the handshake from `mirror`, and learn from
    /// a refusal instead of failing on it.
    async fn finish_configuration_mirroring(
        &mut self,
        mirror: &mut MirroredChannels,
    ) -> Result<MirrorOutcome, String> {
        let plugin_id = CPluginMessage::to_id(CURRENT_MC_VERSION);
        let ping_id = CConfigPing::to_id(CURRENT_MC_VERSION);
        let known_packs_id = CKnownPacks::to_id(CURRENT_MC_VERSION);
        let finish_config_id = CFinishConfig::to_id(CURRENT_MC_VERSION);
        let disconnect_id = CConfigDisconnect::to_id(CURRENT_MC_VERSION);
        let mut answered = false;
        let mut refusal: Option<String> = None;
        loop {
            let packet = match self.next_packet().await {
                Ok(packet) => packet,
                // A refused handshake ends with the server closing the connection.
                Err(err) => {
                    return match refusal {
                        Some(message) => Ok(MirrorOutcome::Refused { message }),
                        None => Err(err),
                    };
                }
            };
            if packet.id == plugin_id {
                let mut slice = &packet.payload[..];
                let channel = slice
                    .get_str_borrowed()
                    .map_err(|err| format!("Failed to read a configuration payload: {err}"))?;
                match channel {
                    neoforge_channels::MINECRAFT_REGISTER => {
                        mirror.observe(&decode_register_channels(slice));
                    }
                    neoforge_channels::MINECRAFT_UNREGISTER => {
                        mirror.forget(&decode_register_channels(slice));
                    }
                    neoforge_channels::MODDED_NETWORK_QUERY if !answered => {
                        // The server asks what this client registered and waits a second for the
                        // answer, so it goes out together with the announcement and the pong.
                        answered = true;
                        self.send_config_payload(
                            neoforge_channels::MINECRAFT_REGISTER,
                            &encode_register_channels(&mirror.announced()),
                        )
                        .await?;
                        self.send_config_payload(
                            neoforge_channels::MODDED_NETWORK_QUERY,
                            &mirror.query().encode(),
                        )
                        .await?;
                        // The pong is **not** sent with the answer: it belongs to the ping that
                        // follows the query, and the ping handler below sends it. A vanilla server
                        // starts its configuration tasks on a pong (`handlePong` ->
                        // `runConfiguration`), so a second one runs them twice and the second run
                        // dies on "Task neoforge:sync_registries has not finished yet".
                    }
                    neoforge_channels::MODDED_NETWORK_SETUP_FAILED => {
                        let learned = mirror.learn(slice)?;
                        refusal = Some(format!(
                            "the server refused the channels this client answered with; \
                             {learned} of them changed"
                        ));
                    }
                    other => self.handle_neoforge_payload(other, slice).await?,
                }
            } else if packet.id == ping_id {
                let mut slice = &packet.payload[..];
                let ping = CConfigPing::read(&mut slice, &CURRENT_MC_VERSION)
                    .map_err(|err| format!("Failed to read a configuration ping: {err}"))?;
                self.send_packet(&SConfigPong { id: ping.id }).await?;
            } else if packet.id == known_packs_id {
                self.send_packet(&SKnownPacks {
                    known_packs: Vec::new(),
                })
                .await?;
            } else if packet.id == finish_config_id {
                self.send_packet(&SAcknowledgeFinishConfig).await?;
                return Ok(MirrorOutcome::Accepted);
            } else if packet.id == disconnect_id {
                let mut slice = &packet.payload[..];
                let message = slice
                    .get_component(&CURRENT_MC_VERSION)
                    .map_or_else(|_| String::new(), |reason| reason.to_pretty_console());
                return Ok(MirrorOutcome::Refused { message });
            }
        }
    }

    /// Sends a configuration custom payload (`SPluginMessage`).
    async fn send_config_payload(&mut self, channel: &str, data: &[u8]) -> Result<(), String> {
        self.send_packet(&SPluginMessage { channel, data }).await
    }

    /// The configuration of a `NeoForge` client: answer the handshake, then the vanilla known
    /// packs and finish.
    async fn finish_configuration_neoforge(
        &mut self,
        query: &ModdedNetworkQuery,
        announced: &[String],
    ) -> Result<(), String> {
        self.send_config_payload(
            neoforge_channels::MINECRAFT_REGISTER,
            &encode_register_channels(announced),
        )
        .await?;
        self.send_config_payload(neoforge_channels::MODDED_NETWORK_QUERY, &query.encode())
            .await?;
        self.send_packet(&SConfigPong {
            id: NEOFORGE_PING_ID,
        })
        .await?;

        let plugin_id = CPluginMessage::to_id(CURRENT_MC_VERSION);
        let ping_id = CConfigPing::to_id(CURRENT_MC_VERSION);
        let known_packs_id = CKnownPacks::to_id(CURRENT_MC_VERSION);
        let finish_config_id = CFinishConfig::to_id(CURRENT_MC_VERSION);
        loop {
            let pkt = self.next_packet().await?;
            if pkt.id == plugin_id {
                let mut slice = &pkt.payload[..];
                let channel = slice
                    .get_str_borrowed()
                    .map_err(|err| format!("Failed to read a configuration payload: {err}"))?;
                self.handle_neoforge_payload(channel, slice).await?;
            } else if pkt.id == ping_id {
                let mut slice = &pkt.payload[..];
                let ping = CConfigPing::read(&mut slice, &CURRENT_MC_VERSION)
                    .map_err(|err| format!("Failed to read a configuration ping: {err}"))?;
                self.send_packet(&SConfigPong { id: ping.id }).await?;
            } else if pkt.id == known_packs_id {
                self.send_packet(&SKnownPacks {
                    known_packs: Vec::new(),
                })
                .await?;
            } else if pkt.id == finish_config_id {
                self.send_packet(&SAcknowledgeFinishConfig).await?;
                break;
            }
        }
        Ok(())
    }

    /// One configuration payload of the server, answered as the `NeoForge` handshake expects.
    async fn handle_neoforge_payload(&mut self, channel: &str, data: &[u8]) -> Result<(), String> {
        self.neoforge_handshake
            .received_channels
            .push(channel.to_string());
        match channel {
            // The negotiation was accepted, so the play phase is `NeoForge`.
            neoforge_channels::MODDED_NETWORK => self.neoforge = true,
            BRAND_CHANNEL => {
                self.send_config_payload(BRAND_CHANNEL, &client_brand_payload()).await?;
            }
            neoforge_channels::FROZEN_REGISTRY => {
                self.neoforge_handshake.frozen_registries.push(data.to_vec());
            }
            neoforge_channels::FROZEN_REGISTRY_SYNC_COMPLETED => {
                self.send_config_payload(neoforge_channels::FROZEN_REGISTRY_SYNC_COMPLETED, &[])
                    .await?;
                self.neoforge_handshake.registry_sync_acknowledged = true;
            }
            neoforge_channels::COMMON_VERSION => {
                self.send_config_payload(neoforge_channels::COMMON_VERSION, &encode_common_version())
                    .await?;
            }
            neoforge_channels::COMMON_REGISTER => {
                self.send_config_payload(
                    neoforge_channels::COMMON_REGISTER,
                    &encode_common_register(Phase::Play, &[]),
                )
                .await?;
            }
            // An empty reply: this client knows no data map of its own.
            neoforge_channels::KNOWN_REGISTRY_DATA_MAPS => {
                self.send_config_payload(
                    neoforge_channels::KNOWN_REGISTRY_DATA_MAPS_REPLY,
                    &[0],
                )
                .await?;
            }
            // An empty acknowledgement (`ExtensibleEnumAcknowledgePayload` is a unit codec): the
            // step exists so the server learns that the client's enum values are the ones it sent,
            // and a server that never sends the check (Pumpkin) never sees this arm.
            neoforge_channels::EXTENSIBLE_ENUM_DATA => {
                self.send_config_payload(neoforge_channels::EXTENSIBLE_ENUM_ACK, &[])
                    .await?;
            }
            // An empty acknowledgement, like the enum one: `FeatureFlagAcknowledgePayload` is a
            // unit codec, so echoing the flags it received back leaves a byte the server refuses
            // the packet for ("found 1 bytes extra").
            neoforge_channels::FEATURE_FLAGS => {
                self.send_config_payload(neoforge_channels::FEATURE_FLAGS_ACK, &[])
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn move_to(&mut self, position: Vector3<f64>, on_ground: bool) -> Result<(), String> {
        self.position = position;
        self.send_packet(&SPlayerPosition {
            position,
            collision: u8::from(on_ground),
        })
        .await
    }

    pub async fn send_command(&mut self, command: &str) -> Result<(), String> {
        self.send_packet(&SChatCommand { command }).await
    }

    pub async fn set_held_item(&mut self, slot: i16) -> Result<(), String> {
        self.send_packet(&SSetHeldItem { slot }).await
    }

    pub async fn send_player_command(
        &mut self,
        entity_id: i32,
        action: Action,
    ) -> Result<(), String> {
        self.send_packet(&SPlayerCommand {
            entity_id: VarInt(entity_id),
            action,
            jump_boost: VarInt(0),
        })
        .await
    }

    pub async fn use_block(&mut self, position: BlockPos, sequence: i32) -> Result<(), String> {
        self.send_packet(&SUseItemOn {
            hand: VarInt(0),
            position,
            face: VarInt(1),
            cursor_pos: Vector3::new(0.5, 0.5, 0.5),
            inside_block: false,
            is_against_world_border: false,
            sequence: VarInt(sequence),
        })
        .await
    }

    pub async fn start_digging(&mut self, position: BlockPos, sequence: i32) -> Result<(), String> {
        self.send_packet(&SPlayerAction {
            status: VarInt(Status::StartedDigging as i32),
            position,
            face: 1,
            sequence: VarInt(sequence),
        })
        .await
    }
}

/// The handshake data [`HeadlessBot::connect_and_join_neoforge`] sends, derived from the
/// declarations of the plugins of **this** process.
///
/// That is the answer to the server's `neoforge:register` query and the channels announced with
/// `minecraft:register`, plus the declaration they came from (`None` when no plugin declared
/// anything, and then a plain `NeoForge` client reports its own registrations).
///
/// A `NeoForge` server runs the same declaration, so feeding these to its own
/// `handshake::decide` is the negotiation a live connection would get — which is how a test can
/// prove the handshake without a socket.
#[must_use]
pub fn neoforge_negotiation() -> (Option<Arc<Aggregate>>, ModdedNetworkQuery, Vec<String>) {
    let declaration = declaration::current();
    let aggregate = declaration.as_deref();
    let query = match aggregate {
        Some(aggregate) => ModdedNetworkQuery {
            components: aggregate.channels.clone(),
        },
        None => builtin_query(),
    };
    let announced = receivable_channels(aggregate);
    (declaration, query, announced)
}

/// The registrations of a plain `NeoForge` client, without the mods of the server: used when the
/// declaration of the server's plugins is not registered in this process (an out-of-process
/// server).
fn builtin_query() -> ModdedNetworkQuery {
    let mut components = BTreeMap::new();
    for phase in [Phase::Configuration, Phase::Play] {
        components.insert(phase, neoforge_channels::own_registrations(phase));
    }
    ModdedNetworkQuery { components }
}

/// What this client announces with `minecraft:register`: the configuration payloads it can
/// receive, which a `NeoForge` client takes from its own registrations.
///
/// The channels a clientbound payload is announced on include `REGISTRY_SYNC_REQUIRED`, which the
/// server checks before it negotiates anything (`handshake::decide`), and the optional steps it
/// then runs (`neoforge:config_file`, the data map reply, the feature flags).
fn receivable_channels(aggregate: Option<&Aggregate>) -> Vec<String> {
    let declared = aggregate.and_then(|aggregate| aggregate.channels.get(&Phase::Configuration));
    let mut announced: Vec<String> = Vec::new();
    for component in neoforge_channels::own_registrations(Phase::Configuration)
        .into_iter()
        .chain(declared.into_iter().flatten().cloned())
    {
        if component.flow != Some(Flow::Serverbound) && !announced.contains(&component.id) {
            announced.push(component.id);
        }
    }
    announced
}

/// What a `NeoForge` connection ended its configuration in.
enum MirrorOutcome {
    /// The server negotiated the connection: it is a `NeoForge` connection from here on.
    Accepted,
    /// The server refused the handshake; `message` is the text it sent with the refusal.
    Refused { message: String },
}

/// What a client that cannot read a server's `NeoForge` declaration in-process knows about the
/// server's payload channels, and how it learns the rest from the server itself.
///
/// A `NeoForge` server decides on the answer to its `neoforge:register` query: every channel it
/// registered that is not optional has to be in the answer, with the same flow and the same
/// version (`pumpkin_core::net::neoforge::negotiation`). None of that is on the wire — the server
/// announces the channels it *listens* on and sends an empty query — so a client without the mods
/// has to be told. It can be: a refusal carries `neoforge:modded_network_setup_failed`, one reason
/// per channel that did not match (`missing.server.client`, `flow.client.missing` with the flow
/// the server expects, `version.mismatch` with both versions), and that is the whole channel set
/// one connection at a time.
#[derive(Clone, Debug)]
struct MirroredChannels {
    /// Per channel id, what the server expects of it. Seeded with what a plain `NeoForge` client
    /// registers, which every `NeoForge` server shares.
    components: BTreeMap<String, MirrorComponent>,
}

/// What is known about one mirrored channel.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MirrorComponent {
    /// The direction it flows in, `None` for both.
    flow: Option<Flow>,
    /// The version the server registered it with; `1` is what `NeoForge` and its mods use unless
    /// they version a channel, and a mismatch is corrected from the server's own reason.
    version: String,
}

impl Default for MirrorComponent {
    fn default() -> Self {
        Self {
            flow: None,
            version: "1".to_string(),
        }
    }
}

impl Default for MirroredChannels {
    fn default() -> Self {
        let mut components = BTreeMap::new();
        for phase in [Phase::Configuration, Phase::Play] {
            for component in neoforge_channels::own_registrations(phase) {
                components.insert(
                    component.id.clone(),
                    MirrorComponent {
                        flow: component.flow,
                        version: component.version.clone(),
                    },
                );
            }
        }
        Self { components }
    }
}

impl MirroredChannels {
    /// Records the channels a server announced it listens on (`minecraft:register`).
    fn observe(&mut self, channels: &[String]) {
        for channel in channels {
            self.components.entry(channel.clone()).or_default();
        }
    }

    /// Forgets the channels a server stopped listening on (`minecraft:unregister`).
    fn forget(&mut self, channels: &[String]) {
        for channel in channels {
            self.components.remove(channel);
        }
    }

    /// The answer to the `neoforge:register` query: both protocols, every channel optional.
    ///
    /// Optional matters twice: a channel the server does not have is dropped instead of failing
    /// the negotiation, and so is one of the wrong phase — a refusal does not say which phase a
    /// channel belongs to, so the same set is answered for both.
    fn query(&self) -> ModdedNetworkQuery {
        let components: Vec<QueryComponent> = self
            .components
            .iter()
            .map(|(id, component)| QueryComponent {
                id: id.clone(),
                version: component.version.clone(),
                flow: component.flow,
                optional: true,
            })
            .collect();
        ModdedNetworkQuery {
            components: BTreeMap::from([
                (Phase::Configuration, components.clone()),
                (Phase::Play, components),
            ]),
        }
    }

    /// What this client announces it can receive: the channels a plain `NeoForge` client always
    /// announces (the registry synchronisation needs three of them) plus everything mirrored.
    fn announced(&self) -> Vec<String> {
        let mut announced = receivable_channels(None);
        for channel in self.components.keys() {
            if !announced.contains(channel) {
                announced.push(channel.clone());
            }
        }
        announced
    }

    /// Learns from one `neoforge:modded_network_setup_failed` payload: the reasons of the
    /// negotiation the server refused. Returns how many channels changed.
    fn learn(&mut self, payload: &[u8]) -> Result<usize, String> {
        let mut reader = Reader::new(payload);
        let count = reader
            .var_len(MAX_MIRRORED_CHANNELS)
            .map_err(|err| format!("Failed to read a refusal: {err}"))?;
        let mut changed = 0;
        for _ in 0..count {
            let id = reader
                .string()
                .map_err(|err| format!("Failed to read a refused channel: {err}"))?;
            let reason = NbtText::read(&mut reader)
                .map_err(|err| format!("Failed to read a refusal reason: {err}"))?;
            changed += self.apply(&id, update(&reason));
        }
        Ok(changed)
    }

    /// Applies one reason to the mirror, counting it only when it changed something.
    fn apply(&mut self, id: &str, update: Option<Update>) -> usize {
        match update {
            Some(Update::Add) => {
                usize::from(
                    self.components
                        .insert(id.to_string(), MirrorComponent::default())
                        .is_none(),
                )
            }
            Some(Update::Drop) => usize::from(self.components.remove(id).is_some()),
            Some(Update::Flow(flow)) => match self.components.get_mut(id) {
                Some(component) if component.flow != flow => {
                    component.flow = flow;
                    1
                }
                _ => 0,
            },
            Some(Update::Version(version)) => match self.components.get_mut(id) {
                Some(component) if component.version != version => {
                    component.version = version;
                    1
                }
                _ => 0,
            },
            None => 0,
        }
    }
}

/// What one refusal reason asks for.
enum Update {
    /// The server has this channel and requires it: mirror it.
    Add,
    /// This client answered with a channel the server does not have (and requires): drop it.
    Drop,
    /// The channel's flow has to be this one, the server's (`None` for both directions).
    Flow(Option<Flow>),
    /// The channel's version has to be this one, the server's.
    Version(String),
}

/// The update one `neoforge:modded_network_setup_failed` reason implies, if any.
///
/// The keys are `NeoForge`'s own (`NetworkComponentNegotiator`), and the reason of a channel of a
/// mod is wrapped in the `.mod` key with the mod name as its first argument.
fn update(reason: &NbtText) -> Option<Update> {
    let NbtText::Translatable { key, args } = reason else {
        return None;
    };
    let (mut key, mut args) = (key.as_str(), args.as_slice());
    if key.ends_with("failure.mod")
        && let [_, NbtText::Translatable { key: inner, args: inner_args }] = args
    {
        key = inner.as_str();
        args = inner_args.as_slice();
    }
    let literal = |index: usize| match args.get(index) {
        Some(NbtText::Literal(text)) => Some(text.as_str()),
        _ => None,
    };
    if key.ends_with("failure.missing.server.client") {
        Some(Update::Add)
    } else if key.ends_with("failure.missing.client.server") {
        Some(Update::Drop)
    } else if key.ends_with("failure.flow.client.missing")
        || key.ends_with("failure.flow.client.mismatch")
    {
        // The component the server validated first is its own, so the first argument is the flow
        // it expects.
        flow_of(literal(0)?).map(|flow| Update::Flow(Some(flow)))
    } else if key.ends_with("failure.flow.server.missing") {
        // The server validated this client's component, whose flow is set, against its own, which
        // has none: the channel flows both ways.
        Some(Update::Flow(None))
    } else if key.ends_with("failure.flow.server.mismatch") {
        flow_of(literal(1)?).map(|flow| Update::Flow(Some(flow)))
    } else if key.ends_with("failure.version.mismatch") {
        // The arguments are the client's version, then the server's.
        literal(1).map(|version| Update::Version(version.to_string()))
    } else {
        None
    }
}

/// The flow a `PacketFlow` constant name means, as a failure reason writes it.
fn flow_of(name: &str) -> Option<Flow> {
    match name {
        "SERVERBOUND" => Some(Flow::Serverbound),
        "CLIENTBOUND" => Some(Flow::Clientbound),
        _ => None,
    }
}

pub async fn send_framed_packet<P: ClientPacket + MultiVersionJavaPacket>(
    writer: &mut TCPNetworkEncoder<BufWriter<OwnedWriteHalf>>,
    packet: &P,
) -> Result<(), String> {
    let packet_id = P::to_id(CURRENT_MC_VERSION);
    let mut buf = Vec::new();
    buf.write_var_int(&VarInt(packet_id))
        .map_err(|err| format!("Failed to write packet id {packet_id}: {err:?}"))?;
    packet
        .write_packet_data(&mut buf, &CURRENT_MC_VERSION)
        .map_err(|err| format!("Failed to serialize packet {packet_id}: {err:?}"))?;
    writer
        .write_packet(Bytes::from(buf))
        .await
        .map_err(|err| format!("Failed to write packet to socket: {err}"))?;
    writer
        .flush()
        .await
        .map_err(|err| format!("Failed to flush socket: {err}"))
}
