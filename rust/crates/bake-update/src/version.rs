//! Release version syntax and ordering, ported from Bake 0.3's
//! `packages/boot/updater/src/version.ts`: `MAJOR.MINOR.PATCH` with an
//! optional prerelease, compared by semantic-versioning precedence.

use std::cmp::Ordering;

/// Whether `value` is a release version a native manifest may name.
pub fn is_release_version(value: &str) -> bool {
    let (core, pre) = split(value);
    let mut parts = core.split('.');
    let numbers = [parts.next(), parts.next(), parts.next()];
    if parts.next().is_some() {
        return false;
    }
    if !numbers.iter().all(|part| {
        part.is_some_and(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    }) {
        return false;
    }
    match pre {
        None => true,
        // The 0.3 pattern: one or more identifier characters, dots included.
        Some(pre) => {
            !pre.is_empty()
                && pre
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        }
    }
}

/// Order two release versions by semantic-versioning precedence: a
/// prerelease sorts before its release, numeric identifiers compare as
/// numbers and before alphanumeric ones. Build metadata is not syntax a
/// manifest may carry, so it is not handled.
pub fn compare_versions(left: &str, right: &str) -> Ordering {
    let (left_core, left_pre) = split(left);
    let (right_core, right_pre) = split(right);
    let mut lefts = left_core.split('.');
    let mut rights = right_core.split('.');
    for _ in 0..3 {
        let order = number(lefts.next()).cmp(&number(rights.next()));
        if order != Ordering::Equal {
            return order;
        }
    }
    match (left_pre, right_pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => compare_prerelease(left, right),
    }
}

fn compare_prerelease(left: &str, right: &str) -> Ordering {
    let mut lefts = left.split('.');
    let mut rights = right.split('.');
    loop {
        match (lefts.next(), rights.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) if left == right => continue,
            (Some(left), Some(right)) => {
                let order = match (numeric(left), numeric(right)) {
                    (Some(left), Some(right)) => left.cmp(&right),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => left.cmp(right),
                };
                // Identifiers that differ only in leading zeros (`01`, `1`)
                // tie; the identifiers after them decide.
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// `value` with its prerelease split off.
fn split(value: &str) -> (&str, Option<&str>) {
    match value.find('-') {
        None => (value, None),
        Some(dash) => (&value[..dash], Some(&value[dash + 1..])),
    }
}

/// A core version part as a number; an absent or unparsable one is 0, as
/// `compareVersions` treats a missing part.
fn number(part: Option<&str>) -> u64 {
    part.and_then(|part| part.parse().ok()).unwrap_or(0)
}

/// A prerelease identifier's numeric value, when it is all digits.
fn numeric(identifier: &str) -> Option<u64> {
    if identifier.is_empty() || !identifier.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    identifier.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_version_is_three_numbers_and_an_optional_prerelease() {
        for value in ["0.4.0", "1.2.3", "0.4.0-dev", "10.0.0-rc.1", "0.0.0-a-b.2"] {
            assert!(is_release_version(value), "{value}");
        }
        for value in [
            "",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "1.2.x",
            "1.2.3-",
            "1.2.3+build",
            "1.2.3-a_b",
            " 1.2.3",
            "1.2.3 ",
            "01.2.3-ok",
        ] {
            // Leading zeros are accepted, as the 0.3 pattern accepts them.
            if value == "01.2.3-ok" {
                assert!(is_release_version(value));
                continue;
            }
            assert!(!is_release_version(value), "{value}");
        }
    }

    #[test]
    fn versions_order_by_semantic_precedence() {
        let ordered = [
            "0.4.0-alpha",
            "0.4.0-alpha.1",
            "0.4.0-alpha.beta",
            "0.4.0-beta",
            "0.4.0-beta.2",
            "0.4.0-beta.11",
            "0.4.0-rc.1",
            "0.4.0",
            "0.4.1",
            "0.5.0",
            "1.0.0",
        ];
        for (index, left) in ordered.iter().enumerate() {
            assert_eq!(compare_versions(left, left), Ordering::Equal, "{left}");
            for right in &ordered[index + 1..] {
                assert_eq!(
                    compare_versions(left, right),
                    Ordering::Less,
                    "{left} {right}"
                );
                assert_eq!(
                    compare_versions(right, left),
                    Ordering::Greater,
                    "{right} {left}"
                );
            }
        }
    }

    #[test]
    fn numeric_identifiers_that_tie_leave_the_order_to_the_rest() {
        // Leading zeros are accepted, so `01` and `1` tie as numbers; the
        // order then comes from the identifiers after them, keeping it
        // transitive: `-01` and `-1` both sort before `-1.0`.
        assert_eq!(compare_versions("1.0.0-01", "1.0.0-1"), Ordering::Equal);
        assert_eq!(compare_versions("1.0.0-01", "1.0.0-1.0"), Ordering::Less);
        assert_eq!(compare_versions("1.0.0-1.0", "1.0.0-01"), Ordering::Greater);
        assert_eq!(
            compare_versions("1.0.0-01.b", "1.0.0-1.a"),
            Ordering::Greater
        );
        assert_eq!(compare_versions("1.0.0-00.0", "1.0.0-0"), Ordering::Greater);
    }
}
