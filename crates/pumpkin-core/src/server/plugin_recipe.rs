//! Recipe types and recipes that plugins define.
//!
//! A plugin registers a recipe type key (`create:pressing`) and then adds recipes of that type as
//! plain data: sized item ingredients, fluids, results with a chance and an opaque blob for
//! whatever else the machine that reads the recipe needs. The server does not craft with them, the
//! plugin queries them (see [`super::recipe_query`]) and runs the machine itself.
//!
//! Both tables are append only: types and recipes are never removed or changed. They are kept apart
//! from the datapack recipes, which a datapack reload replaces as a whole, and they are not sent to
//! clients: the recipe book packets only know the vanilla recipe displays.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use pumpkin_data::plugin_registry::{PluginRegistry, RegistryError, split_plugin_key};
use pumpkin_protocol::codec::recipe::OwnedRecipeIngredient;

/// Far more recipe types than a plugin set needs.
const MAX_RECIPE_TYPES: usize = 4096;
/// Most ingredients, results and fluids one recipe can have.
pub const MAX_RECIPE_ENTRIES: usize = 64;
/// Most bytes of the extra blob of one recipe.
pub const MAX_EXTRA_BYTES: usize = 64 * 1024;
/// Most bytes of the network encoding of one recipe.
pub const MAX_NETWORK_BYTES: usize = 32 * 1024;

/// A recipe type a plugin registered.
pub struct PluginRecipeType {
    /// Registration order.
    pub index: usize,
    /// The full resource location, `namespace:path`.
    pub key: &'static str,
}

/// An ingredient and how many items it takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SizedIngredient {
    pub ingredient: OwnedRecipeIngredient,
    pub count: u32,
}

/// An item a recipe produces, possibly only some of the time.
#[derive(Clone, Debug, PartialEq)]
pub struct RecipeOutput {
    /// Resource location of the item.
    pub item_id: String,
    pub count: u32,
    /// Chance in `0.0..=1.0` that the output is produced.
    pub chance: f32,
}

/// A fluid in a recipe, `amount` in millibuckets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeFluid {
    /// Resource location of the fluid, or `#namespace:path` for a fluid tag.
    pub fluid: String,
    pub amount: u32,
}

/// A recipe of a plugin defined type.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginRecipe {
    /// Unique `namespace:path` id of the recipe.
    pub id: String,
    /// The key of the recipe type.
    pub type_key: String,
    pub ingredients: Vec<SizedIngredient>,
    pub results: Vec<RecipeOutput>,
    pub fluid_ingredients: Vec<RecipeFluid>,
    pub fluid_results: Vec<RecipeFluid>,
    /// Whatever else the machine needs (NBT, JSON, ...), the server never looks at it.
    pub extra: Vec<u8>,
}

struct Inner {
    types: PluginRegistry<PluginRecipeType>,
    recipes: Vec<Arc<PluginRecipe>>,
    by_id: HashMap<String, usize>,
    /// The network encoding of a recipe for `NeoForge` clients, set by the plugin.
    network_data: HashMap<String, Vec<u8>>,
}

/// The recipe types and recipes of the plugins.
pub struct PluginRecipeRegistry {
    inner: RwLock<Inner>,
}

impl Default for PluginRecipeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginRecipeRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner {
                types: PluginRegistry::new(MAX_RECIPE_TYPES),
                recipes: Vec::new(),
                by_id: HashMap::new(),
                network_data: HashMap::new(),
            }),
        }
    }

    /// Registers a recipe type. Registering the same key again does nothing, so a plugin that
    /// reloads can do it again. Types can be added at any time, nothing is synchronised with clients.
    pub fn register_type(&self, key: &str) -> Result<(), RegistryError> {
        self.inner
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .types
            .register(key, |_| true, |index, key| PluginRecipeType { index, key })
            .map(|_| ())
    }

    /// The registered type keys in registration order.
    #[must_use]
    pub fn type_keys(&self) -> Vec<&'static str> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .types
            .entries()
            .iter()
            .map(|entry| entry.key)
            .collect()
    }

    #[must_use]
    pub fn has_type(&self, key: &str) -> bool {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .types
            .get_by_key(key)
            .is_some()
    }

    /// Adds a recipe of a registered type. Adding a recipe that is identical to the one with the
    /// same id does nothing, a different one with the same id is [`RegistryError::Duplicate`].
    pub fn add_recipe(&self, recipe: PluginRecipe) -> Result<(), RegistryError> {
        split_plugin_key(&recipe.id)?;
        validate(&recipe)?;
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if inner.types.get_by_key(&recipe.type_key).is_none() {
            return Err(RegistryError::InvalidDefinition(format!(
                "recipe type '{}' is not registered",
                recipe.type_key
            )));
        }
        if let Some(&index) = inner.by_id.get(&recipe.id) {
            return if *inner.recipes[index] == recipe {
                Ok(())
            } else {
                Err(RegistryError::Duplicate(recipe.id))
            };
        }
        let index = inner.recipes.len();
        inner.by_id.insert(recipe.id.clone(), index);
        inner.recipes.push(Arc::new(recipe));
        Ok(())
    }

    /// The recipes of one type in the order they were added.
    #[must_use]
    pub fn recipes_of_type(&self, type_key: &str) -> Vec<Arc<PluginRecipe>> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .recipes
            .iter()
            .filter(|recipe| recipe.type_key == type_key)
            .cloned()
            .collect()
    }

    /// Sets the network encoding (the recipe serializer's stream codec body) that is sent to
    /// `NeoForge` clients for the recipe. Fails for an unknown recipe or an oversized blob.
    pub fn set_network_data(&self, id: &str, data: Vec<u8>) -> Result<(), RegistryError> {
        if data.len() > MAX_NETWORK_BYTES {
            return Err(RegistryError::InvalidDefinition(format!(
                "the network data of a recipe is at most {MAX_NETWORK_BYTES} bytes"
            )));
        }
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if !inner.by_id.contains_key(id) {
            return Err(RegistryError::InvalidDefinition(format!(
                "recipe '{id}' is not registered"
            )));
        }
        inner.network_data.insert(id.to_string(), data);
        Ok(())
    }

    /// The network encoding of a recipe, if the plugin supplied one.
    #[must_use]
    pub fn network_data(&self, id: &str) -> Option<Vec<u8>> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .network_data
            .get(id)
            .cloned()
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<Arc<PluginRecipe>> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner
            .by_id
            .get(id)
            .map(|&index| Arc::clone(&inner.recipes[index]))
    }
}

fn validate(recipe: &PluginRecipe) -> Result<(), RegistryError> {
    let invalid = |message: String| Err(RegistryError::InvalidDefinition(message));
    if recipe.ingredients.len() > MAX_RECIPE_ENTRIES
        || recipe.results.len() > MAX_RECIPE_ENTRIES
        || recipe.fluid_ingredients.len() > MAX_RECIPE_ENTRIES
        || recipe.fluid_results.len() > MAX_RECIPE_ENTRIES
    {
        return invalid(format!(
            "a recipe has at most {MAX_RECIPE_ENTRIES} ingredients, results and fluids each"
        ));
    }
    if recipe.extra.len() > MAX_EXTRA_BYTES {
        return invalid(format!(
            "the extra data of a recipe is at most {MAX_EXTRA_BYTES} bytes"
        ));
    }
    if recipe.ingredients.iter().any(|entry| entry.count == 0) {
        return invalid("an ingredient needs a count of at least 1".to_string());
    }
    if recipe
        .fluid_ingredients
        .iter()
        .any(|fluid| fluid.amount == 0)
        || recipe.fluid_results.iter().any(|fluid| fluid.amount == 0)
    {
        return invalid("a fluid needs an amount of at least 1".to_string());
    }
    if recipe.results.iter().any(|output| output.count == 0) {
        return invalid("a result needs a count of at least 1".to_string());
    }
    if recipe
        .results
        .iter()
        .any(|output| !(0.0..=1.0).contains(&output.chance))
    {
        return invalid("the chance of a result is between 0 and 1".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe(id: &str, type_key: &str) -> PluginRecipe {
        PluginRecipe {
            id: id.to_string(),
            type_key: type_key.to_string(),
            ingredients: vec![SizedIngredient {
                ingredient: OwnedRecipeIngredient::Simple("minecraft:iron_ingot".to_string()),
                count: 1,
            }],
            results: vec![RecipeOutput {
                item_id: "minecraft:iron_nugget".to_string(),
                count: 9,
                chance: 1.0,
            }],
            fluid_ingredients: Vec::new(),
            fluid_results: Vec::new(),
            extra: Vec::new(),
        }
    }

    #[test]
    fn types_and_recipes_are_append_only() {
        let registry = PluginRecipeRegistry::new();
        assert!(
            registry
                .add_recipe(recipe("test:a", "test:pressing"))
                .is_err()
        );
        registry.register_type("test:pressing").unwrap();
        registry.register_type("test:pressing").unwrap();
        assert!(registry.register_type("minecraft:pressing").is_err());
        registry
            .add_recipe(recipe("test:a", "test:pressing"))
            .unwrap();
        registry
            .add_recipe(recipe("test:a", "test:pressing"))
            .unwrap();
        let mut changed = recipe("test:a", "test:pressing");
        changed.results[0].count = 2;
        assert!(matches!(
            registry.add_recipe(changed),
            Err(RegistryError::Duplicate(_))
        ));
        let mut bad_chance = recipe("test:b", "test:pressing");
        bad_chance.results[0].chance = 1.5;
        assert!(registry.add_recipe(bad_chance).is_err());
        assert_eq!(registry.recipes_of_type("test:pressing").len(), 1);
        assert!(registry.recipes_of_type("test:other").is_empty());
        assert_eq!(registry.get("test:a").unwrap().id, "test:a");
        assert_eq!(registry.type_keys(), vec!["test:pressing"]);
    }

    #[test]
    fn network_data_needs_a_recipe() {
        let registry = PluginRecipeRegistry::new();
        registry.register_type("test:pressing").unwrap();
        assert!(registry.set_network_data("test:a", vec![1]).is_err());
        registry
            .add_recipe(recipe("test:a", "test:pressing"))
            .unwrap();
        registry.set_network_data("test:a", vec![1, 2]).unwrap();
        assert_eq!(registry.network_data("test:a"), Some(vec![1, 2]));
        assert_eq!(registry.network_data("test:b"), None);
        assert!(
            registry
                .set_network_data("test:a", vec![0; MAX_NETWORK_BYTES + 1])
                .is_err()
        );
    }
}
