//! Additive world generation for plugins, see `worldgen.wit`.

use pumpkin_core::plugin::permissions;
use pumpkin_util::math::int_provider::IntProvider;
use pumpkin_wasm_host_common::state::PluginHostState;
use pumpkin_world::generation::feature::additive::{
    self, AdditiveRule, FeatureKind, HeightShape, HeightSpec, OreSpec, OreTargetSpec, Source,
};
use tracing::info;

use crate::pumpkin::plugin::worldgen::{
    FeatureKind as WitFeatureKind, GenerationStep, HeightShape as WitHeightShape, Host,
    OreTarget as WitOreTarget, PlacementRule as WitPlacementRule,
};

const fn step_index(step: GenerationStep) -> usize {
    match step {
        GenerationStep::RawGeneration => 0,
        GenerationStep::Lakes => 1,
        GenerationStep::LocalModifications => 2,
        GenerationStep::UndergroundStructures => 3,
        GenerationStep::SurfaceStructures => 4,
        GenerationStep::StrongestStronghold => 5,
        GenerationStep::UndergroundOres => 6,
        GenerationStep::UndergroundDecoration => 7,
        GenerationStep::FluidSprings => 8,
        GenerationStep::VegetalDecoration => 9,
        GenerationStep::TopLayerModification => 10,
    }
}

/// `GenerationStep` from the step index of the generator.
#[must_use]
pub const fn step_from_index(step: u8) -> GenerationStep {
    match step {
        0 => GenerationStep::RawGeneration,
        1 => GenerationStep::Lakes,
        2 => GenerationStep::LocalModifications,
        3 => GenerationStep::UndergroundStructures,
        4 => GenerationStep::SurfaceStructures,
        5 => GenerationStep::StrongestStronghold,
        6 => GenerationStep::UndergroundOres,
        7 => GenerationStep::UndergroundDecoration,
        8 => GenerationStep::FluidSprings,
        9 => GenerationStep::VegetalDecoration,
        _ => GenerationStep::TopLayerModification,
    }
}

fn check_worldgen_permission(state: &PluginHostState, what: &str) -> Result<(), String> {
    if state
        .permissions
        .iter()
        .any(|p| p == permissions::REGISTRY_WORLDGEN)
    {
        Ok(())
    } else {
        Err(state.missing_permission(what, permissions::REGISTRY_WORLDGEN))
    }
}

fn plugin_label(state: &PluginHostState) -> &str {
    state.name.as_deref().unwrap_or("<unknown>")
}

impl Host for PluginHostState {
    fn register_feature(
        &mut self,
        key: String,
        kind: WitFeatureKind,
    ) -> wasmtime::Result<Result<(), String>> {
        if let Err(error) = check_worldgen_permission(self, "registering features") {
            return Ok(Err(error));
        }
        let kind = match kind {
            WitFeatureKind::Plugin => FeatureKind::Plugin,
        };
        let result = additive::register_feature(&key, kind, Source::Plugin);
        if result.is_ok() {
            info!("Plugin {} registered feature {key}", plugin_label(self));
        }
        Ok(result)
    }

    fn register_ore_feature(
        &mut self,
        key: String,
        targets: Vec<WitOreTarget>,
        size: u32,
        discard_chance_on_air_exposure: f32,
    ) -> wasmtime::Result<Result<(), String>> {
        if let Err(error) = check_worldgen_permission(self, "registering features") {
            return Ok(Err(error));
        }
        let Ok(size) = i32::try_from(size) else {
            return Ok(Err("ore size is too large".to_string()));
        };
        let spec = OreSpec {
            size,
            discard_chance_on_air_exposure,
            targets: targets
                .into_iter()
                .map(|target| OreTargetSpec {
                    target: target.target,
                    state: target.state,
                })
                .collect(),
        };
        let result = additive::register_feature(&key, FeatureKind::Ore(spec), Source::Plugin);
        if result.is_ok() {
            info!("Plugin {} registered ore feature {key}", plugin_label(self));
        }
        Ok(result)
    }

    fn add_feature_to_biomes(
        &mut self,
        feature_key: String,
        rule: WitPlacementRule,
    ) -> wasmtime::Result<Result<(), String>> {
        if let Err(error) = check_worldgen_permission(self, "adding features to biomes") {
            return Ok(Err(error));
        }
        if additive::feature_kind(&feature_key).is_none() {
            return Ok(Err(format!("feature '{feature_key}' is not registered")));
        }
        let (Ok(count), Ok(min), Ok(max)) = (
            i32::try_from(rule.count),
            i16::try_from(rule.height.min),
            i16::try_from(rule.height.max),
        ) else {
            return Ok(Err("count or height out of range".to_string()));
        };
        let biomes = match additive::resolve_biomes(&rule.biomes) {
            Ok(biomes) => biomes,
            Err(error) => return Ok(Err(error)),
        };
        let shape = match rule.height.shape {
            WitHeightShape::Uniform => HeightShape::Uniform,
            WitHeightShape::Triangle => HeightShape::Triangle,
        };
        let result = additive::add_rule(
            AdditiveRule {
                feature: feature_key.clone(),
                biomes,
                step: step_index(rule.step),
                count: IntProvider::Constant(count),
                rarity: rule.rarity,
                height: HeightSpec::absolute(min, max, shape),
                in_square: rule.in_square,
            },
            Source::Plugin,
        );
        if result.is_ok() {
            info!(
                "Plugin {} added feature {feature_key} to biomes",
                plugin_label(self)
            );
        }
        Ok(result)
    }
}
