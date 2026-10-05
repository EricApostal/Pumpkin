//! Append only registries that plugins add entries to at runtime.
//!
//! The data crate is generated, so everything a plugin registers on top of the vanilla tables is
//! kept in a [`PluginRegistry`]: entries are leaked (so they can be `&'static`), get the index of
//! their registration and are never removed or reordered. A registry closes with
//! [`PluginRegistry::freeze`], which the server does before it accepts connections, because the ids
//! (`vanilla count + index`) are handed to clients.
//!
//! The same pattern as the item and block registries, shared by the block entity type, menu type and
//! data component type registries.

use std::collections::HashMap;
use std::fmt;

/// Namespace of the vanilla entries. Plugins cannot register in it.
pub const VANILLA_NAMESPACE: &str = "minecraft";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// The key is not a valid `namespace:path` resource location.
    InvalidKey(String),
    /// The `minecraft` namespace belongs to the vanilla entries.
    ReservedNamespace(String),
    /// An entry with this key exists already with another definition.
    Duplicate(String),
    /// The registry is closed, the server accepts connections.
    Frozen,
    /// There are no ids left.
    Full,
    /// The definition is invalid, the message says why.
    InvalidDefinition(String),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey(key) => {
                write!(f, "'{key}' is not a valid key, expected namespace:path")
            }
            Self::ReservedNamespace(key) => {
                write!(
                    f,
                    "'{key}' uses the reserved '{VANILLA_NAMESPACE}' namespace"
                )
            }
            Self::Duplicate(key) => {
                write!(f, "'{key}' is already registered with another definition")
            }
            Self::Frozen => write!(f, "can only be registered before players connect"),
            Self::Full => write!(f, "the registry is full"),
            Self::InvalidDefinition(message) => write!(f, "invalid definition: {message}"),
        }
    }
}

impl std::error::Error for RegistryError {}

fn is_valid_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
}

fn is_valid_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/'))
}

/// Splits `namespace:path` and checks both parts. The `minecraft` namespace is reserved.
pub fn split_plugin_key(key: &str) -> Result<(&str, &str), RegistryError> {
    let (namespace, path) = key
        .split_once(':')
        .ok_or_else(|| RegistryError::InvalidKey(key.to_string()))?;
    if !is_valid_namespace(namespace) || !is_valid_path(path) {
        return Err(RegistryError::InvalidKey(key.to_string()));
    }
    if namespace == VANILLA_NAMESPACE {
        return Err(RegistryError::ReservedNamespace(key.to_string()));
    }
    Ok((namespace, path))
}

/// Append only table of leaked entries, in registration order.
pub struct PluginRegistry<T: 'static> {
    frozen: bool,
    capacity: usize,
    entries: Vec<&'static T>,
    by_key: HashMap<&'static str, &'static T>,
}

impl<T: 'static> PluginRegistry<T> {
    /// A registry that holds at most `capacity` entries.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            frozen: false,
            capacity,
            entries: Vec::new(),
            by_key: HashMap::new(),
        }
    }

    pub const fn freeze(&mut self) {
        self.frozen = true;
    }

    #[must_use]
    pub const fn is_frozen(&self) -> bool {
        self.frozen
    }

    #[must_use]
    pub fn get_by_key(&self, key: &str) -> Option<&'static T> {
        self.by_key.get(key).copied()
    }

    #[must_use]
    pub fn get_by_index(&self, index: usize) -> Option<&'static T> {
        self.entries.get(index).copied()
    }

    /// The entries in registration order.
    #[must_use]
    pub fn entries(&self) -> &[&'static T] {
        &self.entries
    }

    /// Adds an entry. A key that is registered already returns the existing entry when
    /// `same_definition` accepts it (so a reloaded plugin can register again, even when the registry
    /// is closed) and is [`RegistryError::Duplicate`] otherwise. `create` gets the index of the new
    /// entry and the leaked key it is stored under.
    pub fn register(
        &mut self,
        key: &str,
        same_definition: impl FnOnce(&'static T) -> bool,
        create: impl FnOnce(usize, &'static str) -> T,
    ) -> Result<&'static T, RegistryError> {
        split_plugin_key(key)?;
        if let Some(existing) = self.by_key.get(key).copied() {
            return if same_definition(existing) {
                Ok(existing)
            } else {
                Err(RegistryError::Duplicate(key.to_string()))
            };
        }
        if self.frozen {
            return Err(RegistryError::Frozen);
        }
        let index = self.entries.len();
        if index >= self.capacity {
            return Err(RegistryError::Full);
        }
        let leaked_key: &'static str = Box::leak(key.to_string().into_boxed_str());
        let entry: &'static T = Box::leak(Box::new(create(index, leaked_key)));
        self.entries.push(entry);
        self.by_key.insert(leaked_key, entry);
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Entry {
        index: usize,
        key: &'static str,
    }

    #[test]
    fn registers_in_order_and_rejects_bad_keys() {
        let mut registry = PluginRegistry::<Entry>::new(2);
        let first = registry
            .register("test:first", |_| true, |index, key| Entry { index, key })
            .unwrap();
        assert_eq!((first.index, first.key), (0, "test:first"));
        assert!(matches!(
            registry.register("minecraft:x", |_| true, |index, key| Entry { index, key }),
            Err(RegistryError::ReservedNamespace(_))
        ));
        assert!(matches!(
            registry.register("nokey", |_| true, |index, key| Entry { index, key }),
            Err(RegistryError::InvalidKey(_))
        ));
        let again = registry
            .register("test:first", |_| true, |index, key| Entry { index, key })
            .unwrap();
        assert!(std::ptr::eq(first, again));
        assert!(matches!(
            registry.register("test:first", |_| false, |index, key| Entry { index, key }),
            Err(RegistryError::Duplicate(_))
        ));
        registry
            .register("test:second", |_| true, |index, key| Entry { index, key })
            .unwrap();
        assert!(matches!(
            registry.register("test:third", |_| true, |index, key| Entry { index, key }),
            Err(RegistryError::Full)
        ));
        registry.freeze();
        assert_eq!(registry.entries().len(), 2);
        assert!(registry.is_frozen());
    }
}
