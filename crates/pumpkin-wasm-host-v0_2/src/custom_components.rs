use crate::pumpkin::plugin::custom_components::{CodecOp, ComponentTypeEntry, Host};
use crate::pumpkin::plugin::item_stack::ItemStack as ItemStackHandle;
use pumpkin_core::plugin::permissions;
use pumpkin_data::data_component::DataComponent;
use pumpkin_data::data_component_impl::DataComponentImpl;
use pumpkin_data::data_component_registry::{
    ComponentCodec, MAX_CODEC_DEPTH, MAX_CODEC_NODES, PluginComponentImpl,
    plugin_component_type_by_key, plugin_component_types, register_plugin_component_type,
};
use pumpkin_protocol::codec::data_component::plugin_component_from_bytes;
use pumpkin_wasm_host_common::state::PluginHostState;
use tracing::debug;
use wasmtime::component::Resource;

/// Reads one node of a codec in prefix notation, see `custom-components.wit`.
fn parse_codec_node(
    ops: &[CodecOp],
    index: &mut usize,
    depth: usize,
    nodes: &mut usize,
) -> Result<ComponentCodec, String> {
    *nodes += 1;
    if *nodes > MAX_CODEC_NODES {
        return Err(format!("a codec has at most {MAX_CODEC_NODES} nodes"));
    }
    if depth > MAX_CODEC_DEPTH {
        return Err(format!("a codec nests at most {MAX_CODEC_DEPTH} levels"));
    }
    let Some(op) = ops.get(*index) else {
        return Err("the codec ends before its last node is complete".to_string());
    };
    *index += 1;
    Ok(match op {
        CodecOp::VarInt => ComponentCodec::VarInt,
        CodecOp::VarLong => ComponentCodec::VarLong,
        CodecOp::Flag => ComponentCodec::Bool,
        CodecOp::Byte => ComponentCodec::Byte,
        CodecOp::Short => ComponentCodec::Short,
        CodecOp::Int => ComponentCodec::Int,
        CodecOp::Long => ComponentCodec::Long,
        CodecOp::Float32 => ComponentCodec::Float,
        CodecOp::Float64 => ComponentCodec::Double,
        CodecOp::Text => ComponentCodec::String,
        CodecOp::ByteArray => ComponentCodec::ByteArray,
        CodecOp::Uuid => ComponentCodec::Uuid,
        CodecOp::FixedBytes(length) => ComponentCodec::FixedBytes(*length),
        CodecOp::Nbt => ComponentCodec::Nbt,
        CodecOp::Stack => ComponentCodec::ItemStack,
        CodecOp::ComponentPatch => ComponentCodec::ComponentPatch,
        CodecOp::Optional => {
            ComponentCodec::Optional(Box::new(parse_codec_node(ops, index, depth + 1, nodes)?))
        }
        CodecOp::Repeated => {
            ComponentCodec::List(Box::new(parse_codec_node(ops, index, depth + 1, nodes)?))
        }
        CodecOp::Sequence(count) => {
            let count = usize::try_from(*count).unwrap_or(usize::MAX);
            if count > MAX_CODEC_NODES {
                return Err(format!("a codec has at most {MAX_CODEC_NODES} nodes"));
            }
            let mut fields = Vec::with_capacity(count);
            for _ in 0..count {
                fields.push(parse_codec_node(ops, index, depth + 1, nodes)?);
            }
            ComponentCodec::Sequence(fields)
        }
    })
}

/// A codec is one root node and nothing after it.
fn parse_codec(ops: &[CodecOp]) -> Result<ComponentCodec, String> {
    let mut index = 0;
    let codec = parse_codec_node(ops, &mut index, 0, &mut 0)?;
    if index != ops.len() {
        return Err(format!(
            "the codec has {} nodes after its root node",
            ops.len() - index
        ));
    }
    Ok(codec)
}

impl Host for PluginHostState {
    fn register_data_component_type(
        &mut self,
        key: String,
        persistent: bool,
        codec: Vec<CodecOp>,
    ) -> wasmtime::Result<Result<u32, String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::REGISTRY_COMPONENTS)
        {
            return Ok(Err(self.missing_permission(
                "registering data component types",
                permissions::REGISTRY_COMPONENTS,
            )));
        }
        let codec = match parse_codec(&codec) {
            Ok(codec) => codec,
            Err(error) => return Ok(Err(format!("invalid codec for '{key}': {error}"))),
        };
        Ok(
            match register_plugin_component_type(&key, persistent, codec) {
                Ok(component_type) => {
                    debug!(
                        "Plugin {} registered data component type {} with id {}",
                        self.name.as_deref().unwrap_or("<unknown>"),
                        component_type.key,
                        component_type.id()
                    );
                    self.registrations.data_component_type(component_type.key);
                    Ok(u32::from(component_type.id()))
                }
                Err(error) => Err(format!("data component type '{key}': {error}")),
            },
        )
    }

    fn get_data_component_type_id(&mut self, key: String) -> wasmtime::Result<Option<u32>> {
        Ok(plugin_component_type_by_key(&key).map(|component_type| u32::from(component_type.id())))
    }

    fn get_vanilla_data_component_type_count(&mut self) -> wasmtime::Result<u32> {
        Ok(u32::from(DataComponent::VANILLA_COUNT))
    }

    fn get_registered_data_component_types(&mut self) -> wasmtime::Result<Vec<ComponentTypeEntry>> {
        Ok(plugin_component_types()
            .into_iter()
            .map(|component_type| ComponentTypeEntry {
                key: component_type.key.to_string(),
                id: u32::from(component_type.id()),
                persistent: component_type.persistent,
            })
            .collect())
    }

    async fn set_custom_component(
        &mut self,
        stack: Resource<ItemStackHandle>,
        key: String,
        value: Vec<u8>,
    ) -> wasmtime::Result<Result<(), String>> {
        let Some(component_type) = plugin_component_type_by_key(&key) else {
            return Ok(Err(format!("unknown data component type '{key}'")));
        };
        let component = match plugin_component_from_bytes(component_type.component, &value) {
            Ok(component) => component,
            Err(error) => {
                return Ok(Err(format!(
                    "the value does not match the codec of '{key}': {error}"
                )));
            }
        };
        let id = component_type.component;
        let stack = self.get(&stack)?;
        let mut stack = stack.lock().await;
        let value = Some(component.to_dyn());
        if let Some((_, existing)) = stack.patch.iter_mut().find(|(known, _)| *known == id) {
            *existing = value;
        } else {
            stack.patch.push((id, value));
        }
        Ok(Ok(()))
    }

    async fn get_custom_component(
        &mut self,
        stack: Resource<ItemStackHandle>,
        key: String,
    ) -> wasmtime::Result<Option<Vec<u8>>> {
        let Some(component_type) = plugin_component_type_by_key(&key) else {
            return Ok(None);
        };
        let id = component_type.component;
        let stack = self.get(&stack)?;
        let stack = stack.lock().await;
        Ok(stack
            .patch
            .iter()
            .find(|(known, _)| *known == id)
            .and_then(|(_, data)| data.as_ref())
            .and_then(|data| data.as_any().downcast_ref::<PluginComponentImpl>())
            .map(|component| component.data.clone()))
    }

    async fn remove_custom_component(
        &mut self,
        stack: Resource<ItemStackHandle>,
        key: String,
    ) -> wasmtime::Result<Result<(), String>> {
        let Some(component_type) = plugin_component_type_by_key(&key) else {
            return Ok(Err(format!("unknown data component type '{key}'")));
        };
        let id = component_type.component;
        let stack = self.get(&stack)?;
        let mut stack = stack.lock().await;
        stack.patch.retain(|(known, _)| *known != id);
        Ok(Ok(()))
    }
}
