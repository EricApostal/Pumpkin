//! Placement of plugin features during chunk generation, see
//! `pumpkin_world::generation::feature::additive`.

use std::sync::{Arc, Mutex, PoisonError};

use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector2::Vector2;
use pumpkin_world::ProtoChunk;

use super::World;
use crate::plugin::api::events::world::plugin_feature_place::PluginFeaturePlaceEvent;

/// Gives the chunk back to the generator when dropped, also when a handler panicked.
struct Restore<'a> {
    target: &'a mut ProtoChunk,
    source: Arc<Mutex<ProtoChunk>>,
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        let mut source = self.source.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::swap(self.target, &mut source);
    }
}

/// Fires [`PluginFeaturePlaceEvent`] with the chunk as writable state. Blocks the generation
/// worker until the handlers are done.
pub fn place(
    world: &Arc<World>,
    feature_key: &str,
    chunk: &mut ProtoChunk,
    step: usize,
    seed: u64,
    positions: &[BlockPos],
) {
    let Some(server) = world.server.upgrade() else {
        return;
    };
    if !server
        .plugin_manager
        .has_handlers::<PluginFeaturePlaceEvent>()
    {
        return;
    }
    let chunk_pos = Vector2::new(chunk.x, chunk.z);
    let shell = chunk.empty_shell();
    let shared = Arc::new(Mutex::new(std::mem::replace(chunk, shell)));
    let _restore = Restore {
        target: chunk,
        source: Arc::clone(&shared),
    };
    let mut event = PluginFeaturePlaceEvent {
        world: Arc::clone(world),
        chunk_pos,
        feature_key: feature_key.to_string(),
        seed,
        step: step as u8,
        positions: positions.to_vec(),
        chunk: shared,
    };
    server.plugin_manager.fire_blocking(&server, &mut event);
}
