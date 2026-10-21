use pumpkin_protocol::codec::bit_set::BitSet;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::{CLightUpdate, LightData};
use pumpkin_protocol::ser::WritingError;
use pumpkin_util::version::JavaMinecraftVersion;
use pumpkin_world::chunk::ChunkData;
use pumpkin_world::chunk::format::LightContainer;

pub trait ChunkLightExt {
    fn from_chunk(chunk: &ChunkData, version: JavaMinecraftVersion) -> Result<Self, WritingError>
    where
        Self: Sized;
}

impl ChunkLightExt for CLightUpdate {
    fn from_chunk(chunk: &ChunkData, version: JavaMinecraftVersion) -> Result<Self, WritingError> {
        let light_data = light_data_from_chunk(chunk, version)?;
        Ok(Self {
            chunk_x: VarInt(chunk.x),
            chunk_z: VarInt(chunk.z),
            light_data,
        })
    }
}

/// Files one section of one light layer under the mask the client reads it from.
///
/// The empty mask is **not** "nothing to change": it tells the client the section is all zeros
/// (`ClientPacketListener.readSectionList` stores a `new DataLayer()` for it), and it overrides
/// the all-15 layer a client gives a light-on section it has no data for
/// (`SkyLightSectionStorage.createDataLayer`). A section that is uniformly `value > 0` therefore
/// has to be sent as an array of `value` — unlike a genuinely dark section, which the empty mask
/// describes exactly.
fn push_section_light(
    container: &LightContainer,
    bit_index: usize,
    light_mask: &mut u64,
    empty_mask: &mut u64,
    arrays: &mut Vec<Vec<u8>>,
) {
    match container {
        LightContainer::Full(data) => {
            *light_mask |= 1 << bit_index;
            arrays.push(data.to_vec());
        }
        LightContainer::Empty(value) if *value > 0 => {
            *light_mask |= 1 << bit_index;
            arrays.push(LightContainer::uniform_bytes(*value).into_vec());
        }
        LightContainer::Empty(_) => *empty_mask |= 1 << bit_index,
    }
}

/// The light the client is told for one chunk section of one layer.
#[cfg(test)]
fn section_light_from_payload(
    light_mask: &BitSet,
    empty_mask: &BitSet,
    arrays: &[Vec<u8>],
    section_index: usize,
) -> Option<Vec<u8>> {
    let bit = section_index + 1;
    if empty_mask.get_bit(bit) {
        return Some(vec![0; LightContainer::ARRAY_SIZE]);
    }
    if !light_mask.get_bit(bit) {
        return None;
    }
    let array_index = (0..bit).filter(|i| light_mask.get_bit(*i)).count();
    Some(arrays[array_index].clone())
}

#[expect(clippy::too_many_lines)]
pub fn light_data_from_chunk(
    chunk: &ChunkData,
    version: JavaMinecraftVersion,
) -> Result<LightData, WritingError> {
    let light_engine = chunk
        .light_engine
        .lock()
        .map_err(|_| WritingError::Message("light_engine lock poisoned".into()))?;

    if version < JavaMinecraftVersion::V_1_18 {
        let base_section = (0 - chunk.section.min_y).max(0) as usize / 16;
        let mut sky_light_mask = 0u64;
        let mut block_light_mask = 0u64;
        let mut sky_light_empty_mask = 0u64;
        let mut block_light_empty_mask = 0u64;
        let mut sky_light_arrays = Vec::new();
        let mut block_light_arrays = Vec::new();

        // Bit 0: Y = -1 (below world section 0)
        if base_section > 0 && base_section - 1 < light_engine.sky_light.len() {
            match &light_engine.sky_light[base_section - 1] {
                LightContainer::Full(data) => {
                    sky_light_mask |= 1 << 0;
                    sky_light_arrays.push(data.to_vec());
                }
                LightContainer::Empty(val) if *val > 0 => {
                    sky_light_mask |= 1 << 0;
                    sky_light_arrays.push(vec![*val << 4 | *val; 2048]);
                }
                LightContainer::Empty(_) => {
                    sky_light_empty_mask |= 1 << 0;
                }
            }
        } else {
            sky_light_empty_mask |= 1 << 0;
        }

        if base_section > 0 && base_section - 1 < light_engine.block_light.len() {
            match &light_engine.block_light[base_section - 1] {
                LightContainer::Full(data) => {
                    block_light_mask |= 1 << 0;
                    block_light_arrays.push(data.to_vec());
                }
                LightContainer::Empty(val) if *val > 0 => {
                    block_light_mask |= 1 << 0;
                    block_light_arrays.push(vec![*val << 4 | *val; 2048]);
                }
                LightContainer::Empty(_) => {
                    block_light_empty_mask |= 1 << 0;
                }
            }
        } else {
            block_light_empty_mask |= 1 << 0;
        }

        // Bits 1..=16: world sections (Y = 0..15)
        for i in 0..16 {
            let bit_index = i + 1;
            let sec_idx = base_section + i;

            if sec_idx < light_engine.sky_light.len() {
                match &light_engine.sky_light[sec_idx] {
                    LightContainer::Full(data) => {
                        sky_light_mask |= 1 << bit_index;
                        sky_light_arrays.push(data.to_vec());
                    }
                    LightContainer::Empty(val) if *val > 0 => {
                        sky_light_mask |= 1 << bit_index;
                        sky_light_arrays.push(vec![*val << 4 | *val; 2048]);
                    }
                    LightContainer::Empty(_) => {
                        sky_light_empty_mask |= 1 << bit_index;
                    }
                }
            } else {
                sky_light_empty_mask |= 1 << bit_index;
            }

            if sec_idx < light_engine.block_light.len() {
                match &light_engine.block_light[sec_idx] {
                    LightContainer::Full(data) => {
                        block_light_mask |= 1 << bit_index;
                        block_light_arrays.push(data.to_vec());
                    }
                    LightContainer::Empty(val) if *val > 0 => {
                        block_light_mask |= 1 << bit_index;
                        block_light_arrays.push(vec![*val << 4 | *val; 2048]);
                    }
                    LightContainer::Empty(_) => {
                        block_light_empty_mask |= 1 << bit_index;
                    }
                }
            } else {
                block_light_empty_mask |= 1 << bit_index;
            }
        }

        // Bit 17: Y = 16 (above world section 15)
        let top_sec = base_section + 16;
        if top_sec < light_engine.sky_light.len() {
            match &light_engine.sky_light[top_sec] {
                LightContainer::Full(data) => {
                    sky_light_mask |= 1 << 17;
                    sky_light_arrays.push(data.to_vec());
                }
                LightContainer::Empty(val) if *val > 0 => {
                    sky_light_mask |= 1 << 17;
                    sky_light_arrays.push(vec![*val << 4 | *val; 2048]);
                }
                LightContainer::Empty(_) => {
                    sky_light_empty_mask |= 1 << 17;
                }
            }
        } else {
            sky_light_empty_mask |= 1 << 17;
        }

        if top_sec < light_engine.block_light.len() {
            match &light_engine.block_light[top_sec] {
                LightContainer::Full(data) => {
                    block_light_mask |= 1 << 17;
                    block_light_arrays.push(data.to_vec());
                }
                LightContainer::Empty(val) if *val > 0 => {
                    block_light_mask |= 1 << 17;
                    block_light_arrays.push(vec![*val << 4 | *val; 2048]);
                }
                LightContainer::Empty(_) => {
                    block_light_empty_mask |= 1 << 17;
                }
            }
        } else {
            block_light_empty_mask |= 1 << 17;
        }

        Ok(LightData {
            trust_edges: true,
            sky_light_mask: BitSet::from_u64(sky_light_mask),
            block_light_mask: BitSet::from_u64(block_light_mask),
            empty_sky_light_mask: BitSet::from_u64(sky_light_empty_mask),
            empty_block_light_mask: BitSet::from_u64(block_light_empty_mask),
            sky_light_arrays,
            block_light_arrays,
        })
    } else {
        let num_sections = light_engine.sky_light.len();
        let mut sky_light_empty_mask = 0u64;
        let mut block_light_empty_mask = 0u64;
        let mut sky_light_mask = 0u64;
        let mut block_light_mask = 0u64;

        let mut sky_light_arrays = Vec::new();
        let mut block_light_arrays = Vec::new();

        sky_light_empty_mask |= 1 << 0;
        block_light_empty_mask |= 1 << 0;

        for section_index in 0..num_sections {
            let bit_index = section_index + 1;

            push_section_light(
                &light_engine.sky_light[section_index],
                bit_index,
                &mut sky_light_mask,
                &mut sky_light_empty_mask,
                &mut sky_light_arrays,
            );
            push_section_light(
                &light_engine.block_light[section_index],
                bit_index,
                &mut block_light_mask,
                &mut block_light_empty_mask,
                &mut block_light_arrays,
            );
        }

        sky_light_empty_mask |= 1 << (num_sections + 1);
        block_light_empty_mask |= 1 << (num_sections + 1);

        Ok(LightData {
            trust_edges: true,
            sky_light_mask: BitSet::from_u64(sky_light_mask),
            block_light_mask: BitSet::from_u64(block_light_mask),
            empty_sky_light_mask: BitSet::from_u64(sky_light_empty_mask),
            empty_block_light_mask: BitSet::from_u64(block_light_empty_mask),
            sky_light_arrays,
            block_light_arrays,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumpkin_world::chunk::format::LightContainer;

    /// The light the client is told for a chunk that holds one section per representation.
    #[test]
    fn uniform_sections_travel_as_their_own_light() {
        let chunk = ChunkData::empty(0, 0);
        {
            let mut engine = chunk
                .light_engine
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // A sky-lit air section, a dark one, and one with real values in it.
            engine.sky_light = vec![
                LightContainer::new_empty(15),
                LightContainer::new_empty(0),
                LightContainer::new_filled(7),
            ]
            .into_boxed_slice();
            engine.block_light = vec![LightContainer::new_empty(0); 3].into_boxed_slice();
        }

        let light =
            light_data_from_chunk(&chunk, JavaMinecraftVersion::V_26_3).expect("light data builds");

        assert_eq!(
            section_light_from_payload(
                &light.sky_light_mask,
                &light.empty_sky_light_mask,
                &light.sky_light_arrays,
                0,
            ),
            Some(vec![0xFF; LightContainer::ARRAY_SIZE]),
            "a section that is uniformly 15 is not an empty one: the empty mask means all zeros",
        );
        assert_eq!(
            section_light_from_payload(
                &light.sky_light_mask,
                &light.empty_sky_light_mask,
                &light.sky_light_arrays,
                1,
            ),
            Some(vec![0; LightContainer::ARRAY_SIZE]),
            "a uniformly dark section belongs in the empty mask",
        );
        assert_eq!(
            section_light_from_payload(
                &light.sky_light_mask,
                &light.empty_sky_light_mask,
                &light.sky_light_arrays,
                2,
            ),
            Some(vec![0x77; LightContainer::ARRAY_SIZE]),
            "a filled section keeps its values",
        );
        assert_eq!(
            section_light_from_payload(
                &light.block_light_mask,
                &light.empty_block_light_mask,
                &light.block_light_arrays,
                0,
            ),
            Some(vec![0; LightContainer::ARRAY_SIZE]),
        );
        assert_eq!(
            light.sky_light_arrays.len(),
            2,
            "one array per section in the sky light mask",
        );
    }
}
