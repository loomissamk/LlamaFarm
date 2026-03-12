use crate::providers::ToolCall;
use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone)]
pub(crate) struct ParsedToolCall {
    pub(crate) name: String,
    pub(crate) arguments: serde_json::Value,
    pub(crate) tool_call_id: Option<String>,
}

pub(super) fn parse_arguments_value(raw: Option<&serde_json::Value>) -> serde_json::Value {
    match raw {
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new())),
        Some(value) => value.clone(),
        None => serde_json::Value::Object(serde_json::Map::new()),
    }
}

pub(super) fn raw_string_argument_hint(raw: Option<&serde_json::Value>) -> Option<&str> {
    raw.and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub(super) fn normalize_shell_command_from_raw(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let unwrapped = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
        })
        .unwrap_or(trimmed)
        .trim();

    if unwrapped.is_empty() {
        return None;
    }

    if (unwrapped.starts_with('{') && unwrapped.ends_with('}'))
        || (unwrapped.starts_with('[') && unwrapped.ends_with(']'))
    {
        return None;
    }

    if unwrapped.starts_with("http://") || unwrapped.starts_with("https://") {
        return build_curl_command(unwrapped).or_else(|| Some(unwrapped.to_string()));
    }

    Some(unwrapped.to_string())
}

fn normalize_string_argument(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let unwrapped = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
        })
        .unwrap_or(trimmed)
        .trim();

    (!unwrapped.is_empty()).then(|| unwrapped.to_string())
}

fn normalize_known_workspace_file_path(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let normalized = trimmed.replace('\\', "/");
    let lowered = normalized.to_ascii_lowercase();

    match lowered.as_str() {
        "agents.md" | "agent.md" | "agen.md" => "AGENTS.md".to_string(),
        "soul.md" => "SOUL.md".to_string(),
        "tools.md" | "tool.md" => "TOOLS.md".to_string(),
        "identity.md" => "IDENTITY.md".to_string(),
        "user.md" => "USER.md".to_string(),
        "./agents.md" | "./agent.md" | "./agen.md" => "./AGENTS.md".to_string(),
        "./soul.md" => "./SOUL.md".to_string(),
        "./tools.md" | "./tool.md" => "./TOOLS.md".to_string(),
        "./identity.md" => "./IDENTITY.md".to_string(),
        "./user.md" => "./USER.md".to_string(),
        _ => normalized,
    }
}

fn extract_alias_string(
    map: &serde_json::Map<String, serde_json::Value>,
    aliases: &[&str],
) -> Option<String> {
    aliases.iter().find_map(|alias| {
        map.get(*alias)
            .and_then(serde_json::Value::as_str)
            .and_then(normalize_string_argument)
    })
}

fn normalize_path_arguments(
    arguments: serde_json::Value,
    raw_string_hint: Option<&str>,
) -> serde_json::Value {
    match arguments {
        serde_json::Value::Object(mut map) => {
            if let Some(path) = map
                .get("path")
                .and_then(|v| v.as_str())
                .and_then(normalize_string_argument)
            {
                map.insert(
                    "path".to_string(),
                    serde_json::Value::String(normalize_known_workspace_file_path(&path)),
                );
                return serde_json::Value::Object(map);
            }

            if let Some(path) =
                extract_alias_string(&map, &["file_path", "filepath", "filename", "target"])
            {
                map.insert(
                    "path".to_string(),
                    serde_json::Value::String(normalize_known_workspace_file_path(&path)),
                );
                return serde_json::Value::Object(map);
            }

            if let Some(raw) = raw_string_hint.and_then(normalize_string_argument) {
                map.insert(
                    "path".to_string(),
                    serde_json::Value::String(normalize_known_workspace_file_path(&raw)),
                );
            }

            serde_json::Value::Object(map)
        }
        serde_json::Value::String(raw) => normalize_string_argument(&raw)
            .map(|path| serde_json::json!({ "path": normalize_known_workspace_file_path(&path) }))
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        _ => raw_string_hint
            .and_then(normalize_string_argument)
            .map(|path| serde_json::json!({ "path": normalize_known_workspace_file_path(&path) }))
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
    }
}

fn normalize_file_write_arguments(
    arguments: serde_json::Value,
    raw_string_hint: Option<&str>,
) -> serde_json::Value {
    match arguments {
        serde_json::Value::Object(mut map) => {
            if map
                .get("path")
                .and_then(|v| v.as_str())
                .and_then(normalize_string_argument)
                .is_none()
            {
                if let Some(path) =
                    extract_alias_string(&map, &["file_path", "filepath", "filename", "target"])
                {
                    map.insert(
                        "path".to_string(),
                        serde_json::Value::String(normalize_known_workspace_file_path(&path)),
                    );
                }
            } else if let Some(path) = map
                .get("path")
                .and_then(|v| v.as_str())
                .and_then(normalize_string_argument)
            {
                map.insert(
                    "path".to_string(),
                    serde_json::Value::String(normalize_known_workspace_file_path(&path)),
                );
            }

            if map
                .get("content")
                .and_then(|v| v.as_str())
                .and_then(normalize_string_argument)
                .is_none()
            {
                if let Some(content) =
                    extract_alias_string(&map, &["contents", "text", "body", "value"])
                {
                    map.insert("content".to_string(), serde_json::Value::String(content));
                } else if let Some(raw) = raw_string_hint.and_then(normalize_string_argument) {
                    map.insert("content".to_string(), serde_json::Value::String(raw));
                }
            }

            serde_json::Value::Object(map)
        }
        _ => raw_string_hint
            .and_then(normalize_string_argument)
            .map(|content| serde_json::json!({ "content": content }))
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
    }
}

fn normalize_file_edit_arguments(
    arguments: serde_json::Value,
    raw_string_hint: Option<&str>,
) -> serde_json::Value {
    match arguments {
        serde_json::Value::Object(mut map) => {
            if map
                .get("path")
                .and_then(|v| v.as_str())
                .and_then(normalize_string_argument)
                .is_none()
            {
                if let Some(path) =
                    extract_alias_string(&map, &["file_path", "filepath", "filename", "target"])
                {
                    map.insert(
                        "path".to_string(),
                        serde_json::Value::String(normalize_known_workspace_file_path(&path)),
                    );
                }
            } else if let Some(path) = map
                .get("path")
                .and_then(|v| v.as_str())
                .and_then(normalize_string_argument)
            {
                map.insert(
                    "path".to_string(),
                    serde_json::Value::String(normalize_known_workspace_file_path(&path)),
                );
            }

            if map
                .get("old_string")
                .and_then(|v| v.as_str())
                .and_then(normalize_string_argument)
                .is_none()
            {
                if let Some(old_string) =
                    extract_alias_string(&map, &["old_text", "old", "find", "search"])
                {
                    map.insert(
                        "old_string".to_string(),
                        serde_json::Value::String(old_string),
                    );
                }
            }

            if map
                .get("new_string")
                .and_then(|v| v.as_str())
                .and_then(normalize_string_argument)
                .is_none()
            {
                if let Some(new_string) =
                    extract_alias_string(&map, &["new_text", "new", "replace", "replacement"])
                {
                    map.insert(
                        "new_string".to_string(),
                        serde_json::Value::String(new_string),
                    );
                } else if let Some(raw) = raw_string_hint.and_then(normalize_string_argument) {
                    map.insert("new_string".to_string(), serde_json::Value::String(raw));
                }
            }

            serde_json::Value::Object(map)
        }
        _ => serde_json::Value::Object(serde_json::Map::new()),
    }
}

fn derive_glob_pattern(raw: &str) -> Option<String> {
    let candidate = normalize_string_argument(raw)?;
    if candidate.contains('*')
        || candidate.contains('?')
        || candidate.contains('[')
        || candidate.contains('{')
    {
        return Some(candidate);
    }

    let trimmed = candidate.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        return Some("**/*".to_string());
    }

    Some(format!("{trimmed}/**/*"))
}

fn normalize_glob_search_arguments(
    arguments: serde_json::Value,
    raw_string_hint: Option<&str>,
) -> serde_json::Value {
    match arguments {
        serde_json::Value::Object(mut map) => {
            if map
                .get("pattern")
                .and_then(|v| v.as_str())
                .and_then(derive_glob_pattern)
                .is_some()
            {
                return serde_json::Value::Object(map);
            }

            if let Some(pattern) =
                extract_alias_string(&map, &["path", "dir", "directory", "root"])
                    .and_then(|raw| derive_glob_pattern(&raw))
            {
                map.insert("pattern".to_string(), serde_json::Value::String(pattern));
                return serde_json::Value::Object(map);
            }

            if let Some(pattern) = raw_string_hint.and_then(derive_glob_pattern) {
                map.insert("pattern".to_string(), serde_json::Value::String(pattern));
            }

            serde_json::Value::Object(map)
        }
        serde_json::Value::String(raw) => derive_glob_pattern(&raw)
            .map(|pattern| serde_json::json!({ "pattern": pattern }))
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        _ => raw_string_hint
            .and_then(derive_glob_pattern)
            .map(|pattern| serde_json::json!({ "pattern": pattern }))
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
    }
}

fn normalize_content_search_arguments(
    arguments: serde_json::Value,
    raw_string_hint: Option<&str>,
) -> serde_json::Value {
    match arguments {
        serde_json::Value::Object(mut map) => {
            if map
                .get("pattern")
                .and_then(|v| v.as_str())
                .and_then(normalize_string_argument)
                .is_some()
            {
                return serde_json::Value::Object(map);
            }

            if let Some(pattern) =
                extract_alias_string(&map, &["query", "search", "text", "regex", "needle"])
            {
                map.insert("pattern".to_string(), serde_json::Value::String(pattern));
                return serde_json::Value::Object(map);
            }

            if let Some(raw) = raw_string_hint.and_then(normalize_string_argument) {
                map.insert("pattern".to_string(), serde_json::Value::String(raw));
            }

            serde_json::Value::Object(map)
        }
        serde_json::Value::String(raw) => normalize_string_argument(&raw)
            .map(|pattern| serde_json::json!({ "pattern": pattern }))
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        _ => raw_string_hint
            .and_then(normalize_string_argument)
            .map(|pattern| serde_json::json!({ "pattern": pattern }))
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
    }
}

fn normalize_web_search_arguments(
    arguments: serde_json::Value,
    raw_string_hint: Option<&str>,
) -> serde_json::Value {
    match arguments {
        serde_json::Value::Object(mut map) => {
            if map
                .get("query")
                .and_then(|v| v.as_str())
                .and_then(normalize_string_argument)
                .is_some()
            {
                return serde_json::Value::Object(map);
            }

            if let Some(query) =
                extract_alias_string(&map, &["q", "search_query", "input", "text", "value"])
            {
                map.insert("query".to_string(), serde_json::Value::String(query));
                return serde_json::Value::Object(map);
            }

            if let Some(raw) = raw_string_hint.and_then(normalize_string_argument) {
                map.insert("query".to_string(), serde_json::Value::String(raw));
            }

            serde_json::Value::Object(map)
        }
        serde_json::Value::String(raw) => normalize_string_argument(&raw)
            .map(|query| serde_json::json!({ "query": query }))
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        _ => raw_string_hint
            .and_then(normalize_string_argument)
            .map(|query| serde_json::json!({ "query": query }))
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
    }
}

pub(super) fn normalize_shell_arguments(
    arguments: serde_json::Value,
    raw_string_hint: Option<&str>,
) -> serde_json::Value {
    match arguments {
        serde_json::Value::Object(mut map) => {
            if map
                .get("command")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .is_some_and(|cmd| !cmd.is_empty())
            {
                return serde_json::Value::Object(map);
            }

            for alias in [
                "cmd",
                "script",
                "shell_command",
                "command_line",
                "bash",
                "sh",
                "input",
            ] {
                if let Some(value) = map.get(alias).and_then(|v| v.as_str()) {
                    if let Some(command) = normalize_shell_command_from_raw(value) {
                        map.insert("command".to_string(), serde_json::Value::String(command));
                        return serde_json::Value::Object(map);
                    }
                }
            }

            if let Some(url) = map
                .get("url")
                .or_else(|| map.get("http_url"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|url| !url.is_empty())
            {
                if let Some(command) = normalize_shell_command_from_raw(url) {
                    map.insert("command".to_string(), serde_json::Value::String(command));
                    return serde_json::Value::Object(map);
                }
            }

            if let Some(raw) = raw_string_hint.and_then(normalize_shell_command_from_raw) {
                map.insert("command".to_string(), serde_json::Value::String(raw));
            }

            serde_json::Value::Object(map)
        }
        serde_json::Value::String(raw) => normalize_shell_command_from_raw(&raw)
            .map(|command| serde_json::json!({ "command": command }))
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        _ => raw_string_hint
            .and_then(normalize_shell_command_from_raw)
            .map(|command| serde_json::json!({ "command": command }))
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
    }
}

pub(super) fn normalize_tool_arguments(
    tool_name: &str,
    arguments: serde_json::Value,
    raw_string_hint: Option<&str>,
) -> serde_json::Value {
    match map_tool_name_alias(tool_name) {
        "shell" => normalize_shell_arguments(arguments, raw_string_hint),
        "file_read" => normalize_path_arguments(arguments, raw_string_hint),
        "file_write" => normalize_file_write_arguments(arguments, raw_string_hint),
        "file_edit" => normalize_file_edit_arguments(arguments, raw_string_hint),
        "glob_search" => normalize_glob_search_arguments(arguments, raw_string_hint),
        "content_search" => normalize_content_search_arguments(arguments, raw_string_hint),
        "web_search_tool" => normalize_web_search_arguments(arguments, raw_string_hint),
        _ => arguments,
    }
}

pub(super) fn parse_tool_call_id(
    root: &serde_json::Value,
    function: Option<&serde_json::Value>,
) -> Option<String> {
    function
        .and_then(|func| func.get("id"))
        .or_else(|| root.get("id"))
        .or_else(|| root.get("tool_call_id"))
        .or_else(|| root.get("call_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
}

pub(super) fn canonicalize_json_for_tool_signature(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort_unstable();
            let mut ordered = serde_json::Map::new();
            for key in keys {
                if let Some(child) = map.get(&key) {
                    ordered.insert(key, canonicalize_json_for_tool_signature(child));
                }
            }
            serde_json::Value::Object(ordered)
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(canonicalize_json_for_tool_signature)
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn normalize_browser_action_for_signature(action: &str) -> String {
    action.trim().to_ascii_lowercase().replace('-', "_")
}

fn canonicalize_browser_signature(arguments: &serde_json::Value) -> serde_json::Value {
    let canonical_args = canonicalize_json_for_tool_signature(arguments);
    let serde_json::Value::Object(mut map) = canonical_args else {
        return canonical_args;
    };

    let action = map
        .get("action")
        .and_then(serde_json::Value::as_str)
        .map(normalize_browser_action_for_signature)
        .or_else(|| {
            map.get("url")
                .and_then(serde_json::Value::as_str)
                .map(|_| "open".to_string())
        });

    if let Some(action) = action {
        map.insert(
            "action".to_string(),
            serde_json::Value::String(action.clone()),
        );

        if action == "open" {
            let mut reduced = serde_json::Map::new();
            reduced.insert("action".to_string(), serde_json::Value::String(action));
            if let Some(url) = map
                .get("url")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|url| !url.is_empty())
            {
                reduced.insert(
                    "url".to_string(),
                    serde_json::Value::String(url.to_string()),
                );
            }
            return serde_json::Value::Object(reduced);
        }
    }

    serde_json::Value::Object(map)
}

fn canonicalize_browser_open_signature(arguments: &serde_json::Value) -> serde_json::Value {
    let canonical_args = canonicalize_json_for_tool_signature(arguments);
    let serde_json::Value::Object(map) = canonical_args else {
        return canonical_args;
    };

    let mut reduced = serde_json::Map::new();
    if let Some(url) = map
        .get("url")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
    {
        reduced.insert(
            "url".to_string(),
            serde_json::Value::String(url.to_string()),
        );
    }

    serde_json::Value::Object(reduced)
}

pub(super) fn tool_call_signature(name: &str, arguments: &serde_json::Value) -> (String, String) {
    let normalized_name = name.trim().to_ascii_lowercase();
    let canonical_args = match normalized_name.as_str() {
        "browser" => canonicalize_browser_signature(arguments),
        "browser_open" => canonicalize_browser_open_signature(arguments),
        _ => canonicalize_json_for_tool_signature(arguments),
    };
    let args_json = serde_json::to_string(&canonical_args).unwrap_or_else(|_| "{}".to_string());
    (normalized_name, args_json)
}

pub(super) fn parse_tool_call_value(value: &serde_json::Value) -> Option<ParsedToolCall> {
    fn tool_name_from_value(value: &serde_json::Value) -> Option<String> {
        if let Some(name) = value
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            return Some(name.to_string());
        }

        value
            .get("tool")
            .or_else(|| value.get("tool_name"))
            .or_else(|| value.get("function_name"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| map_tool_name_alias(name).to_string())
    }

    fn inferred_arguments_from_object(
        value: &serde_json::Value,
        tool_name: &str,
    ) -> serde_json::Value {
        let Some(object) = value.as_object() else {
            return serde_json::Value::Object(serde_json::Map::new());
        };

        let mut inferred = serde_json::Map::new();
        for (key, child) in object {
            if matches!(
                key.as_str(),
                "id" | "type"
                    | "name"
                    | "tool"
                    | "tool_name"
                    | "function"
                    | "function_name"
                    | "tool_call_id"
                    | "call_id"
            ) {
                continue;
            }
            inferred.insert(key.clone(), child.clone());
        }

        normalize_tool_arguments(tool_name, serde_json::Value::Object(inferred), None)
    }

    if let Some(function) = value.get("function") {
        let tool_call_id = parse_tool_call_id(value, Some(function));
        let name = tool_name_from_value(function).unwrap_or_default();
        if !name.is_empty() {
            let raw_arguments = function
                .get("arguments")
                .or_else(|| function.get("parameters"))
                .or_else(|| function.get("args"))
                .or_else(|| function.get("params"));
            let arguments = if raw_arguments.is_some() {
                normalize_tool_arguments(
                    &name,
                    parse_arguments_value(raw_arguments),
                    raw_string_argument_hint(raw_arguments),
                )
            } else {
                inferred_arguments_from_object(function, &name)
            };
            return Some(ParsedToolCall {
                name,
                arguments,
                tool_call_id,
            });
        }
    }

    let tool_call_id = parse_tool_call_id(value, None);
    let name = tool_name_from_value(value).unwrap_or_default();

    if name.is_empty() {
        return None;
    }

    let raw_arguments = value
        .get("arguments")
        .or_else(|| value.get("parameters"))
        .or_else(|| value.get("args"))
        .or_else(|| value.get("params"));
    let arguments = if raw_arguments.is_some() {
        normalize_tool_arguments(
            &name,
            parse_arguments_value(raw_arguments),
            raw_string_argument_hint(raw_arguments),
        )
    } else {
        inferred_arguments_from_object(value, &name)
    };
    Some(ParsedToolCall {
        name,
        arguments,
        tool_call_id,
    })
}

pub(super) fn parse_tool_calls_from_json_value(value: &serde_json::Value) -> Vec<ParsedToolCall> {
    let mut calls = Vec::new();

    if let Some(tool_calls) = value.get("tool_calls").and_then(|v| v.as_array()) {
        for call in tool_calls {
            if let Some(parsed) = parse_tool_call_value(call) {
                calls.push(parsed);
            }
        }

        if !calls.is_empty() {
            return calls;
        }
    }

    if let Some(message) = value.get("message") {
        let nested = parse_tool_calls_from_json_value(message);
        if !nested.is_empty() {
            return nested;
        }
    }

    if let Some(choices) = value.get("choices").and_then(|v| v.as_array()) {
        for choice in choices {
            if let Some(message) = choice.get("message") {
                let nested = parse_tool_calls_from_json_value(message);
                if !nested.is_empty() {
                    calls.extend(nested);
                }
            }
        }
        if !calls.is_empty() {
            return calls;
        }
    }

    if let Some(array) = value.as_array() {
        for item in array {
            if let Some(parsed) = parse_tool_call_value(item) {
                calls.push(parsed);
            }
        }
        return calls;
    }

    if let Some(parsed) = parse_tool_call_value(value) {
        calls.push(parsed);
    }

    calls
}

fn extract_tool_text_from_json_value(value: &serde_json::Value) -> Option<String> {
    if let Some(content) = value
        .get("content")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Some(content.to_string());
    }

    if let Some(message) = value.get("message") {
        if let Some(content) = extract_tool_text_from_json_value(message) {
            return Some(content);
        }
    }

    if let Some(choices) = value.get("choices").and_then(|v| v.as_array()) {
        for choice in choices {
            if let Some(content) = extract_tool_text_from_json_value(choice) {
                return Some(content);
            }
        }
    }

    None
}

pub(super) fn is_xml_meta_tag(tag: &str) -> bool {
    let normalized = tag.to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "tool_call"
            | "toolcall"
            | "tool-call"
            | "invoke"
            | "thinking"
            | "thought"
            | "analysis"
            | "reasoning"
            | "reflection"
    )
}

/// Match opening XML tags: `<tag_name>`.  Does NOT use backreferences.
static XML_OPEN_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([a-zA-Z_][a-zA-Z0-9_-]*)>").unwrap());

/// MiniMax XML invoke format:
/// `<invoke name="shell"><parameter name="command">pwd</parameter></invoke>`
static MINIMAX_INVOKE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<invoke\b[^>]*\bname\s*=\s*(?:"([^"]+)"|'([^']+)')[^>]*>(.*?)</invoke>"#)
        .unwrap()
});

static MINIMAX_PARAMETER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?is)<parameter\b[^>]*\bname\s*=\s*(?:"([^"]+)"|'([^']+)')[^>]*>(.*?)</parameter>"#,
    )
    .unwrap()
});

/// Extracts all `<tag>…</tag>` pairs from `input`, returning `(tag_name, inner_content)`.
/// Handles matching closing tags without regex backreferences.
pub(super) fn extract_xml_pairs(input: &str) -> Vec<(&str, &str)> {
    let mut results = Vec::new();
    let mut search_start = 0;
    while let Some(open_cap) = XML_OPEN_TAG_RE.captures(&input[search_start..]) {
        let full_open = open_cap.get(0).unwrap();
        let tag_name = open_cap.get(1).unwrap().as_str();
        let open_end = search_start + full_open.end();

        let closing_tag = format!("</{tag_name}>");
        if let Some(close_pos) = input[open_end..].find(&closing_tag) {
            let inner = &input[open_end..open_end + close_pos];
            results.push((tag_name, inner.trim()));
            search_start = open_end + close_pos + closing_tag.len();
        } else {
            search_start = open_end;
        }
    }
    results
}

/// Parse XML-style tool calls in `<tool_call>` bodies.
/// Supports both nested argument tags and JSON argument payloads:
/// - `<memory_recall><query>...</query></memory_recall>`
/// - `<shell>{"command":"pwd"}</shell>`
pub(super) fn parse_xml_tool_calls(xml_content: &str) -> Option<Vec<ParsedToolCall>> {
    let mut calls = Vec::new();
    let trimmed = xml_content.trim();

    if !trimmed.starts_with('<') || !trimmed.contains('>') {
        return None;
    }

    for (tool_name_str, inner_content) in extract_xml_pairs(trimmed) {
        let tool_name = tool_name_str.to_string();
        if is_xml_meta_tag(&tool_name) {
            continue;
        }

        if inner_content.is_empty() {
            continue;
        }

        let mut args = serde_json::Map::new();

        if let Some(first_json) = extract_json_values(inner_content).into_iter().next() {
            match first_json {
                serde_json::Value::Object(object_args) => {
                    args = object_args;
                }
                other => {
                    args.insert("value".to_string(), other);
                }
            }
        } else {
            for (key_str, value) in extract_xml_pairs(inner_content) {
                let key = key_str.to_string();
                if is_xml_meta_tag(&key) {
                    continue;
                }
                if !value.is_empty() {
                    args.insert(key, serde_json::Value::String(value.to_string()));
                }
            }

            if args.is_empty() {
                args.insert(
                    "content".to_string(),
                    serde_json::Value::String(inner_content.to_string()),
                );
            }
        }

        calls.push(ParsedToolCall {
            name: tool_name,
            arguments: serde_json::Value::Object(args),
            tool_call_id: None,
        });
    }

    if calls.is_empty() {
        None
    } else {
        Some(calls)
    }
}

/// Parse MiniMax-style XML tool calls with attributed invoke/parameter tags.
pub(super) fn parse_minimax_invoke_calls(response: &str) -> Option<(String, Vec<ParsedToolCall>)> {
    let mut calls = Vec::new();
    let mut text_parts = Vec::new();
    let mut last_end = 0usize;

    for cap in MINIMAX_INVOKE_RE.captures_iter(response) {
        let Some(full_match) = cap.get(0) else {
            continue;
        };

        let before = response[last_end..full_match.start()].trim();
        if !before.is_empty() {
            text_parts.push(before.to_string());
        }

        let name = cap
            .get(1)
            .or_else(|| cap.get(2))
            .map(|m| m.as_str().trim())
            .filter(|v| !v.is_empty());
        let body = cap.get(3).map(|m| m.as_str()).unwrap_or("").trim();
        last_end = full_match.end();

        let Some(name) = name else {
            continue;
        };

        let mut args = serde_json::Map::new();
        for param_cap in MINIMAX_PARAMETER_RE.captures_iter(body) {
            let key = param_cap
                .get(1)
                .or_else(|| param_cap.get(2))
                .map(|m| m.as_str().trim())
                .unwrap_or_default();
            if key.is_empty() {
                continue;
            }
            let value = param_cap
                .get(3)
                .map(|m| m.as_str().trim())
                .unwrap_or_default();
            if value.is_empty() {
                continue;
            }

            let parsed = extract_json_values(value).into_iter().next();
            args.insert(
                key.to_string(),
                parsed.unwrap_or_else(|| serde_json::Value::String(value.to_string())),
            );
        }

        if args.is_empty() {
            if let Some(first_json) = extract_json_values(body).into_iter().next() {
                match first_json {
                    serde_json::Value::Object(obj) => args = obj,
                    other => {
                        args.insert("value".to_string(), other);
                    }
                }
            } else if !body.is_empty() {
                args.insert(
                    "content".to_string(),
                    serde_json::Value::String(body.to_string()),
                );
            }
        }

        calls.push(ParsedToolCall {
            name: name.to_string(),
            arguments: serde_json::Value::Object(args),
            tool_call_id: None,
        });
    }

    if calls.is_empty() {
        return None;
    }

    let after = response[last_end..].trim();
    if !after.is_empty() {
        text_parts.push(after.to_string());
    }

    let text = text_parts
        .join("\n")
        .replace("<minimax:tool_call>", "")
        .replace("</minimax:tool_call>", "")
        .replace("<minimax:toolcall>", "")
        .replace("</minimax:toolcall>", "")
        .trim()
        .to_string();

    Some((text, calls))
}

const TOOL_CALL_OPEN_TAGS: [&str; 6] = [
    "<tool_call>",
    "<toolcall>",
    "<tool-call>",
    "<invoke>",
    "<minimax:tool_call>",
    "<minimax:toolcall>",
];

const TOOL_CALL_CLOSE_TAGS: [&str; 6] = [
    "</tool_call>",
    "</toolcall>",
    "</tool-call>",
    "</invoke>",
    "</minimax:tool_call>",
    "</minimax:toolcall>",
];

pub(super) fn find_first_tag<'a>(haystack: &str, tags: &'a [&'a str]) -> Option<(usize, &'a str)> {
    tags.iter()
        .filter_map(|tag| haystack.find(tag).map(|idx| (idx, *tag)))
        .min_by_key(|(idx, _)| *idx)
}

pub(super) fn matching_tool_call_close_tag(open_tag: &str) -> Option<&'static str> {
    match open_tag {
        "<tool_call>" => Some("</tool_call>"),
        "<toolcall>" => Some("</toolcall>"),
        "<tool-call>" => Some("</tool-call>"),
        "<invoke>" => Some("</invoke>"),
        "<minimax:tool_call>" => Some("</minimax:tool_call>"),
        "<minimax:toolcall>" => Some("</minimax:toolcall>"),
        _ => None,
    }
}

pub(super) fn extract_first_json_value_with_end(input: &str) -> Option<(serde_json::Value, usize)> {
    let trimmed = input.trim_start();
    let trim_offset = input.len().saturating_sub(trimmed.len());

    let mut stream = serde_json::Deserializer::from_str(trimmed).into_iter::<serde_json::Value>();
    if let Some(Ok(value)) = stream.next() {
        let consumed = stream.byte_offset();
        if consumed > 0 {
            return Some((value, trim_offset + consumed));
        }
    }

    for (byte_idx, ch) in trimmed.char_indices() {
        if ch != '{' && ch != '[' {
            continue;
        }

        let slice = &trimmed[byte_idx..];
        let mut stream = serde_json::Deserializer::from_str(slice).into_iter::<serde_json::Value>();
        if let Some(Ok(value)) = stream.next() {
            let consumed = stream.byte_offset();
            if consumed > 0 {
                return Some((value, trim_offset + byte_idx + consumed));
            }
        }
    }

    None
}

pub(super) fn strip_leading_close_tags(mut input: &str) -> &str {
    loop {
        let trimmed = input.trim_start();
        if !trimmed.starts_with("</") {
            return trimmed;
        }

        let Some(close_end) = trimmed.find('>') else {
            return "";
        };
        input = &trimmed[close_end + 1..];
    }
}

/// Extract JSON values from a string.
///
/// # Security Warning
///
/// This function extracts ANY JSON objects/arrays from the input. It MUST only
/// be used on content that is already trusted to be from the LLM, such as
/// content inside `<invoke>` tags where the LLM has explicitly indicated intent
/// to make a tool call. Do NOT use this on raw user input or content that
/// could contain prompt injection payloads.
pub(super) fn extract_json_values(input: &str) -> Vec<serde_json::Value> {
    let mut values = Vec::new();
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return values;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        values.push(value);
        return values;
    }

    let char_positions: Vec<(usize, char)> = trimmed.char_indices().collect();
    let mut idx = 0;
    while idx < char_positions.len() {
        let (byte_idx, ch) = char_positions[idx];
        if ch == '{' || ch == '[' {
            let slice = &trimmed[byte_idx..];
            let mut stream =
                serde_json::Deserializer::from_str(slice).into_iter::<serde_json::Value>();
            if let Some(Ok(value)) = stream.next() {
                let consumed = stream.byte_offset();
                if consumed > 0 {
                    values.push(value);
                    let next_byte = byte_idx + consumed;
                    while idx < char_positions.len() && char_positions[idx].0 < next_byte {
                        idx += 1;
                    }
                    continue;
                }
            }
        }
        idx += 1;
    }

    values
}

/// Find the end position of a JSON object by tracking balanced braces.
pub(super) fn find_json_end(input: &str) -> Option<usize> {
    let trimmed = input.trim_start();
    let offset = input.len() - trimmed.len();

    if !trimmed.starts_with('{') {
        return None;
    }

    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in trimmed.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match ch {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset + i + ch.len_utf8());
                }
            }
            _ => {}
        }
    }

    None
}

/// Parse XML attribute-style tool calls from response text.
/// This handles MiniMax and similar providers that output:
/// ```xml
/// <minimax:toolcall>
/// <invoke name="shell">
/// <parameter name="command">ls</parameter>
/// </invoke>
/// </minimax:toolcall>
/// ```
pub(super) fn parse_xml_attribute_tool_calls(response: &str) -> Vec<ParsedToolCall> {
    let mut calls = Vec::new();

    // Regex to find <invoke name="toolname">...</invoke> blocks
    static INVOKE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<invoke\s+name="([^"]+)"[^>]*>(.*?)</invoke>"#).unwrap()
    });

    // Regex to find <parameter name="paramname">value</parameter>
    static PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"<parameter\s+name="([^"]+)"[^>]*>([^<]*)</parameter>"#).unwrap()
    });

    for cap in INVOKE_RE.captures_iter(response) {
        let tool_name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let inner = cap.get(2).map(|m| m.as_str()).unwrap_or("");

        if tool_name.is_empty() {
            continue;
        }

        let mut arguments = serde_json::Map::new();

        for param_cap in PARAM_RE.captures_iter(inner) {
            let param_name = param_cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let param_value = param_cap.get(2).map(|m| m.as_str()).unwrap_or("");

            if !param_name.is_empty() {
                arguments.insert(
                    param_name.to_string(),
                    serde_json::Value::String(param_value.to_string()),
                );
            }
        }

        if !arguments.is_empty() {
            calls.push(ParsedToolCall {
                name: map_tool_name_alias(tool_name).to_string(),
                arguments: serde_json::Value::Object(arguments),
                tool_call_id: None,
            });
        }
    }

    calls
}

/// Parse Perl/hash-ref style tool calls from response text.
/// This handles formats like:
/// ```text
/// TOOL_CALL
/// {tool => "shell", args => {
///   --command "ls -la"
///   --description "List current directory contents"
/// }}
/// /TOOL_CALL
/// ```
pub(super) fn parse_perl_style_tool_calls(response: &str) -> Vec<ParsedToolCall> {
    let mut calls = Vec::new();

    // Regex to find TOOL_CALL blocks - handle double closing braces }}
    static PERL_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)TOOL_CALL\s*\{(.+?)\}\}\s*/TOOL_CALL").unwrap());

    // Regex to find tool => "name" in the content
    static TOOL_NAME_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"tool\s*=>\s*"([^"]+)""#).unwrap());

    // Regex to find args => { ... } block
    static ARGS_BLOCK_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)args\s*=>\s*\{(.+?)\}").unwrap());

    // Regex to find --key "value" pairs
    static ARGS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"--(\w+)\s+"([^"]+)""#).unwrap());

    for cap in PERL_RE.captures_iter(response) {
        let content = cap.get(1).map(|m| m.as_str()).unwrap_or("");

        // Extract tool name
        let tool_name = TOOL_NAME_RE
            .captures(content)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
            .unwrap_or("");

        if tool_name.is_empty() {
            continue;
        }

        // Extract args block
        let args_block = ARGS_BLOCK_RE
            .captures(content)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
            .unwrap_or("");

        let mut arguments = serde_json::Map::new();

        for arg_cap in ARGS_RE.captures_iter(args_block) {
            let key = arg_cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let value = arg_cap.get(2).map(|m| m.as_str()).unwrap_or("");

            if !key.is_empty() {
                arguments.insert(
                    key.to_string(),
                    serde_json::Value::String(value.to_string()),
                );
            }
        }

        if !arguments.is_empty() {
            calls.push(ParsedToolCall {
                name: map_tool_name_alias(tool_name).to_string(),
                arguments: serde_json::Value::Object(arguments),
                tool_call_id: None,
            });
        }
    }

    calls
}

/// Parse FunctionCall-style tool calls from response text.
/// This handles formats like:
/// ```text
/// <FunctionCall>
/// file_read
/// <code>path>/Users/kylelampa/Documents/llamafarm/README.md</code>
/// </FunctionCall>
/// ```
pub(super) fn parse_function_call_tool_calls(response: &str) -> Vec<ParsedToolCall> {
    let mut calls = Vec::new();

    // Regex to find <FunctionCall> blocks
    static FUNC_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<FunctionCall>\s*(\w+)\s*<code>([^<]+)</code>\s*</FunctionCall>").unwrap()
    });

    for cap in FUNC_RE.captures_iter(response) {
        let tool_name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let args_text = cap.get(2).map(|m| m.as_str()).unwrap_or("");

        if tool_name.is_empty() {
            continue;
        }

        // Parse key>value pairs (e.g., path>/Users/.../file.txt)
        let mut arguments = serde_json::Map::new();
        for line in args_text.lines() {
            let line = line.trim();
            if let Some(pos) = line.find('>') {
                let key = line[..pos].trim();
                let value = line[pos + 1..].trim();
                if !key.is_empty() && !value.is_empty() {
                    arguments.insert(
                        key.to_string(),
                        serde_json::Value::String(value.to_string()),
                    );
                }
            }
        }

        if !arguments.is_empty() {
            calls.push(ParsedToolCall {
                name: map_tool_name_alias(tool_name).to_string(),
                arguments: serde_json::Value::Object(arguments),
                tool_call_id: None,
            });
        }
    }

    calls
}

/// Parse GLM-style tool calls from response text.
/// Map tool name aliases from various LLM providers to LlamaFarm tool names.
/// This handles variations like "fileread" -> "file_read", "bash" -> "shell", etc.
pub(super) fn map_tool_name_alias(tool_name: &str) -> &str {
    match tool_name {
        // Shell variations (including GLM aliases that map to shell)
        "shell"
        | "bash"
        | "sh"
        | "exec"
        | "command"
        | "cmd"
        | "run_command"
        | "run_shell_command"
        | "execute_command"
        | "execute_shell_command"
        | "exec_command"
        | "terminal"
        | "terminal_exec"
        | "terminal_execute" => "shell",
        "browser_open" | "browser" => "shell",
        // Messaging variations
        "send_message" | "sendmessage" => "message_send",
        // File tool variations
        "fileread" | "file_read" | "readfile" | "read_file" | "open_file" | "file" => {
            "file_read"
        }
        "filewrite" | "file_write" | "writefile" | "write_file" | "create_file"
        | "save_file" | "write_to_file" => "file_write",
        "fileedit" | "file_edit" | "editfile" | "edit_file" | "modify_file"
        | "replace_in_file" => "file_edit",
        "filelist" | "file_list" | "listfiles" | "list_files" | "list_dir"
        | "list_directory" | "directory_list" => "glob_search",
        // Search variations
        "content_search" | "search_files" | "search_in_files" | "grep_search"
        | "ripgrep_search" => "content_search",
        "glob_search" | "find_files" | "glob" => "glob_search",
        "web_search" | "web_search_tool" | "search_web" | "internet_search" => {
            "web_search_tool"
        }
        // Memory variations
        "memoryrecall" | "memory_recall" | "recall" | "memrecall" => "memory_recall",
        "memorystore" | "memory_store" | "store" | "memstore" => "memory_store",
        "memoryforget" | "memory_forget" | "forget" | "memforget" => "memory_forget",
        // HTTP variations
        "http_request" | "http" | "fetch" | "curl" | "wget" => "http_request",
        _ => tool_name,
    }
}

pub(super) fn build_curl_command(url: &str) -> Option<String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }

    if url.chars().any(char::is_whitespace) {
        return None;
    }

    let escaped = url.replace('\'', r#"'\\''"#);
    Some(format!("curl -s '{}'", escaped))
}

pub(super) fn parse_glm_style_tool_calls(
    text: &str,
) -> Vec<(String, serde_json::Value, Option<String>)> {
    let mut calls = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: tool_name/param>value or tool_name/{json}
        if let Some(pos) = line.find('/') {
            let tool_part = &line[..pos];
            let rest = &line[pos + 1..];

            if tool_part.chars().all(|c| c.is_alphanumeric() || c == '_') {
                let tool_name = map_tool_name_alias(tool_part);

                if let Some(gt_pos) = rest.find('>') {
                    let param_name = rest[..gt_pos].trim();
                    let value = rest[gt_pos + 1..].trim();

                    let arguments = match tool_name {
                        "shell" => {
                            if param_name == "url" {
                                let Some(command) = build_curl_command(value) else {
                                    continue;
                                };
                                serde_json::json!({ "command": command })
                            } else if value.starts_with("http://") || value.starts_with("https://")
                            {
                                if let Some(command) = build_curl_command(value) {
                                    serde_json::json!({ "command": command })
                                } else {
                                    serde_json::json!({ "command": value })
                                }
                            } else {
                                serde_json::json!({ "command": value })
                            }
                        }
                        "http_request" => {
                            serde_json::json!({"url": value, "method": "GET"})
                        }
                        _ => serde_json::json!({ param_name: value }),
                    };
                    let arguments = normalize_tool_arguments(tool_name, arguments, Some(value));

                    calls.push((tool_name.to_string(), arguments, Some(line.to_string())));
                    continue;
                }

                if rest.starts_with('{') {
                    if let Ok(json_args) = serde_json::from_str::<serde_json::Value>(rest) {
                        let arguments = normalize_tool_arguments(tool_name, json_args, Some(rest));
                        calls.push((tool_name.to_string(), arguments, Some(line.to_string())));
                    }
                }
            }
        }

        // Plain URL
        if let Some(command) = build_curl_command(line) {
            calls.push((
                "shell".to_string(),
                serde_json::json!({ "command": command }),
                Some(line.to_string()),
            ));
        }
    }

    calls
}

/// Return the canonical default parameter name for a tool.
///
/// When a model emits a shortened call like `shell>uname -a` (without an
/// explicit `/param_name`), we need to infer which parameter the value maps
/// to. This function encodes the mapping for known LlamaFarm tools.
pub(super) fn default_param_for_tool(tool: &str) -> &'static str {
    match tool {
        "shell" | "bash" | "sh" | "exec" | "command" | "cmd" => "command",
        // All file tools default to "path"
        "file_read" | "fileread" | "readfile" | "read_file" | "file" | "file_write"
        | "filewrite" | "writefile" | "write_file" | "file_edit" | "fileedit" | "editfile"
        | "edit_file" => "path",
        "glob_search" | "filelist" | "file_list" | "listfiles" | "list_files" | "list_dir"
        | "list_directory" | "directory_list" | "find_files" | "glob" => "pattern",
        "content_search" | "search_files" | "search_in_files" | "grep_search"
        | "ripgrep_search" => "pattern",
        // Memory recall and forget both default to "query"
        "memory_recall" | "memoryrecall" | "recall" | "memrecall" | "memory_forget"
        | "memoryforget" | "forget" | "memforget" => "query",
        "memory_store" | "memorystore" | "store" | "memstore" => "content",
        // HTTP and browser tools default to "url"
        "http_request" | "http" | "fetch" | "curl" | "wget" | "browser_open" | "browser" => {
            "url"
        }
        "web_search" | "web_search_tool" | "search_web" | "internet_search" => "query",
        _ => "input",
    }
}

/// Parse GLM-style shortened tool call bodies found inside `<tool_call>` tags.
///
/// Handles three sub-formats that GLM-4.7 emits:
///
/// 1. **Shortened**: `tool_name>value` — single value mapped via
///    [`default_param_for_tool`].
/// 2. **YAML-like multi-line**: `tool_name>\nkey: value\nkey: value` — each
///    subsequent `key: value` line becomes a parameter.
/// 3. **Attribute-style**: `tool_name key="value" [/]>` — XML-like attributes.
///
/// Returns `None` if the body does not match any of these formats.
pub(super) fn parse_glm_shortened_body(body: &str) -> Option<ParsedToolCall> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }

    let function_style = body.find('(').and_then(|open| {
        if body.ends_with(')') && open > 0 {
            Some((body[..open].trim(), body[open + 1..body.len() - 1].trim()))
        } else {
            None
        }
    });

    // Check attribute-style FIRST: `tool_name key="value" />`
    // Must come before `>` check because `/>` contains `>` and would
    // misparse the tool name in the first branch.
    let (tool_raw, value_part) = if let Some((tool, args)) = function_style {
        (tool, args)
    } else if body.contains("=\"") || body.contains("='") {
        // Attribute-style: split at first whitespace to get tool name
        let split_pos = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
        let tool = body[..split_pos].trim();
        let attrs = body[split_pos..]
            .trim()
            .trim_end_matches("/>")
            .trim_end_matches('>')
            .trim_end_matches('/')
            .trim();
        (tool, attrs)
    } else if let Some(gt_pos) = body.find('>') {
        // GLM shortened: `tool_name>value`
        let tool = body[..gt_pos].trim();
        let value = body[gt_pos + 1..].trim();
        // Strip trailing self-close markers that some models emit
        let value = value.trim_end_matches("/>").trim_end_matches('/').trim();
        (tool, value)
    } else {
        return None;
    };

    // Validate tool name: must be alphanumeric + underscore only
    let tool_raw = tool_raw.trim_end_matches(|c: char| c.is_whitespace());
    if tool_raw.is_empty() || !tool_raw.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }

    let tool_name = map_tool_name_alias(tool_raw);

    if function_style.is_some() && (value_part.starts_with('{') || value_part.starts_with('[')) {
        if let Ok(json_args) = serde_json::from_str::<serde_json::Value>(value_part) {
            return Some(ParsedToolCall {
                name: tool_name.to_string(),
                arguments: normalize_tool_arguments(tool_name, json_args, Some(value_part)),
                tool_call_id: None,
            });
        }
    }

    // Try attribute-style: `key="value" key2="value2"`
    if value_part.contains("=\"") || value_part.contains("='") {
        let args = parse_attribute_pairs(value_part);
        if !args.is_empty() {
            return Some(ParsedToolCall {
                name: tool_name.to_string(),
                arguments: normalize_tool_arguments(
                    tool_name,
                    serde_json::Value::Object(args),
                    None,
                ),
                tool_call_id: None,
            });
        }
    }

    // Try YAML-style multi-line: each line is `key: value`
    if value_part.contains('\n') {
        let mut args = serde_json::Map::new();
        for line in value_part.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(colon_pos) = line.find(':') {
                let key = line[..colon_pos].trim();
                let value = line[colon_pos + 1..].trim();
                if !key.is_empty() && !value.is_empty() {
                    // Normalize boolean-like values
                    let json_value = match value {
                        "true" | "yes" => serde_json::Value::Bool(true),
                        "false" | "no" => serde_json::Value::Bool(false),
                        _ => serde_json::Value::String(value.to_string()),
                    };
                    args.insert(key.to_string(), json_value);
                }
            }
        }
        if !args.is_empty() {
            return Some(ParsedToolCall {
                name: tool_name.to_string(),
                arguments: normalize_tool_arguments(
                    tool_name,
                    serde_json::Value::Object(args),
                    None,
                ),
                tool_call_id: None,
            });
        }
    }

    // Single-value shortened: `tool>value`
    if !value_part.is_empty() {
        let param = default_param_for_tool(tool_raw);
        let arguments = match tool_name {
            "shell" => {
                if value_part.starts_with("http://") || value_part.starts_with("https://") {
                    if let Some(cmd) = build_curl_command(value_part) {
                        serde_json::json!({ "command": cmd })
                    } else {
                        serde_json::json!({ "command": value_part })
                    }
                } else {
                    serde_json::json!({ "command": value_part })
                }
            }
            "http_request" => serde_json::json!({"url": value_part, "method": "GET"}),
            _ => serde_json::json!({ param: value_part }),
        };
        return Some(ParsedToolCall {
            name: tool_name.to_string(),
            arguments: normalize_tool_arguments(tool_name, arguments, Some(value_part)),
            tool_call_id: None,
        });
    }

    None
}

fn strip_wrapping_json_code_fence(response: &str) -> &str {
    let trimmed = response.trim();
    for fence in ["```", "'''"] {
        let Some(inner) = trimmed.strip_prefix(fence) else {
            continue;
        };

        let inner = inner
            .strip_prefix("json")
            .or_else(|| inner.strip_prefix("JSON"))
            .unwrap_or(inner);
        if let Some(stripped) = inner.strip_suffix(fence).map(str::trim) {
            return stripped;
        }
    }

    trimmed
}

static ATTRIBUTE_PAIR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"([a-zA-Z_][a-zA-Z0-9_-]*)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap()
});

fn parse_attribute_pairs(input: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();

    for capture in ATTRIBUTE_PAIR_RE.captures_iter(input) {
        let Some(key) = capture.get(1).map(|m| m.as_str().trim()) else {
            continue;
        };
        if key.is_empty() {
            continue;
        }

        let value = capture
            .get(2)
            .or_else(|| capture.get(3))
            .map(|m| m.as_str())
            .unwrap_or_default();
        arguments.insert(key.to_string(), serde_json::Value::String(value.to_string()));
    }

    arguments
}

fn parse_simple_tool_scalar(raw: &str) -> serde_json::Value {
    let Some(value) = normalize_string_argument(raw) else {
        return serde_json::Value::Null;
    };

    match value.as_str() {
        "true" | "yes" => serde_json::Value::Bool(true),
        "false" | "no" => serde_json::Value::Bool(false),
        _ => value
            .parse::<i64>()
            .map(serde_json::Value::from)
            .or_else(|_| value.parse::<f64>().map(serde_json::Value::from))
            .unwrap_or_else(|_| serde_json::Value::String(value)),
    }
}

fn parse_simple_tool_key_value_body(body: &str) -> Option<serde_json::Value> {
    let mut arguments = serde_json::Map::new();

    for line in body.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Some((key_raw, value_raw)) = line
            .split_once(':')
            .or_else(|| line.split_once('='))
            .map(|(key, value)| (key.trim(), value.trim()))
        else {
            return None;
        };

        if key_raw.is_empty() || value_raw.is_empty() {
            return None;
        }

        let key_lower = key_raw.to_ascii_lowercase();
        if matches!(key_lower.as_str(), "arguments" | "args" | "parameters" | "params")
            && (value_raw.starts_with('{') || value_raw.starts_with('['))
        {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(value_raw) else {
                return None;
            };
            match value {
                serde_json::Value::Object(object) => arguments.extend(object),
                other => {
                    arguments.insert("value".to_string(), other);
                }
            }
            continue;
        }

        let value = parse_simple_tool_scalar(value_raw);
        if value.is_null() {
            return None;
        }
        arguments.insert(key_raw.to_string(), value);
    }

    (!arguments.is_empty()).then_some(serde_json::Value::Object(arguments))
}

fn parse_explicit_tool_header(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some((key, value)) = trimmed
        .split_once(':')
        .or_else(|| trimmed.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
    {
        if matches!(
            key.to_ascii_lowercase().as_str(),
            "tool" | "name" | "function" | "tool_name"
        ) {
            return (!value.is_empty()).then_some(value);
        }
    }

    if let Some(value) = trimmed.strip_suffix(':').map(str::trim) {
        if !value.is_empty() {
            return Some(value);
        }
    }

    Some(trimmed)
}

fn parse_explicit_tool_block_candidate(input: &str) -> Option<ParsedToolCall> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let first_newline = trimmed.find('\n')?;
    let header_line = trimmed[..first_newline].trim();
    let body = trimmed[first_newline + 1..].trim();
    if body.is_empty() {
        return None;
    }

    let tool_raw = parse_explicit_tool_header(header_line)?;
    if !tool_raw.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        return None;
    }

    let tool_name = map_tool_name_alias(tool_raw).to_string();
    let arguments = if body.starts_with('{') || body.starts_with('[') {
        let (value, consumed_end) = extract_first_json_value_with_end(body)?;
        if !body[consumed_end..].trim().is_empty() {
            return None;
        }
        normalize_tool_arguments(&tool_name, value, Some(body))
    } else if let Some(arguments) = parse_simple_tool_key_value_body(body) {
        normalize_tool_arguments(&tool_name, arguments, None)
    } else if tool_name == "shell" && looks_like_direct_shell_command(body) {
        serde_json::json!({ "command": body })
    } else {
        return None;
    };

    Some(ParsedToolCall {
        name: tool_name,
        arguments,
        tool_call_id: None,
    })
}

fn parse_explicit_tool_block_tool_calls(response: &str) -> Option<(String, Vec<ParsedToolCall>)> {
    if let Some(call) = parse_explicit_tool_block_candidate(response) {
        return Some((String::new(), vec![call]));
    }

    let mut lines = response.lines();
    let first_line = lines.next()?.trim();
    let rest = lines.collect::<Vec<_>>().join("\n");
    if rest.trim().is_empty() {
        return None;
    }

    if SHELL_BLOCK_INTENT_CUE_RE.is_match(first_line) || first_line.ends_with(':') {
        let call = parse_explicit_tool_block_candidate(rest.trim())?;
        return Some((first_line.to_string(), vec![call]));
    }

    None
}

fn parse_function_style_tool_calls(response: &str) -> Option<(String, Vec<ParsedToolCall>)> {
    let mut text_parts = Vec::new();
    let mut calls = Vec::new();

    for line in response.lines() {
        let trimmed = line.trim();
        if trimmed.contains('(') && trimmed.ends_with(')') {
            if let Some(call) = parse_glm_shortened_body(trimmed) {
                calls.push(call);
                continue;
            }
        }

        if !trimmed.is_empty() {
            text_parts.push(trimmed.to_string());
        }
    }

    (!calls.is_empty()).then(|| (text_parts.join("\n"), calls))
}

static FENCED_SHELL_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)(?:```|''')(?:bash|sh|shell|zsh|console)\s*\n(.*?)(?:(?:```|''')|$)")
        .unwrap()
});

static UNLABELED_FENCED_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)(?:```|''')\s*\n(.*?)(?:(?:```|''')|$)").unwrap());

const DIRECT_SHELL_COMMAND_PREFIXES: &[&str] = &[
    "./",
    "../",
    "bash ",
    "bun ",
    "cargo ",
    "cat ",
    "cd ",
    "clang ",
    "cmake ",
    "curl ",
    "date",
    "deno ",
    "df ",
    "docker ",
    "docker-compose ",
    "dotnet ",
    "du ",
    "echo ",
    "env",
    "find ",
    "g++ ",
    "gcc ",
    "git ",
    "go ",
    "grep ",
    "gradle ",
    "jq ",
    "java ",
    "javac ",
    "ls ",
    "lsblk",
    "lspci",
    "lsusb",
    "make",
    "mvn ",
    "node ",
    "ninja ",
    "npm ",
    "perl ",
    "php ",
    "pip ",
    "pip3 ",
    "pnpm ",
    "pwd",
    "pytest",
    "python ",
    "python3 ",
    "rg ",
    "ruby ",
    "sh ",
    "sqlite3 ",
    "ss ",
    "stat ",
    "tail ",
    "uname",
    "whoami",
    "yarn ",
];

static SHELL_BLOCK_INTENT_CUE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        (
            \bi(?:'ll|\s+will)?\s+(?:run|execute|check|inspect|open|read|search|fetch|list|show)\b|
            \blet\s+me\s+(?:run|execute|check|inspect|open|read|search|fetch|list|show)\b|
            \brunning\b|
            \bexecuting\b|
            \bchecking\b|
            \binspecting\b|
            command\s*:
        )
    ",
    )
    .unwrap()
});

fn normalize_recoverable_shell_block(body: &str) -> Option<String> {
    let mut commands = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let command = trimmed
            .strip_prefix("$ ")
            .or_else(|| trimmed.strip_prefix('$').map(str::trim_start))
            .or_else(|| {
                trimmed
                    .strip_prefix("> ")
                    .or_else(|| trimmed.strip_prefix('>').map(str::trim_start))
            })
            .unwrap_or(trimmed)
            .trim();

        if command.is_empty() {
            continue;
        }

        commands.push(command.to_string());
    }

    (!commands.is_empty()).then(|| commands.join("\n"))
}

fn looks_like_direct_shell_command(candidate: &str) -> bool {
    let trimmed = candidate.trim();
    if trimmed.is_empty() || trimmed.lines().count() != 1 {
        return false;
    }

    if trimmed.starts_with("/bin/")
        || trimmed.starts_with("/usr/bin/")
        || trimmed.starts_with("/usr/local/bin/")
    {
        return true;
    }

    let lowered = trimmed.to_ascii_lowercase();
    DIRECT_SHELL_COMMAND_PREFIXES
        .iter()
        .any(|prefix| lowered == *prefix || lowered.starts_with(prefix))
}

fn parse_plain_shell_command_tool_call(response: &str) -> Option<(String, Vec<ParsedToolCall>)> {
    let trimmed = response.trim();
    if !looks_like_direct_shell_command(trimmed) {
        return None;
    }

    Some((
        String::new(),
        vec![ParsedToolCall {
            name: "shell".to_string(),
            arguments: serde_json::json!({ "command": trimmed }),
            tool_call_id: None,
        }],
    ))
}

fn parse_json_wrapped_tool_calls(response: &str) -> Option<(String, Vec<ParsedToolCall>)> {
    let trimmed = response.trim();
    let inner = trimmed
        .strip_prefix("json")
        .or_else(|| trimmed.strip_prefix("JSON"))?
        .trim_start();
    let inner = inner.strip_prefix('{')?.strip_suffix('}')?.trim();
    if inner.is_empty() {
        return None;
    }

    let json_candidate = format!("{{{inner}}}");
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&json_candidate) {
        let calls = parse_tool_calls_from_json_value(&value);
        if !calls.is_empty() {
            return Some((String::new(), calls));
        }
    }

    if let Some(call) = parse_glm_shortened_body(inner) {
        return Some((String::new(), vec![call]));
    }

    None
}

fn looks_like_recoverable_shell_line(line: &str) -> bool {
    let candidate = line
        .split("&&")
        .next()
        .unwrap_or(line)
        .split("||")
        .next()
        .unwrap_or(line)
        .split(';')
        .next()
        .unwrap_or(line)
        .trim();

    looks_like_direct_shell_command(candidate)
        || candidate.starts_with("./")
        || candidate.starts_with("../")
        || candidate.starts_with("/bin/")
        || candidate.starts_with("/usr/bin/")
        || candidate.starts_with("/usr/local/bin/")
}

fn parse_unlabeled_fenced_shell_tool_calls(response: &str) -> Option<(String, Vec<ParsedToolCall>)> {
    let mut text_parts = Vec::new();
    let mut calls = Vec::new();
    let mut last_end = 0usize;

    for capture in UNLABELED_FENCED_BLOCK_RE.captures_iter(response) {
        let Some(full_match) = capture.get(0) else {
            continue;
        };

        let before = &response[last_end..full_match.start()];
        if !before.trim().is_empty() {
            text_parts.push(before.trim().to_string());
        }

        let body = capture.get(1).map(|m| m.as_str()).unwrap_or("");
        let intent_window = before
            .lines()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");

        if SHELL_BLOCK_INTENT_CUE_RE.is_match(intent_window.trim()) {
            if let Some(command) = normalize_recoverable_shell_block(body) {
                if command.lines().all(looks_like_recoverable_shell_line) {
                    calls.push(ParsedToolCall {
                        name: "shell".to_string(),
                        arguments: serde_json::json!({ "command": command }),
                        tool_call_id: None,
                    });
                    last_end = full_match.end();
                    continue;
                }
            }
        }

        text_parts.push(full_match.as_str().trim().to_string());
        last_end = full_match.end();
    }

    if !calls.is_empty() {
        let after = &response[last_end..];
        if !after.trim().is_empty() {
            text_parts.push(after.trim().to_string());
        }
        return Some((text_parts.join("\n"), calls));
    }

    None
}

fn parse_fenced_shell_tool_calls(response: &str) -> Option<(String, Vec<ParsedToolCall>)> {
    let mut text_parts = Vec::new();
    let mut calls = Vec::new();
    let mut last_end = 0usize;

    for capture in FENCED_SHELL_BLOCK_RE.captures_iter(response) {
        let Some(full_match) = capture.get(0) else {
            continue;
        };

        let before = &response[last_end..full_match.start()];
        if !before.trim().is_empty() {
            text_parts.push(before.trim().to_string());
        }

        let body = capture.get(1).map(|m| m.as_str()).unwrap_or("");
        let response_is_only_block = response.trim() == full_match.as_str().trim();
        let intent_window = before
            .lines()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        let should_recover =
            response_is_only_block || SHELL_BLOCK_INTENT_CUE_RE.is_match(intent_window.trim());

        if should_recover {
            if let Some(command) = normalize_recoverable_shell_block(body) {
                calls.push(ParsedToolCall {
                    name: "shell".to_string(),
                    arguments: serde_json::json!({ "command": command }),
                    tool_call_id: None,
                });
            } else {
                text_parts.push(full_match.as_str().trim().to_string());
            }
        } else {
            text_parts.push(full_match.as_str().trim().to_string());
        }

        last_end = full_match.end();
    }

    if !calls.is_empty() {
        let after = &response[last_end..];
        if !after.trim().is_empty() {
            text_parts.push(after.trim().to_string());
        }
        return Some((text_parts.join("\n"), calls));
    }

    None
}

fn parse_bracket_tool_calls(response: &str) -> Option<(String, Vec<ParsedToolCall>)> {
    const TOOL_CALL_MARKER: &str = "[TOOL_CALLS]";
    const ARGS_MARKER: &str = "[ARGS]";

    if !response.contains(TOOL_CALL_MARKER) {
        return None;
    }

    let mut text_parts = Vec::new();
    let mut calls = Vec::new();
    let mut remaining = response;

    while let Some(start) = remaining.find(TOOL_CALL_MARKER) {
        let before = &remaining[..start];
        if !before.trim().is_empty() {
            text_parts.push(before.trim().to_string());
        }

        let after_marker = &remaining[start + TOOL_CALL_MARKER.len()..];
        let trimmed_after_marker = after_marker.trim_start();
        if trimmed_after_marker.is_empty() {
            remaining = trimmed_after_marker;
            continue;
        }
        if let Some(rest) = trimmed_after_marker.strip_prefix("{}") {
            remaining = rest;
            continue;
        }
        let Some(args_pos) = after_marker.find(ARGS_MARKER) else {
            break;
        };

        let tool_raw = after_marker[..args_pos].trim();
        if tool_raw.is_empty() || !tool_raw.chars().all(|c| c.is_alphanumeric() || c == '_') {
            break;
        }

        let tool_name = match tool_raw {
            "browser" | "browser_open" => tool_raw.to_string(),
            _ => map_tool_name_alias(tool_raw).to_string(),
        };
        let args_input = &after_marker[args_pos + ARGS_MARKER.len()..];
        let trimmed_args = args_input.trim_start();
        let leading_ws = args_input.len() - trimmed_args.len();
        let Some((value, consumed_end)) = extract_first_json_value_with_end(trimmed_args) else {
            break;
        };

        let mut arguments_value = value.clone();
        let (extra_consumed, positional_hint) = merge_repeated_bracket_arg_pairs(
            &tool_name,
            &args_input[leading_ws + consumed_end..],
            &mut arguments_value,
        );
        let raw_hint = positional_hint
            .as_deref()
            .or_else(|| raw_string_argument_hint(Some(&value)));
        let arguments = normalize_tool_arguments(
            &tool_name,
            arguments_value,
            raw_hint,
        );
        calls.push(ParsedToolCall {
            name: tool_name,
            arguments,
            tool_call_id: None,
        });

        remaining = &args_input[leading_ws + consumed_end + extra_consumed..];
    }

    if !remaining.trim().is_empty() {
        text_parts.push(remaining.trim().to_string());
    }

    (!calls.is_empty()).then(|| (text_parts.join("\n"), calls))
}

fn merge_repeated_bracket_arg_pairs(
    tool_name: &str,
    rest: &str,
    arguments: &mut serde_json::Value,
) -> (usize, Option<String>) {
    const ARGS_MARKER: &str = "[ARGS]";
    const TOOL_CALL_MARKER: &str = "[TOOL_CALLS]";

    let Some(args_map) = arguments.as_object_mut() else {
        return (0, None);
    };

    let mut consumed_total = 0;
    let mut remaining = rest;
    let mut positional_hint = None;

    loop {
        let trimmed = remaining.trim_start();
        let leading_ws = remaining.len() - trimmed.len();
        if !trimmed.starts_with(ARGS_MARKER) {
            break;
        }

        let after_marker = &trimmed[ARGS_MARKER.len()..];
        let trimmed_after_marker = after_marker.trim_start();
        let marker_ws = after_marker.len() - trimmed_after_marker.len();
        let Some((key_value, key_consumed)) = extract_first_json_value_with_end(trimmed_after_marker)
        else {
            let raw_end = trimmed_after_marker
                .find(TOOL_CALL_MARKER)
                .unwrap_or(trimmed_after_marker.len());
            let raw_value = trimmed_after_marker[..raw_end].trim();
            if raw_value.is_empty() {
                break;
            }

            positional_hint = Some(raw_value.to_string());
            consumed_total += leading_ws + ARGS_MARKER.len() + marker_ws + raw_end;
            break;
        };

        let key_candidate = key_value
            .as_str()
            .map(str::trim)
            .filter(|key| !key.is_empty());

        let after_key = &trimmed_after_marker[key_consumed..];
        let trimmed_after_key = after_key.trim_start();
        let value_ws = after_key.len() - trimmed_after_key.len();
        let value_looks_absent = trimmed_after_key.is_empty()
            || trimmed_after_key.starts_with(ARGS_MARKER)
            || trimmed_after_key.starts_with(TOOL_CALL_MARKER);

        if let Some(key) = key_candidate {
            if value_looks_absent {
                positional_hint = Some(key.to_string());
                consumed_total += leading_ws + ARGS_MARKER.len() + marker_ws + key_consumed;
                break;
            }

            let Some((arg_value, value_consumed)) =
                extract_first_json_value_with_end(trimmed_after_key)
            else {
                positional_hint = Some(key.to_string());
                consumed_total += leading_ws + ARGS_MARKER.len() + marker_ws + key_consumed;
                break;
            };

            args_map.insert(key.to_string(), arg_value);
            consumed_total +=
                leading_ws + ARGS_MARKER.len() + marker_ws + key_consumed + value_ws + value_consumed;
            remaining = &trimmed_after_key[value_consumed..];
            continue;
        }

        let fallback_param = default_param_for_tool(tool_name);
        args_map.insert(fallback_param.to_string(), key_value);
        consumed_total += leading_ws + ARGS_MARKER.len() + marker_ws + key_consumed;
        break;
    }

    (
        consumed_total,
        positional_hint
            .map(|hint| hint.trim().to_string())
            .filter(|hint| !hint.is_empty()),
    )
}

#[cfg(test)]
pub(super) fn normalize_known_workspace_file_path_for_test(raw: &str) -> String {
    normalize_known_workspace_file_path(raw)
}

// ── Tool-Call Parsing ─────────────────────────────────────────────────────
// LLM responses may contain tool calls in multiple formats depending on
// the provider. Parsing follows a priority chain:
//   1. OpenAI-style JSON with `tool_calls` array (native API)
//   2. XML tags: <tool_call>, <toolcall>, <tool-call>, <invoke>
//   3. Markdown code blocks with `tool_call` language
//   4. GLM-style line-based format (e.g. `shell/command>ls`)
// SECURITY: We never fall back to extracting arbitrary JSON from the
// response body, because that would enable prompt-injection attacks where
// malicious content in emails/files/web pages mimics a tool call.

/// Parse tool calls from an LLM response that uses XML-style function calling.
///
/// Expected format (common with system-prompt-guided tool use):
/// ```text
/// <tool_call>
/// {"name": "shell", "arguments": {"command": "ls"}}
/// </tool_call>
/// ```
///
/// Also accepts common tag variants (`<toolcall>`, `<tool-call>`) for model
/// compatibility.
///
/// Also supports JSON with `tool_calls` array from OpenAI-format responses.
pub(crate) fn parse_tool_calls(response: &str) -> (String, Vec<ParsedToolCall>) {
    let mut text_parts = Vec::new();
    let mut calls = Vec::new();
    let mut remaining = response;
    let fenced_or_raw_json = strip_wrapping_json_code_fence(response);

    // First, try to parse as OpenAI-style JSON response with tool_calls array
    // This handles providers like Minimax that return tool_calls in native JSON format
    if let Ok(json_value) = serde_json::from_str::<serde_json::Value>(fenced_or_raw_json.trim()) {
        calls = parse_tool_calls_from_json_value(&json_value);
        if !calls.is_empty() {
            // If we found tool_calls, extract any content field as text.
            // Some providers wrap tool calls under `message` or `choices[*].message`.
            if let Some(content) = extract_tool_text_from_json_value(&json_value) {
                text_parts.push(content);
            }
            return (text_parts.join("\n"), calls);
        }
    }

    if let Some((function_text, function_calls)) = parse_function_style_tool_calls(response) {
        if !function_calls.is_empty() {
            return (function_text, function_calls);
        }
    }

    if let Some((wrapped_text, wrapped_calls)) = parse_json_wrapped_tool_calls(response) {
        if !wrapped_calls.is_empty() {
            return (wrapped_text, wrapped_calls);
        }
    }

    if let Some((block_text, block_calls)) = parse_explicit_tool_block_tool_calls(response) {
        if !block_calls.is_empty() {
            return (block_text, block_calls);
        }
    }

    if let Some((shell_text, shell_calls)) = parse_plain_shell_command_tool_call(response) {
        if !shell_calls.is_empty() {
            return (shell_text, shell_calls);
        }
    }

    if let Some((minimax_text, minimax_calls)) = parse_minimax_invoke_calls(response) {
        if !minimax_calls.is_empty() {
            return (minimax_text, minimax_calls);
        }
    }

    if let Some((bracket_text, bracket_calls)) = parse_bracket_tool_calls(response) {
        if !bracket_calls.is_empty() {
            return (bracket_text, bracket_calls);
        }
    }

    // Fall back to XML-style tool-call tag parsing.
    while let Some((start, open_tag)) = find_first_tag(remaining, &TOOL_CALL_OPEN_TAGS) {
        // Everything before the tag is text
        let before = &remaining[..start];
        if !before.trim().is_empty() {
            text_parts.push(before.trim().to_string());
        }

        let Some(close_tag) = matching_tool_call_close_tag(open_tag) else {
            break;
        };

        let after_open = &remaining[start + open_tag.len()..];
        if let Some(close_idx) = after_open.find(close_tag) {
            let inner = &after_open[..close_idx];
            let mut parsed_any = false;

            // Try JSON format first
            let json_values = extract_json_values(inner);
            for value in json_values {
                let parsed_calls = parse_tool_calls_from_json_value(&value);
                if !parsed_calls.is_empty() {
                    parsed_any = true;
                    calls.extend(parsed_calls);
                }
            }

            // If JSON parsing failed, try XML format (DeepSeek/GLM style)
            if !parsed_any {
                if let Some(xml_calls) = parse_xml_tool_calls(inner) {
                    calls.extend(xml_calls);
                    parsed_any = true;
                }
            }

            if !parsed_any {
                // GLM-style shortened body: `shell>uname -a` or `shell\ncommand: date`
                if let Some(glm_call) = parse_glm_shortened_body(inner) {
                    calls.push(glm_call);
                    parsed_any = true;
                }
            }

            if !parsed_any {
                tracing::warn!(
                    "Malformed <tool_call>: expected tool-call object in tag body (JSON/XML/GLM)"
                );
            }

            remaining = &after_open[close_idx + close_tag.len()..];
        } else {
            // Matching close tag not found — try cross-alias close tags first.
            // Models sometimes mix open/close tag aliases (e.g. <tool_call>...</invoke>).
            let mut resolved = false;
            if let Some((cross_idx, cross_tag)) = find_first_tag(after_open, &TOOL_CALL_CLOSE_TAGS)
            {
                let inner = &after_open[..cross_idx];
                let mut parsed_any = false;

                // Try JSON
                let json_values = extract_json_values(inner);
                for value in json_values {
                    let parsed_calls = parse_tool_calls_from_json_value(&value);
                    if !parsed_calls.is_empty() {
                        parsed_any = true;
                        calls.extend(parsed_calls);
                    }
                }

                // Try XML
                if !parsed_any {
                    if let Some(xml_calls) = parse_xml_tool_calls(inner) {
                        calls.extend(xml_calls);
                        parsed_any = true;
                    }
                }

                // Try GLM shortened body
                if !parsed_any {
                    if let Some(glm_call) = parse_glm_shortened_body(inner) {
                        calls.push(glm_call);
                        parsed_any = true;
                    }
                }

                if parsed_any {
                    remaining = &after_open[cross_idx + cross_tag.len()..];
                    resolved = true;
                }
            }

            if resolved {
                continue;
            }

            // No cross-alias close tag resolved — fall back to JSON recovery
            // from unclosed tags (brace-balancing).
            if let Some(json_end) = find_json_end(after_open) {
                if let Ok(value) =
                    serde_json::from_str::<serde_json::Value>(&after_open[..json_end])
                {
                    let parsed_calls = parse_tool_calls_from_json_value(&value);
                    if !parsed_calls.is_empty() {
                        calls.extend(parsed_calls);
                        remaining = strip_leading_close_tags(&after_open[json_end..]);
                        continue;
                    }
                }
            }

            if let Some((value, consumed_end)) = extract_first_json_value_with_end(after_open) {
                let parsed_calls = parse_tool_calls_from_json_value(&value);
                if !parsed_calls.is_empty() {
                    calls.extend(parsed_calls);
                    remaining = strip_leading_close_tags(&after_open[consumed_end..]);
                    continue;
                }
            }

            // Last resort: try GLM shortened body on everything after the open tag.
            // The model may have emitted `<tool_call>shell>ls` with no close tag at all.
            let glm_input = after_open.trim();
            if let Some(glm_call) = parse_glm_shortened_body(glm_input) {
                calls.push(glm_call);
                remaining = "";
                continue;
            }

            remaining = &remaining[start..];
            break;
        }
    }

    // If XML tags found nothing, try markdown code blocks with tool_call language.
    // Models behind OpenRouter sometimes output ```tool_call ... ``` or hybrid
    // ```tool_call ... </tool_call> instead of structured API calls or XML tags.
    if calls.is_empty() {
        static MD_TOOL_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"(?s)```(?:tool[_-]?call|invoke)\s*\n(.*?)(?:```|</tool[_-]?call>|</toolcall>|</invoke>|</minimax:toolcall>)",
            )
            .unwrap()
        });
        let mut md_text_parts: Vec<String> = Vec::new();
        let mut last_end = 0;

        for cap in MD_TOOL_CALL_RE.captures_iter(response) {
            let full_match = cap.get(0).unwrap();
            let before = &response[last_end..full_match.start()];
            if !before.trim().is_empty() {
                md_text_parts.push(before.trim().to_string());
            }
            let inner = &cap[1];
            let json_values = extract_json_values(inner);
            for value in json_values {
                let parsed_calls = parse_tool_calls_from_json_value(&value);
                calls.extend(parsed_calls);
            }
            last_end = full_match.end();
        }

        if !calls.is_empty() {
            let after = &response[last_end..];
            if !after.trim().is_empty() {
                md_text_parts.push(after.trim().to_string());
            }
            text_parts = md_text_parts;
            remaining = "";
        }
    }

    // Try ```tool <name> format used by some providers (e.g., xAI grok)
    // Example: ```tool file_write\n{"path": "...", "content": "..."}\n```
    if calls.is_empty() {
        static MD_TOOL_NAME_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(?s)```tool\s+(\w+)\s*\n(.*?)(?:```|$)").unwrap());
        let mut md_text_parts: Vec<String> = Vec::new();
        let mut last_end = 0;

        for cap in MD_TOOL_NAME_RE.captures_iter(response) {
            let full_match = cap.get(0).unwrap();
            let before = &response[last_end..full_match.start()];
            if !before.trim().is_empty() {
                md_text_parts.push(before.trim().to_string());
            }
            let tool_name = &cap[1];
            let inner = &cap[2];

            // Try to parse the inner content as JSON arguments
            let json_values = extract_json_values(inner);
            if json_values.is_empty() {
                // Log a warning if we found a tool block but couldn't parse arguments
                tracing::warn!(
                    tool_name = %tool_name,
                    inner = %inner.chars().take(100).collect::<String>(),
                    "Found ```tool <name> block but could not parse JSON arguments"
                );
            } else {
                for value in json_values {
                    let arguments = if value.is_object() {
                        value
                    } else {
                        serde_json::Value::Object(serde_json::Map::new())
                    };
                    calls.push(ParsedToolCall {
                        name: tool_name.to_string(),
                        arguments,
                        tool_call_id: None,
                    });
                }
            }
            last_end = full_match.end();
        }

        if !calls.is_empty() {
            let after = &response[last_end..];
            if !after.trim().is_empty() {
                md_text_parts.push(after.trim().to_string());
            }
            text_parts = md_text_parts;
            remaining = "";
        }
    }

    if calls.is_empty() {
        if let Some((shell_text, shell_calls)) = parse_fenced_shell_tool_calls(remaining) {
            if !shell_calls.is_empty() {
                text_parts = if shell_text.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![shell_text]
                };
                calls = shell_calls;
                remaining = "";
            }
        }
    }

    if calls.is_empty() {
        if let Some((shell_text, shell_calls)) = parse_unlabeled_fenced_shell_tool_calls(remaining)
        {
            if !shell_calls.is_empty() {
                text_parts = if shell_text.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![shell_text]
                };
                calls = shell_calls;
                remaining = "";
            }
        }
    }

    // XML attribute-style tool calls:
    // <minimax:toolcall>
    // <invoke name="shell">
    // <parameter name="command">ls</parameter>
    // </invoke>
    // </minimax:toolcall>
    if calls.is_empty() {
        let xml_calls = parse_xml_attribute_tool_calls(remaining);
        if !xml_calls.is_empty() {
            let mut cleaned_text = remaining.to_string();
            for call in xml_calls {
                calls.push(call);
                // Try to remove the XML from text
                if let Some(start) = cleaned_text.find("<minimax:toolcall>") {
                    if let Some(end) = cleaned_text.find("</minimax:toolcall>") {
                        let end_pos = end + "</minimax:toolcall>".len();
                        if end_pos <= cleaned_text.len() {
                            cleaned_text =
                                format!("{}{}", &cleaned_text[..start], &cleaned_text[end_pos..]);
                        }
                    }
                }
            }
            if !cleaned_text.trim().is_empty() {
                text_parts.push(cleaned_text.trim().to_string());
            }
            remaining = "";
        }
    }

    // Perl/hash-ref style tool calls:
    // TOOL_CALL
    // {tool => "shell", args => {
    //   --command "ls -la"
    //   --description "List current directory contents"
    // }}
    // /TOOL_CALL
    if calls.is_empty() {
        let perl_calls = parse_perl_style_tool_calls(remaining);
        if !perl_calls.is_empty() {
            let mut cleaned_text = remaining.to_string();
            for call in perl_calls {
                calls.push(call);
                // Try to remove the TOOL_CALL block from text
                while let Some(start) = cleaned_text.find("TOOL_CALL") {
                    if let Some(end) = cleaned_text.find("/TOOL_CALL") {
                        let end_pos = end + "/TOOL_CALL".len();
                        if end_pos <= cleaned_text.len() {
                            cleaned_text =
                                format!("{}{}", &cleaned_text[..start], &cleaned_text[end_pos..]);
                        }
                    } else {
                        break;
                    }
                }
            }
            if !cleaned_text.trim().is_empty() {
                text_parts.push(cleaned_text.trim().to_string());
            }
            remaining = "";
        }
    }

    // <FunctionCall>
    // file_read
    // <code>path>/Users/...</code>
    // </FunctionCall>
    if calls.is_empty() {
        let func_calls = parse_function_call_tool_calls(remaining);
        if !func_calls.is_empty() {
            let mut cleaned_text = remaining.to_string();
            for call in func_calls {
                calls.push(call);
                // Try to remove the FunctionCall block from text
                while let Some(start) = cleaned_text.find("<FunctionCall>") {
                    if let Some(end) = cleaned_text.find("</FunctionCall>") {
                        let end_pos = end + "</FunctionCall>".len();
                        if end_pos <= cleaned_text.len() {
                            cleaned_text =
                                format!("{}{}", &cleaned_text[..start], &cleaned_text[end_pos..]);
                        }
                    } else {
                        break;
                    }
                }
            }
            if !cleaned_text.trim().is_empty() {
                text_parts.push(cleaned_text.trim().to_string());
            }
            remaining = "";
        }
    }

    // GLM-style tool calls (browser_open/url>https://..., shell/command>ls, etc.)
    if calls.is_empty() {
        let glm_calls = parse_glm_style_tool_calls(remaining);
        if !glm_calls.is_empty() {
            let mut cleaned_text = remaining.to_string();
            for (name, args, raw) in &glm_calls {
                calls.push(ParsedToolCall {
                    name: name.clone(),
                    arguments: args.clone(),
                    tool_call_id: None,
                });
                if let Some(r) = raw {
                    cleaned_text = cleaned_text.replace(r, "");
                }
            }
            if !cleaned_text.trim().is_empty() {
                text_parts.push(cleaned_text.trim().to_string());
            }
            remaining = "";
        }
    }

    // SECURITY: We do NOT fall back to extracting arbitrary JSON from the response
    // here. That would enable prompt injection attacks where malicious content
    // (e.g., in emails, files, or web pages) could include JSON that mimics a
    // tool call. Tool calls MUST be explicitly wrapped in either:
    // 1. OpenAI-style JSON with a "tool_calls" array
    // 2. LlamaFarm tool-call tags (<tool_call>, <toolcall>, <tool-call>)
    // 3. Markdown code blocks with tool_call/toolcall/tool-call language
    // 4. Explicit GLM line-based call formats (e.g. `shell/command>...`)
    // This ensures only the LLM's intentional tool calls are executed.

    // Remaining text after last tool call
    if !remaining.trim().is_empty() {
        text_parts.push(remaining.trim().to_string());
    }

    (text_parts.join("\n"), calls)
}

pub(super) fn detect_tool_call_parse_issue(
    response: &str,
    parsed_calls: &[ParsedToolCall],
) -> Option<String> {
    if !parsed_calls.is_empty() {
        return None;
    }

    let trimmed = response.trim();
    if trimmed.is_empty() {
        return None;
    }

    let looks_like_tool_payload = trimmed.contains("<tool_call")
        || trimmed.contains("<toolcall")
        || trimmed.contains("<tool-call")
        || trimmed.contains("```tool_call")
        || trimmed.contains("```toolcall")
        || trimmed.contains("```tool-call")
        || trimmed.contains("'''bash")
        || trimmed.contains("'''sh")
        || trimmed.contains("'''shell")
        || trimmed.contains("```bash")
        || trimmed.contains("```sh")
        || trimmed.contains("```shell")
        || trimmed.contains("json{")
        || trimmed.contains("shell(")
        || trimmed.contains("file_read(")
        || trimmed.contains("file_write(")
        || trimmed.starts_with("tool:")
        || trimmed.starts_with("tool =")
        || trimmed.starts_with("name:")
        || trimmed.starts_with("name =")
        || trimmed.starts_with("function:")
        || trimmed.starts_with("function =")
        || trimmed.contains("```tool file_")
        || trimmed.contains("```tool shell")
        || trimmed.contains("```tool web_")
        || trimmed.contains("```tool memory_")
        || trimmed.contains("```tool ") // Generic ```tool <name> pattern
        || trimmed.contains("\"tool_calls\"")
        || trimmed.contains("\"tool\"")
        || trimmed.contains("TOOL_CALL")
        || trimmed.contains("<FunctionCall>");

    if looks_like_tool_payload {
        Some("response resembled a tool-call payload but no valid tool call could be parsed".into())
    } else {
        None
    }
}

pub(super) fn parse_structured_tool_calls(tool_calls: &[ToolCall]) -> Vec<ParsedToolCall> {
    tool_calls
        .iter()
        .map(|call| {
            let name = call.name.clone();
            let parsed = serde_json::from_str::<serde_json::Value>(&call.arguments)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
            ParsedToolCall {
                name: name.clone(),
                arguments: normalize_tool_arguments(&name, parsed, Some(call.arguments.as_str())),
                tool_call_id: Some(call.id.clone()),
            }
        })
        .collect()
}
