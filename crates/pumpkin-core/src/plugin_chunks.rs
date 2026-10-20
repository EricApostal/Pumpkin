//! Chunks that plugins keep loaded.
//!
//! The host loads a chunk while a player watches it, while the `forceload` command keeps it, or
//! while a plugin holds a ticket (see the `chunk-tickets` interface of the plugin API). A plugin
//! that owns blocks far away from every player — a plot yard, a copy of a structure at a spare
//! position, a machine room outside the map — needs a ticket for those chunks: without one they
//! unload, their block entities stop ticking and the block reads return air.
//!
//! This module is also where a plugin sends a single chunk to a player at a position the host does
//! not send by view distance (`chunk-streaming`), reusing the encoding and batching of the host's
//! own chunk path.
//!
//! # HOST GAP
//! A ticketed chunk is loaded and ticked, but it is not added to any player's view. The plugin has
//! to send it with [`send_chunk_at`] and remove it with [`unload_chunk_at`]; the host does not
//! follow a player into the area and does not unload the chunk when the last player leaves.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};

use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector2::Vector2;
use pumpkin_world::chunk_system::ChunkLoading;

use crate::entity::player::Player;
use crate::world::World;

/// One ticket: a chunk that a plugin keeps loaded.
struct Ticket<W> {
    owner: String,
    world: W,
    pos: Vector2<i32>,
}

/// Bookkeeping of the chunk tickets of plugins.
///
/// Generic over the world handle, so the bookkeeping is testable without a running server.
pub struct ChunkTicketRegistry<W> {
    next_id: u32,
    tickets: HashMap<u32, Ticket<W>>,
}

impl<W> Default for ChunkTicketRegistry<W> {
    fn default() -> Self {
        Self::new()
    }
}

impl<W> ChunkTicketRegistry<W> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: 0,
            tickets: HashMap::new(),
        }
    }

    /// Stores a ticket and returns its handle. A handle is never 0 and never reused while held.
    #[must_use]
    pub fn insert(&mut self, owner: &str, world: W, pos: Vector2<i32>) -> u32 {
        let id = self.allocate_id();
        self.tickets.insert(
            id,
            Ticket {
                owner: owner.to_string(),
                world,
                pos,
            },
        );
        id
    }

    fn allocate_id(&mut self) -> u32 {
        loop {
            self.next_id = self.next_id.wrapping_add(1);
            if self.next_id != 0 && !self.tickets.contains_key(&self.next_id) {
                return self.next_id;
            }
        }
    }

    /// Removes the ticket `id` when `owner` holds it.
    ///
    /// A handle of another plugin is left alone and reported as unknown, so a plugin cannot release
    /// a chunk that another plugin keeps.
    #[must_use]
    pub fn remove(&mut self, owner: &str, id: u32) -> Option<(W, Vector2<i32>)> {
        if self.tickets.get(&id)?.owner != owner {
            return None;
        }
        let ticket = self.tickets.remove(&id)?;
        Some((ticket.world, ticket.pos))
    }

    #[must_use]
    pub fn contains(&self, owner: &str, id: u32) -> bool {
        self.tickets
            .get(&id)
            .is_some_and(|ticket| ticket.owner == owner)
    }

    /// Removes every ticket of `owner` and returns them.
    #[must_use]
    pub fn remove_owner(&mut self, owner: &str) -> Vec<(W, Vector2<i32>)> {
        let ids: Vec<u32> = self
            .tickets
            .iter()
            .filter(|(_, ticket)| ticket.owner == owner)
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .filter_map(|id| self.tickets.remove(&id))
            .map(|ticket| (ticket.world, ticket.pos))
            .collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tickets.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tickets.is_empty()
    }
}

/// The tickets of every plugin, by handle. The world handle is weak: a world that unloads takes
/// its tickets with it, and a release that arrives afterwards does nothing.
static REGISTRY: LazyLock<Mutex<ChunkTicketRegistry<Weak<World>>>> =
    LazyLock::new(|| Mutex::new(ChunkTicketRegistry::new()));

fn registry() -> std::sync::MutexGuard<'static, ChunkTicketRegistry<Weak<World>>> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Force-loads `pos` for `owner` and returns the ticket handle.
///
/// The chunk stays loaded and ticked until [`release_ticket`] or [`release_owner`].
#[must_use]
pub fn add_force_ticket(world: &Arc<World>, owner: &str, pos: Vector2<i32>) -> u32 {
    let ticket = registry().insert(owner, Arc::downgrade(world), pos);
    force_load(world, pos);
    ticket
}

/// Releases one ticket of `owner`. Returns false when `owner` does not hold `ticket`.
#[must_use]
pub fn release_ticket(owner: &str, ticket: u32) -> bool {
    let Some((world, pos)) = registry().remove(owner, ticket) else {
        return false;
    };
    if let Some(world) = world.upgrade() {
        force_unload(&world, pos);
    }
    true
}

/// Releases every ticket of `owner`, for example when the plugin unloads.
pub fn release_owner(owner: &str) {
    let tickets = registry().remove_owner(owner);
    for (world, pos) in tickets {
        if let Some(world) = world.upgrade() {
            force_unload(&world, pos);
        }
    }
}

/// Releases every ticket that points at `world`, for example when the world unloads.
///
/// The chunks are gone with the world, so only the bookkeeping is dropped.
pub fn release_world(world: &World) {
    let uuid = world.uuid;
    registry().tickets.retain(|_, ticket| {
        ticket
            .world
            .upgrade()
            .is_some_and(|held| held.uuid != uuid)
    });
}

/// Returns whether `owner` holds `ticket`.
#[must_use]
pub fn owns_ticket(owner: &str, ticket: u32) -> bool {
    registry().contains(owner, ticket)
}

/// Whether a chunk stays forced (loaded and ticked) after one force ticket was removed.
///
/// Another force ticket keeps it: another plugin's, or the one of the `forceload` command, which
/// uses the same level. Only when no force ticket is left does the chunk lose its forced flag.
fn has_other_force_ticket(levels: Option<&[i8]>) -> bool {
    levels.is_some_and(|levels| levels.contains(&ChunkLoading::FULL_CHUNK_LEVEL))
}

/// Takes the force ticket for `pos` on the chunk loading of a level.
///
/// The ticket puts the chunk at [`ChunkLoading::FULL_CHUNK_LEVEL`] and at the front of the
/// priority list, so the chunk system generates it before the chunks of a view distance even when
/// no player is near it.
fn take_force_ticket(loading: &mut ChunkLoading, pos: Vector2<i32>) {
    loading.add_force_ticket(pos);
    loading.send_change();
}

/// Drops the force ticket for `pos` and returns whether another force ticket still keeps it.
fn drop_force_ticket(loading: &mut ChunkLoading, pos: Vector2<i32>) -> bool {
    loading.remove_force_ticket(pos);
    let still_forced = has_other_force_ticket(loading.ticket.get(&pos).map(Vec::as_slice));
    loading.send_change();
    still_forced
}

fn force_load(world: &World, pos: Vector2<i32>) {
    let mut loading = world
        .level
        .chunk_loading
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    take_force_ticket(&mut loading, pos);
    drop(loading);
    // The forced set makes the chunk active: block entities and random ticks run there.
    world
        .forced_chunks
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(pos);
    world.update_active_chunks();
}

fn force_unload(world: &World, pos: Vector2<i32>) {
    let still_forced = {
        let mut loading = world
            .level
            .chunk_loading
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        drop_force_ticket(&mut loading, pos)
    };
    if still_forced {
        return;
    }
    world
        .forced_chunks
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&pos);
    world.update_active_chunks();
}

/// Sends the chunk that contains `pos` to `player` as a normal chunk packet.
///
/// The chunk has to be loaded in the world of the player. The player's chunk sender records the
/// chunk as sent, so [`unload_chunk_at`] removes it again and the view distance handling of the
/// host does not send it a second time.
///
/// # Errors
/// Fails when the chunk is not loaded, which is also the case when nobody keeps it loaded.
pub async fn send_chunk_at(player: &Arc<Player>, pos: BlockPos) -> Result<(), String> {
    if player.client.closed() {
        return Err("the player is not connected".to_string());
    }
    let chunk_pos = pos.chunk_position();
    let world = player.world();
    let Some(chunk) = world
        .level
        .loaded_chunks
        .get(&chunk_pos)
        .map(|entry| entry.value().clone())
    else {
        return Err(format!(
            "chunk {}, {} is not loaded in this world",
            chunk_pos.x, chunk_pos.y
        ));
    };

    mark_sent(player, chunk_pos);

    // The host's own path: the chunk data packet with light, wrapped in a chunk batch.
    player.client.send_chunks(&[chunk]).await;
    Ok(())
}

/// Records a chunk as held by `player`, so the view distance handling does not send it again.
///
/// A separate function, so the sender lock is not held across the await of the packet send.
fn mark_sent(player: &Player, chunk_pos: Vector2<i32>) {
    let mut sender = player
        .chunk_sender
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    sender.mark_sent_out_of_band(chunk_pos);
}

/// Tells `player` to drop the chunk that contains `pos`. Does nothing when it was never sent.
pub fn unload_chunk_at(player: &Player, pos: BlockPos) {
    let chunk_pos = pos.chunk_position();
    let mut sender = player
        .chunk_sender
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    sender.unload_chunk(&player.client, chunk_pos);
}

#[cfg(test)]
mod tests {
    use super::{
        ChunkTicketRegistry, add_force_ticket, drop_force_ticket, has_other_force_ticket,
        release_ticket, take_force_ticket,
    };
    use crate::world::World;
    use pumpkin_util::math::vector2::Vector2;
    use pumpkin_world::chunk_system::{ChunkLoading, LevelChannel};
    use std::sync::{Arc, Weak};

    #[test]
    fn handles_are_unique_and_never_zero() {
        let mut registry = ChunkTicketRegistry::new();
        let first = registry.insert("a", 1u32, Vector2::new(0, 0));
        let second = registry.insert("a", 1u32, Vector2::new(0, 0));
        let third = registry.insert("b", 2u32, Vector2::new(5, 7));
        assert_ne!(first, 0);
        assert_ne!(second, 0);
        assert_ne!(third, 0);
        assert_ne!(first, second);
        assert_ne!(first, third);
        assert_ne!(second, third);
        assert_eq!(registry.len(), 3);
    }

    #[test]
    fn remove_returns_the_ticket_once() {
        let mut registry = ChunkTicketRegistry::new();
        let ticket = registry.insert("a", 7u32, Vector2::new(3, 4));
        assert!(registry.contains("a", ticket));
        assert_eq!(registry.remove("a", ticket), Some((7, Vector2::new(3, 4))));
        assert!(!registry.contains("a", ticket));
        assert_eq!(registry.remove("a", ticket), None);
        assert!(registry.is_empty());
    }

    #[test]
    fn a_plugin_cannot_release_a_ticket_of_another_plugin() {
        let mut registry = ChunkTicketRegistry::new();
        let ticket = registry.insert("a", 1u32, Vector2::new(0, 0));
        assert_eq!(registry.remove("b", ticket), None);
        assert!(registry.contains("a", ticket));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn remove_owner_only_takes_the_tickets_of_that_plugin() {
        let mut registry = ChunkTicketRegistry::new();
        let a1 = registry.insert("a", 1u32, Vector2::new(0, 0));
        let a2 = registry.insert("a", 2u32, Vector2::new(1, 0));
        let b1 = registry.insert("b", 1u32, Vector2::new(0, 0));

        let mut removed = registry.remove_owner("a");
        removed.sort_by_key(|(world, pos)| (*world, pos.x, pos.y));
        assert_eq!(
            removed,
            vec![(1, Vector2::new(0, 0)), (2, Vector2::new(1, 0))]
        );
        assert!(!registry.contains("a", a1));
        assert!(!registry.contains("a", a2));
        assert!(registry.contains("b", b1));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn two_tickets_for_one_chunk_are_independent() {
        let mut registry = ChunkTicketRegistry::new();
        let first = registry.insert("a", 1u32, Vector2::new(9, 9));
        let second = registry.insert("b", 1u32, Vector2::new(9, 9));
        assert_eq!(registry.len(), 2);
        assert!(registry.remove("a", first).is_some());
        assert!(registry.contains("b", second));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn a_chunk_stays_forced_while_a_force_ticket_is_left() {
        let full = ChunkLoading::FULL_CHUNK_LEVEL;
        let view = ChunkLoading::FULL_CHUNK_LEVEL - 10;
        assert!(has_other_force_ticket(Some(&[full])));
        assert!(has_other_force_ticket(Some(&[view, full])));
        assert!(!has_other_force_ticket(Some(&[view])));
        assert!(!has_other_force_ticket(Some(&[])));
        assert!(!has_other_force_ticket(None));
    }

    fn chunk_loading() -> ChunkLoading {
        ChunkLoading::new(Arc::new(LevelChannel::new()))
    }

    #[test]
    fn a_ticket_puts_a_chunk_at_full_level_without_any_player() {
        let mut loading = chunk_loading();
        let pos = Vector2::new(625, 625);

        take_force_ticket(&mut loading, pos);

        assert_eq!(
            loading.ticket.get(&pos),
            Some(&vec![ChunkLoading::FULL_CHUNK_LEVEL])
        );
        assert_eq!(
            loading.pos_level.get(&pos),
            Some(&ChunkLoading::FULL_CHUNK_LEVEL)
        );
        // Priority: generated before the chunks of a view distance.
        assert!(loading.high_priority.contains(&pos));
    }

    #[test]
    fn dropping_a_ticket_releases_the_chunk() {
        let mut loading = chunk_loading();
        let pos = Vector2::new(625, 625);

        take_force_ticket(&mut loading, pos);
        let still_forced = drop_force_ticket(&mut loading, pos);

        assert!(!still_forced);
        assert!(!loading.ticket.contains_key(&pos));
        assert_ne!(
            loading.pos_level.get(&pos),
            Some(&ChunkLoading::FULL_CHUNK_LEVEL)
        );
        assert!(!loading.high_priority.contains(&pos));
    }

    #[test]
    fn a_second_ticket_keeps_the_chunk_after_the_first_is_dropped() {
        let mut loading = chunk_loading();
        let pos = Vector2::new(625, 625);

        take_force_ticket(&mut loading, pos);
        take_force_ticket(&mut loading, pos);
        // One ticket is left, so the chunk stays forced.
        assert!(drop_force_ticket(&mut loading, pos));
        assert_eq!(
            loading.pos_level.get(&pos),
            Some(&ChunkLoading::FULL_CHUNK_LEVEL)
        );

        assert!(!drop_force_ticket(&mut loading, pos));
    }

    /// A world with no server and no players: enough to check that a ticketed chunk serves block
    /// and block entity calls.
    fn test_world() -> Arc<World> {
        use pumpkin_config::world::LevelConfig;
        use crate::block::registry::BlockRegistry;
        use pumpkin_data::dimension::Dimension;
        use pumpkin_world::level::Level;
        use pumpkin_world::world_info::LevelData;

        let root = std::env::temp_dir().join(format!("pumpkin-ticket-{}", uuid::Uuid::new_v4()));
        let level = Level::from_root_folder(&LevelConfig::default(), root, 0, Dimension::OVERWORLD);
        let info = Arc::new(arc_swap::ArcSwap::from_pointee(LevelData::default(
            pumpkin_util::world_seed::Seed(0),
        )));
        Arc::new(World::load(
            level,
            info,
            Dimension::OVERWORLD,
            Arc::new(BlockRegistry::default()),
            Weak::<crate::server::Server>::new(),
        ))
    }

    /// The plugin ticket is what keeps a far chunk loaded and active without any player. This
    /// checks the world side of it: the ticket is registered on the chunk loading and in the forced
    /// set (the set block entity ticking uses), and the block and block entity calls read and write
    /// the chunk that is loaded there.
    #[tokio::test]
    async fn a_ticketed_chunk_serves_blocks_and_block_entities_without_a_player() {
        use pumpkin_world::world::BlockFlags;
        use pumpkin_data::Block;
        use pumpkin_util::math::position::BlockPos;

        let world = test_world();
        let chunk_pos = Vector2::new(625, 625);
        let ticket = add_force_ticket(&world, "test-plugin", chunk_pos);

        // The chunk system does not run in this test, so place the chunk like a loaded one.
        world
            .level
            .loaded_chunks
            .insert(chunk_pos, pumpkin_world::chunk::ChunkData::empty_sync(625, 625));

        {
            let loading = world
                .level
                .chunk_loading
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(
                loading.pos_level.get(&chunk_pos),
                Some(&ChunkLoading::FULL_CHUNK_LEVEL)
            );
        }
        assert!(
            world
                .forced_chunks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&chunk_pos),
            "the forced set is what makes block entities of the chunk tick"
        );

        let pos = BlockPos::new(625 * 16 + 3, 64, 625 * 16 + 4);
        let stone = Block::STONE.default_state.id;
        world.set_block_state(&pos, stone, BlockFlags::NOTIFY_ALL);
        assert_eq!(world.get_block_state_id(&pos), stone);

        let entity = Arc::new(crate::block::entities::test_block::TestBlockBlockEntity::new(pos));
        world.add_block_entity(entity);
        assert!(
            world.get_block_entity(&pos).is_some(),
            "a block entity of a ticketed chunk is reachable"
        );

        assert!(release_ticket("test-plugin", ticket));
        assert!(
            !world
                .forced_chunks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&chunk_pos)
        );
    }
}
