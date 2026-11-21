//! Callsigns a top-level member of a team takes: short names, picked at
//! random among the free ones, so sessions read apart at a glance.
//!
//! The pool is [`NAMES`], or the `callsigns` list in `settings.json` when it
//! names at least one usable callsign ([`pool`]). A member tries the pool in
//! an order drawn from its session id ([`order`]), then the pool's numbered
//! forms (`nova2` to `nova9`), and takes the first free one; a spawned
//! member still takes `<parent>-<n>`.
//!
//! Each callsign has its own hue ([`hue`]): a name's place in the pool turned
//! by the golden angle, so the names of one pool never share one, a
//! numbered form continues past the pool's end, and a spawned member's
//! turns its parent's by its number.

use super::wire;

/// The built-in pool: short, distinct, and easy to say.
pub const NAMES: [&str; 48] = [
    "nova", "rook", "ember", "flint", "onyx", "pike", "quill", "raven", "sable", "talon", "wren",
    "zephyr", "atlas", "blaze", "cinder", "drift", "fable", "glint", "haze", "iris", "jade",
    "lark", "moth", "nimbus", "orbit", "prism", "quartz", "rune", "sage", "thorn", "umbra", "vale",
    "aster", "briar", "comet", "dune", "fern", "gale", "hawk", "juno", "lumen", "maple", "nyx",
    "opal", "rift", "spark", "tide", "wisp",
];

/// The longest callsign a configured pool may hold, so the status line
/// and the team's board stay short.
pub const MAX_LEN: usize = 16;

/// The golden angle, in degrees: consecutive places it turns by never
/// come back to one hue.
const GOLDEN: f64 = 137.507_764_050_037_85;

/// The pool a member draws from: the configured names that are usable
/// callsigns of at most [`MAX_LEN`] characters, trimmed, lowercased, and
/// each once, in the order given; [`NAMES`] when none is.
pub fn pool(configured: &[String]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in configured {
        let name = name.trim().to_ascii_lowercase();
        if name.len() <= MAX_LEN && wire::is_callsign(&name) && !names.contains(&name) {
            names.push(name);
        }
    }
    if names.is_empty() {
        NAMES.iter().map(|name| (*name).to_owned()).collect()
    } else {
        names
    }
}

/// `pool` in the order a member with `seed`, its session id, tries it,
/// then the pool's numbered forms in that order: the same seed always
/// draws the same order, and different ones draw apart.
pub fn order(pool: &[String], seed: &str) -> Vec<String> {
    let mut shuffled: Vec<&String> = pool.iter().collect();
    let mut state = fnv(seed);
    // Fisher-Yates over splitmix64.
    for last in (1..shuffled.len()).rev() {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let pick = usize::try_from(mix(state) % (last as u64 + 1)).unwrap_or(0);
        shuffled.swap(last, pick);
    }
    let mut words: Vec<String> = shuffled.iter().map(|name| (*name).clone()).collect();
    for n in 2..=9 {
        words.extend(shuffled.iter().map(|name| format!("{name}{n}")));
    }
    words
}

/// The hue of `callsign` in degrees, 0 to 359: its place in `pool` turned
/// by the golden angle; a numbered form's place past the pool's end; a
/// spawned member's (`<parent>-<n>`) its parent's turned by its number;
/// and any other name's from its hash.
pub fn hue(callsign: &str, pool: &[String]) -> u16 {
    let turned = |place: usize| ((place as f64 * GOLDEN) % 360.0) as u16;
    if let Some(place) = pool.iter().position(|name| name == callsign) {
        return turned(place);
    }
    // `nova2`: the pool's names continue past its end.
    let digits = callsign.trim_end_matches(|c: char| c.is_ascii_digit());
    if let Ok(n) = callsign[digits.len()..].parse::<usize>()
        && let Some(place) = pool.iter().position(|name| name == digits)
        && (2..=9).contains(&n)
    {
        return turned(place + (n - 1) * pool.len());
    }
    // `nova-1`: the parent's hue, turned by the child's number.
    if let Some((parent, n)) = callsign.rsplit_once('-')
        && let Ok(n) = n.parse::<usize>()
        && !parent.is_empty()
    {
        return (hue(parent, pool) + turned(n)) % 360;
    }
    (fnv(callsign) % 360) as u16
}

/// FNV-1a over `text`.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// splitmix64's output step.
fn mix(state: u64) -> u64 {
    let mut z = state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built_in() -> Vec<String> {
        pool(&[])
    }

    #[test]
    fn the_built_in_names_are_short_usable_and_each_once() {
        let names = built_in();
        assert_eq!(names.len(), NAMES.len());
        for (index, name) in names.iter().enumerate() {
            assert!(wire::is_callsign(name), "{name}");
            assert!(name.len() <= 6, "{name}");
            assert!(!names[..index].contains(name), "{name} twice");
        }
    }

    #[test]
    fn a_configured_pool_replaces_the_built_in_one() {
        let configured = [
            "Nova",
            " rook ",
            "nova",
            "Bad Name",
            "",
            "x".repeat(17).as_str(),
            "a-b",
        ]
        .map(str::to_owned);
        assert_eq!(pool(&configured), ["nova", "rook", "a-b"]);
        // Nothing usable keeps the built-in pool.
        assert_eq!(pool(&["Bad Name".to_owned()]), built_in());
    }

    #[test]
    fn a_seed_draws_one_order_of_every_name_then_the_numbered_forms() {
        let names = built_in();
        let first = order(&names, "session-1");
        assert_eq!(first, order(&names, "session-1"));
        assert_eq!(first.len(), names.len() * 9);
        let mut drawn = first[..names.len()].to_vec();
        drawn.sort();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(drawn, sorted);
        assert_eq!(first[names.len()], format!("{}2", first[0]));
        // Sessions draw apart: across a handful of seeds, more than one
        // name comes first.
        let leads: std::collections::BTreeSet<String> = (0..16)
            .map(|n| order(&names, &format!("session-{n}"))[0].clone())
            .collect();
        assert!(leads.len() > 4, "{leads:?}");
        assert!(order(&[], "s").is_empty());
    }

    #[test]
    fn each_name_of_a_pool_has_its_own_hue() {
        let names = built_in();
        let mut hues: Vec<u16> = names.iter().map(|name| hue(name, &names)).collect();
        hues.extend(names.iter().map(|name| hue(&format!("{name}2"), &names)));
        let mut seen = hues.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), hues.len());
        assert!(hues.iter().all(|hue| *hue < 360));
        // A child turns its parent's hue; other names hash, within range.
        let child = hue("nova-1", &names);
        assert_ne!(child, hue("nova", &names));
        assert_eq!(child, hue("nova-1", &names));
        for odd in ["", "-", "-1", "x-", "zz9", "a-b-c-12"] {
            assert!(hue(odd, &names) < 360, "{odd}");
        }
    }
}
