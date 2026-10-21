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
use crate::packet_capture::{CapturedPacket, ChunkLight, LightLayer, PacketCapture, SectionLight};
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
    /// World block positions whose chunk light is written out as a `chunk_light_cell` line, so a
    /// specific cell's sky/block light can be read out of the payload without decoding the whole
    /// section array by hand.
    pub light_at: Vec<(i32, i32, i32)>,
    /// The world's minimum Y (`-64` for the overworld), which the chunk packet does not carry: a
    /// client knows it from the dimension, and it is what maps a cell to its light section.
    pub min_y: i32,
    /// A directory to write every chunk packet's payload bytes into, one file per column
    /// (`chunk_<x>_<z>.bin`, the payload without its packet id) — the bytes a decode can be
    /// debugged against.
    pub dump_chunks: Option<PathBuf>,
}

impl ClientOptions {
    /// The defaults a caller that only cares about the packet trace gets.
    #[must_use]
    pub fn new(addr: SocketAddr, name: String, trace: PathBuf) -> Self {
        Self {
            addr,
            name,
            trace,
            neoforge: false,
            mirror: false,
            light_at: Vec::new(),
            min_y: -64,
            dump_chunks: None,
        }
    }
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
                drain(&capture, writer, &mut file, &mut state, &name, &options).await?;
            }
        }
    };

    tokio::time::sleep(CLOSE_GRACE).await;
    drain(&capture, writer, &mut file, &mut state, &name, &options).await?;
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
    options: &ClientOptions,
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
            write_packet_lines(&mut text, packet, name, &mut state.in_bundle, options);
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
    options: &ClientOptions,
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

        if let Some(dir) = &options.dump_chunks {
            let path = dir.join(format!("chunk_{x}_{z}.bin"));
            if let Err(error) = std::fs::write(&path, &packet.payload[..]) {
                let mut line = prefix(name);
                let _ = write!(line, ",\"kind\":\"chunk_dump_failed\",\"chunk\":[{x},{z}],\
                     \"error\":\"{error}\"");
                finish(&mut line, bundled);
                text.push_str(&line);
            }
        }

        // A joining client gets a block entity's client data inside the chunk packet, so what the
        // column carried has to be visible: otherwise a relogin's state cannot be compared with
        // what the present client was shown.
        //
        // A payload this reader cannot walk is a line of its own, not silence: a client that
        // cannot read the list reads the light that follows it at the wrong offset, so "the light
        // data was missing" and "the payload was malformed" have to stay distinguishable.
        match packet.chunk_block_entities() {
            Ok(block_entities) => {
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
            }
            Err(error) => {
                let mut line = prefix(name);
                let _ = write!(line, ",\"kind\":\"chunk_undecodable\",\"chunk\":[{x},{z}],\
                     \"part\":\"block_entities\",\"error\":\"");
                write_json_string(&mut line, &error);
                let _ = write!(line, "\"");
                finish(&mut line, bundled);
                text.push_str(&line);
            }
        }

        // The light and the heightmaps the column carried. A relogin's chunk is the only time a
        // client hears the server's light for a chunk, so it has to be readable: the masks and a
        // digest of the arrays say whether two sends agree byte for byte, and `--light-at` reads
        // a named cell out of the payload.
        if let Ok(light) = packet.chunk_light() {
            let mut line = prefix(name);
            let _ = write!(
                line,
                ",\"kind\":\"chunk_light\",\"chunk\":[{x},{z}],\
                 \"sky_light_mask\":{},\"block_light_mask\":{},\
                 \"empty_sky_light_mask\":{},\"empty_block_light_mask\":{},\
                 \"sky_light_arrays\":{},\"block_light_arrays\":{},\
                 \"sky_light_digest\":\"{:016x}\",\"block_light_digest\":\"{:016x}\"",
                light.sky_light_mask,
                light.block_light_mask,
                light.empty_sky_light_mask,
                light.empty_block_light_mask,
                light.sky_light_arrays.len(),
                light.block_light_arrays.len(),
                light.digest(LightLayer::Sky),
                light.digest(LightLayer::Block),
            );
            finish(&mut line, bundled);
            text.push_str(&line);

            for (cell_x, cell_y, cell_z) in &options.light_at {
                if cell_x.div_euclid(16) != x || cell_z.div_euclid(16) != z {
                    continue;
                }
                let mut line = prefix(name);
                let _ = write!(
                    line,
                    ",\"kind\":\"chunk_light_cell\",\"chunk\":[{x},{z}],\"at\":[{cell_x},{cell_y},{cell_z}]"
                );
                write_cell_light(&mut line, &light, options.min_y, *cell_x, *cell_y, *cell_z);
                finish(&mut line, bundled);
                text.push_str(&line);
            }
        } else if let Err(error) = packet.chunk_light() {
            let mut line = prefix(name);
            let _ = write!(line, ",\"kind\":\"chunk_undecodable\",\"chunk\":[{x},{z}],\
                 \"part\":\"light\",\"error\":\"");
            write_json_string(&mut line, &error);
            let _ = write!(line, "\"");
            finish(&mut line, bundled);
            text.push_str(&line);
        }
        if let Ok(heightmaps) = packet.chunk_heightmaps() {
            for heightmap in heightmaps {
                let mut line = prefix(name);
                let _ = write!(
                    line,
                    ",\"kind\":\"chunk_heightmap\",\"chunk\":[{x},{z}],\"heightmap\":{},\
                     \"longs\":{},\"digest\":\"{:016x}\"",
                    heightmap.index,
                    heightmap.data.len(),
                    heightmap.digest(),
                );
                for (cell_x, _cell_y, cell_z) in &options.light_at {
                    if cell_x.div_euclid(16) != x || cell_z.div_euclid(16) != z {
                        continue;
                    }
                    let _ = write!(
                        line,
                        ",\"height_at\":{}",
                        heightmap.height_at(
                            options.min_y,
                            cell_x.rem_euclid(16) as usize,
                            cell_z.rem_euclid(16) as usize,
                        )
                    );
                }
                finish(&mut line, bundled);
                text.push_str(&line);
            }
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

/// Appends the sky and block light of one cell as the payload states them, with the kind of
/// section each value came from (a section no mask mentions states nothing, and a client keeps
/// the light it already had for it).
fn write_cell_light(
    line: &mut String,
    light: &ChunkLight,
    min_y: i32,
    x: i32,
    y: i32,
    z: i32,
) {
    for (name, layer) in [("sky", LightLayer::Sky), ("block", LightLayer::Block)] {
        let section = usize::try_from((y - min_y).div_euclid(16)).unwrap_or(usize::MAX);
        let kind = match light.section(layer, section) {
            SectionLight::Array(_) => "array",
            SectionLight::Empty => "empty",
            SectionLight::Absent => "absent",
        };
        match light.cell(layer, min_y, x, y, z) {
            Some(value) => {
                let _ = write!(line, ",\"{name}\":{value},\"{name}_from\":\"{kind}\"");
            }
            None => {
                let _ = write!(line, ",\"{name}\":null,\"{name}_from\":\"{kind}\"");
            }
        }
    }
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