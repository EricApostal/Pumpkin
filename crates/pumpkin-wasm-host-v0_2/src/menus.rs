use std::sync::Arc;

use crate::pumpkin::plugin::item_stack::ItemStack as WitItemStack;
use crate::pumpkin::plugin::menus::{
    Host, HostWithStore, MenuClickRules, MenuOptions, MenuSlotKind, MenuTypeEntry, OpenMenuInfo,
};
use crate::pumpkin::plugin::player::Player as WitPlayer;
use crate::pumpkin::plugin::text::TextComponent as WitTextComponent;
use pumpkin_core::plugin::api::menu::{
    MenuClickRules as CoreClickRules, MenuSlotKind as CoreSlotKind, MenuSlotRule as CoreSlotRule,
    PluginMenuOptions,
};
use pumpkin_core::plugin::permissions;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::menu_registry::{
    plugin_menu_type_by_key, plugin_menu_types, register_plugin_menu_type, vanilla_menu_type_count,
};
use pumpkin_wasm_host_common::state::PluginHostState;
use tokio::sync::Mutex;
use tracing::info;
use wasmtime::component::{Access, HasSelf, Resource};

impl Host for PluginHostState {
    fn register_menu_type(&mut self, key: String) -> wasmtime::Result<Result<u32, String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::REGISTRY_MENUS)
        {
            return Ok(Err(self.missing_permission(
                "registering menu types",
                permissions::REGISTRY_MENUS,
            )));
        }
        Ok(match register_plugin_menu_type(&key) {
            Ok(menu_type) => {
                info!(
                    "Plugin {} registered menu type {} with id {}",
                    self.name.as_deref().unwrap_or("<unknown>"),
                    menu_type.key,
                    menu_type.id
                );
                Ok(menu_type.id)
            }
            Err(error) => Err(format!("menu type '{key}': {error}")),
        })
    }

    fn get_menu_type_id(&mut self, key: String) -> wasmtime::Result<Option<u32>> {
        Ok(plugin_menu_type_by_key(&key).map(|menu_type| menu_type.id))
    }

    fn get_vanilla_menu_type_count(&mut self) -> wasmtime::Result<u32> {
        Ok(vanilla_menu_type_count())
    }

    fn get_registered_menu_types(&mut self) -> wasmtime::Result<Vec<MenuTypeEntry>> {
        Ok(plugin_menu_types()
            .into_iter()
            .map(|menu_type| MenuTypeEntry {
                key: menu_type.key.to_string(),
                id: menu_type.id,
            })
            .collect())
    }

    fn get_open_menu(
        &mut self,
        player: Resource<WitPlayer>,
    ) -> wasmtime::Result<Option<OpenMenuInfo>> {
        let player = Arc::clone(self.get(&player)?);
        Ok(player
            .open_plugin_menu_info()
            .map(|(container_id, menu_type, slot_count)| OpenMenuInfo {
                container_id: u32::from(container_id),
                menu_type: menu_type.key.to_string(),
                slot_count: slot_count as u32,
            }))
    }

    async fn set_menu_slot(
        &mut self,
        player: Resource<WitPlayer>,
        slot: u32,
        stack: Resource<WitItemStack>,
    ) -> wasmtime::Result<Result<(), String>> {
        let player = Arc::clone(self.get(&player)?);
        let stack = {
            let stack = self.get(&stack)?;
            stack.lock().await.clone()
        };
        Ok(player.set_plugin_menu_slot(slot as usize, stack))
    }

    fn clear_menu_slot(
        &mut self,
        player: Resource<WitPlayer>,
        slot: u32,
    ) -> wasmtime::Result<Result<(), String>> {
        let player = Arc::clone(self.get(&player)?);
        Ok(player.set_plugin_menu_slot(slot as usize, ItemStack::EMPTY.clone()))
    }

    fn set_menu_data(
        &mut self,
        player: Resource<WitPlayer>,
        id: u32,
        value: i16,
    ) -> wasmtime::Result<Result<(), String>> {
        let player = Arc::clone(self.get(&player)?);
        let Ok(id) = i16::try_from(id) else {
            return Ok(Err(format!("{id} is not a data slot id")));
        };
        Ok(player.set_plugin_menu_data(id, value))
    }

    fn set_menu_click_rules(
        &mut self,
        player: Resource<WitPlayer>,
        rules: MenuClickRules,
    ) -> wasmtime::Result<Result<(), String>> {
        let player = Arc::clone(self.get(&player)?);
        Ok(player.set_plugin_menu_rules(&from_wit_rules(rules)))
    }

    fn get_menu_cursor(
        &mut self,
        player: Resource<WitPlayer>,
    ) -> wasmtime::Result<Option<Resource<WitItemStack>>> {
        let player = Arc::clone(self.get(&player)?);
        match player.plugin_menu_cursor() {
            Some(stack) if !stack.is_empty() => Ok(Some(self.add(Arc::new(Mutex::new(stack)))?)),
            _ => Ok(None),
        }
    }

    async fn set_menu_cursor(
        &mut self,
        player: Resource<WitPlayer>,
        stack: Option<Resource<WitItemStack>>,
    ) -> wasmtime::Result<Result<(), String>> {
        let player = Arc::clone(self.get(&player)?);
        let stack = match stack {
            Some(stack) => self.take(stack)?.lock().await.clone(),
            None => ItemStack::EMPTY.clone(),
        };
        Ok(player.set_plugin_menu_cursor(stack))
    }
}

fn from_wit_rules(rules: MenuClickRules) -> CoreClickRules {
    CoreClickRules {
        slot_rules: rules
            .slot_rules
            .into_iter()
            .map(|rule| CoreSlotRule {
                slot: rule.slot as usize,
                kind: match rule.kind {
                    MenuSlotKind::Normal => CoreSlotKind::Normal,
                    MenuSlotKind::OutputOnly => CoreSlotKind::OutputOnly,
                    MenuSlotKind::Locked => CoreSlotKind::Locked,
                    MenuSlotKind::Ghost => CoreSlotKind::Ghost,
                    MenuSlotKind::Filter(keys) => CoreSlotKind::Filter(keys),
                },
                max_stack_size: rule.max_stack_size,
            })
            .collect(),
        quick_move_targets: rules
            .quick_move_targets
            .into_iter()
            .map(|range| range.start as usize..range.end as usize)
            .collect(),
    }
}

async fn open_menu_with_store(
    mut host: Access<'_, PluginHostState, HasSelf<PluginHostState>>,
    player: Resource<WitPlayer>,
    menu_type_key: String,
    title: Resource<WitTextComponent>,
    extra_data: Option<Vec<u8>>,
    slot_count: u32,
    options: PluginMenuOptions,
) -> wasmtime::Result<Result<u32, String>> {
    let (player, title, plugin) = {
        let state = host.get();
        let player = Arc::clone(state.get(&player)?);
        let title = state.take(title)?;
        let plugin = state
            .plugin
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| wasmtime::Error::msg("Plugin instance not available"))?;
        (player, title, plugin)
    };
    let Some(menu_type) = plugin_menu_type_by_key(&menu_type_key) else {
        return Ok(Err(format!(
            "menu type '{menu_type_key}' is not registered"
        )));
    };

    plugin
        .store
        .pump_blocking(&mut host, move || {
            player.open_plugin_menu_with_options(
                menu_type,
                &title,
                extra_data.as_deref(),
                slot_count as usize,
                &options,
            )
        })
        .await
}

impl HostWithStore<PluginHostState> for HasSelf<PluginHostState> {
    async fn open_menu(
        host: Access<'_, PluginHostState, Self>,
        player: Resource<WitPlayer>,
        menu_type_key: String,
        title: Resource<WitTextComponent>,
        extra_data: Option<Vec<u8>>,
    ) -> wasmtime::Result<Result<u32, String>> {
        open_menu_with_store(
            host,
            player,
            menu_type_key,
            title,
            extra_data,
            0,
            PluginMenuOptions::default(),
        )
        .await
    }

    async fn open_menu_with_slots(
        host: Access<'_, PluginHostState, Self>,
        player: Resource<WitPlayer>,
        menu_type_key: String,
        title: Resource<WitTextComponent>,
        extra_data: Option<Vec<u8>>,
        slot_count: u32,
    ) -> wasmtime::Result<Result<u32, String>> {
        open_menu_with_store(
            host,
            player,
            menu_type_key,
            title,
            extra_data,
            slot_count,
            PluginMenuOptions::default(),
        )
        .await
    }

    async fn open_menu_with_options(
        host: Access<'_, PluginHostState, Self>,
        player: Resource<WitPlayer>,
        menu_type_key: String,
        title: Resource<WitTextComponent>,
        extra_data: Option<Vec<u8>>,
        slot_count: u32,
        options: MenuOptions,
    ) -> wasmtime::Result<Result<u32, String>> {
        let options = PluginMenuOptions {
            apply_vanilla_clicks: options.apply_vanilla_clicks,
            player_inventory: options.player_inventory,
            rules: from_wit_rules(options.rules),
        };
        open_menu_with_store(
            host,
            player,
            menu_type_key,
            title,
            extra_data,
            slot_count,
            options,
        )
        .await
    }

    async fn close_menu(
        mut host: Access<'_, PluginHostState, Self>,
        player: Resource<WitPlayer>,
    ) -> wasmtime::Result<bool> {
        let (player, plugin) = {
            let state = host.get();
            let player = Arc::clone(state.get(&player)?);
            let plugin = state
                .plugin
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .ok_or_else(|| wasmtime::Error::msg("Plugin instance not available"))?;
            (player, plugin)
        };

        plugin
            .store
            .pump_blocking(&mut host, move || player.close_plugin_menu())
            .await
    }
}
