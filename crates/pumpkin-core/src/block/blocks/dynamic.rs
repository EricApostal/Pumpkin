use std::sync::Arc;

use crate::block::registry::BlockActionResult;
use crate::block::{
    AttackArgs, BlockBehaviour, BrokenArgs, CanPlaceAtArgs, EmitsRedstonePowerArgs,
    GetComparatorOutputArgs, GetRedstonePowerArgs, GetStateForNeighborUpdateArgs, NormalUseArgs,
    OnEntityCollisionArgs, OnEntityStepArgs, OnLandedUponArgs, OnNeighborUpdateArgs, OnPlaceArgs,
    OnScheduledTickArgs, OnStateReplacedArgs, PathComputationType, PlacedArgs, PlayerPlacedArgs,
    RandomTickArgs, UseWithItemArgs,
};
use crate::entity::EntityBase;
use crate::entity::player::Player;
use crate::plugin::Payload;
use crate::plugin::block::{
    CustomBlockAttackEvent, CustomBlockBrokenEvent, CustomBlockCanPlaceEvent,
    CustomBlockCloneItemEvent, CustomBlockCollisionShapeEvent, CustomBlockComparatorOutputEvent,
    CustomBlockCondition, CustomBlockConditionEvent, CustomBlockEntityCollisionEvent,
    CustomBlockEntityStepEvent, CustomBlockFireSpreadEvent, CustomBlockLandedEvent,
    CustomBlockNeighborUpdateEvent, CustomBlockPhysicsEvent, CustomBlockPhysicsKind,
    CustomBlockPlacedEvent, CustomBlockPlayerWillDestroyEvent, CustomBlockRandomTickEvent,
    CustomBlockRedstonePowerEvent, CustomBlockScheduledTickEvent, CustomBlockShapeEvent,
    CustomBlockSoundTypeEvent, CustomBlockStateReplacedEvent, CustomBlockUpdateShapeEvent,
    CustomBlockUseEvent, CustomBlockUseResult,
};
use crate::server::Server;
use crate::world::World;
use crate::world::plugin_shapes::CACHED_CALLBACKS;
use pumpkin_data::block_registry::{
    BlockCallbacks, DynamicBlockInfo, PlacementContext, PlacementNeighbour, ShapeKind,
};
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::{
    Block, BlockDirection, BlockState, BlockStateId, FacingExt, HorizontalFacingExt,
};
use pumpkin_util::Hand;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

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
        // The placement rules that look at a neighbour (`neighbour-shaft`) read these.
        let neighbour = |direction: BlockDirection| {
            let (block, state_id) = args
                .world
                .get_block_and_state_id(&args.position.offset(direction.to_offset()));
            PlacementNeighbour { block, state_id }
        };
        // `TrackBlock.getStateForPlacement`: the neighbour along the axis has a solid top and the
        // block above it has no solid bottom.
        let ascend = |direction: BlockDirection| {
            let beside = args.position.offset(direction.to_offset());
            let (_, beside_state) = args.world.get_block_and_state_id(&beside);
            if !beside_state.is_side_solid(BlockDirection::Up) {
                return false;
            }
            let (_, above_state) = args.world.get_block_and_state_id(&beside.up());
            !above_state.is_side_solid(BlockDirection::Down)
        };
        let context = PlacementContext {
            // `direction` points from the placed block to the clicked one.
            clicked_face: args.direction.opposite(),
            horizontal_facing: entity.get_horizontal_facing().to_block_direction(),
            looking_direction: entity.get_facing().to_block_direction(),
            pitch: entity.pitch.load(),
            yaw: entity.yaw.load(),
            cursor_y: args.use_item_on.cursor_pos.y,
            in_water: args.replacing.water_source(),
            sneaking: entity.is_sneaking(),
            neighbours: [
                neighbour(BlockDirection::Down),
                neighbour(BlockDirection::Up),
                neighbour(BlockDirection::North),
                neighbour(BlockDirection::South),
                neighbour(BlockDirection::West),
                neighbour(BlockDirection::East),
            ],
            ascend: [
                ascend(BlockDirection::Down),
                ascend(BlockDirection::Up),
                ascend(BlockDirection::North),
                ascend(BlockDirection::South),
                ascend(BlockDirection::West),
                ascend(BlockDirection::East),
            ],
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
        if event.new_state_index != new_index
            && u32::from(event.new_state_index) < info.state_count()
        {
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
        // Output left behind by a replacement without the callbacks must not carry over.
        if opted_in_any(args.block, STORED_OUTPUT).is_some() {
            args.world.plugin_redstone.remove(args.position);
        }
        if opted_in_any(args.block, CACHED_CALLBACKS).is_some() {
            args.world.plugin_block_cache.invalidate(args.position);
        }
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
        if opted_in_any(args.block, CACHED_CALLBACKS).is_some() {
            args.world.plugin_block_cache.invalidate(args.position);
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

    fn normal_use(&self, args: NormalUseArgs<'_>) -> BlockActionResult {
        let state_id = args.world.get_block_state_id(args.position);
        let Some(hook) = hook::<CustomBlockUseEvent>(
            args.world,
            args.block,
            state_id,
            BlockCallbacks::USE_WITHOUT_ITEM,
        ) else {
            return BlockActionResult::Pass;
        };
        // The click does not tell the hand here, the main hand is what a plain click uses.
        fire_use(
            &hook,
            args.block,
            args.position,
            args.player,
            Hand::Right,
            false,
            args.player.inventory().held_item(),
            *args.hit.face,
            args.hit.cursor_pos,
        )
    }

    fn use_with_item(&self, args: UseWithItemArgs<'_>) -> BlockActionResult {
        let state_id = args.world.get_block_state_id(args.position);
        let Some(hook) = hook::<CustomBlockUseEvent>(
            args.world,
            args.block,
            state_id,
            BlockCallbacks::USE_WITH_ITEM,
        ) else {
            return BlockActionResult::PassToDefaultBlockAction;
        };
        let hand = if matches!(args.equipment_slot, EquipmentSlot::OffHand(_)) {
            Hand::Left
        } else {
            Hand::Right
        };
        fire_use(
            &hook,
            args.block,
            args.position,
            args.player,
            hand,
            true,
            args.item_stack.clone(),
            *args.hit.face,
            args.hit.cursor_pos,
        )
    }

    fn attack(&self, args: AttackArgs<'_>) {
        let Some(hook) = hook::<CustomBlockAttackEvent>(
            args.world,
            args.block,
            args.state.id,
            BlockCallbacks::ATTACK,
        ) else {
            return;
        };
        let mut event = CustomBlockAttackEvent {
            world: hook.world.clone(),
            position: *args.position,
            block: Block::from_id(args.block.id),
            state_index: hook.state_index,
            player: args.player.clone(),
        };
        hook.server
            .plugin_manager
            .fire_blocking(&hook.server, &mut event);
    }

    fn on_landed_upon(&self, args: OnLandedUponArgs<'_>) {
        let state_id = args.world.get_block_state_id(args.position);
        let mut multiplier = 1.0;
        if let Some(hook) = hook::<CustomBlockLandedEvent>(
            args.world,
            args.block,
            state_id,
            BlockCallbacks::ON_LANDED_UPON,
        ) {
            let mut event = CustomBlockLandedEvent {
                world: hook.world.clone(),
                position: *args.position,
                block: Block::from_id(args.block.id),
                state_index: hook.state_index,
                entity_id: args.entity.get_entity().entity_id,
                fall_distance: args.fall_distance,
                damage_multiplier: multiplier,
            };
            hook.server
                .plugin_manager
                .fire_blocking(&hook.server, &mut event);
            if event.damage_multiplier.is_finite() {
                multiplier = event.damage_multiplier.max(0.0);
            }
        }
        if multiplier > 0.0
            && let Some(living) = args.entity.get_living_entity()
        {
            living.handle_fall_damage(args.entity, args.fall_distance, multiplier);
        }
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

/// The most boxes a plugin can give for one shape.
const MAX_SHAPE_BOXES: usize = 64;

/// What firing the event of an opted in hook needs.
struct Hook {
    server: Arc<Server>,
    world: Arc<World>,
    state_index: u16,
}

/// The pieces to fire `E` for `block`, when it opted in to `callback` and a plugin handles `E`.
fn hook<E: Payload + 'static>(
    world: &World,
    block: &Block,
    state_id: BlockStateId,
    callback: BlockCallbacks,
) -> Option<Hook> {
    let info = opted_in(block, callback)?;
    let server = server_with_handlers::<E>(world)?;
    let world = world_arc(&server, world)?;
    let state_index = info.state_index(state_id)?;
    Some(Hook {
        server,
        world,
        state_index,
    })
}

#[expect(clippy::too_many_arguments)]
fn fire_use(
    hook: &Hook,
    block: &Block,
    position: &BlockPos,
    player: &Arc<Player>,
    hand: Hand,
    with_item: bool,
    item: ItemStack,
    face: BlockDirection,
    cursor: &Vector3<f32>,
) -> BlockActionResult {
    let default = if with_item {
        CustomBlockUseResult::TryEmptyHand
    } else {
        CustomBlockUseResult::Pass
    };
    let mut event = CustomBlockUseEvent {
        world: hook.world.clone(),
        position: *position,
        block: Block::from_id(block.id),
        state_index: hook.state_index,
        player: player.clone(),
        hand,
        with_item,
        item,
        face,
        cursor: *cursor,
        sneaking: player.get_entity().is_sneaking(),
        result: default,
    };
    hook.server
        .plugin_manager
        .fire_blocking(&hook.server, &mut event);
    match event.result {
        CustomBlockUseResult::Success => BlockActionResult::Success,
        CustomBlockUseResult::Consume => BlockActionResult::Consume,
        CustomBlockUseResult::Fail => BlockActionResult::Fail,
        CustomBlockUseResult::TryEmptyHand if with_item => {
            BlockActionResult::PassToDefaultBlockAction
        }
        CustomBlockUseResult::Pass | CustomBlockUseResult::TryEmptyHand => BlockActionResult::Pass,
    }
}

/// Whether a box from a plugin can be used: finite and not inverted.
fn usable_box(shape: &BoundingBox) -> bool {
    [
        shape.min.x,
        shape.min.y,
        shape.min.z,
        shape.max.x,
        shape.max.y,
        shape.max.z,
    ]
    .iter()
    .all(|value| value.is_finite())
        && shape.min.x <= shape.max.x
        && shape.min.y <= shape.max.y
        && shape.min.z <= shape.max.z
}

/// The shape a plugin gives for a block with the `dynamic_shape` callback, in block units. Cached
/// per position. `None` when the block did not opt in or no plugin listens, then the shapes of the
/// state apply.
#[must_use]
pub fn dynamic_shapes(
    world: &World,
    position: &BlockPos,
    state: &BlockState,
    kind: ShapeKind,
) -> Option<Arc<[BoundingBox]>> {
    let block = Block::from_state_id(state.id);
    opted_in(block, BlockCallbacks::DYNAMIC_SHAPE)?;
    if let Some(shapes) = world.plugin_block_cache.shape(position, state.id, kind) {
        return Some(shapes);
    }
    let hook =
        hook::<CustomBlockShapeEvent>(world, block, state.id, BlockCallbacks::DYNAMIC_SHAPE)?;
    // The registered shape of the state is where the plugin starts from.
    let shapes: Vec<BoundingBox> = match kind {
        ShapeKind::Selection => state.get_block_outline_shapes().collect(),
        ShapeKind::Collision | ShapeKind::Visual => state.get_block_collision_shapes().collect(),
    };
    let mut event = CustomBlockShapeEvent {
        world: hook.world.clone(),
        position: *position,
        block,
        state_index: hook.state_index,
        kind,
        shapes,
    };
    hook.server
        .plugin_manager
        .fire_blocking(&hook.server, &mut event);
    let shapes: Arc<[BoundingBox]> = event
        .shapes
        .into_iter()
        .filter(usable_box)
        .take(MAX_SHAPE_BOXES)
        .collect::<Vec<_>>()
        .into();
    world
        .plugin_block_cache
        .store_shape(position, state.id, kind, shapes.clone());
    Some(shapes)
}

/// The collision shape a plugin gives for a block with the `entity_collision_shape` callback while
/// `entity` moves, in block units. `None` when the block did not opt in or no plugin listens, then
/// the collision shape of the state applies. Never cached: the answer depends on the entity.
#[must_use]
pub fn entity_collision_shapes(
    world: &World,
    position: &BlockPos,
    state: &BlockState,
    entity: &dyn EntityBase,
) -> Option<Vec<BoundingBox>> {
    // Vanilla blocks are the bulk of the lookups, they have no hooks.
    if state.id.as_u32() < u32::from(Block::vanilla_state_count()) {
        return None;
    }
    let block = Block::from_state_id(state.id);
    opted_in(block, BlockCallbacks::ENTITY_COLLISION_SHAPE)?;
    let hook = hook::<CustomBlockCollisionShapeEvent>(
        world,
        block,
        state.id,
        BlockCallbacks::ENTITY_COLLISION_SHAPE,
    )?;
    let entity = entity.get_entity();
    // The registered collision shape of the state is where the plugin starts from, or what the
    // `dynamic-shape` hook already answered for the position when the block has both.
    let shapes = world
        .plugin_block_cache
        .shape(position, state.id, ShapeKind::Collision)
        .map_or_else(
            || state.get_block_collision_shapes().collect(),
            |shapes| shapes.to_vec(),
        );
    let mut event = CustomBlockCollisionShapeEvent {
        world: hook.world.clone(),
        position: *position,
        block: Block::from_id(block.id),
        state_index: hook.state_index,
        entity_id: entity.entity_id,
        entity_uuid: entity.entity_uuid,
        entity_type: entity.entity_type,
        entity_position: entity.pos.load(),
        shapes,
    };
    hook.server
        .plugin_manager
        .fire_blocking(&hook.server, &mut event);
    Some(
        event
            .shapes
            .into_iter()
            .filter(usable_box)
            .take(MAX_SHAPE_BOXES)
            .collect(),
    )
}

/// The friction, speed factor or jump factor a plugin gives for a block with the matching
/// callback. Cached per position. `None` when the block did not opt in or no plugin listens.
#[must_use]
pub fn physics_value(
    world: &World,
    position: &BlockPos,
    block: &Block,
    state: &BlockState,
    kind: CustomBlockPhysicsKind,
) -> Option<f32> {
    let callback = match kind {
        CustomBlockPhysicsKind::Friction => BlockCallbacks::GET_FRICTION,
        CustomBlockPhysicsKind::SpeedFactor => BlockCallbacks::GET_SPEED_FACTOR,
        CustomBlockPhysicsKind::JumpFactor => BlockCallbacks::GET_JUMP_FACTOR,
    };
    opted_in(block, callback)?;
    if let Some(value) = world.plugin_block_cache.physics(position, state.id, kind) {
        return Some(value);
    }
    let hook = hook::<CustomBlockPhysicsEvent>(world, block, state.id, callback)?;
    let default = match kind {
        CustomBlockPhysicsKind::Friction => block.slipperiness,
        CustomBlockPhysicsKind::SpeedFactor => block.velocity_multiplier,
        CustomBlockPhysicsKind::JumpFactor => block.jump_velocity_multiplier,
    };
    let mut event = CustomBlockPhysicsEvent {
        world: hook.world.clone(),
        position: *position,
        block: Block::from_id(block.id),
        state_index: hook.state_index,
        kind,
        value: default,
    };
    hook.server
        .plugin_manager
        .fire_blocking(&hook.server, &mut event);
    let value = if event.value.is_finite() {
        event.value
    } else {
        default
    };
    world
        .plugin_block_cache
        .store_physics(position, state.id, kind, value);
    Some(value)
}

/// Asks the plugin a yes or no question about a block that opted in to `callback`.
#[expect(clippy::too_many_arguments)]
fn ask(
    world: &World,
    position: &BlockPos,
    block: &Block,
    state: &BlockState,
    callback: BlockCallbacks,
    condition: CustomBlockCondition,
    entity_type: Option<&'static EntityType>,
    direction: Option<BlockDirection>,
    default: bool,
) -> Option<bool> {
    let hook = hook::<CustomBlockConditionEvent>(world, block, state.id, callback)?;
    let mut event = CustomBlockConditionEvent {
        world: hook.world.clone(),
        position: *position,
        block: Block::from_id(block.id),
        state_index: hook.state_index,
        condition,
        entity_type,
        direction,
        value: default,
    };
    hook.server
        .plugin_manager
        .fire_blocking(&hook.server, &mut event);
    Some(event.value)
}

/// Whether an entity (`None` for an explosion) can destroy the block, true unless a custom block
/// with the `can_entity_destroy` callback says otherwise.
#[must_use]
pub fn entity_can_destroy(
    world: &World,
    position: &BlockPos,
    block: &Block,
    state: &BlockState,
    entity_type: Option<&'static EntityType>,
) -> bool {
    if !block.is_dynamic() {
        return true;
    }
    ask(
        world,
        position,
        block,
        state,
        BlockCallbacks::CAN_ENTITY_DESTROY,
        CustomBlockCondition::EntityDestroy,
        entity_type,
        None,
        true,
    )
    .unwrap_or(true)
}

/// The answer of a custom block with the `is_pathfindable` callback, `None` to use the default.
#[must_use]
pub fn pathfindable(
    world: &World,
    position: &BlockPos,
    block: &Block,
    state: &BlockState,
    computation_type: PathComputationType,
) -> Option<bool> {
    if !block.is_dynamic() {
        return None;
    }
    let (condition, default) = match computation_type {
        PathComputationType::Land => (
            CustomBlockCondition::PathfindableLand,
            !state.is_full_cube(),
        ),
        PathComputationType::Air => (CustomBlockCondition::PathfindableAir, !state.is_full_cube()),
        PathComputationType::Water => (
            CustomBlockCondition::PathfindableWater,
            state.is_waterlogged(),
        ),
    };
    ask(
        world,
        position,
        block,
        state,
        BlockCallbacks::IS_PATHFINDABLE,
        condition,
        None,
        None,
        default,
    )
}

/// The answer of a custom block with the `can_connect_redstone` callback about wire to its
/// `direction` side, `None` to use the default.
#[must_use]
pub fn connects_redstone(
    world: &World,
    position: &BlockPos,
    block: &Block,
    state: &BlockState,
    direction: Option<BlockDirection>,
) -> Option<bool> {
    if !block.is_dynamic() {
        return None;
    }
    let default =
        opted_in_any(block, BlockCallbacks::REDSTONE_SOURCE).is_some() && direction.is_some();
    ask(
        world,
        position,
        block,
        state,
        BlockCallbacks::CAN_CONNECT_REDSTONE,
        CustomBlockCondition::RedstoneConnect,
        None,
        direction,
        default,
    )
}

/// The sound type of a custom block: what its plugin says with the `get_sound_type` callback, else
/// the one it was registered with. `None` for a vanilla block.
#[must_use]
pub fn sound_type(
    world: &World,
    position: &BlockPos,
    block: &Block,
    state: &BlockState,
) -> Option<String> {
    let info = block.dynamic_info()?;
    let registered = info.registration.sound_type.clone();
    let Some(hook) =
        hook::<CustomBlockSoundTypeEvent>(world, block, state.id, BlockCallbacks::GET_SOUND_TYPE)
    else {
        return Some(registered);
    };
    let mut event = CustomBlockSoundTypeEvent {
        world: hook.world.clone(),
        position: *position,
        block: Block::from_id(block.id),
        state_index: hook.state_index,
        sound_type: registered.clone(),
    };
    hook.server
        .plugin_manager
        .fire_blocking(&hook.server, &mut event);
    Some(if event.sound_type.is_empty() {
        registered
    } else {
        event.sound_type
    })
}

/// How a custom block with the `get_fire_spread` callback burns, `(spread chance, burn chance)`.
/// `None` when it did not opt in, no plugin listens or it does not burn.
#[must_use]
pub fn flammability(
    world: &World,
    position: &BlockPos,
    block: &Block,
    state: &BlockState,
) -> Option<(u8, u8)> {
    if !block.is_dynamic() {
        return None;
    }
    let hook = hook::<CustomBlockFireSpreadEvent>(
        world,
        block,
        state.id,
        BlockCallbacks::GET_FIRE_SPREAD,
    )?;
    let mut event = CustomBlockFireSpreadEvent {
        world: hook.world.clone(),
        position: *position,
        block: Block::from_id(block.id),
        state_index: hook.state_index,
        spread_chance: 0,
        burn_chance: 0,
    };
    hook.server
        .plugin_manager
        .fire_blocking(&hook.server, &mut event);
    (event.burn_chance > 0).then_some((event.spread_chance, event.burn_chance))
}

/// Whether a player may break the block: false when a custom block with the `player_will_destroy`
/// callback was cancelled by its plugin.
#[must_use]
pub fn player_will_destroy(
    world: &Arc<World>,
    position: &BlockPos,
    block: &Block,
    state: &BlockState,
    player: &Arc<Player>,
) -> bool {
    if !block.is_dynamic() {
        return true;
    }
    let Some(hook) = hook::<CustomBlockPlayerWillDestroyEvent>(
        world,
        block,
        state.id,
        BlockCallbacks::PLAYER_WILL_DESTROY,
    ) else {
        return true;
    };
    let mut event = CustomBlockPlayerWillDestroyEvent {
        world: hook.world.clone(),
        position: *position,
        block: Block::from_id(block.id),
        state_index: hook.state_index,
        player: player.clone(),
        cancelled: false,
    };
    hook.server
        .plugin_manager
        .fire_blocking(&hook.server, &mut event);
    !event.cancelled
}

/// The item a player gets when picking the block.
///
/// The outer `None` means the block did not opt in
/// to `get_clone_item` (or no plugin listens), then the default applies; the inner one is a plugin
/// that wants nothing picked.
#[must_use]
pub fn clone_item(
    world: &Arc<World>,
    position: &BlockPos,
    block: &Block,
    state: &BlockState,
    player: &Arc<Player>,
    default: Option<ItemStack>,
) -> Option<Option<ItemStack>> {
    if !block.is_dynamic() {
        return None;
    }
    let hook =
        hook::<CustomBlockCloneItemEvent>(world, block, state.id, BlockCallbacks::GET_CLONE_ITEM)?;
    let mut event = CustomBlockCloneItemEvent {
        world: hook.world.clone(),
        position: *position,
        block: Block::from_id(block.id),
        state_index: hook.state_index,
        player: player.clone(),
        item: default,
    };
    hook.server
        .plugin_manager
        .fire_blocking(&hook.server, &mut event);
    Some(event.item.filter(|item| !item.is_empty()))
}
