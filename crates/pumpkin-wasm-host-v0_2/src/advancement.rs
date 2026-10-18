use crate::pumpkin::plugin::advancement::{
    AdvancementDisplay as WitAdvancementDisplay, AdvancementInfo as WitAdvancementInfo,
    FrameType as WitFrameType,
};
use pumpkin_data::Advancement;
use pumpkin_data::advancement_data::FrameType;
use pumpkin_wasm_host_common::state::PluginHostState;

#[must_use]
pub fn find_advancement(id: &str) -> Option<&'static Advancement> {
    pumpkin_core::data::runtime_advancement::find(id)
}

#[must_use]
pub const fn to_wasm_frame_type(frame: FrameType) -> WitFrameType {
    match frame {
        FrameType::Task => WitFrameType::Task,
        FrameType::Challenge => WitFrameType::Challenge,
        FrameType::Goal => WitFrameType::Goal,
    }
}

pub fn to_wasm_advancement_info(
    state: &mut PluginHostState,
    advancement: &'static Advancement,
) -> wasmtime::Result<WitAdvancementInfo> {
    let display = if let Some(disp) = advancement.display {
        let title = state.add(disp.get_title())?;
        let description = state.add(disp.get_description())?;
        Some(WitAdvancementDisplay {
            title,
            description,
            frame: to_wasm_frame_type(disp.frame_type),
            show_toast: disp.show_toast,
            hidden: disp.hidden,
            announce_to_chat: disp.announce_to_chat,
            background: disp.background_texture.map(ToString::to_string),
            x: disp.x,
            y: disp.y,
        })
    } else {
        None
    };

    Ok(WitAdvancementInfo {
        id: advancement.id.to_string(),
        parent_id: advancement.parent.as_ref().map(ToString::to_string),
        criteria: advancement
            .criteria
            .iter()
            .map(ToString::to_string)
            .collect(),
        display,
    })
}

impl crate::pumpkin::plugin::advancement::Host for PluginHostState {
    fn register_criterion_trigger(&mut self, key: String) -> wasmtime::Result<Result<(), String>> {
        Ok(pumpkin_core::data::runtime_advancement::register_trigger(
            &key,
        ))
    }

    fn register_advancement(
        &mut self,
        id: String,
        json: String,
    ) -> wasmtime::Result<Result<(), String>> {
        Ok(pumpkin_core::data::runtime_advancement::add_plugin_advancement(&id, &json))
    }

    fn unregister_advancement(&mut self, id: String) -> wasmtime::Result<bool> {
        Ok(pumpkin_core::data::runtime_advancement::remove_plugin_advancement(&id))
    }
}
