//! The fluid tanks of a plugin block entity, see `PluginBlockEntity::register_tanks`.

use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;

/// Most tanks a plugin block entity can have.
pub const MAX_TANKS: usize = 256;
/// Key of the compound with the tanks in the saved plugin block entity.
pub const TANKS_KEY: &str = "PluginTanks";

/// An amount of a fluid. `fluid` is the full key of the still fluid, `data` is plugin defined and
/// part of what makes two fluids the same.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FluidContent {
    pub fluid: String,
    /// Millibuckets, a bucket holds 1000.
    pub amount: u32,
    pub data: Option<Vec<u8>>,
}

impl FluidContent {
    /// The same fluid with another amount.
    #[must_use]
    pub fn with_amount(&self, amount: u32) -> Self {
        Self {
            fluid: self.fluid.clone(),
            amount,
            data: self.data.clone(),
        }
    }

    #[must_use]
    pub fn write_nbt(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        nbt.put_string("Fluid", self.fluid.clone());
        nbt.put_int("Amount", i32::try_from(self.amount).unwrap_or(i32::MAX));
        if let Some(data) = &self.data {
            nbt.put(
                "Data",
                NbtTag::ByteArray(data.iter().map(|b| *b as i8).collect()),
            );
        }
        nbt
    }

    #[must_use]
    pub fn read_nbt(nbt: &NbtCompound) -> Option<Self> {
        let amount = u32::try_from(nbt.get_int("Amount")?).ok().filter(|a| *a > 0)?;
        Some(Self {
            fluid: nbt.get_string("Fluid")?.to_string(),
            amount,
            data: nbt
                .get_byte_array("Data")
                .map(|data| data.iter().map(|b| *b as u8).collect()),
        })
    }
}

/// Puts as much of `stack` as fits into the tanks, on tanks that already hold the fluid first and
/// then into empty ones. Returns the amount that went in. With `simulate` nothing changes.
pub fn fill_tanks(
    tanks: &mut [Option<FluidContent>],
    capacity: u32,
    stack: &FluidContent,
    simulate: bool,
) -> u32 {
    let mut left = stack.amount;
    for merge in [true, false] {
        for tank in tanks.iter_mut() {
            if left == 0 {
                break;
            }
            match tank {
                Some(held)
                    if merge && held.fluid == stack.fluid && held.data == stack.data =>
                {
                    let moved = capacity.saturating_sub(held.amount).min(left);
                    if !simulate {
                        held.amount += moved;
                    }
                    left -= moved;
                }
                None if !merge => {
                    let moved = capacity.min(left);
                    if moved > 0 {
                        if !simulate {
                            *tank = Some(stack.with_amount(moved));
                        }
                        left -= moved;
                    }
                }
                _ => {}
            }
        }
    }
    stack.amount - left
}

/// Takes up to `max` of one fluid out of the tanks: `only` if given, otherwise the fluid of the
/// first tank that is not empty. Tanks with the same fluid and data add up. With `simulate`
/// nothing changes.
pub fn drain_tanks(
    tanks: &mut [Option<FluidContent>],
    max: u32,
    only: Option<&str>,
    simulate: bool,
) -> Option<FluidContent> {
    if max == 0 {
        return None;
    }
    let template = tanks
        .iter()
        .flatten()
        .find(|held| held.amount > 0 && only.is_none_or(|fluid| held.fluid == fluid))?
        .with_amount(0);
    let mut left = max;
    for tank in tanks.iter_mut() {
        if left == 0 {
            break;
        }
        let Some(held) = tank else { continue };
        if held.fluid != template.fluid || held.data != template.data {
            continue;
        }
        let moved = held.amount.min(left);
        if !simulate {
            held.amount -= moved;
            if held.amount == 0 {
                *tank = None;
            }
        }
        left -= moved;
    }
    Some(template.with_amount(max - left))
}

/// A list of tanks that live in a plugin block entity, all with the same capacity.
pub struct PluginTanks {
    tanks: RwLock<Vec<Option<FluidContent>>>,
    capacity: AtomicU32,
    dirty: AtomicBool,
}

impl PluginTanks {
    #[must_use]
    pub fn new(tank_count: usize, capacity: u32) -> Self {
        Self {
            tanks: RwLock::new(vec![None; tank_count]),
            capacity: AtomicU32::new(capacity),
            dirty: AtomicBool::new(false),
        }
    }

    /// Reads the form written by [`Self::write_nbt`].
    #[must_use]
    pub fn from_nbt(nbt: &NbtCompound) -> Option<Self> {
        let size = usize::try_from(nbt.get_int("Size")?).ok()?.min(MAX_TANKS);
        let capacity = u32::try_from(nbt.get_int("Capacity")?).ok()?;
        let tanks = Self::new(size, capacity);
        {
            let mut slots = tanks.write();
            for tag in nbt.get_list("Tanks").unwrap_or_default() {
                let Some(compound) = tag.extract_compound() else {
                    continue;
                };
                let Some(index) = compound.get_int("Tank") else {
                    continue;
                };
                if let Some(target) = usize::try_from(index).ok().and_then(|i| slots.get_mut(i))
                    && let Some(mut content) = FluidContent::read_nbt(compound)
                {
                    content.amount = content.amount.min(capacity);
                    *target = Some(content);
                }
            }
        }
        Some(tanks)
    }

    /// The saved form: `Size`, `Capacity` and the non empty tanks as `Tanks` with an int `Tank`.
    #[must_use]
    pub fn write_nbt(&self) -> NbtCompound {
        let tanks = self.read();
        let mut nbt = NbtCompound::new();
        nbt.put_int("Size", tanks.len() as i32);
        nbt.put_int(
            "Capacity",
            i32::try_from(self.capacity()).unwrap_or(i32::MAX),
        );
        let list: Vec<NbtTag> = tanks
            .iter()
            .enumerate()
            .filter_map(|(index, tank)| {
                let mut compound = tank.as_ref()?.write_nbt();
                compound.put_int("Tank", index as i32);
                Some(NbtTag::Compound(compound))
            })
            .collect();
        nbt.put_list("Tanks", list);
        nbt
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Vec<Option<FluidContent>>> {
        self.tanks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Vec<Option<FluidContent>>> {
        self.tanks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn tank_count(&self) -> usize {
        self.read().len()
    }

    /// Most millibuckets one tank takes.
    #[must_use]
    pub fn capacity(&self) -> u32 {
        self.capacity.load(Ordering::Relaxed)
    }

    /// A copy of what a tank holds, none if it is empty or does not exist.
    #[must_use]
    pub fn get(&self, tank: usize) -> Option<FluidContent> {
        self.read().get(tank).cloned().flatten()
    }

    /// Changes the tank count and capacity. Growing keeps the content. Shrinking fails, and
    /// changes nothing, if a tank that would go is not empty or a kept tank holds more than the
    /// new capacity.
    pub fn resize(&self, tank_count: usize, capacity: u32) -> Result<(), String> {
        let mut tanks = self.write();
        if tank_count < tanks.len() && tanks[tank_count..].iter().any(Option::is_some) {
            return Err(format!(
                "cannot shrink to {tank_count} tanks, a tank that would go is not empty"
            ));
        }
        if tanks
            .iter()
            .take(tank_count)
            .flatten()
            .any(|held| held.amount > capacity)
        {
            return Err(format!(
                "cannot shrink the capacity to {capacity}, a tank holds more"
            ));
        }
        tanks.resize(tank_count, None);
        self.capacity.store(capacity, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// See [`fill_tanks`].
    pub fn fill(&self, stack: &FluidContent, simulate: bool) -> u32 {
        let moved = fill_tanks(&mut self.write(), self.capacity(), stack, simulate);
        if moved > 0 && !simulate {
            self.dirty.store(true, Ordering::Relaxed);
        }
        moved
    }

    /// See [`drain_tanks`].
    pub fn drain(&self, max: u32, only: Option<&str>, simulate: bool) -> Option<FluidContent> {
        let drained = drain_tanks(&mut self.write(), max, only, simulate);
        if drained.as_ref().is_some_and(|d| d.amount > 0) && !simulate {
            self.dirty.store(true, Ordering::Relaxed);
        }
        drained.filter(|d| d.amount > 0)
    }

    /// Takes everything out, in tank order, keeping the tank count.
    pub fn take_all(&self) -> Vec<FluidContent> {
        self.dirty.store(true, Ordering::Relaxed);
        self.write().iter_mut().filter_map(Option::take).collect()
    }

    /// Whether the tanks changed since the last call.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn water(amount: u32) -> FluidContent {
        FluidContent {
            fluid: "minecraft:water".to_string(),
            amount,
            data: None,
        }
    }

    fn lava(amount: u32) -> FluidContent {
        FluidContent {
            fluid: "minecraft:lava".to_string(),
            amount,
            data: None,
        }
    }

    #[test]
    fn fill_merges_then_spills_into_empty_tanks() {
        let tanks = PluginTanks::new(3, 1000);
        assert_eq!(tanks.fill(&water(600), false), 600);
        assert_eq!(tanks.fill(&lava(300), false), 300);
        // Tank 0 takes 400 more, tank 2 the rest.
        assert_eq!(tanks.fill(&water(700), false), 700);
        assert_eq!(tanks.get(0).unwrap().amount, 1000);
        assert_eq!(tanks.get(1).unwrap().fluid, "minecraft:lava");
        assert_eq!(tanks.get(2).unwrap().amount, 300);
        assert_eq!(tanks.fill(&water(5000), false), 700);
    }

    #[test]
    fn simulate_changes_nothing() {
        let tanks = PluginTanks::new(1, 1000);
        assert_eq!(tanks.fill(&water(1500), true), 1000);
        assert!(tanks.get(0).is_none());
        tanks.fill(&water(500), false);
        assert_eq!(tanks.drain(200, None, true).unwrap().amount, 200);
        assert_eq!(tanks.get(0).unwrap().amount, 500);
    }

    #[test]
    fn data_keeps_fluids_apart() {
        let tanks = PluginTanks::new(1, 1000);
        let mut tagged = water(100);
        tagged.data = Some(vec![1]);
        assert_eq!(tanks.fill(&tagged, false), 100);
        assert_eq!(tanks.fill(&water(100), false), 0);
    }

    #[test]
    fn drain_picks_one_fluid_across_tanks() {
        let tanks = PluginTanks::new(3, 1000);
        tanks.fill(&water(400), false);
        tanks.fill(&lava(400), false);
        tanks.fill(&water(1000), false);
        // Water: tank 0 has 1000 now, tank 2 has 400.
        let drained = tanks.drain(1200, None, false).unwrap();
        assert_eq!((drained.fluid.as_str(), drained.amount), ("minecraft:water", 1200));
        assert_eq!(tanks.get(2).unwrap().amount, 200);
        assert_eq!(tanks.get(1).unwrap().fluid, "minecraft:lava");
        let lava_out = tanks.drain(5000, Some("minecraft:lava"), false).unwrap();
        assert_eq!(lava_out.amount, 400);
        assert!(tanks.drain(10, Some("minecraft:lava"), false).is_none());
        assert!(tanks.drain(0, None, false).is_none());
    }

    #[test]
    fn resize_rules() {
        let tanks = PluginTanks::new(2, 1000);
        tanks.fill(&water(800), false);
        assert!(tanks.resize(2, 500).is_err());
        assert!(tanks.resize(4, 1000).is_ok());
        tanks.fill(&water(800), false);
        assert!(tanks.resize(1, 1000).is_err());
        assert_eq!(tanks.tank_count(), 4);
    }

    #[test]
    fn nbt_roundtrip() {
        let tanks = PluginTanks::new(3, 2000);
        let mut tagged = lava(250);
        tagged.data = Some(vec![7, 200]);
        tanks.fill(&water(1500), false);
        tanks.fill(&tagged, false);
        let loaded = PluginTanks::from_nbt(&tanks.write_nbt()).unwrap();
        assert_eq!(loaded.tank_count(), 3);
        assert_eq!(loaded.capacity(), 2000);
        assert_eq!(loaded.get(0), Some(water(1500)));
        assert_eq!(loaded.get(1), Some(tagged));
        assert_eq!(loaded.get(2), None);
    }
}
