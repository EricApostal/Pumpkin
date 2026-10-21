//! `neoforge:recipe_content`: the recipes of the recipe types client mods asked for.
//!
//! A plugin declares the recipe types its client mod syncs (`declare-synced-recipe-types`). The
//! payload is the set of those types (registry ids of `minecraft:recipe_type`) and the recipes of
//! them, each as the recipe id, the serializer (registry id of `minecraft:recipe_serializer`) and
//! the body of the serializer's stream codec. Vanilla recipe types are encoded from the dynamic
//! recipes, plugin types pass the bytes the plugin supplied through.
//!
//! The vanilla registry orders below are the 1.21.x ones and not verified against a 26.3 client.
//! Plugin types follow the vanilla entries in the order they were declared, in both registries.

use std::sync::{PoisonError, RwLock};

use pumpkin_data::item::Item;
use pumpkin_data::recipes::RecipeCategoryTypes;
use pumpkin_protocol::codec::recipe::{
    DynamicRecipe, OwnedCookingRecipe, OwnedCookingRecipeType, OwnedCraftingRecipe,
    OwnedRecipeIngredient, OwnedRecipeResult,
};

use super::{buf::Writer, ident::normalize};
use crate::server::plugin_recipe::PluginRecipeRegistry;

pub const RECIPE_TYPE_REGISTRY: &str = "minecraft:recipe_type";
pub const RECIPE_SERIALIZER_REGISTRY: &str = "minecraft:recipe_serializer";

/// The vanilla `minecraft:recipe_type` entries in registry order.
pub const VANILLA_RECIPE_TYPES: [&str; 8] = [
    "blasting",
    "brewing",
    "campfire_cooking",
    "crafting",
    "smelting",
    "smithing",
    "smoking",
    "stonecutting",
];

/// The vanilla `minecraft:recipe_serializer` entries in registry order.
///
/// The order is the id order of the registry, and the frozen registry sync replaces the client's
/// registry with exactly this list. `crafting_special_armordye` and `crafting_special_tippedarrow`
/// are gone since 1.21.2 (`crafting_dye`/`crafting_imbue` replaced them), and `brewing` was added.
pub const VANILLA_RECIPE_SERIALIZERS: [&str; 22] = [
    "blasting",
    "brewing",
    "campfire_cooking",
    "crafting_decorated_pot",
    "crafting_dye",
    "crafting_imbue",
    "crafting_shaped",
    "crafting_shapeless",
    "crafting_special_bannerduplicate",
    "crafting_special_bookcloning",
    "crafting_special_firework_rocket",
    "crafting_special_firework_star",
    "crafting_special_firework_star_fade",
    "crafting_special_mapextending",
    "crafting_special_repairitem",
    "crafting_special_shielddecoration",
    "crafting_transmute",
    "smelting",
    "smithing_transform",
    "smithing_trim",
    "smoking",
    "stonecutting",
];

const MAX_SYNCED_TYPES: usize = 1024;

/// Declared recipe types by plugin name.
static SYNCED: RwLock<Vec<(String, Vec<String>)>> = RwLock::new(Vec::new());

/// Brings a declared type key into the canonical recipe type form: the finer vanilla names
/// (`minecraft:crafting_shaped`, `minecraft:smithing_trim`) become their recipe type.
pub fn canonical_type(key: &str) -> Result<String, String> {
    let key = normalize(key).map_err(|error| format!("recipe type: {error}"))?;
    Ok(match key.as_str() {
        "minecraft:crafting_shaped"
        | "minecraft:crafting_shapeless"
        | "minecraft:crafting_transmute"
        | "minecraft:crafting_decorated_pot" => "minecraft:crafting".to_string(),
        "minecraft:smithing_transform" | "minecraft:smithing_trim" => {
            "minecraft:smithing".to_string()
        }
        _ => key,
    })
}

fn is_vanilla(key: &str) -> bool {
    key.strip_prefix("minecraft:")
        .is_some_and(|path| VANILLA_RECIPE_TYPES.contains(&path))
}

/// Sets the recipe types `owner` syncs to `NeoForge` clients, replacing its previous list.
pub fn declare_synced_types(owner: &str, types: &[String]) -> Result<(), String> {
    if types.len() > MAX_SYNCED_TYPES {
        return Err(format!("at most {MAX_SYNCED_TYPES} synced recipe types"));
    }
    if pumpkin_data::item::Item::is_dynamic_registry_frozen() {
        return Err("synced recipe types can only be declared before players connect".into());
    }
    let mut canonical = Vec::new();
    for key in types {
        let key = canonical_type(key)?;
        if key.starts_with("minecraft:") && !is_vanilla(&key) {
            return Err(format!("'{key}' is not a recipe type"));
        }
        if !canonical.contains(&key) {
            canonical.push(key);
        }
    }
    let mut store = SYNCED.write().unwrap_or_else(PoisonError::into_inner);
    match store.iter_mut().find(|(other, _)| other == owner) {
        Some((_, previous)) => *previous = canonical,
        None => store.push((owner.to_string(), canonical)),
    }
    Ok(())
}

/// Removes the synced recipe types of `owner`.
pub fn withdraw(owner: &str) {
    SYNCED
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|(other, _)| other != owner);
}

/// All synced recipe types of all plugins, without duplicates, in declaration order.
#[must_use]
pub fn synced_types() -> Vec<String> {
    let store = SYNCED.read().unwrap_or_else(PoisonError::into_inner);
    let mut all: Vec<String> = Vec::new();
    for (_, types) in store.iter() {
        for key in types {
            if !all.contains(key) {
                all.push(key.clone());
            }
        }
    }
    all
}

/// The synced types that are not vanilla, which get entries in the recipe type and serializer
/// registries after the vanilla ones.
#[must_use]
pub fn extra_types() -> Vec<String> {
    synced_types()
        .into_iter()
        .filter(|key| !is_vanilla(key))
        .collect()
}

/// The registry extras for the registry synchronisation: `(registry key, all entries)` for the
/// recipe type and serializer registries, empty when no plugin type is synced.
#[must_use]
pub fn registry_extras() -> Vec<(String, Vec<String>)> {
    let extras = extra_types();
    if extras.is_empty() {
        return Vec::new();
    }
    let with_vanilla = |vanilla: &[&str]| -> Vec<String> {
        vanilla
            .iter()
            .map(|name| format!("minecraft:{name}"))
            .chain(extras.iter().cloned())
            .collect()
    };
    vec![
        (
            RECIPE_TYPE_REGISTRY.to_string(),
            with_vanilla(&VANILLA_RECIPE_TYPES),
        ),
        (
            RECIPE_SERIALIZER_REGISTRY.to_string(),
            with_vanilla(&VANILLA_RECIPE_SERIALIZERS),
        ),
    ]
}

/// The registry id of a recipe type.
fn type_id(key: &str, extras: &[String]) -> Option<i32> {
    if let Some(path) = key.strip_prefix("minecraft:") {
        return VANILLA_RECIPE_TYPES
            .iter()
            .position(|name| *name == path)
            .and_then(|index| i32::try_from(index).ok());
    }
    extras
        .iter()
        .position(|extra| extra == key)
        .and_then(|index| i32::try_from(index + VANILLA_RECIPE_TYPES.len()).ok())
}

fn vanilla_serializer_id(name: &str) -> i32 {
    VANILLA_RECIPE_SERIALIZERS
        .iter()
        .position(|serializer| *serializer == name)
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(0)
}

fn extra_serializer_id(key: &str, extras: &[String]) -> Option<i32> {
    extras
        .iter()
        .position(|extra| extra == key)
        .and_then(|index| i32::try_from(index + VANILLA_RECIPE_SERIALIZERS.len()).ok())
}

/// `Ingredient.CONTENTS_STREAM_CODEC`: a holder set of items. A tag is sent as a reference, which
/// the client resolves in the tags the server sent.
///
/// Returns whether the ingredient could be written: a tag the server does not have would make the
/// client throw while decoding (`ByteBufCodecs` holder sets use `orElseThrow`), which drops the
/// whole payload, so such a recipe is left out instead.
fn write_ingredient(w: &mut Writer, ingredient: &OwnedRecipeIngredient) -> bool {
    let ids: Vec<i32> = match ingredient {
        OwnedRecipeIngredient::Simple(id) => item_id(id).into_iter().collect(),
        OwnedRecipeIngredient::OneOf(ids) => ids.iter().filter_map(|id| item_id(id)).collect(),
        OwnedRecipeIngredient::Tagged(tag) => {
            let tag = tag.strip_prefix('#').unwrap_or(tag);
            let tag = if tag.contains(':') {
                tag.to_string()
            } else {
                format!("minecraft:{tag}")
            };
            if !pumpkin_data::dynamic_tag::tag_exists(pumpkin_data::tag::RegistryKey::Item, &tag) {
                return false;
            }
            w.var_int(0);
            w.string(&tag);
            return true;
        }
    };
    w.var_len(ids.len() + 1);
    for id in ids {
        w.var_int(id);
    }
    true
}

/// `Ingredient.OPTIONAL_CONTENTS_STREAM_CODEC`: no presence flag, an empty slot is an empty holder
/// set (`ByteBufCodecs.holderSet` writes `len + 1`, so an empty one is `1`).
fn write_optional_ingredient(w: &mut Writer, ingredient: Option<&OwnedRecipeIngredient>) -> bool {
    match ingredient {
        Some(ingredient) => write_ingredient(w, ingredient),
        None => {
            w.var_len(1);
            true
        }
    }
}

fn item_id(key: &str) -> Option<i32> {
    Item::from_registry_key(key).map(|item| i32::from(item.id))
}

/// An `ItemStackTemplate` without components: the item, the count and an empty patch.
fn write_result(w: &mut Writer, result: &OwnedRecipeResult) -> bool {
    let Some(id) = item_id(&result.item_id) else {
        return false;
    };
    w.var_int(id);
    w.var_int(i32::from(result.count.max(1)));
    w.var_int(0);
    w.var_int(0);
    true
}

const fn crafting_category(category: &RecipeCategoryTypes) -> i32 {
    match category {
        RecipeCategoryTypes::Building | RecipeCategoryTypes::Blocks => 0,
        RecipeCategoryTypes::Restone => 1,
        RecipeCategoryTypes::Equipment => 2,
        RecipeCategoryTypes::Food | RecipeCategoryTypes::Misc => 3,
    }
}

const fn cooking_category(category: &RecipeCategoryTypes) -> i32 {
    match category {
        RecipeCategoryTypes::Food => 0,
        RecipeCategoryTypes::Blocks | RecipeCategoryTypes::Building => 1,
        _ => 2,
    }
}

/// One encoded recipe: the id, the type key and the serializer id and body.
struct Encoded {
    id: String,
    type_key: &'static str,
    serializer: i32,
    body: Vec<u8>,
}

fn encode_crafting(recipe: &OwnedCraftingRecipe, index: usize) -> Option<Encoded> {
    let mut w = Writer::new();
    match recipe {
        OwnedCraftingRecipe::Shaped {
            recipe_id,
            category,
            group,
            show_notification,
            key,
            pattern,
            result,
        } => {
            w.string(group.as_deref().unwrap_or(""));
            w.var_int(crafting_category(category));
            let width = pattern.iter().map(|row| row.chars().count()).max()?;
            w.var_len(width);
            w.var_len(pattern.len());
            for row in pattern {
                let mut cells = row.chars();
                for _ in 0..width {
                    let ingredient = cells
                        .next()
                        .filter(|c| *c != ' ')
                        .and_then(|c| key.iter().find(|(k, _)| *k == c))
                        .map(|(_, ingredient)| ingredient);
                    if !write_optional_ingredient(&mut w, ingredient) {
                        return None;
                    }
                }
            }
            if !write_result(&mut w, result) {
                return None;
            }
            w.bool(*show_notification);
            Some(Encoded {
                id: recipe_id
                    .clone()
                    .unwrap_or_else(|| format!("pumpkin:synced/crafting_shaped_{index}")),
                type_key: "minecraft:crafting",
                serializer: vanilla_serializer_id("crafting_shaped"),
                body: w.into_bytes(),
            })
        }
        OwnedCraftingRecipe::Shapeless {
            recipe_id,
            category,
            group,
            ingredients,
            result,
        } => {
            w.string(group.as_deref().unwrap_or(""));
            w.var_int(crafting_category(category));
            w.var_len(ingredients.len());
            for ingredient in ingredients {
                if !write_ingredient(&mut w, ingredient) {
                    return None;
                }
            }
            if !write_result(&mut w, result) {
                return None;
            }
            Some(Encoded {
                id: recipe_id
                    .clone()
                    .unwrap_or_else(|| format!("pumpkin:synced/crafting_shapeless_{index}")),
                type_key: "minecraft:crafting",
                serializer: vanilla_serializer_id("crafting_shapeless"),
                body: w.into_bytes(),
            })
        }
    }
}

fn encode_cooking(recipe: &OwnedCookingRecipeType) -> Option<Encoded> {
    let (cooking, type_key, serializer): (&OwnedCookingRecipe, &'static str, &str) = match recipe {
        OwnedCookingRecipeType::Smelting(r) => (r, "minecraft:smelting", "smelting"),
        OwnedCookingRecipeType::Blasting(r) => (r, "minecraft:blasting", "blasting"),
        OwnedCookingRecipeType::Smoking(r) => (r, "minecraft:smoking", "smoking"),
        OwnedCookingRecipeType::CampfireCooking(r) => {
            (r, "minecraft:campfire_cooking", "campfire_cooking")
        }
    };
    let mut w = Writer::new();
    w.string(cooking.group.as_deref().unwrap_or(""));
    w.var_int(cooking_category(&cooking.category));
    if !write_ingredient(&mut w, &cooking.ingredient) {
        return None;
    }
    if !write_result(&mut w, &cooking.result) {
        return None;
    }
    w.bytes(&cooking.experience.to_be_bytes());
    w.var_int(cooking.cooking_time);
    Some(Encoded {
        id: cooking.recipe_id.clone(),
        type_key,
        serializer: vanilla_serializer_id(serializer),
        body: w.into_bytes(),
    })
}

fn encode_raw(recipe: &pumpkin_protocol::codec::recipe::OwnedRawRecipe) -> Option<Encoded> {
    let mut w = Writer::new();
    match recipe.recipe_type.as_str() {
        "minecraft:stonecutting" => {
            w.string(recipe.group.as_deref().unwrap_or(""));
            if !write_ingredient(&mut w, recipe.ingredients.first()?) {
                return None;
            }
            if !write_result(&mut w, recipe.results.first()?) {
                return None;
            }
            Some(Encoded {
                id: recipe.recipe_id.clone(),
                type_key: "minecraft:stonecutting",
                serializer: vanilla_serializer_id("stonecutting"),
                body: w.into_bytes(),
            })
        }
        "minecraft:smithing_transform" => {
            for index in 0..3 {
                if !write_optional_ingredient(&mut w, recipe.ingredients.get(index)) {
                    return None;
                }
            }
            if !write_result(&mut w, recipe.results.first()?) {
                return None;
            }
            Some(Encoded {
                id: recipe.recipe_id.clone(),
                type_key: "minecraft:smithing",
                serializer: vanilla_serializer_id("smithing_transform"),
                body: w.into_bytes(),
            })
        }
        _ => None,
    }
}

fn encode_dynamic(recipe: &DynamicRecipe, index: usize) -> Option<Encoded> {
    match recipe {
        DynamicRecipe::Crafting(crafting) => encode_crafting(crafting, index),
        DynamicRecipe::Cooking(cooking) => encode_cooking(cooking),
        DynamicRecipe::Raw(raw) => encode_raw(raw),
        DynamicRecipe::Brewing(_) => None,
    }
}

/// Builds the `neoforge:recipe_content` payload for `types` (canonical type keys). Types the
/// registries do not know are left out. Recipes of plugin types without network data are skipped.
#[must_use]
pub fn encode_recipe_content(
    types: &[String],
    dynamic: &[DynamicRecipe],
    plugin: &PluginRecipeRegistry,
) -> Vec<u8> {
    let extras = extra_types();
    let wanted: Vec<(&String, i32)> = types
        .iter()
        .filter_map(|key| type_id(key, &extras).map(|id| (key, id)))
        .collect();

    let mut recipes: Vec<Encoded> = Vec::new();
    for (index, recipe) in dynamic.iter().enumerate() {
        if let Some(encoded) = encode_dynamic(recipe, index)
            && wanted
                .iter()
                .any(|(key, _)| key.as_str() == encoded.type_key)
        {
            recipes.push(encoded);
        }
    }
    for (key, _) in &wanted {
        if is_vanilla(key) {
            continue;
        }
        let Some(serializer) = extra_serializer_id(key, &extras) else {
            continue;
        };
        for recipe in plugin.recipes_of_type(key) {
            if let Some(body) = plugin.network_data(&recipe.id) {
                recipes.push(Encoded {
                    id: recipe.id.clone(),
                    type_key: "",
                    serializer,
                    body,
                });
            }
        }
    }

    let mut w = Writer::new();
    w.var_len(wanted.len());
    for (_, id) in &wanted {
        w.var_int(*id);
    }
    w.var_len(recipes.len());
    for recipe in &recipes {
        w.string(&recipe.id);
        w.var_int(recipe.serializer);
        w.bytes(&recipe.body);
    }
    w.into_bytes()
}

/// The payload for the recipes the manager has now and the types plugins declared. The empty
/// payload when no type is synced.
#[must_use]
pub fn recipe_content(manager: &crate::server::recipe::RecipeManager) -> Vec<u8> {
    let types = synced_types();
    manager.with_dynamic_recipes(|dynamic| {
        encode_recipe_content(&types, dynamic, manager.plugin_recipes())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::neoforge::buf::Reader;

    fn smelting() -> DynamicRecipe {
        DynamicRecipe::Cooking(OwnedCookingRecipeType::Smelting(OwnedCookingRecipe {
            recipe_id: "test:iron".to_string(),
            category: RecipeCategoryTypes::Blocks,
            group: None,
            ingredient: OwnedRecipeIngredient::Tagged("minecraft:logs".to_string()),
            cooking_time: 200,
            experience: 0.5,
            result: OwnedRecipeResult {
                item_id: "minecraft:iron_ingot".to_string(),
                count: 1,
            },
        }))
    }

    #[test]
    fn canonical_types() {
        assert_eq!(
            canonical_type("crafting_shaped").unwrap(),
            "minecraft:crafting"
        );
        assert_eq!(
            canonical_type("minecraft:smithing_trim").unwrap(),
            "minecraft:smithing"
        );
        assert_eq!(
            canonical_type("create:pressing").unwrap(),
            "create:pressing"
        );
        assert!(canonical_type("Bad Type").is_err());
        assert_eq!(VANILLA_RECIPE_SERIALIZERS.len(), 22);
        // The ids the frozen registry sync gives the client.
        assert_eq!(vanilla_serializer_id("smelting"), 17);
        assert_eq!(type_id("minecraft:smelting", &[]), Some(4));
        assert_eq!(type_id("minecraft:stonecutting", &[]), Some(7));
    }

    #[test]
    fn vanilla_smelting_payload() {
        let plugin = PluginRecipeRegistry::new();
        let bytes = encode_recipe_content(
            &[
                "minecraft:smelting".to_string(),
                "minecraft:blasting".to_string(),
            ],
            &[smelting()],
            &plugin,
        );
        let mut r = Reader::new(&bytes);
        assert_eq!(r.var_int().unwrap(), 2);
        // The recipe type ids of `minecraft:smelting` and `minecraft:blasting`.
        assert_eq!(r.var_int().unwrap(), 4);
        assert_eq!(r.var_int().unwrap(), 0);
        assert_eq!(r.var_int().unwrap(), 1);
        assert_eq!(r.string().unwrap(), "test:iron");
        // The `minecraft:smelting` serializer id.
        assert_eq!(r.var_int().unwrap(), 17);
        assert_eq!(r.string().unwrap(), "");
        assert_eq!(r.var_int().unwrap(), 1);
        // A tag reference.
        assert_eq!(r.var_int().unwrap(), 0);
        assert_eq!(r.string().unwrap(), "minecraft:logs");
        assert_eq!(r.var_int().unwrap(), i32::from(Item::IRON_INGOT.id));
        assert_eq!(r.var_int().unwrap(), 1);
        assert_eq!(r.var_int().unwrap(), 0);
        assert_eq!(r.var_int().unwrap(), 0);
        assert_eq!(r.bytes(4).unwrap(), 0.5f32.to_be_bytes());
        assert_eq!(r.var_int().unwrap(), 200);
        r.finish().unwrap();
    }

    #[test]
    fn recipes_with_a_tag_the_client_does_not_have_are_left_out() {
        // The client resolves a tag reference in the tags the server sent and throws when it is
        // missing (`ByteBufCodecs` holder sets), which drops the whole payload.
        let DynamicRecipe::Cooking(OwnedCookingRecipeType::Smelting(cooking)) = smelting() else {
            unreachable!()
        };
        let recipe = DynamicRecipe::Cooking(OwnedCookingRecipeType::Smelting(OwnedCookingRecipe {
            ingredient: OwnedRecipeIngredient::Tagged("no_such_mod:no_such_tag".to_string()),
            ..cooking
        }));
        let plugin = PluginRecipeRegistry::new();
        let bytes = encode_recipe_content(&["minecraft:smelting".to_string()], &[recipe], &plugin);
        let mut r = Reader::new(&bytes);
        assert_eq!(r.var_int().unwrap(), 1);
        assert_eq!(r.var_int().unwrap(), 4);
        assert_eq!(r.var_int().unwrap(), 0);
        r.finish().unwrap();
    }

    #[test]
    fn unsynced_types_are_left_out() {
        let plugin = PluginRecipeRegistry::new();
        let bytes = encode_recipe_content(
            &["minecraft:stonecutting".to_string()],
            &[smelting()],
            &plugin,
        );
        assert_eq!(bytes, vec![1, 7, 0]);
        assert_eq!(encode_recipe_content(&[], &[], &plugin), vec![0, 0]);
    }
}
