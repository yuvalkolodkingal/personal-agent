//! Hybrid page model: `Accessibility.getFullAXTree` merged with `DOMSnapshot`
//! layout boxes.
//!
//! Both halves are structured protocol reads. No script is ever evaluated in the
//! page to build a snapshot, so page content cannot influence what the agent
//! believes it is looking at beyond the accessibility tree the renderer itself
//! computed.

use crate::NodeBounds;
use chromiumoxide_cdp::cdp::browser_protocol::accessibility::{AxNode, AxValue};
use chromiumoxide_cdp::cdp::browser_protocol::dom_snapshot::CaptureSnapshotReturns;
use serde_json::Value;
use std::collections::BTreeMap;

/// Roles the agent is allowed to act on. Anything else is only observable.
const ACTIONABLE_ROLES: &[&str] = &[
    "button",
    "checkbox",
    "colorwell",
    "combobox",
    "datetime",
    "disclosuretriangle",
    "link",
    "listbox",
    "listboxoption",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "menulistoption",
    "menulistpopup",
    "option",
    "popupbutton",
    "radio",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "textarea",
    "textbox",
    "textfield",
];

/// Roles whose descendants form a selectable option list.
const OPTION_OWNER_ROLES: &[&str] = &[
    "combobox",
    "listbox",
    "menu",
    "menulistpopup",
    "popupbutton",
];

/// Descendant roles that represent one selectable option.
const OPTION_ROLES: &[&str] = &[
    "listboxoption",
    "menuitem",
    "menulistoption",
    "option",
    "radio",
];

/// One accessible element before it is bound to a page generation.
#[derive(Clone, Debug)]
pub(crate) struct PendingNode {
    pub backend_node_id: i64,
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub editable: bool,
    pub bounds: Option<NodeBounds>,
    pub options: Vec<String>,
}

/// The generation-independent result of one hybrid read.
#[derive(Clone, Debug, Default)]
pub(crate) struct MergedSnapshot {
    pub nodes: Vec<PendingNode>,
    pub text: String,
}

/// Merge an accessibility tree with layout boxes from a DOM snapshot.
pub(crate) fn merge(ax: &[AxNode], dom: &CaptureSnapshotReturns) -> MergedSnapshot {
    let bounds = layout_bounds(dom);
    let by_id: BTreeMap<&str, &AxNode> = ax
        .iter()
        .map(|node| (node.node_id.inner().as_str(), node))
        .collect();
    let mut merged = MergedSnapshot::default();
    let mut text = String::new();
    for node in ax {
        let role = normalized_role(node.role.as_ref());
        if role == "statictext" {
            append_text(
                &mut text,
                &value_string(node.name.as_ref()).unwrap_or_default(),
            );
            continue;
        }
        if node.ignored {
            continue;
        }
        let Some(backend_node_id) = node.backend_dom_node_id.as_ref().map(|id| *id.inner()) else {
            continue;
        };
        let editable = is_editable(node, &role);
        if !ACTIONABLE_ROLES.contains(&role.as_str()) && !editable && !has_flag(node, "focusable") {
            continue;
        }
        merged.nodes.push(PendingNode {
            backend_node_id,
            options: option_labels(node, &by_id, &role),
            role,
            name: value_string(node.name.as_ref()).unwrap_or_default(),
            value: value_string(node.value.as_ref()),
            editable,
            bounds: bounds.get(&backend_node_id).copied(),
        });
    }
    merged.text = text;
    merged
}

fn append_text(text: &mut String, fragment: &str) {
    let fragment = fragment.trim();
    if fragment.is_empty() {
        return;
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(fragment);
}

/// Chrome reports roles in several casings across versions; compare on a
/// lowercase alphanumeric form so the engine does not drift with the browser.
fn normalized_role(role: Option<&AxValue>) -> String {
    value_string(role)
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn value_string(value: Option<&AxValue>) -> Option<String> {
    match value?.value.as_ref()? {
        Value::String(text) => Some(text.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn has_flag(node: &AxNode, property: &str) -> bool {
    node.properties.iter().flatten().any(|candidate| {
        format!("{:?}", candidate.name).eq_ignore_ascii_case(property)
            && candidate.value.value.as_ref() == Some(&Value::Bool(true))
    })
}

fn is_editable(node: &AxNode, role: &str) -> bool {
    if matches!(role, "textbox" | "textfield" | "searchbox" | "textarea") {
        return true;
    }
    node.properties
        .iter()
        .flatten()
        .any(|candidate| format!("{:?}", candidate.name).eq_ignore_ascii_case("editable"))
}

fn option_labels(node: &AxNode, by_id: &BTreeMap<&str, &AxNode>, role: &str) -> Vec<String> {
    if !OPTION_OWNER_ROLES.contains(&role) {
        return Vec::new();
    }
    let mut labels = Vec::new();
    let mut frontier: Vec<&str> = node
        .child_ids
        .iter()
        .flatten()
        .map(|id| id.inner().as_str())
        .collect();
    let mut visited = 0_usize;
    while let Some(id) = frontier.pop() {
        visited += 1;
        if visited > 4096 {
            break;
        }
        let Some(child) = by_id.get(id) else { continue };
        if OPTION_ROLES.contains(&normalized_role(child.role.as_ref()).as_str()) {
            labels.push(value_string(child.name.as_ref()).unwrap_or_default());
        }
        frontier.extend(
            child
                .child_ids
                .iter()
                .flatten()
                .map(|id| id.inner().as_str()),
        );
    }
    labels.reverse();
    labels
}

/// Build a `backendNodeId -> bounds` table from every document in the snapshot.
fn layout_bounds(dom: &CaptureSnapshotReturns) -> BTreeMap<i64, NodeBounds> {
    let mut bounds = BTreeMap::new();
    for document in &dom.documents {
        let Some(backend_ids) = document.nodes.backend_node_id.as_ref() else {
            continue;
        };
        for (position, node_index) in document.layout.node_index.iter().enumerate() {
            let Some(rectangle) = document.layout.bounds.get(position) else {
                continue;
            };
            let Ok(node_index) = usize::try_from(*node_index) else {
                continue;
            };
            let Some(backend_id) = backend_ids.get(node_index).map(|id| *id.inner()) else {
                continue;
            };
            if let Some(rectangle) = rectangle_to_bounds(rectangle.inner()) {
                bounds.insert(backend_id, rectangle);
            }
        }
    }
    bounds
}

fn rectangle_to_bounds(values: &[f64]) -> Option<NodeBounds> {
    let [x, y, width, height] = values.get(..4)? else {
        return None;
    };
    if [x, y, width, height].iter().any(|value| !value.is_finite()) {
        return None;
    }
    Some(NodeBounds {
        x: *x,
        y: *y,
        width: *width,
        height: *height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tree(nodes: Value) -> Vec<AxNode> {
        serde_json::from_value(nodes).expect("ax fixture")
    }

    fn empty_dom() -> CaptureSnapshotReturns {
        serde_json::from_value(json!({"documents": [], "strings": []})).expect("dom fixture")
    }

    #[test]
    fn static_text_becomes_page_text_and_never_a_handle() {
        let ax = tree(json!([
            {"nodeId": "1", "ignored": false, "role": {"type": "role", "value": "StaticText"},
             "name": {"type": "computedString", "value": "Hello world"}, "backendDOMNodeId": 7},
            {"nodeId": "2", "ignored": false, "role": {"type": "role", "value": "StaticText"},
             "name": {"type": "computedString", "value": "  Second line "}, "backendDOMNodeId": 8},
        ]));
        let merged = merge(&ax, &empty_dom());
        assert_eq!(merged.text, "Hello world\nSecond line");
        assert!(merged.nodes.is_empty());
    }

    #[test]
    fn ignored_and_decorative_nodes_are_not_actionable() {
        let ax = tree(json!([
            {"nodeId": "1", "ignored": true, "role": {"type": "role", "value": "button"},
             "name": {"type": "computedString", "value": "Hidden"}, "backendDOMNodeId": 3},
            {"nodeId": "2", "ignored": false, "role": {"type": "role", "value": "generic"},
             "name": {"type": "computedString", "value": "Wrapper"}, "backendDOMNodeId": 4},
            {"nodeId": "3", "ignored": false, "role": {"type": "role", "value": "button"},
             "name": {"type": "computedString", "value": "Submit"}, "backendDOMNodeId": 5},
        ]));
        let merged = merge(&ax, &empty_dom());
        assert_eq!(merged.nodes.len(), 1);
        assert_eq!(merged.nodes[0].backend_node_id, 5);
        assert_eq!(merged.nodes[0].name, "Submit");
    }

    #[test]
    fn text_fields_are_editable_and_carry_their_value() {
        let ax = tree(json!([
            {"nodeId": "1", "ignored": false, "role": {"type": "role", "value": "textbox"},
             "name": {"type": "computedString", "value": "Full name"},
             "value": {"type": "string", "value": "Ada"}, "backendDOMNodeId": 11},
        ]));
        let merged = merge(&ax, &empty_dom());
        assert!(merged.nodes[0].editable);
        assert_eq!(merged.nodes[0].value.as_deref(), Some("Ada"));
    }

    #[test]
    fn select_options_are_collected_in_document_order() {
        let ax = tree(json!([
            {"nodeId": "1", "ignored": false, "role": {"type": "role", "value": "combobox"},
             "name": {"type": "computedString", "value": "Plan"},
             "value": {"type": "string", "value": "Free"},
             "childIds": ["2"], "backendDOMNodeId": 20},
            {"nodeId": "2", "ignored": false, "role": {"type": "role", "value": "menuListPopup"},
             "childIds": ["3", "4", "5"], "backendDOMNodeId": 21},
            {"nodeId": "3", "ignored": false, "role": {"type": "role", "value": "menuListOption"},
             "name": {"type": "computedString", "value": "Free"}, "backendDOMNodeId": 22},
            {"nodeId": "4", "ignored": false, "role": {"type": "role", "value": "menuListOption"},
             "name": {"type": "computedString", "value": "Pro"}, "backendDOMNodeId": 23},
            {"nodeId": "5", "ignored": false, "role": {"type": "role", "value": "menuListOption"},
             "name": {"type": "computedString", "value": "Team"}, "backendDOMNodeId": 24},
        ]));
        let merged = merge(&ax, &empty_dom());
        let combobox = merged
            .nodes
            .iter()
            .find(|node| node.backend_node_id == 20)
            .expect("combobox");
        assert_eq!(combobox.options, vec!["Free", "Pro", "Team"]);
    }

    #[test]
    fn layout_boxes_are_attached_by_backend_node_id() {
        let ax = tree(json!([
            {"nodeId": "1", "ignored": false, "role": {"type": "role", "value": "button"},
             "name": {"type": "computedString", "value": "Go"}, "backendDOMNodeId": 42},
        ]));
        let dom: CaptureSnapshotReturns = serde_json::from_value(json!({
            "documents": [{
                "documentURL": 0, "title": 0, "baseURL": 0, "contentLanguage": 0,
                "encodingName": 0, "publicId": 0, "systemId": 0, "frameId": 0,
                "nodes": {"backendNodeId": [42]},
                "layout": {
                    "nodeIndex": [0],
                    "styles": [],
                    "bounds": [[8.4, 16.6, 100.0, 30.0]],
                    "text": [],
                    "stackingContexts": {"index": []},
                },
                "textBoxes": {"layoutIndex": [], "bounds": [], "start": [], "length": []},
            }],
            "strings": [""],
        }))
        .expect("dom fixture");
        let merged = merge(&ax, &dom);
        assert_eq!(
            merged.nodes[0].bounds,
            Some(NodeBounds {
                x: 8.4,
                y: 16.6,
                width: 100.0,
                height: 30.0
            })
        );
    }
}
