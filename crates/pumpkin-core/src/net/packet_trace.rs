//! Opt-in, machine-readable trace of the packets the server sends to clients.
//!
//! Set `PUMPKIN_PACKET_TRACE=<path>` and the server writes one JSON object per line for every
//! packet enqueued to a client, so an external process (or a person debugging a plugin) can see
//! what the clients are actually being told. The file is truncated at boot and appended to
//! unbuffered, so a reader can `tail -f` it while the server runs.
//!
//! The trace taps the *per-client enqueue path* ([`JavaClient::queue_outgoing`]), not the packet
//! encoders: every send path goes through it, nothing is decoded twice, and a line only exists
//! when there is a recipient — `client` names it (the connection id, `JavaClient::id`). A packet
//! that is dropped because the connection is full or gone is a line of its own, so "the server
//! never sent it" and "the client did not receive it" stay distinguishable.
//!
//! # Cost
//!
//! When the variable is unset nothing here runs: [`active`] is a relaxed load of a `static` that
//! is only ever set by [`init_from_env`] at boot, and the call sites in the send path test it
//! before touching anything else. No allocation, no formatting, no lock, no syscall — the same
//! branch a `debug!` would leave behind.
//!
//! [`JavaClient::queue_outgoing`]: crate::net::java::JavaClient

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

use pumpkin_protocol::ServerPacket;
use pumpkin_protocol::codec::item_stack_seralizer::ItemStackSerializer;
use pumpkin_protocol::java::client::play::{
    CBlockEntityData, CBlockUpdate, CBundleDelimiter, CChunkData, CCustomPayload,
    CMultiBlockUpdate, COpenScreen, CSetContainerContent, CSpawnEntity, CUnloadChunk,
};
use pumpkin_protocol::packet::MultiVersionJavaPacket;
use pumpkin_protocol::ser::{NetworkReadExt, NetworkReadSliceExt, ReadingError};
use pumpkin_util::version::JavaMinecraftVersion;
use tracing::{error, warn};

/// The environment variable that turns the trace on. Its value is the trace file path.
pub const ENV_VAR: &str = "PUMPKIN_PACKET_TRACE";

/// Set once, from [`init_from_env`]. Read on every packet path.
static ENABLED: AtomicBool = AtomicBool::new(false);
/// The server tick the packets enqueued right now belong to.
static TICK: AtomicI32 = AtomicI32::new(0);
/// Guards the "the trace file is gone" warning so it is printed once, not per packet.
static WARNED: AtomicBool = AtomicBool::new(false);

struct Sink {
    file: File,
    /// Connections whose bundle is open: a `BUNDLE_DELIMITER` toggles membership.
    bundling: HashSet<u64>,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

/// Whether a trace is being written. The only check a packet path has to make.
#[inline]
#[must_use]
pub fn active() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Opens `PUMPKIN_PACKET_TRACE` if it is set. Called once at boot, before any client connects.
///
/// A path that cannot be opened leaves the trace off with an error in the log: a broken
/// debugging aid must never keep the server from running.
pub fn init_from_env() {
    let Ok(path) = std::env::var(ENV_VAR) else {
        return;
    };
    if path.is_empty() {
        return;
    }
    match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
    {
        Ok(file) => {
            let sink = Mutex::new(Sink {
                file,
                bundling: HashSet::new(),
            });
            if SINK.set(sink).is_err() {
                // A second server in the same process keeps writing to the first file.
                return;
            }
            ENABLED.store(true, Ordering::Relaxed);
            tracing::info!("{ENV_VAR}={path}: tracing client-bound packets");
        }
        Err(err) => error!("{ENV_VAR}={path}: cannot open the packet trace file: {err}"),
    }
}

/// Records the tick the packets enqueued from now on belong to.
///
/// Sampled once per server tick, so every line carries the tick whose work sent it — the same
/// number `Server::tick_count` exposes.
#[inline]
pub fn set_tick(tick: i32) {
    if active() {
        TICK.store(tick, Ordering::Relaxed);
    }
}

/// One client-bound packet, as it is handed to the connection's outgoing queue.
///
/// `data` is the serialized packet including its var-int id, in the encoding of `version` (the
/// version this connection reads).
pub fn on_outgoing(client: u64, version: JavaMinecraftVersion, data: &[u8]) {
    if !active() {
        return;
    }
    let Some(sink) = SINK.get() else {
        return;
    };
    let mut sink = sink
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    decode_and_write(&mut sink, client, version, data);
}

/// A packet that will never reach the client because its queue is full or gone.
pub fn on_dropped(client: u64, data: &[u8]) {
    if !active() {
        return;
    }
    let Some(sink) = SINK.get() else {
        return;
    };
    let mut reader = data;
    let Ok(packet_id) = reader.get_var_int() else {
        return;
    };
    let mut sink = sink
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tick = TICK.load(Ordering::Relaxed);
    let mut line = String::with_capacity(64);
    let _ = write!(
        line,
        "{{\"tick\":{tick},\"client\":{client},\"kind\":\"dropped\",\"id\":{}}}",
        packet_id.0
    );
    sink.write(&line);
}

impl Sink {
    fn write(&mut self, line: &str) {
        if let Err(err) = self.file.write_all(line.as_bytes()) {
            if !WARNED.swap(true, Ordering::Relaxed) {
                warn!("Packet trace file write failed, the trace is incomplete: {err}");
            }
        }
    }
}

/// Turns one enqueued packet into a line. Packets outside the traced kinds are skipped.
fn decode_and_write(sink: &mut Sink, client: u64, version: JavaMinecraftVersion, data: &[u8]) {
    let mut reader = data;
    let Ok(packet_id) = reader.get_var_int() else {
        return;
    };
    let id = packet_id.0;
    let tick = TICK.load(Ordering::Relaxed);
    let mut line = String::with_capacity(128);
    let _ = write!(line, "{{\"tick\":{tick},\"client\":{client}");

    // The delimiters are not data the client is shown: they frame the packets of one bundle.
    if id == CBundleDelimiter::to_id(version) {
        let kind = if sink.bundling.remove(&client) {
            "bundle_end"
        } else {
            sink.bundling.insert(client);
            "bundle_start"
        };
        let _ = write!(line, ",\"kind\":\"{kind}\"");
        close_line(&mut line, false);
        sink.write(&line);
        return;
    }

    if id == CBlockUpdate::to_id(version) {
        let (Ok(pos), Ok(state)) = (reader.get_block_pos(&version), reader.get_var_int()) else {
            return;
        };
        let _ = write!(
            line,
            ",\"kind\":\"block_update\",\"pos\":[{},{},{}],\"state\":{}",
            pos.0.x, pos.0.y, pos.0.z, state.0
        );
    } else if id == CMultiBlockUpdate::to_id(version) {
        let Ok(update) = CMultiBlockUpdate::read(&mut reader, &version) else {
            return;
        };
        // One line per block the packet carries: a reader looks for a position, not a packet.
        for (pos, state) in &update.updates {
            let mut item = line.clone();
            let _ = write!(
                item,
                ",\"kind\":\"section_block_update\",\"pos\":[{},{},{}],\"state\":{}",
                pos.0.x,
                pos.0.y,
                pos.0.z,
                state.as_u32()
            );
            close_line(&mut item, sink.bundling.contains(&client));
            sink.write(&item);
        }
        return;
    } else if id == CBlockEntityData::to_id(version) {
        let (Ok(pos), Ok(block_entity_type)) =
            (reader.get_block_pos(&version), reader.get_var_int())
        else {
            return;
        };
        let _ = write!(
            line,
            ",\"kind\":\"block_entity_data\",\"pos\":[{},{},{}],\"type\":{},\"nbt\":\"",
            pos.0.x, pos.0.y, pos.0.z, block_entity_type.0
        );
        line.push_str(&hex::encode(reader));
        line.push('"');
    } else if id == CChunkData::to_id(version) {
        let (Ok(x), Ok(z)) = (reader.get_i32(), reader.get_i32()) else {
            return;
        };
        let _ = write!(line, ",\"kind\":\"chunk\",\"chunk\":[{x},{z}]");
    } else if id == CUnloadChunk::to_id(version) {
        let Ok((x, z)) = read_chunk_position(&mut reader, &version) else {
            return;
        };
        let _ = write!(line, ",\"kind\":\"unload_chunk\",\"chunk\":[{x},{z}]");
    } else if id == CCustomPayload::to_id(version) {
        let Ok(channel) = reader.get_str_borrowed() else {
            return;
        };
        let _ = write!(line, ",\"kind\":\"custom_payload\",\"channel\":\"");
        write_json_string(&mut line, channel);
        line.push_str("\",\"data\":\"");
        line.push_str(&hex::encode(reader));
        line.push('"');
    } else if id == COpenScreen::to_id(version) {
        let (Ok(container), Ok(menu_type)) = (reader.get_var_int(), reader.get_var_int()) else {
            return;
        };
        // The title is a component: a reader that needs it reads the packet, the trace only has
        // to say which screen the client was told to open.
        let _ = write!(
            line,
            ",\"kind\":\"open_screen\",\"container\":{},\"menu_type\":{}",
            container.0, menu_type.0
        );
    } else if id == CSetContainerContent::to_id(version) {
        let Ok(container) = reader.get_container_id(&version) else {
            return;
        };
        let (Ok(state), Ok(count)) = (
            reader.get_var_int(),
            if version >= JavaMinecraftVersion::V_1_17_1 {
                reader.get_var_int()
            } else {
                reader
                    .get_i16_be()
                    .map(|count| pumpkin_protocol::VarInt(i32::from(count)))
            },
        ) else {
            return;
        };
        if !(0..=4096).contains(&count.0) {
            return;
        }
        // The slot index is the whole point of tracing this packet: a menu whose layout differs
        // from the client's shows a stack under the wrong index. Empty slots are left out.
        let _ = write!(
            line,
            ",\"kind\":\"set_container_content\",\"container\":{},\"state\":{},\"slots\":{},\"filled\":[",
            container.0, state.0, count.0
        );
        let mut first = true;
        for index in 0..count.0 {
            let Ok(stack) = ItemStackSerializer::read_with_version(&mut reader, &version) else {
                return;
            };
            if stack.0.item_count == 0 {
                continue;
            }
            if first {
                first = false;
            } else {
                line.push(',');
            }
            let _ = write!(line, "{{\"slot\":{index},\"item\":\"");
            write_json_string(&mut line, stack.0.item.registry_key);
            let _ = write!(line, "\",\"count\":{}}}", stack.0.item_count);
        }
        line.push(']');
    } else if id == CSpawnEntity::to_id(version) {
        let Some(spawn) = read_spawn_entity(&mut reader, &version) else {
            return;
        };
        let _ = write!(
            line,
            ",\"kind\":\"add_entity\",\"entity\":{},\"type\":{},\"pos\":[{},{},{}]",
            spawn.entity_id, spawn.entity_type, spawn.x, spawn.y, spawn.z
        );
    } else {
        return;
    }

    close_line(&mut line, sink.bundling.contains(&client));
    sink.write(&line);
}

/// Ends the object, marking packets that went out inside a bundle.
fn close_line(line: &mut String, bundled: bool) {
    if bundled {
        line.push_str(",\"bundle\":true");
    }
    line.push_str("}\n");
}

/// `CUnloadChunk`'s wire form: a packed key since 1.20.2, two ints before.
fn read_chunk_position(
    reader: &mut &[u8],
    version: &JavaMinecraftVersion,
) -> Result<(i32, i32), ReadingError> {
    if *version >= JavaMinecraftVersion::V_1_20_2 {
        let key = reader.get_i64_be()?;
        Ok((key as i32, (key >> 32) as i32))
    } else {
        Ok((reader.get_i32_be()?, reader.get_i32_be()?))
    }
}

/// The fields of `CAddEntity` the trace reports, in **block** coordinates.
struct SpawnEntity {
    entity_id: i32,
    entity_type: i32,
    x: i32,
    y: i32,
    z: i32,
}

/// Reads as much of `CAddEntity` as the trace needs, following its writer's version gates.
fn read_spawn_entity(reader: &mut &[u8], version: &JavaMinecraftVersion) -> Option<SpawnEntity> {
    let entity_id = reader.get_var_int().ok()?.0;
    if *version >= JavaMinecraftVersion::V_1_9 {
        // The entity UUID: not reported, but it sits between the id and the type.
        reader.get_u128_be().ok()?;
        let entity_type = reader.get_var_int().ok()?.0;
        let x = reader.get_f64_be().ok()?;
        let y = reader.get_f64_be().ok()?;
        let z = reader.get_f64_be().ok()?;
        Some(SpawnEntity {
            entity_id,
            entity_type,
            x: x.floor() as i32,
            y: y.floor() as i32,
            z: z.floor() as i32,
        })
    } else {
        let entity_type = i32::from(reader.get_u8().ok()?);
        let x = reader.get_i32_be().ok()?;
        let y = reader.get_i32_be().ok()?;
        let z = reader.get_i32_be().ok()?;
        Some(SpawnEntity {
            entity_id,
            entity_type,
            x,
            y,
            z,
        })
    }
}

/// Writes `value` as the inside of a JSON string. Resource locations are plain ASCII, but a
/// payload channel is plugin-chosen, so quotes and backslashes have to survive the round trip.
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
