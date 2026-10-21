use crate::{
    argument_types::argument_type::{ArgumentType, JavaClientArgumentType},
    context::command_context::CommandContext,
    errors::command_syntax_error::CommandSyntaxError,
    errors::error_types::CommandErrorType,
    string_reader::StringReader,
    suggestion::suggestions::{Suggestions, SuggestionsBuilder},
};
use pumpkin_data::{Block, BlockState, translation};
use pumpkin_util::text::TextComponent;
use std::borrow::Cow;

pub const INVALID_BLOCK_ERROR_TYPE: CommandErrorType<1> = CommandErrorType::new(
    translation::java::ARGUMENT_BLOCK_ID_INVALID,
    translation::java::ARGUMENT_BLOCK_ID_INVALID,
);

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct BlockArgumentType;

impl<S: crate::source::CommandSource> ArgumentType<S> for BlockArgumentType {
    type Item = &'static Block;

    fn parse(&self, reader: &mut StringReader) -> Result<Self::Item, CommandSyntaxError> {
        let start = reader.cursor();
        while let Some(c) = reader.peek() {
            if c.is_alphanumeric() || c == '_' || c == ':' || c == '/' || c == '.' || c == '-' {
                reader.skip();
            } else {
                break;
            }
        }
        let block_name = &reader.string()[start..reader.cursor()];
        let normalized = if block_name.contains(':') {
            block_name.to_string()
        } else {
            format!("minecraft:{block_name}")
        };

        Block::from_name(&normalized)
            .ok_or_else(|| INVALID_BLOCK_ERROR_TYPE.create(reader, TextComponent::text(normalized)))
    }

    fn client_side_parser(&'_ self) -> JavaClientArgumentType {
        JavaClientArgumentType::BlockState
    }

    fn list_suggestions(
        &self,
        _context: &CommandContext<S>,
        builder: SuggestionsBuilder,
    ) -> Suggestions {
        builder.build()
    }
}

impl BlockArgumentType {
    pub fn get<S: crate::source::CommandSource>(
        context: &CommandContext<S>,
        name: &str,
    ) -> Result<&'static Block, CommandSyntaxError> {
        context.get_argument::<&'static Block>(name).copied()
    }
}

pub const INVALID_BLOCK_STATE_ERROR_TYPE: CommandErrorType<2> = CommandErrorType::new(
    translation::java::ARGUMENT_BLOCK_PROPERTY_INVALID,
    translation::java::ARGUMENT_BLOCK_PROPERTY_INVALID,
);

/// A block with block state properties, as `/setblock` and `/fill` take it:
/// `minecraft:oak_stairs[facing=east,half=top]`.
///
/// The given properties are applied to the default state, as Java `BlockStateParser` does; the
/// ones that are left out keep their default. Every given property must exist on the block and
/// its value must be one the block allows, otherwise the parse fails.
pub struct BlockStateArgumentType;

impl<S: crate::source::CommandSource> ArgumentType<S> for BlockStateArgumentType {
    type Item = &'static BlockState;

    fn parse(&self, reader: &mut StringReader) -> Result<Self::Item, CommandSyntaxError> {
        let block = <BlockArgumentType as ArgumentType<S>>::parse(&BlockArgumentType, reader)?;
        if reader.peek() != Some('[') {
            return Ok(block.default_state);
        }
        let properties = super::block_predicate::parse_properties(reader)?;
        let mut state = block.default_state;
        for (name, value) in &properties {
            let Some(next) = with_property(block, state, name, value) else {
                return Err(INVALID_BLOCK_STATE_ERROR_TYPE.create(
                    reader,
                    TextComponent::text(name.clone()),
                    TextComponent::text(value.clone()),
                ));
            };
            state = next;
        }
        Ok(state)
    }

    fn client_side_parser(&'_ self) -> JavaClientArgumentType {
        JavaClientArgumentType::BlockState
    }

    fn list_suggestions(
        &self,
        _context: &CommandContext<S>,
        builder: SuggestionsBuilder,
    ) -> Suggestions {
        builder.build()
    }
}

/// The state of [block] that is [state] with `name` set to `value`, `None` when the block has no
/// such property or no state carries that value.
fn with_property(
    block: &'static Block,
    state: &'static BlockState,
    name: &str,
    value: &str,
) -> Option<&'static BlockState> {
    // `state_property_pairs`, not `to_props`: a block registered at runtime may carry property
    // names the closed vanilla `Property` enum cannot hold (`receiver`, `double_face`, ...), and
    // those have to stay addressable by name like any other.
    let mut props: Vec<(&'static str, Cow<'_, str>)> =
        pumpkin_data::block_registry::state_property_pairs(block, state)
            .into_iter()
            .map(|(name, value)| (name, Cow::Borrowed(value)))
            .collect();
    let slot = props.iter_mut().find(|(prop, _)| *prop == name)?;
    slot.1 = Cow::Borrowed(value);
    block.states.iter().find(|candidate| {
        let candidate_props = pumpkin_data::block_registry::state_property_pairs(block, candidate);
        candidate_props.len() == props.len()
            && props.iter().all(|(prop, want)| {
                candidate_props
                    .iter()
                    .any(|(other, have)| other == prop && *have == want.as_ref())
            })
    })
}

impl BlockStateArgumentType {
    pub fn get<S: crate::source::CommandSource>(
        context: &CommandContext<S>,
        name: &str,
    ) -> Result<&'static BlockState, CommandSyntaxError> {
        context.get_argument::<&'static BlockState>(name).copied()
    }
}

#[cfg(test)]
mod test {
    use crate::{
        argument_types::argument_type::ArgumentType, argument_types::block::BlockStateArgumentType,
        string_reader::StringReader,
    };

    #[test]
    fn a_block_without_properties_is_its_default_state() {
        let mut reader = StringReader::new("minecraft:stone");
        let state = <BlockStateArgumentType as ArgumentType<crate::source::DummySource>>::parse(
            &BlockStateArgumentType,
            &mut reader,
        )
        .expect("stone parses");
        assert_eq!(state.id, pumpkin_data::Block::STONE.default_state.id);
    }

    #[test]
    fn the_given_properties_are_applied_to_the_default_state() {
        // stairs default to `facing=north`; the parser must move only that one property
        let mut reader = StringReader::new("minecraft:oak_stairs[facing=east]");
        let state = <BlockStateArgumentType as ArgumentType<crate::source::DummySource>>::parse(
            &BlockStateArgumentType,
            &mut reader,
        )
        .expect("oak stairs with a facing parse");
        assert_ne!(state.id, pumpkin_data::Block::OAK_STAIRS.default_state.id);
        assert_eq!(
            pumpkin_data::Block::OAK_STAIRS
                .properties(state.id)
                .expect("a state has properties")
                .to_props()
                .into_iter()
                .find(|prop| prop.key_str() == "facing")
                .map(|prop| prop.value_str()),
            Some("east"),
        );
    }

    #[test]
    fn several_properties_are_applied_together() {
        let mut reader = StringReader::new("minecraft:oak_stairs[facing=west,half=top]");
        let state = <BlockStateArgumentType as ArgumentType<crate::source::DummySource>>::parse(
            &BlockStateArgumentType,
            &mut reader,
        )
        .expect("two properties parse");
        let props = pumpkin_data::Block::OAK_STAIRS
            .properties(state.id)
            .expect("a state has properties")
            .to_props();
        assert!(
            props
                .iter()
                .any(|prop| prop.key_str() == "facing" && prop.value_str() == "west")
        );
        assert!(
            props
                .iter()
                .any(|prop| prop.key_str() == "half" && prop.value_str() == "top")
        );
    }

    #[test]
    fn a_property_the_block_does_not_have_is_rejected() {
        let mut reader = StringReader::new("minecraft:stone[facing=east]");
        assert_parse_err_reset!(
            &mut reader,
            BlockStateArgumentType,
            &super::INVALID_BLOCK_STATE_ERROR_TYPE
        );
    }

    #[test]
    fn a_value_the_property_does_not_allow_is_rejected() {
        let mut reader = StringReader::new("minecraft:oak_stairs[facing=sideways]");
        assert_parse_err_reset!(
            &mut reader,
            BlockStateArgumentType,
            &super::INVALID_BLOCK_STATE_ERROR_TYPE
        );
    }

    /// The string-pair accessor must not change what a *vanilla* block answers: for every state of
    /// a vanilla block it has to be the registry's own `to_props`, pair for pair. This is the pin
    /// that a command can still only accept exactly what it accepted before.
    #[test]
    fn vanilla_property_pairs_match_the_registry_answer() {
        use pumpkin_data::block_registry::state_property_pairs;
        for block in [&pumpkin_data::Block::OAK_STAIRS, &pumpkin_data::Block::OAK_LOG] {
            for state in block.states {
                let from_trait: Vec<(&'static str, &'static str)> = block
                    .properties(state.id)
                    .expect("a state of a vanilla block has properties")
                    .to_props()
                    .into_iter()
                    .map(|prop| (prop.key_str(), prop.value_str()))
                    .collect();
                let mut from_pairs = state_property_pairs(block, state);
                let mut from_trait_sorted = from_trait;
                from_pairs.sort_unstable();
                from_trait_sorted.sort_unstable();
                assert_eq!(from_pairs, from_trait_sorted);
            }
        }
    }

    /// A block registered at runtime may carry a property name the vanilla `Property` enum cannot
    /// represent. The command argument has to accept it like any vanilla one.
    #[test]
    fn a_runtime_property_is_addressable_by_name() {
        use pumpkin_data::block_registry::{
            BlockDrops, BlockRegistration, PropertyDefinition, PropertyKind, ShapeDefinition,
        };
        let block = pumpkin_data::Block::register_dynamic(BlockRegistration {
            key: "test_block:custom_prop_holder".to_string(),
            properties: vec![
                PropertyDefinition {
                    name: "facing".to_string(),
                    kind: PropertyKind::Enum(
                        ["north", "south"].into_iter().map(str::to_string).collect(),
                    ),
                },
                PropertyDefinition {
                    name: "custom_flag".to_string(),
                    kind: PropertyKind::Bool,
                },
            ],
            default_state: vec![
                ("facing".to_string(), "north".to_string()),
                ("custom_flag".to_string(), "false".to_string()),
            ],
            hardness: 1.0,
            blast_resistance: 1.0,
            requires_correct_tool: false,
            sound_type: "stone".to_string(),
            luminance: 0,
            can_occlude: true,
            replaceable: false,
            map_color: 0,
            collision_shape: ShapeDefinition::FullCube,
            selection_shape: ShapeDefinition::FullCube,
            shape_rules: Vec::new(),
            piston_behavior: pumpkin_data::block_state::PistonBehavior::Normal,
            connect_rules: Vec::new(),
            drops: BlockDrops::Nothing,
            tags: Vec::new(),
        })
        .expect("a fresh key registers");

        let mut reader = StringReader::new("test_block:custom_prop_holder[custom_flag=true]");
        let state = <BlockStateArgumentType as ArgumentType<crate::source::DummySource>>::parse(
            &BlockStateArgumentType,
            &mut reader,
        )
        .expect("the runtime property parses");
        let props = pumpkin_data::block_registry::state_property_pairs(block, state);
        assert!(
            props
                .iter()
                .any(|(name, value)| *name == "custom_flag" && *value == "true"),
            "custom_flag has to be true, got {props:?}"
        );
    }
}
