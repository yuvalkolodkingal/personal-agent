//! Handle to one streamed turn.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::Stream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::abort::AbortHandle;
use crate::event::LlmEvent;

/// Receiver for the typed events of one turn.
///
/// The stream always terminates: it yields either a `response.completed` or a
/// `response.failed` event and then closes. Dropping the handle aborts the
/// in-flight HTTP request, so a cancelled turn stops consuming tokens as soon
/// as the provider notices the closed connection.
#[derive(Debug)]
pub struct TurnStream {
    events: mpsc::Receiver<LlmEvent>,
    abort: AbortHandle,
    task: JoinHandle<()>,
}

impl TurnStream {
    pub(crate) fn new(
        events: mpsc::Receiver<LlmEvent>,
        abort: AbortHandle,
        task: JoinHandle<()>,
    ) -> Self {
        Self {
            events,
            abort,
            task,
        }
    }

    /// A cloneable handle that cancels this turn from anywhere.
    #[must_use]
    pub fn abort_handle(&self) -> AbortHandle {
        self.abort.clone()
    }

    /// Cancel this turn. The stream then delivers a terminal
    /// `response.failed` event and closes.
    pub fn abort(&self) {
        self.abort.abort();
    }

    /// Await the next event, or `None` once the turn has ended.
    pub async fn recv(&mut self) -> Option<LlmEvent> {
        self.events.recv().await
    }
}

impl Stream for TurnStream {
    type Item = LlmEvent;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().events.poll_recv(context)
    }
}

impl Drop for TurnStream {
    fn drop(&mut self) {
        self.abort.abort();
        self.task.abort();
    }
}
