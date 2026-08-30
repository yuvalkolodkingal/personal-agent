//! Translation from `rmcp` protocol models into the manager's catalog types.
//!
//! Tool behaviour annotations are carried across verbatim: a hint the server
//! actually sent is preserved, and a hint the server omitted stays at the
//! manager's conservative default instead of being invented here.

use personal_agent_mcp_manager::{
    CapabilityCatalog, PromptDescriptor, ResourceDescriptor, ToolAnnotations, ToolDescriptor,
};
use rmcp::model::{Prompt, Resource, ServerCapabilities, Tool};
use serde_json::{Value, json};

/// Maps MCP tool annotation hints onto the manager's behaviour flags.
///
/// Hints the server sent are carried across unchanged. The one reconciliation
/// is `readOnlyHint`, which the MCP schema makes authoritative over the other
/// two: a tool that does not modify its environment is never destructive and is
/// always safe to repeat, whatever the remaining hints claim.
#[must_use]
pub fn tool_annotations(tool: &Tool) -> ToolAnnotations {
    let Some(annotations) = tool.annotations.as_ref() else {
        return ToolAnnotations::default();
    };
    let read_only = annotations.read_only_hint.unwrap_or(false);
    ToolAnnotations {
        read_only,
        destructive: !read_only && annotations.destructive_hint.unwrap_or(false),
        idempotent: read_only || annotations.idempotent_hint.unwrap_or(false),
        open_world: annotations.open_world_hint.unwrap_or(false),
    }
}

fn tool_title(tool: &Tool) -> Option<String> {
    tool.title.clone().or_else(|| {
        tool.annotations
            .as_ref()
            .and_then(|annotations| annotations.title.clone())
    })
}

fn object_value(schema: &serde_json::Map<String, Value>) -> Value {
    Value::Object(schema.clone())
}

/// Converts one advertised tool, namespacing its resolved name.
#[must_use]
pub fn tool_descriptor(namespace: &str, tool: &Tool) -> ToolDescriptor {
    let name = tool.name.to_string();
    ToolDescriptor {
        resolved_name: format!("{namespace}.{name}"),
        name,
        title: tool_title(tool),
        description: tool
            .description
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        input_schema: object_value(&tool.input_schema),
        output_schema: tool.output_schema.as_deref().map(object_value),
        annotations: tool_annotations(tool),
    }
}

fn resource_descriptor(resource: &Resource) -> ResourceDescriptor {
    ResourceDescriptor {
        uri: resource.uri.clone(),
        name: resource.name.clone(),
        description: resource.description.clone().unwrap_or_default(),
        mime_type: resource.mime_type.clone(),
    }
}

fn prompt_descriptor(prompt: &Prompt) -> PromptDescriptor {
    let arguments_schema = prompt.arguments.as_ref().map(|arguments| {
        let required: Vec<Value> = arguments
            .iter()
            .filter(|argument| argument.required.unwrap_or(false))
            .map(|argument| json!(argument.name))
            .collect();
        let properties: serde_json::Map<String, Value> = arguments
            .iter()
            .map(|argument| {
                let description = argument.description.clone().unwrap_or_default();
                (
                    argument.name.clone(),
                    json!({"type": "string", "description": description}),
                )
            })
            .collect();
        json!({"type": "object", "properties": properties, "required": required})
    });
    PromptDescriptor {
        name: prompt.name.clone(),
        description: prompt.description.clone().unwrap_or_default(),
        arguments_schema,
    }
}

/// Assembles the manager catalog from a completed listing pass.
#[must_use]
pub fn capability_catalog(
    namespace: &str,
    capabilities: &ServerCapabilities,
    tools: &[Tool],
    resources: &[Resource],
    prompts: &[Prompt],
) -> CapabilityCatalog {
    CapabilityCatalog {
        tools: tools
            .iter()
            .map(|tool| tool_descriptor(namespace, tool))
            .collect(),
        resources: resources.iter().map(resource_descriptor).collect(),
        prompts: prompts.iter().map(prompt_descriptor).collect(),
        supports_logging: capabilities.logging.is_some(),
        supports_completions: capabilities.completions.is_some(),
        supports_resource_subscriptions: capabilities
            .resources
            .as_ref()
            .and_then(|resources| resources.subscribe)
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::{tool_annotations, tool_descriptor};
    use rmcp::model::{Tool, ToolAnnotations as McpToolAnnotations};
    use std::sync::Arc;

    fn tool(annotations: Option<McpToolAnnotations>) -> Tool {
        Tool {
            name: "echo".into(),
            title: None,
            description: Some("echoes".into()),
            input_schema: Arc::new(serde_json::Map::new()),
            output_schema: None,
            annotations,
            icons: None,
            meta: None,
        }
    }

    #[test]
    fn server_supplied_hints_survive_translation() {
        let annotations = tool_annotations(&tool(Some(McpToolAnnotations {
            title: None,
            read_only_hint: Some(false),
            destructive_hint: Some(true),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })));
        assert!(!annotations.read_only);
        assert!(annotations.destructive);
        assert!(annotations.idempotent);
        assert!(annotations.open_world);
    }

    #[test]
    fn read_only_wins_over_a_contradictory_destructive_hint() {
        let annotations = tool_annotations(&tool(Some(McpToolAnnotations {
            title: None,
            read_only_hint: Some(true),
            destructive_hint: Some(true),
            idempotent_hint: None,
            open_world_hint: Some(false),
        })));
        assert!(annotations.read_only);
        assert!(!annotations.destructive);
        assert!(annotations.idempotent, "a read-only tool is safe to repeat");
    }

    #[test]
    fn missing_annotations_stay_at_the_conservative_default() {
        let annotations = tool_annotations(&tool(None));
        assert_eq!(
            annotations,
            personal_agent_mcp_manager::ToolAnnotations::default()
        );
    }

    #[test]
    fn descriptor_namespaces_the_resolved_name() {
        let descriptor = tool_descriptor("fixture", &tool(None));
        assert_eq!(descriptor.resolved_name, "fixture.echo");
        assert_eq!(descriptor.name, "echo");
        assert_eq!(descriptor.description, "echoes");
    }
}
