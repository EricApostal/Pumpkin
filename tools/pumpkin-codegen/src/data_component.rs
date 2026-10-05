use heck::ToPascalCase;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;

#[derive(Deserialize, Clone, Copy)]
#[serde(untagged)]
pub enum DataComponentValue {
    Simple(u8),
    Detailed {
        id: u8,
        #[serde(default = "default_persistent")]
        persistent: bool,
        #[serde(default)]
        ignore_swap_animation: bool,
    },
}

const fn default_persistent() -> bool {
    true
}

impl DataComponentValue {
    pub const fn id(&self) -> u8 {
        match self {
            Self::Simple(id) => *id,
            Self::Detailed { id, .. } => *id,
        }
    }

    pub const fn persistent(&self) -> bool {
        match self {
            Self::Simple(_) => true,
            Self::Detailed { persistent, .. } => *persistent,
        }
    }

    pub const fn ignore_swap_animation(&self) -> bool {
        match self {
            Self::Simple(_) => false,
            Self::Detailed {
                ignore_swap_animation,
                ..
            } => *ignore_swap_animation,
        }
    }
}

/// Number of ids after the vanilla ones that plugin registered component types can take. Component
/// ids are a byte on the wire and in `DataComponent`, so each possible type needs its own variant.
const PLUGIN_SLOTS: u8 = 64;

/// Generates the `TokenStream` for the `DataComponent` enum and its ID/name conversion methods.
pub fn build() -> TokenStream {
    let data_component: BTreeMap<String, DataComponentValue> =
        serde_json::from_str(&fs::read_to_string("../../assets/data_component.json").unwrap())
            .expect("Failed to parse data_component.json");

    let mut enum_variants = TokenStream::new();
    let mut id_to_enum = TokenStream::new();
    let mut enum_to_name = TokenStream::new();
    let mut name_to_enum = TokenStream::new();
    let mut enum_to_persistent = TokenStream::new();
    let mut enum_to_ignore_swap = TokenStream::new();

    let mut data_component_vec = data_component.iter().collect::<Vec<_>>();
    data_component_vec.sort_by_key(|(_, val)| val.id());

    for (raw_name, component_val) in &data_component_vec {
        let raw_value = component_val.id();
        let persistent = component_val.persistent();
        let ignore_swap = component_val.ignore_swap_animation();

        let strip_name = raw_name
            .strip_prefix("minecraft:")
            .unwrap()
            .replace('/', "_");
        let pascal_case = format_ident!("{}", strip_name.to_pascal_case());

        // Enum variant

        enum_variants.extend(quote! {
            #pascal_case = #raw_value,
        });

        id_to_enum.extend(quote! {
            #raw_value => Some(Self::#pascal_case),
        });

        // TODO use phf
        name_to_enum.extend(quote! {
            #raw_name | #strip_name => Some(Self::#pascal_case),
        });

        // Enum -> &str
        enum_to_name.extend(quote! {
            Self::#pascal_case => #raw_name,
        });

        enum_to_persistent.extend(quote! {
            Self::#pascal_case => #persistent,
        });

        enum_to_ignore_swap.extend(quote! {
            Self::#pascal_case => #ignore_swap,
        });
    }

    // Ids `vanilla_count..vanilla_count + PLUGIN_SLOTS` belong to component types that plugins
    // register at runtime (see `data_component_registry`). The id of a type is the vanilla count
    // plus its registration index, which is what a modded client's registry sync announces.
    let vanilla_count = data_component_vec
        .last()
        .map_or(0u8, |(_, value)| value.id() + 1);
    assert!(
        u16::from(vanilla_count) + u16::from(PLUGIN_SLOTS) <= 256,
        "no room for plugin component ids"
    );
    for slot in 0..PLUGIN_SLOTS {
        let value = vanilla_count + slot;
        let pascal_case = format_ident!("PluginComponent{}", slot);
        let name = format!("pumpkin:plugin_component_{slot}");

        enum_variants.extend(quote! {
            #pascal_case = #value,
        });
        id_to_enum.extend(quote! {
            #value => Some(Self::#pascal_case),
        });
        enum_to_name.extend(quote! {
            Self::#pascal_case => #name,
        });
        // Whether a plugin component is saved is decided by its registered type, see `ItemStack`.
        enum_to_persistent.extend(quote! {
            Self::#pascal_case => true,
        });
        enum_to_ignore_swap.extend(quote! {
            Self::#pascal_case => false,
        });
    }

    quote! {
        use crate::data_component_impl::*;

        #[derive(Copy, Clone, Hash, PartialEq, Eq, Debug)]
        #[repr(u8)]
        pub enum DataComponent {
            #enum_variants
        }

        impl DataComponent {
            /// Number of vanilla components. Component ids from here on are plugin slots.
            pub const VANILLA_COUNT: u8 = #vanilla_count;
            /// Number of component types plugins can register.
            pub const PLUGIN_SLOT_COUNT: u8 = #PLUGIN_SLOTS;

            #[must_use]
            pub const fn to_id(self) -> u8 {
                self as u8
            }

            /// The index of a plugin component slot, `None` for a vanilla component.
            #[must_use]
            pub const fn plugin_slot(self) -> Option<u8> {
                let id = self as u8;
                if id >= Self::VANILLA_COUNT {
                    Some(id - Self::VANILLA_COUNT)
                } else {
                    None
                }
            }

            /// The component of a plugin slot, `None` if the slot does not exist.
            #[must_use]
            pub const fn from_plugin_slot(slot: u8) -> Option<Self> {
                if slot >= Self::PLUGIN_SLOT_COUNT {
                    return None;
                }
                Self::try_from_id(Self::VANILLA_COUNT + slot)
            }

            #[must_use]
            #[allow(clippy::too_many_lines)]
            pub const fn try_from_id(id: u8) -> Option<Self> {
                match id {
                    #id_to_enum
                    _ => None,
                }
            }

            #[must_use]
            #[allow(clippy::too_many_lines)]
            pub fn try_from_name(name: &str) -> Option<Self> {
                match name {
                    #name_to_enum
                    _ => None,
                }
            }

            #[must_use]
            #[allow(clippy::too_many_lines)]
            pub const fn to_name(self) -> &'static str {
                match self {
                    #enum_to_name
                }
            }

            #[must_use]
            #[allow(clippy::too_many_lines)]
            pub const fn is_persistent(self) -> bool {
                match self {
                    #enum_to_persistent
                }
            }

            #[must_use]
            #[inline]
            pub const fn is_transient(self) -> bool {
                !self.is_persistent()
            }

            #[must_use]
            #[allow(clippy::too_many_lines)]
            pub const fn ignore_swap_animation(self) -> bool {
                match self {
                    #enum_to_ignore_swap
                }
            }
        }
    }
}
