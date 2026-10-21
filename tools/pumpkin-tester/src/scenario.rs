//! Scripted scenarios: build plugin state headlessly, then let the server settle.
//!
//! A scenario dispatches `/…` commands at the **console** and ticks the server, so a test can
//! produce a curve, a contraption or a machine without a client. The console is the only sender
//! that can drive the plugin's own commands with a world handle; a bot cannot, because
//! `setblock` refuses a plugin block *state* with properties, `fill` creates no block entities,
//! and a placement needs the plugin's own item logic.
//!
//! Dispatch goes through the exact entry the console and command blocks use
//! (`pumpkin-core/src/lib.rs:794`, `pumpkin-core/src/block/blocks/signs.rs:725`), so a scenario
//! exercises the same path a real server operator would.

use std::sync::Arc;

use pumpkin_core::{command::CommandSender, server::Server};
use pumpkin_util::math::position::BlockPos;

/// A scripted setup: commands to run, positions to watch, and how long to settle afterwards.
pub struct Scenario {
    pub name: String,
    /// Console commands, dispatched in order.
    pub setup_commands: Vec<String>,
    /// Positions the test intends to assert on. Recorded so a failure can name them.
    pub probes: Vec<BlockPos>,
    /// Server ticks to run after the commands, so async block writes and packet flushes land.
    pub settle_ticks: usize,
}

impl Scenario {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            setup_commands: Vec::new(),
            probes: Vec::new(),
            // Long enough for a track placement to drain its `set_block_state` backlog and for
            // `flush_pending_block_updates` to run, which is the window the client relies on.
            settle_ticks: 40,
        }
    }

    #[must_use]
    pub fn command(mut self, command: impl Into<String>) -> Self {
        self.setup_commands.push(command.into());
        self
    }

    #[must_use]
    pub fn probe(mut self, pos: BlockPos) -> Self {
        self.probes.push(pos);
        self
    }

    #[must_use]
    pub fn settle(mut self, ticks: usize) -> Self {
        self.settle_ticks = ticks;
        self
    }

    /// Dispatches every command, then ticks the server `settle_ticks` times.
    pub fn run(&self, server: &Arc<Server>) {
        for command in &self.setup_commands {
            server.command_dispatcher.load().handle_command(
                &CommandSender::Console.into_source(server),
                command.as_str(),
            );
        }
        for _ in 0..self.settle_ticks {
            server.tick();
        }
    }

    /// Dispatches every command, then ticks `ticks` times.
    ///
    /// Use this when a test needs to sample *during* the settle window — for example to prove
    /// that something is pushed to a client promptly rather than only when a chunk is re-sent.
    pub fn run_with_ticks(&self, server: &Arc<Server>, ticks: usize) {
        for command in &self.setup_commands {
            server.command_dispatcher.load().handle_command(
                &CommandSender::Console.into_source(server),
                command.as_str(),
            );
        }
        for _ in 0..ticks {
            server.tick();
        }
    }
}