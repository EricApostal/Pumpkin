use crate::pumpkin::plugin::fluid_registry::{
    FluidDefinition as WitFluidDefinition, FluidEntry as WitFluidEntry,
    FluidStack as WitFluidStack, Host,
};
use pumpkin_core::plugin::permissions;
use pumpkin_data::fluid::Fluid;
use pumpkin_data::fluid_registry::{FluidPlacement, FluidRegistration};
use pumpkin_data::item::Item;
use pumpkin_wasm_host_common::state::PluginHostState;
use tracing::info;

/// Millibuckets in a full bucket.
const BUCKET_AMOUNT: u32 = 1000;

/// Vanilla keys may omit the `minecraft:` prefix.
fn find_fluid(key: &str) -> Option<&'static Fluid> {
    Fluid::from_registry_key(key.strip_prefix("minecraft:").unwrap_or(key))
}

impl Host for PluginHostState {
    fn register_fluid(
        &mut self,
        definition: WitFluidDefinition,
    ) -> wasmtime::Result<Result<u32, String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::REGISTRY_FLUIDS)
        {
            return Ok(Err(self.missing_permission(
                "registering fluids",
                permissions::REGISTRY_FLUIDS,
            )));
        }
        if Fluid::is_dynamic_registry_frozen() {
            return Ok(Err(
                "fluids can only be registered before players connect".to_string()
            ));
        }

        let result = Fluid::register_dynamic(FluidRegistration {
            key: definition.key,
            placement: definition.placement.map(|placement| FluidPlacement {
                block: placement.block,
                flowing: placement.flowing,
            }),
            flowing_variant: definition.flowing_variant,
            bucket_item: definition.bucket_item,
            tick_rate: definition.tick_rate,
            level_decrease_per_block: definition.level_decrease_per_block,
            slope_find_distance: definition.slope_find_distance,
            can_convert_to_source: definition.can_convert_to_source,
            luminance: definition.luminance,
            density: definition.density,
            temperature: definition.temperature,
            viscosity: definition.viscosity,
            tags: definition.tags,
        });
        Ok(match result {
            Ok(fluid) => {
                info!(
                    "Plugin {} registered fluid {} with id {}",
                    self.name.as_deref().unwrap_or("<unknown>"),
                    fluid.name,
                    fluid.id
                );
                Ok(u32::from(fluid.id))
            }
            Err(error) => Err(error.to_string()),
        })
    }

    fn register_fluid_tag(
        &mut self,
        tag: String,
        entries: Vec<String>,
    ) -> wasmtime::Result<Result<(), String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::REGISTRY_FLUIDS)
        {
            return Ok(Err(self.missing_permission(
                "registering fluid tags",
                permissions::REGISTRY_FLUIDS,
            )));
        }
        if Fluid::is_dynamic_registry_frozen() {
            return Ok(Err(
                "fluid tags can only be registered before players connect".to_string(),
            ));
        }
        Fluid::register_dynamic_tag(&tag, &entries);
        Ok(Ok(()))
    }

    fn get_fluid_id(&mut self, key: String) -> wasmtime::Result<Option<u32>> {
        Ok(find_fluid(&key).map(|fluid| u32::from(fluid.id)))
    }

    fn get_fluid_key(&mut self, id: u32) -> wasmtime::Result<Option<String>> {
        Ok(u16::try_from(id)
            .ok()
            .and_then(Fluid::from_id)
            .map(|fluid| fluid.resource_location().into_owned()))
    }

    fn get_vanilla_fluid_count(&mut self) -> wasmtime::Result<u32> {
        Ok(u32::from(Fluid::VANILLA_COUNT))
    }

    fn get_registered_fluids(&mut self) -> wasmtime::Result<Vec<WitFluidEntry>> {
        Ok(Fluid::dynamic_fluids()
            .into_iter()
            .map(|fluid| WitFluidEntry {
                key: fluid.resource_location().into_owned(),
                id: u32::from(fluid.id),
            })
            .collect())
    }

    fn fluid_from_bucket(&mut self, item_key: String) -> wasmtime::Result<Option<WitFluidStack>> {
        Ok(Item::from_registry_key(&item_key)
            .and_then(Fluid::from_bucket_item)
            .map(|fluid| WitFluidStack {
                fluid: fluid.resource_location().into_owned(),
                amount: BUCKET_AMOUNT,
                data: None,
            }))
    }

    fn bucket_for_fluid(&mut self, fluid_key: String) -> wasmtime::Result<Option<String>> {
        Ok(find_fluid(&fluid_key)
            .and_then(Fluid::bucket_item)
            .map(|item| item.resource_location().into_owned()))
    }
}
