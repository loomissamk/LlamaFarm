//! Workspace RAG inbox tool.
//!
//! Gives the agent lexical retrieval with exact source citations over the
//! operator's document drop zone at `<workspace>/rag/inbox/`. Any text file
//! placed there (by the operator, the Files page, or the agent itself) is
//! indexed automatically: the index is rebuilt lazily whenever a call
//! observes that the inbox changed (file added, removed, or modified), so no
//! background daemon is required and deleted documents drop out of results.

use super::traits::{Tool, ToolResult};
use crate::rag::doc_rag::DocRag;
use async_trait::async_trait;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

const MAX_INBOX_FILES: usize = 500;
const DEFAULT_RESULT_LIMIT: usize = 5;
const MAX_RESULT_LIMIT: usize = 20;
const MAX_CONTEXT_CHARS: usize = 8_000;

/// Fingerprint of the inbox contents used to detect staleness.
type InboxFingerprint = BTreeMap<PathBuf, (u64, SystemTime)>;

struct IndexState {
    rag: DocRag,
    fingerprint: InboxFingerprint,
    /// Embedding cache keyed by chunk content hash, so a reindex (triggered
    /// by any inbox change) does not re-embed chunks whose text is unchanged.
    embed_cache: std::collections::HashMap<u64, Vec<f32>>,
}

fn content_hash(content: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in content.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x00000100000001b3);
    }
    h
}

pub struct WorkspaceRagTool {
    inbox_dir: PathBuf,
    state: Mutex<IndexState>,
    /// Optional local embedder enabling vector+BM25 hybrid retrieval.
    embedder: Option<Arc<dyn crate::memory::embeddings::EmbeddingProvider>>,
}

impl WorkspaceRagTool {
    pub fn new(workspace_dir: &Path) -> Self {
        Self {
            inbox_dir: workspace_dir.join("rag").join("inbox"),
            state: Mutex::new(IndexState {
                rag: DocRag::new(),
                fingerprint: InboxFingerprint::new(),
                embed_cache: std::collections::HashMap::new(),
            }),
            embedder: None,
        }
    }

    /// Enable semantic retrieval: chunks are embedded on index rebuild and
    /// queries use hybrid RRF fusion of BM25 + vector similarity.
    pub fn with_embedder(
        mut self,
        embedder: Arc<dyn crate::memory::embeddings::EmbeddingProvider>,
    ) -> Self {
        self.embedder = Some(embedder);
        self
    }

    fn scan_fingerprint(&self) -> InboxFingerprint {
        let mut fp = InboxFingerprint::new();
        let mut stack = vec![self.inbox_dir.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'))
                {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    stack.push(path);
                } else if fp.len() < MAX_INBOX_FILES {
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    fp.insert(path, (meta.len(), mtime));
                }
            }
        }
        fp
    }

    /// Rebuild the index when the inbox changed since the last call.
    /// Returns (documents indexed, chunks indexed).
    fn refresh(&self, state: &mut IndexState) -> (usize, usize) {
        let current = self.scan_fingerprint();
        if current != state.fingerprint {
            let mut rag = DocRag::new();
            for path in current.keys() {
                // Non-UTF-8 or unreadable files are skipped, matching
                // ingest_directory semantics.
                let _ = rag.ingest_file(path);
            }
            // Restore embeddings for chunks whose content is unchanged, so a
            // reindex only re-embeds genuinely new/edited text.
            if self.embedder.is_some() {
                let restore: Vec<(String, Vec<f32>)> = rag
                    .unembedded_chunks()
                    .iter()
                    .filter_map(|c| {
                        state
                            .embed_cache
                            .get(&content_hash(&c.content))
                            .map(|v| (c.id.clone(), v.clone()))
                    })
                    .collect();
                for (id, vector) in restore {
                    rag.set_embedding(&id, vector);
                }
            }
            state.rag = rag;
            state.fingerprint = current;
        }
        (state.fingerprint.len(), state.rag.len())
    }
}

#[async_trait]
impl Tool for WorkspaceRagTool {
    fn name(&self) -> &str {
        "workspace_rag"
    }

    fn description(&self) -> &str {
        "Search the operator's document inbox (<workspace>/rag/inbox/) with \
         BM25 lexical retrieval and exact source citations. Drop any text, \
         markdown, code, or log file into that directory and it is indexed \
         automatically on the next call. Actions: 'search' {query, limit?} \
         returns cited passages; 'status' reports indexed documents/chunks. \
         Use this before answering questions about operator-provided documents."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["search", "status"],
                    "description": "search: retrieve cited passages; status: index summary"
                },
                "query": {
                    "type": "string",
                    "description": "Search query (required for action=search)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max passages to return (default 5, max 20)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("search");

        // Refresh under a tightly scoped lock: the guard must never live
        // across an await (embedding happens between lock scopes below).
        let (docs, chunks, pending) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let (docs, chunks) = self.refresh(&mut state);
            let pending: Vec<(String, String)> = if self.embedder.is_some() {
                state
                    .rag
                    .unembedded_chunks()
                    .iter()
                    .map(|c| (c.id.clone(), c.content.clone()))
                    .collect()
            } else {
                Vec::new()
            };
            (docs, chunks, pending)
        };

        match action {
            "status" => Ok(ToolResult {
                success: true,
                output: format!(
                    "Workspace RAG inbox: {} at {}\n{} document(s), {} chunk(s) indexed. \
                     Drop text files into the inbox directory to index them.",
                    if docs == 0 { "empty" } else { "ready" },
                    self.inbox_dir.display(),
                    docs,
                    chunks,
                ),
                error: None,
            }),
            "search" => {
                let Some(query) = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|q| !q.is_empty())
                else {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Parameter 'query' is required for action=search".into()),
                    });
                };
                if docs == 0 {
                    return Ok(ToolResult {
                        success: true,
                        output: format!(
                            "No documents indexed. The inbox at {} is empty — \
                             ask the operator to drop files there, or write one \
                             with file_write.",
                            self.inbox_dir.display()
                        ),
                        error: None,
                    });
                }
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize)
                    .unwrap_or(DEFAULT_RESULT_LIMIT)
                    .clamp(1, MAX_RESULT_LIMIT);

                // Semantic lane: embed any new chunks plus the query with no
                // lock held, then fuse BM25 + vector ranks under a fresh
                // lock. Best-effort — embedding failures fall back to BM25.
                let mut embedded: Vec<(String, u64, Vec<f32>)> = Vec::new();
                let mut query_embedding: Option<Vec<f32>> = None;
                if let Some(embedder) = &self.embedder {
                    for (id, content) in &pending {
                        if let Ok(vector) = embedder.embed_one(content).await {
                            embedded.push((id.clone(), content_hash(content), vector));
                        }
                    }
                    query_embedding = embedder.embed_one(query).await.ok();
                }

                let (output, matched) = {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    for (id, hash, vector) in embedded {
                        state.embed_cache.insert(hash, vector.clone());
                        state.rag.set_embedding(&id, vector);
                    }
                    // Over-fetch, then MMR-rerank down to `limit` so the
                    // returned passages are relevant AND non-redundant.
                    let fetched = state.rag.retrieve_hybrid(
                        query,
                        query_embedding.as_deref(),
                        (limit * 3).max(limit),
                    );
                    let results = DocRag::rerank_mmr(fetched, 0.7, limit);
                    if results.is_empty() {
                        (String::new(), false)
                    } else {
                        let context = DocRag::build_context(&results, MAX_CONTEXT_CHARS);
                        let citations = DocRag::build_citation_list(&results);
                        (format!("{context}{citations}"), true)
                    }
                };

                if !matched {
                    return Ok(ToolResult {
                        success: true,
                        output: format!(
                            "No passages matched '{query}' across {docs} document(s)."
                        ),
                        error: None,
                    });
                }
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action '{other}'. Valid: search, status")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tool_with_inbox() -> (WorkspaceRagTool, TempDir) {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("rag").join("inbox")).unwrap();
        (WorkspaceRagTool::new(tmp.path()), tmp)
    }

    /// Embedder that counts how many texts it embedded, for cache tests.
    struct CountingEmbedder {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl crate::memory::embeddings::EmbeddingProvider for CountingEmbedder {
        fn name(&self) -> &str {
            "counting"
        }
        fn dimensions(&self) -> usize {
            3
        }
        async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.calls
                .fetch_add(texts.len(), std::sync::atomic::Ordering::SeqCst);
            Ok(texts
                .iter()
                .map(|t| {
                    let h = content_hash(t) as f32;
                    vec![h % 7.0, h % 13.0, h % 3.0]
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn embedding_cache_avoids_reembedding_unchanged_chunks() {
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("rag").join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(inbox.join("a.md"), "alpha content about widgets").unwrap();

        let counter = Arc::new(CountingEmbedder {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let tool = WorkspaceRagTool::new(tmp.path()).with_embedder(counter.clone());

        // First search embeds the one chunk plus the query.
        tool.execute(json!({"action": "search", "query": "widgets"}))
            .await
            .unwrap();
        let after_first = counter.calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!(after_first >= 2, "chunk + query embedded: {after_first}");

        // Add a second file; the first chunk's embedding must be reused from
        // cache after the reindex, so only the new chunk (+query) is embedded.
        std::fs::write(inbox.join("b.md"), "beta content about gadgets").unwrap();
        let before = counter.calls.load(std::sync::atomic::Ordering::SeqCst);
        tool.execute(json!({"action": "search", "query": "gadgets"}))
            .await
            .unwrap();
        let delta = counter.calls.load(std::sync::atomic::Ordering::SeqCst) - before;
        // 1 new chunk + 1 query = 2; the unchanged chunk is NOT re-embedded.
        assert_eq!(delta, 2, "only new chunk + query embedded, got {delta}");
    }

    #[tokio::test]
    async fn empty_inbox_reports_status_and_search() {
        let (tool, _tmp) = tool_with_inbox();
        let status = tool.execute(json!({"action": "status"})).await.unwrap();
        assert!(status.success);
        assert!(status.output.contains("0 document(s)"));
        let search = tool
            .execute(json!({"action": "search", "query": "anything"}))
            .await
            .unwrap();
        assert!(search.success);
        assert!(search.output.contains("No documents indexed"));
    }

    #[tokio::test]
    async fn dropped_file_is_indexed_and_cited_then_removed() {
        let (tool, tmp) = tool_with_inbox();
        let inbox = tmp.path().join("rag").join("inbox");
        std::fs::write(
            inbox.join("runbook.md"),
            "# Restore runbook\nThe gpu box restore passphrase hint lives in the blue notebook.",
        )
        .unwrap();

        let result = tool
            .execute(json!({"action": "search", "query": "restore passphrase hint"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("blue notebook"), "{}", result.output);
        assert!(result.output.contains("Sources:"));
        assert!(result.output.contains("runbook.md"));

        // Deleting the file drops it from the index on the next call.
        std::fs::remove_file(inbox.join("runbook.md")).unwrap();
        let status = tool.execute(json!({"action": "status"})).await.unwrap();
        assert!(status.output.contains("0 document(s)"), "{}", status.output);
    }

    #[tokio::test]
    async fn missing_query_is_an_error() {
        let (tool, _tmp) = tool_with_inbox();
        let result = tool.execute(json!({"action": "search"})).await.unwrap();
        assert!(!result.success);
    }
}
