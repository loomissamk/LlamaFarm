//! General-purpose document RAG pipeline.
//!
//! Supports the full pipeline for any text corpus:
//!
//! ```text
//! Ingest → Normalize → Chunk → Embed → Index
//!                                         ↓
//! Answer ← Context Pack ← Rerank ← Hybrid Retrieve
//!            ↑ citations                  ↑
//!                              BM25 + Vector merge
//! ```
//!
//! ## Supported corpora
//!
//! - Markdown (`.md`)
//! - Plain text (`.txt`, `.log`, `.conf`, etc.)
//! - PDF (`.pdf`) — requires `rag-pdf` feature
//! - Git repos (walk all tracked files)
//! - Saved outputs and run history (any text file)
//!
//! ## Usage
//!
//! ```rust,ignore
//! let mut rag = DocRag::new();
//! rag.ingest_directory("/path/to/repo").await?;
//! let results = rag.retrieve("how does authentication work", 5);
//! let context = rag.build_context(&results);
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

// ── Core types ───────────────────────────────────────────────────

/// A single indexed document chunk with source metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocChunk {
    /// Unique chunk ID (deterministic: `<source_hash>-<index>`).
    pub id: String,
    /// Original file path (absolute or relative to corpus root).
    pub source: String,
    /// Optional section heading extracted from the document.
    pub heading: Option<String>,
    /// Chunk index within the source document.
    pub chunk_index: usize,
    /// The chunk text content.
    pub content: String,
    /// Pre-computed embedding vector (`None` until `embed_all()` is called).
    #[serde(skip)]
    pub embedding: Option<Vec<f32>>,
    /// BM25 term-frequency map for this chunk (built during indexing).
    #[serde(skip)]
    pub tf: HashMap<String, f32>,
}

impl DocChunk {
    /// Build a deterministic chunk ID from source path and chunk index.
    fn make_id(source: &str, index: usize) -> String {
        let hash = {
            let mut h: u64 = 0xcbf29ce484222325; // FNV offset basis
            for b in source.bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x00000100000001b3); // FNV prime
            }
            h
        };
        format!("{hash:016x}-{index:04}")
    }
}

/// A retrieval result with relevance score and citation metadata.
#[derive(Debug, Clone)]
pub struct RetrievalResult {
    pub chunk: DocChunk,
    /// Combined hybrid relevance score (higher = more relevant).
    pub score: f32,
    /// Source of score: `"bm25"`, `"vector"`, or `"hybrid"`.
    pub score_type: String,
}

// ── Tokenization / BM25 helpers ───────────────────────────────────

/// Tokenize text into lowercase words, stripping punctuation.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty() && t.len() > 1)
        .map(|t| t.to_lowercase())
        .collect()
}

/// Jaccard similarity between two token sets (|∩| / |∪|), for MMR diversity.
fn jaccard(
    a: &std::collections::HashSet<String>,
    b: &std::collections::HashSet<String>,
) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Compute term-frequency map for a list of tokens.
fn compute_tf(tokens: &[String]) -> HashMap<String, f32> {
    let mut tf = HashMap::new();
    for token in tokens {
        *tf.entry(token.clone()).or_insert(0.0) += 1.0;
    }
    let total = tokens.len() as f32;
    if total > 0.0 {
        for v in tf.values_mut() {
            *v /= total;
        }
    }
    tf
}

/// BM25 score for a query term against a chunk.
///
/// Uses standard BM25 parameters: k1=1.5, b=0.75.
fn bm25_score(
    query_tokens: &[String],
    chunk_tf: &HashMap<String, f32>,
    chunk_len: usize,
    avg_doc_len: f32,
    idf: &HashMap<String, f32>,
) -> f32 {
    const K1: f32 = 1.5;
    const B: f32 = 0.75;

    let mut score = 0.0f32;
    let dl = chunk_len as f32;

    for token in query_tokens {
        let tf = *chunk_tf.get(token).unwrap_or(&0.0);
        let idf_val = *idf.get(token).unwrap_or(&0.0);
        let numerator = tf * (K1 + 1.0);
        let denominator = tf + K1 * (1.0 - B + B * dl / avg_doc_len);
        score += idf_val * numerator / denominator;
    }

    score
}

/// Cosine similarity between two equal-length vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na * nb)
}

// ── DocRag ───────────────────────────────────────────────────────

/// General-purpose document RAG index.
///
/// Holds all ingested chunks in memory with BM25 and optional vector indices.
/// For large corpora (> 10k chunks) consider a persistent backend.
#[derive(Default)]
pub struct DocRag {
    chunks: Vec<DocChunk>,
    /// Inverted document frequency per token (rebuilt on every ingest/embed).
    idf: HashMap<String, f32>,
    /// Average document length (tokens) for BM25 normalization.
    avg_doc_len: f32,
}

impl DocRag {
    /// Create an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the number of indexed chunks.
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    // ── Ingestion ─────────────────────────────────────────────────

    /// Ingest a single string with a given source label (e.g. file path).
    ///
    /// Chunks the text semantically (markdown headings) and adds to the index.
    pub fn ingest_text(&mut self, source: &str, text: &str) {
        let raw_chunks = crate::memory::chunker::chunk_markdown(text, 512);
        for rc in raw_chunks {
            let content = rc.content.trim().to_string();
            if content.is_empty() {
                continue;
            }
            let tokens = tokenize(&content);
            let tf = compute_tf(&tokens);
            let id = DocChunk::make_id(source, rc.index);
            self.chunks.push(DocChunk {
                id,
                source: source.to_string(),
                heading: rc.heading.map(|h| h.to_string()),
                chunk_index: rc.index,
                content,
                embedding: None,
                tf,
            });
        }
        self.rebuild_idf();
        debug!("doc_rag: ingested '{}' → {} total chunks", source, self.chunks.len());
    }

    /// Ingest a file from disk. Reads text content and delegates to `ingest_text`.
    ///
    /// Supports: `.md`, `.txt`, `.log`, `.rs`, `.py`, `.js`, `.ts`, `.go`,
    /// `.toml`, `.yaml`, `.json`, `.conf`, `.sh` and any UTF-8 text file.
    pub fn ingest_file(&mut self, path: &Path) -> anyhow::Result<()> {
        let content = std::fs::read_to_string(path)?;
        let source = path.to_string_lossy().to_string();
        self.ingest_text(&source, &content);
        Ok(())
    }

    /// Recursively ingest all text files in a directory (up to `max_files`).
    ///
    /// Skips hidden directories, `target/`, `node_modules/`, `.git/`, and
    /// non-UTF-8 files.
    pub fn ingest_directory(
        &mut self,
        dir: &Path,
        max_files: usize,
    ) -> anyhow::Result<usize> {
        let mut count = 0;
        self.ingest_directory_inner(dir, &mut count, max_files)?;
        if count > 0 {
            info!("doc_rag: ingested {} files from {}", count, dir.display());
        }
        Ok(count)
    }

    fn ingest_directory_inner(
        &mut self,
        dir: &Path,
        count: &mut usize,
        max_files: usize,
    ) -> anyhow::Result<()> {
        if *count >= max_files {
            return Ok(());
        }

        let skip_dirs = [
            "target", "node_modules", ".git", ".svn", "__pycache__",
            "dist", "build", ".next", ".cargo",
        ];

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                warn!("doc_rag: cannot read {}: {e}", dir.display());
                return Ok(());
            }
        };

        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .collect();
        paths.sort();

        for path in paths {
            if *count >= max_files {
                break;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");

            if name.starts_with('.') {
                continue;
            }

            if path.is_dir() {
                if skip_dirs.contains(&name) {
                    continue;
                }
                self.ingest_directory_inner(&path, count, max_files)?;
                continue;
            }

            if is_text_file(&path) {
                if let Err(e) = self.ingest_file(&path) {
                    warn!("doc_rag: skip {}: {e}", path.display());
                } else {
                    *count += 1;
                }
            }
        }
        Ok(())
    }

    // ── Embedding ─────────────────────────────────────────────────

    /// Store a pre-computed embedding for a chunk identified by `chunk_id`.
    pub fn set_embedding(&mut self, chunk_id: &str, embedding: Vec<f32>) {
        if let Some(chunk) = self.chunks.iter_mut().find(|c| c.id == chunk_id) {
            chunk.embedding = Some(embedding);
        }
    }

    /// Return all chunks that have not yet been embedded (for batch embedding).
    pub fn unembedded_chunks(&self) -> Vec<&DocChunk> {
        self.chunks
            .iter()
            .filter(|c| c.embedding.is_none())
            .collect()
    }

    // ── Retrieval ─────────────────────────────────────────────────

    /// BM25 keyword retrieval. Returns top-`limit` chunks by BM25 score.
    pub fn retrieve_bm25(&self, query: &str, limit: usize) -> Vec<RetrievalResult> {
        let query_tokens = tokenize(query);
        if query_tokens.is_empty() || self.chunks.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<(f32, usize)> = self
            .chunks
            .iter()
            .enumerate()
            .map(|(i, chunk)| {
                let score = bm25_score(
                    &query_tokens,
                    &chunk.tf,
                    chunk.tf.len(),
                    self.avg_doc_len,
                    &self.idf,
                );
                (score, i)
            })
            .filter(|(s, _)| *s > 0.0)
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        scored
            .into_iter()
            .map(|(score, i)| RetrievalResult {
                chunk: self.chunks[i].clone(),
                score,
                score_type: "bm25".to_string(),
            })
            .collect()
    }

    /// Vector similarity retrieval. Returns top-`limit` chunks by cosine similarity.
    ///
    /// Returns empty if no chunks have embeddings yet.
    pub fn retrieve_vector(&self, query_embedding: &[f32], limit: usize) -> Vec<RetrievalResult> {
        if query_embedding.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<(f32, usize)> = self
            .chunks
            .iter()
            .enumerate()
            .filter_map(|(i, chunk)| {
                chunk
                    .embedding
                    .as_deref()
                    .map(|emb| (cosine_similarity(query_embedding, emb), i))
            })
            .filter(|(s, _)| *s > 0.0)
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        scored
            .into_iter()
            .map(|(score, i)| RetrievalResult {
                chunk: self.chunks[i].clone(),
                score,
                score_type: "vector".to_string(),
            })
            .collect()
    }

    /// Hybrid retrieval: merge BM25 + vector results using Reciprocal Rank Fusion.
    ///
    /// `rrf_k` is the RRF constant (default 60). Returns top-`limit` chunks.
    pub fn retrieve_hybrid(
        &self,
        query: &str,
        query_embedding: Option<&[f32]>,
        limit: usize,
    ) -> Vec<RetrievalResult> {
        const RRF_K: f32 = 60.0;
        let fetch = (limit * 3).max(20);

        let bm25_results = self.retrieve_bm25(query, fetch);
        let vector_results = query_embedding
            .map(|emb| self.retrieve_vector(emb, fetch))
            .unwrap_or_default();

        // Build RRF score map keyed by chunk ID
        let mut rrf_scores: HashMap<String, f32> = HashMap::new();

        for (rank, result) in bm25_results.iter().enumerate() {
            let rrf = 1.0 / (RRF_K + rank as f32 + 1.0);
            *rrf_scores.entry(result.chunk.id.clone()).or_insert(0.0) += rrf;
        }
        for (rank, result) in vector_results.iter().enumerate() {
            let rrf = 1.0 / (RRF_K + rank as f32 + 1.0);
            *rrf_scores.entry(result.chunk.id.clone()).or_insert(0.0) += rrf;
        }

        // Build result list from all candidates
        let mut all_results: Vec<RetrievalResult> = bm25_results
            .into_iter()
            .chain(vector_results)
            .collect::<Vec<_>>();

        // Deduplicate by chunk ID, keeping highest RRF score
        let mut seen = std::collections::HashSet::new();
        all_results.retain(|r| seen.insert(r.chunk.id.clone()));

        // Sort by RRF score
        all_results.sort_by(|a, b| {
            let sa = rrf_scores.get(&a.chunk.id).copied().unwrap_or(0.0);
            let sb = rrf_scores.get(&b.chunk.id).copied().unwrap_or(0.0);
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });

        all_results.truncate(limit);

        // Update score and type for returned results
        for result in &mut all_results {
            if let Some(s) = rrf_scores.get(&result.chunk.id) {
                result.score = *s;
                result.score_type = "hybrid".to_string();
            }
        }

        all_results
    }

    // ── Context packing ───────────────────────────────────────────

    /// Build a context string from retrieval results with source citations.
    ///
    /// Each chunk is wrapped with `[Source: <path> | Chunk <n>]` headers so the
    /// model can cite sources in its answer.
    pub fn build_context(results: &[RetrievalResult], max_chars: usize) -> String {
        let mut out = String::new();
        let mut total = 0usize;

        for (i, result) in results.iter().enumerate() {
            let heading = result
                .chunk
                .heading
                .as_deref()
                .map(|h| format!(" § {h}"))
                .unwrap_or_default();
            let header = format!(
                "[Source {n}: {src}{heading}]\n",
                n = i + 1,
                src = result.chunk.source,
            );
            let body = &result.chunk.content;
            let entry = format!("{header}{body}\n\n");

            if total + entry.len() > max_chars {
                break;
            }
            out.push_str(&entry);
            total += entry.len();
        }

        out
    }

    /// Build a compact citation list suitable for appending to an answer.
    pub fn build_citation_list(results: &[RetrievalResult]) -> String {
        let mut out = String::from("Sources:\n");
        for (i, r) in results.iter().enumerate() {
            out.push_str(&format!("  [{}] {}\n", i + 1, r.chunk.source));
        }
        out
    }

    /// Rerank results with lexical Maximal Marginal Relevance to trade a little
    /// pure relevance for diversity, so the returned passages don't repeat the
    /// same content. `lambda` in [0,1]: 1.0 = relevance only (no-op), lower =
    /// more diversity. Similarity is token-set Jaccard between chunk contents.
    /// Deterministic and model-free.
    pub fn rerank_mmr(results: Vec<RetrievalResult>, lambda: f32, top_k: usize) -> Vec<RetrievalResult> {
        if results.len() <= 1 {
            return results;
        }
        let lambda = lambda.clamp(0.0, 1.0);
        let token_sets: Vec<std::collections::HashSet<String>> = results
            .iter()
            .map(|r| tokenize(&r.chunk.content).into_iter().collect())
            .collect();

        let max_score = results
            .iter()
            .map(|r| r.score)
            .fold(f32::MIN, f32::max)
            .max(1e-6);

        let mut remaining: Vec<usize> = (0..results.len()).collect();
        let mut selected: Vec<usize> = Vec::new();
        let target = top_k.min(results.len());

        while selected.len() < target && !remaining.is_empty() {
            let mut best_pos = 0usize;
            let mut best_mmr = f32::MIN;
            for (pos, &idx) in remaining.iter().enumerate() {
                let relevance = results[idx].score / max_score;
                let max_sim = selected
                    .iter()
                    .map(|&s| jaccard(&token_sets[idx], &token_sets[s]))
                    .fold(0.0_f32, f32::max);
                let mmr = lambda * relevance - (1.0 - lambda) * max_sim;
                if mmr > best_mmr {
                    best_mmr = mmr;
                    best_pos = pos;
                }
            }
            selected.push(remaining.remove(best_pos));
        }

        let mut by_index: Vec<Option<RetrievalResult>> = results.into_iter().map(Some).collect();
        selected
            .into_iter()
            .filter_map(|i| by_index[i].take())
            .collect()
    }

    // ── Internal ──────────────────────────────────────────────────

    /// Rebuild the IDF table and average document length from current chunks.
    fn rebuild_idf(&mut self) {
        let n = self.chunks.len() as f32;
        if n == 0.0 {
            self.idf.clear();
            self.avg_doc_len = 0.0;
            return;
        }

        // Document frequency per term
        let mut df: HashMap<String, f32> = HashMap::new();
        let mut total_len = 0usize;

        for chunk in &self.chunks {
            total_len += chunk.tf.len();
            for term in chunk.tf.keys() {
                *df.entry(term.clone()).or_insert(0.0) += 1.0;
            }
        }

        self.avg_doc_len = total_len as f32 / n;

        // BM25 IDF: log((N - df + 0.5) / (df + 0.5) + 1)
        self.idf = df
            .into_iter()
            .map(|(term, d)| {
                let idf = ((n - d + 0.5) / (d + 0.5) + 1.0).ln();
                (term, idf)
            })
            .collect();
    }
}

// ── File type detection ────────────────────────────────────────────

/// Return `true` if the file is likely UTF-8 text worth indexing.
fn is_text_file(path: &Path) -> bool {
    let text_extensions = [
        "md", "txt", "rst", "log", "conf", "cfg", "ini", "env",
        "toml", "yaml", "yml", "json", "jsonl",
        "rs", "py", "js", "ts", "jsx", "tsx", "go", "java", "c", "cpp", "h",
        "sh", "bash", "zsh", "fish",
        "html", "css", "sql", "graphql",
        "Makefile", "Dockerfile",
        "AGENTS", "README", "LICENSE", "CHANGELOG", "ARCHITECTURE",
    ];

    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        return text_extensions.contains(&ext);
    }

    // No extension — check if the filename itself matches (e.g. Makefile)
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        return text_extensions.contains(&name);
    }

    false
}

use tracing::info;

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn result(source: &str, content: &str, score: f32) -> RetrievalResult {
        RetrievalResult {
            chunk: DocChunk {
                id: source.to_string(),
                source: source.to_string(),
                heading: None,
                chunk_index: 0,
                content: content.to_string(),
                embedding: None,
                tf: HashMap::new(),
            },
            score,
            score_type: "test".into(),
        }
    }

    #[test]
    fn mmr_rerank_demotes_near_duplicate() {
        // Two near-identical top hits and one distinct hit. MMR should keep
        // the best duplicate and promote the distinct passage over the second
        // duplicate for the second slot.
        let results = vec![
            result("a", "the cat sat on the warm mat by the fire", 1.0),
            result("b", "the cat sat on the warm mat by the fire today", 0.95),
            result("c", "quantum entanglement links distant particles", 0.6),
        ];
        let reranked = DocRag::rerank_mmr(results, 0.6, 2);
        assert_eq!(reranked.len(), 2);
        assert_eq!(reranked[0].chunk.source, "a", "most relevant stays first");
        assert_eq!(
            reranked[1].chunk.source, "c",
            "distinct passage beats near-duplicate for diversity"
        );
    }

    #[test]
    fn mmr_lambda_one_preserves_relevance_order() {
        let results = vec![
            result("a", "alpha", 0.9),
            result("b", "beta", 0.8),
            result("c", "gamma", 0.7),
        ];
        let reranked = DocRag::rerank_mmr(results, 1.0, 3);
        let order: Vec<&str> = reranked.iter().map(|r| r.chunk.source.as_str()).collect();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn ingest_and_bm25_retrieval() {
        let mut rag = DocRag::new();
        rag.ingest_text(
            "auth.md",
            "# Authentication\n\nThe auth system uses JWT tokens. Tokens expire after 24 hours.",
        );
        rag.ingest_text(
            "deploy.md",
            "# Deployment\n\nDeploy using Docker Compose. Run `docker-compose up -d`.",
        );

        let results = rag.retrieve_bm25("JWT token authentication", 3);
        assert!(!results.is_empty());
        assert_eq!(results[0].chunk.source, "auth.md");
    }

    #[test]
    fn hybrid_retrieval_without_embeddings_falls_back_to_bm25() {
        let mut rag = DocRag::new();
        rag.ingest_text("readme.md", "# Setup\n\nInstall with cargo. Run `cargo build`.");

        let results = rag.retrieve_hybrid("cargo build", None, 3);
        assert!(!results.is_empty());
    }

    #[test]
    fn vector_retrieval_with_embeddings() {
        let mut rag = DocRag::new();
        rag.ingest_text(
            "src/main.rs",
            "fn main() { println!(\"Hello, world!\"); }",
        );

        // Plant a fake embedding
        let chunk_id = rag.chunks[0].id.clone();
        rag.set_embedding(&chunk_id, vec![1.0, 0.0, 0.0]);

        let results = rag.retrieve_vector(&[0.9, 0.1, 0.0], 3);
        assert!(!results.is_empty());
        assert!(results[0].score > 0.8);
    }

    #[test]
    fn context_has_citations() {
        let mut rag = DocRag::new();
        rag.ingest_text("docs/api.md", "# API\n\nThe API uses REST over HTTPS.");
        let results = rag.retrieve_bm25("API REST", 3);
        let ctx = DocRag::build_context(&results, 10000);
        assert!(ctx.contains("[Source 1:"));
        assert!(ctx.contains("docs/api.md"));
    }

    #[test]
    fn build_citation_list() {
        let mut rag = DocRag::new();
        rag.ingest_text("a.md", "# A\n\nSome content.");
        rag.ingest_text("b.md", "# B\n\nOther content.");
        let results = rag.retrieve_bm25("content", 5);
        let citations = DocRag::build_citation_list(&results);
        assert!(citations.contains("Sources:"));
    }

    #[test]
    fn empty_query_returns_empty() {
        let mut rag = DocRag::new();
        rag.ingest_text("x.md", "Something");
        assert!(rag.retrieve_bm25("", 5).is_empty());
    }

    #[test]
    fn is_text_file_detects_known_extensions() {
        assert!(is_text_file(Path::new("foo.rs")));
        assert!(is_text_file(Path::new("README.md")));
        assert!(is_text_file(Path::new("Makefile")));
        assert!(!is_text_file(Path::new("image.png")));
        assert!(!is_text_file(Path::new("binary.exe")));
    }

    #[test]
    fn rrf_deduplicates_overlap() {
        let mut rag = DocRag::new();
        rag.ingest_text("shared.md", "# Topic\n\nContent about the topic.");

        // Both BM25 and fake vector should both retrieve the same chunk
        let chunk_id = rag.chunks[0].id.clone();
        rag.set_embedding(&chunk_id, vec![1.0]);

        let results = rag.retrieve_hybrid("topic content", Some(&[1.0]), 5);
        // Deduplication: same chunk should appear only once
        let ids: Vec<_> = results.iter().map(|r| r.chunk.id.clone()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len());
    }
}
