use std::sync::Arc;

use crate::block::{
    BlockBehaviour, BrokenArgs, CanPlaceAtArgs, EmitsRedstonePowerArgs, GetComparatorOutputArgs,
    GetRedstonePowerArgs, GetStateForNeighborUpdateArgs, OnEntityCollisionArgs, OnEntityStepArgs,
    OnNeighborUpdateArgs, OnPlaceArgs, OnScheduledTickArgs, OnStateReplacedArgs, PlacedArgs,
    PlayerPlacedArgs, RandomTickArgs,
};
use crate::entity::EntityBase;
use crate::plugin::Payload;
use crate::plugin::block::{
    CustomBlockBrokenEvent, CustomBlockCanPlaceEvent, CustomBlockComparatorOutputEvent,
    CustomBlockEntityCollisionEvent, CustomBlockEntityStepEvent, CustomBlockNeighborUpdateEvent,
    CustomBlockPlacedEvent, CustomBlockRandomTickEvent, CustomBlockRedstonePowerEvent,
    CustomBlockScheduledTickEvent, CustomBlockStateReplacedEvent, CustomBlockUpdateShapeEvent,
};
use crate::server::Server;
use crate::world::World;
use pumpkin_data::block_registry::{BlockCallbacks, DynamicBlockInfo, PlacementContext};
use pumpkin_data::{Block, BlockState, BlockStateId, FacingExt, HorizontalFacingExt};

/// The highest redstone power and analog output there is.
const MAX_POWER: u8 = 15;

/// The callbacks with output that is stored per position and dropped when the block goes.
const STORED_OUTPUT: BlockCallbacks =
    BlockCallbacks::REDSTONE_OUTPUT.union(BlockCallbacks::COMPARATOR_OUTPUT);

/// Behaviour of every block that a plugin registers, driven by its registration.
///
/// The hooks that call back into the plugin do so only for the blocks that opted in with
/// [`BlockCallbacks`], and only when a plugin listens for the event.
pub struct DynamicBlock;

fn state_id(block: &Block, state_index: u16) -> BlockStateId {
    block
        .states
        .get(usize::from(state_index))
        .map_or(block.default_state.id, |state| state.id)
}

/// The info of `block` when it is a custom block that opted in to `callback`.
fn opted_in(block: &Block, callback: BlockCallbacks) -> Option<&'static DynamicBlockInfo> {
    block
        .dynamic_info()
        .filter(|info| info.callbacks().contains(callback))
}

/// The info of `block` when it is a custom block that opted in to any of `callbacks`.
fn opted_in_any(block: &Block, callbacks: BlockCallbacks) -> Option<&'static DynamicBlockInfo> {
    block
        .dynamic_info()
        .filter(|info| info.callbacks().intersects(callbacks))
}

/// Whether comparators read `block`: it has an analog output state flag, or it is a custom block
/// with the comparator output callback (the state flags of custom blocks are fixed on registration).
#[must_use]
pub fn has_analog_output(block: &Block, state: &BlockState) -> bool {
    state.has_analog_output_signal() || opted_in(block, BlockCallbacks::COMPARATOR_OUTPUT).is_some()
}

/// The server of `world`, when a plugin handles `E`.
fn server_with_handlers<E: Payload + 'static>(world: &World) -> Option<Arc<Server>> {
    let server = world.server.upgrade()?;
    server.plugin_manager.has_handlers::<E>().then_some(server)
}

/// The shared `Arc` of a world that is only borrowed.
fn world_arc(server: &Server, world: &World) -> Option<Arc<World>> {
    server
        .worlds
        .load()
        .iter()
        .find(|candidate| candidate.uuid == world.uuid)
        .cloned()
}

impl BlockBehaviour for DynamicBlock {
    fn on_place(&self, args: OnPlaceArgs<'_>) -> BlockStateId {
        let Some(info) = args.block.dynamic_info() else {
            return args.block.default_state.id;
        };
        let entity = args.player.get_entity();
        let context = PlacementContext {
            // `direction` points from the placed block to the clicked one.
            clicked_face: args.direction.opposite(),
            horizontal_facing: entity.get_horizontal_facing().to_block_direction(),
            looking_direction: entity.get_facing().to_block_direction(),
            pitch: entity.pitch.load(),
            cursor_y: args.use_item_on.cursor_pos.y,
            in_water: args.replacing.water_source(),
            sneaking: entity.is_sneaking(),
        };
        // Placement rules first, then connect rules from the neighbours.
        let placed = info.state_for_placement(info.default_state_index(), &context);
        let index = info.state_for_neighbours(placed, |direction| {
            args.world
                .get_block(&args.position.offset(direction.to_offset()))
        });
        state_id(args.block, index)
    }

    fn get_state_for_neighbor_update(
        &self,
        args: GetStateForNeighborUpdateArgs<'_>,
    ) -> BlockStateId {
        let Some(info) = args.block.dynamic_info() else {
            return args.state_id;
        };
        let Some(index) = info.state_index(args.state_id) else {
            return args.state_id;
        };
        let neighbour = args.world.get_block(args.neighbor_position);
        let new_index = info.update_connection(index, args.direction, neighbour);
        let connected = state_id(args.block, new_index);
        if !info.callbacks().contains(BlockCallbacks::UPDATE_SHAPE) {
            return connected;
        }

        let Some(server) = server_with_handlers::<CustomBlockUpdateShapeEvent>(args.world) else {
            return connected;
        };
        let Some(world) = world_arc(&server, args.world) else {
            return connected;
        };
        let mut event = CustomBlockUpdateShapeEvent {
            world,
            position: *args.position,
            block: Block::from_id(args.block.id),
            state_index: index,
            direction: args.direction,
            neighbor_position: *args.neighbor_position,
            neighbor_block: neighbour,
            neighbor_state_id: args.neighbor_state_id,
            new_state_index: new_index,
        };
        server.plugin_manager.fire_blocking(&server, &mut event);
        if event.new_state_index != new_index && event.new_state_index < info.state_count() {
            state_id(args.block, event.new_state_index)
        } else {
            connected
        }
    }

    fn on_neighbor_update(&self, args: OnNeighborUpdateArgs<'_>) {
        let Some(info) = opted_in(args.block, BlockCallbacks::NEIGHBOR_UPDATE) else {
            return;
        };
        let Some(server) = server_with_handlers::<CustomBlockNeighborUpdateEvent>(args.world)
        else {
            return;
        };
        let Some(state_index) = info.state_index(args.world.get_block_state_id(args.position))
        else {
            return;
        };
        let mut event = CustomBlockNeighborUpdateEvent {
            world: args.world.clone(),
            position: *args.position,
            block: Block::from_id(args.block.id),
            state_index,
            source_position: *args.source_position,
            source_block: Block::from_id(args.source_block.id),
        };
        server.plugin_manager.fire_blocking(&server, &mut event);
    }

    fn placed(&self, args: PlacedArgs<'_>) {
        let Some(info) = opted_in(args.block, BlockCallbacks::PLACED) else {
            return;
        };
        let Some(server) = server_with_handlers::<CustomBlockPlacedEvent>(args.world) else {
            return;
        };
        let Some(state_index) = info.state_index(args.state_id) else {
            return;
        };
        let mut event = CustomBlockPlacedEvent {
            world: args.world.clone(),
            position: *args.position,
            block: Block::from_id(args.block.id),
            state_index,
            player: None,
            old_state_id: Some(args.old_state_id),
            moved: args.notify,
        };
        server.plugin_manager.fire_blocking(&server, &mut event);
    }

    fn player_placed(&self, args: PlayerPlacedArgs<'_>) {
        let Some(info) = opted_in(args.block, BlockCallbacks::PLAYER_PLACED) else {
            return;
        };
        let Some(server) = server_with_handlers::<CustomBlockPlacedEvent>(args.world) else {
            return;
        };
        let Some(state_index) = info.state_index(args.state_id) else {
            return;
        };
        let mut event = CustomBlockPlacedEvent {
            world: args.world.clone(),
            position: *args.position,
            block: Block::from_id(args.block.id),
            state_index,
            player: Some(args.player.gameprofile.id),
            old_state_id: None,
            moved: false,
        };
        server.plugin_manager.fire_blocking(&server, &mut event);
    }

    fn on_state_replaced(&self, args: OnStateReplacedArgs<'_>) {
        if opted_in_any(args.block, STORED_OUTPUT).is_some() {
            args.world.plugin_redstone.remove(args.position);
        }
        let Some(info) = opted_in(args.block, BlockCallbacks::STATE_REPLACED) else {
            return;
        };
        let Some(server) = server_with_handlers::<CustomBlockStateReplacedEvent>(args.world) else {
            return;
        };
        let Some(old_state_index) = info.state_index(args.old_state_id) else {
            return;
        };
        let mut event = CustomBlockStateReplacedEvent {
            world: args.world.clone(),
            position: *args.position,
            block: Block::from_id(args.block.id),
            old_state_index,
            moved: args.moved,
        };
        server.plugin_manager.fire_blocking(&server, &mut event);
    }

    fn broken(&self, args: BrokenArgs<'_>) {
        let Some(info) = opted_in(args.block, BlockCallbacks::BROKEN) else {
            return;
        };
        let Some(server) = server_with_handlers::<CustomBlockBrokenEvent>(args.world) else {
            return;
        };
        let Some(state_index) = info.state_index(args.state.id) else {
            return;
        };
        let mut event = CustomBlockBrokenEvent {
            world: args.world.clone(),
            position: *args.position,
            block: Block::from_id(args.block.id),
            state_index,
            player: args.player.gameprofile.id,
        };
        server.plugin_manager.fire_blocking(&server, &mut event);
    }

    fn on_scheduled_tick(&self, args: OnScheduledTickArgs<'_>) {
        let Some(info) = opted_in(args.block, BlockCallbacks::SCHEDULED_TICK) else {
            return;
        };
        let Some(server) = server_with_handlers::<CustomBlockScheduledTickEvent>(args.world) else {
            return;
        };
        let Some(state_index) = info.state_index(args.world.get_block_state_id(args.position))
        else {
            return;
        };
        let mut event = CustomBlockScheduledTickEvent {
            world: args.world.clone(),
            position: *args.position,
            block: Block::from_id(args.block.id),
            state_index,
        };
        server.plugin_manager.fire_blocking(&server, &mut event);
    }

    fn random_tick(&self, args: RandomTickArgs<'_>) {
        let Some(info) = opted_in(args.block, BlockCallbacks::RANDOM_TICK) else {
            return;
        };
        let Some(server) = server_with_handlers::<CustomBlockRandomTickEvent>(args.world) else {
            return;
        };
        let Some(state_index) = info.state_index(args.world.get_block_state_id(args.position))
        else {
            return;
        };
        let mut event = CustomBlockRandomTickEvent {
            world: args.world.clone(),
            position: *args.position,
            block: Block::from_id(args.block.id),
            state_index,
        };
        server.plugin_manager.fire_blocking(&server, &mut event);
    }

    fn on_entity_collision(&self, args: OnEntityCollisionArgs<'_>) {
        let Some(info) = opted_in(args.block, BlockCallbacks::ENTITY_COLLISION) else {
            return;
        };
        let Some(server) = server_with_handlers::<CustomBlockEntityCollisionEvent>(args.world)
        else {
            return;
        };
        let Some(state_index) = info.state_index(args.state.id) else {
            return;
        };
        let entity = args.entity.get_entity();
        let mut event = CustomBlockEntityCollisionEvent {
            world: args.world.clone(),
            position: *args.position,
            block: Block::from_id(args.block.id),
            state_index,
            entity_id: entity.entity_id,
            entity_uuid: entity.entity_uuid,
            entity_type: entity.entity_type,
            entity_position: entity.pos.load(),
        };
        server.plugin_manager.fire_blocking(&server, &mut event);
    }

    fn on_entity_step(&self, args: OnEntityStepArgs<'_>) {
        let Some(info) = opted_in(args.block, BlockCallbacks::ENTITY_STEP) else {
            return;
        };
        let Some(server) = server_with_handlers::<CustomBlockEntityStepEvent>(args.world) else {
            return;
        };
        let Some(state_index) = info.state_index(args.state.id) else {
            return;
        };
        let entity = args.entity.get_entity();
        let mut event = CustomBlockEntityStepEvent {
            world: args.world.clone(),
            position: *args.position,
            block: Block::from_id(args.block.id),
            state_index,
            entity_id: entity.entity_id,
            entity_uuid: entity.entity_uuid,
            entity_type: entity.entity_type,
            entity_position: entity.pos.load(),
            below_supporting_block: args.below_supporting_block,
        };
        server.plugin_manager.fire_blocking(&server, &mut event);
    }

    fn can_place_at(&self, args: CanPlaceAtArgs<'_>) -> bool {
        let Some(info) = opted_in(args.block, BlockCallbacks::CAN_PLACE) else {
            return true;
        };
        // Without a world (world generation) there is no one to ask.
        let Some(world) = args.world else {
            return true;
        };
        let Some(server) = server_with_handlers::<CustomBlockCanPlaceEvent>(world) else {
            return true;
        };
        let (Some(world), Some(state_index)) =
            (world_arc(&server, world), info.state_index(args.state.id))
        else {
            return true;
        };
        let mut event = CustomBlockCanPlaceEvent {
            world,
            position: *args.position,
            block: Block::from_id(args.block.id),
            state_index,
            face: args.direction,
            player: args.player.map(|player| player.gameprofile.id),
            can_place: true,
        };
        server.plugin_manager.fire_blocking(&server, &mut event);
        event.can_place
    }

    fn emits_redstone_power(&self, args: EmitsRedstonePowerArgs<'_>) -> bool {
        // A signal source on every side, like vanilla `isSignalSource`.
        opted_in_any(args.block, BlockCallbacks::REDSTONE_SOURCE).is_some()
    }

    fn get_weak_redstone_power(&self, args: GetRedstonePowerArgs<'_>) -> u8 {
        redstone_power(&args, false)
    }

    fn get_strong_redstone_power(&self, args: GetRedstonePowerArgs<'_>) -> u8 {
        redstone_power(&args, true)
    }

    fn get_comparator_output(&self, args: GetComparatorOutputArgs<'_>) -> Option<u8> {
        let info = opted_in(args.block, BlockCallbacks::COMPARATOR_OUTPUT)?;
        // What the plugin stored goes before asking it.
        if let Some(output) = args
            .world
            .plugin_redstone
            .comparator_output(args.position, args.block.id)
        {
            return Some(output.min(MAX_POWER));
        }
        let server = server_with_handlers::<CustomBlockComparatorOutputEvent>(args.world)?;
        let world = world_arc(&server, args.world)?;
        let state_index = info.state_index(args.state.id)?;
        let mut event = CustomBlockComparatorOutputEvent {
            world,
            position: *args.position,
            block: Block::from_id(args.block.id),
            state_index,
            face: args.direction,
            output: None,
        };
        server.plugin_manager.fire_blocking(&server, &mut event);
        event.output.map(|output| output.min(MAX_POWER))
    }
}

/// The weak or strong power of a custom block towards a reader: what the plugin stored, else the
/// answer of its handler. `args.direction` points from the reader to the block.
fn redstone_power(args: &GetRedstonePowerArgs<'_>, strong: bool) -> u8 {
    let Some(info) = args.block.dynamic_info() else {
        return 0;
    };
    let callbacks = info.callbacks();
    // The side of the block that faces the reader.
    let face = args.direction.opposite();
    if callbacks.contains(BlockCallbacks::REDSTONE_OUTPUT)
        && let Some(power) =
            args.world
                .plugin_redstone
                .face_power(args.position, args.block.id, face)
    {
        return if strong { power.strong } else { power.weak };
    }
    let asked = if strong {
        BlockCallbacks::STRONG_POWER
    } else {
        BlockCallbacks::WEAK_POWER
    };
    if !callbacks.contains(asked) {
        return 0;
    }
    let Some(server) = server_with_handlers::<CustomBlockRedstonePowerEvent>(args.world) else {
        return 0;
    };
    let (Some(world), Some(state_index)) = (
        world_arc(&server, args.world),
        info.state_index(args.state.id),
    ) else {
        return 0;
    };
    let mut event = CustomBlockRedstonePowerEvent {
        world,
        position: *args.position,
        block: Block::from_id(args.block.id),
        state_index,
        face,
        strong,
        power: 0,
    };
    server.plugin_manager.fire_blocking(&server, &mut event);
    event.power.min(MAX_POWER)
}
