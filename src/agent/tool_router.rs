//! Per-task tool routing (Tool RAG).
//!
//! Research (RAG-MCP, "Over-Tooled Agent Problem", 2025-2026) shows local
//! model tool-selection accuracy collapses as the tool set grows — ~13% with a
//! large registry — and that retrieving only the query-relevant tools both
//! restores accuracy (~3x) and cuts prompt size 50%+.
//!
//! This module scores the registry against the user's message with a
//! dependency-free lexical ranker, always keeps a small essential set, and
//! returns the tools to EXCLUDE for the turn (the complement of the keep set),
//! which plugs directly into the existing `excluded_tools` path. Cheap enough
//! to run every turn with no embedding round-trip.

use std::collections::HashSet;

/// Tools that are always available regardless of the query — core file/shell
/// work, planning, memory, and RAG the agent needs on almost any task.
const ESSENTIAL_TOOLS: &[&str] = &[
    "task_plan",
    "file_read",
    "file_write",
    "file_edit",
    "shell",
    "glob_search",
    "content_search",
    "workspace_rag",
    "memory_recall",
    "memory_store",
    "code_run",
];

/// Lowercase alphanumeric tokens of length >= 2.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_lowercase())
        .collect()
}

/// Score one tool (name + description) against the query token set.
/// Overlap on the tool NAME is weighted higher than on the description.
fn score_tool(name: &str, description: &str, query_tokens: &HashSet<String>) -> f32 {
    let name_tokens: HashSet<String> = tokenize(name).into_iter().collect();
    let desc_tokens: HashSet<String> = tokenize(description).into_iter().collect();
    let name_hits = name_tokens.intersection(query_tokens).count() as f32;
    let desc_hits = desc_tokens.intersection(query_tokens).count() as f32;
    name_hits * 3.0 + desc_hits
}

/// Decide which tools to expose for this turn.
///
/// Returns the set of tool names to EXCLUDE. Given `(name, description)` pairs
/// for the full registry, keep the essential set plus the top-`top_k` tools by
/// lexical relevance to `query`; exclude the rest. If `top_k == 0` or the
/// registry is at or below the keep floor, nothing is excluded.
pub fn tools_to_exclude(
    tools: &[(String, String)],
    query: &str,
    top_k: usize,
) -> Vec<String> {
    if top_k == 0 || tools.len() <= ESSENTIAL_TOOLS.len() {
        return Vec::new();
    }
    let query_tokens: HashSet<String> = tokenize(query).into_iter().collect();

    // Always-keep essentials that actually exist in this registry.
    let mut keep: HashSet<String> = tools
        .iter()
        .filter(|(name, _)| ESSENTIAL_TOOLS.contains(&name.as_str()))
        .map(|(name, _)| name.clone())
        .collect();

    // Rank the non-essential tools by relevance and keep the top-k scoring > 0.
    let mut ranked: Vec<(&String, f32)> = tools
        .iter()
        .filter(|(name, _)| !keep.contains(name))
        .map(|(name, desc)| (name, score_tool(name, desc, &query_tokens)))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    for (name, score) in ranked.into_iter().take(top_k) {
        if score > 0.0 {
            keep.insert(name.clone());
        }
    }

    // Exclude everything not kept.
    tools
        .iter()
        .map(|(name, _)| name.clone())
        .filter(|name| !keep.contains(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Vec<(String, String)> {
        [
            ("task_plan", "break work into steps"),
            ("file_read", "read a file"),
            ("shell", "run a shell command"),
            ("docker", "manage docker containers and images"),
            ("packet_capture", "capture network packets with tshark"),
            ("git_operations", "clone pull push commit git repositories"),
            ("db_query", "query a sql or mongodb database"),
            ("web_search_tool", "search the web"),
            ("pushover", "send a pushover notification"),
            ("arxiv_search", "search arxiv papers"),
            ("service_control", "systemctl start stop services"),
            ("workspace_rag", "search the document inbox"),
        ]
        .iter()
        .map(|(n, d)| (n.to_string(), d.to_string()))
        .collect()
    }

    #[test]
    fn keeps_relevant_and_essential_excludes_rest() {
        let excluded = tools_to_exclude(&registry(), "clone the git repository and push", 3);
        // git_operations is relevant → kept (not excluded).
        assert!(!excluded.contains(&"git_operations".to_string()));
        // essentials always kept.
        assert!(!excluded.contains(&"shell".to_string()));
        assert!(!excluded.contains(&"file_read".to_string()));
        // irrelevant tools excluded.
        assert!(excluded.contains(&"pushover".to_string()));
        assert!(excluded.contains(&"arxiv_search".to_string()));
    }

    #[test]
    fn database_query_routes_to_db_tool() {
        let excluded = tools_to_exclude(&registry(), "query the mongodb database for users", 3);
        assert!(!excluded.contains(&"db_query".to_string()));
        assert!(excluded.contains(&"docker".to_string()));
    }

    #[test]
    fn top_k_zero_disables_routing() {
        assert!(tools_to_exclude(&registry(), "anything", 0).is_empty());
    }

    #[test]
    fn small_registry_is_untouched() {
        let small: Vec<(String, String)> =
            vec![("shell".into(), "run".into()), ("file_read".into(), "read".into())];
        assert!(tools_to_exclude(&small, "x", 5).is_empty());
    }
}
