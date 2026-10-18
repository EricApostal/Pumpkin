use crate::world::World;
use pumpkin_macros::Event;
use std::sync::Arc;

/// An event that occurs once per tick for every world, after its entities and block entities
/// ticked.
#[derive(Event, Clone)]
pub struct WorldTickEvent {
    /// The world that ticked.
    pub world: Arc<World>,
    /// The tick of the server, `server.tick_count`.
    pub tick: i32,
}

impl WorldTickEvent {
    #[must_use]
    pub const fn new(world: Arc<World>, tick: i32) -> Self {
        Self { world, tick }
    }
}
