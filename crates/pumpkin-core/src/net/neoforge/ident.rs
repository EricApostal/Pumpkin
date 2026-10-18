//! Resource locations (identifiers) as the `NeoForge` payloads carry them.

use super::buf::CodecError;

/// The namespace of an identifier without one.
pub const DEFAULT_NAMESPACE: &str = "minecraft";

const fn is_namespace_char(c: char) -> bool {
    matches!(c, 'a'..='z' | '0'..='9' | '_' | '.' | '-')
}

const fn is_path_char(c: char) -> bool {
    is_namespace_char(c) || c == '/'
}

/// Whether `text` is a valid namespace, which is also what a mod id is.
#[must_use]
pub fn is_valid_namespace(text: &str) -> bool {
    !text.is_empty() && text.chars().all(is_namespace_char)
}

/// Parses like `Identifier.parse` and returns the canonical `namespace:path` form. An identifier
/// without a colon, or with an empty namespace, is in the `minecraft` namespace.
pub fn normalize(text: &str) -> Result<String, CodecError> {
    let (namespace, path) = text.split_once(':').unwrap_or((DEFAULT_NAMESPACE, text));
    let namespace = if namespace.is_empty() {
        DEFAULT_NAMESPACE
    } else {
        namespace
    };
    if !namespace.chars().all(is_namespace_char) {
        return Err(CodecError::new(format!(
            "'{text}' has an invalid namespace"
        )));
    }
    if path.is_empty() || !path.chars().all(is_path_char) {
        return Err(CodecError::new(format!("'{text}' has an invalid path")));
    }
    Ok(format!("{namespace}:{path}"))
}

/// The namespace of a canonical identifier.
#[must_use]
pub fn namespace_of(identifier: &str) -> &str {
    identifier
        .split_once(':')
        .map_or("", |(namespace, _)| namespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_validates() {
        assert_eq!(normalize("stone").unwrap(), "minecraft:stone");
        assert_eq!(normalize(":stone").unwrap(), "minecraft:stone");
        assert_eq!(normalize("a-b.c:d/e_f").unwrap(), "a-b.c:d/e_f");
        for bad in ["", "A:b", "a:", "a:B", "a b:c", "a:b:c"] {
            assert!(normalize(bad).is_err(), "{bad}");
        }
        assert_eq!(namespace_of("mod:thing"), "mod");
    }
}
