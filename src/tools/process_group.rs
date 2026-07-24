use std::io;

pub(crate) const fn is_supported() -> bool {
    cfg!(unix)
}

/// Configure a child as the leader of a new process group.
///
/// `kill_on_drop` is a final fallback for the direct child. On Unix, callers
/// should also keep a [`ProcessGroupGuard`] armed until the command finishes so
/// cancellation and timeout paths terminate descendants as well.
pub(crate) fn configure(command: &mut tokio::process::Command) {
    command.kill_on_drop(true);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
}

/// Terminate every process in the group led by `pid`.
#[cfg(unix)]
pub(crate) fn terminate(pid: u32) -> io::Result<()> {
    use nix::errno::Errno;
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;

    let raw_pid = i32::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "process ID exceeds i32"))?;
    match killpg(Pid::from_raw(raw_pid), Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
    }
}

/// Process groups are a Unix facility. Non-Unix callers use direct-child
/// termination instead.
#[cfg(not(unix))]
pub(crate) fn terminate(_pid: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process-group termination is unavailable on this platform",
    ))
}

/// RAII cleanup for a newly spawned process group.
///
/// This is deliberately synchronous so dropping an in-flight tool future (for
/// example, when a WebSocket turn is cancelled) cannot orphan the command.
pub(crate) struct ProcessGroupGuard {
    pid: Option<u32>,
}

impl ProcessGroupGuard {
    pub(crate) fn new(pid: u32) -> Self {
        Self { pid: Some(pid) }
    }

    pub(crate) fn disarm(&mut self) {
        self.pid = None;
    }

    pub(crate) fn terminate(&mut self) -> io::Result<()> {
        let Some(pid) = self.pid else {
            return Ok(());
        };
        terminate(pid)?;
        self.disarm();
        Ok(())
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            let _ = terminate(pid);
        }
    }
}
