//! `pumpkin-tester client`: a real protocol-777 client that writes down what it receives.
//!
//! Used to observe a server from the outside while someone else drives it — the Dart test
//! harness spawns one `client` per simulated player next to a server, runs a console command,
//! and asserts on the resulting trace. The format is the server-side one
//! (`pumpkin-core/src/net/packet_trace.rs`) so the two can be read side by side, with
//! `"dir":"recv"` and the bot's `--name` as the identity of this connection:
//!
//! ```text
//! {"dir":"recv","client":"A","kind":"bundle_start"}
//! {"dir":"recv","client":"A","kind":"add_entity","entity":42,"type":161,"pos":[1,64,2],"bundle":true}
//! {"dir":"recv","client":"A","kind":"custom_payload","channel":"c:spawn","data":"0a01","bundle":true}
//! {"dir":"recv","client":"A","kind":"bundle_end"}
//! {"dir":"recv","client":"A","kind":"other","id":80}
//! ```
//!
//! Every received packet produces a line, so "the server never sent it" and "the client did not
//! get it" cannot be confused; bundle delimiters get a line each, so a missing closing delimiter
//! is visible instead of being inferred from adjacency. Decoding is
//! [`crate::packet_capture`]'s — the same code the integration tests assert through.

use std::fmt::Write as _;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_protocol::java::client::play::{
    CBlockEntityData, CBlockUpdate, CBundleDelimiter, CChunkData, CCustomPayload, CMultiBlockUpdate,
    CSpawnEntity, CUnloadChunk,
};
use pumpkin_protocol::java::packet_encoder::TCPNetworkEncoder;
use pumpkin_protocol::java::server::play::SKeepAlive;
use pumpkin_protocol::packet::MultiVersionJavaPacket;
use tokio::io::{AsyncReadExt, BufWriter};
use tokio::net::tcp::OwnedWriteHalf;

use crate::bot::send_framed_packet;
use crate::packet_capture::{CapturedPacket, PacketCapture};
use crate::recorder::ConnectedBot;

/// How often the socket is drained into the trace file.
const DRAIN_INTERVAL: Duration = Duration::from_millis(25);
/// Grace before the last drain, so packets in flight at exit still land in the file.
const CLOSE_GRACE: Duration = Duration::from_millis(250);

/// The `pumpkin-tester client` invocation.
#[derive(Clone, Debug)]
pub struct ClientOptions {
    /// The server to join.
    pub addr: SocketAddr,
    /// The player name to join as.
    pub name: String,
    /// Where to write the received-packet trace.
    pub trace: PathBuf,
    /// Join as a `NeoForge` client (the modded handshake a peer added), so the server sends the
    /// modded payloads (`neoforge:advanced_add_entity` and friends).
    pub neoforge: bool,
    /// Join by mirroring: derive the `NeoForge` handshake from what the **server** says (and from
    /// its refusals), instead of from the plugins of this process — which is the only way to join
    /// a server whose mods run out of process, such as a `Java` `NeoForge` server.
    ///
    /// Implies `--neoforge`; against a server without the handshake it is a plain join.
    pub mirror: bool,
}

/// The state the trace needs across drains: how much is written, and whether a bundle is open.
struct TraceState {
    written: usize,
    in_bundle: bool,
    lines: u64,
}

/// Joins, records every packet into `options.trace`, and returns once stdin closes or the process
/// is signalled. `Err` means the join never completed, which is a failed run.
#[expect(
    clippy::print_stderr,
    reason = "a CLI mode: its one summary line belongs on stderr"
)]
pub async fn run(options: ClientOptions) -> Result<(), String> {
    let name = options.name.clone();
    let mut bot = if options.mirror {
        ConnectedBot::join_neoforge_mirroring(options.addr, &options.name).await?
    } else if options.neoforge {
        ConnectedBot::join_neoforge(options.addr, &options.name).await?
    } else {
        ConnectedBot::join(options.addr, &options.name).await?
    };

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&options.trace)
        .map_err(|err| {
            format!(
                "cannot open the trace file {}: {err}",
                options.trace.display()
            )
        })?;

    let capture = bot.recorder().capture();
    let writer = bot.writer();
    let mut state = TraceState {
        written: 0,
        in_bundle: false,
        lines: 0,
    };
    let mut stdin = tokio::io::stdin();
    let mut buffer = [0u8; 256];

    let stop = loop {
        tokio::select! {
            () = signalled() => break "signal",
            read = stdin.read(&mut buffer) => match read {
                Ok(0) | Err(_) => break "stdin closed",
                Ok(_) => {}
            },
            () = tokio::time::sleep(DRAIN_INTERVAL) => {
                drain(&capture, writer, &mut file, &mut state, &name).await?;
            }
        }
    };

    tokio::time::sleep(CLOSE_GRACE).await;
    drain(&capture, writer, &mut file, &mut state, &name).await?;
    eprintln!(
        "client '{name}': joined {} ({stop}), wrote {} trace line(s) to {}",
        options.addr,
        state.lines,
        options.trace.display()
    );
    Ok(())
}

/// Resolves when the process is asked to stop, so the tail of the trace is still written.
async fn signalled() {
    #[cfg(unix)]
    {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .ok();
        tokio::select! {
            () = async { tokio::signal::ctrl_c().await.ok(); } => {}
            () = async {
                match terminate.as_mut() {
                    Some(terminate) => {
                        terminate.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.ok();
}

/// Writes the packets that arrived since the last drain, answering keep-alives on the way.
async fn drain(
    capture: &Arc<Mutex<PacketCapture>>,
    writer: &mut TCPNetworkEncoder<BufWriter<OwnedWriteHalf>>,
    file: &mut std::fs::File,
    state: &mut TraceState,
    name: &str,
) -> Result<(), String> {
    let mut text = String::new();
    let mut keep_alives = Vec::new();
    {
        let capture = capture
            .lock()
            .map_err(|_| "packet capture lock poisoned".to_string())?;
        for packet in &capture.all()[state.written..] {
            if let Some(keep_alive) = keep_alive_id(packet) {
                keep_alives.push(keep_alive);
            }
            let before = text.len();
            write_packet_lines(&mut text, packet, name, &mut state.in_bundle);
            if text.len() == before {
                // A packet of a kind this trace decodes, whose payload did not decode. The
                // client still received it, so it gets the catch-all rather than vanishing
                // from the file.
                let mut line = prefix(name);
                let _ = write!(
                    line,
                    ",\"kind\":\"other\",\"id\":{},\"len\":{}",
                    packet.id,
                    packet.payload.len()
                );
                finish(&mut line, state.in_bundle);
                text.push_str(&line);
            }
        }
        state.written = capture.all().len();
    }
    state.lines += text.lines().count() as u64;
    if !text.is_empty() {
        file.write_all(text.as_bytes())
            .map_err(|err| format!("cannot write to the trace file: {err}"))?;
    }
    for keep_alive_id in keep_alives {
        // A client that never answers is kicked after the keep-alive timeout, and the bot has to
        // survive a whole test run.
        send_framed_packet(writer, &SKeepAlive { keep_alive_id }).await?;
    }
    Ok(())
}

/// The keep-alive id of a `KEEP_ALIVE` packet, if this is one.
fn keep_alive_id(packet: &CapturedPacket) -> Option<i64> {
    if packet.id != pumpkin_protocol::java::client::play::CKeepAlive::to_id(CURRENT_MC_VERSION) {
        return None;
    }
    let mut slice = &packet.payload[..];
    <pumpkin_protocol::java::client::play::CKeepAlive as pumpkin_protocol::ServerPacket>::read(
        &mut slice,
        &CURRENT_MC_VERSION,
    )
    .ok()
    .map(|keep_alive| keep_alive.keep_alive_id)
}

/// Appends the trace line(s) of one received packet.
///
/// A `SECTION_BLOCKS_UPDATE` carries several blocks and gets one line per block, like the server
/// side: a reader looks for a position, not for a packet. A packet the schema does not describe
/// still gets a line (`kind":"other"`) — "every packet received" is the point of this side.
fn write_packet_lines(
    text: &mut String,
    packet: &CapturedPacket,
    name: &str,
    in_bundle: &mut bool,
) {
    if packet.id == CBundleDelimiter::to_id(CURRENT_MC_VERSION) {
        let kind = if *in_bundle { "bundle_end" } else { "bundle_start" };
        *in_bundle = !*in_bundle;
        let mut line = prefix(name);
        let _ = write!(line, ",\"kind\":\"{kind}\"}}\n");
        text.push_str(&line);
        return;
    }

    let mut line = prefix(name);
    let bundled = *in_bundle;
    if packet.id == CBlockUpdate::to_id(CURRENT_MC_VERSION) {
        let Ok(update) = packet.block_update() else {
            return;
        };
        let _ = write!(
            line,
            ",\"kind\":\"block_update\",\"pos\":[{},{},{}],\"state\":{}",
            update.location.0.x, update.location.0.y, update.location.0.z, update.state_id.0
        );
    } else if packet.id == CMultiBlockUpdate::to_id(CURRENT_MC_VERSION) {
        let Ok(update) = packet.section_block_update() else {
            return;
        };
        for (pos, block_state) in &update.updates {
            let mut item = line.clone();
            let _ = write!(
                item,
                ",\"kind\":\"section_block_update\",\"pos\":[{},{},{}],\"state\":{}",
                pos.0.x,
                pos.0.y,
                pos.0.z,
                block_state.as_u32()
            );
            finish(&mut item, bundled);
            text.push_str(&item);
        }
        return;
    } else if packet.id == CBlockEntityData::to_id(CURRENT_MC_VERSION) {
        let Ok(block_entity) = packet.block_entity_data() else {
            return;
        };
        let _ = write!(
            line,
            ",\"kind\":\"block_entity_data\",\"pos\":[{},{},{}],\"type\":{},\"nbt\":\"{}\"",
            block_entity.location.0.x,
            block_entity.location.0.y,
            block_entity.location.0.z,
            block_entity.r#type.0,
            hex::encode(&block_entity.nbt_data)
        );
    } else if packet.id == CSpawnEntity::to_id(CURRENT_MC_VERSION) {
        let Ok(entity) = packet.add_entity() else {
            return;
        };
        let _ = write!(
            line,
            ",\"kind\":\"add_entity\",\"entity\":{},\"type\":{},\"pos\":[{},{},{}]",
            entity.entity_id.0,
            entity.r#type.0,
            entity.position.x.floor() as i32,
            entity.position.y.floor() as i32,
            entity.position.z.floor() as i32
        );
    } else if packet.id == CCustomPayload::to_id(CURRENT_MC_VERSION) {
        let Ok(payload) = packet.custom_payload() else {
            return;
        };
        let _ = write!(line, ",\"kind\":\"custom_payload\",\"channel\":\"");
        write_json_string(&mut line, &payload.channel);
        let _ = write!(line, "\",\"data\":\"{}\"", hex::encode(&payload.data));
    } else if packet.id == CChunkData::to_id(CURRENT_MC_VERSION) {
        let Ok((x, z)) = packet.chunk() else {
            return;
        };
        let mut line = prefix(name);
        let _ = write!(line, ",\"kind\":\"chunk\",\"chunk\":[{x},{z}]");
        finish(&mut line, bundled);
        text.push_str(&line);

        // A joining client gets a block entity's client data inside the chunk packet, so what the
        // column carried has to be visible: otherwise a relogin's state cannot be compared with
        // what the present client was shown.
        let Ok(block_entities) = packet.chunk_block_entities() else {
            return;
        };
        for block_entity in block_entities {
            let mut line = prefix(name);
            let _ = write!(
                line,
                ",\"kind\":\"chunk_block_entity\",\"chunk\":[{x},{z}],\"pos\":[{},{},{}],\
                 \"type\":{},\"nbt\":\"{}\"",
                block_entity.pos.0.x,
                block_entity.pos.0.y,
                block_entity.pos.0.z,
                block_entity.type_id,
                hex::encode(&block_entity.nbt)
            );
            finish(&mut line, bundled);
            text.push_str(&line);
        }
        return;
    } else if packet.id == CUnloadChunk::to_id(CURRENT_MC_VERSION) {
        let Ok((x, z)) = packet.unload_chunk() else {
            return;
        };
        let _ = write!(line, ",\"kind\":\"unload_chunk\",\"chunk\":[{x},{z}]");
    } else {
        // Unclassified, but still recorded: the id and the length are enough to tell a
        // mis-classified packet from a kind this trace does not decode.
        let _ = write!(
            line,
            ",\"kind\":\"other\",\"id\":{},\"len\":{}",
            packet.id,
            packet.payload.len()
        );
    }

    finish(&mut line, bundled);
    text.push_str(&line);
}

/// `{"dir":"recv","client":"<name>"`, the fields every line starts with.
fn prefix(name: &str) -> String {
    let mut line = String::with_capacity(160);
    line.push_str("{\"dir\":\"recv\",\"client\":\"");
    write_json_string(&mut line, name);
    line.push('"');
    line
}

/// Ends the object, marking packets that arrived inside a bundle.
fn finish(line: &mut String, bundled: bool) {
    if bundled {
        line.push_str(",\"bundle\":true");
    }
    line.push_str("}\n");
}

/// Writes `value` as the inside of a JSON string.
fn write_json_string(line: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '"' => line.push_str("\\\""),
            '\\' => line.push_str("\\\\"),
            '\n' => line.push_str("\\n"),
            '\r' => line.push_str("\\r"),
            '\t' => line.push_str("\\t"),
            other if other.is_control() => {
                let _ = write!(line, "\\u{:04x}", u32::from(other));
            }
            other => line.push(other),
        }
    }
}