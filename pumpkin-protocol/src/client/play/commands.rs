use bytes::BufMut;
use pumpkin_data::packet::clientbound::PLAY_COMMANDS;
use pumpkin_macros::packet;

use crate::{ClientPacket, VarInt, bytebuf::ByteBufMut};

#[packet(PLAY_COMMANDS)]
pub struct CCommands<'a> {
    pub nodes: Vec<ProtoNode<'a>>,
    pub root_node_index: VarInt,
}

impl<'a> CCommands<'a> {
    pub fn new(nodes: Vec<ProtoNode<'a>>, root_node_index: VarInt) -> Self {
        Self {
            nodes,
            root_node_index,
        }
    }
}

impl ClientPacket for CCommands<'_> {
    fn write(&self, bytebuf: &mut impl BufMut) {
        bytebuf.put_list(&self.nodes, |bytebuf, node: &ProtoNode| {
            node.write_to(bytebuf)
        });
        bytebuf.put_var_int(&self.root_node_index);
    }
}

pub struct ProtoNode<'a> {
    pub children: Vec<VarInt>,
    pub node_type: ProtoNodeType<'a>,
}

#[derive(Debug)]
pub enum ProtoNodeType<'a> {
    Root,
    Literal {
        name: &'a str,
        is_executable: bool,
    },
    Argument {
        name: &'a str,
        is_executable: bool,
        parser: ArgumentType<'a>,
        override_suggestion_type: Option<SuggestionProviders>,
    },
}

impl ProtoNode<'_> {
    const FLAG_IS_EXECUTABLE: u8 = 4;
    const FLAG_HAS_REDIRECT: u8 = 8;
    const FLAG_HAS_SUGGESTION_TYPE: u8 = 16;

    pub fn write_to(&self, bytebuf: &mut impl BufMut) {
        // flags
        let flags = match self.node_type {
            ProtoNodeType::Root => 0,
            ProtoNodeType::Literal {
                name: _,
                is_executable,
            } => {
                let mut n = 1;
                if is_executable {
                    n |= Self::FLAG_IS_EXECUTABLE
                }
                n
            }
            ProtoNodeType::Argument {
                name: _,
                is_executable,
                parser: _,
                override_suggestion_type,
            } => {
                let mut n = 2;
                if override_suggestion_type.is_some() {
                    n |= Self::FLAG_HAS_SUGGESTION_TYPE
                }
                if is_executable {
                    n |= Self::FLAG_IS_EXECUTABLE
                }
                n
            }
        };
        bytebuf.put_u8(flags);

        // child count + children
        bytebuf.put_list(&self.children, |bytebuf, child| bytebuf.put_var_int(child));

        // redirect node
        if flags & Self::FLAG_HAS_REDIRECT != 0 {
            bytebuf.put_var_int(&1.into());
        }

        // name
        match self.node_type {
            ProtoNodeType::Argument { name, .. } | ProtoNodeType::Literal { name, .. } => {
                bytebuf.put_string(name)
            }
            ProtoNodeType::Root => {}
        }

        // parser id + properties
        if let ProtoNodeType::Argument {
            name: _,
            is_executable: _,
            parser,
            override_suggestion_type: _,
        } = &self.node_type
        {
            parser.write_to_buffer(bytebuf)
        }

        if flags & Self::FLAG_HAS_SUGGESTION_TYPE != 0 {
            match &self.node_type {
                ProtoNodeType::Argument {
                    name: _,
                    is_executable: _,
                    parser: _,
                    override_suggestion_type,
                } => {
                    // suggestion type
                    let suggestion_type = &override_suggestion_type.expect("ProtoNode::FLAG_HAS_SUGGESTION_TYPE should only be set if override_suggestion_type is not `None`.");
                    bytebuf.put_string(suggestion_type.identifier());
                }
                _ => unimplemented!(
                    "`ProtoNode::FLAG_HAS_SUGGESTION_TYPE` is only implemented for `ProtoNodeType::Argument`"
                ),
            }
        }
    }
}

#[derive(Debug, Clone)]
#[repr(u32)]
pub enum ArgumentType<'a> {
    Bool,
    Float { min: Option<f32>, max: Option<f32> },
    Double { min: Option<f64>, max: Option<f64> },
    Integer { min: Option<i32>, max: Option<i32> },
    Long { min: Option<i64>, max: Option<i64> },
    String(StringProtoArgBehavior),
    Entity { flags: u8 },
    GameProfile,
    BlockPos,
    ColumnPos,
    Vec3,
    Vec2,
    BlockState,
    BlockPredicate,
    ItemStack,
    ItemPredicate,
    Color,
    Component,
    Style,
    Message,
    Nbt,
    NbtTag,
    NbtPath,
    Objective,
    ObjectiveCriteria,
    Operation,
    Particle,
    Angle,
    Rotation,
    ScoreboardSlot,
    ScoreHolder { flags: u8 },
    Swizzle,
    Team,
    ItemSlot,
    ItemSlots,
    ResourceLocation,
    Function,
    EntityAnchor,
    IntRange,
    FloatRange,
    Dimension,
    Gamemode,
    Time { min: i32 },
    ResourceOrTag { identifier: &'a str },
    ResourceOrTagKey { identifier: &'a str },
    Resource { identifier: &'a str },
    ResourceKey { identifier: &'a str },
    TemplateMirror,
    TemplateRotation,
    Heightmap,
    LootTable,
    LootPredicate,
    LootModifier,
    Uuid,
}

impl ArgumentType<'_> {
    pub const ENTITY_FLAG_ONLY_SINGLE: u8 = 1;
    pub const ENTITY_FLAG_PLAYERS_ONLY: u8 = 2;

    pub const SCORE_HOLDER_FLAG_ALLOW_MULTIPLE: u8 = 1;

    pub fn write_to_buffer(&self, bytebuf: &mut impl BufMut) {
        let id = unsafe { *(self as *const Self as *const i32) };
        bytebuf.put_var_int(&(id).into());
        match self {
            Self::Float { min, max } => Self::write_number_arg(*min, *max, bytebuf),
            Self::Double { min, max } => Self::write_number_arg(*min, *max, bytebuf),
            Self::Integer { min, max } => Self::write_number_arg(*min, *max, bytebuf),
            Self::Long { min, max } => Self::write_number_arg(*min, *max, bytebuf),
            Self::String(behavior) => {
                let i = match behavior {
                    StringProtoArgBehavior::SingleWord => 0,
                    StringProtoArgBehavior::QuotablePhrase => 1,
                    StringProtoArgBehavior::GreedyPhrase => 2,
                };
                bytebuf.put_var_int(&i.into());
            }
            Self::Entity { flags } => Self::write_with_flags(*flags, bytebuf),
            Self::ScoreHolder { flags } => Self::write_with_flags(*flags, bytebuf),
            Self::Time { min } => {
                bytebuf.put_i32(*min);
            }
            Self::ResourceOrTag { identifier } => Self::write_with_identifier(identifier, bytebuf),
            Self::ResourceOrTagKey { identifier } => {
                Self::write_with_identifier(identifier, bytebuf)
            }
            Self::Resource { identifier } => Self::write_with_identifier(identifier, bytebuf),
            Self::ResourceKey { identifier } => Self::write_with_identifier(identifier, bytebuf),
            _ => {}
        }
    }

    fn write_number_arg<T: NumberCmdArg>(
        min: Option<T>,
        max: Option<T>,
        bytebuf: &mut impl BufMut,
    ) {
        let mut flags: u8 = 0;
        if min.is_some() {
            flags |= 1
        }
        if max.is_some() {
            flags |= 2
        }

        bytebuf.put_u8(flags);
        if let Some(min) = min {
            min.write(bytebuf);
        }
        if let Some(max) = max {
            max.write(bytebuf);
        }
    }

    fn write_with_flags(flags: u8, bytebuf: &mut impl BufMut) {
        bytebuf.put_u8(flags);
    }

    fn write_with_identifier(extra_identifier: &str, bytebuf: &mut impl BufMut) {
        bytebuf.put_string(extra_identifier);
    }
}

#[derive(Debug, Clone, Copy)]
pub enum StringProtoArgBehavior {
    SingleWord,
    QuotablePhrase,
    /// does not stop after a space
    GreedyPhrase,
}

trait NumberCmdArg {
    fn write(self, bytebuf: &mut impl BufMut);
}

impl NumberCmdArg for f32 {
    fn write(self, bytebuf: &mut impl BufMut) {
        bytebuf.put_f32(self);
    }
}

impl NumberCmdArg for f64 {
    fn write(self, bytebuf: &mut impl BufMut) {
        bytebuf.put_f64(self);
    }
}

impl NumberCmdArg for i32 {
    fn write(self, bytebuf: &mut impl BufMut) {
        bytebuf.put_i32(self);
    }
}

impl NumberCmdArg for i64 {
    fn write(self, bytebuf: &mut impl BufMut) {
        bytebuf.put_i64(self);
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SuggestionProviders {
    AskServer,
    AllRecipes,
    AvailableSounds,
    SummonableEntities,
}

impl SuggestionProviders {
    fn identifier(&self) -> &'static str {
        match self {
            Self::AskServer => "minecraft:ask_server",
            Self::AllRecipes => "minecraft:all_recipes",
            Self::AvailableSounds => "minecraft:available_sounds",
            Self::SummonableEntities => "minecraft:summonable_entities",
        }
    }
}
