use super::{AccessorExt, run_blocking};
use crate::events::from_wasm_position;
use crate::mob::from_wit_block_direction;
use crate::pumpkin::plugin::common::{BlockPos as WitBlockPos, Hand as WitHand};
use crate::pumpkin::plugin::fake_player::{BreakProgress, Host, HostWithStore};
use crate::pumpkin::plugin::player::Player as WitPlayer;
use crate::pumpkin::plugin::uuid::Uuid as WitUuid;
use crate::pumpkin::plugin::world::{
    BlockDirection as WitBlockDirection, Entity as WitEntity, World as WitWorld,
};
use crate::uuid::UuidExt;
use pumpkin_core::entity::fake_player::FakeBreakProgress;
use pumpkin_core::entity::player::Player;
use pumpkin_core::plugin::permissions;
use pumpkin_util::Hand;
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use pumpkin_wasm_host_common::state::PluginHostState;
use std::sync::Arc;
use wasmtime::component::{Accessor, HasSelf, Resource};

const fn from_wit_hand(hand: WitHand) -> Hand {
    match hand {
        WitHand::Right => Hand::Right,
        WitHand::Left => Hand::Left,
    }
}

const fn to_wit_progress(progress: FakeBreakProgress) -> BreakProgress {
    match progress {
        FakeBreakProgress::Rejected => BreakProgress::Rejected,
        FakeBreakProgress::InProgress(progress) => BreakProgress::InProgress(progress),
        FakeBreakProgress::Broken => BreakProgress::Broken,
    }
}

const fn block_pos(pos: &WitBlockPos) -> BlockPos {
    BlockPos::new(pos.x, pos.y, pos.z)
}

fn permission_error(state: &PluginHostState) -> Option<String> {
    (!state.permissions.iter().any(|p| p == permissions::FAKE_PLAYERS)).then(|| {
        format!(
            "fake players need the '{}' permission",
            permissions::FAKE_PLAYERS
        )
    })
}

/// The fake player of a resource, or the error to hand to the plugin.
fn fake_player(
    accessor: &Accessor<PluginHostState, HasSelf<PluginHostState>>,
    player: &Resource<WitPlayer>,
) -> wasmtime::Result<Result<Arc<Player>, String>> {
    let denied = accessor.with(|mut host| permission_error(host.get()));
    if let Some(error) = denied {
        return Ok(Err(error));
    }
    let player = accessor.get_res(player)?;
    Ok(if player.is_fake {
        Ok(player)
    } else {
        Err("this is not a fake player".to_string())
    })
}

impl Host for PluginHostState {
    fn set_fake_player_location(
        &mut self,
        player: Resource<WitPlayer>,
        world: Option<Resource<WitWorld>>,
        pos: crate::pumpkin::plugin::common::Position,
        yaw: f32,
        pitch: f32,
    ) -> wasmtime::Result<Result<(), String>> {
        if let Some(error) = permission_error(self) {
            return Ok(Err(error));
        }
        let player = Arc::clone(self.get(&player)?);
        let world = match world {
            Some(world) => Some(Arc::clone(self.get(&world)?)),
            None => None,
        };
        Ok(player.set_fake_location(world.as_ref(), from_wasm_position(pos), yaw, pitch))
    }

    fn set_fake_player_sneaking(
        &mut self,
        player: Resource<WitPlayer>,
        sneaking: bool,
    ) -> wasmtime::Result<Result<(), String>> {
        if let Some(error) = permission_error(self) {
            return Ok(Err(error));
        }
        let player = Arc::clone(self.get(&player)?);
        if !player.is_fake {
            return Ok(Err("this is not a fake player".to_string()));
        }
        player.living_entity.entity.set_sneaking(sneaking);
        Ok(Ok(()))
    }
}

impl HostWithStore for HasSelf<PluginHostState> {
    async fn create_fake_player(
        accessor: &Accessor<PluginHostState, Self>,
        world: Resource<WitWorld>,
        profile_name: String,
        id: Option<WitUuid>,
    ) -> wasmtime::Result<Result<Resource<WitPlayer>, String>> {
        let denied = accessor.with(|mut host| permission_error(host.get()));
        if let Some(error) = denied {
            return Ok(Err(error));
        }
        let world = accessor.get_res(&world)?;
        let id = id.as_ref().map(WitUuid::from_wit);
        let created =
            run_blocking(accessor, move || Player::create_fake(&world, &profile_name, id)).await?;
        match created {
            Ok(player) => Ok(Ok(accessor.add_res(player)?)),
            Err(error) => Ok(Err(error)),
        }
    }

    async fn remove_fake_player(
        accessor: &Accessor<PluginHostState, Self>,
        player: Resource<WitPlayer>,
    ) -> wasmtime::Result<Result<(), String>> {
        let player = match fake_player(accessor, &player)? {
            Ok(player) => player,
            Err(error) => return Ok(Err(error)),
        };
        run_blocking(accessor, move || player.remove_fake()).await
    }

    async fn use_item_on_block(
        accessor: &Accessor<PluginHostState, Self>,
        player: Resource<WitPlayer>,
        pos: WitBlockPos,
        face: WitBlockDirection,
        hand: WitHand,
        cursor: (f32, f32, f32),
    ) -> wasmtime::Result<Result<bool, String>> {
        let player = match fake_player(accessor, &player)? {
            Ok(player) => player,
            Err(error) => return Ok(Err(error)),
        };
        let pos = block_pos(&pos);
        let face = from_wit_block_direction(face);
        let hand = from_wit_hand(hand);
        let cursor = Vector3::new(cursor.0, cursor.1, cursor.2);
        run_blocking(accessor, move || {
            player.fake_use_item_on_block(pos, face, hand, cursor)
        })
        .await
    }

    async fn use_item(
        accessor: &Accessor<PluginHostState, Self>,
        player: Resource<WitPlayer>,
        hand: WitHand,
    ) -> wasmtime::Result<Result<(), String>> {
        let player = match fake_player(accessor, &player)? {
            Ok(player) => player,
            Err(error) => return Ok(Err(error)),
        };
        let hand = from_wit_hand(hand);
        run_blocking(accessor, move || player.fake_use_item(hand)).await
    }

    async fn attack_entity(
        accessor: &Accessor<PluginHostState, Self>,
        player: Resource<WitPlayer>,
        target: Resource<WitEntity>,
    ) -> wasmtime::Result<Result<(), String>> {
        let player = match fake_player(accessor, &player)? {
            Ok(player) => player,
            Err(error) => return Ok(Err(error)),
        };
        let target = accessor.get_res(&target)?;
        run_blocking(accessor, move || player.fake_attack(&target)).await
    }

    async fn interact_entity(
        accessor: &Accessor<PluginHostState, Self>,
        player: Resource<WitPlayer>,
        target: Resource<WitEntity>,
    ) -> wasmtime::Result<Result<(), String>> {
        let player = match fake_player(accessor, &player)? {
            Ok(player) => player,
            Err(error) => return Ok(Err(error)),
        };
        let target = accessor.get_res(&target)?;
        run_blocking(accessor, move || player.fake_interact_entity(&target)).await
    }

    async fn start_break_block(
        accessor: &Accessor<PluginHostState, Self>,
        player: Resource<WitPlayer>,
        pos: WitBlockPos,
        face: WitBlockDirection,
    ) -> wasmtime::Result<Result<BreakProgress, String>> {
        let player = match fake_player(accessor, &player)? {
            Ok(player) => player,
            Err(error) => return Ok(Err(error)),
        };
        let pos = block_pos(&pos);
        let face = from_wit_block_direction(face);
        Ok(run_blocking(accessor, move || player.fake_start_break(pos, face))
            .await?
            .map(to_wit_progress))
    }

    async fn continue_break_block(
        accessor: &Accessor<PluginHostState, Self>,
        player: Resource<WitPlayer>,
    ) -> wasmtime::Result<Result<BreakProgress, String>> {
        let player = match fake_player(accessor, &player)? {
            Ok(player) => player,
            Err(error) => return Ok(Err(error)),
        };
        Ok(run_blocking(accessor, move || player.fake_continue_break())
            .await?
            .map(to_wit_progress))
    }

    async fn stop_break_block(
        accessor: &Accessor<PluginHostState, Self>,
        player: Resource<WitPlayer>,
    ) -> wasmtime::Result<Result<(), String>> {
        let player = match fake_player(accessor, &player)? {
            Ok(player) => player,
            Err(error) => return Ok(Err(error)),
        };
        run_blocking(accessor, move || player.fake_stop_break()).await
    }
}
