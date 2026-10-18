//! Read only recipe queries for plugins, over every recipe the server knows: the vanilla tables,
//! the dynamic recipes (datapacks and the recipes plugins registered as vanilla types) and the
//! recipes of plugin defined types.
//!
//! Everything comes back as plain data. Vanilla crafting, stonecutting and smithing recipes have no
//! id in the generated tables, so their [`RecipeInfo::id`] is `None`.

use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::recipes::{
    CookingRecipeKind, CookingRecipeType, CraftingRecipeTypes, RECIPES_COOKING, RECIPES_CRAFTING,
    RECIPES_SMITHING_TRANSFORM, RECIPES_STONECUTTING, RecipeIngredientTypes,
    get_cooking_recipe_with_ingredient,
};
use pumpkin_inventory::crafting::crafting_inventory::CraftingInventory;
use pumpkin_inventory::crafting::crafting_screen_handler::{MatchedCrafting, find_crafting_match};
use pumpkin_inventory::crafting::recipe_provider::RecipeProvider;
use pumpkin_inventory::inventory::Inventory;
use pumpkin_protocol::codec::recipe::{
    DynamicRecipe, OwnedCookingRecipe, OwnedCookingRecipeType, OwnedCraftingRecipe,
    OwnedRecipeIngredient, OwnedRecipeResult,
};

use super::RecipeManager;
use super::plugin_recipe::{PluginRecipe, RecipeFluid, RecipeOutput, SizedIngredient};

pub const TYPE_CRAFTING_SHAPED: &str = "minecraft:crafting_shaped";
pub const TYPE_CRAFTING_SHAPELESS: &str = "minecraft:crafting_shapeless";
pub const TYPE_CRAFTING_TRANSMUTE: &str = "minecraft:crafting_transmute";
pub const TYPE_CRAFTING_DECORATED_POT: &str = "minecraft:crafting_decorated_pot";
pub const TYPE_CRAFTING_SPECIAL: &str = "minecraft:crafting_special";
pub const TYPE_SMELTING: &str = "minecraft:smelting";
pub const TYPE_BLASTING: &str = "minecraft:blasting";
pub const TYPE_SMOKING: &str = "minecraft:smoking";
pub const TYPE_CAMPFIRE_COOKING: &str = "minecraft:campfire_cooking";
pub const TYPE_STONECUTTING: &str = "minecraft:stonecutting";
pub const TYPE_SMITHING_TRANSFORM: &str = "minecraft:smithing_transform";

/// The layout of a shaped crafting recipe.
#[derive(Clone, Debug, PartialEq)]
pub struct RecipeShape {
    pub pattern: Vec<String>,
    pub key: Vec<(char, OwnedRecipeIngredient)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CookingInfo {
    pub experience: f32,
    /// Cooking time in ticks.
    pub cooking_time: u32,
}

/// A recipe as plain data.
#[derive(Clone, Debug, PartialEq)]
pub struct RecipeInfo {
    pub id: Option<String>,
    /// The recipe type key, `minecraft:smelting`, `create:pressing`, ...
    pub recipe_type: String,
    pub group: Option<String>,
    /// For a shaped recipe one per filled cell of the pattern, row by row.
    pub ingredients: Vec<SizedIngredient>,
    pub results: Vec<RecipeOutput>,
    pub fluid_ingredients: Vec<RecipeFluid>,
    pub fluid_results: Vec<RecipeFluid>,
    pub shape: Option<RecipeShape>,
    pub cooking: Option<CookingInfo>,
    /// The opaque blob of a recipe of a plugin defined type, empty for the others.
    pub extra: Vec<u8>,
}

impl RecipeInfo {
    fn new(recipe_type: &str, id: Option<String>, group: Option<String>) -> Self {
        Self {
            id,
            recipe_type: recipe_type.to_string(),
            group,
            ingredients: Vec::new(),
            results: Vec::new(),
            fluid_ingredients: Vec::new(),
            fluid_results: Vec::new(),
            shape: None,
            cooking: None,
            extra: Vec::new(),
        }
    }
}

/// The recipe a crafting grid matched.
#[derive(Clone, Debug, PartialEq)]
pub struct CraftingQueryResult {
    pub id: Option<String>,
    pub recipe_type: String,
    pub output: RecipeOutput,
}

/// The recipe that cooks an item.
#[derive(Clone, Debug, PartialEq)]
pub struct CookingQueryResult {
    pub id: Option<String>,
    pub output: RecipeOutput,
    pub experience: f32,
    /// Cooking time in ticks.
    pub cooking_time: u32,
}

fn qualified(id: &str) -> String {
    if id.contains(':') {
        id.to_string()
    } else {
        format!("minecraft:{id}")
    }
}

fn output(item_id: &str, count: u8) -> RecipeOutput {
    RecipeOutput {
        item_id: qualified(item_id),
        count: u32::from(count),
        chance: 1.0,
    }
}

fn owned_output(result: &OwnedRecipeResult) -> RecipeOutput {
    output(&result.item_id, result.count)
}

fn sized(ingredient: OwnedRecipeIngredient) -> SizedIngredient {
    // Tags are written without the leading '#' that the generated tables carry.
    let ingredient = match ingredient {
        OwnedRecipeIngredient::Tagged(tag) => {
            OwnedRecipeIngredient::Tagged(tag.strip_prefix('#').unwrap_or(&tag).to_string())
        }
        other => other,
    };
    SizedIngredient {
        ingredient,
        count: 1,
    }
}

fn vanilla_ingredient(ingredient: &RecipeIngredientTypes) -> OwnedRecipeIngredient {
    match ingredient {
        RecipeIngredientTypes::Simple(id) => OwnedRecipeIngredient::Simple((*id).to_string()),
        RecipeIngredientTypes::Tagged(tag) => OwnedRecipeIngredient::Tagged((*tag).to_string()),
        RecipeIngredientTypes::OneOf(ids) => {
            OwnedRecipeIngredient::OneOf(ids.iter().map(|id| (*id).to_string()).collect())
        }
    }
}

fn shaped_info(
    id: Option<String>,
    group: Option<String>,
    key: Vec<(char, OwnedRecipeIngredient)>,
    pattern: Vec<String>,
    result: RecipeOutput,
) -> RecipeInfo {
    let mut info = RecipeInfo::new(TYPE_CRAFTING_SHAPED, id, group);
    for cell in pattern.iter().flat_map(|row| row.chars()) {
        if let Some((_, ingredient)) = key.iter().find(|(k, _)| *k == cell) {
            info.ingredients.push(sized(ingredient.clone()));
        }
    }
    info.results.push(result);
    info.shape = Some(RecipeShape { pattern, key });
    info
}

const fn crafting_type_key(recipe: &CraftingRecipeTypes) -> &'static str {
    match recipe {
        CraftingRecipeTypes::CraftingShaped { .. } => TYPE_CRAFTING_SHAPED,
        CraftingRecipeTypes::CraftingShapeless { .. } => TYPE_CRAFTING_SHAPELESS,
        CraftingRecipeTypes::CraftingTransmute { .. } => TYPE_CRAFTING_TRANSMUTE,
        CraftingRecipeTypes::CraftingDecoratedPot { .. } => TYPE_CRAFTING_DECORATED_POT,
        CraftingRecipeTypes::CraftingSpecial => TYPE_CRAFTING_SPECIAL,
    }
}

const fn owned_crafting_type_key(recipe: &OwnedCraftingRecipe) -> &'static str {
    match recipe {
        OwnedCraftingRecipe::Shaped { .. } => TYPE_CRAFTING_SHAPED,
        OwnedCraftingRecipe::Shapeless { .. } => TYPE_CRAFTING_SHAPELESS,
    }
}

/// `None` for the special recipes, which have no ingredients or results to describe.
fn vanilla_crafting_info(recipe: &CraftingRecipeTypes) -> Option<RecipeInfo> {
    match recipe {
        CraftingRecipeTypes::CraftingShaped {
            group,
            key,
            pattern,
            result,
            ..
        } => Some(shaped_info(
            None,
            group.map(ToString::to_string),
            key.iter()
                .map(|(k, ingredient)| (*k, vanilla_ingredient(ingredient)))
                .collect(),
            pattern.iter().map(ToString::to_string).collect(),
            output(result.id, result.count),
        )),
        CraftingRecipeTypes::CraftingShapeless {
            group,
            ingredients,
            result,
            ..
        } => {
            let mut info = RecipeInfo::new(
                TYPE_CRAFTING_SHAPELESS,
                None,
                group.map(ToString::to_string),
            );
            info.ingredients = ingredients
                .iter()
                .map(|ingredient| sized(vanilla_ingredient(ingredient)))
                .collect();
            info.results.push(output(result.id, result.count));
            Some(info)
        }
        CraftingRecipeTypes::CraftingTransmute {
            group,
            input,
            material,
            result,
            ..
        } => {
            let mut info = RecipeInfo::new(
                TYPE_CRAFTING_TRANSMUTE,
                None,
                group.map(ToString::to_string),
            );
            info.ingredients = vec![
                sized(vanilla_ingredient(input)),
                sized(vanilla_ingredient(material)),
            ];
            info.results.push(output(result.id, result.count));
            Some(info)
        }
        CraftingRecipeTypes::CraftingDecoratedPot { .. } | CraftingRecipeTypes::CraftingSpecial => {
            None
        }
    }
}

fn owned_crafting_info(recipe: &OwnedCraftingRecipe) -> RecipeInfo {
    match recipe {
        OwnedCraftingRecipe::Shaped {
            recipe_id,
            group,
            key,
            pattern,
            result,
            ..
        } => shaped_info(
            recipe_id.clone(),
            group.clone(),
            key.clone(),
            pattern.clone(),
            owned_output(result),
        ),
        OwnedCraftingRecipe::Shapeless {
            recipe_id,
            group,
            ingredients,
            result,
            ..
        } => {
            let mut info =
                RecipeInfo::new(TYPE_CRAFTING_SHAPELESS, recipe_id.clone(), group.clone());
            info.ingredients = ingredients.iter().cloned().map(sized).collect();
            info.results.push(owned_output(result));
            info
        }
    }
}

const fn cooking_type_key(kind: &CookingRecipeKind) -> &'static str {
    match kind {
        CookingRecipeKind::Smelting => TYPE_SMELTING,
        CookingRecipeKind::Blasting => TYPE_BLASTING,
        CookingRecipeKind::Smoking => TYPE_SMOKING,
        CookingRecipeKind::CampfireCooking => TYPE_CAMPFIRE_COOKING,
    }
}

fn owned_cooking_parts(
    recipe: &OwnedCookingRecipeType,
) -> (CookingRecipeKind, &OwnedCookingRecipe) {
    match recipe {
        OwnedCookingRecipeType::Smelting(r) => (CookingRecipeKind::Smelting, r),
        OwnedCookingRecipeType::Blasting(r) => (CookingRecipeKind::Blasting, r),
        OwnedCookingRecipeType::Smoking(r) => (CookingRecipeKind::Smoking, r),
        OwnedCookingRecipeType::CampfireCooking(r) => (CookingRecipeKind::CampfireCooking, r),
    }
}

fn vanilla_cooking_parts(
    recipe: &CookingRecipeType,
) -> (CookingRecipeKind, &pumpkin_data::recipes::CookingRecipe) {
    let kind = CookingRecipeKind::from(recipe);
    match recipe {
        CookingRecipeType::Blasting(r)
        | CookingRecipeType::Smelting(r)
        | CookingRecipeType::Smoking(r)
        | CookingRecipeType::CampfireCooking(r) => (kind, r),
    }
}

fn cooking_ticks(ticks: i32) -> u32 {
    u32::try_from(ticks).unwrap_or(0)
}

fn vanilla_cooking_info(
    kind: &CookingRecipeKind,
    recipe: &pumpkin_data::recipes::CookingRecipe,
) -> RecipeInfo {
    let mut info = RecipeInfo::new(
        cooking_type_key(kind),
        Some(recipe.recipe_id.to_string()),
        recipe.group.map(ToString::to_string),
    );
    info.ingredients
        .push(sized(vanilla_ingredient(&recipe.ingredient)));
    info.results
        .push(output(recipe.result.id, recipe.result.count));
    info.cooking = Some(CookingInfo {
        experience: recipe.experience,
        cooking_time: cooking_ticks(recipe.cookingtime),
    });
    info
}

fn owned_cooking_info(kind: &CookingRecipeKind, recipe: &OwnedCookingRecipe) -> RecipeInfo {
    let mut info = RecipeInfo::new(
        cooking_type_key(kind),
        Some(recipe.recipe_id.clone()),
        recipe.group.clone(),
    );
    info.ingredients.push(sized(recipe.ingredient.clone()));
    info.results.push(owned_output(&recipe.result));
    info.cooking = Some(CookingInfo {
        experience: recipe.experience,
        cooking_time: cooking_ticks(recipe.cooking_time),
    });
    info
}

fn plugin_info(recipe: &PluginRecipe) -> RecipeInfo {
    let mut info = RecipeInfo::new(&recipe.type_key, Some(recipe.id.clone()), None);
    info.ingredients.clone_from(&recipe.ingredients);
    info.results.clone_from(&recipe.results);
    info.fluid_ingredients.clone_from(&recipe.fluid_ingredients);
    info.fluid_results.clone_from(&recipe.fluid_results);
    info.extra.clone_from(&recipe.extra);
    info
}

impl RecipeManager {
    /// All recipes of a type: `minecraft:crafting_shaped`, `crafting_shapeless`,
    /// `crafting_transmute`, `smelting`, `blasting`, `smoking`, `campfire_cooking`, `stonecutting`,
    /// `smithing_transform` or the key of a plugin defined type. Unknown types have no recipes.
    #[must_use]
    pub fn find_recipes(&self, type_key: &str) -> Vec<RecipeInfo> {
        let mut found = Vec::new();
        match type_key {
            TYPE_CRAFTING_SHAPED | TYPE_CRAFTING_SHAPELESS | TYPE_CRAFTING_TRANSMUTE => {
                found.extend(
                    RECIPES_CRAFTING
                        .iter()
                        .filter(|recipe| crafting_type_key(recipe) == type_key)
                        .filter_map(vanilla_crafting_info),
                );
                self.with_dynamic_recipes(|dynamic| {
                    found.extend(dynamic.iter().filter_map(|recipe| match recipe {
                        DynamicRecipe::Crafting(crafting)
                            if owned_crafting_type_key(crafting) == type_key =>
                        {
                            Some(owned_crafting_info(crafting))
                        }
                        _ => None,
                    }));
                });
            }
            TYPE_SMELTING | TYPE_BLASTING | TYPE_SMOKING | TYPE_CAMPFIRE_COOKING => {
                found.extend(RECIPES_COOKING.iter().filter_map(|recipe| {
                    let (kind, recipe) = vanilla_cooking_parts(recipe);
                    (cooking_type_key(&kind) == type_key)
                        .then(|| vanilla_cooking_info(&kind, recipe))
                }));
                self.with_dynamic_recipes(|dynamic| {
                    found.extend(dynamic.iter().filter_map(|recipe| {
                        let DynamicRecipe::Cooking(cooking) = recipe else {
                            return None;
                        };
                        let (kind, recipe) = owned_cooking_parts(cooking);
                        (cooking_type_key(&kind) == type_key)
                            .then(|| owned_cooking_info(&kind, recipe))
                    }));
                });
            }
            TYPE_STONECUTTING => {
                found.extend(RECIPES_STONECUTTING.iter().map(|recipe| {
                    let mut info = RecipeInfo::new(
                        TYPE_STONECUTTING,
                        None,
                        recipe.group.map(ToString::to_string),
                    );
                    info.ingredients
                        .push(sized(vanilla_ingredient(&recipe.ingredient)));
                    info.results
                        .push(output(recipe.result.id, recipe.result.count));
                    info
                }));
            }
            TYPE_SMITHING_TRANSFORM => {
                found.extend(RECIPES_SMITHING_TRANSFORM.iter().map(|recipe| {
                    let mut info = RecipeInfo::new(TYPE_SMITHING_TRANSFORM, None, None);
                    info.ingredients = vec![
                        sized(vanilla_ingredient(&recipe.template)),
                        sized(vanilla_ingredient(&recipe.base)),
                        sized(vanilla_ingredient(&recipe.addition)),
                    ];
                    info.results
                        .push(output(recipe.result.id, recipe.result.count));
                    info
                }));
            }
            _ => {
                found.extend(
                    self.plugin_recipes()
                        .recipes_of_type(type_key)
                        .iter()
                        .map(|recipe| plugin_info(recipe)),
                );
            }
        }
        found
    }

    /// The recipe with this id. Looks at the recipes of plugin defined types, the dynamic recipes
    /// and the vanilla cooking recipes, the only vanilla recipes that have an id.
    #[must_use]
    pub fn find_recipe_by_id(&self, id: &str) -> Option<RecipeInfo> {
        if let Some(recipe) = self.plugin_recipes().get(id) {
            return Some(plugin_info(&recipe));
        }
        let dynamic = self.with_dynamic_recipes(|dynamic| {
            dynamic.iter().find_map(|recipe| match recipe {
                DynamicRecipe::Crafting(crafting) => {
                    let recipe_id = match crafting {
                        OwnedCraftingRecipe::Shaped { recipe_id, .. }
                        | OwnedCraftingRecipe::Shapeless { recipe_id, .. } => recipe_id,
                    };
                    (recipe_id.as_deref() == Some(id)).then(|| owned_crafting_info(crafting))
                }
                DynamicRecipe::Cooking(cooking) => {
                    let (kind, recipe) = owned_cooking_parts(cooking);
                    (recipe.recipe_id == id).then(|| owned_cooking_info(&kind, recipe))
                }
                DynamicRecipe::Brewing(_) => None,
            })
        });
        if dynamic.is_some() {
            return dynamic;
        }
        RECIPES_COOKING.iter().find_map(|recipe| {
            let (kind, recipe) = vanilla_cooking_parts(recipe);
            (recipe.recipe_id == id).then(|| vanilla_cooking_info(&kind, recipe))
        })
    }

    /// The crafting recipe a grid matches, `grid` row by row with `None` for an empty cell. Like the
    /// crafting table, only the items matter, not counts or components. Vanilla recipes are tried
    /// before the dynamic ones.
    #[must_use]
    pub fn find_crafting_recipe(
        &self,
        width: u8,
        height: u8,
        grid: &[Option<&'static Item>],
    ) -> Option<CraftingQueryResult> {
        let inventory = CraftingInventory::new(width, height);
        for (slot, item) in grid.iter().enumerate() {
            if let Some(item) = item {
                inventory.set_stack(slot, ItemStack::new(1, item));
            }
        }
        let matched = find_crafting_match(&inventory, Some(self as &dyn RecipeProvider))?;
        let (id, recipe_type) = match &matched.recipe {
            MatchedCrafting::Vanilla(recipe) => (None, crafting_type_key(recipe)),
            MatchedCrafting::Dynamic(recipe) => {
                let recipe_id = match recipe {
                    OwnedCraftingRecipe::Shaped { recipe_id, .. }
                    | OwnedCraftingRecipe::Shapeless { recipe_id, .. } => recipe_id.clone(),
                };
                (recipe_id, owned_crafting_type_key(recipe))
            }
        };
        Some(CraftingQueryResult {
            id,
            recipe_type: recipe_type.to_string(),
            output: output(&matched.result.item_id, matched.result.count),
        })
    }

    /// The recipe that cooks `item` in a station. The vanilla recipes come first, then the dynamic
    /// ones. The furnaces of the server only use the vanilla recipes, so a dynamic recipe is only
    /// reported here, nothing smelts it by itself.
    #[must_use]
    pub fn find_cooking_recipe(
        &self,
        item: &Item,
        kind: CookingRecipeKind,
    ) -> Option<CookingQueryResult> {
        if let Some(recipe) = get_cooking_recipe_with_ingredient(item, kind.clone()) {
            return Some(CookingQueryResult {
                id: Some(recipe.recipe_id.to_string()),
                output: output(recipe.result.id, recipe.result.count),
                experience: recipe.experience,
                cooking_time: cooking_ticks(recipe.cookingtime),
            });
        }
        self.with_dynamic_recipes(|dynamic| {
            dynamic.iter().find_map(|recipe| {
                let DynamicRecipe::Cooking(cooking) = recipe else {
                    return None;
                };
                let (recipe_kind, recipe) = owned_cooking_parts(cooking);
                (std::mem::discriminant(&recipe_kind) == std::mem::discriminant(&kind)
                    && recipe.ingredient.match_item(item))
                .then(|| CookingQueryResult {
                    id: Some(recipe.recipe_id.clone()),
                    output: owned_output(&recipe.result),
                    experience: recipe.experience,
                    cooking_time: cooking_ticks(recipe.cooking_time),
                })
            })
        })
    }
}
