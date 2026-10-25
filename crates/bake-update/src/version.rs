//! Release version syntax and ordering: `MAJOR.MINOR.PATCH` with an
//! optional prerelease, in canonical semantic-versioning form, compared by
//! semantic-versioning precedence. The ordering is ported from Bake 0.3's
//! `packages/boot/updater/src/version.ts`; the syntax is stricter than its
//! pattern, which accepted leading zeros and empty prerelease identifiers.
//! Over canonical versions the order is total: two versions compare equal
//! only when they are the same text, so a manifest can never offer a
//! different release as the installed one, and the order is transitive.

use std::cmp::Ordering;

/// Whether `value` is a release version a native manifest may name:
/// canonical semantic versioning without build metadata. Each core number
/// and each numeric prerelease identifier is `0` or has no leading zero;
/// each prerelease identifier is non-empty ASCII alphanumerics and `-`.
pub fn is_release_version(value: &str) -> bool {
    let (core, pre) = split(value);
    let mut parts = core.split('.');
    let numbers = [parts.next(), parts.next(), parts.next()];
    if parts.next().is_some() || !numbers.iter().all(|part| part.is_some_and(is_number)) {
        return false;
    }
    match pre {
        None => true,
        Some(pre) => pre.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && (!is_digits(identifier) || is_number(identifier))
        }),
    }
}

/// Order two release versions by semantic-versioning precedence: a
/// prerelease sorts before its release, numeric identifiers compare as
/// numbers of any size and before alphanumeric ones. Build metadata is not
/// syntax a manifest may carry, so it is not handled. Text that is not a
/// [release version](is_release_version) is ordered without panicking, but
/// that order is not specified.
pub fn compare_versions(left: &str, right: &str) -> Ordering {
    let (left_core, left_pre) = split(left);
    let (right_core, right_pre) = split(right);
    let mut lefts = left_core.split('.');
    let mut rights = right_core.split('.');
    for _ in 0..3 {
        let order = compare_numbers(
            lefts.next().filter(|part| is_digits(part)).unwrap_or("0"),
            rights.next().filter(|part| is_digits(part)).unwrap_or("0"),
        );
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
            (Some(left), Some(right)) => {
                let order = match (is_digits(left), is_digits(right)) {
                    (true, true) => compare_numbers(left, right),
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => left.cmp(right),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// `value` with its prerelease split off.
fn split(value: &str) -> (&str, Option<&str>) {
    match value.split_once('-') {
        None => (value, None),
        Some((core, pre)) => (core, Some(pre)),
    }
}

/// Whether `part` is a non-empty run of ASCII digits.
fn is_digits(part: &str) -> bool {
    !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())
}

/// Whether `part` is a canonical number: `0`, or digits without a leading
/// zero.
fn is_number(part: &str) -> bool {
    is_digits(part) && (part == "0" || !part.starts_with('0'))
}

/// Order two runs of digits by value, whatever their length: without leading
/// zeros the shorter is smaller, and runs of one length order as text.
fn compare_numbers(left: &str, right: &str) -> Ordering {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_version_is_canonical_semantic_versioning() {
        for value in [
            "0.4.0",
            "1.2.3",
            "0.4.0-dev",
            "10.0.0-rc.1",
            "0.0.0-a-b.2",
            "1.0.0-0",
            "1.0.0-0a",
            "1.0.0--",
            "1.0.0-x.7.z.92",
            "18446744073709551616.0.0-18446744073709551616",
        ] {
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
            "..",
            "1..3",
            // Leading zeros and empty identifiers, which the 0.3 pattern
            // accepted and which made the order non-total.
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "01.2.3-ok",
            "1.0.0-01",
            "1.0.0-00",
            "1.0.0-rc.01",
            "1.0.0-a..b",
            "1.0.0-.a",
            "1.0.0-a.",
        ] {
            assert!(!is_release_version(value), "{value}");
        }
    }

    #[test]
    fn versions_order_by_semantic_precedence() {
        // The precedence example of semver.org section 11, then releases.
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
            "18446744073709551615.0.0",
            "18446744073709551616.0.0",
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

    /// Recorded version order from the native release contract.
    const SHARED_ORDER: &str = include_str!("../tests/fixtures/native-version-order.txt");

    #[test]
    fn the_shared_case_table_is_in_ascending_order() {
        let ordered: Vec<&str> = SHARED_ORDER
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        assert!(ordered.len() > 30, "{}", ordered.len());
        for (index, left) in ordered.iter().enumerate() {
            assert!(is_release_version(left), "{left}");
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
    fn numeric_identifiers_compare_by_value_at_any_size() {
        for (left, right) in [
            ("1.0.0-9", "1.0.0-10"),
            ("1.0.0-18446744073709551615", "1.0.0-18446744073709551616"),
            ("1.0.0-99999999999999999999", "1.0.0-100000000000000000000"),
            // A number past `u64` is still a number, so it sorts before a word.
            ("1.0.0-100000000000000000000", "1.0.0-0a"),
        ] {
            assert_eq!(
                compare_versions(left, right),
                Ordering::Less,
                "{left} {right}"
            );
        }
    }

    #[test]
    fn text_that_is_not_a_release_version_still_orders_without_panicking() {
        for (left, right) in [
            ("", ""),
            ("-", "1.0.0"),
            ("1.0.0-", "1.0.0--"),
            ("x.y.z", "0.0.0"),
            ("1.0.0-01", "1.0.0-1"),
            ("1.0.0-a..b", "1.0.0-a.b"),
        ] {
            assert_eq!(
                compare_versions(left, right),
                compare_versions(right, left).reverse(),
                "{left} {right}"
            );
        }
    }

    mod properties {
        use proptest::prelude::*;

        use super::*;

        /// Version-shaped text: canonical parts and identifiers mixed with
        /// the leading zeros, empty identifiers, and numbers past `u64` that
        /// a manifest might still carry. The values are few, so generated
        /// versions often tie or share a core.
        fn candidate() -> impl Strategy<Value = String> {
            let number =
                prop::sample::select(vec!["0", "1", "2", "10", "01", "18446744073709551616"]);
            let core = prop_oneof![
                Just(("1", "0", "0")),
                (number.clone(), number.clone(), number),
            ];
            let identifier = prop::sample::select(vec![
                "0",
                "1",
                "2",
                "10",
                "01",
                "00",
                "",
                "a",
                "b",
                "a-1",
                "-",
                "0a",
                "18446744073709551616",
            ]);
            let pre = prop::option::weighted(0.8, prop::collection::vec(identifier, 1..4));
            (core, pre).prop_map(|((major, minor, patch), pre)| match pre {
                None => format!("{major}.{minor}.{patch}"),
                Some(ids) => format!("{major}.{minor}.{patch}-{}", ids.join(".")),
            })
        }

        /// The candidates a native manifest may name.
        fn version() -> impl Strategy<Value = String> {
            candidate().prop_filter("a canonical release version", |value| {
                is_release_version(value)
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(2048))]

            #[test]
            fn a_version_with_a_leading_zero_or_an_empty_identifier_is_refused(
                value in candidate(),
            ) {
                let (core, pre) = value.split_once('-').map_or((value.as_str(), None), |(core, pre)| (core, Some(pre)));
                let zero_led = |part: &str| part.len() > 1 && part.starts_with('0') && part.bytes().all(|b| b.is_ascii_digit());
                let non_canonical = core.split('.').any(zero_led)
                    || pre.is_some_and(|pre| pre.split('.').any(|id| id.is_empty() || zero_led(id)));
                prop_assert_eq!(is_release_version(&value), !non_canonical, "{}", value);
            }

            #[test]
            fn release_versions_are_equal_only_when_they_are_the_same_text(
                left in version(),
                right in version(),
            ) {
                prop_assert_eq!(compare_versions(&left, &left), Ordering::Equal);
                prop_assert_eq!(
                    compare_versions(&left, &right),
                    compare_versions(&right, &left).reverse()
                );
                prop_assert_eq!(
                    compare_versions(&left, &right) == Ordering::Equal,
                    left == right,
                    "{} {}", left, right
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
                if compare_versions(low, middle) == Ordering::Less
                    || compare_versions(middle, high) == Ordering::Less
                {
                    prop_assert_eq!(compare_versions(low, high), Ordering::Less);
                }
            }

            #[test]
            fn any_two_candidates_order_antisymmetrically(
                left in candidate(),
                right in candidate(),
            ) {
                prop_assert_eq!(
                    compare_versions(&left, &right),
                    compare_versions(&right, &left).reverse()
                );
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
                left in any::<u128>(),
                right in any::<u128>(),
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
                left in (any::<u128>(), any::<u128>(), any::<u128>()),
                right in (any::<u128>(), any::<u128>(), any::<u128>()),
            ) {
                let text = |(major, minor, patch): (u128, u128, u128)| format!("{major}.{minor}.{patch}");
                prop_assert!(is_release_version(&text(left)));
                prop_assert_eq!(compare_versions(&text(left), &text(right)), left.cmp(&right));
            }
        }
    }
}
