//! The `NeoForge` handshake of the headless bot.
//!
//! A vanilla client never sees the modded payloads: the server negotiates the handshake only with
//! a client that answers it, and a client that does not answer is let through unmodified
//! (`allow_non_neoforge` of the plugin's declaration). `neoforge:advanced_add_entity` — where a
//! plugin entity's spawn data lives — therefore never reaches a vanilla bot, which is exactly the
//! blind spot this closes.
//!
//! Two tests:
//!
//! * [`the_negotiation_the_bot_sends_is_accepted`] feeds exactly what
//!   [`HeadlessBot::connect_and_join_neoforge`] puts on the wire to the server's own decision code
//!   (`net::neoforge::handshake::decide`), through the real encoders and decoders, and requires an
//!   acceptance with the frozen-registry channels. It needs the plugin loaded, but no socket, so
//!   it is not affected by the in-process connection lifecycle.
//! * [`the_bot_negotiates_as_a_neoforge_client`] is the end-to-end version over a socket. It is
//!   `#[ignore]`d: an in-process plugin server currently loses its wasm store driver
//!   (`pumpkin-wasm-host`: "store driver task ended without reporting a terminal state"), after
//!   which the connection is held and nothing is answered. Run it against a plugin server in its
//!   own process (the harness the Dart side drives).
//!
//! # Running
//!
//! The harness loads plugins from the cwd-relative `./plugins`
//! (`pumpkin-core/src/plugin/mod.rs:40`), so this has to run from the repository root:
//!
//! ```text
//! cargo test -p pumpkin-tester --test neoforge_handshake
//! ```

use std::{path::Path, sync::Arc, time::Duration};

use pumpkin_core::{
    command::CommandSender,
    net::neoforge::{
        channels::{self, REGISTRY_SYNC_REQUIRED},
        declaration,
        handshake::{self, Decision},
        payloads::{
            ModdedNetworkQuery, decode_register_channels, encode_register_channels,
        },
    },
    server::Server,
};
use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_protocol::{
    java::client::play::CCustomPayload, packet::MultiVersionJavaPacket, ser::NetworkReadSliceExt,
};
use pumpkin_tester::{bot::HeadlessBot, harness};
use tokio::net::TcpListener;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// The channel a `NeoForge` client takes a plugin entity's spawn data from.
const ADVANCED_ADD_ENTITY: &str = "neoforge:advanced_add_entity";

/// The channel the server's accept of the negotiation arrives on.
const MODDED_NETWORK: &str = "neoforge:network";

/// How long the handshake and the payload watch may take.
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(120);
/// How long the teardown may take.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(60);

/// **The negotiation the bot sends is one the server accepts.**
///
/// `decide` is the whole of the server's decision: the announced channels must cover
/// `REGISTRY_SYNC_REQUIRED` or the client is refused with `NO_REGISTRY_SYNC`, and the answer to
/// the query must carry a version and a flow the declaration accepts for every channel the server
/// declared, or the negotiation fails with the channels as reasons.
///
/// The payloads go through their wire encoders and decoders first, so a layout change on either
/// side fails here rather than in a live run.
#[tokio::test(flavor = "multi_thread")]
async fn the_negotiation_the_bot_sends_is_accepted() -> Result<(), String> {
    let running = boot_plugin_server().await?;
    let result = negotiation_is_accepted();
    shutdown(&running.server).await;
    result
}

fn negotiation_is_accepted() -> Result<(), String> {
    let aggregate = declaration::current()
        .ok_or_else(|| "the plugin declared nothing, so there is no handshake to accept".to_owned())?;
    let (_declaration, query, announced) = pumpkin_tester::bot::neoforge_negotiation();

    // What the socket carries: `neoforge:register` and `minecraft:register`.
    let query = ModdedNetworkQuery::decode(&query.encode())
        .map_err(|err| format!("the query answer does not decode: {err:?}"))?;
    let announced = decode_register_channels(&encode_register_channels(&announced));

    let missing: Vec<&str> = REGISTRY_SYNC_REQUIRED
        .iter()
        .copied()
        .filter(|channel| !announced.iter().any(|name| name == channel))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "the announced channels miss {missing:?}, so the server would refuse the client \
             (NO_REGISTRY_SYNC) even though it declared {} registry/registries",
            aggregate.registries.len()
        ));
    }

    match handshake::decide(&aggregate, &query, &announced) {
        Decision::Accept(setup) => {
            let configuration = setup.configuration_channels();
            if !configuration.contains(&channels::FROZEN_REGISTRY.to_string()) {
                return Err(format!(
                    "the accepted setup does not carry `{}`: {configuration:?}",
                    channels::FROZEN_REGISTRY
                ));
            }
            Ok(())
        }
        Decision::Reject(message) => Err(format!("the server refuses the bot's negotiation: {message}")),
        Decision::SetupFailed { payload, message } => Err(format!(
            "the server fails the setup for the bot's negotiation ({message}), {} byte(s) of \
             reasons",
            payload.len()
        )),
    }
}

/// **End to end, over a socket.** See the module docs for why this is ignored.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "an in-process plugin server loses its wasm store driver and holds the connection; \
            run this against a plugin server in its own process"]
#[expect(clippy::expect_used, reason = "test failure reporting")]
async fn the_bot_negotiates_as_a_neoforge_client() {
    run_end_to_end()
        .await
        .expect("the NeoForge handshake of the bot");
}

async fn run_end_to_end() -> Result<(), String> {
    let running = boot_plugin_server().await?;
    let server = running.server.clone();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|err| format!("Failed to bind a listener: {err}"))?;
    let addr = listener
        .local_addr()
        .map_err(|err| format!("Failed to read the listener address: {err}"))?;
    let cancel = CancellationToken::new();
    let tasks = TaskTracker::new();
    harness::spawn_java_listener(listener, Arc::clone(&server), cancel.clone(), &tasks);

    let result = match tokio::time::timeout(OBSERVE_TIMEOUT, observe(&server, addr)).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "The NeoForge handshake did not finish within {}s",
            OBSERVE_TIMEOUT.as_secs()
        )),
    };

    cancel.cancel();
    let teardown = async {
        for player in server.get_all_players() {
            player.remove().await;
            server.remove_player(&player);
        }
        tasks.close();
        tasks.wait().await;
        server.shutdown().await;
    };
    let _ = tokio::time::timeout(TEARDOWN_TIMEOUT, teardown).await;
    result
}

/// Joins as a `NeoForge` client, then watches what the server sends it.
async fn observe(server: &Arc<Server>, addr: std::net::SocketAddr) -> Result<(), String> {
    let mut bot = HeadlessBot::connect_and_join_neoforge(addr, "NeoForgeBot").await?;

    // 1. The handshake was accepted. The server logs "negotiated, a NeoForge client" here; the
    //    client sees `neoforge:network` and knows the play phase is modded from this point on.
    if !bot.neoforge
        || !bot
            .neoforge_handshake
            .received_channels
            .iter()
            .any(|channel| channel == MODDED_NETWORK)
    {
        return Err(format!(
            "The bot was not negotiated as a NeoForge client (neoforge={}); configuration \
             channels it saw: {:?}",
            bot.neoforge, bot.neoforge_handshake.received_channels
        ));
    }

    // 2. The frozen registries arrived and were acknowledged, which is what the server waits for
    //    before the rest of the configuration.
    if bot.neoforge_handshake.frozen_registries.is_empty() {
        return Err(format!(
            "No `neoforge:frozen_registry` payload arrived; channels: {:?}",
            bot.neoforge_handshake.received_channels
        ));
    }
    if !bot.neoforge_handshake.registry_sync_acknowledged {
        return Err("The registry sync was never acknowledged".to_string());
    }

    // 3. A plugin entity spawns. Its type declares a spawn data channel, so a `NeoForge` client
    //    gets the data as `neoforge:advanced_add_entity` (`plugin_entity.rs` picks the channel by
    //    flavour and sends it in the same bundle as the add-entity packet).
    let position = bot.position;
    let command = format!(
        "/summon create:super_glue {} {} {}",
        position.x, position.y, position.z
    );
    server.command_dispatcher.load().handle_command(
        &CommandSender::Console.into_source(server),
        command.as_str(),
    );
    for _ in 0..40 {
        server.tick();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let custom_payload_id = CCustomPayload::to_id(CURRENT_MC_VERSION);
    let mut seen: Vec<i32> = Vec::new();
    for _ in 0..40 {
        let packet = bot.next_packet().await?;
        if packet.id != custom_payload_id {
            seen.push(packet.id);
            continue;
        }
        let mut slice = &packet.payload[..];
        let channel = slice
            .get_str_borrowed()
            .map_err(|err| format!("Failed to read a play payload channel: {err}"))?;
        if channel == ADVANCED_ADD_ENTITY {
            return Ok(());
        }
        seen.push(packet.id);
    }
    Err(format!(
        "A plugin entity spawned without `{ADVANCED_ADD_ENTITY}` reaching the client; other \
         packet ids seen after the spawn: {seen:?}"
    ))
}

/// A plugin server on an ephemeral world directory.
struct Running {
    server: Arc<Server>,
    /// Kept alive for the lifetime of the server.
    _temp: tempfile::TempDir,
}

/// Stops the server without waiting for a plugin task that never ends.
async fn shutdown(server: &Arc<Server>) {
    let teardown = async {
        for player in server.get_all_players() {
            player.remove().await;
            server.remove_player(&player);
        }
        server.shutdown().await;
    };
    let _ = tokio::time::timeout(TEARDOWN_TIMEOUT, teardown).await;
}

/// A server with the plugins of `./plugins` loaded, from the repository root.
async fn boot_plugin_server() -> Result<Running, String> {
    // `PLUGIN_DIR` is the cwd-relative `./plugins` (`pumpkin-core/src/plugin/mod.rs:40`) and cargo
    // runs a test binary from its *package* directory, so the workspace root has to be established
    // explicitly or the plugin silently never loads.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "no workspace root above the package".to_string())?;
    std::env::set_current_dir(root)
        .map_err(|err| format!("Failed to enter the workspace root: {err}"))?;

    let temp = tempfile::tempdir().map_err(|err| format!("Failed to create a temp dir: {err}"))?;
    let server = harness::create_test_server_with_plugin(&temp).await?;
    Ok(Running {
        server,
        _temp: temp,
    })
}