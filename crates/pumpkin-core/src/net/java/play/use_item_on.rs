#[allow(clippy::wildcard_imports)]
use super::*;
use crate::item::registry::should_try_block_placement;
use crate::plugin::player::player_use_block::PlayerUseBlockEvent;
use crate::plugin::player::player_use_item::PlayerUseItemEvent;

impl JavaClient {
    #[allow(clippy::too_many_lines)]
    pub fn handle_use_item_on(
        &self,
        player: &Arc<Player>,
        use_item_on: &SUseItemOn,
        server: &Arc<Server>,
    ) -> Result<(), BlockPlacingError> {
        if !player.has_client_loaded() {
            return Ok(());
        }
        player.update_last_action_time();
        self.update_sequence(use_item_on.sequence.0);

        let position = use_item_on.position;
        let cursor_pos = use_item_on.cursor_pos;

        let mut should_try_decrement = false;

        if !player.can_interact_with_block_at(&position, 1.0) {
            // TODO: maybe log?
            return Err(BlockPlacingError::BlockOutOfReach);
        }

        let Ok(face) = BlockDirection::try_from(use_item_on.face.0) else {
            return Err(BlockPlacingError::InvalidBlockFace);
        };

        let Ok(hand) = Hand::from_packet_id(use_item_on.hand.0) else {
            return Err(BlockPlacingError::InvalidHand);
        };

        if player.gamemode.load() == GameMode::Spectator {
            let entity = &player.get_entity();
            let world = entity.world.load_full();
            let block = world.get_block(&position);

            let event = PlayerInteractEvent::new(
                player,
                InteractAction::RightClickBlock,
                block,
                Some(position),
            );

            send_cancellable_blocking! {{
                server;
                event;
                'cancelled: {
                    let state_id = world.get_block_state_id(&position);
                    player.try_send_client_packet(&CBlockUpdate::new(
                        position,
                        VarInt(i32::from(state_id.as_u16())),
                    ));
                    return Ok(());
                }
            }}

            if let Some(factory) = server
                .block_registry
                .get_screen_handler_factory(block, player, &position, server, &world)
            {
                player.open_handled_screen(factory.as_ref(), Some(position));
            }
            return Ok(());
        }

        let inventory = player.inventory();
        let held_item = inventory.held_item();
        let off_hand_item = inventory.off_hand_item();
        let held_item_empty = held_item.is_empty();
        let off_hand_item_empty = off_hand_item.is_empty();

        let mut item = inventory.get_stack_in_hand(hand);
        let item_id = item.item.id;
        player.increment_stat(StatisticCategory::Used, item_id as i32, 1);

        let entity = &player.get_entity();
        let world = entity.world.load_full();
        let block = world.get_block(&position);

        let event = PlayerInteractEvent::new(
            player,
            InteractAction::RightClickBlock,
            block,
            Some(position),
        );

        send_cancellable_blocking! {{
            server;
            event;
            'cancelled: {
                let state_id = world.get_block_state_id(&position);
                player.try_send_client_packet(&CBlockUpdate::new(
                    position,
                    VarInt(i32::from(state_id.as_u16())),
                ));
                return Ok(());
            }
        }}

        let sneaking = player.get_entity().is_sneaking();

        // A plugin can take over the click: it gets the face, the hit position and the held item
        // before the block and the item do anything with them.
        let mut use_block_event = PlayerUseBlockEvent::new(
            player,
            hand,
            item.clone(),
            position,
            block,
            face,
            cursor_pos,
            sneaking,
        );
        server
            .plugin_manager
            .fire_blocking(server, &mut use_block_event);
        if use_block_event.cancelled {
            Self::resync_after_cancelled_use(player, &position, face, hand, &item);
            return Ok(());
        }

        // A plugin can take the click away from the block and the item.
        if server
            .plugin_manager
            .has_handlers::<crate::plugin::api::events::entity::ItemUseFirstEvent>()
        {
            use crate::plugin::api::events::entity::ItemUseFirstResult;
            let mut use_first_event = crate::plugin::api::events::entity::ItemUseFirstEvent::new(
                player.clone(),
                hand,
                item.clone(),
                position,
                face,
            );
            server
                .plugin_manager
                .fire_blocking(server, &mut use_first_event);
            match use_first_event.result {
                ItemUseFirstResult::Pass => {}
                ItemUseFirstResult::Consume => return Ok(()),
                ItemUseFirstResult::Success => {
                    player.swing_hand(hand, true);
                    return Ok(());
                }
            }
        }

        let equipment_slot = if matches!(hand, Hand::Right) {
            EquipmentSlot::MAIN_HAND
        } else {
            EquipmentSlot::OFF_HAND
        };

        // Code based on the java class ServerPlayerInteractionManager
        if !(sneaking && (!held_item_empty || !off_hand_item_empty)) {
            let result = Self::call_use_item_on(
                player,
                &position,
                &cursor_pos,
                face,
                &mut item,
                &equipment_slot,
                &world,
                block,
                server,
            );
            if result.consumes_action() {
                // TODO: Trigger ANY_BLOCK_USE Criteria

                if matches!(result, BlockActionResult::SuccessServer) {
                    player.swing_hand(hand, true);
                }
                return Ok(());
            }
        }

        let slot_index = if matches!(hand, Hand::Right) {
            inventory.get_selected_slot() as usize
        } else {
            PlayerInventory::OFF_HAND_SLOT
        };

        if item.is_empty() {
            // TODO item cool down
            // If the hand is empty we stop here
            return Ok(());
        }

        let mut use_item_event =
            PlayerUseItemEvent::new(player, hand, item.clone(), Some(position), Some(face));
        server
            .plugin_manager
            .fire_blocking(server, &mut use_item_event);
        if use_item_event.cancelled {
            Self::resync_after_cancelled_use(player, &position, face, hand, &item);
            return Ok(());
        }

        let before = item.clone();

        let item_result = server
            .item_registry
            .use_on_block(&mut item, player, position, face, cursor_pos, block, server);

        if should_try_block_placement(&item_result) {
            // Check if the item is a block, because not every item can be placed :D
            let item_id = item.item.id;
            if let Some(block) = Block::from_item_id(item_id) {
                should_try_decrement =
                    Self::run_is_block_place(player, block, server, use_item_on, position, face)?;
            }
        }

        if should_try_decrement {
            // TODO: Config
            // Decrease block count
            if player.gamemode.load() != GameMode::Creative {
                item.decrement(1);
            }
        }

        let after = item.clone();

        if matches!(item_result, BlockActionResult::SuccessServer) {
            player.swing_hand(hand, true);
        }

        // Broadcast the break entity status before the slot sync; the client
        // needs the old item texture in the slot for break particles.
        if !before.is_empty() && after.is_empty() {
            let slot = if slot_index == player.inventory.get_selected_slot() as usize {
                &EquipmentSlot::MAIN_HAND
            } else {
                &EquipmentSlot::OFF_HAND
            };
            if before.is_damageable() {
                player.increment_stat(StatisticCategory::Broken, before.item.id as i32, 1);
            }
            player.world().send_entity_status(
                player.get_entity(),
                equipment_break_status(slot),
                None,
            );
        }

        if !after.are_equal(&before) {
            player.sync_hand_slot(slot_index, after.clone());
            inventory.set_stack_in_hand(hand, after);
        }

        Ok(())
    }

    /// Puts the client back in step after a plugin cancelled a click: the clicked block, the block a
    /// placement would have created next to it, and the held item the client may have used up.
    fn resync_after_cancelled_use(
        player: &Arc<Player>,
        position: &BlockPos,
        face: BlockDirection,
        hand: Hand,
        held: &ItemStack,
    ) {
        let world = player.world();
        for block_position in [*position, position.offset(face.to_offset())] {
            let state_id = world.get_block_state_id(&block_position);
            player.try_send_client_packet(&CBlockUpdate::new(
                block_position,
                VarInt(i32::from(state_id.as_u16())),
            ));
        }
        let slot_index = if matches!(hand, Hand::Right) {
            player.inventory().get_selected_slot() as usize
        } else {
            PlayerInventory::OFF_HAND_SLOT
        };
        player.sync_hand_slot(slot_index, held.clone());
    }

    #[expect(clippy::too_many_arguments)]
    fn call_use_item_on(
        player: &Arc<Player>,
        position: &BlockPos,
        cursor_pos: &Vector3<f32>,
        face: BlockDirection,
        held_item: &mut ItemStack,
        equipment_slot: &EquipmentSlot,
        world: &Arc<World>,
        block: &Block,
        server: &Arc<Server>,
    ) -> BlockActionResult {
        let result = server.block_registry.use_with_item(
            block,
            player,
            position,
            &BlockHitResult {
                face: &face,
                cursor_pos,
            },
            held_item,
            equipment_slot,
            server,
            world,
        );

        if result.consumes_action() {
            // TODO: Trigger ITEM_USED_ON_BLOCK Criteria
            return result;
        }

        if matches!(result, BlockActionResult::PassToDefaultBlockAction) {
            let result = server.block_registry.on_use(
                block,
                player,
                position,
                &BlockHitResult {
                    face: &face,
                    cursor_pos,
                },
                server,
                world,
            );

            if result.consumes_action() {
                // TODO: Trigger DEFAULT_BLOCK_USE Criteria
                return result;
            }
        }

        BlockActionResult::Pass
    }

    fn run_is_block_place(
        player: &Arc<Player>,
        block: &'static Block,
        server: &Arc<Server>,
        use_item_on: &SUseItemOn,
        location: BlockPos,
        face: BlockDirection,
    ) -> Result<bool, BlockPlacingError> {
        if player.is_fake
            && server
                .plugin_manager
                .has_handlers::<crate::plugin::api::events::entity::FakePlayerPlaceCheckEvent>()
        {
            let world = player.world();
            let clicked = world.get_block_state(&location);
            let mut check = crate::plugin::api::events::entity::FakePlayerPlaceCheckEvent::new(
                player.clone(),
                player.inventory().get_stack_in_hand(
                    Hand::from_packet_id(use_item_on.hand.0).unwrap_or(Hand::Right),
                ),
                location,
                face,
                clicked.replaceable(),
            );
            server.plugin_manager.fire_blocking(server, &mut check);
            if check.cancelled {
                return Ok(false);
            }
        }

        match server
            .block_registry
            .place_block(player, block, server, use_item_on, location, face)
        {
            Ok(Some((final_block_pos, new_state))) => {
                player.try_send_client_packet(&CBlockUpdate::new(
                    final_block_pos,
                    VarInt(i32::from(new_state.as_u16())),
                ));
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(crate::block::registry::BlockPlacingError::InvalidGamemode) => {
                Err(BlockPlacingError::InvalidGamemode)
            }
            Err(crate::block::registry::BlockPlacingError::BlockOutOfWorld) => {
                Err(BlockPlacingError::BlockOutOfWorld)
            }
        }
    }
}
