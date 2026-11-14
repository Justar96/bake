//! Paired statistics: the change between two arms and how sure it is.
//!
//! A pair is one measure of the base and of the candidate on the same task,
//! model, and trial number. The change is the ratio of the sums,
//! `(Σcandidate − Σbase) / Σbase`, so a task that uses many tokens weighs as
//! it does in a real session. Its interval is a percentile bootstrap over
//! the pairs with a fixed seed, so a report is reproducible from its sample
//! records.
//!
//! A measure that is 0 in most pairs, such as failed tool calls, has no
//! stable ratio: a resample can draw a base sum of 0, and the ones that do
//! not are a biased remainder. Such a measure gets no interval.
//!
//! One task that uses twenty times the tokens of the others decides a ratio
//! of sums, and its own variance becomes the whole interval. The typical
//! change ([`typical_change`]) weighs every task the same: the geometric
//! mean over the tasks of each task's ratio of sums, resampled within each
//! task.

/// Bootstrap resamples.
pub const RESAMPLES: usize = 2000;

/// The interval's coverage.
pub const COVERAGE: f64 = 0.95;

const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// The paired change of one measure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Change {
    /// The base arm's sum.
    pub base: f64,
    /// The candidate arm's sum.
    pub candidate: f64,
    /// The relative change, `None` when the base sum is 0.
    pub change: Option<f64>,
    /// The bootstrap interval of the change; `None` with it, and when some
    /// resample had a base sum of 0, which marks a measure too sparse to
    /// have one.
    pub interval: Option<(f64, f64)>,
}

/// xorshift64*: enough for resampling, with no dependency and no state
/// outside the report.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        // The bias of a modulo is far below the resampling noise.
        (self.next() % bound as u64) as usize
    }
}

fn ratio(base: f64, candidate: f64) -> Option<f64> {
    (base > 0.0).then(|| (candidate - base) / base)
}

/// The change over `pairs` of `(base, candidate)` measures, which must be
/// finite and not negative.
pub fn paired_change(pairs: &[(f64, f64)]) -> Change {
    let base: f64 = pairs.iter().map(|pair| pair.0).sum();
    let candidate: f64 = pairs.iter().map(|pair| pair.1).sum();
    let change = ratio(base, candidate);
    let mut interval = None;
    if change.is_some() {
        let mut rng = Rng(SEED);
        let ratios: Option<Vec<f64>> = (0..RESAMPLES)
            .map(|_| {
                let (mut base, mut candidate) = (0.0, 0.0);
                for _ in 0..pairs.len() {
                    let pair = pairs[rng.below(pairs.len())];
                    base += pair.0;
                    candidate += pair.1;
                }
                ratio(base, candidate)
            })
            .collect();
        if let Some(mut ratios) = ratios {
            ratios.sort_by(f64::total_cmp);
            let last = ratios.len() - 1;
            let tail = (1.0 - COVERAGE) / 2.0;
            let low = (tail * last as f64).floor() as usize;
            let high = ((1.0 - tail) * last as f64).ceil() as usize;
            interval = Some((ratios[low], ratios[high.min(last)]));
        }
    }
    Change {
        base,
        candidate,
        change,
        interval,
    }
}

/// The change of one measure with every group weighing the same.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypicalChange {
    /// The geometric mean of the groups' ratios of sums, less one; `None`
    /// without groups or when a group's base or candidate sum is 0.
    pub change: Option<f64>,
    /// Its bootstrap interval, resampling the pairs inside each group;
    /// `None` with the change, and when a resample drew a sum of 0.
    pub interval: Option<(f64, f64)>,
}

fn geometric_change(sums: impl Iterator<Item = (f64, f64)>) -> Option<f64> {
    let mut logs = 0.0;
    let mut groups = 0usize;
    for (base, candidate) in sums {
        if base <= 0.0 || candidate <= 0.0 {
            return None;
        }
        logs += (candidate / base).ln();
        groups += 1;
    }
    (groups > 0).then(|| (logs / groups as f64).exp() - 1.0)
}

/// The typical change over `groups`, each the `(base, candidate)` pairs of
/// one task.
pub fn typical_change(groups: &[Vec<(f64, f64)>]) -> TypicalChange {
    let sums = |pairs: &Vec<(f64, f64)>| {
        pairs
            .iter()
            .fold((0.0, 0.0), |sum, pair| (sum.0 + pair.0, sum.1 + pair.1))
    };
    let change = geometric_change(groups.iter().map(sums));
    let mut interval = None;
    if change.is_some() {
        let mut rng = Rng(SEED);
        let changes: Option<Vec<f64>> = (0..RESAMPLES)
            .map(|_| {
                geometric_change(groups.iter().map(|pairs| {
                    let (mut base, mut candidate) = (0.0, 0.0);
                    for _ in 0..pairs.len() {
                        let pair = pairs[rng.below(pairs.len())];
                        base += pair.0;
                        candidate += pair.1;
                    }
                    (base, candidate)
                }))
            })
            .collect();
        if let Some(mut changes) = changes {
            changes.sort_by(f64::total_cmp);
            let last = changes.len() - 1;
            let tail = (1.0 - COVERAGE) / 2.0;
            let low = (tail * last as f64).floor() as usize;
            let high = ((1.0 - tail) * last as f64).ceil() as usize;
            interval = Some((changes[low], changes[high.min(last)]));
        }
    }
    TypicalChange { change, interval }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_known_change() {
        let change = paired_change(&[(100.0, 80.0), (300.0, 240.0)]);
        assert_eq!((change.base, change.candidate), (400.0, 320.0));
        let value = change.change.expect("change");
        assert!((value + 0.2).abs() < 1e-12, "{value}");
        let (low, high) = change.interval.expect("interval");
        assert!((low + 0.2).abs() < 1e-12 && (high + 0.2).abs() < 1e-12);
    }

    #[test]
    fn nothing_to_compare() {
        assert_eq!(
            paired_change(&[]),
            Change {
                base: 0.0,
                candidate: 0.0,
                change: None,
                interval: None
            }
        );
        let zero = paired_change(&[(0.0, 5.0)]);
        assert_eq!((zero.change, zero.interval), (None, None));
    }

    #[test]
    fn a_noisy_pair_set_has_a_wide_interval() {
        let pairs = [(100.0, 150.0), (100.0, 60.0), (100.0, 130.0), (100.0, 70.0)];
        let change = paired_change(&pairs);
        let (low, high) = change.interval.expect("interval");
        assert!(low < 0.0 && high > 0.0, "{low} {high}");
    }

    #[test]
    fn a_sparse_measure_has_a_change_and_no_interval() {
        // One pair in thirty has a value: most resamples of a few draws miss
        // it, and one that never does would still say nothing.
        let mut pairs = vec![(0.0, 0.0); 29];
        pairs.push((10.0, 0.0));
        let change = paired_change(&pairs);
        assert_eq!(change.change, Some(-1.0));
        assert_eq!(change.interval, None);
    }

    #[test]
    fn a_typical_change_weighs_every_task_the_same() {
        // A small task that halves and a large one that does not change:
        // the ratio of sums barely moves, the typical change is the
        // geometric mean of 0.5 and 1.
        let small = vec![(10.0, 5.0), (10.0, 5.0)];
        let large = vec![(1000.0, 1000.0), (1000.0, 1000.0)];
        let typical = typical_change(&[small.clone(), large.clone()]);
        let expected = 0.5f64.sqrt() - 1.0;
        let value = typical.change.expect("change");
        assert!((value - expected).abs() < 1e-12, "{value}");
        let (low, high) = typical.interval.expect("interval");
        assert!((low - expected).abs() < 1e-12 && (high - expected).abs() < 1e-12);
        let all: Vec<(f64, f64)> = small.into_iter().chain(large).collect();
        let summed = paired_change(&all).change.expect("change");
        assert!(summed > -0.01, "{summed}");
    }

    #[test]
    fn a_typical_change_needs_a_value_in_every_task() {
        let none = TypicalChange {
            change: None,
            interval: None,
        };
        assert_eq!(typical_change(&[]), none);
        assert_eq!(typical_change(&[vec![(1.0, 2.0)], vec![(0.0, 0.0)]]), none);
        assert_eq!(typical_change(&[vec![(1.0, 0.0)]]), none);
        // Every pair has a value only in the sum: a resample can miss it.
        let sparse = typical_change(&[vec![(0.0, 0.0), (0.0, 0.0), (0.0, 0.0), (4.0, 2.0)]]);
        assert_eq!(sparse.change, Some(-0.5));
        assert_eq!(sparse.interval, None);
    }

    fn pairs() -> impl Strategy<Value = Vec<(f64, f64)>> {
        proptest::collection::vec((1u32..1_000_000, 0u32..1_000_000), 1..40).prop_map(|pairs| {
            pairs
                .into_iter()
                .map(|(base, candidate)| (f64::from(base), f64::from(candidate)))
                .collect()
        })
    }

    proptest! {
        #[test]
        fn intervals_are_ordered_bounded_and_repeatable(pairs in pairs()) {
            let change = paired_change(&pairs);
            let (low, high) = change.interval.expect("interval");
            prop_assert!(low <= high);
            // No resample can change more than the most extreme pair.
            let extremes: Vec<f64> = pairs.iter().map(|(base, candidate)| (candidate - base) / base).collect();
            let least = extremes.iter().copied().fold(f64::INFINITY, f64::min);
            let most = extremes.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            prop_assert!(low >= least - 1e-9 && high <= most + 1e-9);
            let value = change.change.expect("change");
            prop_assert!(value >= least - 1e-9 && value <= most + 1e-9);
            prop_assert_eq!(paired_change(&pairs), change);
        }

        #[test]
        fn one_task_s_typical_change_is_its_paired_change(pairs in pairs()) {
            prop_assume!(pairs.iter().all(|pair| pair.1 > 0.0));
            let typical = typical_change(std::slice::from_ref(&pairs));
            let paired = paired_change(&pairs);
            let (typical_value, paired_value) = (typical.change.expect("typical"), paired.change.expect("paired"));
            prop_assert!((typical_value - paired_value).abs() < 1e-9);
            let (low, high) = typical.interval.expect("interval");
            let (paired_low, paired_high) = paired.interval.expect("interval");
            prop_assert!((low - paired_low).abs() < 1e-9 && (high - paired_high).abs() < 1e-9);
        }

        #[test]
        fn a_scaled_arm_s_typical_change_is_its_scale(
            groups in proptest::collection::vec(pairs(), 1..6),
            percent in 1u32..400,
        ) {
            let scale = f64::from(percent) / 100.0;
            let scaled: Vec<Vec<(f64, f64)>> = groups
                .iter()
                .map(|pairs| pairs.iter().map(|pair| (pair.0, pair.0 * scale)).collect())
                .collect();
            let typical = typical_change(&scaled);
            let value = typical.change.expect("change");
            prop_assert!((value - (scale - 1.0)).abs() < 1e-9);
            let (low, high) = typical.interval.expect("interval");
            prop_assert!(low <= high);
            prop_assert!((low - (scale - 1.0)).abs() < 1e-9 && (high - (scale - 1.0)).abs() < 1e-9);
        }

        #[test]
        fn equal_arms_do_not_change(pairs in pairs()) {
            let same: Vec<(f64, f64)> = pairs.iter().map(|pair| (pair.0, pair.0)).collect();
            let change = paired_change(&same);
            prop_assert_eq!(change.change, Some(0.0));
            prop_assert_eq!(change.interval, Some((0.0, 0.0)));
        }

        #[test]
        fn a_scaled_arm_changes_by_its_scale(pairs in pairs(), percent in 0u32..400) {
            let scale = f64::from(percent) / 100.0;
            let scaled: Vec<(f64, f64)> = pairs.iter().map(|pair| (pair.0, pair.0 * scale)).collect();
            let change = paired_change(&scaled);
            let value = change.change.expect("change");
            prop_assert!((value - (scale - 1.0)).abs() < 1e-9);
            let (low, high) = change.interval.expect("interval");
            prop_assert!((low - (scale - 1.0)).abs() < 1e-9 && (high - (scale - 1.0)).abs() < 1e-9);
        }
    }
}
