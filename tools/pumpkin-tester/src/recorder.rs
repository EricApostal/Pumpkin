//! Background packet reader for the client-visibility tests.
//!
//! Reading in a task rather than with a per-packet timeout is deliberate: `get_raw_packet` reads
//! a length-prefixed frame, and cancelling it part-way consumes some of that prefix, which
//! desynchronises every later read. A recorder owns the read half and never cancels, so a test
//! can tick the server, wait, and then inspect everything that arrived.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use pumpkin_protocol::java::{packet_decoder::TCPNetworkDecoder, packet_encoder::TCPNetworkEncoder};
use tokio::{
    io::{BufReader, BufWriter},
    net::tcp::{OwnedReadHalf, OwnedWriteHalf},
    task::JoinHandle,
};

use crate::{
    bot::{HeadlessBot, MIRRORING_ATTEMPTS},
    packet_capture::PacketCapture,
};

/// A joined bot whose read half is being recorded.
///
/// The write half is deliberately held: dropping it closes the socket, and the server would then
/// see the client leave — which is the one thing the relogin diff distinguishes.
pub struct ConnectedBot {
    /// The write half, kept alive for the lifetime of the connection: dropping it closes the
    /// socket, and the server would then see the client leave. Also used to answer the server's
    /// keep-alives.
    writer: TCPNetworkEncoder<BufWriter<OwnedWriteHalf>>,
    recorder: PacketRecorder,
    neoforge: bool,
}

impl ConnectedBot {
    /// Joins as a vanilla client and starts recording everything the server sends.
    pub async fn join(addr: std::net::SocketAddr, name: &str) -> Result<Self, String> {
        Self::from_bot(HeadlessBot::connect_and_join(addr, name).await?)
    }

    /// Joins as a **NeoForge** client, so the server sends the NeoForge payloads — the
    /// `advanced_add_entity` spawn data and the frozen-registry sync.
    ///
    /// The handshake answer is derived from the in-process plugin declaration, so this only works
    /// against a server booted with the plugin loaded
    /// ([`crate::harness::create_test_server_with_plugin`]).
    pub async fn join_neoforge(addr: std::net::SocketAddr, name: &str) -> Result<Self, String> {
        Self::from_bot(HeadlessBot::connect_and_join_neoforge(addr, name).await?)
    }

    /// Joins by **mirroring** the handshake from what the server says and from what the server's
    /// own refusals reveal, so a server whose `NeoForge` declaration this process cannot read (a
    /// `Java` server that runs its own mods, out of process) can be joined too.
    pub async fn join_neoforge_mirroring(
        addr: std::net::SocketAddr,
        name: &str,
    ) -> Result<Self, String> {
        Self::from_bot(
            HeadlessBot::connect_and_join_neoforge_mirroring(addr, name, MIRRORING_ATTEMPTS).await?,
        )
    }

    fn from_bot(bot: HeadlessBot) -> Result<Self, String> {
        let neoforge = bot.neoforge;
        let HeadlessBot { reader, writer, .. } = bot;
        Ok(Self {
            recorder: PacketRecorder::spawn(reader),
            writer,
            neoforge,
        })
    }

    #[must_use]
    pub fn recorder(&self) -> &PacketRecorder {
        &self.recorder
    }

    /// The write half, so a caller can drive the client (answer keep-alives, send commands)
    /// while the read half is being recorded.
    pub fn writer(&mut self) -> &mut TCPNetworkEncoder<BufWriter<OwnedWriteHalf>> {
        &mut self.writer
    }

    /// Whether the connection negotiated as a NeoForge client.
    #[must_use]
    pub fn is_neoforge(&self) -> bool {
        self.neoforge
    }
}

/// Reads every packet the socket receives into a shared [`PacketCapture`].
pub struct PacketRecorder {
    capture: Arc<Mutex<PacketCapture>>,
    reader: JoinHandle<()>,
}

impl PacketRecorder {
    /// Takes ownership of a socket's read half and starts recording.
    #[must_use]
    pub fn spawn(mut reader: TCPNetworkDecoder<BufReader<OwnedReadHalf>>) -> Self {
        let capture = Arc::new(Mutex::new(PacketCapture::default()));
        let sink = Arc::clone(&capture);
        let task = tokio::spawn(async move {
            loop {
                match reader.get_raw_packet().await {
                    Ok(packet) => {
                        if let Ok(mut sink) = sink.lock() {
                            sink.push(packet);
                        }
                    }
                    // The stream ended, or a frame would not decode: nothing more will arrive.
                    Err(_) => break,
                }
            }
        });
        Self {
            capture,
            reader: task,
        }
    }

    /// The shared capture. Lock it to read what has arrived so far.
    #[must_use]
    pub fn capture(&self) -> Arc<Mutex<PacketCapture>> {
        Arc::clone(&self.capture)
    }

    /// How many packets have been recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.capture.lock().map_or(0, |capture| capture.len())
    }

    /// Waits until at least `count` packets have arrived, up to `limit`.
    ///
    /// Returns whether the count was reached — a false result is a useful failure signal in
    /// itself ("the server never sent it") rather than a hang.
    pub async fn wait_for(&self, count: usize, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if self.len() >= count {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        self.len() >= count
    }

    /// Waits a fixed grace period, so a test can tick the server and then let the socket catch up.
    pub async fn settle(&self, grace: Duration) {
        tokio::time::sleep(grace).await;
    }

    /// Stops recording.
    pub fn stop(&self) {
        self.reader.abort();
    }
}