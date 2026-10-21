//! Runtime fluids registered by plugins on top of the generated vanilla fluids.
//!
//! Registered fluids get the ids `Fluid::VANILLA_COUNT + index`, in registration order. The
//! generated lookups (`Fluid::from_id`, `from_registry_key`, `from_state_id`) fall back to this
//! registry when their static tables miss. Registration closes with
//! [`Fluid::freeze_dynamic_registry`].
//!
//! Like vanilla, a still fluid and its flowing variant are two entries (`water` and
//! `flowing_water`). An entry may name a plugin block as its fluid block: every state of the block
//! then holds the fluid, a source for a still fluid, a flowing amount (`level` 1 to 8 and
//! `falling` block properties) for a flowing one. Fluids without a block are virtual, they only
//! exist in registries, tanks and buckets. The server has no flow physics for registered fluids.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, OnceLock, PoisonError, RwLock, RwLockReadGuard};

use crate::fluid::{Fluid, FluidState};
use crate::item::Item;
use crate::tag::RegistryKey;
use crate::{Block, BlockStateId};

/// Namespace of the generated fluids. It is implicit in their `name`.
const VANILLA_NAMESPACE: &str = "minecraft";
/// Fluid ids are `u16` in `Fluid`.
const MAX_FLUID_ID: usize = u16::MAX as usize;
const MAX_LUMINANCE: u8 = 15;
/// Vanilla fluid amounts: 8 for a source, 1 to 7 for flowing fluid.
const SOURCE_AMOUNT: i16 = 8;

/// The plugin block that holds a fluid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FluidPlacement {
    /// Full key of a registered plugin block.
    pub block: String,
    /// Whether the block holds flowing fluid. Otherwise all its states are sources.
    pub flowing: bool,
}

/// Description of a fluid to add to the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FluidRegistration {
    /// Full resource location, `namespace:path`. The `minecraft` namespace is reserved.
    pub key: String,
    /// The block that holds the fluid in the world, if it can be placed there.
    pub placement: Option<FluidPlacement>,
    /// Key of the registered fluid that is the flowing variant of this (still) fluid.
    pub flowing_variant: Option<String>,
    /// Key of the registered plugin item that is the bucket of this fluid.
    pub bucket_item: Option<String>,
    /// Ticks between flow steps (`Fluid::flow_speed`).
    pub tick_rate: u8,
    /// How much the amount drops per block of flow, 1 to 8.
    pub level_decrease_per_block: u8,
    /// How far from a hole the fluid looks for a slope (`Fluid::flow_distance`).
    pub slope_find_distance: u8,
    pub can_convert_to_source: bool,
    /// Emitted light, 0 to 15. Informational.
    pub luminance: u8,
    /// Vanilla water is 1000 and lava 3000. Informational.
    pub density: i32,
    /// In kelvin, water is 300 and lava 1300. Informational.
    pub temperature: i32,
    /// Vanilla water is 1000 and lava 6000. Informational.
    pub viscosity: i32,
    /// Fluid tags the fluid joins, `namespace:path`.
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FluidRegistrationError {
    /// The key is not a valid `namespace:path` resource location.
    InvalidKey(String),
    /// The `minecraft` namespace belongs to the generated fluids.
    ReservedNamespace(String),
    /// A fluid with this key exists already with another definition.
    Duplicate(String),
    /// The definition is not valid, the text says why.
    Invalid(String),
    /// There is no registered plugin block with this key.
    UnknownBlock(String),
    /// The block holds another fluid already.
    BlockInUse(String),
    /// There is no registered plugin fluid with this key.
    UnknownFluid(String),
    /// The fluid is the flowing variant of another fluid already.
    VariantInUse(String),
    /// There is no registered plugin item with this key.
    UnknownItem(String),
    /// The item is the bucket of another fluid already, or one of the vanilla buckets.
    ItemInUse(String),
    /// The registry is closed, see [`Fluid::freeze_dynamic_registry`].
    Frozen,
    /// There are no ids left.
    Full,
}

impl fmt::Display for FluidRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey(key) => write!(
                f,
                "'{key}' is not a valid fluid key, expected namespace:path"
            ),
            Self::ReservedNamespace(key) => write!(
                f,
                "'{key}' uses the reserved '{VANILLA_NAMESPACE}' namespace"
            ),
            Self::Duplicate(key) => write!(
                f,
                "fluid '{key}' is already registered with another definition"
            ),
            Self::Invalid(reason) => write!(f, "invalid fluid definition: {reason}"),
            Self::UnknownBlock(key) => {
                write!(f, "'{key}' is not a block registered by a plugin")
            }
            Self::BlockInUse(key) => write!(f, "block '{key}' holds another fluid already"),
            Self::UnknownFluid(key) => {
                write!(f, "'{key}' is not a fluid registered by a plugin")
            }
            Self::VariantInUse(key) => {
                write!(f, "fluid '{key}' is the flowing variant of another fluid")
            }
            Self::UnknownItem(key) => {
                write!(f, "'{key}' is not an item registered by a plugin")
            }
            Self::ItemInUse(key) => write!(f, "item '{key}' is the bucket of another fluid"),
            Self::Frozen => write!(f, "fluids can only be registered before players connect"),
            Self::Full => write!(f, "the fluid registry is full"),
        }
    }
}

impl std::error::Error for FluidRegistrationError {}

fn is_valid_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
}

fn is_valid_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/'))
}

fn check_key(key: &str) -> Result<(), FluidRegistrationError> {
    let (namespace, path) = key
        .split_once(':')
        .ok_or_else(|| FluidRegistrationError::InvalidKey(key.to_string()))?;
    if !is_valid_namespace(namespace) || !is_valid_path(path) {
        return Err(FluidRegistrationError::InvalidKey(key.to_string()));
    }
    if namespace == VANILLA_NAMESPACE {
        return Err(FluidRegistrationError::ReservedNamespace(key.to_string()));
    }
    Ok(())
}

/// Checks that `variant` may be the flowing variant of a still fluid.
fn check_flowing_variant(
    key: &str,
    variant: &'static DynamicFluidInfo,
    linked: &HashSet<u16>,
) -> Result<&'static Fluid, FluidRegistrationError> {
    if linked.contains(&variant.fluid.id) {
        return Err(FluidRegistrationError::VariantInUse(key.to_string()));
    }
    let holds_still_block = variant
        .registration
        .placement
        .as_ref()
        .is_some_and(|placement| !placement.flowing);
    if holds_still_block || variant.registration.flowing_variant.is_some() {
        return Err(FluidRegistrationError::Invalid(format!(
            "'{key}' cannot be a flowing variant"
        )));
    }
    Ok(variant.fluid)
}

/// What the registry keeps for a registered fluid beyond its `Fluid`.
pub struct DynamicFluidInfo {
    pub fluid: &'static Fluid,
    /// The registration as given.
    pub registration: FluidRegistration,
    /// The block that holds this fluid.
    pub block: Option<&'static Block>,
    /// The bucket item.
    pub bucket_item: Option<&'static Item>,
    /// Set when the flowing variant registers; a still fluid may be registered first.
    flowing_variant: OnceLock<&'static Fluid>,
    /// Set when the still fluid that names this one as its flowing variant registers.
    still_variant: OnceLock<&'static Fluid>,
}

impl DynamicFluidInfo {
    /// The flowing variant of this (still) fluid, `None` while it is not registered yet.
    #[must_use]
    pub fn flowing_variant(&self) -> Option<&'static Fluid> {
        self.flowing_variant.get().copied()
    }

    /// Whether the block of this fluid holds flowing fluid.
    #[must_use]
    pub fn holds_flowing(&self) -> bool {
        self.registration
            .placement
            .as_ref()
            .is_some_and(|placement| placement.flowing)
    }
}

/// The fluid state of a state of the block of a registered fluid.
fn fluid_state_of(block: &Block, flowing: bool, id: BlockStateId) -> FluidState {
    let mut amount = SOURCE_AMOUNT;
    let mut falling = false;
    if flowing {
        let info = block.dynamic_info();
        let index = info.and_then(|info| info.state_index(id));
        let value = |property: &str| {
            info.zip(index)
                .and_then(|(info, index)| info.value(index, property))
        };
        falling = value("falling") == Some("true");
        if !falling {
            amount = value("level")
                .and_then(|level| level.parse::<i16>().ok())
                .map_or(SOURCE_AMOUNT, |level| level.clamp(1, SOURCE_AMOUNT));
        }
    }
    FluidState {
        height: f32::from(amount) / 9.0,
        level: amount,
        is_empty: false,
        blast_resistance: block.blast_resistance,
        block_state_id: id,
        is_still: !flowing,
        is_source: !flowing,
        falling,
    }
}

/// Append only fluid table. The global instance backs `Fluid::from_id` and friends.
#[derive(Clone)]
pub struct FluidRegistry {
    frozen: bool,
    fluids: Vec<&'static DynamicFluidInfo>,
    by_key: HashMap<&'static str, &'static DynamicFluidInfo>,
    /// State id ranges of fluid blocks: first state, last state, fluid.
    block_ranges: Vec<(u32, u32, &'static DynamicFluidInfo)>,
    /// Bucket item id to fluid.
    by_bucket: HashMap<u16, &'static DynamicFluidInfo>,
    /// Ids of the fluids that are the flowing variant of a still fluid.
    linked: HashSet<u16>,
    /// Still fluids whose `flowing-variant` was not registered yet: the key they wait for.
    pending_variants: Vec<(String, &'static DynamicFluidInfo)>,
}

impl FluidRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            frozen: false,
            fluids: Vec::new(),
            by_key: HashMap::new(),
            block_ranges: Vec::new(),
            by_bucket: HashMap::new(),
            linked: HashSet::new(),
            pending_variants: Vec::new(),
        }
    }

    /// Closes the registry. A still fluid whose `flowing-variant` never registered is
    /// [`FluidRegistrationError::UnknownFluid`]: the key is a typo, not a fluid.
    pub fn freeze(&mut self) -> Result<(), FluidRegistrationError> {
        self.frozen = true;
        match self.pending_variants.first() {
            Some((key, _)) => Err(FluidRegistrationError::UnknownFluid(key.clone())),
            None => Ok(()),
        }
    }

    #[must_use]
    pub const fn is_frozen(&self) -> bool {
        self.frozen
    }

    fn validate(registration: &FluidRegistration) -> Result<(), FluidRegistrationError> {
        check_key(&registration.key)?;
        let invalid = |reason: &str| Err(FluidRegistrationError::Invalid(reason.to_string()));
        if registration.tick_rate == 0 {
            return invalid("tick-rate must be at least 1");
        }
        if !(1..=8).contains(&registration.level_decrease_per_block) {
            return invalid("level-decrease-per-block must be 1 to 8");
        }
        if registration.slope_find_distance > 8 {
            return invalid("slope-find-distance must be at most 8");
        }
        if registration.luminance > MAX_LUMINANCE {
            return invalid("luminance must be 0 to 15");
        }
        if registration.flowing_variant.is_some()
            && registration
                .placement
                .as_ref()
                .is_some_and(|placement| placement.flowing)
        {
            return invalid("a flowing fluid cannot have a flowing variant");
        }
        if registration.flowing_variant.as_deref() == Some(registration.key.as_str()) {
            return invalid("a fluid cannot be its own flowing variant");
        }
        Ok(())
    }

    /// Adds a fluid. Registering the same definition again returns the existing fluid, even once
    /// closed. A different definition for a known key is [`FluidRegistrationError::Duplicate`].
    pub fn register(
        &mut self,
        registration: FluidRegistration,
    ) -> Result<&'static Fluid, FluidRegistrationError> {
        Self::validate(&registration)?;

        if let Some(existing) = self.by_key.get(registration.key.as_str()) {
            return if existing.registration == registration {
                Ok(existing.fluid)
            } else {
                Err(FluidRegistrationError::Duplicate(registration.key))
            };
        }
        if self.frozen {
            return Err(FluidRegistrationError::Frozen);
        }
        let id = usize::from(Fluid::VANILLA_COUNT) + self.fluids.len();
        if id > MAX_FLUID_ID {
            return Err(FluidRegistrationError::Full);
        }

        let fluid_key = registration.key.clone();
        let flowing_key = registration.flowing_variant.clone();
        let awaited = self
            .pending_variants
            .iter()
            .any(|(key, _)| key == &registration.key);
        if awaited {
            let key = registration.key.as_str();
            let holds_still_block = registration
                .placement
                .as_ref()
                .is_some_and(|placement| !placement.flowing);
            if holds_still_block || registration.flowing_variant.is_some() {
                return Err(FluidRegistrationError::Invalid(format!(
                    "'{key}' cannot be a flowing variant"
                )));
            }
        } else if let Some(key) = registration.flowing_variant.as_deref() {
            // A fluid is the flowing variant of one still fluid at most, so two still fluids
            // waiting for the same key is a conflict. Report it before anything is registered.
            if self
                .pending_variants
                .iter()
                .any(|(pending, _)| pending == key)
            {
                return Err(FluidRegistrationError::VariantInUse(key.to_string()));
            }
        }

        let block = match &registration.placement {
            Some(placement) => {
                let block = Block::from_dynamic_registry_key(&placement.block)
                    .filter(|block| block.is_dynamic())
                    .ok_or_else(|| FluidRegistrationError::UnknownBlock(placement.block.clone()))?;
                let first = block.dynamic_info().map(|info| info.base_state_id());
                if self
                    .block_ranges
                    .iter()
                    .any(|(start, _, _)| Some(*start) == first)
                {
                    return Err(FluidRegistrationError::BlockInUse(placement.block.clone()));
                }
                Some(block)
            }
            None => None,
        };

        // A still fluid may name its flowing variant before that variant registers, so a
        // name that is unknown yet is kept as pending instead of failing here.
        let flowing_variant = match registration.flowing_variant.as_deref() {
            Some(key) => match self.by_key.get(key) {
                Some(variant) => Some(check_flowing_variant(key, variant, &self.linked)?),
                None => None,
            },
            None => None,
        };

        let bucket_item = match &registration.bucket_item {
            Some(key) => {
                let item = Item::from_registry_key(key)
                    .filter(|item| item.is_dynamic())
                    .ok_or_else(|| FluidRegistrationError::UnknownItem(key.clone()))?;
                if self.by_bucket.contains_key(&item.id) {
                    return Err(FluidRegistrationError::ItemInUse(key.clone()));
                }
                Some(item)
            }
            None => None,
        };

        let flowing = registration
            .placement
            .as_ref()
            .is_some_and(|placement| placement.flowing);
        let state = match block {
            Some(block) => fluid_state_of(block, flowing, block.default_state.id),
            // Only registries and tanks know the fluid, there is no block state.
            None => FluidState {
                height: f32::from(SOURCE_AMOUNT) / 9.0,
                level: SOURCE_AMOUNT,
                is_empty: false,
                blast_resistance: 100.0,
                block_state_id: BlockStateId::AIR,
                is_still: !flowing,
                is_source: !flowing,
                falling: false,
            },
        };

        let name: &'static str = Box::leak(registration.key.clone().into_boxed_str());
        let fluid: &'static Fluid = Box::leak(Box::new(Fluid {
            id: id as u16,
            name,
            properties: None,
            states: Box::leak(vec![state].into_boxed_slice()),
            default_state_index: 0,
            flow_speed: u32::from(registration.tick_rate),
            flow_distance: u32::from(registration.slope_find_distance),
            can_convert_to_source: registration.can_convert_to_source,
        }));
        let info: &'static DynamicFluidInfo = Box::leak(Box::new(DynamicFluidInfo {
            fluid,
            registration,
            block,
            bucket_item,
            flowing_variant: OnceLock::new(),
            still_variant: OnceLock::new(),
        }));
        if let Some(variant) = flowing_variant {
            self.linked.insert(variant.id);
            let _ = info.flowing_variant.set(variant);
            if let Some(variant_info) = self.get_info(variant.id) {
                let _ = variant_info.still_variant.set(fluid);
            }
        }
        if let Some(block_info) = block.and_then(Block::dynamic_info) {
            self.block_ranges.push((
                block_info.base_state_id(),
                block_info.base_state_id() + (block_info.state_count() - 1),
                info,
            ));
        }
        if let Some(item) = bucket_item {
            self.by_bucket.insert(item.id, info);
        }
        self.fluids.push(info);
        self.by_key.insert(name, info);

        // Fluids registered before this one that wait for it as their flowing variant.
        if awaited {
            let mut waiting = Vec::new();
            let mut rest = Vec::new();
            for entry in self.pending_variants.drain(..) {
                if entry.0 == fluid_key {
                    waiting.push(entry)
                } else {
                    rest.push(entry)
                }
            }
            self.pending_variants = rest;
            for (_, still) in waiting {
                let _ = still.flowing_variant.set(fluid);
                let _ = info.still_variant.set(still.fluid);
                self.linked.insert(id as u16);
            }
        } else if flowing_variant.is_none() {
            // The variant registered after this fluid: wait for it. When it was already there
            // (found above), linking happened above and nothing may stay pending -- `freeze`
            // reports every leftover entry as an unregistered fluid.
            if let Some(key) = flowing_key {
                self.pending_variants.push((key, info));
            }
        }
        Ok(fluid)
    }

    #[must_use]
    pub fn get_info(&self, id: u16) -> Option<&'static DynamicFluidInfo> {
        let index = usize::from(id).checked_sub(usize::from(Fluid::VANILLA_COUNT))?;
        self.fluids.get(index).copied()
    }

    #[must_use]
    pub fn get_by_id(&self, id: u16) -> Option<&'static Fluid> {
        self.get_info(id).map(|info| info.fluid)
    }

    #[must_use]
    pub fn get_by_key(&self, key: &str) -> Option<&'static Fluid> {
        self.by_key.get(key).map(|info| info.fluid)
    }

    /// The fluid whose block has this state.
    #[must_use]
    pub fn get_by_state(&self, id: BlockStateId) -> Option<&'static DynamicFluidInfo> {
        let id = id.as_u32();
        self.block_ranges
            .iter()
            .find(|(start, end, _)| (*start..=*end).contains(&id))
            .map(|(_, _, info)| *info)
    }

    /// The fluid that has this bucket item.
    #[must_use]
    pub fn get_by_bucket(&self, item_id: u16) -> Option<&'static DynamicFluidInfo> {
        self.by_bucket.get(&item_id).copied()
    }

    /// Registered fluids in registration (and so id) order.
    #[must_use]
    pub fn fluids(&self) -> &[&'static DynamicFluidInfo] {
        &self.fluids
    }
}

impl Default for FluidRegistry {
    fn default() -> Self {
        Self::new()
    }
}

static REGISTRY: LazyLock<RwLock<FluidRegistry>> =
    LazyLock::new(|| RwLock::new(FluidRegistry::new()));
/// A copy of the registry taken when it was closed, read without taking a lock.
static FROZEN: OnceLock<FluidRegistry> = OnceLock::new();
/// Lets the state lookups of every block skip the registry while no fluid has a block.
static HAS_FLUID_BLOCKS: AtomicBool = AtomicBool::new(false);

/// Read access: the lock free copy once closed, the live registry before that.
enum RegistryRead {
    Frozen(&'static FluidRegistry),
    Live(RwLockReadGuard<'static, FluidRegistry>),
}

impl std::ops::Deref for RegistryRead {
    type Target = FluidRegistry;

    fn deref(&self) -> &FluidRegistry {
        match self {
            Self::Frozen(registry) => registry,
            Self::Live(guard) => guard,
        }
    }
}

fn read_registry() -> RegistryRead {
    match FROZEN.get() {
        Some(registry) => RegistryRead::Frozen(registry),
        None => RegistryRead::Live(REGISTRY.read().unwrap_or_else(PoisonError::into_inner)),
    }
}

fn write_registry() -> std::sync::RwLockWriteGuard<'static, FluidRegistry> {
    REGISTRY.write().unwrap_or_else(PoisonError::into_inner)
}

impl Fluid {
    /// Whether this fluid was registered at runtime instead of being generated.
    #[must_use]
    pub const fn is_dynamic(&self) -> bool {
        self.id >= Self::VANILLA_COUNT
    }

    /// The full resource location, `minecraft:` prefixed for vanilla fluids.
    #[must_use]
    pub fn resource_location(&self) -> Cow<'static, str> {
        if self.is_dynamic() {
            Cow::Borrowed(self.name)
        } else {
            Cow::Owned(format!("{VANILLA_NAMESPACE}:{}", self.name))
        }
    }

    /// Registers a fluid in the global registry and returns it, see [`FluidRegistry::register`].
    /// The fluid joins the tags of its definition.
    pub fn register_dynamic(
        registration: FluidRegistration,
    ) -> Result<&'static Self, FluidRegistrationError> {
        let key = registration.key.clone();
        let tags = registration.tags.clone();
        let has_block = registration.placement.is_some();
        let fluid = write_registry().register(registration)?;
        if has_block {
            HAS_FLUID_BLOCKS.store(true, Ordering::Release);
        }
        for tag in tags {
            Self::register_dynamic_tag(&tag, std::slice::from_ref(&key));
        }
        Ok(fluid)
    }

    /// Adds entries (fluid keys or `#tag` references) to a fluid tag, creating it if needed.
    pub fn register_dynamic_tag(tag: &str, entries: &[String]) {
        crate::dynamic_tag::register_tag(RegistryKey::Fluid, tag, entries.iter().cloned());
    }

    /// Closes the global registry. Later registrations fail with [`FluidRegistrationError::Frozen`].
    pub fn freeze_dynamic_registry() -> Result<(), FluidRegistrationError> {
        {
            let mut registry = write_registry();
            registry.freeze()?;
            // Set while the write lock is held, so no change can slip in between.
            let _ = FROZEN.set(registry.clone());
        }
        // Runtime tags that name entries registered after the tag are resolved now.
        crate::dynamic_tag::refresh();
        Ok(())
    }

    #[must_use]
    pub fn is_dynamic_registry_frozen() -> bool {
        FROZEN.get().is_some()
    }

    /// Fluids registered at runtime, in id order.
    #[must_use]
    pub fn dynamic_fluids() -> Vec<&'static Self> {
        read_registry()
            .fluids()
            .iter()
            .map(|info| info.fluid)
            .collect()
    }

    /// What the registry keeps for a dynamic fluid, `None` for a generated one.
    #[must_use]
    pub fn dynamic_info(&self) -> Option<&'static DynamicFluidInfo> {
        if !self.is_dynamic() {
            return None;
        }
        read_registry().get_info(self.id)
    }

    /// The fluid tags for the client tag sync (generated plus runtime). `None` when there are no
    /// runtime fluid tags, so the generated tables can be sent as they are.
    #[must_use]
    pub fn network_tags() -> Option<Vec<(String, Vec<u16>)>> {
        crate::dynamic_tag::merged_tags(RegistryKey::Fluid, |key| {
            crate::fluid::get_fluid(key).map(|fluid| fluid.id)
        })
    }

    /// Fallback of the generated `from_id` for ids past the vanilla range.
    #[must_use]
    pub fn from_dynamic_id(id: u16) -> Option<&'static Self> {
        if id < Self::VANILLA_COUNT {
            return None;
        }
        read_registry().get_by_id(id)
    }

    /// Fallback of the generated `from_registry_key` for keys that are not vanilla. Only
    /// namespaced keys can match, vanilla keys never carry a namespace here.
    #[must_use]
    pub fn from_dynamic_registry_key(key: &str) -> Option<&'static Self> {
        if !key.contains(':') {
            return None;
        }
        read_registry().get_by_key(key)
    }

    /// Fallback of the generated `from_state_id` for the states of the blocks of registered fluids.
    #[must_use]
    pub fn from_dynamic_state_id(id: BlockStateId) -> Option<&'static Self> {
        if id.as_u32() < u32::from(BlockStateId::COUNT) || !HAS_FLUID_BLOCKS.load(Ordering::Acquire) {
            return None;
        }
        read_registry().get_by_state(id).map(|info| info.fluid)
    }

    /// The fluid and its state in a block state of a registered fluid block. The fluid is the
    /// flowing variant when there is one, like `to_flowing`.
    #[must_use]
    pub fn dynamic_fluid_state(id: BlockStateId) -> Option<(&'static Self, FluidState)> {
        if id.as_u32() < u32::from(BlockStateId::COUNT) || !HAS_FLUID_BLOCKS.load(Ordering::Acquire) {
            return None;
        }
        let info = read_registry().get_by_state(id)?;
        let block = info.block?;
        let state = fluid_state_of(block, info.holds_flowing(), id);
        let fluid = info.flowing_variant().unwrap_or(info.fluid);
        Some((fluid, state))
    }

    /// Fallback of the generated `same_fluid_type` for registered fluids: a still fluid and its
    /// flowing variant are the same type. Equal ids are handled by the caller.
    #[must_use]
    pub fn dynamic_same_fluid_type(a: u16, b: u16) -> bool {
        if a < Self::VANILLA_COUNT || b < Self::VANILLA_COUNT {
            return false;
        }
        let registry = read_registry();
        let (Some(a), Some(b)) = (registry.get_info(a), registry.get_info(b)) else {
            return false;
        };
        a.flowing_variant()
            .is_some_and(|fluid| fluid.id == b.fluid.id)
            || b.flowing_variant()
                .is_some_and(|fluid| fluid.id == a.fluid.id)
    }

    /// The flowing variant of a registered still fluid.
    #[must_use]
    pub fn dynamic_flowing_variant(&self) -> Option<&'static Self> {
        self.dynamic_info()?.flowing_variant()
    }

    /// The still fluid of this fluid's family: `water` for `flowing_water`, the still fluid that
    /// names a registered fluid as its flowing variant. Other fluids are their own still fluid.
    #[must_use]
    pub fn to_still(&self) -> &'static Self {
        match self.id {
            1 | 2 => &Self::WATER,
            3 | 4 => &Self::LAVA,
            _ => self
                .dynamic_info()
                .and_then(|info| info.still_variant.get().copied())
                .or_else(|| Self::from_id(self.id))
                .unwrap_or(&Self::EMPTY),
        }
    }

    /// The bucket item of the fluid, if it has one.
    #[must_use]
    pub fn bucket_item(&self) -> Option<&'static Item> {
        match self.id {
            1 | 2 => Some(&Item::WATER_BUCKET),
            3 | 4 => Some(&Item::LAVA_BUCKET),
            _ => {
                let info = self.dynamic_info()?;
                info.bucket_item.or_else(|| {
                    // The flowing variant shares the bucket of the still fluid.
                    info.still_variant
                        .get()
                        .and_then(|still| still.dynamic_info())
                        .and_then(|still| still.bucket_item)
                })
            }
        }
    }

    /// The still fluid that a bucket item holds, `None` for items that are not fluid buckets.
    #[must_use]
    pub fn from_bucket_item(item: &Item) -> Option<&'static Self> {
        if item.id == Item::WATER_BUCKET.id {
            return Some(&Self::WATER);
        }
        if item.id == Item::LAVA_BUCKET.id {
            return Some(&Self::LAVA);
        }
        if !item.is_dynamic() {
            return None;
        }
        read_registry()
            .get_by_bucket(item.id)
            .map(|info| info.fluid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_registry::{BlockDrops, BlockRegistration, ShapeDefinition};

    fn registration(key: &str) -> FluidRegistration {
        FluidRegistration {
            key: key.to_string(),
            placement: None,
            flowing_variant: None,
            bucket_item: None,
            tick_rate: 5,
            level_decrease_per_block: 1,
            slope_find_distance: 4,
            can_convert_to_source: false,
            luminance: 0,
            density: 1000,
            temperature: 300,
            viscosity: 1000,
            tags: Vec::new(),
        }
    }

    fn block_registration(key: &str) -> BlockRegistration {
        BlockRegistration {
            key: key.to_string(),
            properties: Vec::new(),
            default_state: Vec::new(),
            hardness: 100.0,
            blast_resistance: 100.0,
            requires_correct_tool: false,
            sound_type: "stone".to_string(),
            luminance: 0,
            can_occlude: false,
            replaceable: true,
            map_color: 0,
            collision_shape: ShapeDefinition::Empty,
            selection_shape: ShapeDefinition::Empty,
            shape_rules: Vec::new(),
            piston_behavior: crate::block_state::PistonBehavior::Normal,
            connect_rules: Vec::new(),
            drops: BlockDrops::Nothing,
            tags: Vec::new(),
        }
    }

    #[test]
    fn vanilla_count_matches_generated_ids() {
        assert!(Fluid::from_id(Fluid::VANILLA_COUNT - 1).is_some());
        assert_eq!(Fluid::VANILLA_COUNT, 5);
        assert_eq!(Fluid::LAVA.id, Fluid::VANILLA_COUNT - 1);
    }

    #[test]
    fn ids_follow_vanilla_count_in_registration_order() {
        let mut registry = FluidRegistry::new();
        let first = registry.register(registration("test_order:first")).unwrap();
        let second = registry
            .register(registration("test_order:second"))
            .unwrap();
        assert_eq!(first.id, Fluid::VANILLA_COUNT);
        assert_eq!(second.id, Fluid::VANILLA_COUNT + 1);
        assert!(registry.get_by_id(first.id) == Some(first));
        assert!(registry.get_by_key("test_order:second") == Some(second));
        assert!(first.is_dynamic());
        assert!(!Fluid::WATER.is_dynamic());
        assert_eq!(first.resource_location(), "test_order:first");
        assert_eq!(Fluid::WATER.resource_location(), "minecraft:water");
        assert!(registry.get_by_id(Fluid::VANILLA_COUNT + 2).is_none());
    }

    #[test]
    fn rejects_bad_definitions_duplicates_and_frozen() {
        let mut registry = FluidRegistry::new();
        for bad in ["nocolon", "Upper:case", "ns:", ":path", "ns:sp ace"] {
            assert_eq!(
                registry.register(registration(bad)).err(),
                Some(FluidRegistrationError::InvalidKey(bad.to_string())),
            );
        }
        assert_eq!(
            registry.register(registration("minecraft:thing")).err(),
            Some(FluidRegistrationError::ReservedNamespace(
                "minecraft:thing".to_string()
            )),
        );
        let mut slow = registration("test_dup:slow");
        slow.tick_rate = 0;
        assert!(matches!(
            registry.register(slow),
            Err(FluidRegistrationError::Invalid(_))
        ));
        let mut no_block = registration("test_dup:no_block");
        no_block.placement = Some(FluidPlacement {
            block: "test_dup:no_such_block".to_string(),
            flowing: false,
        });
        assert_eq!(
            registry.register(no_block).err(),
            Some(FluidRegistrationError::UnknownBlock(
                "test_dup:no_such_block".to_string()
            )),
        );

        let first = registry.register(registration("test_dup:a")).unwrap();
        assert!(registry.register(registration("test_dup:a")).unwrap() == first);
        let mut different = registration("test_dup:a");
        different.tick_rate = 30;
        assert_eq!(
            registry.register(different).err(),
            Some(FluidRegistrationError::Duplicate("test_dup:a".to_string())),
        );
        registry.freeze();
        assert_eq!(
            registry.register(registration("test_dup:b")).err(),
            Some(FluidRegistrationError::Frozen),
        );
        assert!(registry.register(registration("test_dup:a")).unwrap() == first);
        assert_eq!(registry.fluids().len(), 1);
    }

    #[test]
    fn a_still_fluid_may_name_a_variant_that_registers_later() {
        let mut registry = FluidRegistry::new();
        let mut still = registration("test_pending:goo");
        still.flowing_variant = Some("test_pending:flowing_goo".to_string());
        let still = registry.register(still).unwrap();
        assert!(registry.get_info(still.id).unwrap().flowing_variant().is_none());

        let flowing = registry
            .register(registration("test_pending:flowing_goo"))
            .unwrap();
        let info = registry.get_info(still.id).unwrap();
        assert!(info.flowing_variant() == Some(flowing));
        assert!(registry.get_info(flowing.id).unwrap().still_variant.get() == Some(&still));
        assert_eq!(registry.freeze(), Ok(()));

        // Two still fluids cannot wait for the same flowing variant.
        let mut duplicate = FluidRegistry::new();
        let mut first = registration("test_pending_dup:goo");
        first.flowing_variant = Some("test_pending_dup:flowing_goo".to_string());
        assert!(duplicate.register(first).is_ok());
        let mut second = registration("test_pending_dup:goo2");
        second.flowing_variant = Some("test_pending_dup:flowing_goo".to_string());
        assert_eq!(
            duplicate.register(second).err(),
            Some(FluidRegistrationError::VariantInUse(
                "test_pending_dup:flowing_goo".to_string()
            )),
        );

        // A name that never registers is a typo, reported when the registry closes.
        let mut missing = FluidRegistry::new();
        let mut still = registration("test_missing:goo");
        still.flowing_variant = Some("test_missing:nothing".to_string());
        assert!(missing.register(still).is_ok());
        assert_eq!(
            missing.freeze().err(),
            Some(FluidRegistrationError::UnknownFluid(
                "test_missing:nothing".to_string()
            )),
        );
    }

    #[test]
    fn flowing_variant_links_the_family() {
        let mut registry = FluidRegistry::new();
        let flowing = registry
            .register(registration("test_family:flowing_goo"))
            .unwrap();
        let mut still = registration("test_family:goo");
        still.flowing_variant = Some("test_family:flowing_goo".to_string());
        let still = registry.register(still).unwrap();
        let info = registry.get_info(still.id).unwrap();
        assert!(info.flowing_variant() == Some(flowing));
        assert!(registry.get_info(flowing.id).unwrap().still_variant.get() == Some(&still));

        let mut again = registration("test_family:goo2");
        again.flowing_variant = Some("test_family:flowing_goo".to_string());
        assert_eq!(
            registry.register(again).err(),
            Some(FluidRegistrationError::VariantInUse(
                "test_family:flowing_goo".to_string()
            )),
        );
        // Nothing is waiting: the variant registered first, so the still fluid linked to it
        // immediately and must not have left a pending entry behind.
        assert_eq!(registry.freeze(), Ok(()));
    }

    /// Both orders of a still/flowing pair close cleanly. A still fluid whose variant is
    /// already registered used to also queue the variant as pending, and `freeze` then
    /// reported the pair's own flowing fluid as unknown.
    #[test]
    fn a_fluid_pair_closes_cleanly_in_either_order() {
        let pair = |registry: &mut FluidRegistry, still_first: bool| {
            let mut still = registration("test_pair:goo");
            still.flowing_variant = Some("test_pair:flowing_goo".to_string());
            let (still, flowing) = if still_first {
                let still = registry.register(still).unwrap();
                let flowing = registry.register(registration("test_pair:flowing_goo")).unwrap();
                (still, flowing)
            } else {
                let flowing = registry.register(registration("test_pair:flowing_goo")).unwrap();
                let still = registry.register(still).unwrap();
                (still, flowing)
            };
            let info = registry.get_info(still.id).unwrap();
            assert!(info.flowing_variant() == Some(flowing));
            assert!(registry.get_info(flowing.id).unwrap().still_variant.get() == Some(&still));
            assert_eq!(registry.freeze(), Ok(()));
        };
        let mut still_first = FluidRegistry::new();
        pair(&mut still_first, true);
        let mut flowing_first = FluidRegistry::new();
        pair(&mut flowing_first, false);
    }

    #[test]
    fn fluid_blocks_hold_their_fluid() {
        let source_block = Block::register_dynamic(block_registration("test_block:goo")).unwrap();
        let flowing_block =
            Block::register_dynamic(block_registration("test_block:flowing_goo")).unwrap();

        let mut flowing = registration("test_block:flowing_goo");
        flowing.placement = Some(FluidPlacement {
            block: "test_block:flowing_goo".to_string(),
            flowing: true,
        });
        let mut still = registration("test_block:goo");
        still.placement = Some(FluidPlacement {
            block: "test_block:goo".to_string(),
            flowing: false,
        });
        still.flowing_variant = Some("test_block:flowing_goo".to_string());

        let flowing = Fluid::register_dynamic(flowing).unwrap();
        let still = Fluid::register_dynamic(still).unwrap();
        let source_state = source_block.default_state.id;
        let flowing_state = flowing_block.default_state.id;

        assert!(Fluid::from_state_id(source_state) == Some(still));
        assert!(Fluid::from_state_id(flowing_state) == Some(flowing));
        assert!(Fluid::from_state_id(Block::STONE.default_state.id) == None);
        assert!(Fluid::from_id(still.id) == Some(still));
        assert!(Fluid::from_registry_key("test_block:goo") == Some(still));
        assert!(still.matches_type(flowing));
        assert!(flowing.matches_type(still));
        assert!(!still.matches_type(&Fluid::WATER));
        assert!(still.to_flowing() == flowing);
        assert!(flowing.to_still() == still);
        assert!(Fluid::FLOWING_WATER.to_still() == &Fluid::WATER);

        let (fluid, state) = Fluid::dynamic_fluid_state(source_state).unwrap();
        assert!(fluid == flowing);
        assert!(state.is_source && state.is_still && !state.falling);
        assert_eq!(state.level, 8);
        let (fluid, state) = Fluid::dynamic_fluid_state(flowing_state).unwrap();
        assert!(fluid == flowing);
        assert!(!state.is_source);

        assert_eq!(
            Fluid::register_dynamic(FluidRegistration {
                placement: Some(FluidPlacement {
                    block: "test_block:goo".to_string(),
                    flowing: false,
                }),
                ..registration("test_block:other")
            })
            .err(),
            Some(FluidRegistrationError::BlockInUse(
                "test_block:goo".to_string()
            )),
        );
    }

    #[test]
    fn vanilla_buckets() {
        assert!(Fluid::from_bucket_item(&Item::WATER_BUCKET) == Some(&Fluid::WATER));
        assert!(Fluid::from_bucket_item(&Item::LAVA_BUCKET) == Some(&Fluid::LAVA));
        assert!(Fluid::from_bucket_item(&Item::BUCKET) == None);
        assert_eq!(Fluid::WATER.bucket_item(), Some(&Item::WATER_BUCKET));
        assert_eq!(Fluid::FLOWING_LAVA.bucket_item(), Some(&Item::LAVA_BUCKET));
        assert_eq!(Fluid::EMPTY.bucket_item(), None);
    }
}
