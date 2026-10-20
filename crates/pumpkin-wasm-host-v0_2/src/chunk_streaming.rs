//! The `chunk-streaming` interface: single chunks at arbitrary positions for one player.
//!
//! The encoding, light and batching are the host's own chunk path
//! ([`pumpkin_core::plugin_chunks::send_chunk_at`] uses the player's chunk sender and
//! `ClientPlatform::send_chunks`), so a plugin never builds a chunk packet itself.

use pumpkin_core::plugin_chunks;
use pumpkin_util::math::position::BlockPos;
use pumpkin_wasm_host_common::state::PluginHostState;
use wasmtime::component::{Accessor, HasSelf, Resource};

use crate::pumpkin::plugin::chunk_streaming::{Host, HostWithStore};
use crate::pumpkin::plugin::common::BlockPos as WitBlockPos;
use crate::pumpkin::plugin::player::Player as WitPlayer;
use crate::AccessorExt;

impl Host for PluginHostState {
    fn unload_chunk_at(
        &mut self,
        player: Resource<WitPlayer>,
        pos: WitBlockPos,
    ) -> wasmtime::Result<()> {
        let player = self.get(&player)?;
        plugin_chunks::unload_chunk_at(player, BlockPos::new(pos.x, pos.y, pos.z));
        Ok(())
    }
}

impl HostWithStore<PluginHostState> for HasSelf<PluginHostState> {
    async fn send_chunk_at(
        accessor: &Accessor<PluginHostState, Self>,
        player: Resource<WitPlayer>,
        pos: WitBlockPos,
    ) -> wasmtime::Result<Result<(), String>> {
        let player = accessor.get_res(&player)?;
        Ok(plugin_chunks::send_chunk_at(&player, BlockPos::new(pos.x, pos.y, pos.z)).await)
    }
}
