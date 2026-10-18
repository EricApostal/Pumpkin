//! Bulk block placement, block destruction and entity spawning from NBT for `world`.

use pumpkin_core::world::bulk::{BlockChange, PluginBlockEntityChange};
use pumpkin_data::BlockStateId;
use pumpkin_data::block_entity_registry::plugin_block_entity_type_by_key;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use pumpkin_wasm_host_common::state::PluginHostState;
use wasmtime::component::{Accessor, HasSelf, Resource};

use super::{AccessorExt, run_blocking};
use crate::common::{WitNbtTree, from_wit_nbt_tree};
use crate::pumpkin::plugin::common::{BlockPos as WitBlockPos, Position as WitPosition};
use crate::pumpkin::plugin::world::{
    BlockChange as WitBlockChange, BlockFlags as WitBlockFlags, Entity, World,
};
use crate::world::from_wit_block_flags;

type Acc = Accessor<PluginHostState, HasSelf<PluginHostState>>;

const fn block_pos(pos: &WitBlockPos) -> BlockPos {
    BlockPos::new(pos.x, pos.y, pos.z)
}

/// Reads binary NBT with an unnamed root, the format of `get-block-entity-nbt`.
pub(crate) fn decode_nbt(bytes: &[u8]) -> Result<NbtCompound, String> {
    let mut cursor = std::io::Cursor::new(bytes);
    let mut reader = pumpkin_nbt::deserializer::NbtReadHelperJava::new(
        pumpkin_nbt::deserializer::NbtStreamReader(&mut cursor),
    );
    pumpkin_nbt::Nbt::read_unnamed(&mut reader)
        .map(|nbt| nbt.root_tag)
        .map_err(|error| format!("invalid NBT: {error}"))
}

/// Writes binary NBT with an unnamed root.
pub(crate) fn encode_nbt(nbt: NbtCompound) -> Vec<u8> {
    pumpkin_nbt::Nbt::from(nbt).write_unnamed().to_vec()
}

fn compound_from_tree(tree: &WitNbtTree, what: &str) -> Result<NbtCompound, String> {
    match from_wit_nbt_tree(tree)? {
        NbtTag::Compound(compound) => Ok(compound),
        _ => Err(format!("the {what} has to be a compound tag")),
    }
}

fn convert_change(change: WitBlockChange) -> Result<Option<BlockChange>, String> {
    let pos = block_pos(&change.pos);
    let Some(state) = BlockStateId::from_raw(change.state) else {
        return Err(format!("invalid block state id {}", change.state));
    };
    let mut converted = BlockChange::new(pos, state);
    if let Some(bytes) = change.block_entity_nbt {
        converted.block_entity = Some(decode_nbt(&bytes)?);
    }
    if let Some(plugin) = change.plugin_block_entity {
        let entity_type = plugin_block_entity_type_by_key(&plugin.type_key)
            .ok_or_else(|| format!("block entity type '{}' is not registered", plugin.type_key))?;
        converted.plugin_block_entity = Some(PluginBlockEntityChange {
            entity_type,
            data: compound_from_tree(&plugin.save_data, "save data")?,
            client_data: plugin
                .client_data
                .as_ref()
                .map(|data| compound_from_tree(data, "client data"))
                .transpose()?,
        });
    }
    Ok(Some(converted))
}

pub(crate) async fn set_blocks(
    accessor: &Acc,
    world: Resource<World>,
    changes: Vec<WitBlockChange>,
    update_flags: WitBlockFlags,
) -> wasmtime::Result<Result<u32, String>> {
    let world = accessor.get_res(&world)?;
    let flags = from_wit_block_flags(update_flags);
    let mut converted = Vec::with_capacity(changes.len());
    for change in changes {
        match convert_change(change) {
            Ok(Some(change)) => converted.push(change),
            Ok(None) => {}
            Err(error) => return Ok(Err(error)),
        }
    }
    let outcome = run_blocking(accessor, move || world.set_blocks_bulk(converted, flags)).await?;
    if outcome.errors.is_empty() {
        Ok(Ok(outcome.changed))
    } else {
        Ok(Err(outcome.errors.join("; ")))
    }
}

pub(crate) async fn remove_blocks(
    accessor: &Acc,
    world: Resource<World>,
    positions: Vec<WitBlockPos>,
    update_flags: WitBlockFlags,
) -> wasmtime::Result<u32> {
    let world = accessor.get_res(&world)?;
    let flags = from_wit_block_flags(update_flags);
    let positions: Vec<BlockPos> = positions.iter().map(block_pos).collect();
    let outcome = run_blocking(accessor, move || {
        world.remove_blocks_bulk(&positions, flags)
    })
    .await?;
    Ok(outcome.changed)
}

pub(crate) async fn destroy_block(
    accessor: &Acc,
    world: Resource<World>,
    pos: WitBlockPos,
    drop: bool,
    player: Option<Resource<Entity>>,
    with_particles: bool,
) -> wasmtime::Result<bool> {
    let world = accessor.get_res(&world)?;
    let player = match player {
        Some(entity) => {
            let uuid = accessor.get_res(&entity)?.get_entity().entity_uuid;
            accessor.server()?.get_player_by_uuid(uuid)
        }
        None => None,
    };
    let pos = block_pos(&pos);
    run_blocking(accessor, move || {
        world.destroy_block(&pos, drop, player.as_ref(), with_particles)
    })
    .await
}

pub(crate) async fn update_shape_around(
    accessor: &Acc,
    world: Resource<World>,
    pos: WitBlockPos,
) -> wasmtime::Result<()> {
    let world = accessor.get_res(&world)?;
    let pos = block_pos(&pos);
    run_blocking(accessor, move || world.update_shape_around(&pos)).await
}

pub(crate) async fn mark_and_notify(
    accessor: &Acc,
    world: Resource<World>,
    pos: WitBlockPos,
    old_state: u16,
    new_state: u16,
    update_flags: WitBlockFlags,
) -> wasmtime::Result<()> {
    let (Some(old), Some(new)) = (
        BlockStateId::from_raw(old_state),
        BlockStateId::from_raw(new_state),
    ) else {
        return Err(wasmtime::Error::msg("Invalid BlockStateId"));
    };
    let world = accessor.get_res(&world)?;
    let flags = from_wit_block_flags(update_flags);
    let pos = block_pos(&pos);
    run_blocking(accessor, move || {
        world.mark_and_notify(&pos, old, new, flags)
    })
    .await
}

pub(crate) async fn spawn_entity_from_nbt(
    accessor: &Acc,
    world: Resource<World>,
    nbt: Vec<u8>,
    pos: WitPosition,
) -> wasmtime::Result<Option<Resource<Entity>>> {
    let world = accessor.get_res(&world)?;
    let Ok(nbt) = decode_nbt(&nbt) else {
        return Ok(None);
    };
    let pos = Vector3::new(pos.0, pos.1, pos.2);
    let spawned = run_blocking(accessor, move || {
        world.spawn_entity_from_nbt(&nbt, Some(pos))
    })
    .await?;
    spawned.map(|entity| accessor.add_res(entity)).transpose()
}
