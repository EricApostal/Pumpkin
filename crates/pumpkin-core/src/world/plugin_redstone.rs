//! Redstone output that a plugin stores for its custom blocks, instead of answering an event each
//! time a redstone reader asks. It lives in memory only: the plugin sets it again after a restart.

use std::sync::Arc;

use dashmap::DashMap;
use pumpkin_data::block_registry::BlockCallbacks;
use pumpkin_data::{Block, BlockDirection, BlockId};
use pumpkin_util::math::position::BlockPos;

use super::World;

/// The highest redstone power and analog output there is.
const MAX_POWER: u8 = 15;

/// The power a custom block gives on one of its faces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FacePower {
    /// Reaches the block next to the face, vanilla `getSignal`.
    pub weak: u8,
    /// Also goes through a solid block next to the face, vanilla `getDirectSignal`.
    pub strong: u8,
}

/// What a plugin stored for the custom block at one position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoredOutput {
    /// The block it was stored for, so a block that took the place of it does not inherit it.
    block: BlockId,
    /// By `BlockDirection::to_index`, `None` when the plugin stored no power.
    faces: Option<[FacePower; 6]>,
    comparator: Option<u8>,
}

impl StoredOutput {
    const fn is_empty(&self) -> bool {
        self.faces.is_none() && self.comparator.is_none()
    }
}

/// The outputs that plugins stored, by position.
#[derive(Default)]
pub struct PluginRedstoneOutputs(DashMap<BlockPos, StoredOutput>);

impl PluginRedstoneOutputs {
    /// The power stored for the face of `block` at `position`, `None` when the plugin stored none.
    #[must_use]
    pub fn face_power(
        &self,
        position: &BlockPos,
        block: BlockId,
        face: BlockDirection,
    ) -> Option<FacePower> {
        let stored = self.0.get(position)?;
        if stored.block != block {
            return None;
        }
        stored
            .faces
            .map(|faces| faces[usize::from(face.to_index())])
    }

    /// The analog output stored for `block` at `position`.
    #[must_use]
    pub fn comparator_output(&self, position: &BlockPos, block: BlockId) -> Option<u8> {
        let stored = self.0.get(position)?;
        if stored.block != block {
            return None;
        }
        stored.comparator
    }

    /// Forgets everything stored for `position`.
    pub fn remove(&self, position: &BlockPos) {
        self.0.remove(position);
    }

    /// Changes the power of `face`, or of every face for `None`. Returns the faces before and
    /// after, `None` for the faces when there was no power.
    fn set_power(
        &self,
        position: BlockPos,
        block: BlockId,
        face: Option<BlockDirection>,
        power: FacePower,
    ) -> ([FacePower; 6], [FacePower; 6]) {
        let mut entry = self.0.entry(position).or_insert(StoredOutput {
            block,
            faces: None,
            comparator: None,
        });
        if entry.block != block {
            *entry = StoredOutput {
                block,
                faces: None,
                comparator: None,
            };
        }
        let before = entry.faces.unwrap_or_default();
        let mut after = before;
        match face {
            Some(face) => after[usize::from(face.to_index())] = power,
            None => after = [power; 6],
        }
        let all_zero = after.iter().all(|face| *face == FacePower::default());
        entry.faces = (!all_zero).then_some(after);
        let empty = entry.is_empty();
        drop(entry);
        if empty {
            self.0.remove_if(&position, |_, stored| stored.is_empty());
        }
        (before, after)
    }

    /// Stores the analog output, returns the one before.
    fn set_comparator(&self, position: BlockPos, block: BlockId, output: Option<u8>) -> Option<u8> {
        let mut entry = self.0.entry(position).or_insert(StoredOutput {
            block,
            faces: None,
            comparator: None,
        });
        if entry.block != block {
            *entry = StoredOutput {
                block,
                faces: None,
                comparator: None,
            };
        }
        let before = entry.comparator;
        entry.comparator = output;
        let empty = entry.is_empty();
        drop(entry);
        if empty {
            self.0.remove_if(&position, |_, stored| stored.is_empty());
        }
        before
    }
}

/// Fails unless `block` is a custom block that opted in to `callback`.
fn require_callback(block: &Block, callback: BlockCallbacks, name: &str) -> Result<(), String> {
    let info = block.dynamic_info().ok_or_else(|| {
        format!(
            "the block {} at the position is not a custom block",
            block.name
        )
    })?;
    if info.callbacks().contains(callback) {
        Ok(())
    } else {
        Err(format!(
            "the block {} did not set the '{name}' callback",
            block.name
        ))
    }
}

impl World {
    /// Stores the redstone power of the custom block at `position` for `face` (the side that
    /// touches the neighbour at `position + face`), or for every face when `face` is `None`, and
    /// updates the neighbours that can notice. The block needs the `REDSTONE_OUTPUT` callback.
    /// Values above 15 count as 15, zero for both removes the output.
    ///
    /// # Errors
    /// When there is no custom block with the callback at `position`.
    pub fn set_plugin_redstone_output(
        self: &Arc<Self>,
        position: &BlockPos,
        face: Option<BlockDirection>,
        weak: u8,
        strong: u8,
    ) -> Result<(), String> {
        let block = self.get_block(position);
        require_callback(block, BlockCallbacks::REDSTONE_OUTPUT, "redstone-output")?;
        let power = FacePower {
            weak: weak.min(MAX_POWER),
            strong: strong.min(MAX_POWER),
        };
        let (before, after) = self
            .plugin_redstone
            .set_power(*position, block.id, face, power);
        if before == after {
            return Ok(());
        }

        self.update_neighbors_at(position, block, None);
        // A solid block next to a face passes the strong power on to its own neighbours.
        for direction in BlockDirection::all() {
            let index = usize::from(direction.to_index());
            if before[index].strong == after[index].strong {
                continue;
            }
            let conductor = position.offset(direction.to_offset());
            if self.get_block_state(&conductor).is_solid_block() {
                self.update_neighbors_at(&conductor, block, Some(direction.opposite()));
            }
        }
        Ok(())
    }

    /// Stores the analog output that comparators read from the custom block at `position`, `None`
    /// for no output, and updates the comparators next to it. The block needs the
    /// `COMPARATOR_OUTPUT` callback. Values above 15 count as 15.
    ///
    /// # Errors
    /// When there is no custom block with the callback at `position`.
    pub fn set_plugin_comparator_output(
        self: &Arc<Self>,
        position: &BlockPos,
        output: Option<u8>,
    ) -> Result<(), String> {
        let block = self.get_block(position);
        require_callback(
            block,
            BlockCallbacks::COMPARATOR_OUTPUT,
            "comparator-output",
        )?;
        let output = output.map(|output| output.min(MAX_POWER));
        let before = self
            .plugin_redstone
            .set_comparator(*position, block.id, output);
        if before != output {
            self.update_neighbour_for_output_signal(position, block);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: BlockId = BlockId::new_or_air(1);
    const OTHER: BlockId = BlockId::new_or_air(2);

    fn pos() -> BlockPos {
        BlockPos::new(1, 2, 3)
    }

    #[test]
    fn power_is_stored_by_face_and_removed_at_zero() {
        let outputs = PluginRedstoneOutputs::default();
        let power = FacePower { weak: 7, strong: 3 };
        let (before, after) = outputs.set_power(pos(), BLOCK, Some(BlockDirection::North), power);
        assert_eq!(before, [FacePower::default(); 6]);
        assert_eq!(after[usize::from(BlockDirection::North.to_index())], power);
        assert_eq!(
            outputs.face_power(&pos(), BLOCK, BlockDirection::North),
            Some(power)
        );
        assert_eq!(
            outputs.face_power(&pos(), BLOCK, BlockDirection::South),
            Some(FacePower::default())
        );

        outputs.set_power(pos(), BLOCK, None, FacePower::default());
        assert_eq!(
            outputs.face_power(&pos(), BLOCK, BlockDirection::North),
            None
        );
        assert!(outputs.0.is_empty());
    }

    #[test]
    fn output_of_another_block_is_not_inherited() {
        let outputs = PluginRedstoneOutputs::default();
        outputs.set_power(
            pos(),
            BLOCK,
            None,
            FacePower {
                weak: 15,
                strong: 15,
            },
        );
        outputs.set_comparator(pos(), BLOCK, Some(9));
        assert_eq!(outputs.comparator_output(&pos(), BLOCK), Some(9));
        assert_eq!(outputs.comparator_output(&pos(), OTHER), None);
        assert_eq!(outputs.face_power(&pos(), OTHER, BlockDirection::Up), None);

        // Storing for the new block starts from nothing.
        let (before, _) = outputs.set_power(pos(), OTHER, None, FacePower { weak: 1, strong: 0 });
        assert_eq!(before, [FacePower::default(); 6]);
        assert_eq!(outputs.comparator_output(&pos(), OTHER), None);

        outputs.remove(&pos());
        assert!(outputs.0.is_empty());
    }

    #[test]
    fn comparator_reports_the_previous_output() {
        let outputs = PluginRedstoneOutputs::default();
        assert_eq!(outputs.set_comparator(pos(), BLOCK, Some(4)), None);
        assert_eq!(outputs.set_comparator(pos(), BLOCK, None), Some(4));
        assert!(outputs.0.is_empty());
    }
}
