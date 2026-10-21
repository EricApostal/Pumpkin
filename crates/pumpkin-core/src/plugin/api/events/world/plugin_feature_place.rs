use crate::world::World;
use pumpkin_macros::Event;
use pumpkin_util::math::{position::BlockPos, vector2::Vector2};
use pumpkin_world::ProtoChunk;
use std::sync::{Arc, Mutex};

/// An event that occurs during the feature step of chunk generation for every plugin feature
/// that a placement rule selected for the chunk.
///
/// It fires on a chunk generation worker thread, so handlers must be blocking and cheap. The
/// writable chunk is only the chunk being generated, blocks outside it cannot be reached.
///
/// It dispatches in [`DispatchClass::Background`](crate::plugin::DispatchClass::Background): the
/// only thing a generation worker wants from the plugin is throughput, so this event never delays
/// a dispatch the server's own timeline waits on (a tick, a player action), see
/// `Payload::dispatch_class`. Every generation worker blocks here until its answer arrives, which
/// is what makes that queue the one place where a slow plugin could otherwise stall the tick once
/// the workers pile up on the single guest.
#[derive(Event, Clone)]
#[dispatch_class(background)]
pub struct PluginFeaturePlaceEvent {
    /// The world that generates the chunk.
    pub world: Arc<World>,
    /// The chunk being generated.
    pub chunk_pos: Vector2<i32>,
    /// The key the feature was registered with.
    pub feature_key: String,
    /// Decorator seed of the placement rule for this chunk, for deterministic randomness.
    pub seed: u64,
    /// Vanilla generation step of the rule, `0..11`.
    pub step: u8,
    /// The origins that the placement rule chose, absolute block positions inside the chunk.
    pub positions: Vec<BlockPos>,
    /// The chunk under generation, moved out of the generator for the length of the event.
    pub chunk: Arc<Mutex<ProtoChunk>>,
}
