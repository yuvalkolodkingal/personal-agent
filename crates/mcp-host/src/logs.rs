//! Bounded per-server lifecycle log ring.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use personal_agent_mcp_manager::MAX_SERVER_LOGS;
use serde::{Deserialize, Serialize};

/// Longest single log line retained. Server stderr is untrusted and unbounded.
const MAX_LINE_BYTES: usize = 2_000;

/// Severity of a host log line.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Lifecycle progress.
    Info,
    /// Recoverable problem, such as a retried connection attempt.
    Warn,
    /// Terminal failure for the current operation.
    Error,
}

/// One retained lifecycle line for a server.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LogLine {
    /// When the host recorded the line.
    pub at: DateTime<Utc>,
    /// Severity.
    pub level: LogLevel,
    /// Message, truncated to a bounded length.
    pub message: String,
}

/// Fixed-capacity ring of [`LogLine`] values.
#[derive(Clone, Debug, Default)]
pub struct LogRing {
    lines: VecDeque<LogLine>,
}

impl LogRing {
    /// Appends a line, evicting the oldest entry when full.
    pub fn push(&mut self, level: LogLevel, message: impl Into<String>) {
        let mut message = message.into();
        if message.len() > MAX_LINE_BYTES {
            let cut = (0..=MAX_LINE_BYTES)
                .rev()
                .find(|index| message.is_char_boundary(*index))
                .unwrap_or(0);
            message.truncate(cut);
            message.push('…');
        }
        if self.lines.len() == MAX_SERVER_LOGS {
            self.lines.pop_front();
        }
        self.lines.push_back(LogLine {
            at: Utc::now(),
            level,
            message,
        });
    }

    /// Returns the retained lines, oldest first.
    #[must_use]
    pub fn lines(&self) -> Vec<LogLine> {
        self.lines.iter().cloned().collect()
    }

    /// Number of retained lines.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the ring holds no lines.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{LogLevel, LogRing, MAX_LINE_BYTES};
    use personal_agent_mcp_manager::MAX_SERVER_LOGS;

    #[test]
    fn ring_evicts_oldest_beyond_capacity() {
        let mut ring = LogRing::default();
        for index in 0..MAX_SERVER_LOGS + 10 {
            ring.push(LogLevel::Info, format!("line {index}"));
        }
        let lines = ring.lines();
        assert_eq!(lines.len(), MAX_SERVER_LOGS);
        assert_eq!(lines[0].message, "line 10");
    }

    #[test]
    fn long_lines_are_truncated_on_a_character_boundary() {
        let mut ring = LogRing::default();
        ring.push(LogLevel::Error, "é".repeat(MAX_LINE_BYTES));
        let line = ring.lines().remove(0);
        assert!(line.message.len() <= MAX_LINE_BYTES + '…'.len_utf8());
        assert!(line.message.ends_with('…'));
    }
}
