//! Features that plugins and datapacks add to existing biomes, like NeoForge `add_features`
//! biome modifiers.
//!
//! A feature is either a declarative ore (placed with the vanilla ore code) or a plugin feature
//! that a [`WorldPortalExt::place_plugin_feature`] callback places. A rule says where a feature
//! goes: biomes, generation step, count, rarity, height range and in-square. Rules run after the
//! vanilla features of their step, with the same modifier math as placed features
//! (`rarity_filter`, `count`, `in_square`, `height_range`, biome check), so vanilla generation is
//! untouched while the registry is empty.
//!
//! Chunk generation runs on worker threads, so registration is behind a lock that is only read
//! while generating.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::{cell::RefCell, iter};

use pumpkin_data::chunk::Biome;
use pumpkin_data::placed_feature::PlacedFeature;
use pumpkin_data::tag::{RegistryKey, get_tag_values};
use pumpkin_data::{Block, BlockState};
use pumpkin_util::math::int_provider::IntProvider;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::random::{
    RandomGenerator, RandomImpl, get_decorator_seed, worldgen_random::WorldgenRandom,
};
use pumpkin_util::y_offset::{Absolute, YOffset};
use tracing::warn;

use super::features::ore::{OreFeature, OreTarget};
use super::placed_features::{
    CountPlacementModifier, CountPlacementModifierBase, HeightRangePlacementModifier,
    SquarePlacementModifier, generate_with_modifiers,
};
use crate::generation::height_provider::{
    HeightProvider, TrapezoidHeightProvider, UniformHeightProvider,
};
use crate::generation::proto_chunk::GenerationCache;
use crate::generation::rule::RuleTest;
use crate::world::WorldPortalExt;

/// Index offset of additive rules in decorator seeds, far above any vanilla feature index.
const SEED_INDEX_BASE: u64 = 1 << 32;

/// Who registered a feature or rule. Datapack entries are dropped on every datapack reload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Plugin,
    Datapack,
}

/// What one ore target matches and places.
#[derive(Clone, Debug)]
pub struct OreTargetSpec {
    /// A block key or a `#tag`.
    pub target: String,
    /// The block key whose default state is placed.
    pub state: String,
}

/// A declarative ore feature, placed by the vanilla ore code.
#[derive(Clone, Debug)]
pub struct OreSpec {
    pub size: i32,
    pub discard_chance_on_air_exposure: f32,
    pub targets: Vec<OreTargetSpec>,
}

#[derive(Clone, Debug)]
pub enum FeatureKind {
    Ore(OreSpec),
    /// Placed by [`WorldPortalExt::place_plugin_feature`].
    Plugin,
}

/// How the height of a placement is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightShape {
    Uniform,
    /// Vanilla `trapezoid` without a plateau, a triangle.
    Triangle,
}

#[derive(Clone, Copy, Debug)]
pub struct HeightSpec {
    pub min: YOffset,
    pub max: YOffset,
    pub shape: HeightShape,
}

impl HeightSpec {
    #[must_use]
    pub const fn absolute(min: i16, max: i16, shape: HeightShape) -> Self {
        Self {
            min: YOffset::Absolute(Absolute { absolute: min }),
            max: YOffset::Absolute(Absolute { absolute: max }),
            shape,
        }
    }

    const fn provider(&self) -> HeightProvider {
        match self.shape {
            HeightShape::Uniform => HeightProvider::Uniform(UniformHeightProvider {
                min_inclusive: self.min,
                max_inclusive: self.max,
            }),
            HeightShape::Triangle => HeightProvider::Trapezoid(TrapezoidHeightProvider {
                min_inclusive: self.min,
                max_inclusive: self.max,
                plateau: Some(0),
            }),
        }
    }
}

/// Where and how often a feature is placed.
#[derive(Clone, Debug)]
pub struct AdditiveRule {
    pub feature: String,
    /// Ids of the biomes the rule applies to, see [`resolve_biomes`].
    pub biomes: Vec<u8>,
    /// Vanilla generation step, `0..11`, 6 is underground ores.
    pub step: usize,
    pub count: IntProvider,
    /// Placed with chance `1 / rarity` per chunk, `None` always.
    pub rarity: Option<u32>,
    pub height: HeightSpec,
    pub in_square: bool,
}

struct Entry<T> {
    value: T,
    source: Source,
}

struct ResolvedOre(Option<OreFeature>);

type FeatureEntry = Entry<(FeatureKind, Arc<OnceLock<ResolvedOre>>)>;

#[derive(Default)]
struct Registry {
    features: HashMap<String, FeatureEntry>,
    rules: Vec<Entry<AdditiveRule>>,
    removals: Vec<(Vec<u8>, Vec<PlacedFeature>)>,
}

static REGISTRY: RwLock<Option<Registry>> = RwLock::new(None);
/// Lets generation skip the lock while nothing is registered.
static ACTIVE: AtomicBool = AtomicBool::new(false);

fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut guard = REGISTRY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let result = f(guard.get_or_insert_with(Registry::default));
    if let Some(registry) = guard.as_ref() {
        ACTIVE.store(
            !registry.rules.is_empty() || !registry.removals.is_empty(),
            Ordering::Release,
        );
    }
    result
}

/// Registers a feature under `key`. Fails for an empty key, a duplicate or an invalid ore.
pub fn register_feature(key: &str, kind: FeatureKind, source: Source) -> Result<(), String> {
    if key.is_empty() {
        return Err("feature key is empty".to_string());
    }
    if let FeatureKind::Ore(ore) = &kind {
        if !(1..=64).contains(&ore.size) {
            return Err(format!("ore size {} is not in 1..=64", ore.size));
        }
        if ore.targets.is_empty() {
            return Err("ore feature has no targets".to_string());
        }
        if !(0.0..=1.0).contains(&ore.discard_chance_on_air_exposure) {
            return Err("discard chance is not in 0.0..=1.0".to_string());
        }
    }
    with_registry(|registry| {
        if registry.features.contains_key(key) {
            return Err(format!("feature '{key}' is already registered"));
        }
        registry.features.insert(
            key.to_string(),
            Entry {
                value: (kind, Arc::new(OnceLock::new())),
                source,
            },
        );
        Ok(())
    })
}

#[must_use]
pub fn feature_kind(key: &str) -> Option<FeatureKind> {
    let guard = REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .as_ref()?
        .features
        .get(key)
        .map(|entry| entry.value.0.clone())
}

/// Adds a placement rule. The feature should be registered, rules for unknown features are
/// skipped when generating.
pub fn add_rule(rule: AdditiveRule, source: Source) -> Result<(), String> {
    if rule.step >= 11 {
        return Err(format!("generation step {} is not in 0..11", rule.step));
    }
    if rule.biomes.is_empty() {
        return Err("biome filter matches no biome".to_string());
    }
    if rule.rarity == Some(0) {
        return Err("rarity must be at least 1".to_string());
    }
    with_registry(|registry| {
        registry.rules.push(Entry {
            value: rule,
            source,
        })
    });
    Ok(())
}

/// Stops the placed features from generating in the biomes, like `NeoForge` `remove_features`.
pub fn add_removal(biomes: Vec<u8>, features: Vec<PlacedFeature>) {
    with_registry(|registry| registry.removals.push((biomes, features)));
}

/// Drops everything the datapacks registered, before they load again.
pub fn clear_datapack_entries() {
    with_registry(|registry| {
        registry
            .features
            .retain(|_, entry| entry.source == Source::Plugin);
        registry
            .rules
            .retain(|entry| entry.source == Source::Plugin);
        // Removals only come from datapacks.
        registry.removals.clear();
    });
}

/// Whether a modifier removed the placed feature from the biome.
#[must_use]
pub fn is_removed(biome_id: u8, feature: PlacedFeature) -> bool {
    if !ACTIVE.load(Ordering::Acquire) {
        return false;
    }
    let guard = REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.as_ref().is_some_and(|registry| {
        registry
            .removals
            .iter()
            .any(|(biomes, features)| biomes.contains(&biome_id) && features.contains(&feature))
    })
}

/// Resolves biome keys (`namespace:path`, `minecraft:` optional) and `#tag`s to biome ids,
/// without duplicates. Only generated biomes and tags exist, unknown entries are an error.
pub fn resolve_biomes(entries: &[String]) -> Result<Vec<u8>, String> {
    let mut ids = Vec::new();
    for entry in entries {
        if let Some(tag) = entry.strip_prefix('#') {
            let values = get_tag_values(RegistryKey::WorldgenBiome, tag)
                .ok_or_else(|| format!("unknown biome tag '{entry}'"))?;
            for value in values {
                let name = value.strip_prefix("minecraft:").unwrap_or(value);
                if let Some(biome) = Biome::from_name(name)
                    && !ids.contains(&biome.id)
                {
                    ids.push(biome.id);
                }
            }
        } else {
            let name = entry.strip_prefix("minecraft:").unwrap_or(entry);
            let biome = Biome::from_name(name).ok_or_else(|| format!("unknown biome '{entry}'"))?;
            if !ids.contains(&biome.id) {
                ids.push(biome.id);
            }
        }
    }
    Ok(ids)
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Runs the modifiers of a rule (rarity, count, in-square, height range, biome check) from
/// `origin` and calls `place` for every position that passes, with the same traversal as vanilla
/// placed features. Returns whether any placement reported success.
#[expect(clippy::too_many_arguments)]
fn run_rule<T>(
    state: &mut T,
    rule: &AdditiveRule,
    random: &mut RandomGenerator,
    origin: BlockPos,
    min_y: i8,
    height: u16,
    biome_at: &dyn Fn(&T, BlockPos) -> u8,
    place: &dyn Fn(&mut T, &mut RandomGenerator, BlockPos) -> bool,
) -> bool {
    let count = CountPlacementModifier {
        count: rule.count.clone(),
    };
    let height_range = HeightRangePlacementModifier {
        height: rule.height.provider(),
    };
    // Modifier order: rarity, count, in_square, height_range, biome.
    let mut stages = Vec::with_capacity(5);
    if let Some(chance) = rule.rarity {
        stages.push(Stage::Rarity(chance));
    }
    stages.push(Stage::Count);
    if rule.in_square {
        stages.push(Stage::Square);
    }
    stages.push(Stage::Height);
    stages.push(Stage::Biome);

    generate_with_modifiers(
        state,
        0,
        stages.len(),
        random,
        origin,
        &|state, index, random, pos| -> Box<dyn Iterator<Item = BlockPos>> {
            match stages[index] {
                Stage::Rarity(chance) => {
                    if random.next_f32() < 1.0 / chance as f32 {
                        Box::new(iter::once(pos))
                    } else {
                        Box::new(iter::empty())
                    }
                }
                Stage::Count => count.get_positions(random, pos),
                Stage::Square => SquarePlacementModifier::get_positions(random, pos),
                Stage::Height => height_range.get_positions(min_y, height, random, pos),
                Stage::Biome => {
                    if rule.biomes.contains(&biome_at(state, pos)) {
                        Box::new(iter::once(pos))
                    } else {
                        Box::new(iter::empty())
                    }
                }
            }
        },
        &|state, random, pos| place(state, random, pos),
    )
}

#[derive(Clone, Copy)]
enum Stage {
    Rarity(u32),
    Count,
    Square,
    Height,
    Biome,
}

fn resolve_ore(spec: &OreSpec) -> ResolvedOre {
    let mut targets = Vec::with_capacity(spec.targets.len());
    for target in &spec.targets {
        let Some(state) = Block::from_name(&target.state).map(|block| block.default_state) else {
            warn!("Ore feature state block '{}' does not exist", target.state);
            return ResolvedOre(None);
        };
        let rule = if let Some(tag) = target.target.strip_prefix('#') {
            RuleTest::TagKey(tag.to_string())
        } else if let Some(block) = Block::from_name(&target.target) {
            RuleTest::BlockMatch(crate::generation::rule::block_match::BlockMatchRuleTest {
                block: block.id,
            })
        } else {
            warn!(
                "Ore feature target block '{}' does not exist",
                target.target
            );
            return ResolvedOre(None);
        };
        let state: &'static BlockState = state;
        targets.push(OreTarget {
            target: rule,
            state,
        });
    }
    ResolvedOre(Some(OreFeature {
        size: spec.size,
        discard_chance_on_air_exposure: spec.discard_chance_on_air_exposure,
        targets,
    }))
}

/// Runs the rules of `step` on the center chunk of the cache. Called after the vanilla features
/// of the step.
pub fn apply_step<T: GenerationCache>(
    cache: &mut T,
    block_registry: &dyn WorldPortalExt,
    step: usize,
    population_seed: u64,
    origin: BlockPos,
    min_y: i8,
    height: u16,
) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    // Snapshot under the lock, so the lock is not held while features generate or plugins run.
    let rules: Vec<(AdditiveRule, FeatureKind, Arc<OnceLock<ResolvedOre>>, u64)> = {
        let guard = REGISTRY
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(registry) = guard.as_ref() else {
            return;
        };
        let mut ordinals: HashMap<&str, u64> = HashMap::new();
        let mut selected = Vec::new();
        for entry in &registry.rules {
            let ordinal = ordinals.entry(entry.value.feature.as_str()).or_insert(0);
            let current = *ordinal;
            *ordinal += 1;
            if entry.value.step != step {
                continue;
            }
            let Some(feature) = registry.features.get(&entry.value.feature) else {
                continue;
            };
            // Stable under registration order: the key and the rule number of that key.
            let index = SEED_INDEX_BASE + (fnv1a(&entry.value.feature) % SEED_INDEX_BASE) + current;
            selected.push((
                entry.value.clone(),
                feature.value.0.clone(),
                Arc::clone(&feature.value.1),
                index,
            ));
        }
        selected
    };

    for (rule, kind, resolved, index) in rules {
        let seed = get_decorator_seed(population_seed, index, step as u64);
        let mut random = RandomGenerator::Worldgen(WorldgenRandom::from_seed(seed));
        let biome_at = |cache: &T, pos: BlockPos| {
            cache
                .get_biome_for_terrain_gen(pos.0.x, pos.0.y, pos.0.z)
                .id
        };
        match kind {
            FeatureKind::Ore(spec) => {
                let ore = resolved.get_or_init(|| resolve_ore(&spec));
                let Some(ore) = &ore.0 else {
                    continue;
                };
                run_rule(
                    cache,
                    &rule,
                    &mut random,
                    origin,
                    min_y,
                    height,
                    &biome_at,
                    &|cache, random, pos| ore.generate_at(cache, random, pos),
                );
            }
            FeatureKind::Plugin => {
                let positions = RefCell::new(Vec::new());
                run_rule(
                    cache,
                    &rule,
                    &mut random,
                    origin,
                    min_y,
                    height,
                    &biome_at,
                    &|_, _, pos| {
                        positions.borrow_mut().push(pos);
                        true
                    },
                );
                let positions = positions.into_inner();
                if !positions.is_empty() {
                    // The chunk has to carry the plugin's blocks before the next step reads it, so
                    // this waits for the handler; the host schedules the dispatch in the background
                    // class instead of letting it delay the tick (see `PluginFeaturePlaceEvent`).
                    block_registry.place_plugin_feature(
                        &rule.feature,
                        cache.get_center_chunk_mut(),
                        step,
                        seed,
                        &positions,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use pumpkin_util::random::xoroshiro128::Xoroshiro;

    use super::*;
    use pumpkin_util::math::vector3::Vector3;

    const fn pos(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos(Vector3::new(x, y, z))
    }

    fn rule(count: i32, rarity: Option<u32>, in_square: bool) -> AdditiveRule {
        AdditiveRule {
            feature: "test:zinc".to_string(),
            biomes: vec![Biome::PLAINS.id],
            step: 6,
            count: IntProvider::Constant(count),
            rarity,
            height: HeightSpec::absolute(-16, 48, HeightShape::Triangle),
            in_square,
        }
    }

    fn run(rule: &AdditiveRule, seed: u64, biome: u8) -> Vec<BlockPos> {
        let mut placed: Vec<BlockPos> = Vec::new();
        let mut random = RandomGenerator::Xoroshiro(Xoroshiro::from_seed(seed));
        run_rule(
            &mut placed,
            rule,
            &mut random,
            pos(32, -64, -48),
            -64,
            384,
            &|_, _| biome,
            &|placed, _, pos| {
                placed.push(pos);
                true
            },
        );
        placed
    }

    #[test]
    fn count_in_square_and_height_stay_in_bounds() {
        let rule = rule(4, None, true);
        for seed in 0..64 {
            let placed = run(&rule, seed, Biome::PLAINS.id);
            assert_eq!(placed.len(), 4);
            for pos in placed {
                assert!((32..48).contains(&pos.0.x), "x {}", pos.0.x);
                assert!((-48..-32).contains(&pos.0.z), "z {}", pos.0.z);
                assert!((-16..=48).contains(&pos.0.y), "y {}", pos.0.y);
            }
        }
    }

    #[test]
    fn without_in_square_positions_stay_at_origin_column() {
        let rule = rule(3, None, false);
        let placed = run(&rule, 7, Biome::PLAINS.id);
        assert_eq!(placed.len(), 3);
        assert!(placed.iter().all(|pos| pos.0.x == 32 && pos.0.z == -48));
    }

    #[test]
    fn biome_filter_rejects_other_biomes() {
        let rule = rule(4, None, true);
        assert!(run(&rule, 1, Biome::DESERT.id).is_empty());
        assert_eq!(run(&rule, 1, Biome::PLAINS.id).len(), 4);
    }

    #[test]
    fn rarity_places_about_one_in_chance_chunks() {
        let rule = rule(1, Some(4), true);
        let hits = (0..4000)
            .filter(|seed| !run(&rule, *seed, Biome::PLAINS.id).is_empty())
            .count();
        // Expected 1000, six standard deviations are about 160.
        assert!((840..1160).contains(&hits), "hits {hits}");
    }

    #[test]
    fn triangle_height_clusters_around_the_middle() {
        let rule = rule(1, None, true);
        let mut near_middle = 0;
        let total = 2000;
        for seed in 0..total {
            let y = run(&rule, seed, Biome::PLAINS.id)[0].0.y;
            if (0..=32).contains(&y) {
                near_middle += 1;
            }
        }
        // A triangle over -16..=48 puts 75 percent of the mass in the middle half.
        assert!(near_middle > total * 65 / 100, "near middle {near_middle}");
    }

    #[test]
    fn negative_count_places_nothing_and_rule_is_deterministic() {
        let negative = rule(-3, None, true);
        assert!(run(&negative, 3, Biome::PLAINS.id).is_empty());
        let rule = rule(2, None, true);
        assert_eq!(
            run(&rule, 99, Biome::PLAINS.id),
            run(&rule, 99, Biome::PLAINS.id)
        );
    }

    #[test]
    fn biome_resolution_handles_keys_and_tags() {
        let ids = resolve_biomes(&["minecraft:plains".to_string(), "desert".to_string()]).unwrap();
        assert_eq!(ids, vec![Biome::PLAINS.id, Biome::DESERT.id]);
        assert!(resolve_biomes(&["nope:biome".to_string()]).is_err());
        let overworld = resolve_biomes(&["#minecraft:is_overworld".to_string()]).unwrap();
        assert!(overworld.contains(&Biome::PLAINS.id));
        assert!(!overworld.contains(&Biome::NETHER_WASTES.id));
    }

    #[test]
    fn decorator_seeds_do_not_depend_on_registration_order() {
        assert_eq!(fnv1a("create:zinc_ore"), fnv1a("create:zinc_ore"));
        assert_ne!(
            fnv1a("create:zinc_ore"),
            fnv1a("create:striated_ores_overworld")
        );
    }
}
