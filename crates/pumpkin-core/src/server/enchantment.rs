use pumpkin_data::enchantment::AttributeModifierSlot;
use pumpkin_data::enchantment_registry::{
    self, EnchantmentRegistration, enchantment_effect_type_id,
};
use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_data::registry::RegistryEntryData;
use pumpkin_nbt::deserializer::NbtReadHelperJava;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_nbt::{Nbt, compound::NbtCompound};
use pumpkin_util::text::TextComponent;
use rustc_hash::FxHashMap;
use std::io::Cursor;
use tokio::sync::RwLock;

#[derive(Clone, Debug)]
pub struct CustomEnchantmentEntry {
    pub id: String,
    pub description: TextComponent,
    pub max_level: u32,
    pub anvil_cost: u32,
    pub supported_items: String,
    pub weight: u32,
    pub slots: Vec<AttributeModifierSlot>,
    pub exclusive_set: Vec<String>,
}

/// The parts of the client facing definition that `CustomEnchantmentEntry` does not have.
#[derive(Clone, Debug)]
pub struct EnchantmentExtras {
    /// `(base, per level above first)`.
    pub min_cost: (i32, i32),
    pub max_cost: (i32, i32),
    /// Effect component key and its value as network NBT, in the order they were set.
    pub effects: Vec<(String, NbtTag)>,
}

impl Default for EnchantmentExtras {
    fn default() -> Self {
        Self {
            min_cost: (1, 10),
            max_cost: (21, 10),
            effects: Vec::new(),
        }
    }
}

pub struct EnchantmentManager {
    custom_enchantments: RwLock<FxHashMap<String, CustomEnchantmentEntry>>,
    extras: RwLock<FxHashMap<String, EnchantmentExtras>>,
}

impl Default for EnchantmentManager {
    fn default() -> Self {
        Self::new()
    }
}

impl EnchantmentManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            custom_enchantments: RwLock::new(FxHashMap::default()),
            extras: RwLock::new(FxHashMap::default()),
        }
    }

    pub async fn register(&self, enchantment: CustomEnchantmentEntry) -> Result<(), String> {
        let mut map = self.custom_enchantments.write().await;
        if map.contains_key(&enchantment.id) {
            return Err(format!(
                "Enchantment '{}' is already registered",
                enchantment.id
            ));
        }
        // Gives it the registry id clients get, fails once clients can connect.
        enchantment_registry::register_plugin_enchantment(
            &enchantment.id,
            EnchantmentRegistration {
                max_level: enchantment.max_level.max(1) as i32,
                anvil_cost: enchantment.anvil_cost,
                weight: enchantment.weight.max(1) as i32,
                slots: enchantment.slots.clone(),
            },
        )
        .map_err(|err| {
            format!(
                "Enchantment '{}' cannot be registered: {err}",
                enchantment.id
            )
        })?;
        map.insert(enchantment.id.clone(), enchantment);
        Ok(())
    }

    /// Sets the effect components of a registered enchantment (`effects` of the registry data).
    /// Each value is network NBT of the component value, the key a vanilla or plugin
    /// registered effect component type.
    pub async fn set_effects(
        &self,
        id: &str,
        effects: Vec<(String, Vec<u8>)>,
    ) -> Result<(), String> {
        let mut parsed = Vec::with_capacity(effects.len());
        for (key, bytes) in effects {
            if enchantment_effect_type_id(&key).is_none() {
                return Err(format!(
                    "'{key}' is not an enchantment effect component type"
                ));
            }
            let key = if key.contains(':') {
                key
            } else {
                format!("minecraft:{key}")
            };
            let mut reader = NbtReadHelperJava::new(Cursor::new(bytes.as_slice()));
            let tag = NbtTag::deserialize(&mut reader)
                .map_err(|err| format!("the value of '{key}' is not valid NBT: {err}"))?;
            parsed.push((key, tag));
        }
        if !self.has(id).await {
            return Err(format!("Enchantment '{id}' is not registered"));
        }
        self.extras
            .write()
            .await
            .entry(id.to_string())
            .or_default()
            .effects = parsed;
        Ok(())
    }

    /// Sets the enchanting costs of a registered enchantment as `(base, per level above first)`.
    pub async fn set_costs(
        &self,
        id: &str,
        min_cost: (i32, i32),
        max_cost: (i32, i32),
    ) -> Result<(), String> {
        if !self.has(id).await {
            return Err(format!("Enchantment '{id}' is not registered"));
        }
        let mut extras = self.extras.write().await;
        let entry = extras.entry(id.to_string()).or_default();
        entry.min_cost = min_cost;
        entry.max_cost = max_cost;
        Ok(())
    }

    /// The effect components of an enchantment.
    pub async fn effects_of(&self, id: &str) -> Vec<(String, NbtTag)> {
        self.extras
            .read()
            .await
            .get(id)
            .map(|extras| extras.effects.clone())
            .unwrap_or_default()
    }

    /// The `minecraft:enchantment` registry entries of the plugin enchantments, in id order. They
    /// go right after the vanilla entries, which is where their ids point to.
    pub async fn registry_entries(&self) -> Vec<RegistryEntryData> {
        let map = self.custom_enchantments.read().await;
        let extras = self.extras.read().await;
        let mut out = Vec::new();
        for plugin in enchantment_registry::plugin_enchantments() {
            let Some(entry) = map.get(plugin.key) else {
                continue;
            };
            let extra = extras.get(plugin.key).cloned().unwrap_or_default();
            let mut data = NbtCompound::new();
            data.put(
                "description",
                entry
                    .description
                    .to_nbt_tag_for_version(&CURRENT_MC_VERSION),
            );
            if !entry.exclusive_set.is_empty() {
                data.put("exclusive_set", holder_set(&entry.exclusive_set));
            }
            data.put(
                "supported_items",
                holder_set(&split_ids(&entry.supported_items)),
            );
            data.put_int("weight", entry.weight.max(1) as i32);
            data.put_int("max_level", entry.max_level.max(1) as i32);
            data.put("min_cost", cost_tag(extra.min_cost));
            data.put("max_cost", cost_tag(extra.max_cost));
            data.put_int("anvil_cost", entry.anvil_cost as i32);
            data.put(
                "slots",
                NbtTag::List(
                    entry
                        .slots
                        .iter()
                        .map(|slot| NbtTag::String(slot_name(slot).into()))
                        .collect(),
                ),
            );
            if !extra.effects.is_empty() {
                let mut effects = NbtCompound::new();
                for (key, value) in extra.effects {
                    effects.put(&key, value);
                }
                data.put("effects", NbtTag::Compound(effects));
            }
            out.push(RegistryEntryData {
                entry_id: plugin.key.to_string(),
                data: Some(Nbt::from(data).write_unnamed().to_vec().into_boxed_slice()),
            });
        }
        out
    }

    pub async fn get(&self, id: &str) -> Option<CustomEnchantmentEntry> {
        let map = self.custom_enchantments.read().await;
        map.get(id).cloned()
    }

    pub async fn has(&self, id: &str) -> bool {
        let map = self.custom_enchantments.read().await;
        map.contains_key(id)
    }

    pub async fn get_all_ids(&self) -> Vec<String> {
        let map = self.custom_enchantments.read().await;
        map.keys().cloned().collect()
    }
}

fn split_ids(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn with_namespace(id: &str) -> String {
    if id.contains(':') {
        id.to_string()
    } else {
        format!("minecraft:{id}")
    }
}

/// A holder set: one `#tag` as a string, otherwise a list of ids.
fn holder_set(ids: &[String]) -> NbtTag {
    if let [single] = ids
        && let Some(tag) = single.strip_prefix('#')
    {
        return NbtTag::String(format!("#{}", with_namespace(tag)).into());
    }
    NbtTag::List(
        ids.iter()
            .filter(|id| !id.starts_with('#'))
            .map(|id| NbtTag::String(with_namespace(id).into()))
            .collect(),
    )
}

fn cost_tag((base, per_level): (i32, i32)) -> NbtTag {
    let mut cost = NbtCompound::new();
    cost.put_int("base", base);
    cost.put_int("per_level_above_first", per_level);
    NbtTag::Compound(cost)
}

const fn slot_name(slot: &AttributeModifierSlot) -> &'static str {
    match slot {
        AttributeModifierSlot::Any => "any",
        AttributeModifierSlot::MainHand => "mainhand",
        AttributeModifierSlot::OffHand => "offhand",
        AttributeModifierSlot::Hand => "hand",
        AttributeModifierSlot::Feet => "feet",
        AttributeModifierSlot::Legs => "legs",
        AttributeModifierSlot::Chest => "chest",
        AttributeModifierSlot::Head => "head",
        AttributeModifierSlot::Armor => "armor",
        AttributeModifierSlot::Body => "body",
        AttributeModifierSlot::Saddle => "saddle",
    }
}
