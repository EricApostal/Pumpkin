use std::sync::Arc;

use crate::pumpkin::plugin::item_stack::ItemStack as WitItemStack;
use crate::pumpkin::plugin::menus::{Host, HostWithStore, MenuTypeEntry, OpenMenuInfo};
use crate::pumpkin::plugin::player::Player as WitPlayer;
use crate::pumpkin::plugin::text::TextComponent as WitTextComponent;
use pumpkin_core::plugin::permissions;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::menu_registry::{
    plugin_menu_type_by_key, plugin_menu_types, register_plugin_menu_type, vanilla_menu_type_count,
};
use pumpkin_wasm_host_common::state::PluginHostState;
use tracing::info;
use wasmtime::component::{Access, HasSelf, Resource};

impl Host for PluginHostState {
    fn register_menu_type(&mut self, key: String) -> wasmtime::Result<Result<u32, String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::REGISTRY_MENUS)
        {
            return Ok(Err(format!(
                "registering menu types needs the '{}' permission",
                permissions::REGISTRY_MENUS
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
}

async fn open_menu_with_store(
    mut host: Access<'_, PluginHostState, HasSelf<PluginHostState>>,
    player: Resource<WitPlayer>,
    menu_type_key: String,
    title: Resource<WitTextComponent>,
    extra_data: Option<Vec<u8>>,
    slot_count: u32,
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
            player.open_plugin_menu(
                menu_type,
                &title,
                extra_data.as_deref(),
                slot_count as usize,
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
        open_menu_with_store(host, player, menu_type_key, title, extra_data, 0).await
    }

    async fn open_menu_with_slots(
        host: Access<'_, PluginHostState, Self>,
        player: Resource<WitPlayer>,
        menu_type_key: String,
        title: Resource<WitTextComponent>,
        extra_data: Option<Vec<u8>>,
        slot_count: u32,
    ) -> wasmtime::Result<Result<u32, String>> {
        open_menu_with_store(host, player, menu_type_key, title, extra_data, slot_count).await
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
