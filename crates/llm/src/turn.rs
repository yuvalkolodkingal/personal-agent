//! Accumulator shared by the provider decoders.
//!
//! Both wire dialects stream the same logical turn: interleaved text,
//! reasoning, and incrementally serialized tool-call arguments, followed by a
//! stop reason and usage. This module owns that assembly so the dialect
//! decoders only classify frames.

use serde_json::Value;

use crate::event::{AssistantMessage, FinishReason, LlmEvent, ToolCall, Usage};

/// A tool call being assembled from argument fragments.
#[derive(Clone, Debug, Default)]
struct ToolCallBuilder {
    call_id: String,
    tool: String,
    arguments: String,
    announced: bool,
    settled: bool,
}

/// Streaming state for one turn.
#[derive(Clone, Debug)]
pub(crate) struct TurnState {
    provider: String,
    model: String,
    started: bool,
    message: AssistantMessage,
    calls: Vec<ToolCallBuilder>,
    usage: Usage,
    finish: Option<FinishReason>,
    completed: bool,
}

impl TurnState {
    pub(crate) fn new(provider: &str, model: &str) -> Self {
        Self {
            provider: provider.to_owned(),
            model: model.to_owned(),
            started: false,
            message: AssistantMessage::default(),
            calls: Vec::new(),
            usage: Usage::default(),
            finish: None,
            completed: false,
        }
    }

    pub(crate) fn usage_mut(&mut self) -> &mut Usage {
        &mut self.usage
    }

    pub(crate) fn set_finish(&mut self, reason: FinishReason) {
        self.finish = Some(reason);
    }

    /// Whether the provider stated why it stopped.
    pub(crate) fn has_finish(&self) -> bool {
        self.finish.is_some()
    }

    /// Emit `response.started` exactly once, adopting the served model name.
    pub(crate) fn start(
        &mut self,
        model: Option<&str>,
        response_id: Option<String>,
    ) -> Vec<LlmEvent> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        if let Some(model) = model.filter(|model| !model.is_empty()) {
            model.clone_into(&mut self.model);
        }
        vec![LlmEvent::ResponseStarted {
            provider: self.provider.clone(),
            model: self.model.clone(),
            response_id,
        }]
    }

    pub(crate) fn text(&mut self, delta: &str) -> Vec<LlmEvent> {
        if delta.is_empty() {
            return Vec::new();
        }
        self.message.text.push_str(delta);
        vec![LlmEvent::ResponseDelta {
            text: delta.to_owned(),
        }]
    }

    pub(crate) fn reasoning(&mut self, delta: &str) -> Vec<LlmEvent> {
        if delta.is_empty() {
            return Vec::new();
        }
        self.message.reasoning.push_str(delta);
        vec![LlmEvent::ReasoningAvailable {
            text: delta.to_owned(),
        }]
    }

    fn slot(&mut self, index: usize) -> &mut ToolCallBuilder {
        while self.calls.len() <= index {
            self.calls.push(ToolCallBuilder::default());
        }
        &mut self.calls[index]
    }

    /// Record a call's identity, emitting `tool.started` once it is known.
    pub(crate) fn announce(
        &mut self,
        index: usize,
        call_id: Option<&str>,
        tool: Option<&str>,
    ) -> Vec<LlmEvent> {
        let slot = self.slot(index);
        if let Some(call_id) = call_id.filter(|value| !value.is_empty()) {
            call_id.clone_into(&mut slot.call_id);
        }
        if let Some(tool) = tool.filter(|value| !value.is_empty()) {
            tool.clone_into(&mut slot.tool);
        }
        if slot.announced || slot.tool.is_empty() {
            return Vec::new();
        }
        slot.announced = true;
        let (call_id, tool) = (slot.call_id.clone(), slot.tool.clone());
        vec![LlmEvent::ToolCallStarted {
            index: index_as_u32(index),
            call_id,
            tool,
        }]
    }

    /// Append an argument fragment, emitting `tool.progress`.
    pub(crate) fn arguments(&mut self, index: usize, fragment: &str) -> Vec<LlmEvent> {
        if fragment.is_empty() {
            return Vec::new();
        }
        let slot = self.slot(index);
        slot.arguments.push_str(fragment);
        let (call_id, tool) = (slot.call_id.clone(), slot.tool.clone());
        vec![LlmEvent::ToolCallProgress {
            index: index_as_u32(index),
            call_id,
            tool,
            arguments_delta: fragment.to_owned(),
        }]
    }

    /// Close one call: parse its arguments and record it on the message.
    pub(crate) fn settle(&mut self, index: usize) -> Vec<LlmEvent> {
        let slot = self.slot(index);
        if slot.settled || (slot.tool.is_empty() && slot.arguments.is_empty()) {
            return Vec::new();
        }
        slot.settled = true;
        let (call_id, tool, raw) = (
            slot.call_id.clone(),
            slot.tool.clone(),
            slot.arguments.clone(),
        );
        match parse_arguments(&raw) {
            Ok(arguments) => {
                let call = ToolCall {
                    call_id,
                    tool,
                    arguments,
                };
                self.message.tool_calls.push(call.clone());
                vec![LlmEvent::ToolCallReady {
                    index: index_as_u32(index),
                    call,
                }]
            }
            Err(error) => vec![LlmEvent::ToolCallFailed {
                index: index_as_u32(index),
                call_id,
                tool,
                error,
            }],
        }
    }

    /// Terminate the turn: settle every open call, then report the step and
    /// the terminal completion.
    pub(crate) fn complete(&mut self) -> Vec<LlmEvent> {
        if self.completed {
            return Vec::new();
        }
        self.completed = true;
        let mut events = Vec::new();
        for index in 0..self.calls.len() {
            events.extend(self.settle(index));
        }
        let finish = match self.finish.clone() {
            Some(finish) => finish,
            None if self.message.tool_calls.is_empty() => FinishReason::Stop,
            None => FinishReason::ToolCalls,
        };
        events.push(LlmEvent::ResponseStepCompleted {
            finish_reason: finish.clone(),
            usage: self.usage,
        });
        events.push(LlmEvent::ResponseCompleted {
            finish_reason: finish,
            usage: self.usage,
            message: Box::new(self.message.clone()),
        });
        events
    }

    /// Terminate the turn with a failure, preserving whatever was billed.
    pub(crate) fn fail(&mut self, error: String) -> Vec<LlmEvent> {
        if self.completed {
            return Vec::new();
        }
        self.completed = true;
        vec![LlmEvent::ResponseFailed {
            error,
            usage: self.usage,
        }]
    }

    #[cfg(test)]
    pub(crate) fn is_completed(&self) -> bool {
        self.completed
    }
}

fn index_as_u32(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

/// Parse streamed arguments, treating an empty stream as an empty object.
fn parse_arguments(raw: &str) -> Result<Value, String> {
    if raw.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    serde_json::from_str(raw).map_err(|error| format!("tool arguments are not valid JSON: {error}"))
}

#[cfg(test)]
mod tests {
    use super::TurnState;
    use crate::event::{FinishReason, LlmEvent};
    use serde_json::json;

    #[test]
    fn assembles_a_tool_call_from_fragments() {
        let mut state = TurnState::new("fixture", "deterministic");
        let mut events = state.start(Some("deterministic"), Some("resp_1".to_owned()));
        events.extend(state.announce(0, Some("call_1"), Some("write")));
        events.extend(state.announce(0, Some("call_1"), Some("write")));
        events.extend(state.arguments(0, "{\"path\":"));
        events.extend(state.arguments(0, "\"a.txt\"}"));
        state.set_finish(FinishReason::ToolCalls);
        events.extend(state.complete());
        events.extend(state.complete());

        let types: Vec<&str> = events.iter().map(LlmEvent::event_type).collect();
        assert_eq!(
            types,
            [
                "response.started",
                "tool.started",
                "tool.progress",
                "tool.progress",
                "tool.progress",
                "response.step_completed",
                "response.completed",
            ]
        );
        let LlmEvent::ResponseCompleted {
            message,
            finish_reason,
            ..
        } = events.last().expect("terminal event")
        else {
            panic!("last event must be terminal");
        };
        assert_eq!(*finish_reason, FinishReason::ToolCalls);
        assert_eq!(message.tool_calls.len(), 1);
        assert_eq!(message.tool_calls[0].tool, "write");
        assert_eq!(message.tool_calls[0].arguments, json!({"path": "a.txt"}));
    }

    #[test]
    fn unparsable_arguments_fail_only_that_call() {
        let mut state = TurnState::new("fixture", "deterministic");
        state.start(None, None);
        state.announce(0, Some("call_1"), Some("write"));
        state.arguments(0, "{\"path\":");
        let events = state.complete();
        assert_eq!(events[0].event_type(), "tool.failed");
        assert_eq!(events[1].event_type(), "response.step_completed");
        assert_eq!(events[2].event_type(), "response.completed");
        let LlmEvent::ToolCallFailed { error, .. } = &events[0] else {
            panic!("expected a failed call");
        };
        assert!(error.contains("not valid JSON"));
    }

    #[test]
    fn a_call_with_no_arguments_settles_as_an_empty_object() {
        let mut state = TurnState::new("fixture", "deterministic");
        state.start(None, None);
        state.announce(0, Some("call_1"), Some("status"));
        let events = state.complete();
        let LlmEvent::ToolCallReady { call, .. } = &events[0] else {
            panic!("expected a ready call");
        };
        assert_eq!(call.arguments, json!({}));
        assert_eq!(
            events[2].event_type(),
            "response.completed",
            "an unstated finish reason defaults from the call set"
        );
    }

    #[test]
    fn failure_is_terminal_and_suppresses_completion() {
        let mut state = TurnState::new("fixture", "deterministic");
        state.start(None, None);
        let failed = state.fail("stream closed".to_owned());
        assert_eq!(failed.len(), 1);
        assert!(failed[0].is_terminal());
        assert!(state.is_completed());
        assert!(state.complete().is_empty());
    }
}
