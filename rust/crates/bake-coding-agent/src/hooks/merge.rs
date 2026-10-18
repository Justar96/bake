//! Folding every matched hook's outcome into the most restrictive one,
//! ported from Bake 0.3's `packages/hooks/hook-protocol/src/merge.ts`.
//!
//! Decisions rank `deny` over `ask` over `allow`, and only the winning
//! rank's reasons are kept, joined by a blank line. The first
//! `continue: false` stops, with its `stopReason`. Context and system
//! messages accumulate in hook order.

use super::codec::{HookDecision, HookOutput};

/// The decision one hook point resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MergedDecision {
    /// No hook expressed one.
    None,
    /// `allow` or `approve`.
    Allow,
    /// `ask`.
    Ask,
    /// `deny` or `block`.
    Deny,
}

impl MergedDecision {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }
}

/// The folded outcome of every hook that matched one point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedOutcome {
    /// The most restrictive decision.
    pub decision: MergedDecision,
    /// The winning rank's reasons, for `deny` and `ask` only.
    pub reason: Option<String>,
    /// Whether a hook returned `continue: false`.
    pub stop: bool,
    /// The first stopping hook's `stopReason`.
    pub stop_reason: Option<String>,
    /// Every non-empty `additionalContext`, in hook order.
    pub additional_context: Vec<String>,
    /// Every non-empty `systemMessage`, in hook order.
    pub system_messages: Vec<String>,
}

fn rank(decision: Option<HookDecision>) -> MergedDecision {
    match decision {
        Some(HookDecision::Deny | HookDecision::Block) => MergedDecision::Deny,
        Some(HookDecision::Ask) => MergedDecision::Ask,
        Some(HookDecision::Approve | HookDecision::Allow) => MergedDecision::Allow,
        None => MergedDecision::None,
    }
}

/// Folds `outputs`, in hook order; an empty list is neutral.
pub fn merge_hook_outputs<'a>(outputs: impl IntoIterator<Item = &'a HookOutput>) -> MergedOutcome {
    let mut decision = MergedDecision::None;
    let mut deny_reasons = Vec::new();
    let mut ask_reasons = Vec::new();
    let mut stop = false;
    let mut stop_reason = None;
    let mut additional_context = Vec::new();
    let mut system_messages = Vec::new();
    for output in outputs {
        let ranked = rank(output.decision);
        decision = decision.max(ranked);
        if let Some(reason) = output.reason.as_ref().filter(|reason| !reason.is_empty()) {
            match ranked {
                MergedDecision::Deny => deny_reasons.push(reason.clone()),
                MergedDecision::Ask => ask_reasons.push(reason.clone()),
                _ => {}
            }
        }
        if output.continue_ == Some(false) && !stop {
            stop = true;
            stop_reason.clone_from(&output.stop_reason);
        }
        if let Some(context) = output.additional_context.as_ref().filter(|c| !c.is_empty()) {
            additional_context.push(context.clone());
        }
        if let Some(message) = output.system_message.as_ref().filter(|m| !m.is_empty()) {
            system_messages.push(message.clone());
        }
    }
    let reasons = match decision {
        MergedDecision::Deny => deny_reasons,
        MergedDecision::Ask => ask_reasons,
        _ => Vec::new(),
    };
    MergedOutcome {
        decision,
        reason: (!reasons.is_empty()).then(|| reasons.join("\n\n")),
        stop,
        stop_reason,
        additional_context,
        system_messages,
    }
}

#[cfg(test)]
mod tests {
    //! Ported from 0.3's `hook-protocol/tests/merge.spec.ts`.
    use super::*;

    fn out(decision: Option<HookDecision>, reason: Option<&str>) -> HookOutput {
        HookOutput {
            exit_code: Some(0),
            decision,
            reason: reason.map(str::to_owned),
            ..HookOutput::default()
        }
    }

    fn decide(decisions: &[HookDecision]) -> MergedDecision {
        let outputs: Vec<HookOutput> = decisions.iter().map(|d| out(Some(*d), None)).collect();
        merge_hook_outputs(&outputs).decision
    }

    #[test]
    fn an_empty_list_is_neutral() {
        let merged = merge_hook_outputs(&[]);
        assert_eq!(merged.decision, MergedDecision::None);
        assert!(!merged.stop);
        assert!(merged.additional_context.is_empty() && merged.system_messages.is_empty());
    }

    #[test]
    fn deny_beats_ask_beats_allow_in_any_order() {
        use HookDecision::*;
        assert_eq!(decide(&[Allow]), MergedDecision::Allow);
        assert_eq!(decide(&[Approve]), MergedDecision::Allow);
        assert_eq!(decide(&[Allow, Ask]), MergedDecision::Ask);
        assert_eq!(decide(&[Ask, Deny]), MergedDecision::Deny);
        assert_eq!(decide(&[Deny, Allow]), MergedDecision::Deny);
        assert_eq!(decide(&[Allow, Block]), MergedDecision::Deny);
        let neutral = [HookOutput::default(), HookOutput::default()];
        assert_eq!(merge_hook_outputs(&neutral).decision, MergedDecision::None);
    }

    #[test]
    fn only_the_winning_ranks_reasons_are_joined() {
        let merged = merge_hook_outputs(&[
            out(Some(HookDecision::Deny), Some("first objection")),
            out(Some(HookDecision::Allow), Some("not collected")),
            out(Some(HookDecision::Block), Some("second objection")),
        ]);
        assert_eq!(
            merged.reason.as_deref(),
            Some("first objection\n\nsecond objection")
        );
        let merged = merge_hook_outputs(&[out(Some(HookDecision::Allow), Some("x"))]);
        assert_eq!(merged.reason, None);
        let merged = merge_hook_outputs(&[
            out(Some(HookDecision::Allow), Some("not surfaced")),
            out(Some(HookDecision::Ask), Some("needs approval")),
        ]);
        assert_eq!(
            (merged.decision, merged.reason.as_deref()),
            (MergedDecision::Ask, Some("needs approval"))
        );
        let merged = merge_hook_outputs(&[
            out(Some(HookDecision::Ask), Some("dropped once deny wins")),
            out(Some(HookDecision::Deny), Some("the real objection")),
        ]);
        assert_eq!(merged.reason.as_deref(), Some("the real objection"));
    }

    #[test]
    fn the_first_stop_is_sticky() {
        let stop = |reason: Option<&str>| HookOutput {
            continue_: Some(false),
            stop_reason: reason.map(str::to_owned),
            ..HookOutput::default()
        };
        let go = HookOutput {
            continue_: Some(true),
            ..HookOutput::default()
        };
        let merged =
            merge_hook_outputs(&[go.clone(), stop(Some("halt now")), stop(Some("ignored"))]);
        assert!(merged.stop);
        assert_eq!(merged.stop_reason.as_deref(), Some("halt now"));
        let merged = merge_hook_outputs(&[go, HookOutput::default()]);
        assert!(!merged.stop);
        let merged = merge_hook_outputs(&[stop(None)]);
        assert!(merged.stop);
        assert_eq!(merged.stop_reason, None);
    }

    #[test]
    fn context_and_system_messages_accumulate_in_order() {
        let with = |context: Option<&str>, message: Option<&str>| HookOutput {
            additional_context: context.map(str::to_owned),
            system_message: message.map(str::to_owned),
            ..HookOutput::default()
        };
        let merged = merge_hook_outputs(&[
            with(Some("ctx-A"), Some("warn-A")),
            with(Some(""), Some("")),
            with(Some("ctx-B"), None),
            with(None, Some("warn-B")),
        ]);
        assert_eq!(merged.additional_context, ["ctx-A", "ctx-B"]);
        assert_eq!(merged.system_messages, ["warn-A", "warn-B"]);
    }
}
