//! Provider-neutral request model.
//!
//! One request shape is translated per provider by `anthropic::wire` and
//! `openai::wire`. Callers never build provider JSON directly, so a prompt or
//! tool schema cannot accidentally acquire a provider-specific field.

use serde_json::Value;

/// Who authored a message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    /// Turn authored by the person.
    User,
    /// Turn authored by the model.
    Assistant,
}

/// A cacheable block of prompt text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextBlock {
    /// The text itself.
    pub text: String,
    /// Mark the prefix ending at this block as cacheable.
    pub cache: bool,
}

impl TextBlock {
    /// A block that does not close a cacheable prefix.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cache: false,
        }
    }

    /// A block that closes a cacheable prefix.
    #[must_use]
    pub fn cached(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cache: true,
        }
    }
}

/// One content block inside a message.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    /// Visible text.
    Text(TextBlock),
    /// A tool call the model previously requested, replayed back to it.
    ToolUse {
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        tool: String,
        /// Arguments the model produced.
        arguments: Value,
    },
    /// The result of executing a tool call.
    ToolResult {
        /// Identifier of the call being answered.
        call_id: String,
        /// Result rendered for the model.
        content: String,
        /// Whether the tool failed.
        is_error: bool,
    },
}

/// One conversation turn.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    /// Author of the turn.
    pub role: Role,
    /// Content blocks in wire order.
    pub content: Vec<Content>,
}

impl Message {
    /// A user turn containing a single text block.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![Content::Text(TextBlock::new(text))],
        }
    }

    /// An assistant turn containing a single text block.
    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![Content::Text(TextBlock::new(text))],
        }
    }
}

/// A tool the model may call.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDefinition {
    /// Tool name as the model will emit it.
    pub name: String,
    /// Description used by the model to decide when to call the tool.
    pub description: String,
    /// JSON Schema object describing the arguments.
    pub input_schema: Value,
    /// Mark the tool list prefix ending at this tool as cacheable.
    pub cache: bool,
}

impl ToolDefinition {
    /// Define a tool that does not close a cacheable prefix.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            cache: false,
        }
    }
}

/// How the model should decide between answering and calling a tool.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ToolChoice {
    /// The model decides.
    #[default]
    Auto,
    /// The model must call some tool.
    Any,
    /// The model must not call a tool.
    None,
    /// The model must call the named tool.
    Named(String),
}

/// One streamed turn request.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatRequest {
    /// Model identifier as the provider spells it.
    pub model: String,
    /// System prompt blocks, rendered before the messages.
    pub system: Vec<TextBlock>,
    /// Conversation history, oldest first.
    pub messages: Vec<Message>,
    /// Tools advertised for this turn.
    pub tools: Vec<ToolDefinition>,
    /// Hard ceiling on generated tokens.
    pub max_output_tokens: u32,
    /// Tool selection policy.
    pub tool_choice: ToolChoice,
}

impl ChatRequest {
    /// A request with no system prompt, history, or tools.
    #[must_use]
    pub fn new(model: impl Into<String>, max_output_tokens: u32) -> Self {
        Self {
            model: model.into(),
            system: Vec::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_output_tokens,
            tool_choice: ToolChoice::Auto,
        }
    }

    /// Replace the system prompt with one uncached block.
    #[must_use]
    pub fn with_system(mut self, text: impl Into<String>) -> Self {
        self.system = vec![TextBlock::new(text)];
        self
    }

    /// Append one message.
    #[must_use]
    pub fn with_message(mut self, message: Message) -> Self {
        self.messages.push(message);
        self
    }

    /// Append one tool definition.
    #[must_use]
    pub fn with_tool(mut self, tool: ToolDefinition) -> Self {
        self.tools.push(tool);
        self
    }

    /// Reject a request the provider would reject anyway, before any network use.
    pub(crate) fn validate(&self) -> Result<(), crate::error::LlmError> {
        if self.model.trim().is_empty() {
            return Err(crate::error::LlmError::Request(
                "model identifier is empty".to_owned(),
            ));
        }
        if self.max_output_tokens == 0 {
            return Err(crate::error::LlmError::Request(
                "max_output_tokens must be greater than zero".to_owned(),
            ));
        }
        if self.messages.is_empty() {
            return Err(crate::error::LlmError::Request(
                "request contains no messages".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ChatRequest, Message};
    use crate::error::LlmError;

    #[test]
    fn validation_rejects_unusable_requests_without_a_network_call() {
        let valid = ChatRequest::new("claude-opus-5", 1024).with_message(Message::user("hi"));
        assert!(valid.validate().is_ok());

        let mut empty_model = valid.clone();
        empty_model.model = "  ".to_owned();
        assert!(matches!(empty_model.validate(), Err(LlmError::Request(_))));

        let mut zero_tokens = valid.clone();
        zero_tokens.max_output_tokens = 0;
        assert!(matches!(zero_tokens.validate(), Err(LlmError::Request(_))));

        let mut no_messages = valid;
        no_messages.messages.clear();
        assert!(matches!(no_messages.validate(), Err(LlmError::Request(_))));
    }
}
