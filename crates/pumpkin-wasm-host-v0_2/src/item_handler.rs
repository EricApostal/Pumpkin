use std::sync::Arc;

use tokio::sync::Mutex;
use wasmtime::component::{Accessor, HasSelf, Resource};

use crate::AccessorExt;
use crate::mob::from_wit_block_direction;
use crate::pumpkin::plugin::common::BlockPos as WitBlockPos;
use crate::pumpkin::plugin::item_handler::{
    BlockDirection as WitBlockDirection, Host, HostItemHandler, HostItemHandlerWithStore,
    ItemHandler as WitItemHandler,
};
use crate::pumpkin::plugin::item_stack::ItemStack as WitHostItemStack;
use crate::pumpkin::plugin::world::World as WitWorld;
use pumpkin_core::block::item_handler::ItemHandler;
use pumpkin_core::plugin::permissions;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_util::math::position::BlockPos;
use pumpkin_wasm_host_common::state::PluginHostState;

const fn from_wit_position(position: &WitBlockPos) -> BlockPos {
    BlockPos::new(position.x, position.y, position.z)
}

fn non_empty(stack: ItemStack) -> Option<ItemStack> {
    (!stack.is_empty()).then_some(stack)
}

fn new_item_stack_handle(
    state: &mut PluginHostState,
    stack: ItemStack,
) -> wasmtime::Result<Resource<WitHostItemStack>> {
    state.add(Arc::new(Mutex::new(stack)))
}

fn item_handler(
    state: &PluginHostState,
    res: &Resource<WitItemHandler>,
) -> wasmtime::Result<Arc<ItemHandler>> {
    state.get(res).cloned()
}

fn permission_error(state: &PluginHostState) -> Option<String> {
    (!state
        .permissions
        .iter()
        .any(|p| p == permissions::CONTAINERS))
    .then(|| {
        format!(
            "item handlers and plugin inventories need the '{}' permission",
            permissions::CONTAINERS
        )
    })
}

impl Host for PluginHostState {
    fn get_item_handler(
        &mut self,
        target_world: Resource<WitWorld>,
        pos: WitBlockPos,
        side: Option<WitBlockDirection>,
    ) -> wasmtime::Result<Option<Resource<WitItemHandler>>> {
        // No error channel, so a plugin without the permission sees no container.
        if permission_error(self).is_some() {
            return Ok(None);
        }
        let world = Arc::clone(self.get(&target_world)?);
        let side = side.map(from_wit_block_direction);
        ItemHandler::at(&world, &from_wit_position(&pos), side)
            .map(|handler| self.add(Arc::new(handler)))
            .transpose()
    }

    fn register_plugin_inventory(
        &mut self,
        target_world: Resource<WitWorld>,
        pos: WitBlockPos,
        slot_count: u32,
        slot_limit: u8,
    ) -> wasmtime::Result<Result<(), String>> {
        if let Some(error) = permission_error(self) {
            return Ok(Err(error));
        }
        let world = Arc::clone(self.get(&target_world)?);
        Ok(world
            .with_plugin_block_entity(&from_wit_position(&pos), |entity| {
                entity
                    .register_inventory(slot_count as usize, slot_limit)
                    .map(|_| ())
            })
            .unwrap_or_else(|| Err("there is no plugin block entity at the position".to_string())))
    }

    fn unregister_plugin_inventory(
        &mut self,
        target_world: Resource<WitWorld>,
        pos: WitBlockPos,
    ) -> wasmtime::Result<Result<Vec<Resource<WitHostItemStack>>, String>> {
        if let Some(error) = permission_error(self) {
            return Ok(Err(error));
        }
        let world = Arc::clone(self.get(&target_world)?);
        let Some(stacks) = world.with_plugin_block_entity(&from_wit_position(&pos), |entity| {
            entity.unregister_inventory()
        }) else {
            return Ok(Err(
                "there is no plugin block entity at the position".to_string()
            ));
        };
        let mut handles = Vec::with_capacity(stacks.len());
        for stack in stacks {
            handles.push(new_item_stack_handle(self, stack)?);
        }
        Ok(Ok(handles))
    }
}

impl HostItemHandler for PluginHostState {
    fn slot_count(&mut self, res: Resource<WitItemHandler>) -> wasmtime::Result<u32> {
        Ok(item_handler(self, &res)?.slot_count() as u32)
    }

    fn get_stack(
        &mut self,
        res: Resource<WitItemHandler>,
        slot: u32,
    ) -> wasmtime::Result<Option<Resource<WitHostItemStack>>> {
        let stack = item_handler(self, &res)?.get_stack(slot as usize);
        non_empty(stack)
            .map(|stack| new_item_stack_handle(self, stack))
            .transpose()
    }

    fn get_slot_limit(&mut self, res: Resource<WitItemHandler>, slot: u32) -> wasmtime::Result<u8> {
        Ok(item_handler(self, &res)?.get_slot_limit(slot as usize))
    }

    fn extract(
        &mut self,
        res: Resource<WitItemHandler>,
        slot: u32,
        amount: u8,
        simulate: bool,
    ) -> wasmtime::Result<Option<Resource<WitHostItemStack>>> {
        let stack = item_handler(self, &res)?.extract(slot as usize, amount, simulate);
        non_empty(stack)
            .map(|stack| new_item_stack_handle(self, stack))
            .transpose()
    }

    fn drop(&mut self, rep: Resource<WitItemHandler>) -> wasmtime::Result<()> {
        self.drop(rep)
    }
}

impl HostItemHandlerWithStore<PluginHostState> for HasSelf<PluginHostState> {
    async fn is_item_valid(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<WitItemHandler>,
        slot: u32,
        stack: Resource<WitHostItemStack>,
    ) -> wasmtime::Result<bool> {
        let handler = accessor.get_res(&res)?;
        let stack = accessor.get_res(&stack)?.lock().await.clone();
        Ok(handler.is_item_valid(slot as usize, &stack))
    }

    async fn insert(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<WitItemHandler>,
        slot: Option<u32>,
        stack: Resource<WitHostItemStack>,
        simulate: bool,
    ) -> wasmtime::Result<Option<Resource<WitHostItemStack>>> {
        let handler = accessor.get_res(&res)?;
        let stack = accessor.get_res(&stack)?.lock().await.clone();
        let remainder = handler.insert(slot.map(|s| s as usize), &stack, simulate);
        non_empty(remainder)
            .map(|stack| accessor.add_res(Arc::new(Mutex::new(stack))))
            .transpose()
    }
}
