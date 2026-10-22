use super::light;
use super::util::write_compound_nbt;
use crate::block::entities::plugin::CLIENT_DATA_KEY;
use pumpkin_data::block_entity_registry::plugin_block_entity_type_by_key;
use pumpkin_data::block_properties::BLOCK_ENTITY_TYPES;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::ser::NetworkWriteExt;
use pumpkin_protocol::ser::WritingError;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::position::get_local_cord;
use pumpkin_util::version::JavaMinecraftVersion;
use pumpkin_world::chunk::ChunkData;
use pumpkin_world::chunk::palette::NetworkPalette;
use std::io::Write;

/// The block entity type name of the trial spawner. Its update tag is smaller than its save data
/// (see the chunk block entity loop below), so it is the one name the chunk serializer has to know.
const TRIAL_SPAWNER: &str = "trial_spawner";

/// Serializes chunk data for Minecraft 1.18+ through 26.2+.
#[expect(clippy::too_many_lines)]
pub fn write_chunk_data(
    chunk: &ChunkData,
    mut write: impl Write,
    version: &JavaMinecraftVersion,
) -> Result<(), WritingError> {
    write.write_i32_be(chunk.x)?;
    write.write_i32_be(chunk.z)?;

    let heightmaps = chunk
        .heightmap
        .lock()
        .map_err(|_| WritingError::Message("heightmap lock poisoned".into()))?;
    if version >= &JavaMinecraftVersion::V_1_21_5 {
        write.write_var_int(&VarInt(3))?; // Map size

        let mut write_heightmap = |index: i32, data: &[i64]| -> Result<(), WritingError> {
            write.write_var_int(&VarInt(index))?;
            write.write_var_int(&VarInt(data.len() as i32))?;
            for val in data {
                write.write_i64_be(*val)?;
            }
            Ok(())
        };

        write_heightmap(1, heightmaps.world_surface.as_deref().unwrap_or(&[0; 37]))?;
        write_heightmap(4, heightmaps.motion_blocking.as_deref().unwrap_or(&[0; 37]))?;
        write_heightmap(
            5,
            heightmaps
                .motion_blocking_no_leaves
                .as_deref()
                .unwrap_or(&[0; 37]),
        )?;
    } else {
        let mut comp = pumpkin_nbt::compound::NbtCompound::new();
        if let Some(ref ws) = heightmaps.world_surface {
            comp.put(
                "WORLD_SURFACE",
                pumpkin_nbt::tag::NbtTag::LongArray(ws.to_vec()),
            );
        }
        if let Some(ref mb) = heightmaps.motion_blocking {
            comp.put(
                "MOTION_BLOCKING",
                pumpkin_nbt::tag::NbtTag::LongArray(mb.to_vec()),
            );
        }
        if let Some(ref mbnl) = heightmaps.motion_blocking_no_leaves {
            comp.put(
                "MOTION_BLOCKING_NO_LEAVES",
                pumpkin_nbt::tag::NbtTag::LongArray(mbnl.to_vec()),
            );
        }
        write_compound_nbt(&mut write, &comp, *version)?;
    }
    drop(heightmaps);

    {
        let mut blocks_and_biomes_buf = Vec::new();
        let block_sections = chunk
            .section
            .block_sections
            .read()
            .map_err(|_| WritingError::Message("block_sections read lock poisoned".into()))?;
        let biome_sections = chunk
            .section
            .biome_sections
            .read()
            .map_err(|_| WritingError::Message("biome_sections read lock poisoned".into()))?;

        let mut zero_bytes_count = 0;

        for (block_palette, biome_palette) in block_sections.iter().zip(biome_sections.iter()) {
            let non_empty_block_count = block_palette.non_air_block_count() as i16;
            blocks_and_biomes_buf.write_i16_be(non_empty_block_count)?;
            if version >= &JavaMinecraftVersion::V_26_1 {
                // New in 26.1, fluid count
                let liquid_count = block_palette.liquid_block_count() as i16;
                blocks_and_biomes_buf.write_i16_be(liquid_count)?;
            }

            let block_network = block_palette.convert_network();
            blocks_and_biomes_buf.write_u8(block_network.bits_per_entry)?;

            match block_network.palette {
                NetworkPalette::Single(registry_id) => {
                    blocks_and_biomes_buf.write_var_int(&VarInt(registry_id as i32))?;
                }
                NetworkPalette::Indirect(palette) => {
                    blocks_and_biomes_buf.write_var_int(&palette.len().try_into().map_err(
                        |_| {
                            WritingError::Message(format!(
                                "{} is not representable as a VarInt!",
                                palette.len()
                            ))
                        },
                    )?)?;
                    for registry_id in palette {
                        blocks_and_biomes_buf.write_var_int(&VarInt(registry_id as i32))?;
                    }
                }
                NetworkPalette::Direct => {}
            }

            if version <= &JavaMinecraftVersion::V_1_21_4 {
                blocks_and_biomes_buf.write_list(&block_network.packed_data, |buf, &packed| {
                    buf.write_i64_be(packed)
                })?;
            } else {
                for packed in &block_network.packed_data {
                    blocks_and_biomes_buf.write_i64_be(*packed)?;
                }
            }

            let biome_network = biome_palette.convert_network();
            blocks_and_biomes_buf.write_u8(biome_network.bits_per_entry)?;

            match biome_network.palette {
                NetworkPalette::Single(registry_id) => {
                    blocks_and_biomes_buf.write_var_int(&VarInt(registry_id as i32))?;
                }
                NetworkPalette::Indirect(palette) => {
                    blocks_and_biomes_buf.write_var_int(&palette.len().try_into().map_err(
                        |_| {
                            WritingError::Message(format!(
                                "{} is not representable as a VarInt!",
                                palette.len()
                            ))
                        },
                    )?)?;
                    for registry_id in palette {
                        blocks_and_biomes_buf.write_var_int(&registry_id.into())?;
                    }
                }
                NetworkPalette::Direct => {}
            }

            if version <= &JavaMinecraftVersion::V_1_21_4 {
                blocks_and_biomes_buf.write_list(&biome_network.packed_data, |buf, &packed| {
                    buf.write_i64_be(packed)
                })?;
            } else {
                for packed in &biome_network.packed_data {
                    blocks_and_biomes_buf.write_i64_be(*packed)?;
                }
            }

            if version == &JavaMinecraftVersion::V_1_21_5 {
                let block_storage_len = block_network.packed_data.len() as i32;
                let biome_storage_len = biome_network.packed_data.len() as i32;
                zero_bytes_count += VarInt(block_storage_len).written_size()
                    + VarInt(biome_storage_len).written_size();
            }
        }

        if version == &JavaMinecraftVersion::V_1_21_5 && zero_bytes_count > 0 {
            blocks_and_biomes_buf.resize(blocks_and_biomes_buf.len() + zero_bytes_count, 0);
        }

        write.write_var_int(&blocks_and_biomes_buf.len().try_into().map_err(|_| {
            WritingError::Message(format!(
                "{} is not representable as a VarInt!",
                blocks_and_biomes_buf.len()
            ))
        })?)?;
        write.write_slice(&blocks_and_biomes_buf)?;
    };

    let block_entities = chunk
        .pending_block_entities
        .lock()
        .map_err(|_| WritingError::Message("block_entities lock poisoned".into()))?;
    let mut entries: Vec<(BlockPos, i32, NbtCompound)> = Vec::with_capacity(block_entities.len());
    for (pos, nbt) in block_entities.iter() {
        let plugin_type = nbt
            .get_string("id")
            .and_then(plugin_block_entity_type_by_key);
        if let Some(entity_type) = plugin_type {
            // The save data of a block entity of a plugin stays on the server, clients get its
            // update tag. An entity without one is not sent.
            if let Some(client_data) = nbt.get_compound(CLIENT_DATA_KEY) {
                entries.push((*pos, entity_type.id as i32, client_data.clone()));
            }
            continue;
        }

        let name = nbt
            .get_string("id")
            .map_or("", |id_str| id_str.split(':').next_back().unwrap_or(id_str));
        let id = BLOCK_ENTITY_TYPES
            .iter()
            .position(|&n| n == name)
            .unwrap_or(0);

        let mut client_nbt = nbt.clone();
        client_nbt.child_tags.remove("id");
        client_nbt.child_tags.remove("x");
        client_nbt.child_tags.remove("y");
        client_nbt.child_tags.remove("z");
        client_nbt.child_tags.remove("LootTable");
        client_nbt.child_tags.remove("LootTableSeed");
        client_nbt.child_tags.remove("PumpkinCustomData");
        client_nbt.child_tags.remove("BukkitValues");
        // A chunk carries an entity's *update tag* (`BlockEntity.getUpdateTag`), and for most
        // entities that is the whole save data minus the metadata removed above — but not for the
        // trial spawner: `TrialSpawnerStateData.getUpdateTag` sends only `next_mob_spawns_at` and
        // `spawn_data`, because its configuration is a `Holder<TrialSpawnerConfig>` of the
        // `minecraft:trial_spawner` data pack registry. A client never receives that registry
        // (`Registries.TRIAL_SPAWNER_CONFIG` is not one of `RegistryDataLoader
        // .SYNCHRONIZED_REGISTRIES`), so sending the saved form makes every trial spawner in view
        // fail with `Failed to decode from map: Registry does not exist: minecraft:trial_spawner`.
        if name == TRIAL_SPAWNER {
            for field in [
                "config",
                "normal_config",
                "ominous_config",
                "target_cooldown_length",
                "required_player_range",
            ] {
                client_nbt.child_tags.remove(field);
            }
        }
        entries.push((*pos, id as i32, client_nbt));
    }
    write.write_var_int(&VarInt(entries.len() as i32))?;
    for (pos, id, client_nbt) in &entries {
        let local_xz = ((get_local_cord(pos.0.x) & 0xF) << 4) | (get_local_cord(pos.0.z) & 0xF);

        write.write_u8(local_xz as u8)?;
        write.write_i16_be(pos.0.y as i16)?;
        write.write_var_int(&VarInt(*id))?;
        write_compound_nbt(&mut write, client_nbt, *version)?;
    }

    // The light payload lives in one place (`light.rs`): the masks, the arrays and the
    // trust-edges flag are the same bytes the `LIGHT_UPDATE` packet would carry, and keeping a
    // second copy here is how a uniform section came to be announced as an empty one.
    light::light_data_from_chunk(chunk, *version)?.write(write, version)?;

    Ok(())
}
