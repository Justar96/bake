//! Tool discovery ranking, ported from Pi's
//! `packages/coding-agent/src/extensions/tool-search/tool.ts` (v1.3.0; MIT,
//! see `NOTICE`): an Okapi BM25 ranker over tool metadata, which the host
//! uses to answer a script's `searchTools()`. The `tool_search` tool itself is
//! not ported.

use serde_json::Value;

/// `searchTools()`'s default number of matches.
pub const DEFAULT_TOOL_SEARCH_LIMIT: usize = 8;

/// A tool as the ranker sees it: its name and the text built by
/// [`create_tool_search_document`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSearchDocument {
    /// The tool's name.
    pub name: String,
    /// The searchable text.
    pub text: String,
}

/// One ranked match.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSearchMatch {
    /// The matching document's name.
    pub name: String,
    /// Its BM25 score, above zero.
    pub score: f64,
}

/// A group of tools, such as an MCP server's: `describeNamespace()` reports it
/// and its text is part of each member's search document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolNamespace {
    /// The namespace's name, for example `mcp__docs`.
    pub name: String,
    /// What the namespace offers.
    pub description: Option<String>,
    /// How to use its tools, such as an MCP server's instructions.
    pub instructions: Option<String>,
}

const STOP_WORDS: [&str; 22] = [
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on",
    "or", "that", "the", "this", "to", "with", "",
];

/// Naive singular form, so `issues` matches `issue` and `searches` matches `search`.
fn stem(term: &str) -> String {
    let length = term.len();
    if length > 4 && term.ends_with("ies") {
        return format!("{}y", &term[..length - 3]);
    }
    if length > 4
        && ["ches", "shes", "sses", "xes", "zes"]
            .iter()
            .any(|end| term.ends_with(end))
    {
        return term[..length - 2].to_owned();
    }
    if length > 3 && term.ends_with('s') && !term.ends_with("ss") {
        return term[..length - 1].to_owned();
    }
    term.to_owned()
}

/// Lowercase terms, split at camelCase boundaries and non-alphanumerics,
/// without stop words.
pub fn tokenize(text: &str) -> Vec<String> {
    let lowered = split_acronyms(&split_camel_case(text)).to_lowercase();
    lowered
        .split(|character: char| !(character.is_ascii_lowercase() || character.is_ascii_digit()))
        .filter(|term| !STOP_WORDS.contains(term))
        .map(stem)
        .collect()
}

/// `text.replace(/([a-z0-9])([A-Z])/g, "$1 $2")`.
fn split_camel_case(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    let mut index = 0;
    while index < characters.len() {
        let current = characters[index];
        match characters.get(index + 1) {
            Some(&next)
                if (current.is_ascii_lowercase() || current.is_ascii_digit())
                    && next.is_ascii_uppercase() =>
            {
                out.push(current);
                out.push(' ');
                out.push(next);
                index += 2;
            }
            _ => {
                out.push(current);
                index += 1;
            }
        }
    }
    out
}

/// `text.replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")`: a run of two or more
/// capitals followed by a lowercase letter gets a space before its last
/// capital.
fn split_acronyms(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    let mut index = 0;
    while index < characters.len() {
        if !characters[index].is_ascii_uppercase() {
            out.push(characters[index]);
            index += 1;
            continue;
        }
        let start = index;
        let mut end = index;
        while end < characters.len() && characters[end].is_ascii_uppercase() {
            end += 1;
        }
        let followed = characters.get(end).is_some_and(char::is_ascii_lowercase);
        if followed && end - start >= 2 {
            out.extend(&characters[start..end - 1]);
            out.push(' ');
            out.push(characters[end - 1]);
            out.push(characters[end]);
            index = end + 1;
        } else {
            out.extend(&characters[start..end]);
            index = end;
        }
    }
    out
}

/// Schema descriptions and property names, recursively.
fn schema_text(schema: &Value, parts: &mut Vec<String>) {
    let Value::Object(schema) = schema else {
        return;
    };
    if let Some(Value::String(description)) = schema.get("description") {
        parts.push(description.clone());
    }
    if let Some(Value::Object(properties)) = schema.get("properties") {
        for (name, property) in properties {
            parts.push(name.clone());
            schema_text(property, parts);
        }
    }
    if let Some(items) = schema.get("items") {
        schema_text(items, parts);
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(variants)) = schema.get(key) {
            for variant in variants {
                schema_text(variant, parts);
            }
        }
    }
}

/// Search text of a tool: the name, the name with `_` as spaces, the
/// description, schema descriptions and property names, and the namespace
/// with its description and instructions.
pub fn create_tool_search_document(
    name: &str,
    description: &str,
    parameters: Option<&Value>,
    namespace: Option<&ToolNamespace>,
) -> ToolSearchDocument {
    let mut parts = vec![
        name.to_owned(),
        name.replace('_', " "),
        description.to_owned(),
    ];
    if let Some(parameters) = parameters {
        schema_text(parameters, &mut parts);
    }
    if let Some(namespace) = namespace {
        parts.push(namespace.name.clone());
        parts.push(namespace.description.clone().unwrap_or_default());
        parts.push(namespace.instructions.clone().unwrap_or_default());
    }
    parts.retain(|part| !part.trim().is_empty());
    ToolSearchDocument {
        name: name.to_owned(),
        text: parts.join(" "),
    }
}

/// Okapi BM25 with the usual parameters. Ties keep document order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bm25Ranker {
    k1: f64,
    b: f64,
}

impl Default for Bm25Ranker {
    fn default() -> Self {
        Self { k1: 1.2, b: 0.75 }
    }
}

impl Bm25Ranker {
    /// A ranker with other parameters than the defaults (`k1` 1.2, `b` 0.75).
    pub fn new(k1: f64, b: f64) -> Self {
        Self { k1, b }
    }

    /// The documents that match `query`, best first, at most `limit`.
    pub fn rank(
        &self,
        query: &str,
        documents: &[ToolSearchDocument],
        limit: usize,
    ) -> Vec<ToolSearchMatch> {
        let mut query_terms: Vec<String> = Vec::new();
        for term in tokenize(query) {
            if !query_terms.contains(&term) {
                query_terms.push(term);
            }
        }
        if query_terms.is_empty() || documents.is_empty() || limit == 0 {
            return Vec::new();
        }
        let term_counts: Vec<std::collections::HashMap<String, usize>> = documents
            .iter()
            .map(|document| {
                let mut counts = std::collections::HashMap::new();
                for term in tokenize(&document.text) {
                    *counts.entry(term).or_insert(0) += 1;
                }
                counts
            })
            .collect();
        let lengths: Vec<f64> = term_counts
            .iter()
            .map(|counts| counts.values().sum::<usize>() as f64)
            .collect();
        let mut average_length = lengths.iter().sum::<f64>() / documents.len() as f64;
        if average_length == 0.0 || average_length.is_nan() {
            average_length = 1.0;
        }
        let count = documents.len() as f64;
        let idf: Vec<f64> = query_terms
            .iter()
            .map(|term| {
                let frequency = term_counts
                    .iter()
                    .filter(|counts| counts.contains_key(term))
                    .count() as f64;
                (1.0 + (count - frequency + 0.5) / (frequency + 0.5)).ln()
            })
            .collect();
        let mut matches = Vec::new();
        for (index, document) in documents.iter().enumerate() {
            let mut score = 0.0;
            for (term, idf) in query_terms.iter().zip(&idf) {
                let Some(&occurrences) = term_counts[index].get(term) else {
                    continue;
                };
                let occurrences = occurrences as f64;
                let norm = self.k1 * (1.0 - self.b + (self.b * lengths[index]) / average_length);
                score += idf * ((occurrences * (self.k1 + 1.0)) / (occurrences + norm));
            }
            if score > 0.0 {
                matches.push(ToolSearchMatch {
                    name: document.name.clone(),
                    score,
                });
            }
        }
        matches.sort_by(|left, right| right.score.total_cmp(&left.score));
        matches.truncate(limit);
        matches
    }
}

/// Pi's `test/tool-search.test.ts` cases for `tokenize` and `Bm25Ranker`.
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn tokenize_splits_camel_case_and_snake_case_drops_stop_words_and_folds_plurals() {
        assert_eq!(
            tokenize("listIssues for the GitHub_repo"),
            ["list", "issue", "git", "hub", "repo"]
        );
        assert_eq!(
            tokenize("searches queries HTTPServer"),
            ["search", "query", "http", "server"]
        );
    }

    fn documents() -> Vec<ToolSearchDocument> {
        let tool = |name: &str, description: &str, properties: Value| {
            create_tool_search_document(
                name,
                description,
                Some(&json!({ "type": "object", "properties": properties })),
                None,
            )
        };
        vec![
            tool(
                "mcp__github__list_issues",
                "List issues in a repository.",
                json!({ "state": { "type": "string", "description": "open or closed" } }),
            ),
            tool(
                "mcp__github__create_pull_request",
                "Open a pull request.",
                json!({}),
            ),
            tool(
                "mcp__linear__search_issues",
                "Search Linear issues by text.",
                json!({}),
            ),
            tool("mcp__docs__search", "Search the documentation.", json!({})),
        ]
    }

    fn names(matches: &[ToolSearchMatch]) -> Vec<&str> {
        matches.iter().map(|found| found.name.as_str()).collect()
    }

    #[test]
    fn ranks_by_term_relevance_and_respects_the_limit() {
        let ranker = Bm25Ranker::default();
        let documents = documents();
        assert_eq!(
            names(&ranker.rank("issue", &documents, 8)),
            ["mcp__linear__search_issues", "mcp__github__list_issues"]
        );
        assert_eq!(
            names(&ranker.rank("pull requests", &documents, 8)).first(),
            Some(&"mcp__github__create_pull_request")
        );
        assert_eq!(ranker.rank("search", &documents, 1).len(), 1);
        // Property names and descriptions are searchable.
        assert_eq!(
            names(&ranker.rank("closed", &documents, 8)),
            ["mcp__github__list_issues"]
        );
    }

    #[test]
    fn returns_nothing_for_unknown_or_empty_queries() {
        let ranker = Bm25Ranker::default();
        let documents = documents();
        assert!(ranker.rank("kubernetes", &documents, 8).is_empty());
        assert!(ranker.rank("the", &documents, 8).is_empty());
        // Known v1 limit: no synonyms, so "tickets" does not find "issues".
        assert!(ranker.rank("tickets", &documents, 8).is_empty());
    }

    #[test]
    fn includes_the_namespace_in_the_search_text() {
        let namespace = ToolNamespace {
            name: "mcp__x".to_owned(),
            description: Some("Kubernetes cluster tools".to_owned()),
            instructions: None,
        };
        let document = create_tool_search_document(
            "mcp__x__run",
            "Run it.",
            Some(&json!({ "type": "object", "properties": {} })),
            Some(&namespace),
        );
        let matches = Bm25Ranker::default().rank("kubernetes", &[document], 8);
        assert_eq!(names(&matches), ["mcp__x__run"]);
        assert!(matches[0].score > 0.0);
    }
}
