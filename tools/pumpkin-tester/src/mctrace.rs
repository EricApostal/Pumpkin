//! `mctrace`: a recording TCP proxy in front of a Minecraft server, and the decoder of what it
//! recorded.
//!
//! The proxy needs no protocol knowledge: every Java edition connection frames each packet as a
//! `VarInt` length followed by that many bytes, so it splits on that, records the payload and
//! forwards it. That is what makes it server-agnostic (a Java `NeoForge` server, Pumpkin, any
//! mod set, any version) and what makes it useful for a differential between two servers: the
//! same client, the same scenario, the same recording format on both sides. It records **both**
//! directions, so what a working client *answers* with is as visible as what the server sends.
//!
//! One JSON line per frame, flushed as it is written, so the file can be tailed while a run is
//! live:
//!
//! ```text
//! {"c":1,"dir":"c2s","t":12,"id":0,"len":16,"data":"04f0..."}
//! ```
//!
//! | field | meaning |
//! | ----- | ------- |
//! | `c` | the connection index, 1 for the first client that connects (a relogin is a new number) |
//! | `dir` | `c2s` (client to server) or `s2c` (server to client) |
//! | `t` | milliseconds since the proxy started |
//! | `id` | the `VarInt` at the start of the frame, `null` when there is not one (an empty payload). **A frame the server compressed after `set_compression` starts with the uncompressed length instead of the id**, which this field then carries; [`decode`] resolves it |
//! | `len` | the frame length: the bytes after the length prefix |
//! | `data` | lowercase hex of the frame's bytes, exactly as they crossed the socket (the id first), so a frame can be replayed byte for byte |
//!
//! [`decode`] is the other half: it replays the recorded frames of a connection into the protocol
//! decoder this repository already has (`pumpkin-protocol`'s `TCPNetworkDecoder`, which also
//! handles that compression), and classifies the packets through [`crate::packet_capture`], so a
//! capture can be read with the decoders the rest of the tooling uses. A connection whose session
//! negotiated `NeoForge` writes the play phase in the `NeoForge` encodings, which these decoders
//! do not know; those packets are reported as `other`.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_data::packet::clientbound::config::CUSTOM_PAYLOAD as CONFIG_CUSTOM_PAYLOAD;
use pumpkin_data::packet::clientbound::config::FINISH_CONFIGURATION as CONFIG_FINISH;
use pumpkin_data::packet::clientbound::login::LOGIN_COMPRESSION;
use pumpkin_data::packet::clientbound::login::LOGIN_FINISHED;
use pumpkin_data::packet::serverbound::config::CUSTOM_PAYLOAD as SERVERBOUND_CONFIG_CUSTOM_PAYLOAD;
use pumpkin_data::packet::serverbound::config::FINISH_CONFIGURATION as SERVERBOUND_CONFIG_FINISH;
use pumpkin_data::packet::serverbound::login::LOGIN_ACKNOWLEDGED;
use pumpkin_protocol::java::packet_decoder::TCPNetworkDecoder;
use pumpkin_protocol::java::client::login::CSetCompression;
use pumpkin_protocol::ser::{NetworkReadExt, NetworkWriteExt};
use pumpkin_protocol::{PacketDecodeError, ServerPacket, VarInt};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{
        TcpListener, TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{mpsc, oneshot},
};

use crate::packet_capture::CapturedPacket;

/// The largest frame the proxy forwards, so a corrupt length cannot allocate the machine away.
const MAX_FRAME: usize = 8 * 1024 * 1024;
/// How long a client waits for the upstream to appear before the connection is dropped.
const DEFAULT_UPSTREAM_WAIT: Duration = Duration::from_secs(30);
/// How often the upstream connection is retried while a client waits for it.
const UPSTREAM_RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// Which way a frame went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dir {
    /// Client to server.
    ClientToServer,
    /// Server to client.
    ServerToClient,
}

impl Dir {
    /// The name the trace lines use.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ClientToServer => "c2s",
            Self::ServerToClient => "s2c",
        }
    }

    /// The direction of a trace line's `dir` field.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "c2s" => Some(Self::ClientToServer),
            "s2c" => Some(Self::ServerToClient),
            _ => None,
        }
    }

    /// The stage of a connection this direction's packets are read in.
    const fn initial_phase(self) -> Phase {
        match self {
            Self::ClientToServer | Self::ServerToClient => Phase::Login,
        }
    }
}

/// The `mctrace` proxy invocation.
#[derive(Clone, Debug)]
pub struct ProxyOptions {
    /// Where clients connect.
    pub listen: SocketAddr,
    /// The server to forward to. It may not be up yet: it is connected to lazily, on the first
    /// client frame, and retried.
    pub upstream: SocketAddr,
    /// Where the frames are written, one JSON line each.
    pub out: PathBuf,
}

/// What the proxy did, for the summary it prints on exit.
#[derive(Debug, Default)]
struct Counters {
    connections: AtomicU64,
    c2s_frames: AtomicU64,
    c2s_bytes: AtomicU64,
    s2c_frames: AtomicU64,
    s2c_bytes: AtomicU64,
    /// Frames a connection ended inside of, which are not recorded (the bytes are not a frame).
    truncated: AtomicU64,
}

impl Counters {
    fn record(&self, dir: Dir, len: usize) {
        let (frames, bytes) = match dir {
            Dir::ClientToServer => (&self.c2s_frames, &self.c2s_bytes),
            Dir::ServerToClient => (&self.s2c_frames, &self.s2c_bytes),
        };
        frames.fetch_add(1, Ordering::Relaxed);
        bytes.fetch_add(len as u64, Ordering::Relaxed);
    }

    fn truncated(&self) {
        self.truncated.fetch_add(1, Ordering::Relaxed);
    }

    /// `(frames, bytes)` of one direction.
    fn of(&self, dir: Dir) -> (u64, u64) {
        match dir {
            Dir::ClientToServer => (
                self.c2s_frames.load(Ordering::Relaxed),
                self.c2s_bytes.load(Ordering::Relaxed),
            ),
            Dir::ServerToClient => (
                self.s2c_frames.load(Ordering::Relaxed),
                self.s2c_bytes.load(Ordering::Relaxed),
            ),
        }
    }
}

/// Runs the proxy until the process is signalled, then prints a summary of the session.
///
/// # Errors
/// The listen address cannot be bound, the trace file cannot be written, or the upstream never
/// appeared for a client that needed it.
#[expect(
    clippy::print_stderr,
    reason = "a CLI: its progress lines and its summary belong on stderr"
)]
pub async fn run(options: ProxyOptions) -> Result<(), String> {
    let listener = TcpListener::bind(options.listen)
        .await
        .map_err(|err| format!("cannot listen on {}: {err}", options.listen))?;
    let bound = listener
        .local_addr()
        .map_err(|err| format!("cannot read the listening address: {err}"))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&options.out)
        .map_err(|err| format!("cannot open the trace file {}: {err}", options.out.display()))?;

    let start = Instant::now();
    let counters = Arc::new(Counters::default());
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    eprintln!(
        "mctrace: {} -> {} (upstream connected on the first client frame), trace {}",
        bound,
        options.upstream,
        options.out.display()
    );

    let writer = tokio::task::spawn_blocking(move || -> Result<(), String> {
        while let Some(line) = rx.blocking_recv() {
            std::io::Write::write_all(&mut file, line.as_bytes())
                .and_then(|()| std::io::Write::flush(&mut file))
                .map_err(|err| format!("cannot write to the trace file: {err}"))?;
        }
        Ok(())
    });

    let mut tasks = Vec::new();
    loop {
        tokio::select! {
            () = signalled() => break,
            accepted = listener.accept() => {
                let (client, peer) = accepted.map_err(|err| format!("cannot accept a client: {err}"))?;
                let index = u32::try_from(counters.connections.fetch_add(1, Ordering::Relaxed) + 1)
                    .unwrap_or(u32::MAX);
                let options = options.clone();
                let counters = Arc::clone(&counters);
                let tx = tx.clone();
                tasks.push(tokio::spawn(async move {
                    if let Err(err) = serve(index, client, &options, tx, start, counters).await {
                        eprintln!("mctrace: connection {index} ({peer}): {err}");
                    }
                }));
            }
        }
    }

    drop(tx);
    for task in tasks {
        let _ = task.await;
    }
    writer
        .await
        .map_err(|err| format!("the trace writer panicked: {err}"))??;

    let elapsed = start.elapsed();
    let (c2s_frames, c2s_bytes) = counters.of(Dir::ClientToServer);
    let (s2c_frames, s2c_bytes) = counters.of(Dir::ServerToClient);
    eprintln!(
        "mctrace: {} connection(s), {} frames, {} bytes in {} ms \
         (c2s {c2s_frames} frames/{c2s_bytes} bytes, s2c {s2c_frames} frames/{s2c_bytes} bytes), \
         {} truncated frame(s), trace {}",
        counters.connections.load(Ordering::Relaxed),
        c2s_frames + s2c_frames,
        c2s_bytes + s2c_bytes,
        elapsed.as_millis(),
        counters.truncated.load(Ordering::Relaxed),
        options.out.display()
    );
    Ok(())
}

/// Serves one client: the client's frames go upstream, the upstream's frames go to the client,
/// and both are recorded under `index`.
async fn serve(
    index: u32,
    client: TcpStream,
    options: &ProxyOptions,
    tx: mpsc::UnboundedSender<String>,
    start: Instant,
    counters: Arc<Counters>,
) -> Result<(), String> {
    let _ = client.set_nodelay(true);
    let (mut client_read, mut client_write) = client.into_split();

    // The upstream connection is only made once the client has sent something, so the proxy may
    // start before the server does. Its read half is handed to the task below.
    let (upstream_read_tx, upstream_read_rx) = oneshot::channel::<OwnedReadHalf>();
    let s2c_counters = Arc::clone(&counters);
    let s2c_tx = tx.clone();
    let pump = tokio::spawn(async move {
        let Ok(mut upstream_read) = upstream_read_rx.await else {
            return;
        };
        loop {
            match next_frame(
                &mut upstream_read,
                Dir::ServerToClient,
                index,
                start,
                &s2c_tx,
                &s2c_counters,
            )
            .await
            {
                Ok(Some(payload)) => {
                    if forward(&mut client_write, &payload).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        let _ = client_write.shutdown().await;
    });

    let mut upstream_read_tx = Some(upstream_read_tx);
    let mut upstream: Option<OwnedWriteHalf> = None;
    while let Some(payload) =
        next_frame(&mut client_read, Dir::ClientToServer, index, start, &tx, &counters).await?
    {
        if upstream.is_none() {
            let (read, write) = connect_upstream(index, options.upstream).await?;
            let sender = upstream_read_tx
                .take()
                .ok_or_else(|| "the upstream read half was already handed over".to_string())?;
            if sender.send(read).is_err() {
                return Err("the server-to-client task is gone".to_string());
            }
            upstream = Some(write);
        }
        if let Some(write) = upstream.as_mut() {
            forward(write, &payload).await?;
        }
    }
    if let Some(write) = upstream.as_mut() {
        let _ = write.shutdown().await;
    }
    pump.await
        .map_err(|err| format!("the server-to-client task failed: {err}"))
}

/// Connects to the upstream, retrying while a client waits for it.
#[expect(
    clippy::print_stderr,
    reason = "a CLI: the upstream being late is worth a line"
)]
async fn connect_upstream(
    index: u32,
    upstream: SocketAddr,
) -> Result<(OwnedReadHalf, OwnedWriteHalf), String> {
    let deadline = Instant::now() + DEFAULT_UPSTREAM_WAIT;
    let mut reported = false;
    let mut last;
    loop {
        match TcpStream::connect(upstream).await {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                if reported {
                    eprintln!("mctrace: connection {index}: upstream {upstream} is up");
                }
                return Ok(stream.into_split());
            }
            Err(err) => {
                last = err.to_string();
                if !reported {
                    eprintln!(
                        "mctrace: connection {index}: upstream {upstream} is not up yet ({err}); \
                         waiting up to {} s",
                        DEFAULT_UPSTREAM_WAIT.as_secs()
                    );
                    reported = true;
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(format!("the upstream {upstream} never came up ({last})"));
        }
        tokio::time::sleep(UPSTREAM_RETRY_INTERVAL).await;
    }
}

/// Writes one frame on: the `VarInt` length prefix, then the payload.
async fn forward<W: AsyncWrite + Unpin>(writer: &mut W, payload: &[u8]) -> Result<(), String> {
    writer
        .write_all(&length_prefix(payload.len()))
        .await
        .map_err(|err| format!("cannot forward a frame length: {err}"))?;
    writer
        .write_all(payload)
        .await
        .map_err(|err| format!("cannot forward a frame of {} bytes: {err}", payload.len()))?;
    writer
        .flush()
        .await
        .map_err(|err| format!("cannot flush a forwarded frame: {err}"))
}

/// The `VarInt` length prefix of a frame of `len` bytes.
fn length_prefix(len: usize) -> Vec<u8> {
    let mut value = len as u32;
    let mut prefix = Vec::with_capacity(5);
    loop {
        if value & !0x7F == 0 {
            prefix.push(value as u8);
            return prefix;
        }
        prefix.push(((value & 0x7F) | 0x80) as u8);
        value >>= 7;
    }
}

/// Reads the next frame of one direction, records it and returns its payload.
///
/// `Ok(None)` is the end of the stream. An error is a stream that ended inside a frame, which is
/// counted and not recorded: the bytes were never a frame.
async fn next_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    dir: Dir,
    index: u32,
    start: Instant,
    tx: &mpsc::UnboundedSender<String>,
    counters: &Counters,
) -> Result<Option<Vec<u8>>, String> {
    match read_frame(reader).await {
        Ok(Some(payload)) => {
            counters.record(dir, payload.len());
            let _ = tx.send(frame_line(index, dir, start, &payload));
            Ok(Some(payload))
        }
        Ok(None) => Ok(None),
        Err(err) => {
            counters.truncated();
            Err(format!("{} stream: {err}", dir.name()))
        }
    }
}

/// Reads one length-prefixed frame: the `VarInt` length, then that many bytes.
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>, String> {
    let mut length = 0usize;
    let mut shift = 0u32;
    let mut read = 0usize;
    loop {
        let mut byte = [0u8; 1];
        match reader.read_exact(&mut byte).await {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return eof(read),
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionReset => return eof(read),
            Err(err) => return Err(format!("cannot read a frame length: {err}")),
        }
        read += 1;
        length |= usize::from(byte[0] & 0x7F) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 28 {
            return Err("a frame length that is not a valid VarInt".to_string());
        }
    }
    if length > MAX_FRAME {
        return Err(format!("a frame of {length} bytes, over the {MAX_FRAME} byte cap"));
    }
    let mut payload = vec![0u8; length];
    match reader.read_exact(&mut payload).await {
        Ok(_) => Ok(Some(payload)),
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
            Err("the stream ended inside a frame".to_string())
        }
        Err(err) => Err(format!("cannot read a frame of {length} bytes: {err}")),
    }
}

/// The end of a stream, clean when no length byte of the next frame was read.
fn eof(read: usize) -> Result<Option<Vec<u8>>, String> {
    if read == 0 {
        Ok(None)
    } else {
        Err("the stream ended inside a frame length".to_string())
    }
}

/// One trace line: the frame length, the packet id when there is one, and the frame's bytes.
fn frame_line(index: u32, dir: Dir, start: Instant, payload: &[u8]) -> String {
    let id = match read_var_int(payload) {
        Some((id, _)) => id.to_string(),
        None => "null".to_string(),
    };
    let mut data = String::with_capacity(payload.len() * 2);
    for byte in payload {
        let _ = write!(data, "{byte:02x}");
    }
    format!(
        "{{\"c\":{index},\"dir\":\"{}\",\"t\":{},\"id\":{id},\"len\":{},\"data\":\"{data}\"}}\n",
        dir.name(),
        start.elapsed().as_millis(),
        payload.len()
    )
}

/// The `VarInt` at the start of `bytes` and how many bytes it took, `None` when there is not one.
fn read_var_int(bytes: &[u8]) -> Option<(i32, usize)> {
    let mut value = 0u32;
    for (index, byte) in bytes.iter().take(5).enumerate() {
        value |= u32::from(byte & 0x7F) << (7 * index as u32);
        if byte & 0x80 == 0 {
            return Some((value as i32, index + 1));
        }
    }
    None
}

/// Resolves when the process is asked to stop, so the summary is printed and the trace closed.
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

// ---------------------------------------------------------------------------
// Decoding a trace
// ---------------------------------------------------------------------------

/// The `mctrace decode` invocation: replay what a trace recorded into the decoders this
/// repository already has.
#[derive(Clone, Debug)]
pub struct DecodeOptions {
    /// The trace to decode.
    pub trace: PathBuf,
    /// Only this direction; both when absent.
    pub dir: Option<Dir>,
    /// Only this connection index; all of them when absent.
    pub connection: Option<u32>,
}

/// One recorded frame, read back from a trace line.
struct RecordedFrame {
    connection: u32,
    dir: Dir,
    /// Milliseconds since the proxy started, the trace's `t`.
    time: u64,
    /// The line index in the trace, which is the order the proxy saw the frames in: that is what
    /// says whether a client frame was sent before or after the server turned compression on.
    line: usize,
    payload: Vec<u8>,
}

/// The stage of a connection a frame was sent in, so an id is read in the right table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Login,
    Configuration,
    Play,
}

/// Decodes the frames of a trace and writes one line per packet.
///
/// Each connection is replayed through [`TCPNetworkDecoder`], which splits the frames again,
/// undoes the compression a server turned on with `set_compression` and hands over the same
/// `(id, payload)` the packet codecs read; the packets are then classified with
/// [`crate::packet_capture`].
///
/// Compression belongs to the whole connection, not to one direction: the server turns it on in
/// the login phase and from then on **both** directions use the smaller framing. The
/// server-to-client stream is decoded first to find where that happened, and the client's frames
/// recorded after it are decoded the same way, so a capture of a real client session decodes
/// completely.
///
/// # Errors
/// The trace cannot be read, a line is not a trace line, or a frame cannot be decoded.
pub async fn decode(options: &DecodeOptions, out: &mut impl std::io::Write) -> Result<(), String> {
    let text = std::fs::read_to_string(&options.trace)
        .map_err(|err| format!("cannot read the trace {}: {err}", options.trace.display()))?;
    let mut groups: BTreeMap<(u32, Dir), Vec<RecordedFrame>> = BTreeMap::new();
    for (number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let frame = parse_frame(line, number)
            .map_err(|err| format!("{}:{}: {err}", options.trace.display(), number + 1))?;
        if options
            .connection
            .is_some_and(|index| index != frame.connection)
        {
            continue;
        }
        groups
            .entry((frame.connection, frame.dir))
            .or_default()
            .push(frame);
    }

    let mut connections: Vec<u32> = groups.keys().map(|(connection, _)| *connection).collect();
    connections.dedup();
    let mut counts: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut decoded_groups = 0usize;
    for connection in connections {
        let s2c = groups
            .remove(&(connection, Dir::ServerToClient))
            .unwrap_or_default();
        let c2s = groups
            .remove(&(connection, Dir::ClientToServer))
            .unwrap_or_default();
        decoded_groups += usize::from(!s2c.is_empty()) + usize::from(!c2s.is_empty());

        let mut phase = Phase::Login;
        let compression = decode_frames(
            &s2c,
            Dir::ServerToClient,
            &mut phase,
            None,
            options.dir.is_none_or(|dir| dir == Dir::ServerToClient),
            out,
            &mut counts,
        )
        .await?;

        // The client's frames are in the plain framing until the compression packet crossed the
        // connection, and in the compressed one after it.
        let (plain, compressed, threshold) = match compression {
            Some((line, threshold)) => {
                let split = c2s
                    .iter()
                    .position(|frame| frame.line > line)
                    .unwrap_or(c2s.len());
                (&c2s[..split], &c2s[split..], Some(threshold))
            }
            None => (&c2s[..0], &c2s[..], None),
        };
        let mut phase = Phase::Login;
        let print = options.dir.is_none_or(|dir| dir == Dir::ClientToServer);
        decode_frames(plain, Dir::ClientToServer, &mut phase, None, print, out, &mut counts).await?;
        decode_frames(compressed, Dir::ClientToServer, &mut phase, threshold, print, out, &mut counts)
            .await?;
    }

    let mut summary = String::new();
    for (kind, count) in &counts {
        let _ = write!(summary, " {kind}={count}");
    }
    let _ = writeln!(
        out,
        "# {decoded_groups} frame group(s),{}",
        if summary.is_empty() {
            " nothing decoded".to_string()
        } else {
            summary
        }
    );
    Ok(())
}

/// Decodes one segment of one direction and writes its lines.
///
/// `threshold` is the compression already in effect for the segment. The return value is where the
/// server turned compression on, when this segment carried that packet: the line index of the
/// packet and the threshold, which is what the other direction of the same connection is decoded
/// with.
async fn decode_frames(
    frames: &[RecordedFrame],
    dir: Dir,
    phase: &mut Phase,
    threshold: Option<i32>,
    print: bool,
    out: &mut impl std::io::Write,
    counts: &mut BTreeMap<&'static str, u64>,
) -> Result<Option<(usize, i32)>, String> {
    let Some(connection) = frames.first().map(|frame| frame.connection) else {
        return Ok(None);
    };
    let mut stream = Vec::new();
    for frame in frames {
        let length = i32::try_from(frame.payload.len())
            .map_err(|_| format!("a frame of {} bytes", frame.payload.len()))?;
        stream
            .write_var_int(&VarInt(length))
            .map_err(|err| format!("cannot reframe a frame: {err:?}"))?;
        stream.extend_from_slice(&frame.payload);
    }
    let mut decoder = TCPNetworkDecoder::new(std::io::Cursor::new(stream));
    if let Some(threshold) = threshold.and_then(|threshold| usize::try_from(threshold).ok()) {
        decoder.set_compression(threshold);
    }

    let mut compression = None;
    let mut decoded = 0usize;
    loop {
        let packet = match decoder.get_raw_packet().await {
            Ok(packet) => packet,
            Err(PacketDecodeError::ConnectionClosed) => break,
            Err(err) => {
                return Err(format!(
                    "connection {connection} ({}): frame {decoded} of {} does not decode: {err}",
                    dir.name(),
                    frames.len()
                ));
            }
        };
        let frame = frames.get(decoded);
        decoded += 1;
        let captured = CapturedPacket {
            id: packet.id,
            payload: packet.payload,
        };
        let kind = classify(dir, phase, &captured, &mut decoder);
        if let Kind::Compression(value) = &kind
            && *value >= 0
        {
            compression = frame.map(|frame| (frame.line, *value));
        }
        if print {
            let time = frame.map_or(0, |frame| frame.time);
            let _ = writeln!(
                out,
                "{}",
                packet_line(connection, dir, time, &captured, &kind)
            );
        }
        *counts.entry(kind.name()).or_default() += 1;
    }
    Ok(compression)
}

/// What a decoded frame turned out to be.
enum Kind {
    /// A `LEVEL_CHUNK_WITH_LIGHT` column.
    Chunk(i32, i32),
    /// A `CUSTOM_PAYLOAD`, or a configuration payload (the same shape, another table).
    Payload { channel: String, data: Vec<u8> },
    /// An `ADD_ENTITY`.
    AddEntity { entity: i32, kind: i32 },
    /// A `FORGET_LEVEL_CHUNK`.
    UnloadChunk(i32, i32),
    /// A `SET_COMPRESSION`: the frames after it are compressed.
    Compression(i32),
    /// A bundle delimiter, which frames a bundle and carries nothing.
    BundleDelimiter,
    /// A packet this decoder does not describe.
    Other,
}

impl Kind {
    const fn name(&self) -> &'static str {
        match self {
            Self::Chunk(_, _) => "chunk",
            Self::Payload { .. } => "payload",
            Self::AddEntity { .. } => "add_entity",
            Self::UnloadChunk(_, _) => "unload_chunk",
            Self::Compression(_) => "set_compression",
            Self::BundleDelimiter => "bundle_delimiter",
            Self::Other => "other",
        }
    }
}

/// Classifies one decoded packet, and lets a `SET_COMPRESSION` arm the decoder.
fn classify(
    dir: Dir,
    phase: &mut Phase,
    packet: &CapturedPacket,
    decoder: &mut TCPNetworkDecoder<std::io::Cursor<Vec<u8>>>,
) -> Kind {
    if *phase == Phase::Login {
        if dir == Dir::ServerToClient && packet.id == LOGIN_COMPRESSION.to_id(CURRENT_MC_VERSION) {
            let mut slice = &packet.payload[..];
            if let Ok(compression) = CSetCompression::read(&mut slice, &CURRENT_MC_VERSION) {
                decoder.set_compression(usize::try_from(compression.threshold.0).unwrap_or(0));
                return Kind::Compression(compression.threshold.0);
            }
        }
        if dir == Dir::ServerToClient && packet.id == LOGIN_FINISHED.to_id(CURRENT_MC_VERSION) {
            *phase = Phase::Configuration;
        } else if dir == Dir::ClientToServer
            && packet.id == LOGIN_ACKNOWLEDGED.to_id(CURRENT_MC_VERSION)
        {
            *phase = Phase::Configuration;
        }
        return Kind::Other;
    }
    if *phase == Phase::Configuration {
        let payload_id = match dir {
            Dir::ClientToServer => SERVERBOUND_CONFIG_CUSTOM_PAYLOAD.to_id(CURRENT_MC_VERSION),
            Dir::ServerToClient => CONFIG_CUSTOM_PAYLOAD.to_id(CURRENT_MC_VERSION),
        };
        let finish = match dir {
            Dir::ClientToServer => SERVERBOUND_CONFIG_FINISH.to_id(CURRENT_MC_VERSION),
            Dir::ServerToClient => CONFIG_FINISH.to_id(CURRENT_MC_VERSION),
        };
        if packet.id == payload_id {
            if let Some(kind) = config_payload(packet) {
                return kind;
            }
        }
        if packet.id == finish {
            *phase = Phase::Play;
        }
        return Kind::Other;
    }
    if packet.is_bundle_delimiter() {
        return Kind::BundleDelimiter;
    }
    if let Ok((x, z)) = packet.chunk() {
        return Kind::Chunk(x, z);
    }
    if let Ok((x, z)) = packet.unload_chunk() {
        return Kind::UnloadChunk(x, z);
    }
    if let Ok(entity) = packet.add_entity() {
        return Kind::AddEntity {
            entity: entity.entity_id.0,
            kind: entity.r#type.0,
        };
    }
    if let Ok(payload) = packet.custom_payload() {
        return Kind::Payload {
            channel: payload.channel,
            data: payload.data,
        };
    }
    Kind::Other
}

/// The channel and bytes of a configuration payload, which is `string channel` then the data.
fn config_payload(packet: &CapturedPacket) -> Option<Kind> {
    let mut slice = &packet.payload[..];
    let channel = slice.get_str().ok()?.into_string();
    Some(Kind::Payload {
        channel,
        data: slice.to_vec(),
    })
}

/// One decoded line, in the shape of the client trace (`crate::client_trace`).
fn packet_line(connection: u32, dir: Dir, time: u64, packet: &CapturedPacket, kind: &Kind) -> String {
    let mut line = format!(
        "{{\"c\":{connection},\"dir\":\"{}\",\"t\":{time},\"id\":{},\"len\":{},\"kind\":\"{}\"",
        dir.name(),
        packet.id,
        packet.payload.len(),
        kind.name()
    );
    match kind {
        Kind::Chunk(x, z) | Kind::UnloadChunk(x, z) => {
            let _ = write!(line, ",\"x\":{x},\"z\":{z}");
        }
        Kind::Payload { channel, data } => {
            let mut hex = String::with_capacity(data.len() * 2);
            for byte in data {
                let _ = write!(hex, "{byte:02x}");
            }
            line.push_str(",\"channel\":\"");
            for character in channel.chars() {
                match character {
                    '"' => line.push_str("\\\""),
                    '\\' => line.push_str("\\\\"),
                    character if character.is_ascii_graphic() => line.push(character),
                    _ => {
                        let _ = write!(line, "\\u{:04x}", u32::from(character));
                    }
                }
            }
            let _ = write!(line, "\",\"data\":\"{hex}\"");
        }
        Kind::AddEntity { entity, kind } => {
            let _ = write!(line, ",\"entity\":{entity},\"type\":{kind}");
        }
        Kind::Compression(threshold) => {
            let _ = write!(line, ",\"threshold\":{threshold}");
        }
        Kind::BundleDelimiter | Kind::Other => {}
    }
    line.push_str("}\n");
    line
}

/// Reads one trace line back into a frame.
fn parse_frame(line: &str, number: usize) -> Result<RecordedFrame, String> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|err| format!("not a trace line: {err}"))?;
    let connection = value
        .get("c")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "a trace line without a connection index".to_string())?;
    let connection =
        u32::try_from(connection).map_err(|_| format!("connection index {connection}"))?;
    let dir = value
        .get("dir")
        .and_then(serde_json::Value::as_str)
        .and_then(Dir::from_name)
        .ok_or_else(|| "a trace line without a direction".to_string())?;
    let time = value
        .get("t")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "a trace line without data".to_string())?;
    let body = decode_hex(data)?;
    Ok(RecordedFrame {
        connection,
        dir,
        time,
        line: number,
        payload: body,
    })
}

/// The bytes of a lowercase hex string.
fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err(format!("{} hex characters are not a byte string", text.len()));
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let digits = text.as_bytes();
    for pair in digits.chunks(2) {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        bytes.push(high << 4 | low);
    }
    Ok(bytes)
}

/// The value of one hex digit.
fn hex_digit(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(format!("'{}' is not a hex digit", char::from(byte))),
    }
}

