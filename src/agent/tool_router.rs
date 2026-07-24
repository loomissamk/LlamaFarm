//! Per-turn tool routing for large local registries.
//!
//! Routing is an attention and prompt-size optimization, not an authorization
//! boundary. The ranker is deliberately local and deterministic: it combines
//! inverse-document-frequency weighted matches across tool names,
//! descriptions, and parameter schemas with a small set of domain aliases.
//! When the query carries no useful signal, it fails open instead of hiding a
//! tool the model may need.

use std::collections::{HashMap, HashSet};

use crate::tools::ToolSpec;

/// Core tools that remain available when relevance routing succeeds. These
/// cover planning, workspace inspection/mutation, local execution, and memory.
const ESSENTIAL_TOOLS: &[&str] = &[
    "task_plan",
    "file_read",
    "file_write",
    "file_edit",
    "apply_patch",
    "shell",
    "glob_search",
    "content_search",
    "workspace_rag",
    "memory_recall",
    "memory_store",
    "code_run",
    // Long-running services must not be forced through the synchronous shell
    // tool merely because their prompt has no lexical overlap with "process".
    "process",
    // Fans out a subtask to a specialized agent.planner/coder/verifier/
    // operator persona (see config.toml [agents.*]). Lexical routing rarely
    // surfaces "delegate" on its own merits — a request like "build a
    // trading platform with an ML model and a dashboard" has no token
    // overlap with the word "delegate" — so without pinning it, large
    // multi-part tasks silently never get the option to split off a
    // tightly-scoped coder/verifier pass and just get done ad hoc in the
    // main loop instead, one shell call at a time with no separate
    // verification step.
    "delegate",
];

/// Tools whose successful use normally requires a follow-up tool. Dependency
/// closure prevents routing from selecting the first half of a workflow while
/// making the required continuation unavailable.
const TOOL_DEPENDENCIES: &[(&str, &[&str])] = &[
    ("web_search_tool", &["web_fetch"]),
    ("db_schema", &["db_query"]),
    ("subagent_spawn", &["subagent_list", "subagent_manage"]),
    ("schedule", &["cron_add"]),
    (
        "sop_execute",
        &["sop_list", "sop_status", "sop_advance", "sop_approve"],
    ),
    ("sop_status", &["sop_advance", "sop_approve"]),
    ("sop_approve", &["sop_status", "sop_advance"]),
    ("sop_advance", &["sop_status", "sop_approve"]),
];

/// Small, explicit synonym families for common operator intent. This is not
/// presented as semantic/vector retrieval; it closes high-value lexical gaps
/// without another model request on the latency-critical path.
const ALIAS_GROUPS: &[&[&str]] = &[
    &["repo", "repository", "git", "github", "commit", "branch"],
    &[
        "database", "db", "sql", "sqlite", "postgres", "postgresql", "mongo", "mongodb",
        "redis",
    ],
    &["container", "docker", "podman", "compose"],
    &["internet", "web", "online", "news", "search", "browse"],
    &["paper", "papers", "publication", "publications", "arxiv", "research"],
    &["notify", "notification", "alert", "pushover", "message"],
    &[
        "schedule",
        "scheduled",
        "timer",
        "reminder",
        "cron",
        "recurring",
        "job",
        "jobs",
    ],
    &["packet", "packets", "pcap", "traffic", "network", "tshark", "tcpdump"],
    &["service", "daemon", "systemd", "systemctl", "process"],
    &["photo", "picture", "image", "screenshot", "visual"],
    &["hardware", "board", "gpio", "arduino", "nucleo", "register"],
    &["model", "models", "ollama", "inference", "llm"],
    &["delegate", "delegation", "subagent", "worker", "parallel"],
    &["sop", "procedure", "workflow", "playbook"],
    &["email", "mail", "gmail", "inbox", "composio"],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRouteStrategy {
    Disabled,
    WithinBudget,
    Lexical,
    FailOpenNoQuery,
    FailOpenNoMatch,
    FailOpenAmbiguous,
    FailOpenUndeclaredDependencies,
    DirectIntent,
}

impl ToolRouteStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::WithinBudget => "within_budget",
            Self::Lexical => "lexical_idf",
            Self::FailOpenNoQuery => "fail_open_no_query",
            Self::FailOpenNoMatch => "fail_open_no_match",
            Self::FailOpenAmbiguous => "fail_open_ambiguous",
            Self::FailOpenUndeclaredDependencies => "fail_open_undeclared_dependencies",
            Self::DirectIntent => "direct_intent",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolRouteScore {
    pub name: String,
    pub score: f32,
    pub matched_terms: Vec<String>,
}

/// One immutable selection used for prompt descriptions, XML/native schemas,
/// compatibility fallback, and execution filtering for a turn.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolRoute {
    pub selected: Vec<String>,
    pub excluded: Vec<String>,
    pub ranked: Vec<ToolRouteScore>,
    pub strategy: ToolRouteStrategy,
    pub reason: String,
}

impl ToolRoute {
    pub fn selected_specs(&self, tools: &[ToolSpec]) -> Vec<ToolSpec> {
        let selected: HashSet<&str> = self.selected.iter().map(String::as_str).collect();
        unique_specs(tools)
            .into_iter()
            .filter(|spec| selected.contains(spec.name.as_str()))
            .cloned()
            .collect()
    }
}

/// Return an all-tools selection with an explicit reason. Used for disabled
/// routing and conservative fallbacks.
pub fn full_selection(
    tools: &[ToolSpec],
    strategy: ToolRouteStrategy,
    reason: impl Into<String>,
) -> ToolRoute {
    ToolRoute {
        selected: unique_specs(tools)
            .into_iter()
            .map(|spec| spec.name.clone())
            .collect(),
        excluded: Vec::new(),
        ranked: Vec::new(),
        strategy,
        reason: reason.into(),
    }
}

/// Build a direct-intent selection. Only registered names are selected; every
/// other registered tool is excluded.
pub fn direct_selection(
    tools: &[ToolSpec],
    allowed: &[&str],
    reason: impl Into<String>,
) -> ToolRoute {
    let allowed: HashSet<&str> = allowed.iter().copied().collect();
    let specs = unique_specs(tools);
    let selected = specs
        .iter()
        .filter(|spec| allowed.contains(spec.name.as_str()))
        .map(|spec| spec.name.clone())
        .collect();
    let excluded = specs
        .iter()
        .filter(|spec| !allowed.contains(spec.name.as_str()))
        .map(|spec| spec.name.clone())
        .collect();
    ToolRoute {
        selected,
        excluded,
        ranked: Vec::new(),
        strategy: ToolRouteStrategy::DirectIntent,
        reason: reason.into(),
    }
}

/// Select query-relevant tools while preserving essentials and workflow
/// dependencies. `top_k` counts relevant non-essential roots; dependency tools
/// are added outside that budget. `top_k == 0` disables routing.
pub fn route_tools(tools: &[ToolSpec], query: &str, top_k: usize) -> ToolRoute {
    if top_k == 0 {
        return full_selection(
            tools,
            ToolRouteStrategy::Disabled,
            "tool_routing_top_k is 0",
        );
    }

    let specs = unique_specs(tools);
    let registered: HashSet<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
    let essential: HashSet<&str> = ESSENTIAL_TOOLS
        .iter()
        .copied()
        .filter(|name| registered.contains(name))
        .collect();
    let nonessential_count = specs
        .iter()
        .filter(|spec| !essential.contains(spec.name.as_str()))
        .count();
    if nonessential_count <= top_k {
        return full_selection(
            tools,
            ToolRouteStrategy::WithinBudget,
            format!("{nonessential_count} non-essential tools fit within top_k={top_k}"),
        );
    }

    let query_tokens = expanded_query_tokens(query);
    if query_tokens.is_empty() {
        return full_selection(
            tools,
            ToolRouteStrategy::FailOpenNoQuery,
            "query contained no discriminating routing terms",
        );
    }

    let documents: Vec<ToolDocument> = specs.iter().map(|spec| ToolDocument::new(spec)).collect();
    let document_frequency = document_frequency(&documents);
    let document_count = documents.len() as f32;
    let mut ranked: Vec<ToolRouteScore> = documents
        .iter()
        .filter(|doc| !essential.contains(doc.name.as_str()))
        .filter(|doc| candidate_matches_intent(doc.name.as_str(), &query_tokens))
        .filter_map(|doc| score_document(doc, &query_tokens, &document_frequency, document_count))
        .collect();
    ranked.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.name.cmp(&right.name))
    });

    if ranked.is_empty() {
        return full_selection(
            tools,
            ToolRouteStrategy::FailOpenNoMatch,
            "no tool had a name match or multi-term description match; preserving the full registry",
        );
    }

    if ranked.len() > top_k {
        let cutoff = ranked[top_k - 1].score;
        let next = ranked[top_k].score;
        let minimum_margin = cutoff.abs().max(1.0) * 0.05;
        if cutoff - next <= minimum_margin {
            return full_selection(
                tools,
                ToolRouteStrategy::FailOpenAmbiguous,
                format!(
                    "ranking was ambiguous at the top_k={top_k} cutoff; preserving the full registry"
                ),
            );
        }
    }

    let mut selected: HashSet<&str> = essential;
    for score in ranked.iter().take(top_k) {
        selected.insert(score.name.as_str());
    }
    close_dependencies(&mut selected, &registered);

    let selected_names = specs
        .iter()
        .filter(|spec| selected.contains(spec.name.as_str()))
        .map(|spec| spec.name.clone())
        .collect::<Vec<_>>();
    let excluded = specs
        .iter()
        .filter(|spec| !selected.contains(spec.name.as_str()))
        .map(|spec| spec.name.clone())
        .collect::<Vec<_>>();
    ranked.truncate(top_k);

    ToolRoute {
        selected: selected_names,
        excluded,
        ranked,
        strategy: ToolRouteStrategy::Lexical,
        reason: format!(
            "selected up to {top_k} relevant non-essential tools plus essentials and dependencies"
        ),
    }
}

fn candidate_matches_intent(name: &str, query_tokens: &HashSet<String>) -> bool {
    if name == "schedule" || name.starts_with("cron_") {
        return [
            "cron",
            "delay",
            "job",
            "recurring",
            "reminder",
            "schedule",
            "schedul",
            "timer",
        ]
        .iter()
        .any(|token| query_tokens.contains(*token));
    }
    true
}

fn unique_specs(tools: &[ToolSpec]) -> Vec<&ToolSpec> {
    let mut seen = HashSet::new();
    tools
        .iter()
        .filter(|spec| seen.insert(spec.name.as_str()))
        .collect()
}

#[derive(Debug)]
struct ToolDocument {
    name: String,
    name_tokens: HashSet<String>,
    description_tokens: HashSet<String>,
    schema_tokens: HashSet<String>,
    all_tokens: HashSet<String>,
}

impl ToolDocument {
    fn new(spec: &ToolSpec) -> Self {
        let name_tokens = tokenize(&spec.name);
        let description_tokens = tokenize(&spec.description);
        let schema_tokens = tokenize(&spec.parameters.to_string());
        let all_tokens = name_tokens
            .iter()
            .chain(description_tokens.iter())
            .chain(schema_tokens.iter())
            .cloned()
            .collect();
        Self {
            name: spec.name.clone(),
            name_tokens,
            description_tokens,
            schema_tokens,
            all_tokens,
        }
    }
}

fn document_frequency(documents: &[ToolDocument]) -> HashMap<String, usize> {
    let mut frequencies = HashMap::new();
    for document in documents {
        for token in &document.all_tokens {
            *frequencies.entry(token.clone()).or_insert(0) += 1;
        }
    }
    frequencies
}

fn score_document(
    document: &ToolDocument,
    query_tokens: &HashSet<String>,
    frequencies: &HashMap<String, usize>,
    document_count: f32,
) -> Option<ToolRouteScore> {
    let mut score = 0.0_f32;
    let mut name_match_count = 0usize;
    let mut description_match_count = 0usize;
    let mut matched_terms = Vec::new();
    let mut ordered_tokens = query_tokens.iter().collect::<Vec<_>>();
    ordered_tokens.sort();
    for token in ordered_tokens {
        let df = *frequencies.get(token).unwrap_or(&0) as f32;
        let idf = ((document_count + 1.0) / (df + 1.0)).ln() + 1.0;
        let mut matched = false;
        if document.name_tokens.contains(token) {
            score += 5.0 * idf;
            matched = true;
            name_match_count += 1;
        }
        if document.description_tokens.contains(token) {
            score += 2.0 * idf;
            matched = true;
            description_match_count += 1;
        }
        if document.schema_tokens.contains(token) {
            score += 0.35 * idf;
            matched = true;
        }
        if matched {
            matched_terms.push(token.clone());
        }
    }

    if document.name_tokens.len() > 1
        && document
            .name_tokens
            .iter()
            .all(|token| query_tokens.contains(token))
    {
        score += 4.0;
    }

    let has_confident_match = name_match_count > 0 || description_match_count >= 2;
    if score <= 0.0 || !has_confident_match {
        None
    } else {
        matched_terms.sort();
        Some(ToolRouteScore {
            name: document.name.clone(),
            score,
            matched_terms,
        })
    }
}

fn close_dependencies<'a>(selected: &mut HashSet<&'a str>, registered: &HashSet<&'a str>) {
    loop {
        let mut changed = false;
        for (tool, dependencies) in TOOL_DEPENDENCIES {
            if !selected.contains(tool) {
                continue;
            }
            for dependency in *dependencies {
                if let Some(registered_name) = registered.get(dependency) {
                    changed |= selected.insert(*registered_name);
                }
            }
        }
        if !changed {
            break;
        }
    }
}

fn expanded_query_tokens(query: &str) -> HashSet<String> {
    let mut tokens = tokenize(query);
    let original = tokens.clone();
    for group in ALIAS_GROUPS {
        if group
            .iter()
            .map(|alias| normalize_token(alias))
            .any(|alias| original.contains(&alias))
        {
            tokens.extend(group.iter().map(|alias| normalize_token(alias)));
        }
    }
    tokens
}

fn tokenize(text: &str) -> HashSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .map(normalize_token)
        .filter(|token| token.chars().count() >= 2 && !is_stopword(token))
        .collect()
}

fn normalize_token(token: &str) -> String {
    let mut normalized = token.to_lowercase();
    if normalized.len() > 5 && normalized.ends_with("ies") {
        normalized.truncate(normalized.len() - 3);
        normalized.push('y');
    } else if normalized.len() > 5 && normalized.ends_with("ing") {
        normalized.truncate(normalized.len() - 3);
    } else if normalized.len() > 4 && normalized.ends_with("ed") {
        normalized.truncate(normalized.len() - 2);
    } else if normalized.len() > 4
        && normalized.ends_with('s')
        && !normalized.ends_with("ss")
        && !normalized.ends_with("us")
        && !normalized.ends_with("is")
    {
        normalized.truncate(normalized.len() - 1);
    }
    normalized
}

fn is_stopword(token: &str) -> bool {
    matches!(
        token,
        "about"
            | "action"
            | "after"
            | "again"
            | "all"
            | "also"
            | "and"
            | "any"
            | "are"
            | "as"
            | "at"
            | "be"
            | "by"
            | "can"
            | "could"
            | "data"
            | "do"
            | "for"
            | "from"
            | "have"
            | "in"
            | "into"
            | "id"
            | "is"
            | "it"
            | "its"
            | "latest"
            | "like"
            | "need"
            | "object"
            | "of"
            | "on"
            | "or"
            | "please"
            | "query"
            | "request"
            | "response"
            | "result"
            | "status"
            | "string"
            | "that"
            | "the"
            | "then"
            | "this"
            | "too"
            | "to"
            | "tool"
            | "type"
            | "use"
            | "value"
            | "values"
            | "want"
            | "what"
            | "when"
            | "where"
            | "which"
            | "with"
            | "would"
            | "yes"
            | "you"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(name: &str, description: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: description.to_string(),
            parameters: json!({
                "type": "object",
                "properties": { "query": { "type": "string" } }
            }),
        }
    }

    fn registry() -> Vec<ToolSpec> {
        [
            ("task_plan", "break work into steps"),
            ("file_read", "read a file"),
            ("shell", "run a shell command"),
            (
                "process",
                "start and manage long-running services and development servers",
            ),
            ("docker", "manage Docker containers and images"),
            ("packet_capture", "capture network packets with tshark"),
            ("git_operations", "clone pull push and commit git repositories"),
            ("db_query", "query a SQL or MongoDB database"),
            ("web_search_tool", "search the web for current news"),
            ("web_fetch", "read the full content of a web URL"),
            ("pushover", "send a Pushover notification"),
            ("arxiv_search", "search arXiv papers"),
            ("service_control", "start and stop system services"),
            ("workspace_rag", "search the document inbox"),
        ]
        .into_iter()
        .map(|(name, description)| spec(name, description))
        .collect()
    }

    #[test]
    fn keeps_relevant_essential_and_dependency_tools() {
        let route = route_tools(&registry(), "find the latest news online", 1);
        assert_eq!(route.strategy, ToolRouteStrategy::Lexical);
        assert!(route.selected.contains(&"web_search_tool".to_string()));
        assert!(route.selected.contains(&"web_fetch".to_string()));
        assert!(route.selected.contains(&"shell".to_string()));
        assert!(route.excluded.contains(&"pushover".to_string()));
    }

    #[test]
    fn process_remains_available_when_routing_selects_other_tools() {
        let route = route_tools(&registry(), "find the latest news online", 1);
        assert_eq!(route.strategy, ToolRouteStrategy::Lexical);
        assert!(route.selected.contains(&"process".to_string()));
    }

    #[test]
    fn database_alias_routes_to_database_tool() {
        let route = route_tools(&registry(), "inspect the postgres records", 1);
        assert!(route.selected.contains(&"db_query".to_string()));
        assert!(route.excluded.contains(&"docker".to_string()));
    }

    #[test]
    fn top_k_zero_disables_routing() {
        let route = route_tools(&registry(), "anything", 0);
        assert_eq!(route.strategy, ToolRouteStrategy::Disabled);
        assert!(route.excluded.is_empty());
    }

    #[test]
    fn routes_small_registry_when_subset_is_possible() {
        let tools = vec![
            spec("docker", "manage containers"),
            spec("pushover", "send a notification"),
            spec("arxiv_search", "find academic papers"),
        ];
        let route = route_tools(&tools, "send an alert notification", 1);
        assert_eq!(route.selected, vec!["pushover"]);
        assert_eq!(route.excluded.len(), 2);
    }

    #[test]
    fn no_signal_fails_open_instead_of_hiding_tools() {
        let route = route_tools(&registry(), "yes, do that too", 1);
        assert_eq!(route.strategy, ToolRouteStrategy::FailOpenNoQuery);
        assert!(route.excluded.is_empty());
        assert_eq!(route.selected.len(), registry().len());
    }

    #[test]
    fn generic_schema_terms_fail_open_instead_of_selecting_arbitrarily() {
        let route = route_tools(&registry(), "what type is this", 1);
        assert_eq!(route.strategy, ToolRouteStrategy::FailOpenNoQuery);
        assert!(route.excluded.is_empty());

        let tools = vec![
            ToolSpec {
                name: "alpha".into(),
                description: "Perform alpha operations".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "widget": { "type": "string" } }
                }),
            },
            spec("beta", "Perform beta operations"),
            spec("gamma", "Perform gamma operations"),
        ];
        let schema_only = route_tools(&tools, "widget", 1);
        assert_eq!(schema_only.strategy, ToolRouteStrategy::FailOpenNoMatch);
        assert!(schema_only.excluded.is_empty());

        let weak_description = vec![
            spec("alpha", "Inspect a widget"),
            spec("beta", "Send a message"),
            spec("gamma", "Manage a service"),
        ];
        let weak = route_tools(&weak_description, "widget", 1);
        assert_eq!(weak.strategy, ToolRouteStrategy::FailOpenNoMatch);
        assert!(weak.excluded.is_empty());
    }

    #[test]
    fn unmatched_language_fails_open() {
        let route = route_tools(&registry(), "继续上一个任务", 1);
        assert_eq!(route.strategy, ToolRouteStrategy::FailOpenNoMatch);
        assert!(route.excluded.is_empty());
    }

    #[test]
    fn tokenization_is_case_and_separator_insensitive() {
        let tools = vec![
            spec("git_operations", "work with source repositories"),
            spec("packet_capture", "record traffic"),
            spec("pushover", "send alert"),
        ];
        let route = route_tools(&tools, "GIT-OPERATIONS for this REPOSITORY", 1);
        assert_eq!(route.selected, vec!["git_operations"]);
    }

    #[test]
    fn duplicate_names_do_not_consume_budget_or_duplicate_output() {
        let tools = vec![
            spec("docker", "manage containers"),
            spec("docker", "duplicate"),
            spec("pushover", "send alerts"),
            spec("arxiv_search", "find papers"),
        ];
        let route = route_tools(&tools, "manage a container", 1);
        assert_eq!(route.selected, vec!["docker"]);
        assert_eq!(route.excluded, vec!["pushover", "arxiv_search"]);
    }

    #[test]
    fn ambiguous_cutoff_fails_open_instead_of_hiding_an_equal_match() {
        let tools = vec![
            spec("zeta_tool", "inspect widget"),
            spec("alpha_tool", "inspect widget"),
            spec("other_tool", "send message"),
        ];
        let first = route_tools(&tools, "inspect widget", 1);
        let second = route_tools(&tools, "inspect widget", 1);
        assert_eq!(first.ranked, second.ranked);
        assert_eq!(first.strategy, ToolRouteStrategy::FailOpenAmbiguous);
        assert!(first.excluded.is_empty());
        assert_eq!(first.selected.len(), tools.len());
    }

    #[test]
    fn direct_selection_allows_only_registered_requested_tools() {
        let route = direct_selection(
            &registry(),
            &["file_write", "task_plan"],
            "forced file write",
        );
        assert_eq!(route.selected, vec!["task_plan"]);
        assert!(!route.excluded.contains(&"task_plan".to_string()));
        assert_eq!(route.strategy, ToolRouteStrategy::DirectIntent);
    }

    #[test]
    fn ranked_observability_is_bounded_to_selected_roots() {
        let route = route_tools(&registry(), "search web news papers research", 1);
        assert!(route.ranked.len() <= 1);
        assert!(route
            .ranked
            .iter()
            .all(|score| route.selected.contains(&score.name)));
    }

    #[test]
    fn sop_execution_keeps_required_continuation_tools() {
        let tools = [
            ("sop_execute", "Run a standard operating procedure"),
            ("sop_list", "List standard operating procedures"),
            ("sop_status", "Inspect SOP execution status"),
            ("sop_advance", "Advance the current SOP step"),
            ("sop_approve", "Approve a pending SOP step"),
            ("docker", "Manage containers"),
            ("pushover", "Send notifications"),
        ]
        .into_iter()
        .map(|(name, description)| spec(name, description))
        .collect::<Vec<_>>();
        let route = route_tools(&tools, "execute this SOP workflow", 1);
        for required in [
            "sop_execute",
            "sop_list",
            "sop_status",
            "sop_advance",
            "sop_approve",
        ] {
            assert!(route.selected.iter().any(|name| name == required));
        }
        assert!(route.excluded.contains(&"docker".to_string()));
    }

    #[test]
    fn schedule_adds_delivery_alternative_without_the_entire_cron_suite() {
        let tools = [
            ("schedule", "Manage scheduled shell jobs"),
            ("cron_add", "Create a delivered agent cron job"),
            ("cron_list", "List cron jobs"),
            ("cron_remove", "Remove a cron job"),
            ("docker", "Manage containers"),
        ]
        .into_iter()
        .map(|(name, description)| spec(name, description))
        .collect::<Vec<_>>();
        let route = route_tools(&tools, "schedule a recurring shell job", 1);
        assert!(route.selected.contains(&"schedule".to_string()));
        assert!(route.selected.contains(&"cron_add".to_string()));
        assert!(route.excluded.contains(&"cron_list".to_string()));
        assert!(route.excluded.contains(&"cron_remove".to_string()));
    }

    #[test]
    fn list_jobs_keeps_cron_list_reachable() {
        let tools = vec![
            spec("cron_list", "List scheduled cron jobs"),
            spec("sop_list", "List standard operating procedures"),
            spec("docker", "Manage containers"),
        ];
        let route = route_tools(&tools, "list jobs", 1);
        assert!(route.selected.contains(&"cron_list".to_string()));
    }

    #[test]
    fn immediate_email_routes_to_composio_not_scheduled_delivery() {
        let tools = vec![
            spec(
                "composio",
                "Execute actions on apps via Composio including Gmail",
            ),
            spec(
                "cron_add",
                "Create a scheduled job and deliver output through Email",
            ),
            spec("docker", "Manage containers"),
        ];
        let route = route_tools(&tools, "email Jane the report now", 1);
        assert_eq!(route.strategy, ToolRouteStrategy::Lexical);
        assert_eq!(route.selected, vec!["composio"]);
        assert!(route.excluded.contains(&"cron_add".to_string()));
    }

    #[test]
    fn enabled_registry_tools_remain_reachable_and_reduce_payload() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config {
            workspace_dir: temp.path().join("workspace"),
            config_path: temp.path().join("config.toml"),
            ..crate::config::Config::default()
        };
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        config.browser.enabled = true;
        config.browser.allowed_domains = vec!["example.com".into()];
        config.http_request.enabled = true;
        config.http_request.allowed_domains = vec!["example.com".into()];
        config.web_fetch.enabled = true;
        config.web_fetch.allowed_domains = vec!["example.com".into()];
        config.web_search.enabled = true;
        config.federation.enable_delegation = true;
        config.db_connections.push(crate::config::DbConnectionConfig {
            name: "routing-fixture".into(),
            driver: crate::config::DbDriver::Sqlite,
            uri: temp.path().join("fixture.sqlite3").display().to_string(),
            database: None,
            read_only: true,
            max_rows: 50,
            label: None,
        });

        let security = std::sync::Arc::new(crate::security::SecurityPolicy::default());
        let memory_config = crate::config::MemoryConfig {
            backend: "markdown".into(),
            ..crate::config::MemoryConfig::default()
        };
        let memory: std::sync::Arc<dyn crate::memory::Memory> = std::sync::Arc::from(
            crate::memory::create_memory(&memory_config, temp.path(), None).unwrap(),
        );
        let mut agents = HashMap::new();
        agents.insert(
            "routing-worker".to_string(),
            crate::config::DelegateAgentConfig {
                provider: "ollama".into(),
                model: "test-model".into(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 2,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 4,
            },
        );
        let tools = crate::tools::all_tools(
            std::sync::Arc::new(config.clone()),
            &security,
            memory,
            Some("routing-test-key"),
            None,
            &config.browser,
            &config.http_request,
            &config.web_fetch,
            &config.workspace_dir,
            &agents,
            Some("delegate-test-key"),
            &config,
        );
        let specs = tools.iter().map(|tool| tool.spec()).collect::<Vec<_>>();
        assert!(specs.len() >= 40, "expected a large registry, got {}", specs.len());

        for spec in &specs {
            let route = route_tools(&specs, &spec.name, 12);
            assert!(
                route.selected.iter().any(|name| name == &spec.name),
                "explicit tool name '{}' became unreachable via {:?}: {}",
                spec.name,
                route.strategy,
                route.reason
            );
        }

        let docker_route = route_tools(&specs, "inspect the Docker containers", 12);
        assert!(docker_route.selected.iter().any(|name| name == "docker"));
        let full_chars = serde_json::to_string(&specs).unwrap().chars().count();
        let selected_specs = docker_route.selected_specs(&specs);
        let selected_chars = serde_json::to_string(&selected_specs)
            .unwrap()
            .chars()
            .count();
        assert!(
            selected_chars * 100 < full_chars * 80,
            "representative route should cut at least 20% of tool payload: {selected_chars}/{full_chars} chars"
        );
        eprintln!(
            "tool-routing coverage: {} tools reachable; Docker route payload {selected_chars}/{full_chars} chars ({:.1}% reduction)",
            specs.len(),
            100.0 * (full_chars - selected_chars) as f64 / full_chars as f64,
        );
    }
}
