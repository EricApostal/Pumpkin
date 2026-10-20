use crate::pumpkin::plugin::block_placement::{
    Host, PlacementRule as WitPlacementRule, PlacementSource as WitPlacementSource,
    ShaftKind as WitShaftKind, ShaftNeighbour as WitShaftNeighbour,
};
use pumpkin_core::plugin::permissions;
use pumpkin_data::Block;
use pumpkin_data::block_registry::{
    PlacementRule, PlacementSource, ShaftKind, ShaftNeighbour,
};
use pumpkin_wasm_host_common::state::PluginHostState;
use tracing::info;

fn from_wit_shaft(shaft: WitShaftNeighbour) -> ShaftNeighbour {
    ShaftNeighbour {
        tag: shaft.tag,
        property: shaft.property,
        kind: match shaft.kind {
            WitShaftKind::Facing => ShaftKind::Facing,
            WitShaftKind::Axis => ShaftKind::Axis,
        },
    }
}

fn from_wit_source(source: WitPlacementSource) -> PlacementSource {
    match source {
        WitPlacementSource::HorizontalFacing => PlacementSource::HorizontalFacing,
        WitPlacementSource::LookingDirection => PlacementSource::LookingDirection,
        WitPlacementSource::ClickedFace => PlacementSource::ClickedFace,
        WitPlacementSource::ClickedFaceDown => PlacementSource::ClickedFaceDown,
        WitPlacementSource::ClickedAxis => PlacementSource::ClickedAxis,
        WitPlacementSource::HorizontalAxis => PlacementSource::HorizontalAxis,
        WitPlacementSource::ClickedHalf => PlacementSource::ClickedHalf,
        WitPlacementSource::VerticalLook(degrees) => PlacementSource::VerticalLook(degrees),
        WitPlacementSource::InWater => PlacementSource::InWater,
        WitPlacementSource::Sneaking => PlacementSource::Sneaking,
        WitPlacementSource::NeighbourShaft(shafts) => {
            PlacementSource::NeighbourShaft(shafts.into_iter().map(from_wit_shaft).collect())
        }
        WitPlacementSource::Constant(value) => PlacementSource::Constant(value),
    }
}

fn from_wit_rule(rule: WitPlacementRule) -> PlacementRule {
    PlacementRule {
        property: rule.property,
        source: from_wit_source(rule.source),
        opposite: rule.opposite,
        when_sneaking: rule.when_sneaking,
    }
}

impl Host for PluginHostState {
    fn set_block_placement_rules(
        &mut self,
        block_key: String,
        rules: Vec<WitPlacementRule>,
    ) -> wasmtime::Result<Result<(), String>> {
        if !self
            .permissions
            .iter()
            .any(|p| p == permissions::REGISTRY_BLOCKS)
        {
            return Ok(Err(format!(
                "setting block placement rules needs the '{}' permission",
                permissions::REGISTRY_BLOCKS
            )));
        }
        let count = rules.len();
        let rules = rules.into_iter().map(from_wit_rule).collect();
        Ok(
            match Block::set_dynamic_placement_rules(&block_key, rules) {
                Ok(()) => {
                    info!(
                        "Plugin {} set {} placement rules for block {}",
                        self.name.as_deref().unwrap_or("<unknown>"),
                        count,
                        block_key
                    );
                    Ok(())
                }
                Err(error) => Err(error.to_string()),
            },
        )
    }
}
