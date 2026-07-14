//! In-memory tool-result cache for the autonomous agent loop.
//!
//! Caches the output of **read-only** tools keyed by `(tool_name, sha256(args_json))`
//! with a configurable TTL.  Mutating tools (shell, file_write, git, docker, etc.) are
//! never cached — their results depend on side-effects and must always be re-executed.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let cache = Arc::new(ToolResultCache::new(Duration::from_secs(300)));
//! // In execute_one_tool:
//! if let Some(cached) = cache.get(tool_name, &args_json) {
//!     return Ok(cached);
//! }
//! let result = tool.execute(args).await?;
//! cache.set(tool_name, &args_json, result.clone());
//! ```

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::tools::ToolResult;

/// Read-only tools whose results are safe to cache across calls.
const CACHEABLE_TOOLS: &[&str] = &[
    "file_read",
    "memory_recall",
    "glob_search",
    "content_search",
    "web_fetch",
    "web_search_tool",
    "pdf_read",
    "image_info",
    "ollama_model",
    "hardware_board_info",
    "hardware_memory_map",
    "hardware_memory_read",
];

struct CacheEntry {
    result: CachedResult,
    inserted_at: Instant,
}

/// A cached tool result (cloneable subset of `ToolResult`).
#[derive(Debug, Clone)]
pub struct CachedResult {
    pub output: String,
    pub success: bool,
}

impl From<&ToolResult> for CachedResult {
    fn from(r: &ToolResult) -> Self {
        Self {
            output: r.output.clone(),
            success: r.success,
        }
    }
}

/// Thread-safe in-memory tool-result cache with TTL eviction.
pub struct ToolResultCache {
    ttl: Duration,
    store: Mutex<HashMap<String, CacheEntry>>,
}

impl ToolResultCache {
    /// Create a cache with the given TTL.  A TTL of zero disables caching.
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            store: Mutex::new(HashMap::new()),
        }
    }

    /// Default cache: 5-minute TTL.
    pub fn default_ttl() -> Self {
        Self::new(Duration::from_secs(300))
    }

    /// Return true if `tool_name` is eligible for caching.
    pub fn is_cacheable(tool_name: &str) -> bool {
        CACHEABLE_TOOLS.contains(&tool_name)
    }

    /// Look up a cached result.  Returns `None` if absent, expired, or caching is disabled.
    pub fn get(&self, tool_name: &str, args: &serde_json::Value) -> Option<CachedResult> {
        if self.ttl.is_zero() || !Self::is_cacheable(tool_name) {
            return None;
        }
        let key = cache_key(tool_name, args);
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = store.get(&key) {
            if entry.inserted_at.elapsed() < self.ttl {
                return Some(entry.result.clone());
            }
            // Expired — evict inline.
            store.remove(&key);
        }
        None
    }

    /// Store a tool result in the cache.  No-op if the tool is not cacheable.
    pub fn set(&self, tool_name: &str, args: &serde_json::Value, result: &ToolResult) {
        if self.ttl.is_zero() || !Self::is_cacheable(tool_name) {
            return;
        }
        let key = cache_key(tool_name, args);
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store.insert(
            key,
            CacheEntry {
                result: CachedResult::from(result),
                inserted_at: Instant::now(),
            },
        );
    }

    /// Evict all expired entries.
    pub fn evict_expired(&self) {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store.retain(|_, entry| entry.inserted_at.elapsed() < self.ttl);
    }

    /// Remove all cache entries (e.g. after a mutating tool call that could
    /// invalidate cached file contents).
    pub fn invalidate_all(&self) {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store.clear();
    }

    /// Number of live (non-expired) entries.
    pub fn len(&self) -> usize {
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        store
            .values()
            .filter(|e| now.duration_since(e.inserted_at) < self.ttl)
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn cache_key(tool_name: &str, args: &serde_json::Value) -> String {
    // Canonicalise args to a sorted, compact JSON string so argument order
    // differences in the model's JSON output don't produce different keys.
    let canonical = sorted_json(args);
    let mut hasher = Sha256::new();
    hasher.update(tool_name.as_bytes());
    hasher.update(b":");
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn sorted_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by_key(|(k, _)| *k);
            let inner: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("\"{}\":{}", k, sorted_json(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        serde_json::Value::Array(arr) => {
            let inner: Vec<String> = arr.iter().map(sorted_json).collect();
            format!("[{}]", inner.join(","))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolResult;

    fn hit() -> ToolResult {
        ToolResult {
            output: "cached_output".to_string(),
            success: true,
            error: None,
        }
    }

    #[test]
    fn cacheable_tools_list() {
        assert!(ToolResultCache::is_cacheable("file_read"));
        assert!(ToolResultCache::is_cacheable("memory_recall"));
        assert!(!ToolResultCache::is_cacheable("shell"));
        assert!(!ToolResultCache::is_cacheable("file_write"));
        assert!(!ToolResultCache::is_cacheable("docker"));
    }

    #[test]
    fn get_miss_returns_none() {
        let cache = ToolResultCache::default_ttl();
        let result = cache.get("file_read", &serde_json::json!({"path": "/tmp/x"}));
        assert!(result.is_none());
    }

    #[test]
    fn set_then_get_returns_cached() {
        let cache = ToolResultCache::default_ttl();
        let args = serde_json::json!({"path": "/tmp/x"});
        cache.set("file_read", &args, &hit());
        let result = cache.get("file_read", &args);
        assert!(result.is_some());
        assert_eq!(result.unwrap().output, "cached_output");
    }

    #[test]
    fn non_cacheable_tool_not_stored() {
        let cache = ToolResultCache::default_ttl();
        let args = serde_json::json!({"command": "ls"});
        cache.set("shell", &args, &hit());
        assert!(cache.get("shell", &args).is_none());
    }

    #[test]
    fn zero_ttl_disables_caching() {
        let cache = ToolResultCache::new(Duration::ZERO);
        let args = serde_json::json!({"path": "/tmp/x"});
        cache.set("file_read", &args, &hit());
        assert!(cache.get("file_read", &args).is_none());
    }

    #[test]
    fn arg_order_independent() {
        let cache = ToolResultCache::default_ttl();
        let args1 = serde_json::json!({"a": 1, "b": 2});
        let args2 = serde_json::json!({"b": 2, "a": 1});
        cache.set("file_read", &args1, &hit());
        // Same canonical JSON — should be a cache hit.
        assert!(cache.get("file_read", &args2).is_some());
    }

    #[test]
    fn expired_entry_returns_none() {
        let cache = ToolResultCache::new(Duration::from_nanos(1));
        let args = serde_json::json!({"path": "/tmp/x"});
        cache.set("file_read", &args, &hit());
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get("file_read", &args).is_none());
    }

    #[test]
    fn invalidate_all_clears_cache() {
        let cache = ToolResultCache::default_ttl();
        let args = serde_json::json!({"path": "/tmp/x"});
        cache.set("file_read", &args, &hit());
        cache.invalidate_all();
        assert!(cache.get("file_read", &args).is_none());
    }
}
