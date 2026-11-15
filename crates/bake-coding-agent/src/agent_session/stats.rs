//! Pi's session statistics, the footer's usage totals, and context usage.

use super::*;

/// Pi's `SessionStats`, without its context usage.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionStats {
    /// The session file, when the session persists.
    pub session_file: Option<PathBuf>,
    pub session_id: String,
    /// The display name.
    pub name: Option<String>,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_calls: usize,
    pub tool_results: usize,
    pub total_messages: usize,
    /// Uncached input tokens.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// The cost in dollars.
    pub cost: f64,
}

/// The usage Pi's footer reads: the session's token totals over every
/// entry, counted as [`SessionStats`] counts them, and the latest
/// response's prompt-cache share.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageTotals {
    /// Uncached input tokens.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// The cost in dollars.
    pub cost: f64,
    /// Pi's `latestCacheHitRate` as a share from 0 to 1: the latest
    /// assistant response's cache reads over its prompt tokens (input,
    /// cache reads, and cache writes). `None` before a response, or when
    /// the latest one reported no prompt tokens.
    pub latest_cache_hit: Option<f64>,
}

impl SessionStats {
    /// The four token counts' sum, Pi's `tokens.total`.
    pub fn total_tokens(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    /// Pi's `addUsageToTotals` over a stored usage object.
    fn add_usage(&mut self, usage: &serde_json::Value) {
        let count = |key: &str| {
            usage
                .get(key)
                .and_then(serde_json::Value::as_f64)
                .filter(|value| value.is_finite() && *value > 0.0)
                .map_or(0, |value| value as u64)
        };
        self.input = self.input.saturating_add(count("input"));
        self.output = self.output.saturating_add(count("output"));
        self.cache_read = self.cache_read.saturating_add(count("cacheRead"));
        self.cache_write = self.cache_write.saturating_add(count("cacheWrite"));
        if let Some(cost) = usage
            .get("cost")
            .and_then(|cost| cost.get("total"))
            .and_then(serde_json::Value::as_f64)
            .filter(|cost| cost.is_finite())
        {
            self.cost += cost;
        }
    }
}

/// How full the model's context window is, Pi's `ContextUsage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsage {
    /// The tokens the next request would carry, estimated; `None` after a
    /// compaction until a response after it records usage.
    pub tokens: Option<u64>,
    /// The model's context window.
    pub context_window: u64,
}

impl AgentSession {
    /// Pi's `getSessionStats`, read from the session's entries: message
    /// counts by role, tool calls, and the token and cost totals of every
    /// usage the log records. Reads only; a malformed member counts as
    /// absent.
    pub fn session_stats(&self) -> SessionStats {
        let session = lock(&self.session);
        let mut stats = SessionStats {
            session_file: session
                .is_persisted()
                .then(|| session.session_file().map(PathBuf::from))
                .flatten(),
            session_id: session.session_id().to_owned(),
            name: session.session_name(),
            ..SessionStats::default()
        };
        for entry in session.entries() {
            let kind = entry.entry_type();
            if matches!(kind, "usage" | "branch_summary" | "compaction")
                && let Some(usage) = entry.get("usage")
            {
                stats.add_usage(usage);
            }
            if kind != "message" {
                continue;
            }
            let Some(message) = entry.get("message").and_then(serde_json::Value::as_object) else {
                continue;
            };
            stats.total_messages += 1;
            match message.get("role").and_then(serde_json::Value::as_str) {
                Some("user") => stats.user_messages += 1,
                Some("toolResult") => {
                    stats.tool_results += 1;
                    if let Some(usage) = message.get("usage") {
                        stats.add_usage(usage);
                    }
                }
                Some("assistant") => {
                    stats.assistant_messages += 1;
                    stats.tool_calls += message
                        .get("content")
                        .and_then(serde_json::Value::as_array)
                        .map_or(0, |blocks| {
                            blocks
                                .iter()
                                .filter(|block| {
                                    block.get("type").and_then(serde_json::Value::as_str)
                                        == Some("toolCall")
                                })
                                .count()
                        });
                    if let Some(usage) = message.get("usage") {
                        stats.add_usage(usage);
                    }
                }
                _ => {}
            }
        }
        stats
    }

    /// Pi's footer `getSessionStats` reading: the token totals of every
    /// entry of the session, whatever the branch, and the latest assistant
    /// response's cache share ([`UsageTotals`]).
    pub fn usage_totals(&self) -> UsageTotals {
        let session = lock(&self.session);
        let mut stats = SessionStats::default();
        let mut latest_cache_hit = None;
        for entry in session.entries() {
            let kind = entry.entry_type();
            if matches!(kind, "usage" | "branch_summary" | "compaction") {
                if let Some(usage) = entry.get("usage") {
                    stats.add_usage(usage);
                }
                continue;
            }
            if kind != "message" {
                continue;
            }
            let Some(message) = entry.get("message") else {
                continue;
            };
            let role = message.get("role").and_then(serde_json::Value::as_str);
            let Some(usage) = message.get("usage") else {
                continue;
            };
            match role {
                Some("toolResult") => stats.add_usage(usage),
                Some("assistant") => {
                    stats.add_usage(usage);
                    let count = |key: &str| {
                        usage
                            .get(key)
                            .and_then(serde_json::Value::as_f64)
                            .filter(|count| count.is_finite() && *count > 0.0)
                            .unwrap_or(0.0)
                    };
                    let prompt = count("input") + count("cacheRead") + count("cacheWrite");
                    latest_cache_hit = (prompt > 0.0).then(|| count("cacheRead") / prompt);
                }
                _ => {}
            }
        }
        UsageTotals {
            input: stats.input,
            output: stats.output,
            cache_read: stats.cache_read,
            cache_write: stats.cache_write,
            cost: stats.cost,
            latest_cache_hit,
        }
    }

    /// Pi's `getContextUsage`: the current model's context window and the
    /// projected tokens of the session's branch
    /// ([`crate::compaction::projected_context_tokens`]). `None` without a
    /// model, or for a model with no window.
    pub fn context_usage(&self) -> Option<ContextUsage> {
        let model = self.model().filter(|model| model.context_window > 0)?;
        let session = lock(&self.session);
        let branch: Vec<_> = session.branch_entries(None).into_iter().cloned().collect();
        Some(ContextUsage {
            tokens: crate::compaction::projected_context_tokens(
                &session.build_session_projection(),
                &branch,
            ),
            context_window: model.context_window,
        })
    }
}
