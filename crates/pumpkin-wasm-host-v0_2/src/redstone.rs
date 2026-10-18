//! The redstone functions of the `world` resource. They can run handlers of custom blocks (which
//! power events), so they run as the host work of the active guest call, like `set-block-state`.

use pumpkin_core::block::blocks::redstone as logic;
use pumpkin_data::BlockDirection;
use pumpkin_util::math::position::BlockPos;
use pumpkin_wasm_host_common::state::PluginHostState;
use wasmtime::component::{Accessor, HasSelf, Resource};

use crate::mob::from_wit_block_direction;
use crate::pumpkin::plugin::world::World;
use crate::pumpkin::plugin::world::{BlockDirection as WitBlockDirection, BlockPos as WitBlockPos};
use crate::{AccessorExt, run_blocking};

type Store = Accessor<PluginHostState, HasSelf<PluginHostState>>;

const fn block_pos(pos: WitBlockPos) -> BlockPos {
    BlockPos::new(pos.x, pos.y, pos.z)
}

/// Runs `op` on the world, with the position and direction converted.
async fn read<R: Send + 'static>(
    accessor: &Store,
    world: Resource<World>,
    pos: WitBlockPos,
    direction: WitBlockDirection,
    op: impl FnOnce(&pumpkin_core::world::World, &BlockPos, BlockDirection) -> R + Send + 'static,
) -> wasmtime::Result<R> {
    let world = accessor.get_res(&world)?;
    let pos = block_pos(pos);
    let direction = from_wit_block_direction(direction);
    run_blocking(accessor, move || op(&world, &pos, direction)).await
}

pub(crate) async fn get_redstone_power(
    accessor: &Store,
    world: Resource<World>,
    pos: WitBlockPos,
    direction: WitBlockDirection,
) -> wasmtime::Result<u8> {
    read(accessor, world, pos, direction, logic::get_signal_from).await
}

pub(crate) async fn get_direct_redstone_power(
    accessor: &Store,
    world: Resource<World>,
    pos: WitBlockPos,
    direction: WitBlockDirection,
) -> wasmtime::Result<u8> {
    read(
        accessor,
        world,
        pos,
        direction,
        logic::get_direct_signal_from,
    )
    .await
}

pub(crate) async fn has_neighbor_signal(
    accessor: &Store,
    world: Resource<World>,
    pos: WitBlockPos,
) -> wasmtime::Result<bool> {
    let world = accessor.get_res(&world)?;
    let pos = block_pos(pos);
    run_blocking(accessor, move || {
        logic::block_receives_redstone_power(&world, &pos)
    })
    .await
}

pub(crate) async fn get_best_neighbor_signal(
    accessor: &Store,
    world: Resource<World>,
    pos: WitBlockPos,
) -> wasmtime::Result<u8> {
    let world = accessor.get_res(&world)?;
    let pos = block_pos(pos);
    run_blocking(accessor, move || {
        logic::get_best_neighbor_signal(&world, &pos)
    })
    .await
}

pub(crate) async fn get_analog_signal(
    accessor: &Store,
    world: Resource<World>,
    pos: WitBlockPos,
    direction: WitBlockDirection,
) -> wasmtime::Result<Option<u8>> {
    read(accessor, world, pos, direction, logic::get_analog_signal).await
}

pub(crate) async fn update_neighbors(
    accessor: &Store,
    world: Resource<World>,
    pos: WitBlockPos,
    source_block: Option<String>,
) -> wasmtime::Result<Result<(), String>> {
    let source = match source_block {
        Some(key) => match pumpkin_data::Block::from_name(&key) {
            Some(block) => Some(block),
            None => return Ok(Err(format!("unknown block '{key}'"))),
        },
        None => None,
    };
    let world = accessor.get_res(&world)?;
    let pos = block_pos(pos);
    run_blocking(accessor, move || {
        let source = source.unwrap_or_else(|| world.get_block(&pos));
        world.update_neighbors_at(&pos, source, None);
    })
    .await?;
    Ok(Ok(()))
}

pub(crate) async fn update_comparators(
    accessor: &Store,
    world: Resource<World>,
    pos: WitBlockPos,
) -> wasmtime::Result<()> {
    let world = accessor.get_res(&world)?;
    let pos = block_pos(pos);
    run_blocking(accessor, move || {
        let block = world.get_block(&pos);
        world.update_neighbour_for_output_signal(&pos, block);
    })
    .await
}

pub(crate) async fn set_block_redstone_output(
    accessor: &Store,
    world: Resource<World>,
    pos: WitBlockPos,
    face: Option<WitBlockDirection>,
    weak: u8,
    strong: u8,
) -> wasmtime::Result<Result<(), String>> {
    let world = accessor.get_res(&world)?;
    let pos = block_pos(pos);
    let face = face.map(from_wit_block_direction);
    run_blocking(accessor, move || {
        world.set_plugin_redstone_output(&pos, face, weak, strong)
    })
    .await
}

pub(crate) async fn set_block_comparator_output(
    accessor: &Store,
    world: Resource<World>,
    pos: WitBlockPos,
    output: Option<u8>,
) -> wasmtime::Result<Result<(), String>> {
    let world = accessor.get_res(&world)?;
    let pos = block_pos(pos);
    run_blocking(accessor, move || {
        world.set_plugin_comparator_output(&pos, output)
    })
    .await
}
