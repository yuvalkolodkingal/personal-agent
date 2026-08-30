//! Cooperative abort for an in-flight turn.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

/// A cloneable handle that cancels one streamed turn.
///
/// Aborting is level-triggered and idempotent: the flag latches, so a reader
/// that checks after the fact still observes the abort, and the notification
/// wakes a task parked on the next network chunk.
#[derive(Clone, Debug, Default)]
pub struct AbortHandle {
    aborted: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl AbortHandle {
    /// Create a handle that has not been aborted.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation of the associated turn.
    pub fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }

    /// Resolve once cancellation has been requested.
    pub(crate) async fn cancelled(&self) {
        loop {
            if self.is_aborted() {
                return;
            }
            let notified = self.notify.notified();
            if self.is_aborted() {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AbortHandle;
    use std::time::Duration;

    #[tokio::test]
    async fn abort_latches_and_wakes_waiters() {
        let handle = AbortHandle::new();
        let waiter = handle.clone();
        let task = tokio::spawn(async move { waiter.cancelled().await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!handle.is_aborted());
        handle.abort();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("abort must wake the waiter")
            .expect("waiter task");
        assert!(handle.is_aborted());
        // Late observers still see the abort.
        assert!(handle.clone().is_aborted());
    }
}
