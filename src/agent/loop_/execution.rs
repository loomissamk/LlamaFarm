use super::parsing::ParsedToolCall;
use super::{TOOL_CACHE, ToolLoopCancelled, scrub_credentials};
use crate::agent::tool_cache::ToolResultCache;
use crate::approval::ApprovalManager;
use crate::observability::{Observer, ObserverEvent};
use crate::tools::Tool;
use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

fn find_tool<'a>(tools: &'a [Box<dyn Tool>], name: &str) -> Option<&'a dyn Tool> {
    tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
}

fn active_cache() -> Option<Arc<ToolResultCache>> {
    TOOL_CACHE.try_with(|c| c.clone()).ok().flatten()
}

/// Saves `output` to a temp file and returns a context-friendly summary:
/// the first `preview_bytes` of content plus a path the model can read_file.
/// Falls back to hard truncation if the disk write fails.
fn persist_large_output(tool_name: &str, output: &str, max_bytes: usize) -> String {
    let tmp_dir = std::env::temp_dir().join("llamafarm-tool-output");
    let _ = std::fs::create_dir_all(&tmp_dir);
    let filename = format!("{}-{}.txt", tool_name, uuid::Uuid::new_v4());
    let path = tmp_dir.join(&filename);

    let preview_end = crate::util::floor_utf8_char_boundary(output, 2048.min(max_bytes / 4));
    let preview = &output[..preview_end];

    if std::fs::write(&path, output.as_bytes()).is_ok() {
        format!(
            "{preview}\n\n[Output too large for context ({total} bytes). \
Full output saved to {path_display}. Use file_read to access the complete output.]",
            total = output.len(),
            path_display = path.display()
        )
    } else {
        let cutoff = crate::util::floor_utf8_char_boundary(output, max_bytes);
        format!(
            "{}\n\n[Output truncated at {max_bytes} bytes]",
            &output[..cutoff]
        )
    }
}

async fn execute_one_tool(
    call_name: &str,
    call_arguments: serde_json::Value,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
) -> Result<ToolExecutionOutcome> {
    // ── cache check ────────────────────────────────────────────────
    if let Some(cache) = active_cache() {
        if let Some(cached) = cache.get(call_name, &call_arguments) {
            observer.record_event(&ObserverEvent::ToolCallStart {
                tool: call_name.to_string(),
            });
            let duration = Duration::ZERO;
            observer.record_event(&ObserverEvent::ToolCall {
                tool: call_name.to_string(),
                duration,
                success: cached.success,
            });
            return Ok(ToolExecutionOutcome {
                output: cached.output,
                success: cached.success,
                error_reason: None,
                duration,
            });
        }
    }

    observer.record_event(&ObserverEvent::ToolCallStart {
        tool: call_name.to_string(),
    });
    let start = Instant::now();

    let Some(tool) = find_tool(tools_registry, call_name) else {
        let reason = format!("Unknown tool: {call_name}");
        let duration = start.elapsed();
        observer.record_event(&ObserverEvent::ToolCall {
            tool: call_name.to_string(),
            duration,
            success: false,
        });
        return Ok(ToolExecutionOutcome {
            output: reason.clone(),
            success: false,
            error_reason: Some(scrub_credentials(&reason)),
            duration,
        });
    };

    let tool_future = tool.execute(call_arguments.clone());
    let tool_result = if let Some(token) = cancellation_token {
        tokio::select! {
            () = token.cancelled() => return Err(ToolLoopCancelled.into()),
            result = tool_future => result,
        }
    } else {
        tool_future.await
    };

    match tool_result {
        Ok(r) => {
            let duration = start.elapsed();
            observer.record_event(&ObserverEvent::ToolCall {
                tool: call_name.to_string(),
                duration,
                success: r.success,
            });

            if r.success {
                // Only cache results from tools that declare themselves read-only.
                // Caching write-tool results risks replaying stale side-effect output.
                if tool.is_read_only() {
                    if let Some(cache) = active_cache() {
                        cache.set(call_name, &call_arguments, &r);
                    }
                }

                // Large outputs are persisted to disk; the model gets a preview + path
                // it can access via file_read rather than flooding its context window.
                let output = if r.output.len() > tool.max_output_bytes() {
                    persist_large_output(call_name, &r.output, tool.max_output_bytes())
                } else {
                    r.output
                };

                Ok(ToolExecutionOutcome {
                    output: scrub_credentials(&output),
                    success: true,
                    error_reason: None,
                    duration,
                })
            } else {
                let reason = r.error.unwrap_or(r.output);
                Ok(ToolExecutionOutcome {
                    output: format!("Error: {reason}"),
                    success: false,
                    error_reason: Some(scrub_credentials(&reason)),
                    duration,
                })
            }
        }
        Err(e) => {
            let duration = start.elapsed();
            observer.record_event(&ObserverEvent::ToolCall {
                tool: call_name.to_string(),
                duration,
                success: false,
            });
            let reason = format!("Error executing {call_name}: {e}");
            Ok(ToolExecutionOutcome {
                output: reason.clone(),
                success: false,
                error_reason: Some(scrub_credentials(&reason)),
                duration,
            })
        }
    }
}

pub(super) struct ToolExecutionOutcome {
    pub(super) output: String,
    pub(super) success: bool,
    pub(super) error_reason: Option<String>,
    pub(super) duration: Duration,
}

/// Returns true only when every tool in the batch declares itself concurrency-safe
/// (i.e. read-only). Write tools always execute sequentially to prevent races.
/// Tools that require approval also force sequential execution for consistent prompting.
pub(super) fn should_execute_tools_in_parallel(
    tool_calls: &[ParsedToolCall],
    tools_registry: &[Box<dyn Tool>],
    approval: Option<&ApprovalManager>,
) -> bool {
    if tool_calls.len() <= 1 {
        return false;
    }

    if let Some(mgr) = approval {
        if tool_calls.iter().any(|call| mgr.needs_approval(&call.name)) {
            return false;
        }
    }

    // Parallel only when every tool in this batch is concurrency-safe.
    // Unknown tools (not in registry) are treated as unsafe.
    tool_calls
        .iter()
        .all(|call| find_tool(tools_registry, &call.name).is_some_and(|t| t.is_concurrency_safe()))
}

pub(super) async fn execute_tools_parallel(
    tool_calls: &[ParsedToolCall],
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
) -> Result<Vec<ToolExecutionOutcome>> {
    let futures: Vec<_> = tool_calls
        .iter()
        .map(|call| {
            execute_one_tool(
                &call.name,
                call.arguments.clone(),
                tools_registry,
                observer,
                cancellation_token,
            )
        })
        .collect();

    let results = futures_util::future::join_all(futures).await;
    results.into_iter().collect()
}

pub(super) async fn execute_tools_sequential(
    tool_calls: &[ParsedToolCall],
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
) -> Result<Vec<ToolExecutionOutcome>> {
    let mut outcomes = Vec::with_capacity(tool_calls.len());

    for call in tool_calls {
        outcomes.push(
            execute_one_tool(
                &call.name,
                call.arguments.clone(),
                tools_registry,
                observer,
                cancellation_token,
            )
            .await?,
        );
    }

    Ok(outcomes)
}
