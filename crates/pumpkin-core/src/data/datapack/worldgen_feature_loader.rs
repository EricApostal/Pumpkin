//! Additive world generation from datapacks: `worldgen/configured_feature` (ore),
//! `worldgen/placed_feature` and `neoforge/biome_modifier` (`neoforge:add_features`,
//! `neoforge:remove_features`).
//!
//! This is the subset that the additive generator (see
//! `pumpkin_world::generation::feature::additive`) supports. Files outside it are skipped with a
//! warning: configured features other than `minecraft:ore`, placed features with modifiers other
//! than `count`, `rarity_filter`, `in_square`, `height_range` (uniform or triangle) and `biome`,
//! and adding vanilla placed features to other biomes.

use std::fs;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use pumpkin_data::placed_feature::PlacedFeature;
use pumpkin_util::math::int_provider::IntProvider;
use pumpkin_util::y_offset::YOffset;
use pumpkin_world::generation::feature::additive::{
    self, AdditiveRule, FeatureKind, HeightShape, HeightSpec, OreSpec, OreTargetSpec, Source,
};
use serde_json::Value;
use tracing::{debug, warn};

struct PlacedDef {
    /// Key of the registered feature this placement uses.
    feature: String,
    count: IntProvider,
    rarity: Option<u32>,
    in_square: bool,
    height: HeightSpec,
}

enum Modifier {
    Add {
        biomes: Vec<String>,
        features: Vec<String>,
        step: usize,
    },
    Remove {
        biomes: Vec<String>,
        features: Vec<String>,
    },
}

#[derive(Default)]
struct Pending {
    configured: Vec<(String, OreSpec)>,
    placed: Vec<(String, PlacedDef)>,
    modifiers: Vec<(String, Modifier)>,
}

static PENDING: Mutex<Pending> = Mutex::new(Pending {
    configured: Vec::new(),
    placed: Vec::new(),
    modifiers: Vec::new(),
});

const STEPS: [&str; 11] = [
    "raw_generation",
    "lakes",
    "local_modifications",
    "underground_structures",
    "surface_structures",
    "strongest_stronghold",
    "underground_ores",
    "underground_decoration",
    "fluid_springs",
    "vegetal_decoration",
    "top_layer_modification",
];

/// Forgets everything of the previous load, before the datapacks load again.
pub fn clear() {
    additive::clear_datapack_entries();
    *PENDING.lock().unwrap_or_else(PoisonError::into_inner) = Pending::default();
}

/// Reads the additive worldgen files of one namespace directory.
pub fn load_namespace(namespace: &str, ns_path: &Path) {
    let mut pending = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
    let worldgen = ns_path.join("worldgen");
    visit_json(
        &worldgen.join("configured_feature"),
        namespace,
        "",
        &mut |key, json| match parse_configured(&json) {
            Ok(Some(ore)) => pending.configured.push((key, ore)),
            Ok(None) => debug!("Skipping configured feature {key}: not an ore"),
            Err(error) => warn!("Invalid configured feature {key}: {error}"),
        },
    );
    visit_json(
        &worldgen.join("placed_feature"),
        namespace,
        "",
        &mut |key, json| match parse_placed(&key, &json, &mut pending.configured) {
            Ok(placed) => pending.placed.push((key, placed)),
            Err(error) => debug!("Skipping placed feature {key}: {error}"),
        },
    );
    visit_json(
        &ns_path.join("neoforge").join("biome_modifier"),
        namespace,
        "",
        &mut |key, json| match parse_modifier(&json) {
            Ok(Some(modifier)) => pending.modifiers.push((key, modifier)),
            Ok(None) => {}
            Err(error) => warn!("Invalid biome modifier {key}: {error}"),
        },
    );
}

/// Registers what all namespaces defined. Call once after every pack was read.
pub fn finish() {
    let pending = std::mem::take(&mut *PENDING.lock().unwrap_or_else(PoisonError::into_inner));
    for (key, ore) in pending.configured {
        if let Err(error) =
            additive::register_feature(&key, FeatureKind::Ore(ore), Source::Datapack)
        {
            warn!("Datapack feature {key}: {error}");
        }
    }
    for (modifier_key, modifier) in pending.modifiers {
        match modifier {
            Modifier::Add {
                biomes,
                features,
                step,
            } => add_features(&modifier_key, &pending.placed, &biomes, &features, step),
            Modifier::Remove { biomes, features } => {
                remove_features(&modifier_key, &biomes, &features);
            }
        }
    }
}

fn add_features(
    modifier_key: &str,
    placed: &[(String, PlacedDef)],
    biomes: &[String],
    features: &[String],
    step: usize,
) {
    let biomes = match additive::resolve_biomes(biomes) {
        Ok(biomes) => biomes,
        Err(error) => {
            warn!("Biome modifier {modifier_key}: {error}");
            return;
        }
    };
    for feature in features {
        let Some((_, def)) = placed.iter().rev().find(|(key, _)| key == feature) else {
            warn!(
                "Biome modifier {modifier_key}: placed feature {feature} is not a supported datapack feature"
            );
            continue;
        };
        if additive::feature_kind(&def.feature).is_none() {
            warn!(
                "Biome modifier {modifier_key}: feature {} of {feature} is not registered",
                def.feature
            );
            continue;
        }
        let rule = AdditiveRule {
            feature: def.feature.clone(),
            biomes: biomes.clone(),
            step,
            count: def.count.clone(),
            rarity: def.rarity,
            height: def.height,
            in_square: def.in_square,
        };
        if let Err(error) = additive::add_rule(rule, Source::Datapack) {
            warn!("Biome modifier {modifier_key}: {error}");
        }
    }
}

fn remove_features(modifier_key: &str, biomes: &[String], features: &[String]) {
    let biomes = match additive::resolve_biomes(biomes) {
        Ok(biomes) => biomes,
        Err(error) => {
            warn!("Biome modifier {modifier_key}: {error}");
            return;
        }
    };
    let features: Vec<PlacedFeature> = features
        .iter()
        .filter_map(|feature| {
            let found = PlacedFeature::from_name(feature);
            if found.is_none() {
                warn!("Biome modifier {modifier_key}: {feature} is not a vanilla placed feature");
            }
            found
        })
        .collect();
    if !features.is_empty() {
        additive::add_removal(biomes, features);
    }
}

fn visit_json(dir: &Path, namespace: &str, prefix: &str, f: &mut dyn FnMut(String, Value)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.is_dir() {
            let next = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            visit_json(&path, namespace, &next, f);
            continue;
        }
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        let key = if prefix.is_empty() {
            format!("{namespace}:{stem}")
        } else {
            format!("{namespace}:{prefix}/{stem}")
        };
        match fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()))
        {
            Ok(json) => f(key, json),
            Err(error) => warn!("Failed to read {}: {error}", path.display()),
        }
    }
}

fn block_name(state: &Value) -> Result<String, String> {
    state
        .get("Name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "ore state has no Name".to_string())
}

/// `Ok(None)` for a configured feature that is not an ore.
fn parse_configured(json: &Value) -> Result<Option<OreSpec>, String> {
    let kind = json.get("type").and_then(Value::as_str).unwrap_or_default();
    if kind != "minecraft:ore" && kind != "ore" {
        return Ok(None);
    }
    let config = json.get("config").ok_or("no config")?;
    let size = config
        .get("size")
        .and_then(Value::as_i64)
        .and_then(|size| i32::try_from(size).ok())
        .ok_or("no valid size")?;
    let discard = config
        .get("discard_chance_on_air_exposure")
        .and_then(Value::as_f64)
        .unwrap_or(0.0) as f32;
    let mut targets = Vec::new();
    for target in config
        .get("targets")
        .and_then(Value::as_array)
        .ok_or("no targets")?
    {
        let state = block_name(target.get("state").ok_or("target has no state")?)?;
        let predicate = target.get("target").ok_or("target has no predicate")?;
        let predicate_type = predicate
            .get("predicate_type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let matcher = match predicate_type {
            "minecraft:tag_match" | "tag_match" => predicate
                .get("tag")
                .and_then(Value::as_str)
                .map(|tag| format!("#{tag}")),
            "minecraft:block_match" | "block_match" => predicate
                .get("block")
                .and_then(Value::as_str)
                .map(str::to_string),
            other => return Err(format!("unsupported target predicate '{other}'")),
        }
        .ok_or("target predicate is incomplete")?;
        targets.push(OreTargetSpec {
            target: matcher,
            state,
        });
    }
    Ok(Some(OreSpec {
        size,
        discard_chance_on_air_exposure: discard,
        targets,
    }))
}

fn parse_height(json: &Value) -> Result<HeightSpec, String> {
    let anchor = |value: Option<&Value>| -> Result<YOffset, String> {
        serde_json::from_value(value.cloned().ok_or("height anchor missing")?)
            .map_err(|error| format!("invalid height anchor: {error}"))
    };
    // A bare anchor is a constant height.
    if json.get("type").is_none() {
        let anchor = anchor(Some(json))?;
        return Ok(HeightSpec {
            min: anchor,
            max: anchor,
            shape: HeightShape::Uniform,
        });
    }
    let kind = json.get("type").and_then(Value::as_str).unwrap_or_default();
    let shape = match kind {
        "minecraft:uniform" | "uniform" => HeightShape::Uniform,
        "minecraft:trapezoid" | "trapezoid" => {
            if json.get("plateau").and_then(Value::as_i64).unwrap_or(0) != 0 {
                return Err("trapezoid height with a plateau is not supported".to_string());
            }
            HeightShape::Triangle
        }
        "minecraft:constant" | "constant" => {
            let value = anchor(json.get("value"))?;
            return Ok(HeightSpec {
                min: value,
                max: value,
                shape: HeightShape::Uniform,
            });
        }
        other => return Err(format!("unsupported height provider '{other}'")),
    };
    Ok(HeightSpec {
        min: anchor(json.get("min_inclusive"))?,
        max: anchor(json.get("max_inclusive"))?,
        shape,
    })
}

fn parse_placed(
    key: &str,
    json: &Value,
    configured: &mut Vec<(String, OreSpec)>,
) -> Result<PlacedDef, String> {
    let feature = match json.get("feature").ok_or("no feature")? {
        Value::String(name) => name.clone(),
        inline @ Value::Object(_) => {
            let ore = parse_configured(inline)?.ok_or("inline feature is not an ore")?;
            let name = format!("{key}/inline");
            configured.push((name.clone(), ore));
            name
        }
        _ => return Err("feature is neither a name nor an object".to_string()),
    };
    let mut count = IntProvider::Constant(1);
    let mut rarity = None;
    let mut in_square = false;
    let mut height = None;
    for modifier in json
        .get("placement")
        .and_then(Value::as_array)
        .ok_or("no placement")?
    {
        let kind = modifier
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind.strip_prefix("minecraft:").unwrap_or(kind) {
            "count" => {
                count = serde_json::from_value(modifier.get("count").cloned().unwrap_or_default())
                    .map_err(|error| format!("invalid count: {error}"))?;
            }
            "rarity_filter" => {
                rarity = modifier
                    .get("chance")
                    .and_then(Value::as_u64)
                    .and_then(|chance| u32::try_from(chance).ok());
                if rarity.is_none() {
                    return Err("invalid rarity chance".to_string());
                }
            }
            "in_square" => in_square = true,
            "height_range" => {
                height = Some(parse_height(modifier.get("height").ok_or("no height")?)?);
            }
            // The rule's biomes already filter by biome.
            "biome" => {}
            other => return Err(format!("unsupported placement modifier '{other}'")),
        }
    }
    Ok(PlacedDef {
        feature,
        count,
        rarity,
        in_square,
        height: height.ok_or("no height_range modifier")?,
    })
}

/// A holder set: one id, a `#tag` or a list of ids.
fn holder_set(value: Option<&Value>) -> Result<Vec<String>, String> {
    match value.ok_or("missing holder set")? {
        Value::String(single) => Ok(vec![single.clone()]),
        Value::Array(list) => list
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "holder set entry is not a string".to_string())
            })
            .collect(),
        _ => Err("invalid holder set".to_string()),
    }
}

fn parse_modifier(json: &Value) -> Result<Option<Modifier>, String> {
    let kind = json.get("type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "neoforge:add_features" => {
            let step_name = json.get("step").and_then(Value::as_str).ok_or("no step")?;
            let step = STEPS
                .iter()
                .position(|name| *name == step_name)
                .ok_or_else(|| format!("unknown step '{step_name}'"))?;
            Ok(Some(Modifier::Add {
                biomes: holder_set(json.get("biomes"))?,
                features: holder_set(json.get("features"))?,
                step,
            }))
        }
        "neoforge:remove_features" => Ok(Some(Modifier::Remove {
            biomes: holder_set(json.get("biomes"))?,
            features: holder_set(json.get("features"))?,
        })),
        // Other biome modifier types (spawns, climate, ...) are not supported.
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ore_placed_feature_and_modifier() {
        let configured = serde_json::json!({
            "type": "minecraft:ore",
            "config": {
                "size": 9,
                "discard_chance_on_air_exposure": 0.0,
                "targets": [{
                    "target": {"predicate_type": "minecraft:tag_match", "tag": "minecraft:stone_ore_replaceables"},
                    "state": {"Name": "create:zinc_ore"}
                }]
            }
        });
        let ore = parse_configured(&configured).unwrap().unwrap();
        assert_eq!(ore.size, 9);
        assert_eq!(ore.targets[0].target, "#minecraft:stone_ore_replaceables");
        assert_eq!(ore.targets[0].state, "create:zinc_ore");

        let placed = serde_json::json!({
            "feature": "create:zinc_ore",
            "placement": [
                {"type": "minecraft:count", "count": 4},
                {"type": "minecraft:in_square"},
                {"type": "minecraft:height_range", "height": {
                    "type": "minecraft:trapezoid",
                    "min_inclusive": {"absolute": -63},
                    "max_inclusive": {"absolute": 70}
                }},
                {"type": "minecraft:biome"}
            ]
        });
        let mut inline = Vec::new();
        let def = parse_placed("create:zinc_ore_placed", &placed, &mut inline).unwrap();
        assert_eq!(def.feature, "create:zinc_ore");
        assert!(def.in_square);
        assert_eq!(def.height.shape, HeightShape::Triangle);

        let unsupported = serde_json::json!({
            "feature": "x:y",
            "placement": [{"type": "minecraft:heightmap", "heightmap": "WORLD_SURFACE"}]
        });
        assert!(parse_placed("x:p", &unsupported, &mut inline).is_err());

        let modifier = serde_json::json!({
            "type": "neoforge:add_features",
            "biomes": "#minecraft:is_overworld",
            "features": "create:zinc_ore_placed",
            "step": "underground_ores"
        });
        match parse_modifier(&modifier).unwrap().unwrap() {
            Modifier::Add { step, biomes, .. } => {
                assert_eq!(step, 6);
                assert_eq!(biomes, vec!["#minecraft:is_overworld".to_string()]);
            }
            Modifier::Remove { .. } => panic!("expected an add modifier"),
        }
    }
}
