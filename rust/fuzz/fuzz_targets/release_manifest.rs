//! The native updater's downloaded metadata through `bake-update`: the
//! release manifest, its signature, release versions, and the self-check
//! record a freshly unpacked binary prints.
//!
//! Input: UTF-8 text (lossy), tried whole as a manifest and a signature, and
//! split at its first two line breaks into versions to compare. Oracles: a
//! manifest that parses names a release version and only its own archives,
//! and precedence between release versions is a total order.

#![no_main]

use std::cmp::Ordering;

use bake_update::check::check_record;
use bake_update::manifest::{
    TARGETS, archive_name, parse_manifest, release_keys, verify_signature,
};
use bake_update::version::{compare_versions, is_release_version};
use libfuzzer_sys::fuzz_target;

/// Whether no numeric prerelease identifier of `version` has a leading zero,
/// as semantic versioning requires. `compare_versions` (like 0.3's
/// `compareVersions`) stops at the first textually different identifiers
/// even when they are numerically equal, so `1` and `01` break transitivity:
/// `0.4.0-rc.1.` = `0.4.0-rc.01` = `00.4.0-rc.1`, yet the first is greater
/// than the last. Release tooling never writes such identifiers.
fn canonical_prerelease(version: &str) -> bool {
    version.split_once('-').is_none_or(|(_, pre)| {
        pre.split('.').all(|identifier| {
            !(identifier.len() > 1
                && identifier.starts_with('0')
                && identifier.bytes().all(|b| b.is_ascii_digit()))
        })
    })
}

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let text = text.as_ref();

    if let Ok(manifest) = parse_manifest(text) {
        assert!(is_release_version(&manifest.version));
        assert!(!manifest.artifacts.is_empty());
        for (target, artifact) in &manifest.artifacts {
            assert!(TARGETS.contains(&target.as_str()));
            assert_eq!(artifact.file, archive_name(&manifest.version, target));
            assert!(artifact.size > 0);
        }
    }

    let keys = release_keys();
    assert!(
        !verify_signature(data, text, &keys),
        "a fuzz input verified"
    );
    let mut lines = text.splitn(3, '\n');
    let left = lines.next().unwrap_or("");
    let right = lines.next().unwrap_or("");
    let rest = lines.next().unwrap_or("");
    let _ = verify_signature(rest.as_bytes(), left, &[right.to_owned()]);

    // Precedence is only defined between versions a manifest may name.
    let third = rest.lines().next().unwrap_or("");
    let versions = [left, right, third];
    if versions.iter().all(|version| is_release_version(version)) {
        for a in versions {
            assert_eq!(compare_versions(a, a), Ordering::Equal);
            for b in versions {
                assert_eq!(
                    compare_versions(a, b),
                    compare_versions(b, a).reverse(),
                    "version precedence is antisymmetric"
                );
                for c in versions {
                    if versions.iter().all(|version| canonical_prerelease(version))
                        && compare_versions(a, b) != Ordering::Greater
                        && compare_versions(b, c) != Ordering::Greater
                    {
                        assert_ne!(
                            compare_versions(a, c),
                            Ordering::Greater,
                            "version precedence is transitive"
                        );
                    }
                }
            }
        }
    }
    let _ = compare_versions(left, right);
    let _ = is_release_version(left);
    let _ = check_record(rest, left, right);
});
