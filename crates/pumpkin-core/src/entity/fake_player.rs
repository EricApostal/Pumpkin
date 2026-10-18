//! Fake players: a [`Player`] that is not connected, for plugins that need an actor which uses
//! items, attacks and breaks blocks like a player (deployers, turtles, harvesters).
//!
//! A fake player has a [`JavaClient::detached`] client, so every packet sent to it is dropped. It
//! is not in the world's player list, so it is not in the tab list, not counted as online, not
//! ticked, not saved and no join or leave event is fired for it. Instead it is registered here, and
//! [`World::get_player_by_uuid`] finds it, which the code that fires player events relies on.
//!
//! The actions reuse the handlers of the client packets (`SUseItemOn`, `SUseItem`,
//! `SPlayerAction`, `SInteract`), so the events, the protection checks, the drops, the durability
//! and the enchantments are the ones of a connected player. The caller drives time: break progress
//! advances once per [`Player::fake_continue_break`] call.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use pumpkin_data::{Block, BlockDirection};
use pumpkin_inventory::player::player_screen_handler::PlayerScreenHandler;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::server::play::{
    SInteract, SPlayerAction, SUseItem, SUseItemOn, Status,
};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::permission::PermissionLvl;
use pumpkin_util::{GameMode, Hand};
use uuid::Uuid;

use crate::block;
use crate::entity::EntityBase;
use crate::entity::player::Player;
use crate::net::java::JavaClient;
use crate::net::{ClientPlatform, GameProfile, PlayerConfig, offline_uuid};
use crate::server::Server;
use crate::world::World;

/// Live fake players by uuid. Weak, so a fake player nobody holds any more disappears.
static REGISTRY: LazyLock<Mutex<HashMap<Uuid, Weak<Player>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Entries in [`REGISTRY`], so lookups cost nothing while there are no fake players.
static COUNT: AtomicUsize = AtomicUsize::new(0);

fn registry() -> std::sync::MutexGuard<'static, HashMap<Uuid, Weak<Player>>> {
    REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The fake player with this uuid that is in `world`.
pub(crate) fn find_in_world(world: &World, id: Uuid) -> Option<Arc<Player>> {
    if COUNT.load(Ordering::Relaxed) == 0 {
        return None;
    }
    let player = registry().get(&id)?.upgrade()?;
    let in_world = std::ptr::eq(Arc::as_ptr(&player.living_entity.entity.world.load()), world);
    in_world.then_some(player)
}

/// Where a block break stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FakeBreakProgress {
    /// There is nothing to break, it is out of reach, an event cancelled it or the held item
    /// cannot mine it. The fake player is not breaking anything afterwards.
    Rejected,
    /// Still breaking, with the progress so far (`1.0` and more breaks the block).
    InProgress(f32),
    /// The block was broken.
    Broken,
}

const fn hand_id(hand: Hand) -> i32 {
    match hand {
        Hand::Right => 0,
        Hand::Left => 1,
    }
}

impl Player {
    /// Creates a fake player in `world` at the world origin. The uuid is the offline uuid of the
    /// name unless given. Needs a Tokio runtime.
    ///
    /// # Errors
    /// The name is empty or too long, or a fake or online player already has the uuid.
    pub fn create_fake(
        world: &Arc<World>,
        name: &str,
        id: Option<Uuid>,
    ) -> Result<Arc<Self>, String> {
        if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
            return Err("the name of a fake player has 1 to 64 characters".to_string());
        }
        let server = world
            .server
            .upgrade()
            .ok_or_else(|| "the server is shutting down".to_string())?;
        let id = match id {
            Some(id) => id,
            None => offline_uuid(name).map_err(|err| err.to_string())?,
        };
        if server.get_player_by_uuid(id).is_some() {
            return Err(format!("the player {id} is already online"));
        }

        let profile = GameProfile {
            id,
            name: name.to_string(),
            properties: arc_swap::ArcSwap::new(Arc::new(Vec::new())),
            profile_actions: None,
        };
        let config = PlayerConfig::default();
        let client = Arc::new(ClientPlatform::Java(JavaClient::detached(
            profile.clone(),
            config.clone(),
        )));
        let player = Self::new_inner(client, profile, config, world, GameMode::Survival, true);
        player.permission_lvl.store(PermissionLvl::Zero);
        let player = Arc::new(player);

        let mut registry = registry();
        registry.retain(|_, fake| fake.strong_count() > 0);
        if registry.contains_key(&id) {
            return Err(format!("the fake player {id} already exists"));
        }
        registry.insert(id, Arc::downgrade(&player));
        COUNT.store(registry.len(), Ordering::Relaxed);
        Ok(player)
    }

    /// Stops what the fake player does and unregisters it. The `Arc`s that remain keep it valid
    /// but nothing finds it any more.
    ///
    /// # Errors
    /// This is not a fake player.
    pub fn remove_fake(&self) -> Result<(), String> {
        self.ensure_fake()?;
        self.stop_mining();
        self.close_fake_screen();
        {
            let mut registry = registry();
            if registry
                .get(&self.gameprofile.id)
                .is_some_and(|fake| std::ptr::eq(fake.as_ptr(), self))
            {
                registry.remove(&self.gameprofile.id);
            }
            COUNT.store(registry.len(), Ordering::Relaxed);
        }
        self.client.close();
        Ok(())
    }

    /// Moves the fake player, into `world` when given.
    ///
    /// # Errors
    /// This is not a fake player.
    pub fn set_fake_location(
        &self,
        world: Option<&Arc<World>>,
        pos: Vector3<f64>,
        yaw: f32,
        pitch: f32,
    ) -> Result<(), String> {
        self.ensure_fake()?;
        let entity = &self.living_entity.entity;
        if let Some(world) = world
            && !Arc::ptr_eq(&entity.world.load_full(), world)
        {
            self.stop_mining();
            entity.world.store(world.clone());
        }
        entity.set_pos(pos);
        entity.last_pos.store(pos);
        entity.set_rotation(yaw, pitch);
        Ok(())
    }

    /// Uses the item in `hand` on a block like a right click does: the block can act first (doors,
    /// chests), then the item (placing, buckets, hoes). Returns whether the clicked block, the block
    /// next to it or the held item changed.
    ///
    /// # Errors
    /// This is not a fake player, or the block is out of reach.
    pub fn fake_use_item_on_block(
        self: &Arc<Self>,
        pos: BlockPos,
        face: BlockDirection,
        hand: Hand,
        cursor: Vector3<f32>,
    ) -> Result<bool, String> {
        let (client, server, world) = self.fake_parts()?;
        let placed_pos = pos.offset(face.to_offset());
        let held_before = self.inventory().get_stack_in_hand(hand);
        let states_before = (
            world.get_block_state_id(&pos),
            world.get_block_state_id(&placed_pos),
        );

        let packet = SUseItemOn {
            hand: VarInt(hand_id(hand)),
            position: pos,
            face: VarInt(face as i32),
            cursor_pos: cursor,
            inside_block: false,
            is_against_world_border: false,
            sequence: VarInt(0),
        };
        let result = client.handle_use_item_on(self, &packet, &server);
        self.close_fake_screen();
        result.map_err(|err| err.to_string())?;

        let held_after = self.inventory().get_stack_in_hand(hand);
        let states_after = (
            world.get_block_state_id(&pos),
            world.get_block_state_id(&placed_pos),
        );
        Ok(states_before != states_after || !held_before.are_equal(&held_after))
    }

    /// Uses the item in `hand` in the air in the direction the fake player looks (eating, throwing,
    /// bows, buckets that scoop, fishing rods).
    ///
    /// # Errors
    /// This is not a fake player.
    pub fn fake_use_item(self: &Arc<Self>, hand: Hand) -> Result<(), String> {
        let (client, server, _) = self.fake_parts()?;
        let (yaw, pitch) = self.rotation();
        let packet = SUseItem {
            hand: VarInt(hand_id(hand)),
            sequence: VarInt(0),
            yaw,
            pitch,
        };
        client.handle_use_item(self, &packet, &server);
        self.close_fake_screen();
        Ok(())
    }

    /// Attacks `target` with the held item at full strength, with the damage, knockback and
    /// enchantments of a player attack. Players are only attacked when PvP is on.
    ///
    /// # Errors
    /// This is not a fake player, or the target is itself or in another world.
    pub fn fake_attack(self: &Arc<Self>, target: &Arc<dyn EntityBase>) -> Result<(), String> {
        let (_, server, world) = self.fake_parts()?;
        self.check_fake_target(target, &world)?;
        if let Some(victim) = target.get_player() {
            let config = &server.advanced_config.pvp;
            if !config.enabled || victim.living_entity.health.load() <= 0.0 {
                return Ok(());
            }
            if config.protect_creative && victim.gamemode.load() == GameMode::Creative {
                return Ok(());
            }
        }
        // Every call is a fresh attack, the caller decides how often to attack.
        self.last_attacked_ticks.store(1000, Ordering::Relaxed);
        self.attack(target);
        Ok(())
    }

    /// Interacts with `target` with the item in the main hand like a right click on it does
    /// (shearing, feeding, trading, saddling, name tags).
    ///
    /// # Errors
    /// This is not a fake player, or the target is itself or in another world.
    pub fn fake_interact_entity(self: &Arc<Self>, target: &Arc<dyn EntityBase>) -> Result<(), String> {
        let (client, server, world) = self.fake_parts()?;
        self.check_fake_target(target, &world)?;
        let packet = SInteract {
            entity_id: VarInt(target.get_entity().entity_id),
            r#type: VarInt(0),
            target_position: None,
            hand: Some(VarInt(hand_id(Hand::Right))),
            sneaking: self.living_entity.entity.is_sneaking(),
        };
        client.handle_interact(self, &packet, &server);
        self.close_fake_screen();
        Ok(())
    }

    /// Starts breaking the block at `pos`. The block is broken at once when it is instant to break
    /// (or the fake player is in creative), otherwise call [`Self::fake_continue_break`] once per
    /// tick. Breaking another block first stops the current one.
    ///
    /// # Errors
    /// This is not a fake player.
    pub fn fake_start_break(
        self: &Arc<Self>,
        pos: BlockPos,
        face: BlockDirection,
    ) -> Result<FakeBreakProgress, String> {
        let (client, server, world) = self.fake_parts()?;
        self.fake_stop_break()?;
        let state_before = world.get_block_state_id(&pos);
        if world.get_block_state(&pos).is_air() {
            return Ok(FakeBreakProgress::Rejected);
        }

        client.handle_player_action(
            self,
            &SPlayerAction {
                status: VarInt(Status::StartedDigging as i32),
                position: pos,
                face: face as u8,
                sequence: VarInt(0),
            },
            &server,
        );

        Ok(if self.mining.load(Ordering::Relaxed) {
            FakeBreakProgress::InProgress(0.0)
        } else if world.get_block_state_id(&pos) != state_before {
            FakeBreakProgress::Broken
        } else {
            FakeBreakProgress::Rejected
        })
    }

    /// Advances the break started by [`Self::fake_start_break`] by one tick. When the progress
    /// reaches `1.0` the block is broken with its drops, the tool wears and the events fire.
    ///
    /// # Errors
    /// This is not a fake player.
    pub fn fake_continue_break(self: &Arc<Self>) -> Result<FakeBreakProgress, String> {
        let (client, server, world) = self.fake_parts()?;
        if !self.mining.load(Ordering::Relaxed) {
            return Ok(FakeBreakProgress::Rejected);
        }
        // Nothing ticks a fake player, so time passes here.
        self.tick_counter.fetch_add(1, Ordering::Relaxed);

        let pos = *self
            .mining_pos
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = world.get_block_state(&pos);
        if state.is_air() {
            self.stop_mining();
            return Ok(FakeBreakProgress::Rejected);
        }
        let start = self.start_mining_time.load(Ordering::Relaxed);
        let finished = self.continue_mining(pos, &world, state, start);
        if !finished {
            let speed = block::calc_block_breaking(self, state, Block::from_state_id(state.id));
            let ticks = self.tick_counter.load(Ordering::Relaxed) - start;
            return Ok(FakeBreakProgress::InProgress(speed * (ticks + 1) as f32));
        }

        client.handle_player_action(
            self,
            &SPlayerAction {
                status: VarInt(Status::FinishedDigging as i32),
                position: pos,
                face: BlockDirection::Up as u8,
                sequence: VarInt(0),
            },
            &server,
        );
        // The handler leaves the break running when the block is out of reach.
        self.stop_mining();
        Ok(if world.get_block_state_id(&pos) == state.id {
            FakeBreakProgress::Rejected
        } else {
            FakeBreakProgress::Broken
        })
    }

    /// Stops breaking the current block, the damage on it goes away.
    ///
    /// # Errors
    /// This is not a fake player.
    pub fn fake_stop_break(self: &Arc<Self>) -> Result<(), String> {
        let (client, server, _) = self.fake_parts()?;
        if self.mining.load(Ordering::Relaxed) {
            let pos = *self
                .mining_pos
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            client.handle_player_action(
                self,
                &SPlayerAction {
                    status: VarInt(Status::CancelledDigging as i32),
                    position: pos,
                    face: BlockDirection::Down as u8,
                    sequence: VarInt(0),
                },
                &server,
            );
        }
        self.stop_mining();
        Ok(())
    }

    fn ensure_fake(&self) -> Result<(), String> {
        if self.is_fake {
            Ok(())
        } else {
            Err("this is not a fake player".to_string())
        }
    }

    /// The client, server and world the packet handlers of a fake player run with.
    fn fake_parts(&self) -> Result<(&JavaClient, Arc<Server>, Arc<World>), String> {
        self.ensure_fake()?;
        let client = self
            .client
            .java()
            .ok_or_else(|| "the fake player has no client".to_string())?;
        let world = self.world();
        let server = world
            .server
            .upgrade()
            .ok_or_else(|| "the server is shutting down".to_string())?;
        Ok((client, server, world))
    }

    fn check_fake_target(
        &self,
        target: &Arc<dyn EntityBase>,
        world: &Arc<World>,
    ) -> Result<(), String> {
        let entity = target.get_entity();
        if entity.entity_id == self.entity_id() {
            return Err("a fake player cannot target itself".to_string());
        }
        if !Arc::ptr_eq(&entity.world.load_full(), world) {
            return Err("the target is in another world".to_string());
        }
        Ok(())
    }

    /// A fake player has no screen to look at: close what a click opened (chests, crafting tables).
    fn close_fake_screen(&self) {
        let is_player_screen = self
            .current_screen_handler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_any()
            .is::<PlayerScreenHandler>();
        if !is_player_screen {
            self.close_handled_screen();
        }
    }
}
