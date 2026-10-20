//! The `chunk-tickets` interface: chunks that a plugin keeps loaded.
//!
//! The ticket bookkeeping and the world side live in [`pumpkin_core::plugin_chunks`]. This module
//! only maps the interface onto it and owns the per-plugin name that keeps the tickets of different
//! plugins apart.

use pumpkin_core::plugin_chunks;
use pumpkin_util::math::vector2::Vector2;
use pumpkin_wasm_host_common::state::PluginHostState;
use wasmtime::component::{Accessor, HasSelf, Resource};

use crate::pumpkin::plugin::chunk_tickets::{Host, HostWithStore};
use crate::pumpkin::plugin::world::World;
use crate::{AccessorExt, run_blocking};

/// How long `force-load-chunk` waits for a chunk before it gives up and releases the ticket.
///
/// Chunk generation of a plot yard or a fresh area can take a while, but a call that never returns
/// would hold the guest forever, so the ticket is released and the plugin gets an error instead.
const LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The name the host records for the calling plugin.
///
/// Tickets and collision boxes are kept per plugin, so a plugin can only release its own tickets.
/// The name is the plugin metadata name, set by the runtime before `on-load`.
pub(crate) fn plugin_owner(state: &PluginHostState) -> String {
    state
        .name
        .clone()
        .unwrap_or_else(|| "unknown".to_string())
}

impl Host for PluginHostState {
    fn release_chunk_ticket(&mut self, ticket: u32) -> wasmtime::Result<bool> {
        let owner = plugin_owner(self);
        Ok(plugin_chunks::release_ticket(&owner, ticket))
    }

    fn has_chunk_ticket(&mut self, ticket: u32) -> wasmtime::Result<bool> {
        let owner = plugin_owner(self);
        Ok(plugin_chunks::owns_ticket(&owner, ticket))
    }

    fn release_all_chunk_tickets(&mut self) -> wasmtime::Result<()> {
        plugin_chunks::release_owner(&plugin_owner(self));
        Ok(())
    }
}

impl HostWithStore<PluginHostState> for HasSelf<PluginHostState> {
    async fn force_load_chunk(
        accessor: &Accessor<PluginHostState, Self>,
        world: Resource<World>,
        x: i32,
        z: i32,
    ) -> wasmtime::Result<Result<u32, String>> {
        let world = accessor.get_res(&world)?;
        let owner = accessor.with(|mut host| Ok::<String, wasmtime::Error>(plugin_owner(host.get())))?;
        let pos = Vector2::new(x, z);

        let ticket = plugin_chunks::add_force_ticket(&world, &owner, pos);
        let level = world.level.clone();
        let loaded = tokio::time::timeout(
            LOAD_TIMEOUT,
            level.get_or_fetch_chunk(pos, |_| ()),
        )
        .await;

        match loaded {
            Ok(()) => {
                // The forced set makes the chunk active (block entities and random ticks) and
                // migrates the block entities of a chunk that was already loaded.
                let active_world = world.clone();
                run_blocking(accessor, move || active_world.update_active_chunks()).await?;
                Ok(Ok(ticket))
            }
            Err(_) => {
                let _ = plugin_chunks::release_ticket(&owner, ticket);
                Ok(Err(format!(
                    "timed out loading chunk {x}, {z}; is the chunk system running?"
                )))
            }
        }
    }
}
