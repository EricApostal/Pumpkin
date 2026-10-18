use crate::pumpkin::plugin::block_callbacks::{BlockCallbackFlags, Host};
use pumpkin_core::plugin::permissions;
use pumpkin_data::Block;
use pumpkin_data::block_registry::BlockCallbacks;
use pumpkin_wasm_host_common::state::PluginHostState;
use tracing::info;

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
            return Ok(Err(format!(
                "setting block callbacks needs the '{}' permission",
                permissions::REGISTRY_BLOCKS
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
