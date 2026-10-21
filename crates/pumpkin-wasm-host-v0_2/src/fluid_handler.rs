use std::sync::Arc;

use tokio::sync::Mutex;
use wasmtime::component::{Accessor, HasSelf, Resource};

use crate::AccessorExt;
use crate::mob::from_wit_block_direction;
use crate::pumpkin::plugin::common::BlockPos as WitBlockPos;
use crate::pumpkin::plugin::fluid_handler::{
    BlockDirection as WitBlockDirection, FluidHandler as WitFluidHandler,
    FluidStack as WitFluidStack, Host, HostFluidHandler, HostItemFluidHandler,
    HostItemFluidHandlerWithStore, ItemFluidHandler as WitItemFluidHandler,
};
use crate::pumpkin::plugin::item_stack::ItemStack as WitHostItemStack;
use crate::pumpkin::plugin::world::World as WitWorld;
use pumpkin_core::block::entities::plugin_tank::FluidContent;
use pumpkin_core::block::fluid_handler::FluidHandler;
use pumpkin_core::block::item_fluid_handler as item_fluid;
use pumpkin_core::plugin::permissions;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_util::math::position::BlockPos;
use pumpkin_wasm_host_common::state::PluginHostState;

/// The stack an item fluid handler works on, shared with the item stack resource.
#[derive(Clone)]
pub struct ItemFluidTarget(Arc<Mutex<ItemStack>>);

const fn from_wit_position(position: &WitBlockPos) -> BlockPos {
    BlockPos::new(position.x, position.y, position.z)
}

fn to_content(stack: WitFluidStack) -> FluidContent {
    FluidContent {
        fluid: stack.fluid,
        amount: stack.amount,
        data: stack.data,
    }
}

fn to_wit(content: FluidContent) -> WitFluidStack {
    WitFluidStack {
        fluid: content.fluid,
        amount: content.amount,
        data: content.data,
    }
}

fn permission_error(state: &PluginHostState) -> Option<String> {
    (!state
        .permissions
        .iter()
        .any(|p| p == permissions::CONTAINERS))
    .then(|| {
        state.missing_permission(
            "fluid handlers and plugin tanks",
            permissions::CONTAINERS,
        )
    })
}

fn fluid_handler(
    state: &PluginHostState,
    res: &Resource<WitFluidHandler>,
) -> wasmtime::Result<Arc<FluidHandler>> {
    state.get(res).cloned()
}

impl Host for PluginHostState {
    fn get_fluid_handler(
        &mut self,
        target_world: Resource<WitWorld>,
        pos: WitBlockPos,
        side: Option<WitBlockDirection>,
    ) -> wasmtime::Result<Option<Resource<WitFluidHandler>>> {
        // No error channel, so a plugin without the permission sees no tanks.
        if permission_error(self).is_some() {
            return Ok(None);
        }
        let world = Arc::clone(self.get(&target_world)?);
        let side = side.map(from_wit_block_direction);
        FluidHandler::at(&world, &from_wit_position(&pos), side)
            .map(|handler| self.add(Arc::new(handler)))
            .transpose()
    }

    fn get_item_fluid_handler(
        &mut self,
        stack: Resource<WitHostItemStack>,
    ) -> wasmtime::Result<Option<Resource<WitItemFluidHandler>>> {
        if permission_error(self).is_some() {
            return Ok(None);
        }
        let stack = Arc::clone(self.get(&stack)?);
        let is_container = stack
            .try_lock()
            .is_ok_and(|stack| item_fluid::is_fluid_container(&stack));
        if !is_container {
            return Ok(None);
        }
        Ok(Some(self.add(ItemFluidTarget(stack))?))
    }

    fn register_plugin_tank(
        &mut self,
        target_world: Resource<WitWorld>,
        pos: WitBlockPos,
        tank_count: u32,
        capacity_per_tank: u32,
    ) -> wasmtime::Result<Result<(), String>> {
        if let Some(error) = permission_error(self) {
            return Ok(Err(error));
        }
        let world = Arc::clone(self.get(&target_world)?);
        Ok(world
            .with_plugin_block_entity(&from_wit_position(&pos), |entity| {
                entity
                    .register_tanks(tank_count as usize, capacity_per_tank)
                    .map(|_| ())
            })
            .unwrap_or_else(|| Err("there is no plugin block entity at the position".to_string())))
    }

    fn unregister_plugin_tank(
        &mut self,
        target_world: Resource<WitWorld>,
        pos: WitBlockPos,
    ) -> wasmtime::Result<Result<Vec<WitFluidStack>, String>> {
        if let Some(error) = permission_error(self) {
            return Ok(Err(error));
        }
        let world = Arc::clone(self.get(&target_world)?);
        Ok(world
            .with_plugin_block_entity(&from_wit_position(&pos), |entity| entity.unregister_tanks())
            .map(|fluids| fluids.into_iter().map(to_wit).collect())
            .ok_or_else(|| "there is no plugin block entity at the position".to_string()))
    }

    fn register_fluid_container_item(
        &mut self,
        item_key: String,
        capacity: u32,
        allowed_fluids: Vec<String>,
    ) -> wasmtime::Result<Result<(), String>> {
        if let Some(error) = permission_error(self) {
            return Ok(Err(error));
        }
        let Some(item) = Item::from_registry_key(&item_key) else {
            return Ok(Err(format!("unknown item '{item_key}'")));
        };
        Ok(item_fluid::register_fluid_container(
            item,
            capacity,
            &allowed_fluids,
        ))
    }
}

impl HostFluidHandler for PluginHostState {
    fn tank_count(&mut self, res: Resource<WitFluidHandler>) -> wasmtime::Result<u32> {
        Ok(fluid_handler(self, &res)?.tank_count() as u32)
    }

    fn get_fluid(
        &mut self,
        res: Resource<WitFluidHandler>,
        tank: u32,
    ) -> wasmtime::Result<Option<WitFluidStack>> {
        Ok(fluid_handler(self, &res)?
            .get_fluid(tank as usize)
            .map(to_wit))
    }

    fn get_tank_capacity(
        &mut self,
        res: Resource<WitFluidHandler>,
        tank: u32,
    ) -> wasmtime::Result<u32> {
        Ok(fluid_handler(self, &res)?.get_tank_capacity(tank as usize))
    }

    fn is_fluid_valid(
        &mut self,
        res: Resource<WitFluidHandler>,
        tank: u32,
        fluid_key: String,
    ) -> wasmtime::Result<bool> {
        Ok(fluid_handler(self, &res)?.is_fluid_valid(tank as usize, &fluid_key))
    }

    fn fill(
        &mut self,
        res: Resource<WitFluidHandler>,
        stack: WitFluidStack,
        simulate: bool,
    ) -> wasmtime::Result<u32> {
        Ok(fluid_handler(self, &res)?.fill(&to_content(stack), simulate))
    }

    fn drain(
        &mut self,
        res: Resource<WitFluidHandler>,
        max_amount: u32,
        simulate: bool,
    ) -> wasmtime::Result<Option<WitFluidStack>> {
        Ok(fluid_handler(self, &res)?
            .drain(max_amount, simulate)
            .map(to_wit))
    }

    fn drain_fluid(
        &mut self,
        res: Resource<WitFluidHandler>,
        fluid_key: String,
        max_amount: u32,
        simulate: bool,
    ) -> wasmtime::Result<Option<WitFluidStack>> {
        Ok(fluid_handler(self, &res)?
            .drain_fluid(&fluid_key, max_amount, simulate)
            .map(to_wit))
    }

    fn drop(&mut self, rep: Resource<WitFluidHandler>) -> wasmtime::Result<()> {
        self.drop(rep)
    }
}

impl HostItemFluidHandler for PluginHostState {
    fn tank_count(&mut self, _res: Resource<WitItemFluidHandler>) -> wasmtime::Result<u32> {
        Ok(1)
    }

    fn drop(&mut self, rep: Resource<WitItemFluidHandler>) -> wasmtime::Result<()> {
        self.drop(rep)
    }
}

impl HostItemFluidHandlerWithStore<PluginHostState> for HasSelf<PluginHostState> {
    async fn get_fluid(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<WitItemFluidHandler>,
        tank: u32,
    ) -> wasmtime::Result<Option<WitFluidStack>> {
        let target = accessor.get_res(&res)?;
        let stack = target.0.lock().await;
        Ok(if tank == 0 {
            item_fluid::contents(&stack).map(to_wit)
        } else {
            None
        })
    }

    async fn get_tank_capacity(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<WitItemFluidHandler>,
        tank: u32,
    ) -> wasmtime::Result<u32> {
        let target = accessor.get_res(&res)?;
        let stack = target.0.lock().await;
        Ok(if tank == 0 {
            item_fluid::capacity(&stack)
        } else {
            0
        })
    }

    async fn is_fluid_valid(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<WitItemFluidHandler>,
        tank: u32,
        fluid_key: String,
    ) -> wasmtime::Result<bool> {
        let target = accessor.get_res(&res)?;
        let stack = target.0.lock().await;
        Ok(tank == 0 && item_fluid::is_fluid_valid(&stack, &fluid_key))
    }

    async fn fill(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<WitItemFluidHandler>,
        stack: WitFluidStack,
        simulate: bool,
    ) -> wasmtime::Result<u32> {
        let target = accessor.get_res(&res)?;
        let mut held = target.0.lock().await;
        Ok(item_fluid::fill(&mut held, &to_content(stack), simulate))
    }

    async fn drain(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<WitItemFluidHandler>,
        max_amount: u32,
        simulate: bool,
    ) -> wasmtime::Result<Option<WitFluidStack>> {
        let target = accessor.get_res(&res)?;
        let mut held = target.0.lock().await;
        Ok(item_fluid::drain(&mut held, None, max_amount, simulate).map(to_wit))
    }

    async fn drain_fluid(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<WitItemFluidHandler>,
        fluid_key: String,
        max_amount: u32,
        simulate: bool,
    ) -> wasmtime::Result<Option<WitFluidStack>> {
        let Some(key) = pumpkin_core::block::fluid_handler::resolve_fluid_key(&fluid_key) else {
            return Ok(None);
        };
        let target = accessor.get_res(&res)?;
        let mut held = target.0.lock().await;
        Ok(item_fluid::drain(&mut held, Some(&key), max_amount, simulate).map(to_wit))
    }
}
