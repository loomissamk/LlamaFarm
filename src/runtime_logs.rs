use parking_lot::Mutex;
use serde::Serialize;
use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::broadcast;
use tracing_subscriber::fmt::writer::MakeWriter;

const DEFAULT_LOG_CAPACITY: usize = 2_000;

static GLOBAL_RUNTIME_LOG_STORE: OnceLock<Arc<RuntimeLogStore>> = OnceLock::new();

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RuntimeLogEntry {
    pub id: u64,
    pub timestamp: String,
    pub line: String,
}

pub struct RuntimeLogStore {
    capacity: usize,
    next_id: AtomicU64,
    entries: Mutex<VecDeque<RuntimeLogEntry>>,
    tx: broadcast::Sender<RuntimeLogEntry>,
}

impl RuntimeLogStore {
    pub fn new(capacity: usize) -> Self {
        let bounded_capacity = capacity.max(1);
        let (tx, _rx) = broadcast::channel(bounded_capacity.max(32));
        Self {
            capacity: bounded_capacity,
            next_id: AtomicU64::new(0),
            entries: Mutex::new(VecDeque::with_capacity(bounded_capacity)),
            tx,
        }
    }

    pub fn push_line(&self, line: impl Into<String>) {
        let line = strip_ansi_sequences(&line.into()).trim().to_string();
        if line.is_empty() {
            return;
        }

        let entry = RuntimeLogEntry {
            id: self.next_id.fetch_add(1, Ordering::Relaxed) + 1,
            timestamp: chrono::Utc::now().to_rfc3339(),
            line,
        };

        {
            let mut entries = self.entries.lock();
            if entries.len() >= self.capacity {
                entries.pop_front();
            }
            entries.push_back(entry.clone());
        }

        let _ = self.tx.send(entry);
    }

    /// Drop all buffered log entries (operator "clear history" support).
    pub fn clear(&self) -> usize {
        let mut entries = self.entries.lock();
        let removed = entries.len();
        entries.clear();
        removed
    }

    pub fn tail(&self, limit: usize) -> Vec<RuntimeLogEntry> {
        let limit = limit.max(1).min(self.capacity);
        let entries = self.entries.lock();
        let start = entries.len().saturating_sub(limit);
        entries.iter().skip(start).cloned().collect()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeLogEntry> {
        self.tx.subscribe()
    }
}

pub fn global_runtime_log_store() -> Arc<RuntimeLogStore> {
    GLOBAL_RUNTIME_LOG_STORE
        .get_or_init(|| Arc::new(RuntimeLogStore::new(DEFAULT_LOG_CAPACITY)))
        .clone()
}

#[derive(Clone)]
pub struct RuntimeLogMakeWriter {
    store: Arc<RuntimeLogStore>,
}

impl RuntimeLogMakeWriter {
    pub fn new(store: Arc<RuntimeLogStore>) -> Self {
        Self { store }
    }
}

impl<'a> MakeWriter<'a> for RuntimeLogMakeWriter {
    type Writer = RuntimeLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        RuntimeLogWriter::new(self.store.clone())
    }
}

pub struct RuntimeLogWriter {
    store: Arc<RuntimeLogStore>,
    inner: io::Stderr,
    pending: Vec<u8>,
}

impl RuntimeLogWriter {
    fn new(store: Arc<RuntimeLogStore>) -> Self {
        Self {
            store,
            inner: io::stderr(),
            pending: Vec::new(),
        }
    }

    fn consume_bytes(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);

        while let Some(pos) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line_bytes = self.pending.drain(..=pos).collect::<Vec<_>>();
            let line = String::from_utf8_lossy(&line_bytes);
            self.store.push_line(line.trim_end_matches(['\r', '\n']));
        }
    }
}

impl Write for RuntimeLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write_all(buf)?;
        self.consume_bytes(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()?;
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            let line = String::from_utf8_lossy(&pending);
            self.store.push_line(line.trim_end_matches('\r'));
        }
        Ok(())
    }
}

fn strip_ansi_sequences(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            output.push(ch);
            continue;
        }

        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' && matches!(chars.peek(), Some(&'\\')) {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_returns_last_entries_up_to_limit() {
        let store = RuntimeLogStore::new(3);
        store.push_line("one");
        store.push_line("two");
        store.push_line("three");
        store.push_line("four");

        let lines = store.tail(2);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].line, "three");
        assert_eq!(lines[1].line, "four");
    }

    #[test]
    fn writer_splits_lines_and_strips_ansi_sequences() {
        let store = Arc::new(RuntimeLogStore::new(10));
        let mut writer = RuntimeLogWriter::new(store.clone());

        writer
            .write_all(b"\x1b[32mINFO\x1b[0m first line\nsecond")
            .expect("writer should accept first chunk");
        writer.flush().expect("writer should flush");

        let lines = store.tail(10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].line, "INFO first line");
        assert_eq!(lines[1].line, "second");
    }
}
