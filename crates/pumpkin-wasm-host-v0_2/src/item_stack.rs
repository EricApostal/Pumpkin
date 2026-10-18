use super::AccessorExt;
use crate::pumpkin::plugin::item_stack::HostItemStackWithStore;

use crate::pumpkin::plugin::attributes::{
    Attribute as WitAttribute, AttributeModifier as WitAttributeModifier,
    ModifierOperation as WitModifierOperation,
};
use crate::pumpkin::plugin::data_components::DataComponent as WitDataComponent;
use crate::pumpkin::plugin::enchantments::Enchantment as WitEnchantment;
use crate::pumpkin::plugin::item_stack::{
    CustomEnchantmentValue as WitCustomEnchantmentValue,
    DataComponentValue as WitDataComponentValue, EnchantmentValue as WitEnchantmentValue,
    Host as ItemStackInterfaceHost, HostItemStack,
    ItemAttributeModifier as WitItemAttributeModifier, ItemStack as ItemStackHandle,
};
use crate::pumpkin::plugin::text::TextComponent as WitTextComponent;
use pumpkin_wasm_host_common::state::PluginHostState;
use std::sync::Arc;
use tokio::sync::Mutex;
use wasmtime::component::{Accessor, HasSelf, Resource};

use super::common::{WitNbtTree, from_wit_nbt_tree, to_wit_nbt_tree};
use pumpkin_data::Enchantment;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::data_component::DataComponent;
use pumpkin_data::data_component_impl::DataComponentImpl;
use pumpkin_data::data_component_impl::combat::{Modifier, Operation};
use pumpkin_data::data_component_impl::{
    AttributeModifiersImpl, CustomNameImpl, EnchantmentsImpl, LoreImpl,
};
use pumpkin_data::enchantment_registry::enchantment_from_key;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_protocol::codec::data_component::{deserialize, serialize};
use std::borrow::Cow;

pub(crate) fn to_wit_data_component(id: DataComponent) -> WitDataComponent {
    // SAFETY: WIT enum is generated in the same order as the internal enum
    unsafe { std::mem::transmute(id as u8) }
}

pub(crate) fn from_wit_data_component(id: WitDataComponent) -> DataComponent {
    // SAFETY: WIT enum is generated in the same order as the internal enum
    unsafe { std::mem::transmute(id as u8) }
}

pub(crate) fn to_wit_enchantment(id: &Enchantment) -> WitEnchantment {
    // SAFETY: WIT enum is generated in the same order as the internal enum
    unsafe { std::mem::transmute(id.id) }
}

pub(crate) fn from_wit_enchantment(id: WitEnchantment) -> &'static Enchantment {
    // Safety: WIT enum is generated in the same order as the internal enum
    Enchantment::from_id(id as u8).expect("valid enchantment ID")
}

/// The enchantments in the enchantments component, vanilla and plugin ones.
fn enchantment_list(
    stack: &pumpkin_data::item_stack::ItemStack,
) -> Vec<(&'static Enchantment, i32)> {
    stack
        .get_data_component::<EnchantmentsImpl>()
        .map(|e| e.enchantment.clone().into_owned())
        .unwrap_or_default()
}

fn set_enchantment_list(
    stack: &mut pumpkin_data::item_stack::ItemStack,
    encs: Vec<(&'static Enchantment, i32)>,
) {
    let component = Some(
        EnchantmentsImpl {
            enchantment: Cow::from(encs),
        }
        .to_dyn(),
    );
    if let Some((_, data)) = stack
        .patch
        .iter_mut()
        .find(|(id, _)| *id == DataComponent::Enchantments)
    {
        *data = component;
    } else {
        stack.patch.push((DataComponent::Enchantments, component));
    }
}

/// The level of an enchantment in the component, by vanilla or plugin key.
fn component_level(stack: &pumpkin_data::item_stack::ItemStack, key: &str) -> Option<i32> {
    let enchantment = enchantment_from_key(key)?;
    stack
        .get_data_component::<EnchantmentsImpl>()?
        .enchantment
        .iter()
        .find(|(e, _)| e.id == enchantment.id)
        .map(|(_, level)| *level)
}

#[must_use]
pub fn to_wit_attribute(attr: &Attributes) -> WitAttribute {
    // SAFETY: WIT enum is generated in the same order as the internal ID
    unsafe { std::mem::transmute(attr.id) }
}

#[must_use]
pub const fn from_wit_item_operation(op: WitModifierOperation) -> Operation {
    match op {
        WitModifierOperation::Add => Operation::AddValue,
        WitModifierOperation::MultiplyBase => Operation::AddMultipliedBase,
        WitModifierOperation::MultiplyTotal => Operation::AddMultipliedTotal,
    }
}

#[must_use]
pub const fn to_wit_item_operation(op: Operation) -> WitModifierOperation {
    match op {
        Operation::AddValue => WitModifierOperation::Add,
        Operation::AddMultipliedBase => WitModifierOperation::MultiplyBase,
        Operation::AddMultipliedTotal => WitModifierOperation::MultiplyTotal,
    }
}

impl ItemStackInterfaceHost for PluginHostState {}

impl HostItemStack for PluginHostState {
    fn new(
        &mut self,
        registry_key: String,
        count: u8,
    ) -> wasmtime::Result<Resource<ItemStackHandle>> {
        let item = pumpkin_data::item::Item::from_registry_key(
            registry_key
                .strip_prefix("minecraft:")
                .unwrap_or(&registry_key),
        )
        .unwrap_or(&pumpkin_data::item::Item::AIR);
        let stack = pumpkin_data::item_stack::ItemStack::new(count, item);
        self.add(Arc::new(Mutex::new(stack)))
    }

    fn drop(&mut self, rep: Resource<ItemStackHandle>) -> wasmtime::Result<()> {
        self.drop(rep)
    }
}

impl HostItemStackWithStore<PluginHostState> for HasSelf<PluginHostState> {
    async fn get_registry_key(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
    ) -> wasmtime::Result<String> {
        let stack = accessor.get_res(&res)?;
        let stack = stack.lock().await;
        Ok(stack.item.registry_key.to_string())
    }

    async fn get_count(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
    ) -> wasmtime::Result<u8> {
        let stack = accessor.get_res(&res)?;
        let stack = stack.lock().await;
        Ok(stack.item_count)
    }

    async fn set_count(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        count: u8,
    ) -> wasmtime::Result<()> {
        let stack = accessor.get_res(&res)?;
        let mut stack = stack.lock().await;
        stack.item_count = count;
        Ok(())
    }

    async fn get_max_count(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
    ) -> wasmtime::Result<u8> {
        let stack = accessor.get_res(&res)?;
        let stack = stack.lock().await;
        // Search in components for MaxStackSize
        if let Some((_, data)) = stack
            .item
            .components
            .iter()
            .find(|(id, _)| *id == DataComponent::MaxStackSize)
            && let Some(max_size) = data
                .as_any()
                .downcast_ref::<pumpkin_data::data_component_impl::MaxStackSizeImpl>()
        {
            return Ok(max_size.size);
        }
        Ok(64) // Default
    }

    async fn get_enchantments(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
    ) -> wasmtime::Result<Vec<WitEnchantmentValue>> {
        let stack = accessor.get_res(&res)?;
        let stack = stack.lock().await;
        let mut enchantments = Vec::new();
        if let Some((_, Some(data))) = stack
            .patch
            .iter()
            .find(|(id, _)| *id == DataComponent::Enchantments)
            && let Some(enc_impl) = data.as_any().downcast_ref::<EnchantmentsImpl>()
        {
            for (enc, level) in enc_impl.enchantment.iter() {
                // Plugin enchantments have no variant in the WIT enum.
                if usize::from(enc.id) >= Enchantment::ALL.len() {
                    continue;
                }
                enchantments.push(WitEnchantmentValue {
                    enchantment: to_wit_enchantment(enc),
                    level: *level as u32,
                });
            }
        }
        Ok(enchantments)
    }

    async fn add_enchantment(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        enchantment: WitEnchantment,
        level: u32,
    ) -> wasmtime::Result<()> {
        let stack = accessor.get_res(&res)?;
        let mut stack = stack.lock().await;
        let enc = from_wit_enchantment(enchantment);

        let mut current_encs = if let Some((_, Some(data))) = stack
            .patch
            .iter()
            .find(|(id, _)| *id == DataComponent::Enchantments)
        {
            data.as_any()
                .downcast_ref::<EnchantmentsImpl>()
                .map(|e| e.enchantment.clone().into_owned())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        current_encs.retain(|(e, _)| e.id != enc.id);
        current_encs.push((enc, level as i32));

        if let Some((_, data)) = stack
            .patch
            .iter_mut()
            .find(|(id, _)| *id == DataComponent::Enchantments)
        {
            *data = Some(Box::new(EnchantmentsImpl {
                enchantment: Cow::from(current_encs),
            }));
        } else {
            stack.patch.push((
                DataComponent::Enchantments,
                Some(Box::new(EnchantmentsImpl {
                    enchantment: Cow::from(current_encs),
                })),
            ));
        }
        Ok(())
    }

    async fn remove_enchantment(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        enchantment: WitEnchantment,
    ) -> wasmtime::Result<()> {
        let stack = accessor.get_res(&res)?;
        let mut stack = stack.lock().await;
        let enc = from_wit_enchantment(enchantment);

        if let Some((_, Some(data))) = stack
            .patch
            .iter_mut()
            .find(|(id, _)| *id == DataComponent::Enchantments)
            && let Some(enc_impl) = data.as_mut_any().downcast_mut::<EnchantmentsImpl>()
        {
            let mut encs = enc_impl.enchantment.clone().into_owned();
            encs.retain(|(e, _)| e.id != enc.id);
            enc_impl.enchantment = Cow::from(encs);
        }
        Ok(())
    }

    async fn get_enchantment_level(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        enchantment_key: String,
    ) -> wasmtime::Result<u32> {
        let stack = accessor.get_res(&res)?;
        let stack = stack.lock().await;
        Ok(component_level(&stack, &enchantment_key).map_or(0, |level| level.max(0) as u32))
    }

    async fn get_enchantment_effect(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        component_key: String,
    ) -> wasmtime::Result<Option<WitNbtTree>> {
        let key = if component_key.contains(':') {
            component_key
        } else {
            format!("minecraft:{component_key}")
        };
        let stack = accessor.get_res(&res)?;
        let levels: Vec<(&'static Enchantment, i32)> = {
            let stack = stack.lock().await;
            stack
                .get_data_component::<EnchantmentsImpl>()
                .map(|e| e.enchantment.iter().map(|(e, l)| (*e, *l)).collect())
                .unwrap_or_default()
        };
        let server = accessor.server()?;
        let mut out = Vec::new();
        for (enc, level) in levels {
            for (effect_key, value) in server.enchantment_manager.effects_of(enc.name).await {
                if effect_key == key {
                    let mut entry = pumpkin_nbt::compound::NbtCompound::new();
                    entry.put_string("enchantment", enc.name.to_string());
                    entry.put_int("level", level);
                    entry.put("value", value);
                    out.push(NbtTag::Compound(entry));
                }
            }
        }
        Ok((!out.is_empty()).then(|| to_wit_nbt_tree(NbtTag::List(out))))
    }

    async fn get_custom_enchantments(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
    ) -> wasmtime::Result<Vec<WitCustomEnchantmentValue>> {
        let stack = accessor.get_res(&res)?;
        let stack = stack.lock().await;
        let mut result = Vec::new();

        if let Some(enc_impl) = stack.get_data_component::<EnchantmentsImpl>() {
            for (enc, level) in enc_impl.enchantment.iter() {
                result.push(WitCustomEnchantmentValue {
                    enchantment_id: enc.name.to_string(),
                    level: (*level).max(1) as u32,
                });
            }
        }

        // Levels stored before enchantments were real components, or for unregistered ids.
        if let Some(compound) = stack.custom_data_compound()
            && let Some(pumpkin_encs) = compound
                .get("pumpkin:enchantments")
                .and_then(NbtTag::extract_compound)
        {
            for (k, v) in &pumpkin_encs.child_tags {
                if let Some(lvl) = v.extract_int()
                    && !result.iter().any(|e| e.enchantment_id == k.as_ref())
                {
                    result.push(WitCustomEnchantmentValue {
                        enchantment_id: k.to_string(),
                        level: (lvl.max(1)) as u32,
                    });
                }
            }
        }

        Ok(result)
    }

    async fn add_custom_enchantment(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        enchantment_id: String,
        level: u32,
    ) -> wasmtime::Result<()> {
        let stack = accessor.get_res(&res)?;
        let mut stack = stack.lock().await;

        let Some(enchantment) = enchantment_from_key(&enchantment_id) else {
            // Not a registered enchantment, keep the level where it was kept before.
            stack.set_custom_data(
                "pumpkin:enchantments",
                &enchantment_id,
                NbtTag::Int(level as i32),
            );
            return Ok(());
        };
        let mut encs = enchantment_list(&stack);
        encs.retain(|(e, _)| e.id != enchantment.id);
        encs.push((enchantment, level as i32));
        set_enchantment_list(&mut stack, encs);
        Ok(())
    }

    async fn remove_custom_enchantment(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        enchantment_id: String,
    ) -> wasmtime::Result<()> {
        let stack = accessor.get_res(&res)?;
        let mut stack = stack.lock().await;

        stack.remove_custom_data("pumpkin:enchantments", &enchantment_id);

        if let Some(enchantment) = enchantment_from_key(&enchantment_id) {
            let mut encs = enchantment_list(&stack);
            let before = encs.len();
            encs.retain(|(e, _)| e.id != enchantment.id);
            if encs.len() != before {
                set_enchantment_list(&mut stack, encs);
            }
        }
        Ok(())
    }

    async fn get_custom_enchantment_level(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        enchantment_id: String,
    ) -> wasmtime::Result<Option<u32>> {
        let stack = accessor.get_res(&res)?;
        let stack = stack.lock().await;

        if let Some(level) = component_level(&stack, &enchantment_id) {
            return Ok(Some(level.max(1) as u32));
        }
        if let Some(NbtTag::Int(level)) =
            stack.get_custom_data("pumpkin:enchantments", &enchantment_id)
        {
            return Ok(Some(level.max(1) as u32));
        }
        Ok(None)
    }

    async fn has_custom_enchantment(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        enchantment_id: String,
    ) -> wasmtime::Result<bool> {
        let stack = accessor.get_res(&res)?;
        let stack = stack.lock().await;

        Ok(component_level(&stack, &enchantment_id).is_some()
            || stack.has_custom_data("pumpkin:enchantments", &enchantment_id))
    }

    async fn get_attribute_modifiers(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
    ) -> wasmtime::Result<Vec<WitItemAttributeModifier>> {
        let stack = accessor.get_res(&res)?;
        let stack = stack.lock().await;
        let mut modifiers = Vec::new();
        if let Some(comp) = stack.get_data_component::<AttributeModifiersImpl>() {
            for m in comp.attribute_modifiers.iter() {
                modifiers.push(WitItemAttributeModifier {
                    attribute: to_wit_attribute(m.r#type),
                    modifier: WitAttributeModifier {
                        id: m.id.to_string(),
                        amount: m.amount,
                        operation: to_wit_item_operation(m.operation),
                    },
                    slot: super::enchantment::to_wit_slot(&m.slot),
                });
            }
        }
        Ok(modifiers)
    }

    async fn add_attribute_modifier(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        modifier: WitItemAttributeModifier,
    ) -> wasmtime::Result<()> {
        let stack = accessor.get_res(&res)?;
        let mut stack = stack.lock().await;
        let attr = super::living_entity::from_wit_attribute(modifier.attribute);
        let slot = super::enchantment::to_data_slot(modifier.slot);
        let op = from_wit_item_operation(modifier.modifier.operation);
        let leaked_id: &'static str = Box::leak(modifier.modifier.id.into_boxed_str());

        let mut current_mods = stack
            .get_data_component::<AttributeModifiersImpl>()
            .map_or_else(Vec::new, |comp| {
                comp.attribute_modifiers.clone().into_owned()
            });

        current_mods.retain(|m| !(m.r#type == attr && m.id == leaked_id && m.slot == slot));
        current_mods.push(Modifier {
            r#type: attr,
            id: leaked_id,
            amount: modifier.modifier.amount,
            operation: op,
            slot,
        });

        stack.set_data_component(AttributeModifiersImpl {
            attribute_modifiers: Cow::Owned(current_mods),
        });

        Ok(())
    }

    async fn remove_attribute_modifiers(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        attribute: WitAttribute,
    ) -> wasmtime::Result<()> {
        let stack = accessor.get_res(&res)?;
        let mut stack = stack.lock().await;
        let attr = super::living_entity::from_wit_attribute(attribute);

        if let Some(comp) = stack.get_data_component::<AttributeModifiersImpl>() {
            let mut current_mods = comp.attribute_modifiers.clone().into_owned();
            current_mods.retain(|m| m.r#type != attr);
            if current_mods.is_empty() {
                stack
                    .patch
                    .retain(|(id, _)| *id != DataComponent::AttributeModifiers);
            } else {
                stack.set_data_component(AttributeModifiersImpl {
                    attribute_modifiers: Cow::Owned(current_mods),
                });
            }
        }

        Ok(())
    }

    async fn clear_attribute_modifiers(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
    ) -> wasmtime::Result<()> {
        let stack = accessor.get_res(&res)?;
        let mut stack = stack.lock().await;
        stack
            .patch
            .retain(|(id, _)| *id != DataComponent::AttributeModifiers);
        Ok(())
    }

    async fn get_lore(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
    ) -> wasmtime::Result<Vec<Resource<WitTextComponent>>> {
        let stack = accessor.get_res(&res)?;
        let lines = {
            let stack = stack.lock().await;
            stack
                .get_data_component::<LoreImpl>()
                .map_or_else(Vec::new, |lore| lore.lines.clone())
        };

        lines
            .into_iter()
            .map(|line| accessor.add_res(line))
            .collect()
    }

    async fn set_lore(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        lore: Vec<Resource<WitTextComponent>>,
    ) -> wasmtime::Result<()> {
        let lore = lore
            .into_iter()
            .map(|line| accessor.take_res(line))
            .collect::<Result<_, _>>()?;
        let stack = accessor.get_res(&res)?;
        stack.lock().await.set_lore(lore);
        Ok(())
    }

    async fn add_lore(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        line: Resource<WitTextComponent>,
    ) -> wasmtime::Result<()> {
        let line = accessor.take_res(line)?;
        let stack = accessor.get_res(&res)?;
        stack.lock().await.add_lore(line);
        Ok(())
    }

    async fn get_custom_name(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
    ) -> wasmtime::Result<Option<Resource<WitTextComponent>>> {
        let stack = accessor.get_res(&res)?.clone();
        let stack = stack.lock().await;
        if let Some((_, Some(data))) = stack
            .patch
            .iter()
            .find(|(id, _)| *id == DataComponent::CustomName)
            && let Some(name_impl) = data.as_any().downcast_ref::<CustomNameImpl>()
        {
            return Ok(Some(accessor.add_res(name_impl.name.clone())?));
        }
        Ok(None)
    }

    async fn set_custom_name(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        name: Option<Resource<WitTextComponent>>,
    ) -> wasmtime::Result<()> {
        let name = name.map(|r| accessor.take_res(r)).transpose()?;
        let stack = accessor.get_res(&res)?;
        let mut stack = stack.lock().await;
        if let Some(name) = name {
            if let Some((_, data)) = stack
                .patch
                .iter_mut()
                .find(|(id, _)| *id == DataComponent::CustomName)
            {
                *data = Some(Box::new(CustomNameImpl { name }));
            } else {
                stack.patch.push((
                    DataComponent::CustomName,
                    Some(Box::new(CustomNameImpl { name })),
                ));
            }
        } else {
            stack
                .patch
                .retain(|(id, _)| *id != DataComponent::CustomName);
        }
        Ok(())
    }

    async fn set_custom_data(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        namespace: String,
        key: String,
        value: WitNbtTree,
    ) -> wasmtime::Result<()> {
        let stack = accessor.get_res(&res)?;
        let mut stack = stack.lock().await;
        let value = from_wit_nbt_tree(&value).map_err(wasmtime::Error::msg)?;
        stack.set_custom_data(&namespace, &key, value);
        Ok(())
    }

    async fn get_custom_data(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        namespace: String,
        key: String,
    ) -> wasmtime::Result<Option<WitNbtTree>> {
        let stack = accessor.get_res(&res)?;
        let stack = stack.lock().await;
        Ok(stack.get_custom_data(&namespace, &key).map(to_wit_nbt_tree))
    }

    async fn remove_custom_data(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        namespace: String,
        key: String,
    ) -> wasmtime::Result<()> {
        let stack = accessor.get_res(&res)?;
        let mut stack = stack.lock().await;
        stack.remove_custom_data(&namespace, &key);
        Ok(())
    }

    async fn has_custom_data(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        namespace: String,
        key: String,
    ) -> wasmtime::Result<bool> {
        let stack = accessor.get_res(&res)?;
        let stack = stack.lock().await;
        Ok(stack.has_custom_data(&namespace, &key))
    }

    async fn get_components(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
    ) -> wasmtime::Result<Vec<WitDataComponentValue>> {
        let stack = accessor.get_res(&res)?;
        let stack = stack.lock().await;
        let mut components = Vec::new();
        for (id, data) in &stack.patch {
            // Components of plugin registered types have no entry in the component enum, they are
            // read with `get-custom-component`.
            if id.plugin_slot().is_some() {
                continue;
            }
            if let Some(data) = data {
                let mut buf = Vec::new();
                if serialize(*id, data.as_ref(), &mut buf).is_ok() {
                    components.push(WitDataComponentValue {
                        component: to_wit_data_component(*id),
                        value: buf,
                    });
                }
            }
        }
        Ok(components)
    }

    async fn set_component(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        component: WitDataComponent,
        value: Vec<u8>,
    ) -> wasmtime::Result<()> {
        let stack = accessor.get_res(&res)?;
        let mut stack = stack.lock().await;
        let id = from_wit_data_component(component);
        let mut cursor = std::io::Cursor::new(value);

        if let Ok(component_impl) = deserialize(id, &mut cursor) {
            if let Some((_, data)) = stack.patch.iter_mut().find(|(pid, _)| *pid == id) {
                *data = Some(component_impl);
            } else {
                stack.patch.push((id, Some(component_impl)));
            }
        }
        Ok(())
    }

    async fn remove_component(
        accessor: &Accessor<PluginHostState, Self>,
        res: Resource<ItemStackHandle>,
        component: WitDataComponent,
    ) -> wasmtime::Result<()> {
        let stack = accessor.get_res(&res)?;
        let mut stack = stack.lock().await;
        let id = from_wit_data_component(component);
        stack.patch.retain(|(pid, _)| *pid != id);
        Ok(())
    }
}
