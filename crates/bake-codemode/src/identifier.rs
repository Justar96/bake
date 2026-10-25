//! Script identifiers for tool names, ported from Pi's
//! `packages/codemode/src/identifier.ts` (v1.3.0; MIT, see `NOTICE`).

/// The identifier a script uses for a tool: characters that are not valid in
/// a JavaScript identifier become `_`, one per character. `mcp__docs__search`
/// stays as is, `my-tool` becomes `my_tool`, and an empty name becomes `_`.
pub fn to_codemode_identifier(name: &str) -> String {
    let mut identifier = String::with_capacity(name.len());
    for character in name.chars() {
        let valid = if identifier.is_empty() {
            character.is_ascii_alphabetic() || character == '_' || character == '$'
        } else {
            character.is_ascii_alphanumeric() || character == '_' || character == '$'
        };
        identifier.push(if valid { character } else { '_' });
    }
    if identifier.is_empty() {
        identifier.push('_');
    }
    identifier
}

/// Whether `name` is an ASCII JavaScript identifier (`^[A-Za-z_$][A-Za-z0-9_$]*$`).
pub(crate) fn is_identifier(name: &str) -> bool {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' || first == '$' => {}
        _ => return false,
    }
    characters
        .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '$')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_invalid_characters() {
        assert_eq!(
            to_codemode_identifier("mcp__docs__search"),
            "mcp__docs__search"
        );
        assert_eq!(to_codemode_identifier("my-tool"), "my_tool");
        assert_eq!(to_codemode_identifier("9lives"), "_lives");
        assert_eq!(to_codemode_identifier("a.b€"), "a_b_");
        assert_eq!(to_codemode_identifier(""), "_");
        assert!(is_identifier("$a_1"));
        assert!(!is_identifier("1a"));
        assert!(!is_identifier("a-b"));
        assert!(!is_identifier(""));
    }
}
