use crate::block::registry::BlockActionResult;
use crate::block::{BlockBehaviour, GetScreenHandlerFactoryArgs, NormalUseArgs};

use crate::entity::player::Player;
use crate::plugin::api::events::inventory::prepare_smithing::PrepareSmithingEvent;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::translation;
use pumpkin_inventory::player::player_inventory::PlayerInventory;
use pumpkin_inventory::screen_handler::{
    InventoryPlayer, ScreenHandlerFactory, SharedScreenHandler,
};
use pumpkin_inventory::smithing_table_screen_handler::{
    SmithingContext, SmithingTableScreenHandler, set_smithing_result_hook,
};
use pumpkin_macros::pumpkin_block;
use pumpkin_util::text::TextComponent;
use std::sync::Arc;
use std::sync::Mutex;

#[pumpkin_block("minecraft:smithing_table")]
pub struct SmithingTableBlock;

impl BlockBehaviour for SmithingTableBlock {
    fn normal_use(&self, args: NormalUseArgs<'_>) -> BlockActionResult {
        if let Some(factory) = self.get_screen_handler_factory(GetScreenHandlerFactoryArgs {
            server: args.server,
            world: args.world,
            block: args.block,
            position: args.position,
            player: args.player,
        }) {
            args.player.increment_stat(
                pumpkin_data::statistic::StatisticCategory::Custom,
                pumpkin_data::statistic::CustomStatistic::InteractWithSmithingTable as i32,
                1,
            );
            args.player
                .open_handled_screen(factory.as_ref(), Some(*args.position));
        }

        BlockActionResult::Success
    }

    fn get_screen_handler_factory(
        &self,
        _args: GetScreenHandlerFactoryArgs<'_>,
    ) -> Option<Box<dyn ScreenHandlerFactory>> {
        Some(Box::new(SmithingTableScreenFactory))
    }
}

/// Fires `PrepareSmithingEvent` for a computed result and applies what plugins changed.
fn smithing_result_hook(context: &SmithingContext<'_>) -> Option<ItemStack> {
    let player = context.player?.as_any().downcast_ref::<Player>()?;
    let server = player.world().server.upgrade()?;
    if !server.plugin_manager.has_handlers::<PrepareSmithingEvent>() {
        return None;
    }
    let player = server.get_player_by_uuid(player.gameprofile.id)?;
    let key = |stack: &ItemStack| (!stack.is_empty()).then(|| stack.item.registry_key.to_string());
    let before = key(context.result);
    let mut event = PrepareSmithingEvent::new(
        player,
        before.clone(),
        key(context.template),
        key(context.base),
        key(context.addition),
    );
    server.plugin_manager.fire_blocking(&server, &mut event);
    if event.result_item == before {
        return None;
    }
    let Some(item_key) = event.result_item else {
        return Some(ItemStack::EMPTY.clone());
    };
    let item = Item::from_registry_key(item_key.strip_prefix("minecraft:").unwrap_or(&item_key))?;
    Some(ItemStack::new(1, item))
}

struct SmithingTableScreenFactory;

impl ScreenHandlerFactory for SmithingTableScreenFactory {
    fn create_screen_handler(
        &self,
        sync_id: u8,
        player_inventory: &Arc<PlayerInventory>,
        _player: &dyn InventoryPlayer,
    ) -> Option<SharedScreenHandler> {
        set_smithing_result_hook(smithing_result_hook);
        let handler: SharedScreenHandler = Arc::new(Mutex::new(SmithingTableScreenHandler::new(
            sync_id,
            player_inventory,
        )));
        Some(handler)
    }

    fn get_display_name(&self) -> TextComponent {
        TextComponent::translate(translation::java::CONTAINER_UPGRADE, [])
    }
}
