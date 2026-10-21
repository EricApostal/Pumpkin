use crate::pumpkin::plugin::block_callbacks::{BlockCallbackFlags, Host};
use pumpkin_core::plugin::permissions;
use pumpkin_data::Block;
use pumpkin_data::block_registry::BlockCallbacks;
use pumpkin_wasm_host_common::state::PluginHostState;
use tracing::info;

#[expect(clippy::too_many_lines)]
fn from_wit_flags(flags: BlockCallbackFlags) -> BlockCallbacks {
    [
        (
            BlockCallbackFlags::NEIGHBOR_UPDATE,
            BlockCallbacks::NEIGHBOR_UPDATE,
        ),
        (BlockCallbackFlags::PLACED, BlockCallbacks::PLACED),
        (
            BlockCallbackFlags::PLAYER_PLACED,
            BlockCallbacks::PLAYER_PLACED,
        ),
        (
            BlockCallbackFlags::STATE_REPLACED,
            BlockCallbacks::STATE_REPLACED,
        ),
        (BlockCallbackFlags::BROKEN, BlockCallbacks::BROKEN),
        (
            BlockCallbackFlags::SCHEDULED_TICK,
            BlockCallbacks::SCHEDULED_TICK,
        ),
        (BlockCallbackFlags::RANDOM_TICK, BlockCallbacks::RANDOM_TICK),
        (
            BlockCallbackFlags::ENTITY_COLLISION,
            BlockCallbacks::ENTITY_COLLISION,
        ),
        (BlockCallbackFlags::ENTITY_STEP, BlockCallbacks::ENTITY_STEP),
        (BlockCallbackFlags::CAN_PLACE, BlockCallbacks::CAN_PLACE),
        (
            BlockCallbackFlags::UPDATE_SHAPE,
            BlockCallbacks::UPDATE_SHAPE,
        ),
        (
            BlockCallbackFlags::REDSTONE_OUTPUT,
            BlockCallbacks::REDSTONE_OUTPUT,
        ),
        (BlockCallbackFlags::WEAK_POWER, BlockCallbacks::WEAK_POWER),
        (
            BlockCallbackFlags::STRONG_POWER,
            BlockCallbacks::STRONG_POWER,
        ),
        (
            BlockCallbackFlags::COMPARATOR_OUTPUT,
            BlockCallbacks::COMPARATOR_OUTPUT,
        ),
        (
            BlockCallbackFlags::DYNAMIC_SHAPE,
            BlockCallbacks::DYNAMIC_SHAPE,
        ),
        (
            BlockCallbackFlags::USE_WITHOUT_ITEM,
            BlockCallbacks::USE_WITHOUT_ITEM,
        ),
        (
            BlockCallbackFlags::USE_WITH_ITEM,
            BlockCallbacks::USE_WITH_ITEM,
        ),
        (BlockCallbackFlags::ATTACK, BlockCallbacks::ATTACK),
        (
            BlockCallbackFlags::GET_CLONE_ITEM,
            BlockCallbacks::GET_CLONE_ITEM,
        ),
        (
            BlockCallbackFlags::PLAYER_WILL_DESTROY,
            BlockCallbacks::PLAYER_WILL_DESTROY,
        ),
        (
            BlockCallbackFlags::GET_FRICTION,
            BlockCallbacks::GET_FRICTION,
        ),
        (
            BlockCallbackFlags::GET_SPEED_FACTOR,
            BlockCallbacks::GET_SPEED_FACTOR,
        ),
        (
            BlockCallbackFlags::GET_JUMP_FACTOR,
            BlockCallbacks::GET_JUMP_FACTOR,
        ),
        (
            BlockCallbackFlags::CAN_ENTITY_DESTROY,
            BlockCallbacks::CAN_ENTITY_DESTROY,
        ),
        (
            BlockCallbackFlags::ON_LANDED_UPON,
            BlockCallbacks::ON_LANDED_UPON,
        ),
        (
            BlockCallbackFlags::GET_SOUND_TYPE,
            BlockCallbacks::GET_SOUND_TYPE,
        ),
        (
            BlockCallbackFlags::IS_PATHFINDABLE,
            BlockCallbacks::IS_PATHFINDABLE,
        ),
        (
            BlockCallbackFlags::CAN_CONNECT_REDSTONE,
            BlockCallbacks::CAN_CONNECT_REDSTONE,
        ),
        (
            BlockCallbackFlags::GET_FIRE_SPREAD,
            BlockCallbacks::GET_FIRE_SPREAD,
        ),
        (
            BlockCallbackFlags::ENTITY_COLLISION_SHAPE,
            BlockCallbacks::ENTITY_COLLISION_SHAPE,
        ),
    ]
    .into_iter()
    .filter(|(wit, _)| flags.contains(*wit))
    .fold(BlockCallbacks::NONE, |callbacks, (_, flag)| {
        callbacks.union(flag)
    })
}

impl Host for PluginHostState {
    fn set_block_callbacks(
        &mut self,
        block_key: String,
        callbacks: BlockCallbackFlags,
    ) -> wasmtime::Result<Result<(), String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::REGISTRY_BLOCKS)
        {
            return Ok(Err(self.missing_permission(
                "setting block callbacks",
                permissions::REGISTRY_BLOCKS,
            )));
        }
        let callbacks = from_wit_flags(callbacks);
        Ok(match Block::set_dynamic_callbacks(&block_key, callbacks) {
            Ok(()) => {
                info!(
                    "Plugin {} set callbacks {:#x} for block {}",
                    self.name.as_deref().unwrap_or("<unknown>"),
                    callbacks.bits(),
                    block_key
                );
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_wit_flag_maps_to_its_own_callback() {
        let all = from_wit_flags(BlockCallbackFlags::all());
        assert_eq!(all, BlockCallbacks::ALL);
        assert!(from_wit_flags(BlockCallbackFlags::empty()).is_empty());

        let single = from_wit_flags(BlockCallbackFlags::SCHEDULED_TICK);
        assert_eq!(single, BlockCallbacks::SCHEDULED_TICK);
        let pair = from_wit_flags(BlockCallbackFlags::PLACED | BlockCallbackFlags::UPDATE_SHAPE);
        assert_eq!(
            pair,
            BlockCallbacks::PLACED.union(BlockCallbacks::UPDATE_SHAPE)
        );
    }
}
