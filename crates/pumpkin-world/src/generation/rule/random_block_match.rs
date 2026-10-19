use pumpkin_data::{BlockId, BlockStateId};
use pumpkin_util::random::{RandomGenerator, RandomImpl};

pub struct RandomBlockMatchRuleTest {
    pub block: BlockId,
    pub probability: f32,
}

impl RandomBlockMatchRuleTest {
    pub fn test(&self, state: BlockStateId, random: &mut RandomGenerator) -> bool {
        u32::from(self.block.as_u16()) == state.as_u32() && random.next_f32() < self.probability
    }
}
