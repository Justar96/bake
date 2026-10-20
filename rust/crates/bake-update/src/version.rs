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

    mod properties {
        use proptest::prelude::*;

        use super::*;

        /// Release versions as manifests may name them, with prerelease
        /// identifiers that are numeric, alphanumeric, or numeric with
        /// leading zeros, which the 0.3 pattern accepts. The values are few,
        /// so generated versions often tie or share a core.
        fn version() -> impl Strategy<Value = String> {
            let core = prop_oneof![Just((1, 0, 0)), (0..2u64, 0..2u64, 0..2u64)];
            let identifier = prop::sample::select(vec!["0", "1", "2", "01", "00", "a", "b", "a-1"]);
            let pre = prop::option::weighted(0.8, prop::collection::vec(identifier, 1..4));
            (core, pre).prop_map(|((major, minor, patch), pre)| match pre {
                None => format!("{major}.{minor}.{patch}"),
                Some(ids) => format!("{major}.{minor}.{patch}-{}", ids.join(".")),
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(2048))]

            #[test]
            fn generated_versions_are_release_versions(value in version()) {
                prop_assert!(is_release_version(&value), "{value}");
            }

            #[test]
            fn version_order_is_antisymmetric_and_reflexive(
                left in version(),
                right in version(),
            ) {
                prop_assert_eq!(compare_versions(&left, &left), Ordering::Equal);
                prop_assert_eq!(
                    compare_versions(&left, &right),
                    compare_versions(&right, &left).reverse()
                );
            }

            #[test]
            fn version_order_is_transitive(
                a in version(),
                b in version(),
                c in version(),
            ) {
                let mut sorted = [a, b, c];
                sorted.sort_by(|left, right| compare_versions(left, right));
                let [low, middle, high] = &sorted;
                prop_assert_ne!(compare_versions(low, middle), Ordering::Greater);
                prop_assert_ne!(compare_versions(middle, high), Ordering::Greater);
                prop_assert_ne!(
                    compare_versions(low, high),
                    Ordering::Greater,
                    "{} {} {}", low, middle, high
                );
                if compare_versions(low, middle) == Ordering::Equal
                    && compare_versions(middle, high) == Ordering::Equal
                {
                    prop_assert_eq!(compare_versions(low, high), Ordering::Equal);
                }
                if compare_versions(low, middle) == Ordering::Less
                    || compare_versions(middle, high) == Ordering::Less
                {
                    prop_assert_eq!(compare_versions(low, high), Ordering::Less);
                }
            }

            #[test]
            fn versions_that_order_equal_order_alike_against_any_other(
                left in version(),
                right in version(),
                other in version(),
            ) {
                if compare_versions(&left, &right) == Ordering::Equal {
                    prop_assert_eq!(
                        compare_versions(&left, &other),
                        compare_versions(&right, &other),
                        "{} = {} against {}", left, right, other
                    );
                }
            }

            #[test]
            fn a_prerelease_sorts_before_its_release_and_after_the_one_before(
                major in 0..1000u64,
                minor in 0..1000u64,
                patch in 1..1000u64,
                pre in "[0-9a-z][0-9a-z.-]{0,6}",
            ) {
                let release = format!("{major}.{minor}.{patch}");
                let prerelease = format!("{release}-{pre}");
                let before = format!("{major}.{minor}.{}", patch - 1);
                prop_assert_eq!(compare_versions(&prerelease, &release), Ordering::Less);
                prop_assert_eq!(compare_versions(&before, &prerelease), Ordering::Less);
            }

            #[test]
            fn prerelease_identifiers_order_numbers_by_value_then_words_and_a_prefix_first(
                left in any::<u64>(),
                right in any::<u64>(),
                word in "[a-z-][0-9a-z-]{0,5}",
                more in "[0-9a-z-]{1,5}",
            ) {
                let at = |pre: &str| format!("1.2.3-{pre}");
                let order = left.cmp(&right);
                let (left, right) = (left.to_string(), right.to_string());
                prop_assert_eq!(compare_versions(&at(&left), &at(&right)), order);
                prop_assert_eq!(compare_versions(&at(&left), &at(&word)), Ordering::Less);
                prop_assert_eq!(
                    compare_versions(&at(&word), &at(&format!("{word}.{more}"))),
                    Ordering::Less
                );
            }

            #[test]
            fn release_versions_order_by_their_numbers(
                left in (0..u64::MAX, 0..u64::MAX, 0..u64::MAX),
                right in (0..u64::MAX, 0..u64::MAX, 0..u64::MAX),
            ) {
                let text = |(major, minor, patch): (u64, u64, u64)| format!("{major}.{minor}.{patch}");
                prop_assert_eq!(compare_versions(&text(left), &text(right)), left.cmp(&right));
            }
        }
    }
}
