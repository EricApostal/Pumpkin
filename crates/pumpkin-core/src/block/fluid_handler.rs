//! A view of the fluid tanks of a block, for plugins that move fluids without buckets.
//!
//! Three kinds of blocks have a handler: plugin block entities with tanks (see
//! [`PluginTanks`]), the cauldrons, and every spot a fluid could be taken from or put into (a
//! fluid block, a waterlogged block, air or a replaceable block) as one tank of one bucket.

use std::sync::Arc;

use pumpkin_data::block_properties::WaterCauldronLikeProperties;
use pumpkin_data::fluid::Fluid;
use pumpkin_data::{Block, BlockDirection, BlockId, BlockStateId};
use pumpkin_util::math::position::BlockPos;
use pumpkin_world::world::BlockFlags;

use crate::block::entities::plugin_tank::{FluidContent, PluginTanks};
use crate::block::fluid::plugin_fluid::{try_pickup_fluid, try_place_fluid};
use crate::world::World;

/// Millibuckets in a full bucket.
pub const BUCKET_AMOUNT: u32 = 1000;
/// Millibuckets of one level of a water or powder snow cauldron, three levels are a bucket.
pub const CAULDRON_LEVEL_AMOUNT: u32 = 333;
/// Powder snow has no fluid in the registry, but cauldrons and buckets hold it.
pub const POWDER_SNOW_KEY: &str = "minecraft:powder_snow";
const WATER_KEY: &str = "minecraft:water";
const LAVA_KEY: &str = "minecraft:lava";

/// The full key of the still fluid for a key, `minecraft:` optional for vanilla fluids. `None`
/// for keys that name no fluid. Powder snow counts as a fluid here.
#[must_use]
pub fn resolve_fluid_key(key: &str) -> Option<String> {
    let bare = key.strip_prefix("minecraft:").unwrap_or(key);
    if bare == "powder_snow" {
        return Some(POWDER_SNOW_KEY.to_string());
    }
    let fluid = Fluid::from_registry_key(bare)?;
    (fluid.id != Fluid::EMPTY.id).then(|| fluid.to_still().resource_location().into_owned())
}

/// The three cauldron blocks that hold a fluid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CauldronKind {
    Water,
    Lava,
    PowderSnow,
}

impl CauldronKind {
    #[must_use]
    pub const fn fluid_key(self) -> &'static str {
        match self {
            Self::Water => WATER_KEY,
            Self::Lava => LAVA_KEY,
            Self::PowderSnow => POWDER_SNOW_KEY,
        }
    }

    #[must_use]
    pub fn from_fluid_key(key: &str) -> Option<Self> {
        match key {
            WATER_KEY => Some(Self::Water),
            LAVA_KEY => Some(Self::Lava),
            POWDER_SNOW_KEY => Some(Self::PowderSnow),
            _ => None,
        }
    }

    /// The highest level the cauldron has: lava is full or empty.
    #[must_use]
    pub const fn max_level(self) -> u8 {
        match self {
            Self::Lava => 1,
            _ => 3,
        }
    }

    /// Millibuckets in a cauldron of a level: a full one is a bucket, otherwise
    /// [`CAULDRON_LEVEL_AMOUNT`] per level.
    #[must_use]
    pub const fn amount(self, level: u8) -> u32 {
        if level >= self.max_level() {
            BUCKET_AMOUNT
        } else {
            level as u32 * CAULDRON_LEVEL_AMOUNT
        }
    }

    /// The level a cauldron of `level` reaches when `amount` is poured in, never more than the
    /// amount allows.
    #[must_use]
    pub fn level_after_fill(self, level: u8, amount: u32) -> u8 {
        let total = self.amount(level).saturating_add(amount);
        (level..=self.max_level())
            .rev()
            .find(|l| self.amount(*l) <= total)
            .unwrap_or(level)
    }

    /// The level a cauldron of `level` drops to when at most `max` is taken out, never more than
    /// `max` is taken.
    #[must_use]
    pub fn level_after_drain(self, level: u8, max: u32) -> u8 {
        let floor = self.amount(level).saturating_sub(max);
        (0..=level)
            .find(|l| self.amount(*l) >= floor)
            .unwrap_or(level)
    }

    fn state_id(self, level: u8) -> BlockStateId {
        match (self, level) {
            (_, 0) => Block::CAULDRON.default_state.id,
            (Self::Lava, _) => Block::LAVA_CAULDRON.default_state.id,
            (Self::Water, level) => Block::WATER_CAULDRON
                .from_properties(&[("level", &level.to_string())])
                .to_state_id(&Block::WATER_CAULDRON),
            (Self::PowderSnow, level) => Block::POWDER_SNOW_CAULDRON
                .from_properties(&[("level", &level.to_string())])
                .to_state_id(&Block::POWDER_SNOW_CAULDRON),
        }
    }
}

/// What the cauldron at a position holds: the kind and level, `None` kind if empty.
fn cauldron_at(world: &World, pos: &BlockPos) -> Option<(Option<CauldronKind>, u8)> {
    let (block, state) = world.get_block_and_state_id(pos);
    match block.id {
        BlockId::CAULDRON => Some((None, 0)),
        BlockId::LAVA_CAULDRON => Some((Some(CauldronKind::Lava), 1)),
        BlockId::WATER_CAULDRON => Some((
            Some(CauldronKind::Water),
            WaterCauldronLikeProperties::from_state_id(state).level,
        )),
        BlockId::POWDER_SNOW_CAULDRON => Some((
            Some(CauldronKind::PowderSnow),
            WaterCauldronLikeProperties::from_state_id(state).level,
        )),
        _ => None,
    }
}

/// The source fluid at a position that a bucket would pick up, without picking it up.
fn source_fluid_at(world: &World, pos: &BlockPos) -> Option<&'static Fluid> {
    let state_id = world.get_block_state_id(pos);
    if let Some((fluid, state)) = Fluid::dynamic_fluid_state(state_id)
        && Fluid::from_state_id(state_id).is_some_and(Fluid::is_dynamic)
    {
        return state.is_source.then(|| fluid.to_still());
    }
    let (fluid, state) = world.get_fluid_and_fluid_state(pos);
    if state.is_empty || !state.is_source {
        return None;
    }
    if fluid.matches_type(&Fluid::WATER) {
        Some(&Fluid::WATER)
    } else if fluid.matches_type(&Fluid::LAVA) {
        Some(&Fluid::LAVA)
    } else {
        None
    }
}

/// Whether [`try_place_fluid`] would place a source of the fluid, without placing it.
fn can_place_fluid(world: &World, pos: &BlockPos, fluid: &Fluid) -> bool {
    let fluid = fluid.to_still();
    let (block, state) = world.get_block_and_state(pos);
    let source = if fluid.matches_type(&Fluid::WATER) {
        if block.is_waterloggable() && !block.is_waterlogged(state.id) {
            return true;
        }
        Block::WATER.default_state.id
    } else if fluid.matches_type(&Fluid::LAVA) {
        Block::LAVA.default_state.id
    } else {
        let Some(info) = fluid.dynamic_info() else {
            return false;
        };
        let Some(block) = info.block.filter(|_| !info.holds_flowing()) else {
            return false;
        };
        block.default_state.id
    };
    (state.is_air() || state.is_liquid() || state.replaceable()) && state.id != source
}

/// The fluid tanks of one block.
pub enum FluidHandler {
    Tanks(Arc<PluginTanks>),
    Cauldron { world: Arc<World>, pos: BlockPos },
    /// One tank of a bucket for the spot itself: a source in the world.
    Block { world: Arc<World>, pos: BlockPos },
}

impl FluidHandler {
    /// The handler at a position. `None` if the chunk is not loaded or the block holds no fluid
    /// and takes none (a solid block). The side does not change what the handlers see.
    #[must_use]
    pub fn at(world: &Arc<World>, pos: &BlockPos, _side: Option<BlockDirection>) -> Option<Self> {
        if !world.is_loaded(pos) {
            return None;
        }
        if let Some(tanks) = world
            .with_plugin_block_entity(pos, |entity| entity.plugin_tanks())
            .flatten()
        {
            return Some(Self::Tanks(tanks));
        }
        if cauldron_at(world, pos).is_some() {
            return Some(Self::Cauldron {
                world: world.clone(),
                pos: *pos,
            });
        }
        let (block, state) = world.get_block_and_state(pos);
        (state.is_air()
            || state.is_liquid()
            || state.replaceable()
            || block.is_waterloggable()
            || source_fluid_at(world, pos).is_some())
        .then(|| Self::Block {
            world: world.clone(),
            pos: *pos,
        })
    }

    #[must_use]
    pub fn tank_count(&self) -> usize {
        match self {
            Self::Tanks(tanks) => tanks.tank_count(),
            Self::Cauldron { .. } | Self::Block { .. } => 1,
        }
    }

    /// A copy of what a tank holds, none if it is empty or does not exist.
    #[must_use]
    pub fn get_fluid(&self, tank: usize) -> Option<FluidContent> {
        match self {
            Self::Tanks(tanks) => tanks.get(tank),
            Self::Cauldron { world, pos } if tank == 0 => {
                let (kind, level) = cauldron_at(world, pos)?;
                let kind = kind?;
                Some(FluidContent {
                    fluid: kind.fluid_key().to_string(),
                    amount: kind.amount(level),
                    data: None,
                })
            }
            Self::Block { world, pos } if tank == 0 => {
                let fluid = source_fluid_at(world, pos)?;
                Some(FluidContent {
                    fluid: fluid.resource_location().into_owned(),
                    amount: BUCKET_AMOUNT,
                    data: None,
                })
            }
            _ => None,
        }
    }

    /// Most millibuckets a tank holds, 0 if it does not exist.
    #[must_use]
    pub fn get_tank_capacity(&self, tank: usize) -> u32 {
        match self {
            Self::Tanks(tanks) if tank < tanks.tank_count() => tanks.capacity(),
            Self::Cauldron { .. } | Self::Block { .. } if tank == 0 => BUCKET_AMOUNT,
            _ => 0,
        }
    }

    /// Whether the fluid may be put into the tank, no matter what it holds.
    #[must_use]
    pub fn is_fluid_valid(&self, tank: usize, fluid_key: &str) -> bool {
        let Some(key) = resolve_fluid_key(fluid_key) else {
            return false;
        };
        match self {
            Self::Tanks(tanks) => tank < tanks.tank_count(),
            Self::Cauldron { .. } => tank == 0 && CauldronKind::from_fluid_key(&key).is_some(),
            Self::Block { world, pos } => {
                tank == 0
                    && Fluid::from_registry_key(key.strip_prefix("minecraft:").unwrap_or(&key))
                        .is_some_and(|fluid| can_place_fluid(world, pos, fluid))
            }
        }
    }

    /// Puts as much of the fluid in as fits and returns the amount that went in. With `simulate`
    /// nothing changes. A spot in the world takes a bucket at once or nothing.
    #[must_use]
    pub fn fill(&self, stack: &FluidContent, simulate: bool) -> u32 {
        let Some(fluid) = resolve_fluid_key(&stack.fluid) else {
            return 0;
        };
        let stack = FluidContent {
            fluid,
            ..stack.clone()
        };
        match self {
            Self::Tanks(tanks) => tanks.fill(&stack, simulate),
            Self::Cauldron { world, pos } => Self::fill_cauldron(world, pos, &stack, simulate),
            Self::Block { world, pos } => {
                if stack.amount < BUCKET_AMOUNT || stack.data.is_some() {
                    return 0;
                }
                let Some(fluid) =
                    Fluid::from_registry_key(stack.fluid.strip_prefix("minecraft:").unwrap_or(&stack.fluid))
                else {
                    return 0;
                };
                let placed = if simulate {
                    can_place_fluid(world, pos, fluid)
                } else {
                    try_place_fluid(world, *pos, fluid)
                };
                if placed { BUCKET_AMOUNT } else { 0 }
            }
        }
    }

    fn fill_cauldron(world: &Arc<World>, pos: &BlockPos, stack: &FluidContent, simulate: bool) -> u32 {
        let Some(kind) = CauldronKind::from_fluid_key(&stack.fluid).filter(|_| stack.data.is_none())
        else {
            return 0;
        };
        let Some((held, level)) = cauldron_at(world, pos) else {
            return 0;
        };
        if held.is_some_and(|held| held != kind) {
            return 0;
        }
        let level = if held.is_some() { level } else { 0 };
        let new_level = kind.level_after_fill(level, stack.amount);
        if new_level <= level {
            return 0;
        }
        if !simulate {
            world.set_block_state(pos, kind.state_id(new_level), BlockFlags::NOTIFY_ALL);
        }
        kind.amount(new_level) - kind.amount(level)
    }

    /// Takes up to `max_amount` of the fluid in the first tank that holds one, with `simulate`
    /// nothing changes.
    #[must_use]
    pub fn drain(&self, max_amount: u32, simulate: bool) -> Option<FluidContent> {
        self.drain_matching(None, max_amount, simulate)
    }

    /// Takes up to `max_amount` of one fluid, with `simulate` nothing changes.
    #[must_use]
    pub fn drain_fluid(
        &self,
        fluid_key: &str,
        max_amount: u32,
        simulate: bool,
    ) -> Option<FluidContent> {
        let key = resolve_fluid_key(fluid_key)?;
        self.drain_matching(Some(&key), max_amount, simulate)
    }

    fn drain_matching(&self, only: Option<&str>, max: u32, simulate: bool) -> Option<FluidContent> {
        match self {
            Self::Tanks(tanks) => tanks.drain(max, only, simulate),
            Self::Cauldron { world, pos } => {
                let (kind, level) = cauldron_at(world, pos)?;
                let kind = kind.filter(|kind| only.is_none_or(|key| kind.fluid_key() == key))?;
                let new_level = kind.level_after_drain(level, max);
                if new_level >= level {
                    return None;
                }
                if !simulate {
                    world.set_block_state(pos, kind.state_id(new_level), BlockFlags::NOTIFY_ALL);
                }
                Some(FluidContent {
                    fluid: kind.fluid_key().to_string(),
                    amount: kind.amount(level) - kind.amount(new_level),
                    data: None,
                })
            }
            Self::Block { world, pos } => {
                if max < BUCKET_AMOUNT {
                    return None;
                }
                let fluid = source_fluid_at(world, pos)?;
                let key = fluid.resource_location().into_owned();
                if only.is_some_and(|only| only != key) {
                    return None;
                }
                if !simulate && try_pickup_fluid(world, *pos).is_none() {
                    return None;
                }
                Some(FluidContent {
                    fluid: key,
                    amount: BUCKET_AMOUNT,
                    data: None,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cauldron_amounts() {
        assert_eq!(CauldronKind::Water.amount(0), 0);
        assert_eq!(CauldronKind::Water.amount(1), 333);
        assert_eq!(CauldronKind::Water.amount(2), 666);
        assert_eq!(CauldronKind::Water.amount(3), 1000);
        assert_eq!(CauldronKind::Lava.amount(1), 1000);
    }

    #[test]
    fn cauldron_fill_levels() {
        let water = CauldronKind::Water;
        assert_eq!(water.level_after_fill(0, 332), 0);
        assert_eq!(water.level_after_fill(0, 333), 1);
        assert_eq!(water.level_after_fill(0, 700), 2);
        assert_eq!(water.level_after_fill(1, 667), 3);
        assert_eq!(water.level_after_fill(3, 1000), 3);
        assert_eq!(CauldronKind::Lava.level_after_fill(0, 999), 0);
        assert_eq!(CauldronKind::Lava.level_after_fill(0, 1000), 1);
    }

    #[test]
    fn cauldron_drain_levels_never_exceed_max() {
        let water = CauldronKind::Water;
        // 1000 minus 100 is 900, the next level down is 666, 334 would be taken: stay full.
        assert_eq!(water.level_after_drain(3, 100), 3);
        assert_eq!(water.level_after_drain(3, 334), 2);
        assert_eq!(water.level_after_drain(3, 1000), 0);
        assert_eq!(water.level_after_drain(2, 333), 1);
        assert_eq!(CauldronKind::Lava.level_after_drain(1, 999), 1);
        assert_eq!(CauldronKind::Lava.level_after_drain(1, 1000), 0);
    }

    #[test]
    fn fluid_keys() {
        assert_eq!(resolve_fluid_key("water").as_deref(), Some("minecraft:water"));
        assert_eq!(
            resolve_fluid_key("minecraft:flowing_lava").as_deref(),
            Some("minecraft:lava")
        );
        assert_eq!(
            resolve_fluid_key("powder_snow").as_deref(),
            Some(POWDER_SNOW_KEY)
        );
        assert_eq!(resolve_fluid_key("minecraft:empty"), None);
        assert_eq!(resolve_fluid_key("nothing:here"), None);
    }
}
