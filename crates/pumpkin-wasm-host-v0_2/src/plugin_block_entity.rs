use std::sync::Arc;

use crate::common::{WitNbtTree, from_wit_nbt_tree, to_wit_nbt_tree};
use crate::pumpkin::plugin::common::BlockPos as WitBlockPos;
use crate::pumpkin::plugin::plugin_block_entity::{
    BlockEntityTypeEntry, Host, HostWithStore, PluginBlockEntityData,
};
use crate::pumpkin::plugin::world::World as WitWorld;
use pumpkin_core::plugin::permissions;
use pumpkin_data::block_entity_registry::{
    plugin_block_entity_type_by_key, plugin_block_entity_types, register_plugin_block_entity_type,
    set_plugin_block_entity_tick_interval, vanilla_block_entity_type_count,
};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::position::BlockPos;
use pumpkin_wasm_host_common::state::PluginHostState;
use tracing::info;
use wasmtime::component::{Access, HasSelf, Resource};

const fn from_wit_position(position: &WitBlockPos) -> BlockPos {
    BlockPos::new(position.x, position.y, position.z)
}

/// The root of a tree has to be a compound tag.
fn compound_from_tree(tree: &WitNbtTree, what: &str) -> Result<NbtCompound, String> {
    match from_wit_nbt_tree(tree)? {
        NbtTag::Compound(compound) => Ok(compound),
        _ => Err(format!("the {what} has to be a compound tag")),
    }
}

impl Host for PluginHostState {
    fn register_block_entity_type(
        &mut self,
        key: String,
        valid_block_keys: Vec<String>,
    ) -> wasmtime::Result<Result<u32, String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::REGISTRY_BLOCK_ENTITIES)
        {
            return Ok(Err(format!(
                "registering block entity types needs the '{}' permission",
                permissions::REGISTRY_BLOCK_ENTITIES
            )));
        }
        Ok(
            match register_plugin_block_entity_type(&key, valid_block_keys) {
                Ok(entity_type) => {
                    info!(
                        "Plugin {} registered block entity type {} with id {}",
                        self.name.as_deref().unwrap_or("<unknown>"),
                        entity_type.key,
                        entity_type.id
                    );
                    Ok(entity_type.id)
                }
                Err(error) => Err(format!("block entity type '{key}': {error}")),
            },
        )
    }

    fn get_block_entity_type_id(&mut self, key: String) -> wasmtime::Result<Option<u32>> {
        Ok(plugin_block_entity_type_by_key(&key).map(|entity_type| entity_type.id))
    }

    fn get_vanilla_block_entity_type_count(&mut self) -> wasmtime::Result<u32> {
        Ok(vanilla_block_entity_type_count())
    }

    fn get_registered_block_entity_types(&mut self) -> wasmtime::Result<Vec<BlockEntityTypeEntry>> {
        Ok(plugin_block_entity_types()
            .into_iter()
            .map(|entity_type| BlockEntityTypeEntry {
                key: entity_type.key.to_string(),
                id: entity_type.id,
                valid_blocks: entity_type
                    .valid_blocks
                    .iter()
                    .map(|block| (*block).to_string())
                    .collect(),
            })
            .collect())
    }

    fn set_block_entity_ticking(
        &mut self,
        type_key: String,
        interval: u32,
    ) -> wasmtime::Result<Result<(), String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::REGISTRY_BLOCK_ENTITIES)
        {
            return Ok(Err(format!(
                "ticking block entity types needs the '{}' permission",
                permissions::REGISTRY_BLOCK_ENTITIES
            )));
        }
        let Some(entity_type) = plugin_block_entity_type_by_key(&type_key) else {
            return Ok(Err(format!(
                "block entity type '{type_key}' is not registered"
            )));
        };
        set_plugin_block_entity_tick_interval(entity_type, interval);
        Ok(Ok(()))
    }

    fn get_block_entity_data(
        &mut self,
        target_world: Resource<WitWorld>,
        pos: WitBlockPos,
    ) -> wasmtime::Result<Option<PluginBlockEntityData>> {
        let world = Arc::clone(self.get(&target_world)?);
        Ok(
            world.with_plugin_block_entity(&from_wit_position(&pos), |entity| {
                PluginBlockEntityData {
                    type_key: entity.entity_type().key.to_string(),
                    save_data: to_wit_nbt_tree(NbtTag::Compound(entity.data())),
                    client_data: entity
                        .client_data()
                        .map(|data| to_wit_nbt_tree(NbtTag::Compound(data))),
                }
            }),
        )
    }

    fn set_block_entity_data(
        &mut self,
        target_world: Resource<WitWorld>,
        pos: WitBlockPos,
        type_key: String,
        save_data: WitNbtTree,
        client_data: Option<WitNbtTree>,
    ) -> wasmtime::Result<Result<(), String>> {
        let world = Arc::clone(self.get(&target_world)?);
        let Some(entity_type) = plugin_block_entity_type_by_key(&type_key) else {
            return Ok(Err(format!(
                "block entity type '{type_key}' is not registered"
            )));
        };
        let save_data = match compound_from_tree(&save_data, "save data") {
            Ok(data) => data,
            Err(error) => return Ok(Err(error)),
        };
        let client_data = match client_data
            .as_ref()
            .map(|data| compound_from_tree(data, "client data"))
            .transpose()
        {
            Ok(data) => data,
            Err(error) => return Ok(Err(error)),
        };
        Ok(world.set_plugin_block_entity(
            from_wit_position(&pos),
            entity_type,
            save_data,
            client_data,
        ))
    }

    fn sync_block_entity(
        &mut self,
        target_world: Resource<WitWorld>,
        pos: WitBlockPos,
    ) -> wasmtime::Result<Result<(), String>> {
        let world = Arc::clone(self.get(&target_world)?);
        Ok(world.sync_plugin_block_entity(&from_wit_position(&pos)))
    }
}

impl HostWithStore<PluginHostState> for HasSelf<PluginHostState> {
    async fn remove_block_entity(
        mut host: Access<'_, PluginHostState, Self>,
        target_world: Resource<WitWorld>,
        pos: WitBlockPos,
    ) -> wasmtime::Result<bool> {
        let (world, plugin) = {
            let state = host.get();
            let world = Arc::clone(state.get(&target_world)?);
            let plugin = state
                .plugin
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .ok_or_else(|| wasmtime::Error::msg("Plugin instance not available"))?;
            (world, plugin)
        };
        let position = from_wit_position(&pos);

        plugin
            .store
            .pump_blocking(&mut host, move || {
                world.remove_plugin_block_entity(&position)
            })
            .await
    }
}
