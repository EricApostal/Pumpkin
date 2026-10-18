use std::sync::Arc;

use pumpkin_macros::Event;

use crate::entity::player::Player;
use crate::plugin::api::events::player::PlayerEvent;

/// Fired when a player riding a plugin entity changes their input (the client sends it on every
/// change). Informational, the plugin moves the vehicle itself.
#[derive(Event, Clone)]
pub struct PluginVehicleInputEvent {
    /// The ID of the plugin entity the player rides.
    pub vehicle_id: i32,
    /// The rider.
    pub player: Arc<Player>,
    /// 1 when moving forward, -1 backward, 0 otherwise.
    pub forward: f32,
    /// 1 when moving left, -1 right, 0 otherwise.
    pub sideways: f32,
    pub jump: bool,
    pub sneak: bool,
    pub sprint: bool,
}

impl PluginVehicleInputEvent {
    #[must_use]
    pub const fn new(
        vehicle_id: i32,
        player: Arc<Player>,
        forward: f32,
        sideways: f32,
        jump: bool,
        sneak: bool,
        sprint: bool,
    ) -> Self {
        Self {
            vehicle_id,
            player,
            forward,
            sideways,
            jump,
            sneak,
            sprint,
        }
    }
}

impl PlayerEvent for PluginVehicleInputEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}
