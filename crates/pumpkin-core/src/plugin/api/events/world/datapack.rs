//! Events of datapack reloads and of the datapack dependent data sent to players.

use std::sync::Arc;

use pumpkin_macros::Event;

use crate::entity::player::Player;
use crate::plugin::api::events::player::PlayerEvent;

/// Fired after the datapacks were reloaded and the recipes and tags are replaced, for plugins that
/// build lookup tables from them.
#[derive(Event, Clone)]
pub struct DatapackReloadEvent {
    /// How many dynamic recipes (datapack and plugin registered ones) are loaded.
    pub recipe_count: u32,
    /// How many runtime tags (plugin and datapack tags) exist.
    pub tag_count: u32,
}

impl DatapackReloadEvent {
    #[must_use]
    pub const fn new(recipe_count: u32, tag_count: u32) -> Self {
        Self {
            recipe_count,
            tag_count,
        }
    }
}

/// Fired after the tags changed (a datapack reload). Informational.
#[derive(Event, Clone)]
pub struct TagsUpdatedEvent {
    /// How many runtime tags exist.
    pub tag_count: u32,
}

impl TagsUpdatedEvent {
    #[must_use]
    pub const fn new(tag_count: u32) -> Self {
        Self { tag_count }
    }
}

/// Fired when datapack dependent data (recipes) was sent to a player, when the player joins and
/// after a datapack reload. Informational.
#[derive(Event, Clone)]
pub struct DatapackSyncEvent {
    /// The player the data was sent to.
    pub player: Arc<Player>,
    /// Whether this is a reload and not the join.
    pub reload: bool,
}

impl DatapackSyncEvent {
    #[must_use]
    pub const fn new(player: Arc<Player>, reload: bool) -> Self {
        Self { player, reload }
    }
}

impl PlayerEvent for DatapackSyncEvent {
    fn get_player(&self) -> &Arc<Player> {
        &self.player
    }
}
