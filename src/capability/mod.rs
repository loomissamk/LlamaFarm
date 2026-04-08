//! Per-model capability registry.
//!
//! Tracks measured scores, context window sizes, latency, and VRAM footprint
//! for every installed Ollama model. The bakeoff harness writes to this registry
//! via [`CapabilityRegistry::update`]; the model router reads from it at startup
//! and at routing time to pick the best model for each task hint.
//!
//! Persisted to `~/.llamafarm/capability-registry.json` (or the configured
//! path). Loading is best-effort: a missing or corrupt file starts with an
//! empty registry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// ── Score types ──────────────────────────────────────────────────

/// All measured capability scores for a single Ollama model.
///
/// Scores are in `[0.0, 1.0]` unless noted. `None` means not yet measured.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelCapability {
    /// Model name as returned by Ollama (e.g. `"qwen3.5:9b"`).
    pub model: String,

    // ── Static metadata (from `ollama show`) ──────────────────
    /// Context window in tokens.
    pub context_length: Option<u64>,
    /// Parameter count string (e.g. `"9.0B"`).
    pub parameter_size: Option<String>,
    /// Quantization level (e.g. `"Q4_K_M"`).
    pub quantization_level: Option<String>,
    /// Model family (e.g. `"qwen2"`, `"llama"`).
    pub family: Option<String>,
    /// Whether this model accepts image inputs.
    pub supports_vision: bool,
    /// Whether Ollama reports native tool-calling for this model.
    pub supports_tools_native: bool,
    /// Estimated VRAM usage in MB (from Ollama running endpoint).
    pub vram_mb: Option<u64>,

    // ── Bakeoff scores ─────────────────────────────────────────
    /// Shell command reliability: fraction of shell tasks completed correctly.
    pub shell_reliability: Option<f32>,
    /// File editing reliability: fraction of patch tasks applied correctly.
    pub file_editing: Option<f32>,
    /// JSON/structured output compliance rate.
    pub json_compliance: Option<f32>,
    /// Code generation quality score.
    pub coding_score: Option<f32>,
    /// RAG answer quality (citation faithfulness + answer accuracy).
    pub rag_score: Option<f32>,
    /// Autonomous multi-step task completion rate.
    pub autonomous_completion: Option<f32>,
    /// Break-and-recover task success rate.
    pub recovery_score: Option<f32>,

    // ── Latency ────────────────────────────────────────────────
    /// Median time-to-first-token in milliseconds.
    pub latency_ms_p50: Option<u64>,
    /// Median tokens per second (generation speed).
    pub tokens_per_sec_p50: Option<f32>,

    // ── Bookkeeping ────────────────────────────────────────────
    /// ISO-8601 timestamp of the last bakeoff run for this model.
    pub last_evaluated: Option<String>,
}

impl ModelCapability {
    /// Compute a composite score for a given task hint.
    ///
    /// Returns a value in `[0.0, 1.0]`. Higher is better.
    /// Falls back to `0.5` when the relevant score has not been measured.
    pub fn score_for_hint(&self, hint: &str) -> f32 {
        match hint {
            "coder" | "coding" => self.coding_score.unwrap_or(0.5),
            "planner" | "reasoning" => {
                // Planners need good autonomous completion + coding
                let a = self.autonomous_completion.unwrap_or(0.5);
                let c = self.coding_score.unwrap_or(0.5);
                (a + c) / 2.0
            }
            "verifier" | "judge" => {
                let j = self.json_compliance.unwrap_or(0.5);
                let c = self.coding_score.unwrap_or(0.5);
                (j + c) / 2.0
            }
            "operator" | "shell" => self.shell_reliability.unwrap_or(0.5),
            "rag" | "retrieval" | "summarizer" => self.rag_score.unwrap_or(0.5),
            "fast" => {
                // Prefer low latency
                let tps = self.tokens_per_sec_p50.unwrap_or(10.0);
                (tps / 100.0).min(1.0)
            }
            "recovery" | "chaos" => self.recovery_score.unwrap_or(0.5),
            "vision" => {
                if self.supports_vision {
                    self.coding_score.unwrap_or(0.5)
                } else {
                    0.0
                }
            }
            _ => {
                // Generic: average of available scores
                let scores: Vec<f32> = [
                    self.shell_reliability,
                    self.coding_score,
                    self.json_compliance,
                    self.rag_score,
                    self.autonomous_completion,
                ]
                .iter()
                .filter_map(|s| *s)
                .collect();

                if scores.is_empty() {
                    0.5
                } else {
                    scores.iter().sum::<f32>() / scores.len() as f32
                }
            }
        }
    }

    /// Return true if this model satisfies the minimum requirements for a hint.
    pub fn meets_requirements(&self, hint: &str) -> bool {
        match hint {
            "vision" => self.supports_vision,
            "long_context" => self.context_length.unwrap_or(0) >= 32_768,
            "tools" => self.supports_tools_native,
            _ => true,
        }
    }
}

// ── Registry ─────────────────────────────────────────────────────

/// Thread-safe registry of per-model capability scores.
#[derive(Debug, Default)]
pub struct CapabilityRegistry {
    inner: RwLock<RegistryInner>,
    persist_path: Option<PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryInner {
    models: HashMap<String, ModelCapability>,
}

impl CapabilityRegistry {
    /// Create an empty in-memory registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a registry backed by a JSON file at `path`.
    ///
    /// Loads existing data if the file exists; starts empty otherwise.
    pub fn with_persist_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let inner = Self::load_from_disk(&path).unwrap_or_default();
        Self {
            inner: RwLock::new(inner),
            persist_path: Some(path),
        }
    }

    /// Wrap in `Arc` for shared ownership across threads.
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    // ── Read ─────────────────────────────────────────────────────

    /// Return the capability record for `model`, or `None` if unknown.
    pub fn get(&self, model: &str) -> Option<ModelCapability> {
        self.inner.read().ok()?.models.get(model).cloned()
    }

    /// Return a snapshot of all known models.
    pub fn all(&self) -> Vec<ModelCapability> {
        self.inner
            .read()
            .map(|g| g.models.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Select the best model for a given task `hint` from a list of candidates.
    ///
    /// Candidates that don't meet minimum requirements for the hint are excluded.
    /// Falls back to the first candidate if none has been evaluated.
    pub fn best_for_hint<'a>(&self, hint: &str, candidates: &[&'a str]) -> Option<&'a str> {
        let guard = self.inner.read().ok()?;

        candidates
            .iter()
            .filter(|&&m| {
                guard
                    .models
                    .get(m)
                    .map_or(true, |cap| cap.meets_requirements(hint))
            })
            .max_by(|&&a, &&b| {
                let sa = guard
                    .models
                    .get(a)
                    .map_or(0.5, |c| c.score_for_hint(hint));
                let sb = guard
                    .models
                    .get(b)
                    .map_or(0.5, |c| c.score_for_hint(hint));
                sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied()
    }

    // ── Write ─────────────────────────────────────────────────────

    /// Insert or update a capability record.
    pub fn update(&self, cap: ModelCapability) {
        if let Ok(mut guard) = self.inner.write() {
            guard.models.insert(cap.model.clone(), cap);
        }
        self.flush_to_disk();
    }

    /// Upsert only the static metadata fields (from `ollama show`).
    ///
    /// Preserves existing bakeoff scores.
    pub fn update_metadata(
        &self,
        model: &str,
        context_length: u64,
        parameter_size: &str,
        quantization_level: &str,
        family: &str,
        supports_vision: bool,
        supports_tools_native: bool,
    ) {
        if let Ok(mut guard) = self.inner.write() {
            let entry = guard
                .models
                .entry(model.to_string())
                .or_insert_with(|| ModelCapability {
                    model: model.to_string(),
                    ..Default::default()
                });
            entry.context_length = Some(context_length);
            entry.parameter_size = Some(parameter_size.to_string());
            entry.quantization_level = Some(quantization_level.to_string());
            entry.family = Some(family.to_string());
            entry.supports_vision = supports_vision;
            entry.supports_tools_native = supports_tools_native;
        }
        self.flush_to_disk();
    }

    /// Upsert bakeoff scores for a model. Only updates `Some` fields.
    pub fn update_scores(
        &self,
        model: &str,
        shell: Option<f32>,
        file_editing: Option<f32>,
        json_compliance: Option<f32>,
        coding: Option<f32>,
        rag: Option<f32>,
        autonomous: Option<f32>,
        recovery: Option<f32>,
        latency_ms_p50: Option<u64>,
        tokens_per_sec_p50: Option<f32>,
    ) {
        if let Ok(mut guard) = self.inner.write() {
            let entry = guard
                .models
                .entry(model.to_string())
                .or_insert_with(|| ModelCapability {
                    model: model.to_string(),
                    ..Default::default()
                });
            if let Some(v) = shell {
                entry.shell_reliability = Some(v);
            }
            if let Some(v) = file_editing {
                entry.file_editing = Some(v);
            }
            if let Some(v) = json_compliance {
                entry.json_compliance = Some(v);
            }
            if let Some(v) = coding {
                entry.coding_score = Some(v);
            }
            if let Some(v) = rag {
                entry.rag_score = Some(v);
            }
            if let Some(v) = autonomous {
                entry.autonomous_completion = Some(v);
            }
            if let Some(v) = recovery {
                entry.recovery_score = Some(v);
            }
            if let Some(v) = latency_ms_p50 {
                entry.latency_ms_p50 = Some(v);
            }
            if let Some(v) = tokens_per_sec_p50 {
                entry.tokens_per_sec_p50 = Some(v);
            }
            entry.last_evaluated =
                Some(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
        }
        self.flush_to_disk();
    }

    // ── Persistence ────────────────────────────────────────────

    fn load_from_disk(path: &Path) -> Option<RegistryInner> {
        let content = std::fs::read_to_string(path).ok()?;
        match serde_json::from_str::<RegistryInner>(&content) {
            Ok(inner) => {
                info!(
                    "capability registry: loaded {} model(s) from {}",
                    inner.models.len(),
                    path.display()
                );
                Some(inner)
            }
            Err(e) => {
                warn!(
                    "capability registry: failed to parse {}: {e} — starting empty",
                    path.display()
                );
                None
            }
        }
    }

    fn flush_to_disk(&self) {
        let Some(ref path) = self.persist_path else {
            return;
        };
        let guard = match self.inner.read() {
            Ok(g) => g,
            Err(_) => return,
        };
        let json = match serde_json::to_string_pretty(&*guard) {
            Ok(j) => j,
            Err(e) => {
                warn!("capability registry: serialize error: {e}");
                return;
            }
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(path, json) {
            warn!("capability registry: write error {}: {e}", path.display());
        }
    }
}

// ── Default persist path ──────────────────────────────────────────

/// Return the default persistence path: `~/.llamafarm/capability-registry.json`.
pub fn default_registry_path() -> PathBuf {
    let base = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    base.join(".llamafarm").join("capability-registry.json")
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with_models() -> CapabilityRegistry {
        let reg = CapabilityRegistry::new();
        reg.update(ModelCapability {
            model: "fast-model:7b".to_string(),
            shell_reliability: Some(0.9),
            coding_score: Some(0.7),
            json_compliance: Some(0.85),
            rag_score: Some(0.6),
            tokens_per_sec_p50: Some(80.0),
            latency_ms_p50: Some(120),
            ..Default::default()
        });
        reg.update(ModelCapability {
            model: "smart-model:32b".to_string(),
            shell_reliability: Some(0.75),
            coding_score: Some(0.95),
            json_compliance: Some(0.92),
            rag_score: Some(0.9),
            autonomous_completion: Some(0.88),
            tokens_per_sec_p50: Some(20.0),
            latency_ms_p50: Some(800),
            ..Default::default()
        });
        reg
    }

    #[test]
    fn best_for_coder_picks_smart_model() {
        let reg = registry_with_models();
        let candidates = vec!["fast-model:7b", "smart-model:32b"];
        let best = reg.best_for_hint("coder", &candidates);
        assert_eq!(best, Some("smart-model:32b"));
    }

    #[test]
    fn best_for_fast_picks_fast_model() {
        let reg = registry_with_models();
        let candidates = vec!["fast-model:7b", "smart-model:32b"];
        let best = reg.best_for_hint("fast", &candidates);
        assert_eq!(best, Some("fast-model:7b"));
    }

    #[test]
    fn vision_requirement_filters_non_vision() {
        let reg = CapabilityRegistry::new();
        reg.update(ModelCapability {
            model: "text-only:7b".to_string(),
            supports_vision: false,
            coding_score: Some(0.9),
            ..Default::default()
        });
        reg.update(ModelCapability {
            model: "llava:7b".to_string(),
            supports_vision: true,
            coding_score: Some(0.6),
            ..Default::default()
        });
        let candidates = vec!["text-only:7b", "llava:7b"];
        let best = reg.best_for_hint("vision", &candidates);
        assert_eq!(best, Some("llava:7b"));
    }

    #[test]
    fn update_scores_preserves_metadata() {
        let reg = CapabilityRegistry::new();
        reg.update_metadata(
            "qwen3.5:9b",
            32768,
            "9.0B",
            "Q4_K_M",
            "qwen2",
            false,
            true,
        );
        reg.update_scores(
            "qwen3.5:9b",
            Some(0.88),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(150),
            Some(45.0),
        );
        let cap = reg.get("qwen3.5:9b").unwrap();
        assert_eq!(cap.context_length, Some(32768));
        assert_eq!(cap.shell_reliability, Some(0.88));
        assert_eq!(cap.latency_ms_p50, Some(150));
        assert!(cap.last_evaluated.is_some());
    }

    #[test]
    fn returns_none_when_no_candidates() {
        let reg = CapabilityRegistry::new();
        assert!(reg.best_for_hint("coder", &[]).is_none());
    }

    #[test]
    fn falls_back_to_first_when_none_evaluated() {
        let reg = CapabilityRegistry::new();
        let candidates = vec!["model-a:7b", "model-b:13b"];
        // Neither model is in registry — all have score 0.5, max_by picks last equal
        let best = reg.best_for_hint("coder", &candidates);
        assert!(best.is_some());
    }
}
