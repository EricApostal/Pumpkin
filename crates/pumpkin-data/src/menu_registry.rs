//! Menu types registered by plugins on top of the vanilla ones (the `minecraft:menu` registry).
//!
//! A type registered as the n-th gets the network id `vanilla count + n`, the id a modded client's
//! registry sync announces for its menu type. The registry is append only and closes before players
//! can connect, see [`crate::plugin_registry`].

use std::sync::{LazyLock, PoisonError, RwLock};

use crate::plugin_registry::{PluginRegistry, RegistryError};
use crate::screen::WindowType;

/// Menu type ids are a `VarInt`, this is far more than a plugin needs.
const MAX_PLUGIN_MENU_TYPES: usize = 4096;

/// A menu type that a plugin registered.
pub struct PluginMenuType {
    /// The full resource location, `namespace:path`.
    pub key: &'static str,
    /// The network id: the vanilla count plus the registration index.
    pub id: u32,
}

static REGISTRY: LazyLock<RwLock<PluginRegistry<PluginMenuType>>> =
    LazyLock::new(|| RwLock::new(PluginRegistry::new(MAX_PLUGIN_MENU_TYPES)));

fn read_registry() -> std::sync::RwLockReadGuard<'static, PluginRegistry<PluginMenuType>> {
    REGISTRY.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_registry() -> std::sync::RwLockWriteGuard<'static, PluginRegistry<PluginMenuType>> {
    REGISTRY.write().unwrap_or_else(PoisonError::into_inner)
}

/// Number of vanilla menu types. Plugin types get the ids from here on. `WindowType` lists the
/// vanilla menu registry in order and ends with the stonecutter.
#[must_use]
pub const fn vanilla_menu_type_count() -> u32 {
    WindowType::Stonecutter as u32 + 1
}

/// Registers a menu type. Registering the same key again returns the existing type.
pub fn register_plugin_menu_type(key: &str) -> Result<&'static PluginMenuType, RegistryError> {
    let vanilla_count = vanilla_menu_type_count();
    write_registry().register(
        key,
        |_| true,
        move |index, key| PluginMenuType {
            key,
            id: vanilla_count + index as u32,
        },
    )
}

/// Closes the registry. Later registrations fail with [`RegistryError::Frozen`].
pub fn freeze_plugin_menu_types() {
    write_registry().freeze();
}

#[must_use]
pub fn is_plugin_menu_registry_frozen() -> bool {
    read_registry().is_frozen()
}

/// The registered types in id order.
#[must_use]
pub fn plugin_menu_types() -> Vec<&'static PluginMenuType> {
    read_registry().entries().to_vec()
}

#[must_use]
pub fn plugin_menu_type_by_key(key: &str) -> Option<&'static PluginMenuType> {
    read_registry().get_by_key(key)
}

#[must_use]
pub fn plugin_menu_type_by_id(id: u32) -> Option<&'static PluginMenuType> {
    let index = id.checked_sub(vanilla_menu_type_count())?;
    read_registry().get_by_index(index as usize)
}
