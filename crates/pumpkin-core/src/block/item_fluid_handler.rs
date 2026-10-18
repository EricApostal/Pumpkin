//! Fluids in items: buckets, water bottles and the containers plugins register.
//!
//! The handlers work on one item, the stack must hold exactly one (a stack of buckets has no
//! tank). Filling or draining a bucket or bottle swaps the item for the full or empty one.
//! A registered container keeps its fluid in the custom data of the stack, see
//! [`CONTENT_NAMESPACE`].

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use pumpkin_data::data_component_impl::PotionContentsImpl;
use pumpkin_data::fluid::Fluid;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::potion::Potion;
use pumpkin_nbt::tag::NbtTag;

use crate::block::entities::plugin_tank::FluidContent;
use crate::block::fluid_handler::{BUCKET_AMOUNT, POWDER_SNOW_KEY, resolve_fluid_key};

/// Millibuckets in a bottle: Create counts a bottle as a quarter of a bucket.
pub const BOTTLE_AMOUNT: u32 = 250;
/// Namespace and key in the custom data of the stack where registered containers keep a fluid.
pub const CONTENT_NAMESPACE: &str = "pumpkin_fluid";
pub const CONTENT_KEY: &str = "content";
const WATER_KEY: &str = "minecraft:water";

/// A fluid container item a plugin registered.
#[derive(Clone, Debug)]
pub struct FluidContainerDef {
    pub capacity: u32,
    /// Full keys of the fluids it takes, any fluid if empty.
    pub allowed: Vec<String>,
}

static CONTAINERS: LazyLock<RwLock<HashMap<u16, FluidContainerDef>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Makes an item a fluid container. Registering an item again replaces its definition.
pub fn register_fluid_container(
    item: &'static Item,
    capacity: u32,
    allowed: &[String],
) -> Result<(), String> {
    if capacity == 0 {
        return Err("a fluid container needs a capacity above 0".to_string());
    }
    let allowed = allowed
        .iter()
        .map(|key| resolve_fluid_key(key).ok_or_else(|| format!("unknown fluid '{key}'")))
        .collect::<Result<Vec<_>, _>>()?;
    CONTAINERS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            item.id,
            FluidContainerDef { capacity, allowed },
        );
    Ok(())
}

fn registered(item: &Item) -> Option<FluidContainerDef> {
    CONTAINERS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&item.id)
        .cloned()
}

enum Kind {
    Registered(FluidContainerDef),
    EmptyBucket,
    /// A bucket of this fluid.
    FilledBucket(String),
    EmptyBottle,
    WaterBottle,
}

fn bucket_fluid(item: &Item) -> Option<String> {
    if item.id == Item::POWDER_SNOW_BUCKET.id {
        return Some(POWDER_SNOW_KEY.to_string());
    }
    Fluid::from_bucket_item(item).map(|fluid| fluid.to_still().resource_location().into_owned())
}

/// The bucket item of a fluid key.
fn bucket_for(key: &str) -> Option<&'static Item> {
    if key == POWDER_SNOW_KEY {
        return Some(&Item::POWDER_SNOW_BUCKET);
    }
    Fluid::from_registry_key(key.strip_prefix("minecraft:").unwrap_or(key))?.bucket_item()
}

fn is_water_bottle(stack: &ItemStack) -> bool {
    stack.item.id == Item::POTION.id
        && stack
            .get_data_component::<PotionContentsImpl>()
            .is_some_and(|contents| {
                contents.potion_id == Some(i32::from(Potion::WATER.id))
                    && contents.custom_effects.is_empty()
            })
}

fn kind_of(stack: &ItemStack) -> Option<Kind> {
    if stack.is_empty() {
        return None;
    }
    if let Some(def) = registered(stack.item) {
        return Some(Kind::Registered(def));
    }
    if stack.item.id == Item::BUCKET.id {
        return Some(Kind::EmptyBucket);
    }
    if let Some(key) = bucket_fluid(stack.item) {
        return Some(Kind::FilledBucket(key));
    }
    if stack.item.id == Item::GLASS_BOTTLE.id {
        return Some(Kind::EmptyBottle);
    }
    is_water_bottle(stack).then_some(Kind::WaterBottle)
}

/// Whether the item holds fluid at all.
#[must_use]
pub fn is_fluid_container(stack: &ItemStack) -> bool {
    kind_of(stack).is_some()
}

/// Most millibuckets the container holds, 0 if the item is not one.
#[must_use]
pub fn capacity(stack: &ItemStack) -> u32 {
    match kind_of(stack) {
        Some(Kind::Registered(def)) => def.capacity,
        Some(Kind::EmptyBucket | Kind::FilledBucket(_)) => BUCKET_AMOUNT,
        Some(Kind::EmptyBottle | Kind::WaterBottle) => BOTTLE_AMOUNT,
        None => 0,
    }
}

fn registered_content(stack: &ItemStack) -> Option<FluidContent> {
    match stack.get_custom_data(CONTENT_NAMESPACE, CONTENT_KEY)? {
        NbtTag::Compound(compound) => FluidContent::read_nbt(&compound),
        _ => None,
    }
}

fn set_registered_content(stack: &mut ItemStack, content: Option<&FluidContent>) {
    match content {
        Some(content) => stack.set_custom_data(
            CONTENT_NAMESPACE,
            CONTENT_KEY,
            NbtTag::Compound(content.write_nbt()),
        ),
        None => stack.remove_custom_data(CONTENT_NAMESPACE, CONTENT_KEY),
    }
}

/// What the container holds, none if it is empty or not a container.
#[must_use]
pub fn contents(stack: &ItemStack) -> Option<FluidContent> {
    match kind_of(stack)? {
        Kind::Registered(_) => registered_content(stack),
        Kind::FilledBucket(key) => Some(FluidContent {
            fluid: key,
            amount: BUCKET_AMOUNT,
            data: None,
        }),
        Kind::WaterBottle => Some(FluidContent {
            fluid: WATER_KEY.to_string(),
            amount: BOTTLE_AMOUNT,
            data: None,
        }),
        Kind::EmptyBucket | Kind::EmptyBottle => None,
    }
}

/// Whether the fluid may be put into the container, no matter what it holds.
#[must_use]
pub fn is_fluid_valid(stack: &ItemStack, fluid_key: &str) -> bool {
    let Some(key) = resolve_fluid_key(fluid_key) else {
        return false;
    };
    match kind_of(stack) {
        Some(Kind::Registered(def)) => def.allowed.is_empty() || def.allowed.contains(&key),
        Some(Kind::EmptyBucket) => bucket_for(&key).is_some(),
        Some(Kind::FilledBucket(held)) => held == key,
        Some(Kind::EmptyBottle | Kind::WaterBottle) => key == WATER_KEY,
        None => false,
    }
}

/// A full bottle of water, as the glass bottle makes it.
fn water_bottle() -> ItemStack {
    ItemStack::new_with_component(
        1,
        &Item::POTION,
        vec![(
            pumpkin_data::data_component::DataComponent::PotionContents,
            Some(Box::new(PotionContentsImpl {
                potion_id: Some(i32::from(Potion::WATER.id)),
                custom_color: None,
                custom_effects: Vec::new(),
                custom_name: None,
            }) as Box<_>),
        )],
    )
}

/// Puts as much of the fluid into the container as fits and returns the amount that went in.
/// The stack must hold one item. With `simulate` nothing changes.
pub fn fill(stack: &mut ItemStack, fluid: &FluidContent, simulate: bool) -> u32 {
    if stack.item_count != 1 || fluid.amount == 0 {
        return 0;
    }
    let Some(key) = resolve_fluid_key(&fluid.fluid) else {
        return 0;
    };
    match kind_of(stack) {
        Some(Kind::Registered(def)) => {
            if !(def.allowed.is_empty() || def.allowed.contains(&key)) {
                return 0;
            }
            let held = registered_content(stack);
            if held
                .as_ref()
                .is_some_and(|held| held.fluid != key || held.data != fluid.data)
            {
                return 0;
            }
            let held_amount = held.as_ref().map_or(0, |held| held.amount);
            let moved = def.capacity.saturating_sub(held_amount).min(fluid.amount);
            if moved > 0 && !simulate {
                let content = FluidContent {
                    fluid: key,
                    amount: held_amount + moved,
                    data: fluid.data.clone(),
                };
                set_registered_content(stack, Some(&content));
            }
            moved
        }
        Some(Kind::EmptyBucket) => {
            let Some(bucket) = bucket_for(&key) else {
                return 0;
            };
            if fluid.amount < BUCKET_AMOUNT || fluid.data.is_some() {
                return 0;
            }
            if !simulate {
                *stack = ItemStack::new(1, bucket);
            }
            BUCKET_AMOUNT
        }
        Some(Kind::EmptyBottle) => {
            if key != WATER_KEY || fluid.amount < BOTTLE_AMOUNT || fluid.data.is_some() {
                return 0;
            }
            if !simulate {
                *stack = water_bottle();
            }
            BOTTLE_AMOUNT
        }
        Some(Kind::FilledBucket(_) | Kind::WaterBottle) | None => 0,
    }
}

/// Takes up to `max` out of the container, only `only` if given. Buckets and bottles give all
/// or nothing. The stack must hold one item. With `simulate` nothing changes.
pub fn drain(
    stack: &mut ItemStack,
    only: Option<&str>,
    max: u32,
    simulate: bool,
) -> Option<FluidContent> {
    if stack.item_count != 1 || max == 0 {
        return None;
    }
    let held = contents(stack)?;
    if only.is_some_and(|only| only != held.fluid) {
        return None;
    }
    match kind_of(stack)? {
        Kind::Registered(_) => {
            let moved = held.amount.min(max);
            if !simulate {
                let rest = held.amount - moved;
                set_registered_content(stack, (rest > 0).then(|| held.with_amount(rest)).as_ref());
            }
            Some(held.with_amount(moved))
        }
        Kind::FilledBucket(_) => {
            if max < BUCKET_AMOUNT {
                return None;
            }
            if !simulate {
                *stack = ItemStack::new(1, &Item::BUCKET);
            }
            Some(held)
        }
        Kind::WaterBottle => {
            if max < BOTTLE_AMOUNT {
                return None;
            }
            if !simulate {
                *stack = ItemStack::new(1, &Item::GLASS_BOTTLE);
            }
            Some(held)
        }
        Kind::EmptyBucket | Kind::EmptyBottle => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fluid(key: &str, amount: u32) -> FluidContent {
        FluidContent {
            fluid: key.to_string(),
            amount,
            data: None,
        }
    }

    #[test]
    fn buckets_fill_and_drain() {
        let mut bucket = ItemStack::new(1, &Item::BUCKET);
        assert_eq!(capacity(&bucket), 1000);
        assert_eq!(fill(&mut bucket, &fluid("minecraft:water", 999), false), 0);
        assert_eq!(fill(&mut bucket, &fluid("minecraft:lava", 5000), true), 1000);
        assert_eq!(bucket.item.id, Item::BUCKET.id);
        assert_eq!(fill(&mut bucket, &fluid("minecraft:lava", 5000), false), 1000);
        assert_eq!(bucket.item.id, Item::LAVA_BUCKET.id);
        assert!(drain(&mut bucket, Some("minecraft:water"), 1000, false).is_none());
        assert!(drain(&mut bucket, None, 500, false).is_none());
        let out = drain(&mut bucket, None, 1000, false).unwrap();
        assert_eq!((out.fluid.as_str(), out.amount), ("minecraft:lava", 1000));
        assert_eq!(bucket.item.id, Item::BUCKET.id);
    }

    #[test]
    fn stacks_of_buckets_have_no_tank() {
        let mut buckets = ItemStack::new(2, &Item::BUCKET);
        assert_eq!(fill(&mut buckets, &fluid("minecraft:water", 1000), false), 0);
    }

    #[test]
    fn bottles_hold_250() {
        let mut bottle = ItemStack::new(1, &Item::GLASS_BOTTLE);
        assert_eq!(fill(&mut bottle, &fluid("minecraft:lava", 1000), false), 0);
        assert_eq!(fill(&mut bottle, &fluid("minecraft:water", 249), false), 0);
        assert_eq!(fill(&mut bottle, &fluid("minecraft:water", 1000), false), 250);
        assert_eq!(bottle.item.id, Item::POTION.id);
        assert_eq!(contents(&bottle).unwrap().amount, 250);
        assert_eq!(drain(&mut bottle, None, 250, false).unwrap().amount, 250);
        assert_eq!(bottle.item.id, Item::GLASS_BOTTLE.id);
    }

    #[test]
    fn registered_containers_keep_content_in_custom_data() {
        let item = &Item::STICK;
        register_fluid_container(item, 3000, &["minecraft:water".to_string()]).unwrap();
        assert!(register_fluid_container(item, 0, &[]).is_err());
        let mut stack = ItemStack::new(1, item);
        assert_eq!(capacity(&stack), 3000);
        assert!(is_fluid_valid(&stack, "water"));
        assert!(!is_fluid_valid(&stack, "lava"));
        assert_eq!(fill(&mut stack, &fluid("minecraft:lava", 100), false), 0);
        assert_eq!(fill(&mut stack, &fluid("minecraft:water", 2000), false), 2000);
        assert_eq!(fill(&mut stack, &fluid("minecraft:water", 2000), false), 1000);
        assert_eq!(contents(&stack).unwrap().amount, 3000);
        assert_eq!(drain(&mut stack, None, 1200, true).unwrap().amount, 1200);
        assert_eq!(contents(&stack).unwrap().amount, 3000);
        assert_eq!(drain(&mut stack, None, 1200, false).unwrap().amount, 1200);
        assert_eq!(drain(&mut stack, None, 5000, false).unwrap().amount, 1800);
        assert!(contents(&stack).is_none());
        assert!(stack.custom_data_compound().is_none_or(|c| c.get(CONTENT_NAMESPACE).is_none()));
    }
}
