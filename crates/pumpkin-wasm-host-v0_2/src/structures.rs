//! Structure templates for `structures`: loading, accessors, saving from a world, placing.

use std::sync::Arc;

use pumpkin_core::world::structure_place::{load_template, template_to_bytes};
use pumpkin_data::{Mirror, Rotation};
use pumpkin_util::math::position::BlockPos;
use pumpkin_wasm_host_common::state::{FromResource, PluginHostState};
use pumpkin_world::generation::structure::template::{PaletteEntry, StructureTemplate};
use wasmtime::component::{Accessor, HasSelf, Resource};

use super::{AccessorExt, run_blocking};
use crate::bulk::encode_nbt;
use crate::pumpkin::plugin::common::BlockPos as WitBlockPos;
use crate::pumpkin::plugin::structures::{
    Host, HostStructureTemplate, HostWithStore, Mirror as WitMirror,
    PaletteEntry as WitPaletteEntry, Rotation as WitRotation, StructureBlock, StructureEntity,
    StructureTemplate as WitTemplate,
};
use crate::pumpkin::plugin::world::{BlockFlags as WitBlockFlags, World};
use crate::world::from_wit_block_flags;

impl FromResource for WitTemplate {
    type Internal = Arc<StructureTemplate>;
}

const fn rotation(rotation: WitRotation) -> Rotation {
    match rotation {
        WitRotation::None => Rotation::None,
        WitRotation::Clockwise90 => Rotation::Clockwise90,
        WitRotation::Rotate180 => Rotation::Rotate180,
        WitRotation::CounterClockwise90 => Rotation::CounterClockwise90,
    }
}

const fn mirror(mirror: WitMirror) -> Mirror {
    match mirror {
        WitMirror::None => Mirror::None,
        WitMirror::LeftRight => Mirror::LeftRight,
        WitMirror::FrontBack => Mirror::FrontBack,
    }
}

const fn to_wit_pos(pos: pumpkin_util::math::vector3::Vector3<i32>) -> WitBlockPos {
    WitBlockPos {
        x: pos.x,
        y: pos.y,
        z: pos.z,
    }
}

impl Host for PluginHostState {
    fn load_structure_nbt(
        &mut self,
        bytes: Vec<u8>,
    ) -> wasmtime::Result<Result<Resource<WitTemplate>, String>> {
        match load_template(&bytes) {
            Ok(template) => Ok(Ok(self.add(Arc::new(template))?)),
            Err(error) => Ok(Err(error)),
        }
    }
}

impl HostStructureTemplate for PluginHostState {
    fn get_size(&mut self, res: Resource<WitTemplate>) -> wasmtime::Result<WitBlockPos> {
        Ok(to_wit_pos(self.get(&res)?.size))
    }

    fn get_author(&mut self, res: Resource<WitTemplate>) -> wasmtime::Result<String> {
        Ok(self.get(&res)?.get_author().to_string())
    }

    fn get_palette(
        &mut self,
        res: Resource<WitTemplate>,
    ) -> wasmtime::Result<Vec<WitPaletteEntry>> {
        Ok(palette_of(self.get(&res)?)
            .into_iter()
            .map(|entry| WitPaletteEntry {
                name: entry.name,
                properties: entry.properties,
            })
            .collect())
    }

    fn get_blocks(&mut self, res: Resource<WitTemplate>) -> wasmtime::Result<Vec<StructureBlock>> {
        let template = self.get(&res)?;
        let palette = palette_of(template);
        let Some(first) = template.palettes.first() else {
            return Ok(Vec::new());
        };
        Ok(first
            .blocks()
            .iter()
            .map(|block| StructureBlock {
                pos: to_wit_pos(block.pos),
                palette_index: palette
                    .iter()
                    .position(|entry| *entry == block.state)
                    .unwrap_or_default() as u32,
                nbt: block.nbt.clone().map(encode_nbt),
            })
            .collect())
    }

    fn get_entities(
        &mut self,
        res: Resource<WitTemplate>,
    ) -> wasmtime::Result<Vec<StructureEntity>> {
        Ok(self
            .get(&res)?
            .entity_info_list
            .iter()
            .map(|entity| StructureEntity {
                pos: (entity.pos.x, entity.pos.y, entity.pos.z),
                block_pos: to_wit_pos(entity.block_pos),
                nbt: encode_nbt(entity.nbt.clone()),
            })
            .collect())
    }

    fn to_bytes(
        &mut self,
        res: Resource<WitTemplate>,
        compress: bool,
    ) -> wasmtime::Result<Result<Vec<u8>, String>> {
        Ok(template_to_bytes(self.get(&res)?, compress))
    }

    fn drop(&mut self, rep: Resource<WitTemplate>) -> wasmtime::Result<()> {
        self.drop(rep)
    }
}

/// The distinct states of the first palette, in first-use order.
fn palette_of(template: &StructureTemplate) -> Vec<PaletteEntry> {
    let mut entries: Vec<PaletteEntry> = Vec::new();
    if let Some(first) = template.palettes.first() {
        for block in first.blocks() {
            if !entries.contains(&block.state) {
                entries.push(block.state.clone());
            }
        }
    }
    entries
}

impl HostWithStore<PluginHostState> for HasSelf<PluginHostState> {
    async fn save_structure_from_world(
        accessor: &Accessor<PluginHostState, Self>,
        target: Resource<World>,
        min: WitBlockPos,
        max: WitBlockPos,
        include_entities: bool,
    ) -> wasmtime::Result<Result<Vec<u8>, String>> {
        let world = accessor.get_res(&target)?;
        let min = BlockPos::new(min.x, min.y, min.z);
        let max = BlockPos::new(max.x, max.y, max.z);
        run_blocking(accessor, move || {
            let template = world.capture_template(min, max, include_entities)?;
            template_to_bytes(&template, true)
        })
        .await
    }

    async fn place_structure(
        accessor: &Accessor<PluginHostState, Self>,
        template: Resource<WitTemplate>,
        target: Resource<World>,
        origin: WitBlockPos,
        rotation_arg: WitRotation,
        mirror_arg: WitMirror,
        flags: WitBlockFlags,
        ignore_air: bool,
        integrity: Option<f32>,
    ) -> wasmtime::Result<Result<u32, String>> {
        let template = accessor.get_res(&template)?;
        let world = accessor.get_res(&target)?;
        let origin = BlockPos::new(origin.x, origin.y, origin.z);
        let flags = from_wit_block_flags(flags);
        let rotation = rotation(rotation_arg);
        let mirror = mirror(mirror_arg);
        run_blocking(accessor, move || {
            world.place_template_bulk(
                &template, origin, rotation, mirror, flags, ignore_air, integrity,
            )
        })
        .await
    }
}
