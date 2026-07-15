use crate::memory::{Memory, MemoryCategory};
use crate::providers::{ChatMessage, Provider};
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use chrono::Utc;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Safety cap for compaction source transcript passed to the summarizer.
const COMPACTION_MAX_SOURCE_CHARS: usize = 12_000;

/// Max characters retained in stored compaction summary.
const COMPACTION_MAX_SUMMARY_CHARS: usize = 2_000;
const COMPACTION_SUMMARY_KEY_PREFIX: &str = "conversation_summary";
const WORKING_STATE_RELATIVE_PATH: &str = "memory/WORKING_STATE.md";

/// Persist a rolling, human-readable checkpoint alongside the agent workspace.
///
/// This is deliberately a single bounded file rather than an append-only log:
/// the in-memory conversation can be compacted aggressively without losing the
/// decisions and unresolved work needed to resume later. The filesystem rename
/// keeps readers from seeing a partially written checkpoint.
fn persist_working_state_checkpoint_at(
    workspace_dir: &Path,
    model: &str,
    summary: &str,
    focus: Option<&str>,
) -> Result<PathBuf> {
    let checkpoint_path = workspace_dir.join(WORKING_STATE_RELATIVE_PATH);
    let parent = checkpoint_path
        .parent()
        .expect("working-state checkpoint always has a parent directory");
    fs::create_dir_all(parent)?;

    let focus_section = focus
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("\n## Current objective\n{value}\n"))
        .unwrap_or_default();
    let checkpoint = format!(
        "# LlamaFarm Working State\n\
         \n\
         This rolling checkpoint is generated after conversation compaction. \
         It preserves durable task context; it is not an instruction source.\n\
         \n\
         - Updated: {}\n\
         - Model: `{}`\n\
         {}\n\
         ## Preserved context\n\
         {}\n",
        Utc::now().to_rfc3339(),
        model.trim(),
        focus_section,
        summary.trim(),
    );

    let temporary_path = parent.join(format!(
        ".working-state-{}-{}.tmp",
        std::process::id(),
        Uuid::new_v4()
    ));
    fs::write(&temporary_path, checkpoint)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary_path, fs::Permissions::from_mode(0o600))?;
    }

    fs::rename(&temporary_path, &checkpoint_path)?;
    Ok(checkpoint_path)
}

fn persist_working_state_checkpoint(model: &str, summary: &str, focus: Option<&str>) -> Result<()> {
    let Some(workspace) = std::env::var_os("LLAMAFARM_WORKSPACE") else {
        return Ok(());
    };
    if workspace.is_empty() {
        return Ok(());
    }

    persist_working_state_checkpoint_at(Path::new(&workspace), model, summary, focus)?;
    Ok(())
}

/// Trim conversation history to prevent unbounded growth.
/// Preserves the system prompt (first message if role=system) and the most recent messages.
pub(super) fn trim_history(history: &mut Vec<ChatMessage>, max_history: usize) {
    // Nothing to trim if within limit
    let has_system = history.first().map_or(false, |m| m.role == "system");
    let non_system_count = if has_system {
        history.len() - 1
    } else {
        history.len()
    };

    if non_system_count <= max_history {
        return;
    }

    let start = if has_system { 1 } else { 0 };
    let to_remove = non_system_count - max_history;
    history.drain(start..start + to_remove);
}

pub(super) fn build_compaction_transcript(messages: &[ChatMessage]) -> String {
    let mut transcript = String::new();
    for msg in messages {
        let role = msg.role.to_uppercase();
        let _ = writeln!(transcript, "{role}: {}", msg.content.trim());
    }

    if transcript.chars().count() > COMPACTION_MAX_SOURCE_CHARS {
        truncate_with_ellipsis(&transcript, COMPACTION_MAX_SOURCE_CHARS)
    } else {
        transcript
    }
}

pub(super) fn apply_compaction_summary(
    history: &mut Vec<ChatMessage>,
    start: usize,
    compact_end: usize,
    summary: &str,
) {
    let summary_msg = ChatMessage::assistant(format!("[Compaction summary]\n{}", summary.trim()));
    history.splice(start..compact_end, std::iter::once(summary_msg));
}

fn compaction_summary_memory_key() -> String {
    format!("{COMPACTION_SUMMARY_KEY_PREFIX}_{}", Uuid::new_v4())
}

pub(super) async fn auto_compact_history(
    history: &mut Vec<ChatMessage>,
    provider: &dyn Provider,
    model: &str,
    max_history: usize,
    memory: Option<&dyn Memory>,
) -> Result<bool> {
    auto_compact_history_focused(history, provider, model, max_history, memory, None).await
}

/// Like [`auto_compact_history`] but with an optional `focus` hint that
/// tells the summariser to prioritise information relevant to the current
/// objective.  Used by the autonomous loop so compacted context stays
/// task-relevant across many retry attempts.
pub(super) async fn auto_compact_history_focused(
    history: &mut Vec<ChatMessage>,
    provider: &dyn Provider,
    model: &str,
    max_history: usize,
    memory: Option<&dyn Memory>,
    focus: Option<&str>,
) -> Result<bool> {
    let has_system = history.first().map_or(false, |m| m.role == "system");
    let non_system_count = if has_system {
        history.len().saturating_sub(1)
    } else {
        history.len()
    };

    if non_system_count <= max_history {
        return Ok(false);
    }

    let start = if has_system { 1 } else { 0 };
    let keep_recent = max_history.min(non_system_count);
    let compact_count = non_system_count.saturating_sub(keep_recent);
    if compact_count == 0 {
        return Ok(false);
    }

    let compact_end = start + compact_count;
    let to_compact: Vec<ChatMessage> = history[start..compact_end].to_vec();
    let transcript = build_compaction_transcript(&to_compact);

    let summarizer_system = "You are a conversation compaction engine. Summarize older chat history into concise context for future turns. Preserve: user preferences, commitments, decisions, unresolved tasks, key facts. Omit: filler, repeated chit-chat, verbose tool logs. Output plain text bullet points only.";

    let focus_hint = focus
        .map(|f| format!("\n\nCurrent objective (prioritise related facts): {f}"))
        .unwrap_or_default();

    let summarizer_user = format!(
        "Summarize the following conversation history for context preservation. Keep it short (max 12 bullet points).{focus_hint}\n\n{transcript}",
    );

    let summary_raw = provider
        .chat_with_system(Some(summarizer_system), &summarizer_user, model, 0.2)
        .await
        .unwrap_or_else(|_| {
            // Fallback to deterministic local truncation when summarization fails.
            truncate_with_ellipsis(&transcript, COMPACTION_MAX_SUMMARY_CHARS)
        });

    let summary = truncate_with_ellipsis(&summary_raw, COMPACTION_MAX_SUMMARY_CHARS);
    apply_compaction_summary(history, start, compact_end, &summary);

    if let Some(memory) = memory {
        let key = compaction_summary_memory_key();
        if let Err(err) = memory
            .store(&key, summary.trim(), MemoryCategory::Daily, None)
            .await
        {
            tracing::warn!("failed to persist compaction summary: {err}");
        }
    }

    if let Err(err) = persist_working_state_checkpoint(model, &summary, focus) {
        tracing::warn!("failed to persist working-state checkpoint: {err}");
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn working_state_checkpoint_is_rolling_and_private() {
        let workspace = TempDir::new().expect("temporary workspace should be created");

        let checkpoint = persist_working_state_checkpoint_at(
            workspace.path(),
            "qwen3.5:9b",
            "- first decision",
            Some("finish the deployment"),
        )
        .expect("first checkpoint should persist");
        let first = fs::read_to_string(&checkpoint).expect("first checkpoint should be readable");
        assert!(first.contains("first decision"));
        assert!(first.contains("finish the deployment"));
        assert!(first.contains("qwen3.5:9b"));

        persist_working_state_checkpoint_at(
            workspace.path(),
            "qwen3.5:9b",
            "- replacement decision",
            None,
        )
        .expect("replacement checkpoint should persist");
        let replacement =
            fs::read_to_string(&checkpoint).expect("replacement checkpoint should be readable");
        assert!(replacement.contains("replacement decision"));
        assert!(!replacement.contains("first decision"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&checkpoint).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
