//! Structure templates in a live world: loading and saving the vanilla `.nbt` format, capturing
//! an area, and placing a template with rotation and mirroring through the bulk block path.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;

use pumpkin_data::{Block, BlockStateId, Mirror, Rotation};
use pumpkin_nbt::Nbt;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::deserializer::NbtReadHelperJava;
use pumpkin_nbt::nbt_compress::{read_gzip_compound_tag, write_gzip_compound_tag_to_bytes};
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use pumpkin_util::random::{RandomImpl, hash_block_pos, legacy_rand::LegacyRand};
use pumpkin_world::generation::structure::template::{
    BlockStateResolver, Palette, PaletteEntry, StructureBlockInfo, StructureEntityInfo,
    StructureTemplate,
};
use pumpkin_world::world::BlockFlags;

use super::World;
use super::bulk::BlockChange;

/// Most blocks a captured area can have.
pub const MAX_CAPTURE_VOLUME: i64 = 1 << 24;

/// Reads a structure template from `.nbt` bytes, gzip compressed or raw.
pub fn load_template(bytes: &[u8]) -> Result<StructureTemplate, String> {
    let compound = if bytes.starts_with(&[0x1f, 0x8b]) {
        read_gzip_compound_tag(Cursor::new(bytes))
    } else {
        let mut reader = NbtReadHelperJava::new(Cursor::new(bytes));
        Nbt::read(&mut reader).map(|nbt| nbt.root_tag)
    }
    .map_err(|error| format!("invalid structure NBT: {error}"))?;
    StructureTemplate::from_nbt_compound(&compound)
        .map_err(|error| format!("invalid structure: {error}"))
}

/// Writes a template as `.nbt` bytes, gzip compressed or raw.
pub fn template_to_bytes(template: &StructureTemplate, compress: bool) -> Result<Vec<u8>, String> {
    let compound = template.save();
    if compress {
        write_gzip_compound_tag_to_bytes(compound).map_err(|error| error.to_string())
    } else {
        let mut out = Vec::new();
        Nbt::new(String::new(), compound)
            .write_to_writer(&mut out)
            .map_err(|error| error.to_string())?;
        Ok(out)
    }
}

/// The yaw of an entity after the template is mirrored and then rotated, like vanilla
/// `Entity.mirror` followed by `Entity.rotate`.
#[must_use]
pub fn transformed_yaw(yaw: f32, rotation: Rotation, mirror: Mirror) -> f32 {
    let wrapped = yaw.rem_euclid(360.0);
    let wrapped = if wrapped >= 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    };
    let mirrored = match mirror {
        Mirror::LeftRight => 180.0 - wrapped,
        Mirror::FrontBack => -wrapped,
        Mirror::None => wrapped,
    };
    let degrees = match rotation {
        Rotation::None => 0.0,
        Rotation::Clockwise90 => 90.0,
        Rotation::Rotate180 => 180.0,
        Rotation::CounterClockwise90 => 270.0,
    };
    mirrored + degrees
}

/// The world position of a template block. The template's (0, 0, 0) corner is the pivot: it
/// stays at `origin` whatever the rotation, the rest of the template turns around it.
#[must_use]
pub const fn template_to_world(
    pos: Vector3<i32>,
    origin: BlockPos,
    rotation: Rotation,
    mirror: Mirror,
) -> BlockPos {
    let rel = StructureTemplate::transform_block_pos(pos, mirror, rotation, Vector3::new(0, 0, 0));
    BlockPos::new(origin.0.x + rel.x, origin.0.y + rel.y, origin.0.z + rel.z)
}

fn full_name(block: &Block) -> String {
    if block.name.contains(':') {
        block.name.to_string()
    } else {
        format!("minecraft:{}", block.name)
    }
}

fn palette_entry_of(state_id: BlockStateId) -> PaletteEntry {
    let block = Block::from_state_id(state_id);
    let properties = block
        .properties(state_id)
        .map(|props| props.to_props())
        .unwrap_or_default();
    PaletteEntry::with_properties(full_name(block), properties)
}

impl World {
    /// Saves the blocks, block entities and (if asked) entities of the box between `min` and
    /// `max` (inclusive, any order) as a template whose origin is the minimum corner.
    /// Block entities that were just placed and are not loaded yet are not saved.
    pub fn capture_template(
        self: &Arc<Self>,
        min: BlockPos,
        max: BlockPos,
        include_entities: bool,
    ) -> Result<StructureTemplate, String> {
        let low = Vector3::new(
            min.0.x.min(max.0.x),
            min.0.y.min(max.0.y),
            min.0.z.min(max.0.z),
        );
        let high = Vector3::new(
            min.0.x.max(max.0.x),
            min.0.y.max(max.0.y),
            min.0.z.max(max.0.z),
        );
        let size = Vector3::new(high.x - low.x + 1, high.y - low.y + 1, high.z - low.z + 1);
        let volume = i64::from(size.x) * i64::from(size.y) * i64::from(size.z);
        if volume > MAX_CAPTURE_VOLUME {
            return Err(format!(
                "the area has {volume} blocks, at most {MAX_CAPTURE_VOLUME}"
            ));
        }

        let mut blocks = Vec::new();
        for y in low.y..=high.y {
            for z in low.z..=high.z {
                for x in low.x..=high.x {
                    let pos = BlockPos::new(x, y, z);
                    let state_id = self.get_block_state_id(&pos);
                    let nbt = self.get_block_entity(&pos).map(|entity| {
                        let mut nbt = NbtCompound::new();
                        entity.write_internal(&mut nbt);
                        nbt.child_tags.remove("x");
                        nbt.child_tags.remove("y");
                        nbt.child_tags.remove("z");
                        nbt
                    });
                    blocks.push(StructureBlockInfo::new(
                        Vector3::new(x - low.x, y - low.y, z - low.z),
                        palette_entry_of(state_id),
                        nbt,
                    ));
                }
            }
        }

        let mut entity_info_list = Vec::new();
        if include_entities {
            for entity in self.entities.load().iter() {
                let pos = entity.get_entity().pos.load();
                if pos.x < f64::from(low.x)
                    || pos.y < f64::from(low.y)
                    || pos.z < f64::from(low.z)
                    || pos.x >= f64::from(high.x + 1)
                    || pos.y >= f64::from(high.y + 1)
                    || pos.z >= f64::from(high.z + 1)
                {
                    continue;
                }
                let mut nbt = NbtCompound::new();
                entity.write_nbt(&mut nbt);
                nbt.child_tags.remove("UUID");
                let rel = Vector3::new(
                    pos.x - f64::from(low.x),
                    pos.y - f64::from(low.y),
                    pos.z - f64::from(low.z),
                );
                nbt.put(
                    "Pos",
                    NbtTag::List(vec![rel.x.into(), rel.y.into(), rel.z.into()]),
                );
                entity_info_list.push(StructureEntityInfo::new(
                    rel,
                    Vector3::new(
                        rel.x.floor() as i32,
                        rel.y.floor() as i32,
                        rel.z.floor() as i32,
                    ),
                    nbt,
                ));
            }
        }

        let mut template = StructureTemplate::default();
        template.size = size;
        template.palettes.push(Palette::new(blocks));
        template.entity_info_list = entity_info_list;
        // A round trip through the saved form fills in everything derived from the lists.
        StructureTemplate::from_nbt_compound(&template.save())
            .map_err(|error| format!("could not build the template: {error}"))
    }

    /// Places the first palette of `template` with its origin corner at `origin`, mirrored and
    /// then rotated around it (see [`template_to_world`]), and spawns its entities. Blocks go
    /// through [`World::set_blocks_bulk`], so the updates run once at the end. Structure voids
    /// are never placed, air only when `ignore_air` is false. `integrity` below 1 keeps each
    /// block with that chance. Returns the number of blocks written.
    #[expect(clippy::too_many_arguments)]
    pub fn place_template_bulk(
        self: &Arc<Self>,
        template: &StructureTemplate,
        origin: BlockPos,
        rotation: Rotation,
        mirror: Mirror,
        flags: BlockFlags,
        ignore_air: bool,
        integrity: Option<f32>,
    ) -> Result<u32, String> {
        let palette = template
            .palettes
            .first()
            .ok_or_else(|| "the structure has no palette".to_string())?;

        let mut states: HashMap<&PaletteEntry, Option<BlockStateId>> = HashMap::new();
        let mut changes = Vec::with_capacity(palette.blocks().len());
        for info in palette.blocks() {
            if info.state.name == "minecraft:structure_void" {
                continue;
            }
            let state_id = *states.entry(&info.state).or_insert_with(|| {
                let resolved = BlockStateResolver::resolve_simple(&info.state)?.id;
                let block = Block::from_state_id(resolved);
                let mirrored = if mirror == Mirror::None {
                    resolved
                } else {
                    self.block_registry.mirror(block, resolved, mirror).id
                };
                Some(if rotation == Rotation::None {
                    mirrored
                } else {
                    self.block_registry.rotate(block, mirrored, rotation).id
                })
            });
            let Some(state_id) = state_id else {
                continue;
            };
            if ignore_air && state_id.to_state().is_air() {
                continue;
            }

            let pos = template_to_world(info.pos, origin, rotation, mirror);
            if let Some(integrity) = integrity
                && integrity < 1.0
            {
                let mut random =
                    LegacyRand::from_seed(hash_block_pos(pos.0.x, pos.0.y, pos.0.z) as u64);
                if random.next_f32() > integrity {
                    continue;
                }
            }

            let mut change = BlockChange::new(pos, state_id);
            change.block_entity.clone_from(&info.nbt);
            changes.push(change);
        }

        let outcome = self.set_blocks_bulk(changes, flags);
        for error in &outcome.errors {
            tracing::warn!("Structure placement: {error}");
        }

        for info in &template.entity_info_list {
            let rel = StructureTemplate::transform_vec3(
                info.pos,
                mirror,
                rotation,
                Vector3::new(0, 0, 0),
            );
            let pos = Vector3::new(
                f64::from(origin.0.x) + rel.x,
                f64::from(origin.0.y) + rel.y,
                f64::from(origin.0.z) + rel.z,
            );
            let mut nbt = info.nbt.clone();
            if let Some(rotation_list) = nbt.get_list("Rotation")
                && rotation_list.len() == 2
            {
                let yaw = rotation_list[0].extract_float().unwrap_or_default();
                let pitch = rotation_list[1].extract_float().unwrap_or_default();
                nbt.put(
                    "Rotation",
                    NbtTag::List(vec![
                        transformed_yaw(yaw, rotation, mirror).into(),
                        pitch.into(),
                    ]),
                );
            }
            self.spawn_entity_from_nbt(&nbt, Some(pos));
        }

        Ok(outcome.changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world_pos(x: i32, z: i32, rotation: Rotation, mirror: Mirror) -> (i32, i32) {
        let pos = template_to_world(
            Vector3::new(x, 0, z),
            BlockPos::new(0, 0, 0),
            rotation,
            mirror,
        );
        (pos.0.x, pos.0.z)
    }

    #[test]
    fn rotation_turns_around_the_origin() {
        assert_eq!(world_pos(2, 1, Rotation::None, Mirror::None), (2, 1));
        assert_eq!(
            world_pos(2, 1, Rotation::Clockwise90, Mirror::None),
            (-1, 2)
        );
        assert_eq!(world_pos(2, 1, Rotation::Rotate180, Mirror::None), (-2, -1));
        assert_eq!(
            world_pos(2, 1, Rotation::CounterClockwise90, Mirror::None),
            (1, -2)
        );
    }

    #[test]
    fn four_quarter_turns_are_identity() {
        let mut pos = (3, 5);
        for _ in 0..4 {
            pos = world_pos(pos.0, pos.1, Rotation::Clockwise90, Mirror::None);
        }
        assert_eq!(pos, (3, 5));
    }

    #[test]
    fn mirror_flips_one_axis() {
        assert_eq!(world_pos(2, 1, Rotation::None, Mirror::LeftRight), (2, -1));
        assert_eq!(world_pos(2, 1, Rotation::None, Mirror::FrontBack), (-2, 1));
    }

    #[test]
    fn mirror_comes_before_rotation() {
        // Vanilla mirrors first: (2, 1) -> (2, -1) -> clockwise quarter turn.
        assert_eq!(
            world_pos(2, 1, Rotation::Clockwise90, Mirror::LeftRight),
            (1, 2)
        );
    }

    #[test]
    fn yaw_follows_vanilla() {
        assert!((transformed_yaw(0.0, Rotation::Clockwise90, Mirror::None) - 90.0).abs() < 1e-4);
        assert!((transformed_yaw(90.0, Rotation::None, Mirror::FrontBack) + 90.0).abs() < 1e-4);
        assert!((transformed_yaw(0.0, Rotation::None, Mirror::LeftRight) - 180.0).abs() < 1e-4);
    }

    #[test]
    fn structure_bytes_round_trip() {
        let mut template = StructureTemplate::default();
        template.size = Vector3::new(1, 1, 1);
        template
            .palettes
            .push(Palette::new(vec![StructureBlockInfo::new(
                Vector3::new(0, 0, 0),
                PaletteEntry::new("minecraft:stone".to_string()),
                None,
            )]));
        let template = StructureTemplate::from_nbt_compound(&template.save()).unwrap();
        for compress in [true, false] {
            let bytes = template_to_bytes(&template, compress).unwrap();
            let loaded = load_template(&bytes).unwrap();
            assert_eq!(loaded.size, template.size);
            assert_eq!(loaded.palettes[0].blocks().len(), 1);
            assert_eq!(loaded.palettes[0].blocks()[0].state.name, "minecraft:stone");
        }
    }
}
