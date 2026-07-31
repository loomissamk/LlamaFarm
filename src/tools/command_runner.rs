use std::io;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::AsyncReadExt;

use super::process_group::{self, ProcessGroupGuard};

pub(crate) struct CappedStream {
    pub(crate) bytes: Vec<u8>,
    pub(crate) truncated: bool,
}

pub(crate) struct CommandOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: CappedStream,
    pub(crate) stderr: CappedStream,
}

pub(crate) enum CommandExecution {
    Completed(CommandOutput),
    TimedOut,
}

/// Execute a child while continuously draining both pipes.
///
/// `None` means no wall-clock deadline. Dropping this future still terminates
/// the direct child and, on Unix, all descendants in its process group.
pub(crate) async fn run_capped_command(
    mut command: tokio::process::Command,
    max_output_bytes: usize,
    timeout: Option<Duration>,
) -> io::Result<CommandExecution> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    process_group::configure(&mut command);

    let mut child = command.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| io::Error::other("spawned command did not expose a process ID"))?;
    let mut process_group = ProcessGroupGuard::new(pid);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let execution = async {
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            read_capped(stdout, max_output_bytes),
            read_capped(stderr, max_output_bytes),
        );
        Ok::<CommandOutput, io::Error>(CommandOutput {
            status: status?,
            stdout: stdout?,
            stderr: stderr?,
        })
    };
    let mut execution = Box::pin(execution);

    let result = match timeout {
        Some(timeout) => match tokio::time::timeout(timeout, execution.as_mut()).await {
            Ok(result) => Some(result),
            Err(_) => None,
        },
        None => Some(execution.as_mut().await),
    };

    match result {
        Some(Ok(output)) => {
            drop(execution);
            process_group.disarm();
            Ok(CommandExecution::Completed(output))
        }
        Some(Err(error)) => {
            drop(execution);
            let _ = process_group.terminate();
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            Err(error)
        }
        None => {
            drop(execution);
            let _ = process_group.terminate();
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            Ok(CommandExecution::TimedOut)
        }
    }
}

async fn read_capped<R>(handle: Option<R>, max_bytes: usize) -> io::Result<CappedStream>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(mut reader) = handle else {
        return Ok(CappedStream {
            bytes: Vec::new(),
            truncated: false,
        });
    };

    let mut bytes = Vec::with_capacity(max_bytes.min(8192));
    let mut truncated = false;
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        let remaining = max_bytes.saturating_sub(bytes.len());
        let retained = count.min(remaining);
        bytes.extend_from_slice(&chunk[..retained]);
        truncated |= count > retained;
    }

    Ok(CappedStream { bytes, truncated })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unlimited_command_completes_after_delay() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("sleep 0.05; printf complete");

        let result = run_capped_command(command, 1024, None)
            .await
            .expect("command should execute");
        let CommandExecution::Completed(output) = result else {
            panic!("unlimited command must not time out");
        };
        assert!(output.status.success());
        assert_eq!(output.stdout.bytes, b"complete");
    }

    #[tokio::test]
    async fn positive_timeout_starts_when_command_is_spawned() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("sleep 1");

        let result = run_capped_command(command, 1024, Some(Duration::from_millis(50)))
            .await
            .expect("timeout should return an execution outcome");
        assert!(matches!(result, CommandExecution::TimedOut));
    }

    #[tokio::test]
    async fn capped_reader_keeps_draining_after_retention_limit() {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg("head -c 1048576 /dev/zero; printf drained >&2");

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_capped_command(command, 1024, None),
        )
        .await
        .expect("verbose command must not block on a full pipe")
        .expect("verbose command should execute");
        let CommandExecution::Completed(output) = result else {
            panic!("unlimited command must not time out");
        };
        assert!(output.status.success());
        assert_eq!(output.stdout.bytes.len(), 1024);
        assert!(output.stdout.truncated);
        assert_eq!(output.stderr.bytes, b"drained");
        assert!(!output.stderr.truncated);
    }
}
