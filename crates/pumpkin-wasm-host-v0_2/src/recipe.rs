use super::AccessorExt;
use crate::pumpkin::plugin::recipe::HostRecipeManagerWithStore;

use crate::pumpkin::plugin::recipe::{
    CookingInfo as WitCookingInfo, CookingRecipe as WitCookingRecipe,
    CookingResult as WitCookingResult, CookingType as WitCookingType,
    CraftingResult as WitCraftingResult, CustomRecipe as WitCustomRecipe, Host as RecipeHost,
    HostRecipeManager, Ingredient as WitIngredient, RecipeCategory as WitRecipeCategory,
    RecipeFluid as WitRecipeFluid, RecipeInfo as WitRecipeInfo, RecipeManager as WitRecipeManager,
    RecipeOutput as WitRecipeOutput, ShapeInfo as WitShapeInfo, ShapedRecipe as WitShapedRecipe,
    ShapelessRecipe as WitShapelessRecipe, SizedIngredient as WitSizedIngredient,
};
use pumpkin_core::plugin::permissions;
use pumpkin_core::server::plugin_recipe::{
    PluginRecipe, RecipeFluid, RecipeOutput, SizedIngredient,
};
use pumpkin_core::server::recipe_query::{CookingQueryResult, CraftingQueryResult, RecipeInfo};
use pumpkin_data::item::Item;
use pumpkin_data::recipes::{CookingRecipeKind, RecipeCategoryTypes};
use pumpkin_protocol::codec::recipe::{
    DynamicRecipe, OwnedCookingRecipe, OwnedCookingRecipeType, OwnedCraftingRecipe,
    OwnedRecipeIngredient, OwnedRecipeResult,
};
use pumpkin_wasm_host_common::state::PluginHostState;
use std::sync::Arc;
use tracing::info;
use wasmtime::component::{Accessor, HasSelf, Resource};

impl RecipeHost for PluginHostState {}

/// `registry.recipes` guard of the functions that add custom recipe types and recipes.
fn check_recipe_registry_permission(state: &PluginHostState) -> Result<(), String> {
    if state
        .permissions
        .iter()
        .any(|p| p == permissions::REGISTRY_RECIPES)
    {
        Ok(())
    } else {
        Err(format!(
            "registering recipe types and recipes needs the '{}' permission",
            permissions::REGISTRY_RECIPES
        ))
    }
}

impl HostRecipeManager for PluginHostState {
    fn drop(&mut self, rep: Resource<WitRecipeManager>) -> wasmtime::Result<()> {
        self.drop(rep)
    }

    fn register_recipe_type(
        &mut self,
        res: Resource<WitRecipeManager>,
        key: String,
    ) -> wasmtime::Result<Result<(), String>> {
        if let Err(message) = check_recipe_registry_permission(self) {
            return Ok(Err(message));
        }
        let manager = Arc::clone(self.get(&res)?);
        Ok(match manager.plugin_recipes().register_type(&key) {
            Ok(()) => {
                info!(
                    "Plugin {} registered recipe type {key}",
                    self.name.as_deref().unwrap_or("<unknown>")
                );
                Ok(())
            }
            Err(error) => Err(format!("recipe type '{key}': {error}")),
        })
    }

    fn get_registered_recipe_types(
        &mut self,
        res: Resource<WitRecipeManager>,
    ) -> wasmtime::Result<Vec<String>> {
        let manager = Arc::clone(self.get(&res)?);
        Ok(manager
            .plugin_recipes()
            .type_keys()
            .into_iter()
            .map(ToString::to_string)
            .collect())
    }

    fn register_custom_recipe(
        &mut self,
        res: Resource<WitRecipeManager>,
        recipe_type: String,
        recipe: WitCustomRecipe,
    ) -> wasmtime::Result<Result<(), String>> {
        if let Err(message) = check_recipe_registry_permission(self) {
            return Ok(Err(message));
        }
        let manager = Arc::clone(self.get(&res)?);
        let id = recipe.id.clone();
        let recipe = PluginRecipe {
            id: recipe.id,
            type_key: recipe_type,
            ingredients: recipe
                .ingredients
                .into_iter()
                .map(from_wit_sized_ingredient)
                .collect(),
            results: recipe.results.into_iter().map(from_wit_output).collect(),
            fluid_ingredients: recipe
                .fluid_ingredients
                .into_iter()
                .map(from_wit_fluid)
                .collect(),
            fluid_results: recipe
                .fluid_results
                .into_iter()
                .map(from_wit_fluid)
                .collect(),
            extra: recipe.extra,
        };
        Ok(manager
            .plugin_recipes()
            .add_recipe(recipe)
            .map_err(|error| format!("recipe '{id}': {error}")))
    }

    fn register_custom_recipe_with_network_data(
        &mut self,
        res: Resource<WitRecipeManager>,
        recipe_type: String,
        recipe: WitCustomRecipe,
        network_data: Option<Vec<u8>>,
    ) -> wasmtime::Result<Result<(), String>> {
        let manager = Arc::clone(self.get(&res)?);
        let id = recipe.id.clone();
        let registered = self.register_custom_recipe(res, recipe_type, recipe)?;
        if registered.is_err() {
            return Ok(registered);
        }
        if let Some(data) = network_data {
            return Ok(manager
                .plugin_recipes()
                .set_network_data(&id, data)
                .map_err(|error| format!("recipe '{id}': {error}")));
        }
        Ok(Ok(()))
    }

    fn find_recipes(
        &mut self,
        res: Resource<WitRecipeManager>,
        recipe_type: String,
    ) -> wasmtime::Result<Vec<WitRecipeInfo>> {
        let manager = Arc::clone(self.get(&res)?);
        Ok(manager
            .find_recipes(&recipe_type)
            .into_iter()
            .map(to_wit_info)
            .collect())
    }

    fn get_recipe_by_id(
        &mut self,
        res: Resource<WitRecipeManager>,
        id: String,
    ) -> wasmtime::Result<Option<WitRecipeInfo>> {
        let manager = Arc::clone(self.get(&res)?);
        Ok(manager.find_recipe_by_id(&id).map(to_wit_info))
    }

    fn find_crafting_recipe(
        &mut self,
        res: Resource<WitRecipeManager>,
        width: u32,
        height: u32,
        grid: Vec<Option<String>>,
    ) -> wasmtime::Result<Option<WitCraftingResult>> {
        let manager = Arc::clone(self.get(&res)?);
        let (Ok(width), Ok(height)) = (u8::try_from(width), u8::try_from(height)) else {
            return Ok(None);
        };
        if !(1..=9).contains(&width)
            || !(1..=9).contains(&height)
            || grid.len() != usize::from(width) * usize::from(height)
        {
            return Ok(None);
        }
        let mut items = Vec::with_capacity(grid.len());
        for cell in &grid {
            match cell {
                None => items.push(None),
                Some(key) => match Item::from_registry_key(key) {
                    Some(item) => items.push(Some(item)),
                    None => return Ok(None),
                },
            }
        }
        Ok(manager
            .find_crafting_recipe(width, height, &items)
            .map(to_wit_crafting_result))
    }

    fn find_smelting_recipe(
        &mut self,
        res: Resource<WitRecipeManager>,
        item: String,
        station_type: WitCookingType,
    ) -> wasmtime::Result<Option<WitCookingResult>> {
        let manager = Arc::clone(self.get(&res)?);
        let Some(item) = Item::from_registry_key(&item) else {
            return Ok(None);
        };
        let kind = match station_type {
            WitCookingType::Smelting => CookingRecipeKind::Smelting,
            WitCookingType::Blasting => CookingRecipeKind::Blasting,
            WitCookingType::Smoking => CookingRecipeKind::Smoking,
            WitCookingType::Campfire => CookingRecipeKind::CampfireCooking,
        };
        Ok(manager
            .find_cooking_recipe(item, kind)
            .map(to_wit_cooking_result))
    }
}

impl HostRecipeManagerWithStore<PluginHostState> for HasSelf<PluginHostState> {
    async fn register_shaped(
        accessor: &Accessor<PluginHostState, Self>,
        _res: Resource<WitRecipeManager>,
        id: String,
        recipe: WitShapedRecipe,
    ) -> wasmtime::Result<()> {
        let result_stack = accessor.take_res(recipe.output)?;
        let result_stack = result_stack.lock().await;

        let category = recipe
            .category
            .map_or(RecipeCategoryTypes::Misc, to_data_category);

        let owned_recipe = OwnedCraftingRecipe::Shaped {
            recipe_id: Some(id),
            category,
            group: recipe.group,
            show_notification: recipe.show_notification.unwrap_or(true),
            key: recipe
                .key
                .into_iter()
                .map(|(k, ing)| (k.chars().next().unwrap_or(' '), to_owned_ingredient(ing)))
                .collect(),
            pattern: recipe.pattern,
            result: OwnedRecipeResult {
                item_id: result_stack.item.registry_key.to_string(),
                count: result_stack.item_count,
            },
        };

        let server = accessor.server()?;
        server
            .recipe_manager
            .add_plugin_recipe(DynamicRecipe::Crafting(owned_recipe));
        Ok(())
    }

    async fn register_shapeless(
        accessor: &Accessor<PluginHostState, Self>,
        _res: Resource<WitRecipeManager>,
        id: String,
        recipe: WitShapelessRecipe,
    ) -> wasmtime::Result<()> {
        let result_stack = accessor.take_res(recipe.output)?;
        let result_stack = result_stack.lock().await;

        let category = recipe
            .category
            .map_or(RecipeCategoryTypes::Misc, to_data_category);

        let owned_recipe = OwnedCraftingRecipe::Shapeless {
            recipe_id: Some(id),
            category,
            group: recipe.group,
            ingredients: recipe
                .ingredients
                .into_iter()
                .map(to_owned_ingredient)
                .collect(),
            result: OwnedRecipeResult {
                item_id: result_stack.item.registry_key.to_string(),
                count: result_stack.item_count,
            },
        };

        let server = accessor.server()?;
        server
            .recipe_manager
            .add_plugin_recipe(DynamicRecipe::Crafting(owned_recipe));
        Ok(())
    }

    async fn register_cooking(
        accessor: &Accessor<PluginHostState, Self>,
        _res: Resource<WitRecipeManager>,
        id: String,
        station_type: WitCookingType,
        recipe: WitCookingRecipe,
    ) -> wasmtime::Result<()> {
        let result_stack = accessor.take_res(recipe.output)?;
        let result_stack = result_stack.lock().await;

        let category = recipe
            .category
            .map_or(RecipeCategoryTypes::Misc, to_data_category);

        let owned_cooking = OwnedCookingRecipe {
            recipe_id: id,
            category,
            group: recipe.group,
            ingredient: to_owned_ingredient(recipe.ingredient),
            cooking_time: recipe.cooking_time as i32,
            experience: recipe.experience,
            result: OwnedRecipeResult {
                item_id: result_stack.item.registry_key.to_string(),
                count: result_stack.item_count,
            },
        };

        let dynamic_recipe = match station_type {
            WitCookingType::Smelting => {
                DynamicRecipe::Cooking(OwnedCookingRecipeType::Smelting(owned_cooking))
            }
            WitCookingType::Blasting => {
                DynamicRecipe::Cooking(OwnedCookingRecipeType::Blasting(owned_cooking))
            }
            WitCookingType::Smoking => {
                DynamicRecipe::Cooking(OwnedCookingRecipeType::Smoking(owned_cooking))
            }
            WitCookingType::Campfire => {
                DynamicRecipe::Cooking(OwnedCookingRecipeType::CampfireCooking(owned_cooking))
            }
        };

        let server = accessor.server()?;
        server.recipe_manager.add_plugin_recipe(dynamic_recipe);
        Ok(())
    }
}

const fn to_data_category(cat: WitRecipeCategory) -> RecipeCategoryTypes {
    match cat {
        WitRecipeCategory::Building => RecipeCategoryTypes::Building,
        WitRecipeCategory::Redstone => RecipeCategoryTypes::Restone,
        WitRecipeCategory::Equipment => RecipeCategoryTypes::Equipment,
        WitRecipeCategory::Misc => RecipeCategoryTypes::Misc,
        WitRecipeCategory::Food => RecipeCategoryTypes::Food,
        WitRecipeCategory::Blocks => RecipeCategoryTypes::Blocks,
    }
}

fn to_owned_ingredient(ing: WitIngredient) -> OwnedRecipeIngredient {
    match ing {
        WitIngredient::Item(id) => OwnedRecipeIngredient::Simple(id),
        WitIngredient::Tag(tag) => OwnedRecipeIngredient::Tagged(tag),
        WitIngredient::OneOf(items) => OwnedRecipeIngredient::OneOf(items),
    }
}

fn to_wit_ingredient(ing: OwnedRecipeIngredient) -> WitIngredient {
    match ing {
        OwnedRecipeIngredient::Simple(id) => WitIngredient::Item(id),
        OwnedRecipeIngredient::Tagged(tag) => WitIngredient::Tag(tag),
        OwnedRecipeIngredient::OneOf(items) => WitIngredient::OneOf(items),
    }
}

fn from_wit_sized_ingredient(entry: WitSizedIngredient) -> SizedIngredient {
    SizedIngredient {
        ingredient: to_owned_ingredient(entry.ingredient),
        count: entry.count,
    }
}

fn to_wit_sized_ingredient(entry: SizedIngredient) -> WitSizedIngredient {
    WitSizedIngredient {
        ingredient: to_wit_ingredient(entry.ingredient),
        count: entry.count,
    }
}

fn from_wit_output(output: WitRecipeOutput) -> RecipeOutput {
    RecipeOutput {
        item_id: output.item,
        count: output.count,
        chance: output.chance,
    }
}

fn to_wit_output(output: RecipeOutput) -> WitRecipeOutput {
    WitRecipeOutput {
        item: output.item_id,
        count: output.count,
        chance: output.chance,
    }
}

fn from_wit_fluid(fluid: WitRecipeFluid) -> RecipeFluid {
    RecipeFluid {
        fluid: fluid.fluid,
        amount: fluid.amount,
    }
}

fn to_wit_fluid(fluid: RecipeFluid) -> WitRecipeFluid {
    WitRecipeFluid {
        fluid: fluid.fluid,
        amount: fluid.amount,
    }
}

fn to_wit_info(info: RecipeInfo) -> WitRecipeInfo {
    WitRecipeInfo {
        id: info.id,
        recipe_type: info.recipe_type,
        group: info.group,
        ingredients: info
            .ingredients
            .into_iter()
            .map(to_wit_sized_ingredient)
            .collect(),
        results: info.results.into_iter().map(to_wit_output).collect(),
        fluid_ingredients: info
            .fluid_ingredients
            .into_iter()
            .map(to_wit_fluid)
            .collect(),
        fluid_results: info.fluid_results.into_iter().map(to_wit_fluid).collect(),
        shape: info.shape.map(|shape| WitShapeInfo {
            pattern: shape.pattern,
            key: shape
                .key
                .into_iter()
                .map(|(k, ingredient)| (k.to_string(), to_wit_ingredient(ingredient)))
                .collect(),
        }),
        cooking: info.cooking.map(|cooking| WitCookingInfo {
            experience: cooking.experience,
            cooking_time: cooking.cooking_time,
        }),
        extra: info.extra,
    }
}

fn to_wit_crafting_result(result: CraftingQueryResult) -> WitCraftingResult {
    WitCraftingResult {
        id: result.id,
        recipe_type: result.recipe_type,
        output: to_wit_output(result.output),
    }
}

fn to_wit_cooking_result(result: CookingQueryResult) -> WitCookingResult {
    WitCookingResult {
        id: result.id,
        output: to_wit_output(result.output),
        experience: result.experience,
        cooking_time: result.cooking_time,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumpkin_core::server::RecipeManager;
    use std::sync::Arc;

    #[test]
    fn guest_drop_releases_recipe_manager_resource() {
        let mut state = PluginHostState::new();
        let manager = Arc::new(RecipeManager::new());
        let weak_manager = Arc::downgrade(&manager);
        let resource = state
            .add::<WitRecipeManager>(manager)
            .expect("insert recipe manager resource");
        let rep = resource.rep();

        HostRecipeManager::drop(&mut state, resource).expect("drop recipe manager resource");

        assert!(weak_manager.upgrade().is_none());
        assert!(
            state
                .get(&Resource::<WitRecipeManager>::new_borrow(rep))
                .is_err()
        );
    }
}
