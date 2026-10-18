use std::sync::RwLock;

use pumpkin_inventory::crafting::recipe_provider::RecipeProvider;
pub use pumpkin_protocol::codec::recipe::DynamicRecipe;

use super::plugin_recipe::PluginRecipeRegistry;

#[expect(clippy::struct_field_names)]
pub struct RecipeManager {
    dynamic_recipes: RwLock<Vec<DynamicRecipe>>,
    /// Recipes plugins registered, re-applied after a datapack reload.
    registered_recipes: RwLock<Vec<DynamicRecipe>>,
    /// Recipe types and recipes of plugins, not touched by a datapack reload.
    plugin_recipes: PluginRecipeRegistry,
}

impl Default for RecipeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RecipeManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            dynamic_recipes: RwLock::new(Vec::new()),
            registered_recipes: RwLock::new(Vec::new()),
            plugin_recipes: PluginRecipeRegistry::new(),
        }
    }

    #[must_use]
    pub const fn plugin_recipes(&self) -> &PluginRecipeRegistry {
        &self.plugin_recipes
    }

    /// Runs `f` on the dynamic recipes without cloning them.
    pub fn with_dynamic_recipes<R>(&self, f: impl FnOnce(&[DynamicRecipe]) -> R) -> R {
        let recipes = self
            .dynamic_recipes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&recipes)
    }

    pub fn add_recipe(&self, recipe: DynamicRecipe) {
        let mut recipes = self
            .dynamic_recipes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        recipes.push(recipe);
    }

    /// Adds a recipe a plugin registered. It survives [`Self::set_recipes`].
    pub fn add_plugin_recipe(&self, recipe: DynamicRecipe) {
        self.registered_recipes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(recipe.clone());
        self.add_recipe(recipe);
    }

    pub fn add_recipes(&self, new_recipes: impl IntoIterator<Item = DynamicRecipe>) {
        let mut recipes = self
            .dynamic_recipes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        recipes.extend(new_recipes);
    }

    /// Replaces the dynamic recipes, then re-applies the ones plugins registered.
    pub fn set_recipes(&self, mut new_recipes: Vec<DynamicRecipe>) {
        new_recipes.extend(
            self.registered_recipes
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .cloned(),
        );
        let mut recipes = self
            .dynamic_recipes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *recipes = new_recipes;
    }

    pub fn clear(&self) {
        let mut recipes = self
            .dynamic_recipes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        recipes.clear();
    }

    pub fn get_dynamic_recipes_internal(&self) -> Vec<DynamicRecipe> {
        self.dynamic_recipes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl RecipeProvider for RecipeManager {
    fn get_dynamic_recipes(&self) -> Vec<DynamicRecipe> {
        self.dynamic_recipes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
